use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
const SECRET: &str = "hearth-test-secret";
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hearth-pam-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let fixture = Self { dir };
        let module = fixture.dir.join("pam_fixture.so");
        let output = Command::new("gcc")
            .args(["-Wall", "-Wextra", "-Werror", "-shared", "-fPIC"])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/pam-test.c"))
            .arg("-o")
            .arg(&module)
            .arg("-lpam")
            .output()?;
        ensure!(
            output.status.success(),
            "Compile private PAM fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::write(
            fixture.dir.join("hearth-private"),
            format!(
                "auth required {} mode={} pidfile={}\naccount required {} mode={}\n",
                module.display(),
                mode,
                fixture.dir.join("child.pid").display(),
                module.display(),
                mode
            ),
        )?;
        Ok(fixture)
    }
    fn worker(&self, timeout: u64) -> Result<Worker> {
        let mut process = Process(
            Command::new(env!("CARGO_BIN_EXE_hearth-pam"))
                .args(["--service", "hearth-private", "--config-dir"])
                .arg(&self.dir)
                .args(["--timeout-ms", &timeout.to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let input = process.0.stdin.take().context("stdin")?;
        let output = process.0.stdout.take().context("stdout")?;
        nonblocking(output.as_raw_fd())?;
        let mut worker = Worker {
            process,
            input,
            output,
            pending: Vec::new(),
            transcript: Vec::new(),
            session: Value::Null,
        };
        let ready = worker.receive(|v| v["type"] == "ready")?;
        worker.session = ready["session"].clone();
        Ok(worker)
    }
    fn child_pid(&self) -> Result<i32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(value) = fs::read_to_string(self.dir.join("child.pid"))
                && let Ok(pid) = value.trim().parse()
            {
                return Ok(pid);
            }
            ensure!(Instant::now() < deadline, "PAM module never started");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "nonblocking IO"
    );
    Ok(())
}
struct Worker {
    process: Process,
    input: ChildStdin,
    output: ChildStdout,
    pending: Vec<u8>,
    transcript: Vec<u8>,
    session: Value,
}
impl Worker {
    fn send(&mut self, value: Value) -> Result<()> {
        self.input.write_all(&serde_json::to_vec(&value)?)?;
        self.input.write_all(b"\n")?;
        Ok(())
    }
    fn receive(&mut self, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = self.pending.drain(..=end).collect();
                let message = serde_json::from_slice(&line)?;
                if predicate(&message) {
                    return Ok(message);
                }
            }
            let mut bytes = [0; 8192];
            match self.output.read(&mut bytes) {
                Ok(0) => {
                    anyhow::bail!("PAM worker closed stdout: {:?}", self.process.0.try_wait()?)
                }
                Ok(n) => {
                    self.pending.extend_from_slice(&bytes[..n]);
                    self.transcript.extend_from_slice(&bytes[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => return Err(e.into()),
            }
            ensure!(self.pending.len() <= 128 * 1024, "Oversized PAM output");
            ensure!(Instant::now() < deadline, "PAM event timed out");
        }
    }
    fn start(&mut self, id: &str) -> Result<Value> {
        self.send(json!({"type":"authenticate","session":self.session,"request_id":id}))?;
        self.receive(|v| v["type"] == "started" && v["request_id"] == id)
    }
    fn reply(&mut self, prompt: &Value, value: &str) -> Result<()> {
        self.send(json!({"type":"reply","session":self.session,"attempt":prompt["attempt"],"prompt_id":prompt["prompt_id"],"value":value}))
    }
    fn drive(&mut self, wrong: bool) -> Result<(Value, Vec<String>)> {
        let mut styles = Vec::new();
        loop {
            let event = self.receive(|v| v["type"] == "prompt" || v["type"] == "result")?;
            if event["type"] == "result" {
                return Ok((event, styles));
            }
            styles.push(event["style"].as_str().context("prompt style")?.into());
            if event["response_required"] == true {
                let response = match event["text"].as_str().context("prompt text")? {
                    "Test identity:" => "fixture-user",
                    "Test secret:" => {
                        if wrong {
                            "wrong-test-value"
                        } else {
                            SECRET
                        }
                    }
                    "Test code:" => "654321",
                    _ => anyhow::bail!("Unexpected fixture prompt"),
                };
                self.reply(&event, response)?;
            }
        }
    }
    fn cancel(&mut self, attempt: &Value) -> Result<Value> {
        self.send(json!({"type":"cancel","session":self.session,"attempt":attempt}))?;
        self.receive(|v| v["type"] == "result" && v["attempt"] == *attempt)
    }
    fn stop(&mut self) -> Result<()> {
        ensure!(
            unsafe { libc::kill(self.process.0.id() as i32, libc::SIGTERM) } == 0,
            "SIGTERM failed"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.process.0.try_wait()? {
                ensure!(status.success(), "PAM worker exit {status}");
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "PAM worker failed bounded shutdown"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let mut stderr = String::new();
        self.process
            .0
            .stderr
            .take()
            .context("stderr")?
            .read_to_string(&mut stderr)?;
        assert!(!stderr.contains(SECRET));
        assert!(!stderr.contains("654321"));
        self.no_secrets();
        Ok(())
    }
    fn no_secrets(&self) {
        let text = String::from_utf8_lossy(&self.transcript);
        assert!(!text.contains(SECRET));
        assert!(!text.contains("654321"));
        assert!(!text.contains("wrong-test-value"));
    }
}
fn reaped(pid: i32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "PAM child {pid} survived or remains zombie"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn multi_prompt_authentication_wrong_response_and_account_denial() -> Result<()> {
    for (mode, wrong, status) in [
        ("multi", false, "authenticated"),
        ("multi", true, "denied"),
        ("account-denied", false, "account_denied"),
    ] {
        let fixture = Fixture::new(mode)?;
        let mut worker = fixture.worker(3000)?;
        let started = worker.start("auth")?;
        let (result, styles) = worker.drive(wrong)?;
        assert_eq!(result["attempt"], started["attempt"]);
        assert_eq!(result["status"], status);
        assert_eq!(&styles[..4], ["info", "error", "echo_on", "echo_off"]);
        if !wrong {
            assert_eq!(styles[4], "echo_off");
        }
        reaped(fixture.child_pid()?)?;
        worker.stop()?;
    }
    Ok(())
}
#[test]
fn crash_hang_deadline_and_cancel_reap_private_children() -> Result<()> {
    for mode in ["crash", "hang"] {
        let fixture = Fixture::new(mode)?;
        let mut worker = fixture.worker(250)?;
        worker.start("attempt")?;
        let pid = fixture.child_pid()?;
        let result = worker.receive(|v| v["type"] == "result")?;
        assert_eq!(
            result["status"],
            if mode == "crash" { "error" } else { "timeout" }
        );
        reaped(pid)?;
        worker.stop()?;
    }
    let fixture = Fixture::new("hang")?;
    let mut worker = fixture.worker(5000)?;
    let started = worker.start("cancel")?;
    let pid = fixture.child_pid()?;
    assert_eq!(worker.cancel(&started["attempt"])?["status"], "cancelled");
    reaped(pid)?;
    worker.stop()?;
    Ok(())
}
#[test]
fn unanswered_prompt_deadline_and_worker_shutdown_reap_children() -> Result<()> {
    let fixture = Fixture::new("multi")?;
    let mut worker = fixture.worker(250)?;
    worker.start("unanswered")?;
    worker.receive(|v| v["type"] == "prompt" && v["response_required"] == true)?;
    let pid = fixture.child_pid()?;
    assert_eq!(
        worker.receive(|v| v["type"] == "result")?["status"],
        "timeout"
    );
    reaped(pid)?;
    worker.stop()?;
    let fixture = Fixture::new("hang")?;
    let mut worker = fixture.worker(5000)?;
    worker.start("shutdown")?;
    let pid = fixture.child_pid()?;
    worker.stop()?;
    reaped(pid)?;
    Ok(())
}
#[test]
fn duplicate_late_and_cross_attempt_replies_never_authorize_another_attempt() -> Result<()> {
    let fixture = Fixture::new("multi")?;
    let mut worker = fixture.worker(3000)?;
    let started = worker.start("first")?;
    let prompt = worker.receive(|v| v["type"] == "prompt" && v["response_required"] == true)?;
    let reply = json!({"type":"reply","session":worker.session,"attempt":prompt["attempt"],"prompt_id":prompt["prompt_id"],"value":"fixture-user"});
    let mut line = serde_json::to_vec(&reply)?;
    line.push(b'\n');
    let twice = [line.as_slice(), line.as_slice()].concat();
    worker.input.write_all(&twice)?;
    worker.receive(|v| v["type"] == "command_rejected")?;
    let (result, _) = worker.drive(false)?;
    assert_eq!(result["status"], "authenticated");
    worker.reply(&prompt, SECRET)?;
    worker.receive(|v| v["type"] == "command_rejected")?;
    let next = worker.start("second")?;
    assert_ne!(next["attempt"], started["attempt"]);
    let current = worker.receive(|v| v["type"] == "prompt" && v["response_required"] == true)?;
    assert_ne!(current["prompt_id"], prompt["prompt_id"]);
    worker.reply(&prompt, SECRET)?;
    worker.receive(|v| v["type"] == "command_rejected")?;
    assert_eq!(worker.cancel(&next["attempt"])?["status"], "cancelled");
    worker.reply(&current, SECRET)?;
    worker.receive(|v| v["type"] == "command_rejected")?;
    reaped(fixture.child_pid()?)?;
    worker.stop()?;
    Ok(())
}
#[test]
fn output_backpressure_cannot_prevent_cancellation_and_reaping() -> Result<()> {
    let fixture = Fixture::new("hang")?;
    let mut worker = fixture.worker(5000)?;
    let started = worker.start("blocked")?;
    let pid = fixture.child_pid()?;
    let fd = worker.output.as_raw_fd();
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, 4096) }, 4096);
    nonblocking(worker.input.as_raw_fd())?;
    let mut line = serde_json::to_vec(
        &json!({"type":"reply","session":"stale","attempt":started["attempt"],"prompt_id":"stale","value":SECRET}),
    )?;
    line.push(b'\n');
    let deadline = Instant::now() + Duration::from_secs(4);
    let (mut previous, mut stalled) = (0, 0);
    loop {
        match worker.input.write(&line) {
            Ok(n) => assert_eq!(n, line.len()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        thread::sleep(Duration::from_millis(25));
        let mut queued: libc::c_int = 0;
        ensure!(
            unsafe { libc::ioctl(fd, libc::FIONREAD, &mut queued) } == 0,
            "queued output"
        );
        if queued > 0 && queued == previous {
            stalled += 1;
        } else {
            stalled = 0;
        }
        previous = queued;
        if stalled >= 5 {
            break;
        }
        ensure!(Instant::now() < deadline, "PAM output did not stall");
    }
    worker.stop()?;
    reaped(pid)?;
    Ok(())
}
#[test]
fn supervisor_death_kills_hung_attempt_and_module_stdio_is_private() -> Result<()> {
    let fixture = Fixture::new("hang")?;
    let mut worker = fixture.worker(5000)?;
    worker.start("parent-death")?;
    let pid = fixture.child_pid()?;
    worker.process.0.kill()?;
    worker.process.0.wait()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::read_to_string(format!("/proc/{pid}/status")) {
            Ok(status)
                if status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains('Z')) =>
            {
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            _ => {}
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            };
            anyhow::bail!("Orphaned PAM attempt survived supervisor death");
        }
        thread::sleep(Duration::from_millis(5));
    }
    let fixture = Fixture::new("noisy")?;
    let mut worker = fixture.worker(3000)?;
    worker.start("stdio")?;
    assert_eq!(worker.drive(false)?.0["status"], "authenticated");
    reaped(fixture.child_pid()?)?;
    worker.stop()?;
    Ok(())
}
#[test]
fn pam_user_changes_and_missing_private_policy_fail_closed() -> Result<()> {
    for mode in ["switch-user", "account-switch-user"] {
        let fixture = Fixture::new(mode)?;
        let mut worker = fixture.worker(3000)?;
        worker.start("identity")?;
        let result = if mode == "switch-user" {
            worker.receive(|v| v["type"] == "result")?
        } else {
            worker.drive(false)?.0
        };
        assert_ne!(result["status"], "authenticated");
        assert!(matches!(
            result["status"].as_str(),
            Some("error" | "account_denied" | "denied")
        ));
        reaped(fixture.child_pid()?)?;
        worker.stop()?;
    }
    let fixture = Fixture::new("multi")?;
    fs::write(
        fixture.dir.join("other"),
        format!(
            "auth required {} mode=permit pidfile={}\naccount required {} mode=permit\n",
            fixture.dir.join("pam_fixture.so").display(),
            fixture.dir.join("child.pid").display(),
            fixture.dir.join("pam_fixture.so").display(),
        ),
    )?;
    fs::remove_file(fixture.dir.join("hearth-private"))?;
    let mut worker = fixture.worker(1000)?;
    worker.start("missing-policy")?;
    assert_eq!(
        worker.receive(|v| v["type"] == "result")?["status"],
        "error"
    );
    assert!(!fixture.dir.join("child.pid").exists());
    worker.stop()?;
    Ok(())
}
#[test]
fn successful_attempt_does_not_leave_module_helpers_running() -> Result<()> {
    let fixture = Fixture::new("fork")?;
    let mut worker = fixture.worker(3000)?;
    worker.start("fork-helper")?;
    assert_eq!(
        worker.receive(|v| v["type"] == "result")?["status"],
        "authenticated"
    );
    let pid: i32 = fs::read_to_string(fixture.dir.join("child.pid.descendant"))?
        .trim()
        .parse()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::read_to_string(format!("/proc/{pid}/status")) {
            Ok(status)
                if status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains('Z')) =>
            {
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            _ => {}
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            };
            anyhow::bail!("PAM helper survived successful attempt");
        }
        thread::sleep(Duration::from_millis(5));
    }
    reaped(fixture.child_pid()?)?;
    worker.stop()?;
    Ok(())
}
