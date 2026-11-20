use anyhow::Result;
use hearth::channels::{ChannelListener, ChannelsSnapshot};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "hearth-v2-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Self(path))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Client {
    socket: UnixStream,
    reader: BufReader<UnixStream>,
}
impl Client {
    fn raw(path: &Path) -> Result<Self> {
        let socket = UnixStream::connect(path)?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self {
            reader: BufReader::new(socket.try_clone()?),
            socket,
        })
    }
    fn connect(path: &Path) -> Result<Self> {
        let mut client = Self::raw(path)?;
        client.send(json!({"type":"hello","major":2,"capabilities":[],"future_option":true}))?;
        let reply = client.read()?;
        assert_eq!(reply["type"], "hello");
        assert_eq!(reply["major"], 2);
        assert!(reply["session_id"].is_string());
        Ok(client)
    }
    fn send(&mut self, value: Value) -> Result<()> {
        serde_json::to_writer(&mut self.socket, &value)?;
        self.socket.write_all(b"\n")?;
        Ok(())
    }
    fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    }
    fn baseline(&mut self, channel: &str, epoch: u64, seq: u64) -> Result<Value> {
        self.send(json!({"type":"baseline","channel":channel,"epoch":epoch,"seq":seq,"nodes":[node("button")],"data":{"test":true},"future_option":99}))?;
        self.read()
    }
}
fn node(key: &str) -> Value {
    json!({"key":key,"kind":"rect","bounds":[0,0,20,20],"input":true,"future_style":true})
}
fn latest(
    listener: &ChannelListener,
    predicate: impl Fn(&ChannelsSnapshot) -> bool,
) -> Result<Arc<ChannelsSnapshot>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        listener.drain_notifications()?;
        if let Some(snapshot) = listener.take_latest()
            && predicate(&snapshot)
        {
            return Ok(snapshot);
        }
        anyhow::ensure!(Instant::now() < deadline, "No matching renderer update");
        let mut fd = libc::pollfd {
            fd: listener.update_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut fd, 1, 50);
        }
    }
}
#[test]
fn keyed_patches_are_atomic_and_channels_do_not_clobber_each_other() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let listener = ChannelListener::bind(&path)?;
    let mut a = Client::connect(&path)?;
    let mut b = Client::connect(&path)?;
    assert_eq!(a.baseline("ui", 1, 1)?["status"], "applied");
    assert_eq!(b.baseline("service", 1, 1)?["status"], "applied");
    a.send(json!({"type":"patch","channel":"ui","epoch":1,"seq":2,"upsert":[node("second")],"remove":[]}))?;
    assert_eq!(a.read()?["status"], "applied");
    a.send(json!({"type":"patch","channel":"ui","epoch":1,"seq":3,"upsert":[{"key":"bad","kind":"rect","bounds":[0,0,-1,2]}],"remove":["button"]}))?;
    assert_eq!(a.read()?["status"], "rejected");
    let state = latest(&listener, |state| {
        state.channels.len() == 2 && state.channels["ui"].seq == 2
    })?;
    assert_eq!(state.channels["ui"].nodes.len(), 2);
    assert_eq!(state.channels["service"].seq, 1);
    a.send(json!({"type":"inspect","channel":"ui","request_id":"read-1"}))?;
    let response = a.read()?;
    assert_eq!(response["type"], "inspect");
    assert_eq!(response["request_id"], "read-1");
    assert_eq!(response["seq"], 2);
    assert_eq!(response["data"]["test"], true);
    a.send(json!({"type":"patch","channel":"ui","epoch":1,"seq":3,"upsert":[],"remove":[]}))?;
    assert_eq!(a.read()?["status"], "applied");
    assert!(listener.mark_presented_snapshot(&state.channels["ui"], 0.5));
    let receipt = a.read()?;
    assert_eq!(receipt["type"], "presented");
    assert_eq!(receipt["seq"], 2);
    assert!(receipt.get("_connection").is_none());
    Ok(())
}
#[test]
fn epochs_survive_more_than_64_restarts_and_disconnects() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let listener = ChannelListener::bind(&path)?;
    {
        let mut client = Client::connect(&path)?;
        for epoch in 1..=80 {
            assert_eq!(client.baseline("ui", epoch, 1)?["status"], "applied");
        }
    }
    latest(&listener, |state| {
        state
            .channels
            .get("ui")
            .is_some_and(|s| s.epoch == 80 && !s.connected)
    })?;
    let mut next = Client::connect(&path)?;
    assert_eq!(next.baseline("ui", 79, 100)?["status"], "rejected");
    assert_eq!(next.baseline("ui", 80, 2)?["status"], "applied");
    latest(&listener, |state| {
        state.channels["ui"].seq == 2 && state.channels["ui"].connected
    })?;
    Ok(())
}
#[test]
fn ownership_input_receipts_and_subscriptions_are_channel_scoped() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let listener = ChannelListener::bind(&path)?;
    let mut a = Client::connect(&path)?;
    let mut b = Client::connect(&path)?;
    let mut subscriber = Client::connect(&path)?;
    subscriber.send(json!({"type":"subscribe","channel":"ui"}))?;
    assert_eq!(subscriber.read()?["type"], "subscribed");
    assert_eq!(a.baseline("ui", 1, 1)?["status"], "applied");
    assert_eq!(subscriber.read()?["type"], "channel_update");
    assert_eq!(b.baseline("ui", 1, 2)?["status"], "rejected");
    assert_eq!(b.baseline("ui", 2, 1)?["status"], "applied");
    assert_eq!(subscriber.read()?["epoch"], 2);
    assert!(listener.send_input("ui", 2, 1, "button", "activate", 5.0, 6.0));
    let input = b.read()?;
    assert_eq!(input["type"], "input");
    assert_eq!(input["key"], "button");
    assert!(listener.mark_presented("ui", 2, 1, 0.5));
    let receipt = b.read()?;
    assert_eq!(receipt["type"], "presented");
    assert!(receipt["change_to_presented_ms"].is_number());
    assert!(listener.send_input("ui", 1, 1, "button", "activate", 0.0, 0.0));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        listener.drain_notifications()?;
        if let Some(error) = listener.take_error() {
            assert!(error.contains("Stale"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(b);
    assert_eq!(subscriber.read()?["connected"], false);
    Ok(())
}
#[test]
fn partial_lines_bad_major_and_optional_fields_do_not_block_other_clients() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let _listener = ChannelListener::bind(&path)?;
    let mut partial = Client::raw(&path)?;
    partial
        .socket
        .write_all(b"{\"type\":\"hello\",\"major\":2")?;
    let mut good = Client::connect(&path)?;
    assert_eq!(good.baseline("other", 1, 1)?["status"], "applied");
    partial.socket.write_all(b"}\n")?;
    assert_eq!(partial.read()?["major"], 2);
    let mut old = Client::raw(&path)?;
    old.send(json!({"type":"hello","major":1}))?;
    assert_eq!(old.read()?["status"], "rejected");
    let mut unintroduced = Client::raw(&path)?;
    assert_eq!(
        unintroduced.baseline("no-hello", 1, 1)?["status"],
        "rejected"
    );
    Ok(())
}
#[test]
fn required_capabilities_reject_unknown_or_malformed_and_close() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let _listener = ChannelListener::bind(&path)?;
    for required in [
        json!(["future-capability"]),
        json!("patch"),
        Value::Null,
        json!([1]),
        json!([""]),
        json!(["x".repeat(129)]),
        json!(vec!["patch"; 33]),
    ] {
        let mut client = Client::raw(&path)?;
        client.send(json!({"type":"hello","major":2,"required_capabilities":required}))?;
        let reply = client.read()?;
        assert_eq!(reply["type"], "hello");
        assert_eq!(reply["status"], "rejected");
        assert!(reply["reason"].is_string());
        assert!(!reply["session_id"].is_string());
        assert_eq!(client.reader.read_line(&mut String::new())?, 0);
    }
    let mut client = Client::raw(&path)?;
    client.send(json!({"type":"hello","major":2,"capabilities":["future-capability"],"required_capabilities":["channels","patch","inspect","input","presented","subscribe"]}))?;
    let reply = client.read()?;
    assert!(reply["session_id"].is_string());
    assert_eq!(reply["capabilities"].as_array().map(Vec::len), Some(6));
    assert_eq!(client.baseline("ui", 1, 1)?["status"], "applied");
    Ok(())
}

#[test]
fn private_socket_cleanup_and_channel_limits_are_bounded() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let listener = ChannelListener::bind(&path)?;
    let mut client = Client::connect(&path)?;
    for index in 0..32 {
        assert_eq!(
            client.baseline(&format!("channel-{index}"), 1, 1)?["status"],
            "applied"
        );
    }
    assert_eq!(client.baseline("overflow", 1, 1)?["status"], "rejected");
    assert_eq!(client.baseline("channel-0", 100, 1)?["status"], "applied");
    drop(listener);
    assert!(!path.exists());
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755))?;
    assert!(ChannelListener::bind(&path).is_err());
    Ok(())
}

#[test]
fn oversized_frames_disconnect_without_changing_committed_state() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let listener = ChannelListener::bind(&path)?;
    let mut owner = Client::connect(&path)?;
    assert_eq!(owner.baseline("ui", 1, 1)?["status"], "applied");
    let _ = owner.socket.write_all(&vec![b'x'; 1024 * 1024 + 1]);
    let state = latest(&listener, |state| {
        state.channels.get("ui").is_some_and(|s| !s.connected)
    })?;
    assert_eq!(state.channels["ui"].seq, 1);
    let mut next = Client::connect(&path)?;
    assert_eq!(next.baseline("ui", 2, 1)?["status"], "applied");
    Ok(())
}

#[test]
fn stalled_clients_are_bounded_and_listener_remains_responsive() -> Result<()> {
    let directory = Directory::new()?;
    let path = directory.0.join("socket");
    let _listener = ChannelListener::bind(&path)?;
    let mut clients = Vec::new();
    for _ in 0..16 {
        clients.push(Client::connect(&path)?);
    }
    let mut extra = Client::raw(&path)?;
    let _ = extra.send(json!({"type":"hello","major":2}));
    assert!(extra.read().is_err());
    assert_eq!(clients[0].baseline("ui", 1, 1)?["status"], "applied");
    Ok(())
}
