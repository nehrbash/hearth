use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
const MAX_LINE: usize = 1024 * 1024;
const MAX_CLIENTS: usize = 16;
const MAX_QUEUE: usize = 256;
const MAX_OUTBOUND: usize = 2 * MAX_LINE;
const CAPABILITIES: &[&str] = &[
    "channels",
    "patch",
    "inspect",
    "input",
    "presented",
    "subscribe",
];

fn validate_required_capabilities(value: &Value) -> Result<()> {
    let Some(required) = value.get("required_capabilities") else {
        return Ok(());
    };
    let required = required
        .as_array()
        .context("required_capabilities must be an array")?;
    ensure!(required.len() <= 32, "Too many required capabilities");
    for capability in required {
        let capability = capability
            .as_str()
            .context("Required capability must be a string")?;
        ensure!(
            !capability.is_empty() && capability.len() <= 128,
            "Invalid required capability length"
        );
        ensure!(
            CAPABILITIES.contains(&capability),
            "Unsupported required capability: {capability}"
        );
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ChannelSnapshot {
    connection_id: u64,
    pub channel: String,
    pub connected: bool,
    pub epoch: u64,
    pub seq: u64,
    pub nodes: Vec<Value>,
    pub data: Option<Value>,
    pub received_at: Instant,
    pub validation_us: u64,
}
#[derive(Clone, Debug, Default)]
pub struct ChannelsSnapshot {
    pub channels: BTreeMap<String, Arc<ChannelSnapshot>>,
}
struct Stored {
    snapshot: Arc<ChannelSnapshot>,
    owner: Option<u64>,
    bytes: usize,
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Arc<ChannelsSnapshot>>,
    error: Option<String>,
    outbound: VecDeque<Value>,
}
pub struct ChannelListener {
    path: PathBuf,
    identity: (u64, u64),
    mailbox: Arc<Mutex<Mailbox>>,
    stop: Arc<AtomicBool>,
    wake: UnixStream,
    notifications: UnixStream,
    worker: Option<JoinHandle<()>>,
}
impl ChannelListener {
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Socket requires a private parent"))?;
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Socket parent must be owned by this user with mode 0700",
            ));
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let metadata = fs::symlink_metadata(&path)?;
        let identity = (metadata.dev(), metadata.ino());
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, worker_wake) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        worker_wake.set_nonblocking(true)?;
        let (notifications, notify) = UnixStream::pair()?;
        notifications.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let shared = mailbox.clone();
        let stopped = stop.clone();
        let worker = thread::Builder::new()
            .name("hearth-channels".into())
            .spawn(move || run(listener, worker_wake, notify, &stopped, &shared))?;
        Ok(Self {
            path,
            identity,
            mailbox,
            stop,
            wake,
            notifications,
            worker: Some(worker),
        })
    }
    pub fn update_fd(&self) -> RawFd {
        self.notifications.as_raw_fd()
    }
    pub fn drain_notifications(&self) -> io::Result<()> {
        drain(&self.notifications)
    }
    pub fn take_latest(&self) -> Option<Arc<ChannelsSnapshot>> {
        self.mailbox.lock().ok()?.pending.take()
    }
    pub fn take_error(&self) -> Option<String> {
        self.mailbox.lock().ok()?.error.take()
    }
    fn send(&self, value: Value) -> bool {
        let Ok(mut mailbox) = self.mailbox.try_lock() else {
            return false;
        };
        if mailbox.outbound.len() >= MAX_QUEUE {
            return false;
        }
        mailbox.outbound.push_back(value);
        drop(mailbox);
        let _ = (&self.wake).write(&[1]);
        true
    }
    #[allow(clippy::too_many_arguments)]
    pub fn send_input(
        &self,
        channel: &str,
        epoch: u64,
        seq: u64,
        key: &str,
        event: &str,
        x: f64,
        y: f64,
    ) -> bool {
        if !x.is_finite()
            || !y.is_finite()
            || channel.len() > 128
            || key.len() > 128
            || !matches!(
                event,
                "hover_enter" | "hover_leave" | "press" | "release" | "activate" | "cancel"
            )
        {
            return false;
        }
        self.send(json!({"type":"input","channel":channel,"epoch":epoch,"seq":seq,"key":key,"event":event,"x":x,"y":y}))
    }
    pub fn mark_presented_snapshot(&self, snapshot: &ChannelSnapshot, frame_ms: f64) -> bool {
        if !frame_ms.is_finite() || frame_ms < 0.0 || !snapshot.connected {
            return false;
        }
        self.send(json!({"type":"presented","channel":snapshot.channel,"epoch":snapshot.epoch,"seq":snapshot.seq,
            "frame_ms":frame_ms,"validation_us":snapshot.validation_us,
            "change_to_presented_ms":snapshot.received_at.elapsed().as_secs_f64()*1000.0,
            "_connection":snapshot.connection_id}))
    }

    pub fn mark_presented(&self, channel: &str, epoch: u64, seq: u64, frame_ms: f64) -> bool {
        if !frame_ms.is_finite() || frame_ms < 0.0 || channel.len() > 128 {
            return false;
        }
        self.send(json!({"type":"presented","channel":channel,"epoch":epoch,"seq":seq,"frame_ms":frame_ms}))
    }
}
impl Drop for ChannelListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = (&self.wake).write(&[1]);
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
fn drain(socket: &UnixStream) -> io::Result<()> {
    let mut socket = socket;
    let mut bytes = [0; 128];
    loop {
        match socket.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}
struct Packet {
    bytes: Vec<u8>,
    channel: Option<String>,
}
struct Client {
    id: u64,
    socket: UnixStream,
    input: Vec<u8>,
    scanned: usize,
    hello: bool,
    closing: bool,
    queue: VecDeque<Packet>,
    offset: usize,
    bytes: usize,
    subscriptions: BTreeSet<String>,
}
impl Client {
    fn line_end(&mut self) -> Option<usize> {
        if let Some(index) = self.input[self.scanned..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            Some(self.scanned + index)
        } else {
            self.scanned = self.input.len();
            None
        }
    }

    fn queue(&mut self, value: Value, latest: Option<String>) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        ensure!(bytes.len() <= MAX_LINE, "Outbound message exceeds 1 MiB");
        bytes.push(b'\n');
        if let Some(channel) = &latest {
            let mut index = 0;
            self.queue.retain(|packet| {
                let keep =
                    !(packet.channel.as_ref() == Some(channel) && (index > 0 || self.offset == 0));
                index += 1;
                if !keep {
                    self.bytes -= packet.bytes.len();
                }
                keep
            });
        }
        ensure!(
            self.queue.len() < MAX_QUEUE && self.bytes + bytes.len() <= MAX_OUTBOUND,
            "Producer outbound queue exhausted; input delivery disconnected"
        );
        self.bytes += bytes.len();
        self.queue.push_back(Packet {
            bytes,
            channel: latest,
        });
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut written = 0;
        while let Some(packet) = self.queue.front() {
            match self.socket.write(&packet.bytes[self.offset..]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::BrokenPipe, "Producer closed")),
                Ok(count) => {
                    self.offset += count;
                    self.bytes -= count;
                    written += count;
                    if self.offset == packet.bytes.len() {
                        self.queue.pop_front();
                        self.offset = 0;
                    }
                    if written >= 65536 {
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("Missing string {key}"))
}
fn name(value: &Value, key: &str) -> Result<String> {
    let name = string(value, key)?;
    ensure!(
        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
        "Invalid {key}"
    );
    Ok(name.into())
}
fn number(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("Missing u64 {key}"))
}
fn array(value: &Value, key: &str) -> Result<Vec<Value>> {
    Ok(value
        .get(key)
        .and_then(Value::as_array)
        .with_context(|| format!("Missing array {key}"))?
        .clone())
}
fn payload(snapshot: &ChannelSnapshot, kind: &str) -> Value {
    json!({"type":kind,"channel":snapshot.channel,"epoch":snapshot.epoch,"seq":snapshot.seq,"nodes":snapshot.nodes,"data":snapshot.data,"connected":snapshot.connected})
}
fn ack(value: &Value, error: Option<String>) -> Value {
    json!({"type":"ack","status":if error.is_none(){"applied"}else{"rejected"},"channel":value.get("channel"),"epoch":value.get("epoch"),"seq":value.get("seq"),"reason":error})
}

fn apply(
    value: &Value,
    client: u64,
    store: &mut BTreeMap<String, Stored>,
    received: Instant,
) -> Result<String> {
    let channel = name(value, "channel")?;
    let epoch = number(value, "epoch")?;
    let seq = number(value, "seq")?;
    let kind = string(value, "type")?;
    let previous = store.get(&channel);
    if let Some(previous) = previous {
        ensure!(
            epoch > previous.snapshot.epoch
                || epoch == previous.snapshot.epoch && seq > previous.snapshot.seq,
            "Stale channel epoch or sequence"
        );
        if epoch == previous.snapshot.epoch {
            ensure!(
                previous.owner.is_none() || previous.owner == Some(client),
                "Channel belongs to another producer; takeover requires a higher epoch baseline"
            );
        }
    } else {
        ensure!(store.len() < 32, "Channel limit reached");
    }
    let (nodes, data) = match kind {
        "baseline" => (array(value, "nodes")?, value.get("data").cloned()),
        "patch" => {
            let previous = previous.context("Patch requires a baseline")?;
            ensure!(
                previous.owner == Some(client) && epoch == previous.snapshot.epoch,
                "Patch requires ownership and matching epoch"
            );
            let mut nodes = previous.snapshot.nodes.clone();
            let removals = array(value, "remove")?;
            let mut removed = BTreeSet::new();
            for key in removals {
                let key = key.as_str().context("Remove keys must be strings")?;
                ensure!(removed.insert(key.to_owned()), "Duplicate remove key");
            }
            nodes.retain(|node| {
                node.get("key")
                    .and_then(Value::as_str)
                    .is_none_or(|key| !removed.contains(key))
            });
            let mut updated = BTreeSet::new();
            for node in array(value, "upsert")? {
                let key = name(&node, "key")?;
                ensure!(
                    updated.insert(key.clone()) && !removed.contains(&key),
                    "Duplicate or conflicting patch key"
                );
                if let Some(index) = nodes
                    .iter()
                    .position(|node| node.get("key").and_then(Value::as_str) == Some(&key))
                {
                    nodes[index] = node;
                } else {
                    nodes.push(node);
                }
            }
            (
                nodes,
                value
                    .get("data")
                    .cloned()
                    .or_else(|| previous.snapshot.data.clone()),
            )
        }
        _ => anyhow::bail!("Unknown update type"),
    };
    crate::interactive::validate_nodes(&nodes)?;
    let bytes = serde_json::to_vec(&json!({"nodes":nodes,"data":data}))?.len();
    ensure!(
        bytes <= MAX_LINE - 1024,
        "Channel state exceeds message budget"
    );
    let existing: usize = store
        .iter()
        .filter(|(key, _)| *key != &channel)
        .map(|(_, v)| v.bytes)
        .sum();
    ensure!(
        existing + bytes <= 4 * MAX_LINE,
        "Aggregate channel state exceeds 4 MiB"
    );
    let snapshot = Arc::new(ChannelSnapshot {
        connection_id: client,
        channel: channel.clone(),
        connected: true,
        epoch,
        seq,
        nodes,
        data,
        received_at: received,
        validation_us: received.elapsed().as_micros().min(u64::MAX as u128) as u64,
    });
    store.insert(
        channel.clone(),
        Stored {
            snapshot,
            owner: Some(client),
            bytes,
        },
    );
    Ok(channel)
}
fn error(mailbox: &Mutex<Mailbox>, notify: &UnixStream, message: String) {
    if let Ok(mut m) = mailbox.lock() {
        m.error = Some(message.chars().take(4096).collect());
    }
    let mut notify = notify;
    let _ = notify.write(&[1]);
}
fn publish(store: &BTreeMap<String, Stored>, mailbox: &Mutex<Mailbox>, notify: &UnixStream) {
    if let Ok(mut m) = mailbox.lock() {
        m.pending = Some(Arc::new(ChannelsSnapshot {
            channels: store
                .iter()
                .map(|(k, v)| (k.clone(), v.snapshot.clone()))
                .collect(),
        }));
    }
    let mut notify = notify;
    let _ = notify.write(&[1]);
}

fn run(
    listener: UnixListener,
    wake: UnixStream,
    notify: UnixStream,
    stop: &AtomicBool,
    mailbox: &Mutex<Mailbox>,
) {
    let session_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut clients: Vec<Client> = Vec::new();
    let mut store = BTreeMap::<String, Stored>::new();
    let mut next_id = 0u64;
    while !stop.load(Ordering::Acquire) {
        let mut fds = vec![
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
        for client in &clients {
            fds.push(libc::pollfd {
                fd: client.socket.as_raw_fd(),
                events: libc::POLLIN
                    | if client.queue.is_empty() {
                        0
                    } else {
                        libc::POLLOUT
                    },
                revents: 0,
            });
        }
        let pending = clients.iter_mut().any(|client| client.line_end().is_some());
        let ready = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as _,
                if pending { 0 } else { -1 },
            )
        };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        if fds[1].revents != 0 {
            let _ = drain(&wake);
        }
        let outbound = mailbox
            .lock()
            .map(|mut m| std::mem::take(&mut m.outbound))
            .unwrap_or_default();
        let mut dead = BTreeSet::new();
        for mut event in outbound {
            let channel = event
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let target = store.get(&channel).filter(|stored| {
                let same_epoch =
                    event.get("epoch").and_then(Value::as_u64) == Some(stored.snapshot.epoch);
                let seq = event.get("seq").and_then(Value::as_u64);
                if event["type"] == "presented" && event.get("_connection").is_some() {
                    same_epoch
                        && seq.is_some_and(|seq| seq <= stored.snapshot.seq)
                        && event["_connection"].as_u64() == stored.owner
                } else {
                    same_epoch && seq == Some(stored.snapshot.seq)
                }
            });
            if let Some(target) = target
                && let Some(owner) = target.owner
                && let Some(client) = clients.iter_mut().find(|client| client.id == owner)
            {
                if event["type"] == "input"
                    && !target
                        .snapshot
                        .nodes
                        .iter()
                        .any(|node| node.get("key") == event.get("key"))
                {
                    error(mailbox, &notify, "Input key no longer exists".into());
                    continue;
                }
                if event["type"] == "presented" && event.get("_connection").is_none() {
                    event["validation_us"] = json!(target.snapshot.validation_us);
                    event["change_to_presented_ms"] =
                        json!(target.snapshot.received_at.elapsed().as_secs_f64() * 1000.0);
                }
                if let Some(object) = event.as_object_mut() {
                    object.remove("_connection");
                }
                if let Err(e) = client.queue(event, None) {
                    dead.insert(client.id);
                    error(mailbox, &notify, e.to_string());
                }
            } else {
                error(
                    mailbox,
                    &notify,
                    format!("Stale or disconnected renderer event for channel {channel}"),
                );
            }
        }
        let mut changed = BTreeSet::new();
        for (index, client) in clients.iter_mut().enumerate() {
            if fds[index + 2].revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                dead.insert(client.id);
                continue;
            }
            if fds[index + 2].revents & (libc::POLLIN | libc::POLLHUP) != 0 && !client.closing {
                let mut bytes = [0; 8192];
                match client.socket.read(&mut bytes) {
                    Ok(0) => {
                        dead.insert(client.id);
                    }
                    Ok(n) => client.input.extend_from_slice(&bytes[..n]),
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => {
                        dead.insert(client.id);
                    }
                }
            }
            for _ in 0..32 {
                let Some(end) = client.line_end() else {
                    break;
                };
                if end > MAX_LINE {
                    dead.insert(client.id);
                    break;
                }
                let line: Vec<_> = client.input.drain(..=end).collect();
                client.scanned = 0;
                let received = Instant::now();
                let parsed = serde_json::from_slice::<Value>(&line);
                let value = match parsed {
                    Ok(value) => value,
                    Err(e) => {
                        let _ = client.queue(ack(&Value::Null, Some(e.to_string())), None);
                        continue;
                    }
                };
                let result = (|| -> Result<()> {
                    let kind = string(&value, "type")?;
                    if !client.hello {
                        ensure!(kind == "hello", "Hello required before channel messages");
                        if value.get("major").and_then(Value::as_u64) != Some(2) {
                            client.closing = true;
                            return client.queue(json!({"type":"hello","major":2,"status":"rejected","reason":"Unsupported major version"}),None);
                        }
                        if let Err(error) = validate_required_capabilities(&value) {
                            client.closing = true;
                            return client.queue(json!({"type":"hello","major":2,"status":"rejected","reason":error.to_string()}), None);
                        }
                        client.hello = true;
                        return client.queue(json!({"type":"hello","major":2,"session_id":session_id,"capabilities":CAPABILITIES}),None);
                    }
                    match kind {
                        "baseline" | "patch" => {
                            let channel = apply(&value, client.id, &mut store, received)?;
                            changed.insert(channel);
                            client.queue(ack(&value, None), None)?;
                        }
                        "inspect" => {
                            let channel = name(&value, "channel")?;
                            let snapshot =
                                &store.get(&channel).context("Unknown channel")?.snapshot;
                            let mut response = payload(snapshot, "inspect");
                            let request = value.get("request_id").cloned().unwrap_or(Value::Null);
                            ensure!(
                                request.is_null()
                                    || request.is_number()
                                    || request.as_str().is_some_and(|s| s.len() <= 128),
                                "Invalid request_id"
                            );
                            response["request_id"] = request;
                            client.queue(response, None)?;
                        }
                        "subscribe" => {
                            let channel = name(&value, "channel")?;
                            ensure!(
                                client.subscriptions.len() < 32
                                    || client.subscriptions.contains(&channel),
                                "Subscription limit reached"
                            );
                            client.subscriptions.insert(channel.clone());
                            if let Some(snapshot) = store.get(&channel) {
                                client.queue(
                                    payload(&snapshot.snapshot, "channel_update"),
                                    Some(channel),
                                )?;
                            } else {
                                client
                                    .queue(json!({"type":"subscribed","channel":channel}), None)?;
                            }
                        }
                        _ => anyhow::bail!("Unsupported message type"),
                    }
                    Ok(())
                })();
                if let Err(e) = result
                    && client
                        .queue(ack(&value, Some(e.to_string())), None)
                        .is_err()
                {
                    dead.insert(client.id);
                }
                if client.closing {
                    break;
                }
            }
            if client.input.len() > MAX_LINE {
                dead.insert(client.id);
                error(mailbox, &notify, "Producer input exceeds 1 MiB".into());
            }
        }
        if !changed.is_empty() {
            publish(&store, mailbox, &notify);
        }
        for channel in changed {
            if let Some(stored) = store.get(&channel) {
                for client in &mut clients {
                    if client.subscriptions.contains(&channel)
                        && let Err(e) = client.queue(
                            payload(&stored.snapshot, "channel_update"),
                            Some(channel.clone()),
                        )
                    {
                        dead.insert(client.id);
                        error(mailbox, &notify, e.to_string());
                    }
                }
            }
        }
        for client in &mut clients {
            if client.flush().is_err() || client.closing && client.queue.is_empty() {
                dead.insert(client.id);
            }
        }
        loop {
            let mut disconnected = Vec::new();
            for (channel, value) in &mut store {
                if value.owner.is_some_and(|id| dead.contains(&id)) {
                    value.owner = None;
                    let mut snapshot = (*value.snapshot).clone();
                    snapshot.connected = false;
                    value.snapshot = Arc::new(snapshot);
                    disconnected.push(channel.clone());
                }
            }
            if disconnected.is_empty() {
                break;
            }
            publish(&store, mailbox, &notify);
            for channel in disconnected {
                for client in &mut clients {
                    if !dead.contains(&client.id)
                        && client.subscriptions.contains(&channel)
                        && let Some(stored) = store.get(&channel)
                        && let Err(e) = client.queue(
                            payload(&stored.snapshot, "channel_update"),
                            Some(channel.clone()),
                        )
                    {
                        dead.insert(client.id);
                        error(mailbox, &notify, e.to_string());
                    }
                }
            }
        }
        clients.retain(|client| !dead.contains(&client.id));
        if fds[0].revents & libc::POLLIN != 0 {
            for _ in 0..MAX_CLIENTS {
                match listener.accept() {
                    Ok((socket, _)) => {
                        if clients.len() >= MAX_CLIENTS {
                            continue;
                        }
                        if socket.set_nonblocking(true).is_err() {
                            continue;
                        }
                        next_id += 1;
                        clients.push(Client {
                            id: next_id,
                            socket,
                            input: Vec::new(),
                            scanned: 0,
                            hello: false,
                            closing: false,
                            queue: VecDeque::new(),
                            offset: 0,
                            bytes: 0,
                            subscriptions: BTreeSet::new(),
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client() -> io::Result<Client> {
        let (socket, _peer) = UnixStream::pair()?;
        Ok(Client {
            id: 1,
            socket,
            input: Vec::new(),
            scanned: 0,
            hello: true,
            closing: false,
            queue: VecDeque::new(),
            offset: 0,
            bytes: 0,
            subscriptions: BTreeSet::new(),
        })
    }
    #[test]
    fn partial_lines_scan_only_appended_bytes() -> Result<()> {
        let mut client = client()?;
        for _ in 0..128 {
            client.input.extend_from_slice(&[b'x'; 8192]);
            assert_eq!(client.line_end(), None);
            assert_eq!(client.scanned, client.input.len());
            assert_eq!(client.line_end(), None);
        }
        client.input.extend_from_slice(b"\nnext\n");
        assert_eq!(client.line_end(), Some(MAX_LINE));
        client.input.drain(..=MAX_LINE);
        client.scanned = 0;
        assert_eq!(client.line_end(), Some(4));
        Ok(())
    }

    #[test]
    fn subscriber_updates_coalesce_without_replacing_partial_frames() -> Result<()> {
        let mut client = client()?;
        client.queue(json!({"seq":1}), Some("ui".into()))?;
        client.queue(json!({"seq":2}), Some("ui".into()))?;
        assert_eq!(client.queue.len(), 1);
        assert_eq!(client.bytes, client.queue[0].bytes.len());
        client.offset = 1;
        client.bytes -= 1;
        client.queue(json!({"seq":3}), Some("ui".into()))?;
        client.queue(json!({"seq":4}), Some("ui".into()))?;
        assert_eq!(client.queue.len(), 2);
        assert_eq!(
            client.bytes,
            client
                .queue
                .iter()
                .map(|packet| packet.bytes.len())
                .sum::<usize>()
                - 1
        );
        let value: Value = serde_json::from_slice(&client.queue[1].bytes)?;
        assert_eq!(value["seq"], 4);
        Ok(())
    }
    #[test]
    fn reliable_outbound_messages_have_count_and_byte_limits() -> Result<()> {
        let mut client = client()?;
        for _ in 0..MAX_QUEUE {
            client.queue(json!({"type":"input"}), None)?;
        }
        assert!(client.queue(json!({"type":"input"}), None).is_err());
        client.queue.clear();
        client.bytes = 0;
        let large = json!({"type":"input","payload":"x".repeat(MAX_LINE-100)});
        client.queue(large.clone(), None)?;
        client.queue(large.clone(), None)?;
        let error = client
            .queue(large, None)
            .err()
            .context("Expected byte limit")?;
        assert!(error.to_string().contains("input delivery disconnected"));
        Ok(())
    }
}
