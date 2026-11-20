use crate::workspaces::{Instance, connect, request};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{
    collections::HashSet,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub enum Source {
    Discover,
    Instance(Instance),
}
#[derive(Deserialize)]
struct WorkspaceRef {
    id: i64,
}
#[derive(Deserialize)]
struct Monitor {
    #[serde(rename = "activeWorkspace")]
    active: WorkspaceRef,
}
#[derive(Deserialize)]
struct Client {
    workspace: WorkspaceRef,
    fullscreen: u32,
}
pub fn active_fullscreen(monitors: &[u8], clients: &[u8]) -> Result<bool> {
    ensure!(
        monitors.len() <= 1024 * 1024 && clients.len() <= 1024 * 1024,
        "Compositor reply exceeds bounds"
    );
    let monitors: Vec<Monitor> = serde_json::from_slice(monitors)?;
    let clients: Vec<Client> = serde_json::from_slice(clients)?;
    ensure!(
        monitors.len() <= 32 && clients.len() <= 4096,
        "Compositor inventory exceeds bounds"
    );
    let active: HashSet<_> = monitors
        .iter()
        .map(|m| m.active.id)
        .filter(|id| *id != 0)
        .collect();
    ensure!(
        clients.iter().all(|c| c.fullscreen <= 3),
        "Invalid fullscreen inventory"
    );
    Ok(clients
        .iter()
        .any(|c| c.fullscreen > 1 && active.contains(&c.workspace.id)))
}
fn observe(instance: &Instance, stop: &UnixStream) -> Result<bool> {
    let path = instance.directory.join(".socket.sock");
    let before = request(&path, b"j/monitors")?;
    ensure!(!stopped(stop, 0), "Fullscreen feed stopped");
    let clients = request(&path, b"j/clients")?;
    ensure!(!stopped(stop, 0), "Fullscreen feed stopped");
    let after = request(&path, b"j/monitors")?;
    let first: Vec<Monitor> = serde_json::from_slice(&before)?;
    let last: Vec<Monitor> = serde_json::from_slice(&after)?;
    ensure!(
        first.iter().map(|m| m.active.id).collect::<Vec<_>>()
            == last.iter().map(|m| m.active.id).collect::<Vec<_>>(),
        "Active workspaces changed during inspection"
    );
    active_fullscreen(&after, &clients)
}
pub struct Feed {
    stop: UnixStream,
    worker: Option<JoinHandle<()>>,
}
impl Feed {
    pub fn start(
        source: Source,
        mut publish: impl FnMut(Result<bool>) + Send + 'static,
    ) -> Result<Self> {
        let (stop, stop_reader) = UnixStream::pair()?;
        let worker = thread::Builder::new()
            .name("hearth-notification-fullscreen".into())
            .spawn(move || {
                let mut previous = None;
                let mut emit = |result: Result<bool>| {
                    let current = result.as_ref().map(|v| *v).map_err(|_| ());
                    if previous.as_ref() != Some(&current) {
                        previous = Some(current);
                        publish(result);
                    }
                };
                loop {
                    let instance = match &source {
                        Source::Discover => crate::services::discover_hyprland(),
                        Source::Instance(instance) => Ok(instance.clone()),
                    };
                    let result =
                        instance.and_then(|instance| follow(&instance, &stop_reader, &mut emit));
                    match result {
                        Ok(()) => break,
                        Err(e) => emit(Err(e)),
                    }
                    if stopped(&stop_reader, 1000) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Feed {
    fn drop(&mut self) {
        let _ = self.stop.write(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn stopped(stop: &UnixStream, timeout: i32) -> bool {
    let mut fd = libc::pollfd {
        fd: stop.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    (unsafe { libc::poll(&mut fd, 1, timeout) }) > 0
}
fn follow(
    instance: &Instance,
    stop: &UnixStream,
    publish: &mut impl FnMut(Result<bool>),
) -> Result<()> {
    if stopped(stop, 0) {
        return Ok(());
    }
    let mut events = connect(
        &instance.directory.join(".socket2.sock"),
        Instant::now() + Duration::from_millis(600),
    )?;
    publish(Ok(observe(instance, stop)?));
    let mut next = Instant::now() + Duration::from_secs(5);
    let mut pending = Vec::new();
    loop {
        let mut fds = [
            libc::pollfd {
                fd: stop.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: events.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout = next
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(5000) as i32;
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if ready < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if fds[0].revents != 0 {
            return Ok(());
        }
        let mut changed = false;
        if fds[1].revents != 0 {
            let mut bytes = [0u8; 8192];
            for _ in 0..8 {
                match events.read(&mut bytes) {
                    Ok(0) => anyhow::bail!("Compositor event stream disconnected"),
                    Ok(n) => {
                        pending.extend_from_slice(&bytes[..n]);
                        ensure!(pending.len() <= 65536, "Compositor event buffer exceeded");
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if let Some(end) = pending.iter().rposition(|b| *b == b'\n') {
                changed = true;
                pending.drain(..=end);
            }
        }
        if changed || Instant::now() >= next {
            publish(Ok(observe(instance, stop)?));
            next = Instant::now() + Duration::from_secs(5);
        }
    }
}
