use anyhow::Result;
use hearth::{
    services::{HyprlandService, MAX_LINE, ServiceEnvelope, Status},
    workspaces::Instance,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Mock {
    instance: Instance,
    bad: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}
impl Mock {
    fn new() -> Result<Self> {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("hearth-service-{}-{serial}", std::process::id()));
        std::fs::create_dir(&path)?;
        let commands = UnixListener::bind(path.join(".socket.sock"))?;
        let events = UnixListener::bind(path.join(".socket2.sock"))?;
        commands.set_nonblocking(true)?;
        events.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let bad = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let (done, invalid, count) = (stop.clone(), bad.clone(), requests.clone());
        let command_worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = commands.accept() {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    let mut buffer = [0; 64];
                    if let Ok(n) = stream.read(&mut buffer) {
                        count.fetch_add(1, Ordering::Relaxed);
                        let response: &[u8] = if invalid.load(Ordering::Relaxed) {
                            b"invalid"
                        } else if &buffer[..n] == b"j/workspaces" {
                            br#"[{"id":1,"name":"one","monitor":"DP-1","windows":2}]"#
                        } else {
                            br#"[{"name":"DP-1","focused":true,"activeWorkspace":{"id":1},"specialWorkspace":{"id":0}}]"#
                        };
                        let _ = stream.write_all(response);
                    }
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let (done, slots) = (stop.clone(), clients.clone());
        let event_worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Ok((stream, _)) = events.accept() {
                    slots.lock().expect("clients").push(stream);
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        Ok(Self {
            instance: Instance {
                signature: format!("mock-{serial}"),
                wayland_socket: "wayland-test".into(),
                directory: path,
            },
            bad,
            requests,
            clients,
            stop,
            workers: vec![command_worker, event_worker],
        })
    }
    fn event(&self) {
        for stream in self.clients.lock().expect("clients").iter_mut() {
            let _ = stream.write_all(b"workspace>>2\n");
        }
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            worker.join().expect("mock worker");
        }
        let _ = std::fs::remove_dir_all(&self.instance.directory);
    }
}
#[test]
fn events_heartbeats_unavailability_and_new_instances_are_fresh() -> Result<()> {
    let first = Mock::new()?;
    let second = Mock::new()?;
    let selected = Arc::new(AtomicBool::new(false));
    let selection = selected.clone();
    let (a, b) = (first.instance.clone(), second.instance.clone());
    let mut service = HyprlandService::with_discovery(
        move || {
            Ok(if selection.load(Ordering::Relaxed) {
                b.clone()
            } else {
                a.clone()
            })
        },
        Duration::from_millis(35),
    );
    let (read, mut stop) = UnixStream::pair()?;
    let initial = service.next(read.as_raw_fd())?.expect("initial snapshot");
    assert_eq!(initial.status, Status::Available);
    let count = first.requests.load(Ordering::Relaxed);
    first.event();
    let fresh = service.next(read.as_raw_fd())?.expect("event or heartbeat");
    assert_eq!(fresh.status, Status::Available);
    assert!(first.requests.load(Ordering::Relaxed) > count);
    first.bad.store(true, Ordering::Relaxed);
    first.event();
    let unavailable = service.next(read.as_raw_fd())?.expect("unavailable");
    assert_eq!(unavailable.status, Status::Unavailable);
    assert!(unavailable.state.is_none());
    assert!(unavailable.error.is_some());
    selected.store(true, Ordering::Relaxed);
    let mut recovered = false;
    for _ in 0..5 {
        let next = service.next(read.as_raw_fd())?.expect("recovery");
        if next
            .state
            .as_ref()
            .is_some_and(|state| state.instance == second.instance.signature)
        {
            recovered = true;
            break;
        }
    }
    assert!(recovered);
    let count = second.requests.load(Ordering::Relaxed);
    thread::sleep(Duration::from_millis(40));
    let heartbeat = service.next(read.as_raw_fd())?.expect("heartbeat");
    assert_eq!(heartbeat.status, Status::Available);
    assert!(second.requests.load(Ordering::Relaxed) > count);
    stop.write_all(&[1])?;
    assert!(service.next(read.as_raw_fd())?.is_none());
    drop(service);
    Ok(())
}
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hearth-service"));
    command
        .arg("hyprland")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("WAYLAND_DISPLAY");
    command
}
fn exited(child: &mut std::process::Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success());
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Service failed to exit promptly");
        }
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn cli_once_unavailable_sigterm_and_broken_pipe_are_clean() -> Result<()> {
    let output = command().arg("--once").output()?;
    assert!(output.status.success());
    let envelope: ServiceEnvelope = serde_json::from_slice(&output.stdout)?;
    assert_eq!(envelope.status, Status::Unavailable);
    assert!(envelope.state.is_none());
    let mut child = command().stdout(Stdio::piped()).spawn()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout")).read_line(&mut line)?;
    assert!(!line.is_empty());
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    exited(&mut child)?;
    let mut child = command().stdout(Stdio::piped()).spawn()?;
    drop(child.stdout.take());
    exited(&mut child)?;
    Ok(())
}
#[test]
fn error_envelopes_are_bounded_and_utf8_valid() -> Result<()> {
    let envelope = ServiceEnvelope::observed(Err(anyhow::anyhow!("é".repeat(10000))));
    assert!(envelope.error.as_ref().expect("error").len() <= 4096);
    let bytes = envelope.json_line()?;
    assert!(bytes.len() <= MAX_LINE);
    let parsed: ServiceEnvelope = serde_json::from_slice(&bytes)?;
    assert_eq!(parsed.status, Status::Unavailable);
    Ok(())
}

#[test]
fn sigterm_interrupts_a_partially_written_envelope_without_draining_stdout() -> Result<()> {
    use std::os::{
        fd::{FromRawFd, OwnedFd},
        unix::fs::PermissionsExt,
    };
    let directory =
        std::env::temp_dir().join(format!("hearth-backpressure-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let executable = directory.join("hyprctl");
    std::fs::write(&executable, "#!/bin/sh\nprintf '[]'\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let mut pipes = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipes.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read = unsafe { OwnedFd::from_raw_fd(pipes[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(pipes[1]) };
    assert_eq!(
        unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) },
        4096
    );
    let mut child = command()
        .arg("--once")
        .env("XDG_RUNTIME_DIR", &directory)
        .env("WAYLAND_DISPLAY", "x".repeat(12000))
        .env("PATH", &directory)
        .stdout(Stdio::from(write))
        .spawn()?;
    let mut poll = libc::pollfd {
        fd: read.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 2000) }, 1);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let result = exited(&mut child);
    std::fs::remove_dir_all(directory)?;
    result
}
