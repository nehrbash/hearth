use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_STRING_BYTES: usize = 16 * 1024;
const MAX_ROWS: usize = 1024;
const MAX_COLUMNS: usize = 64;
const MAX_RETIRED_GENERATIONS: usize = 64;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub kind: String,
    pub workspace: String,
    pub generation: String,
    pub revision: u64,
    pub zones: Vec<Zone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<crate::activity::Activity>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ZoneType {
    Text,
    Metric,
    Table,
    Wiring,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ZoneType,
    pub display: String,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolError(String);

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProtocolError {}

impl Snapshot {
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(ProtocolError("snapshot exceeds 1 MiB".into()));
        }
        let snapshot: Self = serde_json::from_slice(bytes)
            .map_err(|error| ProtocolError(format!("invalid snapshot JSON: {error}")))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(activity) = &self.activity {
            activity
                .validate()
                .map_err(|error| ProtocolError(error.to_string()))?;
        }
        if self.version != 1 || self.kind != "snapshot" {
            return Err(ProtocolError("unsupported snapshot version or kind".into()));
        }
        for identifier in [&self.workspace, &self.generation] {
            if identifier.is_empty() || identifier.len() > 256 {
                return Err(ProtocolError("invalid workspace or generation".into()));
            }
        }
        if self.zones.len() > 64 {
            return Err(ProtocolError("snapshot exceeds 64 zones".into()));
        }
        let mut names = HashSet::new();
        for zone in &self.zones {
            if zone.name.is_empty() || zone.name.len() > 256 || !names.insert(&zone.name) {
                return Err(ProtocolError("invalid or duplicate zone name".into()));
            }
            if zone.display.len() > MAX_STRING_BYTES || zone.rows.len() > MAX_ROWS {
                return Err(ProtocolError("zone display or rows exceed limit".into()));
            }
            match zone.kind {
                ZoneType::Text | ZoneType::Metric if !zone.rows.is_empty() => {
                    return Err(ProtocolError(
                        "text and metric zones cannot contain rows".into(),
                    ));
                }
                ZoneType::Table | ZoneType::Wiring if !zone.display.is_empty() => {
                    return Err(ProtocolError(
                        "table and wiring zones require row data".into(),
                    ));
                }
                _ => {}
            }
            for row in &zone.rows {
                if row.len() > MAX_COLUMNS
                    || (zone.kind == ZoneType::Wiring && row.len() != 2)
                    || row.iter().any(|cell| cell.len() > MAX_STRING_BYTES)
                {
                    return Err(ProtocolError("invalid or oversized zone row".into()));
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct SnapshotStore {
    current: Option<Arc<Snapshot>>,
    retired: Vec<(String, String)>,
}

impl SnapshotStore {
    pub fn current(&self) -> Option<Arc<Snapshot>> {
        self.current.clone()
    }

    pub fn apply(&mut self, snapshot: Snapshot) -> Result<Arc<Snapshot>, ProtocolError> {
        snapshot.validate()?;
        if self.retired.iter().any(|(workspace, generation)| {
            workspace == &snapshot.workspace && generation == &snapshot.generation
        }) {
            return Err(ProtocolError(
                "snapshot belongs to a retired generation".into(),
            ));
        }
        if let Some(current) = &self.current {
            if snapshot.workspace == current.workspace && snapshot.generation == current.generation
            {
                if snapshot.revision <= current.revision {
                    return Err(ProtocolError("stale snapshot revision".into()));
                }
            } else {
                if self.retired.len() == MAX_RETIRED_GENERATIONS {
                    return Err(ProtocolError(
                        "retired generation history is full; restart the listener".into(),
                    ));
                }
                self.retired
                    .push((current.workspace.clone(), current.generation.clone()));
            }
        }
        let snapshot = Arc::new(snapshot);
        self.current = Some(snapshot.clone());
        Ok(snapshot)
    }
}

#[derive(Default)]
struct Mailbox {
    pending: Option<Arc<Snapshot>>,
    error: Option<String>,
}

pub struct SnapshotListener {
    path: PathBuf,
    identity: (u64, u64),
    stop: Arc<AtomicBool>,
    mailbox: Arc<Mutex<Mailbox>>,
    worker: Option<JoinHandle<()>>,
    wake: UnixStream,
    notifications: UnixStream,
}

impl SnapshotListener {
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("socket needs a private parent directory"))?;
        let metadata = fs::symlink_metadata(parent)?;
        // The socket's parent is the access boundary before chmod can run.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket directory must belong to this user and have mode 0700",
            ));
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let metadata = fs::symlink_metadata(&path)?;
        let identity = (metadata.dev(), metadata.ino());
        let stop = Arc::new(AtomicBool::new(false));
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let (wake, worker_wake) = UnixStream::pair()?;
        let (notifications, worker_notify) = UnixStream::pair()?;
        notifications.set_nonblocking(true)?;
        worker_notify.set_nonblocking(true)?;
        let worker_stop = stop.clone();
        let worker_mailbox = mailbox.clone();
        let worker = thread::Builder::new()
            .name("hearth-snapshots".into())
            .spawn(move || {
                receive(
                    listener,
                    worker_wake,
                    worker_notify,
                    &worker_stop,
                    &worker_mailbox,
                )
            })?;
        Ok(Self {
            path,
            identity,
            stop,
            mailbox,
            worker: Some(worker),
            wake,
            notifications,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn update_fd(&self) -> RawFd {
        self.notifications.as_raw_fd()
    }

    /// Drain readiness before consuming the latest snapshot or error.
    pub fn drain_notifications(&self) -> io::Result<()> {
        let mut socket = &self.notifications;
        let mut bytes = [0; 128];
        loop {
            match socket.read(&mut bytes) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn take_latest(&self) -> Option<Arc<Snapshot>> {
        self.mailbox.try_lock().ok()?.pending.take()
    }

    pub fn take_error(&self) -> Option<String> {
        self.mailbox.try_lock().ok()?.error.take()
    }
}

impl Drop for SnapshotListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.write_all(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && (metadata.dev(), metadata.ino()) == self.identity
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn receive(
    listener: UnixListener,
    wake: UnixStream,
    mut notify: UnixStream,
    stop: &AtomicBool,
    mailbox: &Mutex<Mailbox>,
) {
    let mut store = SnapshotStore::default();
    let mut descriptors = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    while !stop.load(Ordering::Acquire) {
        // Both descriptors remain owned by this thread until poll returns.
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if let Ok(mut mailbox) = mailbox.lock() {
                mailbox.error = Some(error.to_string());
            }
            let _ = notify.write(&[1]);
            break;
        }
        if descriptors[1].revents != 0 || stop.load(Ordering::Acquire) {
            break;
        }
        if descriptors[0].revents & libc::POLLIN == 0 {
            break;
        }
        let result = listener
            .accept()
            .and_then(|(stream, _)| read_snapshot(stream))
            .map_err(|error| ProtocolError(error.to_string()))
            .and_then(|snapshot| store.apply(snapshot));
        if let Ok(mut mailbox) = mailbox.lock() {
            match result {
                Ok(snapshot) => {
                    mailbox.pending = Some(snapshot);
                    mailbox.error = None;
                }
                Err(error) => mailbox.error = Some(error.to_string()),
            }
        }
        // A full notification socket is already readable; the mailbox keeps the latest value.
        let _ = notify.write(&[1]);
    }
}

fn read_snapshot(mut stream: UnixStream) -> io::Result<Snapshot> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "snapshot deadline exceeded"))?;
        stream.set_read_timeout(Some(remaining))?;
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot needs a newline",
            ));
        }
        let end = chunk[..count].iter().position(|byte| *byte == b'\n');
        let part = &chunk[..end.unwrap_or(count)];
        if bytes.len() + part.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(part);
        if end.is_some() {
            return Snapshot::decode(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_MESSAGE_BYTES, Snapshot, SnapshotListener, SnapshotStore, ZoneType};
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    const FIXTURE: &[u8] = include_bytes!("../fixtures/weft-zones.json");
    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hearth-protocol-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("private test directory");
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("clean test directory");
        }
    }

    fn send(path: &std::path::Path, bytes: &[u8]) {
        let mut stream = UnixStream::connect(path).expect("connect snapshot socket");
        stream.write_all(bytes).expect("send snapshot");
        stream.write_all(b"\n").expect("finish snapshot");
    }

    fn wait_for<T>(mut operation: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(value) = operation() {
                return value;
            }
            assert!(Instant::now() < deadline, "I/O worker did not respond");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn decodes_all_zone_types_and_preserves_data() {
        let snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        assert_eq!(snapshot.zones.len(), 4);
        assert_eq!(snapshot.zones[0].kind, ZoneType::Text);
        assert_eq!(snapshot.zones[1].kind, ZoneType::Metric);
        assert_eq!(snapshot.zones[2].rows[1], ["Runtime", "Ready"]);
        assert_eq!(snapshot.zones[3].kind, ZoneType::Wiring);
        assert_eq!(
            Snapshot::decode(&serde_json::to_vec(&snapshot).expect("JSON")).expect("round trip"),
            snapshot
        );
    }

    #[test]
    fn rejects_unknown_protocol_shapes_and_bounds() {
        let text = std::str::from_utf8(FIXTURE).expect("UTF-8 fixture");
        for invalid in [
            text.replace("\"version\": 1", "\"version\": 2"),
            text.replace("\"text\"", "\"shader\""),
            text.replace("\"revision\": 1", "\"revision\": -1"),
            text.replace("\"revision\": 1", "\"revision\": 18446744073709551616"),
            text.replace("\"kind\": \"snapshot\"", "\"kind\": \"evaluate\""),
            text.replace("\"generation\": \"fixture-1\"", "\"generation\": \"\""),
        ] {
            assert!(Snapshot::decode(invalid.as_bytes()).is_err());
        }
        assert!(Snapshot::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]).is_err());
        let mut snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        snapshot.zones[3].rows[0].push("third endpoint".into());
        assert!(snapshot.validate().is_err());
        snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        snapshot.zones[0].display = "x".repeat(16 * 1024 + 1);
        assert!(snapshot.validate().is_err());
        snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        snapshot.zones.push(snapshot.zones[0].clone());
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn stale_or_invalid_updates_leave_the_last_valid_snapshot() {
        let mut store = SnapshotStore::default();
        let snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        store.apply(snapshot.clone()).expect("first snapshot");
        assert!(store.apply(snapshot.clone()).is_err());
        let mut invalid = snapshot.clone();
        invalid.version = 99;
        assert!(store.apply(invalid).is_err());
        assert_eq!(*store.current().expect("retained snapshot"), snapshot);
        let mut next = snapshot;
        next.revision = 0;
        next.generation = "restarted".into();
        store
            .apply(next.clone())
            .expect("new generation resets revision");
        assert_eq!(*store.current().expect("new generation"), next);
    }

    #[test]
    fn retired_generations_cannot_replay_after_a_restart() {
        let mut store = SnapshotStore::default();
        let mut old = Snapshot::decode(FIXTURE).expect("fixture");
        old.revision = 9;
        store.apply(old.clone()).expect("old generation");
        let mut new = old.clone();
        new.generation = "new-generation".into();
        new.revision = 1;
        store.apply(new.clone()).expect("new generation");
        old.revision = 1;
        assert!(
            store
                .apply(old)
                .expect_err("retired generation")
                .to_string()
                .contains("retired")
        );
        assert_eq!(*store.current().expect("current snapshot"), new);
    }

    #[test]
    fn full_generation_history_refuses_new_baselines_without_eviction() {
        let mut store = SnapshotStore::default();
        let original = Snapshot::decode(FIXTURE).expect("fixture");
        store.apply(original.clone()).expect("first generation");
        let mut latest = original.clone();
        for number in 1..=super::MAX_RETIRED_GENERATIONS {
            latest.generation = format!("generation-{number}");
            store.apply(latest.clone()).expect("bounded retirement");
        }
        let mut excess = latest.clone();
        excess.generation = "history-full".into();
        assert!(
            store
                .apply(excess)
                .expect_err("history full")
                .to_string()
                .contains("restart the listener")
        );
        assert_eq!(*store.current().expect("retained baseline"), latest);
        assert!(
            store
                .apply(original)
                .expect_err("first generation still retired")
                .to_string()
                .contains("retired")
        );
        latest.revision += 1;
        store
            .apply(latest.clone())
            .expect("current generation still updates");
        assert_eq!(*store.current().expect("updated baseline"), latest);
    }

    #[test]
    fn listener_recovers_from_malformed_clients_and_reconnects() {
        let directory = Directory::new();
        let listener = SnapshotListener::bind(directory.0.join("hearth.sock")).expect("listener");
        assert_eq!(
            fs::metadata(listener.path())
                .expect("socket")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        send(listener.path(), b"not JSON");
        assert!(wait_for(|| listener.take_error()).contains("JSON"));
        send(
            listener.path(),
            &serde_json::to_vec(&Snapshot::decode(FIXTURE).expect("fixture")).expect("JSON"),
        );
        assert_eq!(wait_for(|| listener.take_latest()).revision, 1);
        send(
            listener.path(),
            &serde_json::to_vec(&Snapshot::decode(FIXTURE).expect("fixture")).expect("JSON"),
        );
        assert!(wait_for(|| listener.take_error()).contains("stale"));
        assert!(listener.take_latest().is_none());
        let mut next = Snapshot::decode(FIXTURE).expect("fixture");
        next.revision = 2;
        send(listener.path(), &serde_json::to_vec(&next).expect("JSON"));
        assert_eq!(wait_for(|| listener.take_latest()).revision, 2);
    }

    #[test]
    fn simultaneous_clients_cannot_roll_back_the_latest_revision() {
        let directory = Directory::new();
        let listener = SnapshotListener::bind(directory.0.join("hearth.sock")).expect("listener");
        let workers: Vec<_> = (1..=8)
            .map(|revision| {
                let path = listener.path().to_owned();
                thread::spawn(move || {
                    let mut snapshot = Snapshot::decode(FIXTURE).expect("fixture");
                    snapshot.revision = revision;
                    send(&path, &serde_json::to_vec(&snapshot).expect("JSON"));
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("client");
        }
        wait_for(|| {
            listener
                .take_latest()
                .filter(|snapshot| snapshot.revision == 8)
        });
    }

    #[test]
    fn never_steals_an_existing_socket_and_shutdown_is_bounded() {
        let directory = Directory::new();
        let path = directory.0.join("hearth.sock");
        let listener = SnapshotListener::bind(&path).expect("listener");
        assert!(SnapshotListener::bind(&path).is_err());
        let _idle = UnixStream::connect(&path).expect("idle client");
        let started = Instant::now();
        drop(listener);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!path.exists());
        let replacement = SnapshotListener::bind(&path).expect("rebind after shutdown");
        drop(replacement);
    }

    #[test]
    fn rejects_shared_socket_directories() {
        let directory = Directory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755))
            .expect("set shared mode");
        assert!(SnapshotListener::bind(directory.0.join("hearth.sock")).is_err());
    }
    #[test]
    fn update_fd_wakes_for_errors_and_snapshots_then_drains() {
        let directory = Directory::new();
        let listener = SnapshotListener::bind(directory.0.join("hearth.sock")).expect("listener");
        let mut descriptor = libc::pollfd {
            fd: listener.update_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        send(listener.path(), b"invalid");
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 3000) }, 1);
        listener
            .drain_notifications()
            .expect("drain error notification");
        assert!(listener.take_error().is_some());
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        let snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        send(
            listener.path(),
            &serde_json::to_vec(&snapshot).expect("JSON"),
        );
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 3000) }, 1);
        listener
            .drain_notifications()
            .expect("drain snapshot notification");
        assert_eq!(listener.take_latest().expect("latest").revision, 1);
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
    }

    #[test]
    fn an_idle_client_times_out_without_blocking_the_next_client() {
        let directory = Directory::new();
        let listener = SnapshotListener::bind(directory.0.join("hearth.sock")).expect("listener");
        let _idle = UnixStream::connect(listener.path()).expect("idle client");
        wait_for(|| listener.take_error());
        let snapshot = Snapshot::decode(FIXTURE).expect("fixture");
        send(
            listener.path(),
            &serde_json::to_vec(&snapshot).expect("JSON"),
        );
        assert_eq!(wait_for(|| listener.take_latest()).revision, 1);
    }
}
