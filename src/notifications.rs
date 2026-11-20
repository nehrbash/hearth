use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zbus::{
    blocking::{Connection, connection::Builder},
    fdo::{RequestNameFlags, RequestNameReply},
    zvariant::OwnedValue,
};

const NAME: &str = "org.freedesktop.Notifications";
const OBJECT: &str = "/org/freedesktop/Notifications";
const MAX_STATE: usize = 4 * 1024 * 1024;
const MAX_INPUT: usize = 16384;

#[derive(Clone, Debug)]
pub struct Config {
    pub bus_address: Option<String>,
    pub socket: PathBuf,
    pub state_file: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub key: String,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub id: u32,
    pub revision: u64,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    pub urgency: u8,
    pub resident: bool,
    pub transient: bool,
    pub app_icon: String,
    pub image_path: Option<String>,
    pub image: Option<Image>,
    pub desktop_entry: Option<String>,
    pub category: Option<String>,
    pub popup: bool,
    pub actionable: bool,
    pub closed_reason: Option<u32>,
    pub created_at_ms: u64,
    #[serde(skip)]
    deadline: Option<Instant>,
    #[serde(skip)]
    popup_deadline: Option<Instant>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub epoch: String,
    pub revision: u64,
    pub dnd: bool,
    pub fullscreen: bool,
    pub active: Vec<Notification>,
    pub history: Vec<Notification>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Disk {
    version: u32,
    next_id: u64,
    dnd: bool,
    records: Vec<Notification>,
}
#[derive(Clone)]
struct State {
    snapshot: Snapshot,
    next_id: u64,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: u64,
    pub epoch: Option<String>,
    pub revision: Option<u64>,
    pub command: Control,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Inspect,
    Subscribe,
    Dismiss { id: u32 },
    Invoke { id: u32, key: String },
    ClearAll,
    ClearGroup { app_name: String },
    SetDnd { value: bool },
    SetFullscreen { value: bool },
}
#[derive(Clone)]
struct Core {
    state: Arc<Mutex<State>>,
    path: PathBuf,
    bus: Connection,
}

fn bounded(text: &str, limit: usize) -> Result<()> {
    ensure!(
        text.len() <= limit,
        "Notification string exceeds {limit} bytes"
    );
    Ok(())
}
fn millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("Missing parent directory")?;
    if !parent.exists() {
        fs::DirBuilder::new().recursive(true).create(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(parent)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "Notification directory must be owned and private"
    );
    Ok(())
}
fn load(path: &Path) -> Result<State> {
    private_parent(path)?;
    let disk = if path.exists() {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
            "Notification state must be owned and private"
        );
        let mut bytes = Vec::new();
        file.take((MAX_STATE + 1) as u64).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= MAX_STATE, "Notification state too large");
        let disk: Disk = serde_json::from_slice(&bytes)?;
        ensure!(
            disk.version == 1
                && disk.records.len() <= 100
                && disk.next_id > 0
                && disk.next_id <= u32::MAX as u64 + 1,
            "Invalid notification state"
        );
        let mut ids = std::collections::BTreeSet::new();
        for record in &disk.records {
            validate_record(record)?;
            ensure!(
                ids.insert(record.id) && u64::from(record.id) < disk.next_id && !record.transient,
                "Invalid persisted notification identity"
            );
        }
        disk
    } else {
        Disk {
            version: 1,
            next_id: 1,
            dnd: false,
            records: Vec::new(),
        }
    };
    let history = disk
        .records
        .into_iter()
        .map(|mut n| {
            n.actionable = false;
            n.popup = false;
            n.actions.clear();
            n.deadline = None;
            n.popup_deadline = None;
            n
        })
        .collect();
    Ok(State {
        next_id: disk.next_id,
        snapshot: Snapshot {
            epoch: format!("{}-{}-{}", std::process::id(), millis(), rand_nonce()?),
            revision: 0,
            dnd: disk.dnd,
            fullscreen: false,
            active: vec![],
            history,
        },
    })
}
fn rand_nonce() -> Result<u64> {
    let mut bytes = [0; 8];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}
fn validate_record(n: &Notification) -> Result<()> {
    ensure!(
        n.id > 0 && n.urgency <= 2 && n.actions.len() <= 32,
        "Invalid notification record"
    );
    for s in [&n.app_name, &n.summary, &n.app_icon] {
        bounded(s, 4096)?;
    }
    bounded(&n.body, 16384)?;
    for s in [&n.image_path, &n.desktop_entry, &n.category]
        .into_iter()
        .flatten()
    {
        bounded(s, 4096)?;
    }
    for a in &n.actions {
        bounded(&a.key, 4096)?;
        bounded(&a.label, 4096)?;
    }
    if let Some(i) = &n.image {
        ensure!(
            i.width > 0
                && i.height > 0
                && i.width <= 512
                && i.height <= 512
                && i.rgba.len() == i.width as usize * i.height as usize * 4
                && i.rgba.len() <= 1024 * 1024,
            "Invalid notification image"
        );
    }
    Ok(())
}
fn persist(path: &Path, state: &State) -> Result<()> {
    let mut records: Vec<_> = state
        .snapshot
        .active
        .iter()
        .chain(&state.snapshot.history)
        .filter(|n| !n.transient)
        .cloned()
        .collect();
    records.truncate(100);
    let disk = Disk {
        version: 1,
        next_id: state.next_id,
        dnd: state.snapshot.dnd,
        records,
    };
    let bytes = serde_json::to_vec(&disk)?;
    ensure!(
        bytes.len() <= MAX_STATE,
        "Notification persistence exceeds 4 MiB"
    );
    let tmp = path.with_extension(format!("tmp-{}", rand_nonce()?));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        fs::File::open(path.parent().context("Missing state parent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
impl Core {
    fn snapshot(&self) -> Result<Snapshot> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Notification state poisoned"))?
            .snapshot
            .clone())
    }
    fn change<T>(&self, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Notification state poisoned"))?;
        let mut next = state.clone();
        let result = f(&mut next)?;
        next.snapshot.revision = next
            .snapshot
            .revision
            .checked_add(1)
            .context("Notification revision exhausted")?;
        ensure!(
            serde_json::to_vec(&next.snapshot)?.len() <= MAX_STATE,
            "Notification state exceeds 4 MiB"
        );
        persist(&self.path, &next)?;
        *state = next;
        Ok(result)
    }
    fn signal_closed(&self, id: u32, reason: u32) -> Result<()> {
        self.bus.emit_signal(
            None::<&str>,
            OBJECT,
            NAME,
            "NotificationClosed",
            &(id, reason),
        )?;
        Ok(())
    }
    fn close(&self, id: u32, reason: u32) -> Result<()> {
        self.change(|s| close_state(s, id, reason))?;
        self.signal_closed(id, reason)
    }
    fn control(&self, request: &Request) -> Result<()> {
        let mut signals = Vec::new();
        let mut action = None;
        self.change(|s| {
            ensure!(
                request.epoch.as_ref() == Some(&s.snapshot.epoch)
                    && request.revision == Some(s.snapshot.revision),
                "Stale notification state"
            );
            match &request.command {
                Control::Inspect | Control::Subscribe => bail!("Read request is not a mutation"),
                Control::Dismiss { id } => {
                    close_state(s, *id, 2)?;
                    signals.push((*id, 2));
                }
                Control::Invoke { id, key } => {
                    let n = s
                        .snapshot
                        .active
                        .iter()
                        .find(|n| n.id == *id)
                        .context("Unknown active notification")?;
                    ensure!(
                        n.actionable && n.actions.iter().any(|a| a.key == *key),
                        "Unknown notification action"
                    );
                    let resident = n.resident;
                    action = Some((*id, key.clone()));
                    if !resident {
                        close_state(s, *id, 2)?;
                        signals.push((*id, 2));
                    }
                }
                Control::ClearAll | Control::ClearGroup { .. } => {
                    let matches = |n: &Notification| match &request.command {
                        Control::ClearGroup { app_name } => &n.app_name == app_name,
                        _ => true,
                    };
                    let ids: Vec<_> = s
                        .snapshot
                        .active
                        .iter()
                        .filter(|n| matches(n))
                        .map(|n| n.id)
                        .collect();
                    for id in ids {
                        close_state(s, id, 2)?;
                        signals.push((id, 2));
                    }
                    s.snapshot.history.retain(|n| !matches(n));
                }
                Control::SetDnd { value } => {
                    s.snapshot.dnd = *value;
                    if *value {
                        for n in &mut s.snapshot.active {
                            n.popup = false;
                        }
                    }
                }
                Control::SetFullscreen { value } => {
                    s.snapshot.fullscreen = *value;
                    if *value {
                        for n in &mut s.snapshot.active {
                            n.popup = false;
                        }
                    }
                }
            }
            Ok(())
        })?;
        if let Some((id, key)) = action {
            self.bus
                .emit_signal(None::<&str>, OBJECT, NAME, "ActionInvoked", &(id, key))?;
        }
        for (id, reason) in signals {
            self.signal_closed(id, reason)?;
        }
        Ok(())
    }
    fn tick(&self) -> Result<()> {
        let now = Instant::now();
        let snapshot = self.snapshot()?;
        let expired: Vec<_> = snapshot
            .active
            .iter()
            .filter(|n| n.deadline.is_some_and(|d| now >= d))
            .map(|n| n.id)
            .collect();
        let hidden: Vec<_> = snapshot
            .active
            .iter()
            .filter(|n| n.popup && n.popup_deadline.is_some_and(|d| now >= d))
            .map(|n| n.id)
            .collect();
        if expired.is_empty() && hidden.is_empty() {
            return Ok(());
        }
        let closed = self.change(|s| {
            let mut closed = Vec::new();
            for id in expired {
                if s.snapshot
                    .active
                    .iter()
                    .any(|n| n.id == id && n.deadline.is_some_and(|d| now >= d))
                {
                    close_state(s, id, 1)?;
                    closed.push(id);
                }
            }
            for n in &mut s.snapshot.active {
                if hidden.contains(&n.id) && n.popup_deadline.is_some_and(|d| now >= d) {
                    n.popup = false;
                }
            }
            Ok(closed)
        })?;
        for id in closed {
            self.signal_closed(id, 1)?;
        }
        Ok(())
    }
}
fn close_state(s: &mut State, id: u32, reason: u32) -> Result<()> {
    let index = s
        .snapshot
        .active
        .iter()
        .position(|n| n.id == id)
        .context("Unknown active notification")?;
    let mut n = s.snapshot.active.remove(index);
    n.closed_reason = Some(reason);
    n.popup = false;
    n.actionable = false;
    n.deadline = None;
    n.popup_deadline = None;
    if !n.transient {
        s.snapshot.history.insert(0, n);
        s.snapshot.history.truncate(100);
    }
    Ok(())
}
fn hint<T>(hints: &HashMap<String, OwnedValue>, name: &str) -> Result<Option<T>>
where
    for<'a> T: TryFrom<&'a OwnedValue>,
    for<'a> <T as TryFrom<&'a OwnedValue>>::Error: std::error::Error + Send + Sync + 'static,
{
    hints
        .get(name)
        .map(|v| T::try_from(v).map_err(anyhow::Error::from))
        .transpose()
}
fn hint_text(h: &HashMap<String, OwnedValue>, name: &str) -> Result<Option<String>> {
    h.get(name)
        .map(|v| {
            let s: &str = v.try_into()?;
            bounded(s, 4096)?;
            Ok(s.to_owned())
        })
        .transpose()
}
fn image_hint(h: &HashMap<String, OwnedValue>) -> Result<Option<Image>> {
    let Some(value) = h
        .get("image-data")
        .or_else(|| h.get("image_data"))
        .or_else(|| h.get("icon_data"))
    else {
        return Ok(None);
    };
    let (width, height, stride, alpha, bits, channels, data): (
        i32,
        i32,
        i32,
        bool,
        i32,
        i32,
        Vec<u8>,
    ) = value.try_clone()?.try_into()?;
    ensure!(
        width > 0
            && height > 0
            && width <= 512
            && height <= 512
            && bits == 8
            && channels == if alpha { 4 } else { 3 },
        "Invalid notification image format"
    );
    let row = width as usize * channels as usize;
    ensure!(
        stride >= 0 && stride as usize >= row && data.len() <= 1024 * 1024,
        "Invalid image stride or size"
    );
    let needed = (height as usize - 1)
        .checked_mul(stride as usize)
        .and_then(|v| v.checked_add(row))
        .context("Image byte count overflow")?;
    ensure!(data.len() >= needed, "Truncated notification image");
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        for pixel in
            data[y * stride as usize..y * stride as usize + row].chunks_exact(channels as usize)
        {
            rgba.extend_from_slice(&pixel[..3]);
            rgba.push(if alpha { pixel[3] } else { 255 });
        }
    }
    Ok(Some(Image {
        width: width as u32,
        height: height as u32,
        rgba,
    }))
}
struct Interface(Core);
fn dbus_error(error: impl std::fmt::Display) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Interface {
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["body", "actions"]
    }
    fn get_server_information(&self) -> (&str, &str, &str, &str) {
        ("Hearth", "Hearth", env!("CARGO_PKG_VERSION"), "1.3")
    }
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        (|| -> Result<u32> {
            ensure!(
                expire_timeout >= -1
                    && actions.len() <= 64
                    && actions.len().is_multiple_of(2)
                    && hints.len() <= 64,
                "Invalid notification arguments"
            );
            let urgency = hint::<u8>(&hints, "urgency")?.unwrap_or(1);
            let now = Instant::now();
            let expiry = if urgency == 2 || expire_timeout == 0 {
                None
            } else {
                Some(
                    now + Duration::from_millis(if expire_timeout == -1 {
                        5000
                    } else {
                        expire_timeout as u64
                    }),
                )
            };
            let mut n = Notification {
                id: 1,
                revision: 0,
                app_name,
                summary,
                body,
                actions: actions
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|a| Action {
                        key: a[0].clone(),
                        label: a[1].clone(),
                    })
                    .collect(),
                urgency,
                resident: hint(&hints, "resident")?.unwrap_or(false),
                transient: hint(&hints, "transient")?.unwrap_or(false),
                app_icon,
                image_path: hint_text(&hints, "image-path")?.or(hint_text(&hints, "image_path")?),
                image: image_hint(&hints)?,
                desktop_entry: hint_text(&hints, "desktop-entry")?,
                category: hint_text(&hints, "category")?,
                popup: true,
                actionable: true,
                closed_reason: None,
                created_at_ms: millis(),
                deadline: expiry,
                popup_deadline: Some(now + Duration::from_millis(5000)),
            };
            validate_record(&n)?;
            self.0.change(|s| {
                let existing = s
                    .snapshot
                    .active
                    .iter()
                    .position(|old| old.id == replaces_id);
                ensure!(
                    existing.is_some() || s.snapshot.active.len() < 100,
                    "Active notification limit reached"
                );
                let id = if existing.is_some() {
                    replaces_id
                } else {
                    ensure!(s.next_id <= u32::MAX as u64, "Notification IDs exhausted");
                    let id = s.next_id as u32;
                    s.next_id += 1;
                    id
                };
                n.id = id;
                n.revision = s
                    .snapshot
                    .revision
                    .checked_add(1)
                    .context("Revision exhausted")?;
                n.popup = !s.snapshot.dnd && !s.snapshot.fullscreen;
                if let Some(index) = existing {
                    s.snapshot.active[index] = n;
                } else {
                    s.snapshot.active.insert(0, n);
                }
                Ok(id)
            })
        })()
        .map_err(dbus_error)
    }
    fn close_notification(&self, id: u32) -> zbus::fdo::Result<()> {
        self.0.close(id, 3).map_err(dbus_error)
    }
}

struct SocketFile {
    path: PathBuf,
    identity: (u64, u64),
}
impl Drop for SocketFile {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.identity) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
pub struct Server {
    core: Core,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    _socket: SocketFile,
}
impl Server {
    pub fn start(config: Config) -> Result<Self> {
        ensure!(
            config.socket != config.state_file,
            "Socket and state paths must differ"
        );
        private_parent(&config.socket)?;
        let state = load(&config.state_file)?;
        let connection = match config.bus_address.as_deref() {
            Some(address) => Builder::address(address)?,
            None => Builder::session()?,
        }
        .method_timeout(Duration::from_millis(400))
        .build()?;
        ensure!(
            matches!(
                connection.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into())?,
                RequestNameReply::PrimaryOwner
            ),
            "Notification service is already owned"
        );
        let listener = UnixListener::bind(&config.socket)?;
        let metadata = fs::symlink_metadata(&config.socket)?;
        let socket_file = SocketFile {
            path: config.socket.clone(),
            identity: (metadata.dev(), metadata.ino()),
        };
        fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let core = Core {
            state: Arc::new(Mutex::new(state)),
            path: config.state_file,
            bus: connection,
        };
        core.bus
            .object_server()
            .at(OBJECT, Interface(core.clone()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_core = core.clone();
        let worker = thread::Builder::new()
            .name("hearth-notifications".into())
            .spawn(move || {
                if let Err(error) = serve(listener, worker_core, worker_stop) {
                    eprintln!("Notification server: {error}");
                }
            });
        match worker {
            Ok(worker) => Ok(Self {
                core,
                stop,
                worker: Some(worker),
                _socket: socket_file,
            }),
            Err(error) => {
                let _ = core.bus.object_server().remove::<Interface, _>(OBJECT);
                Err(error.into())
            }
        }
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.core.snapshot()
    }
    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = self.core.bus.object_server().remove::<Interface, _>(OBJECT);
        let _ = self.core.bus.release_name(NAME);
    }
}
struct Client {
    socket: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    sent: usize,
    subscribed: bool,
    revision: Option<u64>,
}
fn serve(listener: UnixListener, core: Core, stop: Arc<AtomicBool>) -> Result<()> {
    let mut clients: Vec<Client> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        core.tick()?;
        let mut fds = vec![libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        fds.extend(clients.iter().map(|c| libc::pollfd {
            fd: c.socket.as_raw_fd(),
            events: libc::POLLIN
                | if c.sent < c.output.len() {
                    libc::POLLOUT
                } else {
                    0
                },
            revents: 0,
        }));
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 20) };
        if result < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        let snapshot = core.snapshot()?;
        let mut index = 0;
        clients.retain_mut(|client| {
            index += 1;
            let result = handle_client(client, &core, &snapshot, fds[index].revents);
            if let Err(error) = result {
                eprintln!("Notification client: {error}");
                false
            } else {
                true
            }
        });
        if fds[0].revents & libc::POLLIN != 0 {
            for _ in 0..4 {
                match listener.accept() {
                    Ok((socket, _)) => {
                        if clients.len() >= 4 {
                            continue;
                        }
                        socket.set_nonblocking(true)?;
                        clients.push(Client {
                            socket,
                            input: vec![],
                            output: vec![],
                            sent: 0,
                            subscribed: false,
                            revision: None,
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(())
}
fn queue(client: &mut Client, value: Value) -> Result<()> {
    ensure!(
        client.sent == client.output.len(),
        "Notification subscriber is not draining"
    );
    client.output = serde_json::to_vec(&value)?;
    client.output.push(b'\n');
    client.sent = 0;
    ensure!(
        client.output.len() <= MAX_STATE + 1024,
        "Notification reply too large"
    );
    Ok(())
}
fn handle_client(client: &mut Client, core: &Core, snapshot: &Snapshot, ready: i16) -> Result<()> {
    if ready & (libc::POLLERR | libc::POLLNVAL) != 0 {
        bail!("Notification socket failed");
    }
    if ready & (libc::POLLIN | libc::POLLHUP) != 0 {
        let mut bytes = [0; 4096];
        match client.socket.read(&mut bytes) {
            Ok(0) => bail!("Notification client disconnected"),
            Ok(n) => {
                ensure!(
                    client.input.len() + n <= MAX_INPUT,
                    "Notification control too large"
                );
                client.input.extend_from_slice(&bytes[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if client.sent < client.output.len() && ready & libc::POLLOUT != 0 {
        match client.socket.write(&client.output[client.sent..]) {
            Ok(0) => bail!("Notification write closed"),
            Ok(n) => client.sent += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if client.sent == client.output.len() {
        if let Some(end) = client.input.iter().position(|b| *b == b'\n') {
            let bytes: Vec<_> = client.input.drain(..=end).collect();
            let request: Request = serde_json::from_slice(&bytes)?;
            let result = match request.command {
                Control::Inspect => Ok(()),
                Control::Subscribe => {
                    client.subscribed = true;
                    Ok(())
                }
                _ => core.control(&request),
            };
            let value = match result {
                Ok(()) => {
                    let next = core.snapshot()?;
                    client.revision = Some(next.revision);
                    json!({"request_id":request.request_id,"status":"ok","snapshot":next})
                }
                Err(e) => {
                    json!({"request_id":request.request_id,"status":"rejected","error":e.to_string()})
                }
            };
            queue(client, value)?;
        } else if client.subscribed && client.revision != Some(snapshot.revision) {
            client.revision = Some(snapshot.revision);
            queue(client, json!({"type":"state","snapshot":snapshot}))?;
        }
    }
    Ok(())
}
