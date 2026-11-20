use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_BYTES: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_millis(600);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub focused: bool,
    pub active_workspace: i64,
    pub special_workspace: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub instance: String,
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
}

impl WorkspaceSnapshot {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_signature(&self.instance),
            "Invalid compositor instance"
        );
        ensure!(
            self.monitors.len() <= 32 && self.workspaces.len() <= 512,
            "Compositor state exceeds collection limits"
        );
        let mut monitors = HashSet::new();
        let mut ids = HashSet::new();
        for monitor in &self.monitors {
            ensure!(
                !monitor.name.is_empty()
                    && monitor.name.len() <= 256
                    && monitors.insert(&monitor.name),
                "Invalid or duplicate monitor"
            );
        }
        ensure!(
            self.monitors.iter().filter(|m| m.focused).count() <= 1,
            "Multiple focused monitors"
        );
        for workspace in &self.workspaces {
            ensure!(
                ids.insert(workspace.id)
                    && workspace.name.len() <= 1024
                    && workspace.monitor.len() <= 256,
                "Invalid or duplicate workspace"
            );
        }
        Ok(())
    }

    pub fn from_hyprland(instance: &str, monitors: &[u8], workspaces: &[u8]) -> Result<Self> {
        ensure!(
            monitors.len() <= MAX_BYTES && workspaces.len() <= MAX_BYTES,
            "Compositor reply exceeds 1 MiB"
        );
        #[derive(Deserialize)]
        struct WorkspaceRef {
            id: i64,
        }
        #[derive(Deserialize)]
        struct RawMonitor {
            name: String,
            focused: bool,
            #[serde(rename = "activeWorkspace")]
            active: WorkspaceRef,
            #[serde(rename = "specialWorkspace")]
            special: WorkspaceRef,
        }
        let raw: Vec<RawMonitor> = serde_json::from_slice(monitors)?;
        let mut result = Self {
            instance: instance.into(),
            monitors: raw
                .into_iter()
                .map(|m| Monitor {
                    name: m.name,
                    focused: m.focused,
                    active_workspace: m.active.id,
                    special_workspace: (m.special.id != 0).then_some(m.special.id),
                })
                .collect(),
            workspaces: serde_json::from_slice(workspaces)?,
        };
        result.monitors.sort_by(|a, b| a.name.cmp(&b.name));
        result.workspaces.sort_by_key(|w| w.id);
        result.validate()?;
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub struct Instance {
    pub signature: String,
    pub wayland_socket: String,
    pub directory: PathBuf,
}

fn valid_signature(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

impl Instance {
    pub fn from_instances_json(
        runtime: &Path,
        display: &str,
        explicit: Option<&str>,
        bytes: &[u8],
    ) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "Instance list exceeds 1 MiB");
        #[derive(Deserialize)]
        struct Record {
            instance: String,
            wl_socket: String,
        }
        let instances: Vec<Record> = serde_json::from_slice(bytes)?;
        let matches: Vec<_> = instances
            .into_iter()
            .filter(|i| {
                explicit.map_or(i.wl_socket == display, |signature| {
                    i.instance == signature && i.wl_socket == display
                })
            })
            .collect();
        ensure!(
            matches.len() == 1,
            "Expected one compositor matching Wayland display {display}; select an explicit current instance"
        );
        let selected = &matches[0];
        ensure!(
            valid_signature(&selected.instance),
            "Invalid compositor signature"
        );
        Ok(Self {
            signature: selected.instance.clone(),
            wayland_socket: selected.wl_socket.clone(),
            directory: runtime.join("hypr").join(&selected.instance),
        })
    }

    pub fn discover(explicit: Option<&str>) -> Result<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is missing")?;
        let display = std::env::var("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is missing")?;
        let output = Command::new("hyprctl")
            .args(["instances", "-j"])
            .output()
            .context("Discover compositor instances")?;
        ensure!(output.status.success(), "hyprctl instance discovery failed");
        // Session environments can retain the signature of an exited compositor.
        Self::from_instances_json(Path::new(&runtime), &display, explicit, &output.stdout)
    }

    pub fn snapshot(&self) -> Result<WorkspaceSnapshot> {
        let monitors = request(&self.directory.join(".socket.sock"), b"j/monitors")?;
        let workspaces = request(&self.directory.join(".socket.sock"), b"j/workspaces")?;
        let after = request(&self.directory.join(".socket.sock"), b"j/monitors")?;
        let before = WorkspaceSnapshot::from_hyprland(&self.signature, &monitors, &workspaces)?;
        let after = WorkspaceSnapshot::from_hyprland(&self.signature, &after, &workspaces)?;
        ensure!(
            before.monitors == after.monitors,
            "Monitor/workspace selection changed during inspection"
        );
        Ok(after)
    }
}

fn wait_fd(fd: RawFd, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Compositor IPC deadline exceeded",
            ));
        }
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut poll,
                1,
                left.as_millis().max(1).min(i32::MAX as u128) as i32,
            )
        };
        if result > 0 {
            return Ok(());
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
}

fn connect(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid IPC socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let result = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        wait_fd(fd.as_raw_fd(), libc::POLLOUT, deadline)?;
    }
    let stream = UnixStream::from(fd);
    if let Some(error) = stream.take_error()? {
        return Err(error);
    }
    Ok(stream)
}

fn request(path: &Path, command: &[u8]) -> Result<Vec<u8>> {
    let deadline = Instant::now() + DEADLINE;
    let mut stream = connect(path, deadline)?;
    let mut written = 0;
    while written < command.len() {
        match stream.write(&command[written..]) {
            Ok(0) => bail!("Compositor closed command socket"),
            Ok(count) => written += count,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLOUT, deadline)?
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(result),
            Ok(count) => {
                ensure!(
                    result.len() + count <= MAX_BYTES,
                    "Compositor reply exceeds 1 MiB"
                );
                result.extend_from_slice(&buffer[..count]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(stream.as_raw_fd(), libc::POLLIN, deadline)?
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
        ensure!(
            Instant::now() < deadline,
            "Compositor IPC deadline exceeded"
        );
    }
}

#[derive(Default)]
pub struct WorkspaceUpdate {
    pub snapshot: Option<WorkspaceSnapshot>,
    pub error: Option<String>,
}

pub struct WorkspaceFeed {
    updates: Arc<Mutex<WorkspaceUpdate>>,
    notifications: UnixStream,
    stop: UnixStream,
    worker: Option<JoinHandle<()>>,
}

impl WorkspaceFeed {
    pub fn start(instance: Instance) -> io::Result<Self> {
        let (notifications, mut notify) = UnixStream::pair()?;
        notifications.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let (stop, stop_reader) = UnixStream::pair()?;
        let updates = Arc::new(Mutex::new(WorkspaceUpdate::default()));
        let worker_updates = updates.clone();
        let worker = thread::Builder::new()
            .name("hearth-compositor".into())
            .spawn(move || {
                let mut previous = None;
                let mut previous_error = None;
                let mut publish = |result: Result<WorkspaceSnapshot>| {
                    if let Ok(mut slot) = worker_updates.lock() {
                        match result {
                            Ok(snapshot) => {
                                if previous.as_ref() == Some(&snapshot) && previous_error.is_none()
                                {
                                    return;
                                }
                                previous = Some(snapshot.clone());
                                previous_error = None;
                                slot.snapshot = Some(snapshot);
                                slot.error = None;
                            }
                            Err(error) => {
                                let message = error.to_string();
                                if previous_error.as_ref() == Some(&message) {
                                    return;
                                }
                                previous_error = Some(message.clone());
                                slot.error = Some(message);
                            }
                        }
                        let _ = notify.write(&[1]);
                    }
                };
                loop {
                    let result = follow_events(&instance, &stop_reader, &mut publish);
                    match result {
                        Ok(()) => break,
                        Err(error) => publish(Err(error)),
                    }
                    if stop_ready(&stop_reader, 1000) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            updates,
            notifications,
            stop,
            worker: Some(worker),
        })
    }

    pub fn update_fd(&self) -> RawFd {
        self.notifications.as_raw_fd()
    }

    pub fn drain_notifications(&self) -> io::Result<()> {
        let mut reader = &self.notifications;
        let mut bytes = [0; 128];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    pub fn take_update(&self) -> WorkspaceUpdate {
        self.updates
            .lock()
            .map(|mut slot| std::mem::take(&mut *slot))
            .unwrap_or_default()
    }
}

impl Drop for WorkspaceFeed {
    fn drop(&mut self) {
        let _ = self.stop.write(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn stop_ready(stop: &UnixStream, timeout: i32) -> bool {
    let mut poll = libc::pollfd {
        fd: stop.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut poll, 1, timeout) > 0 }
}

fn relevant_event(line: &[u8]) -> bool {
    let event = line.split(|byte| *byte == b'>').next().unwrap_or_default();
    [
        b"workspace".as_slice(),
        b"focusedmon",
        b"createworkspace",
        b"destroyworkspace",
        b"moveworkspace",
        b"monitoradded",
        b"monitorremoved",
        b"openwindow",
        b"closewindow",
        b"movewindow",
        b"activespecial",
        b"configreloaded",
    ]
    .iter()
    .any(|prefix| event.starts_with(prefix))
}

fn follow_events(
    instance: &Instance,
    stop: &UnixStream,
    publish: &mut impl FnMut(Result<WorkspaceSnapshot>),
) -> Result<()> {
    let mut stream = connect(
        &instance.directory.join(".socket2.sock"),
        Instant::now() + DEADLINE,
    )?;
    // Subscribe before reading state so changes during the initial fetch remain queued.
    publish(instance.snapshot());
    let mut partial = Vec::new();
    loop {
        let mut fds = [
            libc::pollfd {
                fd: stop.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fds[0].revents != 0 {
            return Ok(());
        }
        if stop_ready(stop, 25) {
            return Ok(());
        }
        let mut changed = false;
        let mut buffer = [0; 8192];
        let mut received = 0;
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => bail!("Compositor event socket disconnected; retaining last known state"),
                Ok(count) => {
                    partial.extend_from_slice(&buffer[..count]);
                    received += count;
                    ensure!(partial.len() <= 65536, "Compositor event exceeds 64 KiB");
                    while let Some(end) = partial.iter().position(|byte| *byte == b'\n') {
                        changed |= relevant_event(&partial[..end]);
                        partial.drain(..=end);
                    }
                    if received >= 65536 {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        if changed {
            publish(instance.snapshot());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    const MONITORS: &[u8] = br#"[{"name":"DP-1","focused":true,"activeWorkspace":{"id":12},"specialWorkspace":{"id":-98}}]"#;
    const WORKSPACES: &[u8] = br#"[{"id":12,"name":"writing","monitor":"DP-1","windows":2},{"id":-98,"name":"special:mini","monitor":"DP-1","windows":1}]"#;

    #[test]
    fn discovers_current_display_and_rejects_ambiguous_or_stale_explicit_instances() {
        let instances=br#"[{"instance":"current","wl_socket":"wayland-1"},{"instance":"other","wl_socket":"wayland-2"}]"#;
        let instance = Instance::from_instances_json(
            Path::new("/run/user/1000"),
            "wayland-1",
            None,
            instances,
        )
        .expect("current");
        assert_eq!(instance.signature, "current");
        assert!(
            Instance::from_instances_json(Path::new("/tmp"), "wayland-1", Some("stale"), instances)
                .is_err()
        );
        assert!(
            Instance::from_instances_json(Path::new("/tmp"), "wayland-1", Some("other"), instances)
                .is_err()
        );
        let ambiguous=br#"[{"instance":"a","wl_socket":"wayland-1"},{"instance":"b","wl_socket":"wayland-1"}]"#;
        assert!(
            Instance::from_instances_json(Path::new("/tmp"), "wayland-1", None, ambiguous).is_err()
        );
        assert!(
            Instance::from_instances_json(Path::new("/tmp"), "wayland-1", Some("a"), ambiguous)
                .is_ok()
        );
    }

    #[test]
    fn preserves_named_and_negative_workspaces_and_rejects_duplicates() {
        let mut snapshot =
            WorkspaceSnapshot::from_hyprland("current", MONITORS, WORKSPACES).expect("snapshot");
        assert_eq!(snapshot.workspaces[0].id, -98);
        assert_eq!(snapshot.workspaces[1].name, "writing");
        snapshot.workspaces.push(snapshot.workspaces[0].clone());
        assert!(snapshot.validate().is_err());
        assert!(WorkspaceSnapshot::from_hyprland("../other", MONITORS, WORKSPACES).is_err());
        assert!(WorkspaceSnapshot::from_hyprland("current", b"[]", b"bad").is_err());
    }

    #[test]
    fn ignores_title_noise_but_refreshes_selection_and_occupancy() {
        assert!(!relevant_event(b"windowtitle>>address,title"));
        for event in [
            "workspacev2>>12,writing",
            "focusedmon>>DP-1,12",
            "openwindow>>address",
            "closewindow>>address",
            "movewindowv2>>address,12",
            "activespecial>>special:mini,DP-1",
        ] {
            assert!(relevant_event(event.as_bytes()));
        }
    }

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hearth-workspaces-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("temp dir");
        path
    }

    fn await_update(feed: &WorkspaceFeed) -> WorkspaceUpdate {
        let mut fd = libc::pollfd {
            fd: feed.update_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 3000) }, 1);
        feed.drain_notifications().expect("drain");
        feed.take_update()
    }

    #[test]
    fn mock_compositor_publishes_initial_state_events_and_disconnect_without_dispatch() {
        let directory = temp_dir();
        let commands =
            UnixListener::bind(directory.join(".socket.sock")).expect("command listener");
        let events = UnixListener::bind(directory.join(".socket2.sock")).expect("events listener");
        let (advance, receive) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut event_stream, _) = events.accept().expect("event client");
            for cycle in 0..2 {
                if cycle == 1 {
                    receive.recv().expect("advance");
                    event_stream
                        .write_all(b"windowtitle>>ignored\nworkspacev2>>12,writing\n")
                        .expect("event");
                }
                for command in [b"j/monitors".as_slice(), b"j/workspaces", b"j/monitors"] {
                    let (mut stream, _) = commands.accept().expect("command");
                    let mut bytes = vec![0; command.len()];
                    stream.read_exact(&mut bytes).expect("request");
                    assert_eq!(bytes, command);
                    let changed = String::from_utf8(WORKSPACES.to_vec())
                        .expect("utf8")
                        .replace("\"windows\":2", "\"windows\":3");
                    let reply = if command == b"j/monitors" {
                        MONITORS
                    } else if cycle == 0 {
                        WORKSPACES
                    } else {
                        changed.as_bytes()
                    };
                    stream.write_all(reply).expect("reply");
                }
            }
            receive.recv().expect("disconnect");
        });
        let feed = WorkspaceFeed::start(Instance {
            signature: "mock".into(),
            wayland_socket: "test".into(),
            directory: directory.clone(),
        })
        .expect("feed");
        let first = await_update(&feed);
        assert!(first.error.is_none());
        assert_eq!(first.snapshot.expect("snapshot").workspaces[1].windows, 2);
        advance.send(()).expect("event");
        let changed = await_update(&feed);
        assert_eq!(
            changed.snapshot.expect("updated snapshot").workspaces[1].windows,
            3
        );
        advance.send(()).expect("disconnect");
        let disconnected = await_update(&feed);
        assert!(disconnected.error.is_some());
        assert!(disconnected.snapshot.is_none());
        let start = Instant::now();
        drop(feed);
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().expect("server");
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn stalled_ipc_is_bounded() {
        let directory = temp_dir();
        let path = directory.join("socket");
        let listener = UnixListener::bind(&path).expect("listener");
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("accept");
            thread::sleep(Duration::from_millis(800));
        });
        let start = Instant::now();
        assert!(request(&path, b"j/workspaces").is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        server.join().expect("server");
        std::fs::remove_dir_all(directory).expect("cleanup");
    }
}
