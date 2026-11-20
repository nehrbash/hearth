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
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    daemon: Option<Process>,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hearth-audio-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let mut fixture = Self { daemon: None, dir };
        fixture.start()?;
        Ok(fixture)
    }
    fn environment(&self, command: &mut Command) {
        command
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("PIPEWIRE_RUNTIME_DIR", &self.dir)
            .env("PIPEWIRE_REMOTE", "hearth-test");
    }
    fn start(&mut self) -> Result<()> {
        let mut command = Command::new("pipewire");
        self.environment(&mut command);
        command
            .arg("-c")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures/pipewire-test.conf"
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.daemon = Some(Process(
            command
                .spawn()
                .context("private PipeWire requires pipewire on PATH")?,
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.dir.join("hearth-test").exists() {
            ensure!(
                self.daemon
                    .as_mut()
                    .expect("daemon")
                    .0
                    .try_wait()?
                    .is_none(),
                "Private PipeWire exited at startup"
            );
            ensure!(
                Instant::now() < deadline,
                "Private PipeWire socket did not appear"
            );
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
    fn stop(&mut self) {
        self.daemon.take();
        let _ = fs::remove_file(self.dir.join("hearth-test"));
        let _ = fs::remove_file(self.dir.join("hearth-test.lock"));
    }
    fn worker(&self, args: &[&str]) -> Result<Worker> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hearth-audio"));
        self.environment(&mut command);
        let mut process = Process(
            command
                .args(["--remote", "hearth-test"])
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?,
        );
        let input = process.0.stdin.take().context("stdin")?;
        let output = process.0.stdout.take().context("stdout")?;
        let fd = output.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
            "nonblocking stdout"
        );
        Ok(Worker {
            process,
            input,
            output,
            pending: Vec::new(),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.dir);
    }
}
struct Worker {
    process: Process,
    input: ChildStdin,
    output: ChildStdout,
    pending: Vec<u8>,
}
impl Worker {
    fn receive(&mut self, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = self.pending.drain(..=end).collect();
                let value = serde_json::from_slice(&line)?;
                if predicate(&value) {
                    return Ok(value);
                }
            }
            let mut bytes = [0; 8192];
            match self.output.read(&mut bytes) {
                Ok(0) => anyhow::bail!(
                    "Worker closed stdout; status {:?}",
                    self.process.0.try_wait()?
                ),
                Ok(n) => self.pending.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(e) => return Err(e.into()),
            }
            ensure!(
                self.pending.len() <= 1024 * 1024,
                "Worker line exceeded bound"
            );
            ensure!(Instant::now() < deadline, "Worker response timed out");
        }
    }
    fn snapshot(&mut self) -> Result<Value> {
        let message = self.receive(|v| {
            v["status"] == "available"
                && v["state"]["nodes"].as_array().is_some_and(|nodes| {
                    nodes.len() == 2 && nodes.iter().all(|n| n["ready"] == true)
                })
        })?;
        Ok(message["state"].clone())
    }
    fn action(
        &mut self,
        snapshot: &Value,
        name: &str,
        action: Value,
        request_id: &str,
    ) -> Result<Value> {
        let node = snapshot["nodes"]
            .as_array()
            .context("nodes")?
            .iter()
            .find(|n| n["name"] == name)
            .context("node")?;
        let request = json!({"type":"action","request_id":request_id,"request":{"session":snapshot["session"],"id":node["id"],"serial":node["serial"],"action":action}});
        self.input.write_all(&serde_json::to_vec(&request)?)?;
        self.input.write_all(b"\n")?;
        self.receive(|v| v["type"] == "result" && v["request_id"] == request_id)
    }
    fn stop(&mut self) -> Result<()> {
        ensure!(
            unsafe { libc::kill(self.process.0.id() as i32, libc::SIGTERM) } == 0,
            "SIGTERM failed"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.process.0.try_wait()? {
                ensure!(status.success(), "Worker exit {status}");
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "Worker did not stop promptly");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
#[test]
fn private_nodes_volume_mute_defaults_and_stale_identity() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut worker = fixture.worker(&[])?;
    let initial = worker.snapshot()?;
    assert_eq!(initial["defaults"]["sink"], "hearth.test.sink");
    assert_eq!(initial["defaults"]["source"], "hearth.test.source");
    for name in ["hearth.test.sink", "hearth.test.source"] {
        for (id, action) in [
            ("mute", json!({"action":"mute","value":true})),
            (
                "volume",
                json!({"action":"volume","value":0.5,"unmute":true}),
            ),
            ("default", json!({"action":"default"})),
        ] {
            assert_eq!(
                worker.action(&initial, name, action, id)?["status"],
                "applied"
            );
            if id == "mute" {
                let mut observer = fixture.worker(&["--once"])?;
                let state = observer.snapshot()?;
                let observed = state["nodes"]
                    .as_array()
                    .context("nodes")?
                    .iter()
                    .find(|node| node["name"] == name)
                    .context("observed node")?;
                assert_eq!(observed["mute"], true);
            }
        }
    }
    let mut observer = fixture.worker(&["--once"])?;
    let state = observer.snapshot()?;
    for node in state["nodes"].as_array().context("nodes")? {
        assert_eq!(node["mute"], false);
        for volume in node["channel_volumes_linear"]
            .as_array()
            .context("volumes")?
        {
            assert!((volume.as_f64().context("volume")? - 0.125).abs() < 0.001);
        }
    }
    assert_eq!(state["defaults"]["configured_sink"], "hearth.test.sink");
    assert_eq!(state["defaults"]["configured_source"], "hearth.test.source");
    assert_eq!(
        worker.action(
            &initial,
            "hearth.test.sink",
            json!({"action":"volume","value":0.5,"unmute":true}),
            "unchanged-volume",
        )?["status"],
        "applied"
    );
    let mut stale = initial.clone();
    stale["session"] = json!("stale-session");
    assert_eq!(
        worker.action(
            &stale,
            "hearth.test.sink",
            json!({"action":"mute","value":true}),
            "stale-session"
        )?["status"],
        "rejected"
    );
    stale = initial;
    for node in stale["nodes"].as_array_mut().context("nodes")? {
        node["serial"] = json!(u64::MAX);
    }
    assert_eq!(
        worker.action(
            &stale,
            "hearth.test.sink",
            json!({"action":"mute","value":true}),
            "stale-serial"
        )?["status"],
        "rejected"
    );
    worker.stop()?;
    Ok(())
}
#[test]
fn daemon_restart_changes_session_and_worker_shutdown_is_bounded() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let mut worker = fixture.worker(&["--observe"])?;
    let old = worker.snapshot()?;
    fixture.stop();
    worker.receive(|v| v["status"] == "unavailable")?;
    fixture.start()?;
    let fresh = worker.snapshot()?;
    assert_ne!(old["session"], fresh["session"]);
    worker.stop()?;
    let mut worker = fixture.worker(&[])?;
    worker.snapshot()?;
    worker.input.write_all(b"{\"type\":")?;
    worker.stop()?;
    Ok(())
}
#[test]
fn unread_results_do_not_block_shutdown() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut worker = fixture.worker(&[])?;
    let initial = worker.snapshot()?;
    let node = &initial["nodes"][0];
    let mut request = serde_json::to_vec(
        &json!({"type":"action","request_id":"blocked-output","request":{"session":"stale","id":node["id"],"serial":node["serial"],"action":{"action":"mute","value":true}}}),
    )?;
    request.push(b'\n');
    let output = worker.output.as_raw_fd();
    assert_eq!(
        unsafe { libc::fcntl(output, libc::F_SETPIPE_SZ, 4096) },
        4096
    );
    let input = worker.input.as_raw_fd();
    let flags = unsafe { libc::fcntl(input, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(input, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "nonblocking stdin"
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    let (mut previous, mut stalled) = (0, 0);
    loop {
        match worker.input.write(&request) {
            Ok(n) => assert_eq!(n, request.len()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2))
            }
            Err(e) => return Err(e.into()),
        }
        thread::sleep(Duration::from_millis(25));
        let mut queued: libc::c_int = 0;
        ensure!(
            unsafe { libc::ioctl(output, libc::FIONREAD, &mut queued) } == 0,
            "queued bytes"
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
        ensure!(
            Instant::now() < deadline,
            "Worker never filled output pipe: {queued}"
        );
    }
    worker.stop()?;
    Ok(())
}

#[test]
fn delayed_bluetooth_sink_uses_explicit_metadata_and_address_guard() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut worker = fixture.worker(&[])?;
    let initial = worker.snapshot()?;
    assert!(
        initial["nodes"]
            .as_array()
            .context("nodes")?
            .iter()
            .all(|n| n["bluetooth_address"].is_null())
    );
    let create = |address: &str| -> Result<Process> {
        let mut command = Command::new("pw-cli");
        fixture.environment(&mut command);
        let args = format!(
            "{{ factory.name = support.null-audio-sink node.name = hearth.test.bluetooth media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true api.bluez5.address = \"{address}\" }}"
        );
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        writeln!(
            child.stdin.as_mut().context("pw-cli stdin")?,
            "create-node adapter {args}"
        )?;
        Ok(Process(child))
    };
    let first_creator = create("ab:cd:ef:01:23:45")?;
    let state = worker.receive(|v| {
        v["state"]["nodes"].as_array().is_some_and(|nodes| {
            nodes
                .iter()
                .any(|n| n["name"] == "hearth.test.bluetooth" && n["ready"] == true)
        })
    })?["state"]
        .clone();
    let node = state["nodes"]
        .as_array()
        .context("nodes")?
        .iter()
        .find(|n| n["name"] == "hearth.test.bluetooth")
        .context("Bluetooth sink")?
        .clone();
    assert_eq!(node["bluetooth_address"], "AB:CD:EF:01:23:45");
    let mut request = json!({"type":"action","request_id":"wrong-address","request":{"session":state["session"],"id":node["id"],"serial":node["serial"],"expected_bluetooth_address":"AB:CD:EF:01:23:46","action":{"action":"default"}}});
    serde_json::to_writer(&mut worker.input, &request)?;
    worker.input.write_all(b"\n")?;
    assert_eq!(
        worker.receive(|v| v["request_id"] == "wrong-address")?["status"],
        "rejected"
    );
    request["request_id"] = json!("right-address");
    request["request"]["expected_bluetooth_address"] = json!("AB:CD:EF:01:23:45");
    serde_json::to_writer(&mut worker.input, &request)?;
    worker.input.write_all(b"\n")?;
    assert_eq!(
        worker.receive(|v| v["request_id"] == "right-address")?["status"],
        "applied"
    );
    let mut command = Command::new("pw-cli");
    fixture.environment(&mut command);
    ensure!(
        command
            .args(["destroy", &node["id"].to_string()])
            .output()?
            .status
            .success(),
        "Destroy private sink"
    );
    worker.receive(|v| {
        v["state"]["nodes"]
            .as_array()
            .is_some_and(|nodes| !nodes.iter().any(|n| n["name"] == "hearth.test.bluetooth"))
    })?;
    drop(first_creator);
    let _second_creator = create("AB:CD:EF:01:23:46")?;
    let next = worker.receive(|v| {
        v["state"]["nodes"].as_array().is_some_and(|nodes| {
            nodes
                .iter()
                .any(|n| n["name"] == "hearth.test.bluetooth" && n["ready"] == true)
        })
    })?;
    let next = next["state"]["nodes"]
        .as_array()
        .context("nodes")?
        .iter()
        .find(|n| n["name"] == "hearth.test.bluetooth")
        .context("replacement")?;
    assert_ne!(next["serial"], node["serial"]);
    assert_eq!(next["bluetooth_address"], "AB:CD:EF:01:23:46");
    request["request_id"] = json!("replaced-sink");
    serde_json::to_writer(&mut worker.input, &request)?;
    worker.input.write_all(b"\n")?;
    assert_eq!(
        worker.receive(|v| v["request_id"] == "replaced-sink")?["status"],
        "rejected"
    );
    worker.stop()?;
    Ok(())
}
