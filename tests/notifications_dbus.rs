use anyhow::Result;
use hearth::notifications::{Config, Request, Server};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    fdo::RequestNameFlags,
    zvariant::Value,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    child: Child,
    address: String,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hearth-notifications-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("bus stdout")).read_line(&mut address)?;
        Ok(Self {
            child,
            address: address.trim().into(),
            dir,
        })
    }
    fn connection(&self) -> Result<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_secs(2))
            .build()?)
    }
    fn config(&self) -> Config {
        Config {
            bus_address: Some(self.address.clone()),
            socket: self.dir.join("control.sock"),
            state_file: self.dir.join("history.json"),
        }
    }
    fn control(&self, value: serde_json::Value) -> Result<serde_json::Value> {
        let _: Request = serde_json::from_value(value.clone())?;
        let mut stream = UnixStream::connect(self.dir.join("control.sock"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        serde_json::to_writer(&mut stream, &value)?;
        stream.write_all(b"\n")?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        Ok(serde_json::from_str(&reply)?)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn proxy(c: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(
        c,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?)
}
fn notify(
    p: &Proxy<'_>,
    replaces: u32,
    body: &str,
    actions: Vec<&str>,
    hints: HashMap<&str, Value<'_>>,
    timeout: i32,
) -> Result<u32> {
    Ok(p.call(
        "Notify",
        &(
            "test", replaces, "", "summary", body, actions, hints, timeout,
        ),
    )?)
}
#[test]
fn incumbent_is_neither_replaced_nor_queued() -> Result<()> {
    let f = Fixture::new()?;
    let incumbent = f.connection()?;
    incumbent.request_name_with_flags(
        "org.freedesktop.Notifications",
        RequestNameFlags::AllowReplacement.into(),
    )?;
    assert!(Server::start(f.config()).is_err());
    let bus = Proxy::new(
        &incumbent,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owners: Vec<String> = bus.call("ListQueuedOwners", &("org.freedesktop.Notifications",))?;
    assert_eq!(
        owners,
        vec![incumbent.unique_name().expect("unique").to_string()]
    );
    drop(bus);
    drop(incumbent);
    let _server = Server::start(f.config())?;
    Ok(())
}
#[test]
fn replacement_actions_closure_and_restart_history() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let id = notify(&p, 0, "one", vec!["default", "Open"], HashMap::new(), 0)?;
    assert_eq!(
        id,
        notify(&p, id, "two", vec!["default", "Open"], HashMap::new(), 0)?
    );
    assert_eq!(server.snapshot()?.active.len(), 1);
    assert_eq!(server.snapshot()?.active[0].body, "two");
    let snapshot = server.snapshot()?;
    let stale=f.control(serde_json::json!({"request_id":1,"epoch":snapshot.epoch,"revision":snapshot.revision-1,"command":{"type":"invoke","id":id,"key":"default"}}))?;
    assert_eq!(stale["status"], "rejected");
    let result=f.control(serde_json::json!({"request_id":2,"epoch":snapshot.epoch,"revision":snapshot.revision,"command":{"type":"invoke","id":id,"key":"default"}}))?;
    assert_eq!(result["status"], "ok");
    assert!(server.snapshot()?.active.is_empty());
    assert_eq!(server.snapshot()?.history[0].closed_reason, Some(2));
    assert!(p.call::<_, _, ()>("CloseNotification", &(id,)).is_err());
    let active = notify(&p, 0, "retained", vec![], HashMap::new(), 0)?;
    drop(server);
    let restored = Server::start(f.config())?;
    assert!(restored.snapshot()?.active.is_empty());
    assert!(
        restored
            .snapshot()?
            .history
            .iter()
            .any(|n| n.id == active && !n.actionable && !n.popup)
    );
    assert!(notify(&p, 0, "new", vec![], HashMap::new(), 0)? > active);
    Ok(())
}
#[test]
fn timeouts_resident_transient_and_invalid_image() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let id = notify(&p, 0, "expiring", vec![], HashMap::new(), 30)?;
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        server
            .snapshot()?
            .history
            .iter()
            .any(|n| n.id == id && n.closed_reason == Some(1))
    );
    let transient = notify(
        &p,
        0,
        "transient",
        vec![],
        HashMap::from([("transient", Value::from(true))]),
        0,
    )?;
    p.call::<_, _, ()>("CloseNotification", &(transient,))?;
    assert!(!server.snapshot()?.history.iter().any(|n| n.id == transient));
    let id = notify(
        &p,
        0,
        "resident",
        vec!["open", "Open"],
        HashMap::from([("resident", Value::from(true))]),
        0,
    )?;
    let s = server.snapshot()?;
    let r=f.control(serde_json::json!({"request_id":1,"epoch":s.epoch,"revision":s.revision,"command":{"type":"invoke","id":id,"key":"open"}}))?;
    assert_eq!(r["status"], "ok");
    assert!(server.snapshot()?.active.iter().any(|n| n.id == id));
    let image = Value::from((2i32, 2i32, 1i32, true, 8i32, 4i32, vec![0u8; 4]));
    assert!(
        notify(
            &p,
            0,
            "bad image",
            vec![],
            HashMap::from([("image-data", image)]),
            0
        )
        .is_err()
    );
    assert!(notify(&p, 0, "bad actions", vec!["odd"], HashMap::new(), 0).is_err());
    let caps: Vec<String> = p.call("GetCapabilities", &())?;
    assert_eq!(caps, vec!["body", "actions"]);
    Ok(())
}
#[test]
fn capacity_and_dnd_are_explicit() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let s = server.snapshot()?;
    assert_eq!(f.control(serde_json::json!({"request_id":1,"epoch":s.epoch,"revision":s.revision,"command":{"type":"set_dnd","value":true}}))?["status"],"ok");
    for _ in 0..100 {
        notify(&p, 0, "item", vec![], HashMap::new(), 0)?;
    }
    assert!(notify(&p, 0, "overflow", vec![], HashMap::new(), 0).is_err());
    assert!(server.snapshot()?.active.iter().all(|n| !n.popup));
    let id = server.snapshot()?.active[0].id;
    assert_eq!(
        notify(&p, id, "replacement at capacity", vec![], HashMap::new(), 0)?,
        id
    );
    Ok(())
}

#[test]
fn signals_match_actions_and_all_close_reasons() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let mut closed = p.receive_signal("NotificationClosed")?;
    let mut invoked = p.receive_signal("ActionInvoked")?;
    let sender_closed = notify(&p, 0, "sender", vec![], HashMap::new(), 0)?;
    p.call::<_, _, ()>("CloseNotification", &(sender_closed,))?;
    let message = closed.next().expect("close signal");
    assert_eq!(
        message.body().deserialize::<(u32, u32)>()?,
        (sender_closed, 3)
    );
    let id = notify(&p, 0, "action", vec!["open", "Open"], HashMap::new(), 0)?;
    let snapshot = server.snapshot()?;
    let reply = f.control(serde_json::json!({"request_id":1,"epoch":snapshot.epoch,"revision":snapshot.revision,"command":{"type":"invoke","id":id,"key":"open"}}))?;
    assert_eq!(reply["status"], "ok");
    assert_eq!(
        invoked
            .next()
            .expect("action signal")
            .body()
            .deserialize::<(u32, String)>()?,
        (id, "open".into())
    );
    assert_eq!(
        closed
            .next()
            .expect("dismiss signal")
            .body()
            .deserialize::<(u32, u32)>()?,
        (id, 2)
    );
    let expired = notify(&p, 0, "expired", vec![], HashMap::new(), 30)?;
    assert_eq!(
        closed
            .next()
            .expect("expiry signal")
            .body()
            .deserialize::<(u32, u32)>()?,
        (expired, 1)
    );
    Ok(())
}

#[test]
fn replacing_timer_cancels_original_expiry_and_raw_images_are_copied() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let id = notify(&p, 0, "original", vec![], HashMap::new(), 30)?;
    let image = Value::from((1i32, 1i32, 3i32, false, 8i32, 3i32, vec![10u8, 20, 30]));
    notify(
        &p,
        id,
        "replacement",
        vec![],
        HashMap::from([("image-data", image)]),
        0,
    )?;
    std::thread::sleep(Duration::from_millis(100));
    let state = server.snapshot()?;
    assert_eq!(state.active.len(), 1);
    assert_eq!(
        state.active[0].image.as_ref().expect("image").rgba,
        vec![10, 20, 30, 255]
    );
    assert!(state.history.is_empty());
    Ok(())
}

#[test]
fn popup_timeout_hides_without_closing_persistent_notification() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let id = notify(&p, 0, "never expire", vec![], HashMap::new(), 0)?;
    assert!(server.snapshot()?.active[0].popup);
    let deadline = std::time::Instant::now() + Duration::from_secs(7);
    loop {
        let state = server.snapshot()?;
        assert_eq!(state.active.len(), 1);
        assert_eq!(state.active[0].id, id);
        assert!(state.history.is_empty());
        if !state.active[0].popup {
            break;
        }
        anyhow::ensure!(std::time::Instant::now() < deadline, "Popup did not hide");
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn existing_socket_path_is_preserved_and_bus_name_is_released() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.dir.join("control.sock"), b"owned by someone else")?;
    assert!(Server::start(f.config()).is_err());
    assert_eq!(
        fs::read(f.dir.join("control.sock"))?,
        b"owned by someone else"
    );
    let c = f.connection()?;
    assert!(matches!(
        c.request_name_with_flags(
            "org.freedesktop.Notifications",
            RequestNameFlags::DoNotQueue.into()
        )?,
        zbus::fdo::RequestNameReply::PrimaryOwner
    ));
    Ok(())
}

#[test]
fn persisted_ids_cannot_be_reused_or_duplicated() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    notify(&p, 0, "saved", vec![], HashMap::new(), 0)?;
    drop(server);
    let path = f.dir.join("history.json");
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let mut invalid = original.clone();
    invalid["next_id"] = serde_json::json!(1);
    fs::write(&path, serde_json::to_vec(&invalid)?)?;
    assert!(Server::start(f.config()).is_err());
    let mut invalid = original;
    let duplicate = invalid["records"][0].clone();
    invalid["records"]
        .as_array_mut()
        .expect("records")
        .push(duplicate);
    fs::write(&path, serde_json::to_vec(&invalid)?)?;
    assert!(Server::start(f.config()).is_err());
    Ok(())
}

#[test]
fn local_images_resolve_in_delivery_and_follow_hint_precedence() -> Result<()> {
    let f = Fixture::new()?;
    let server = Server::start(f.config())?;
    let c = f.connection()?;
    let p = proxy(&c)?;
    let path = f.dir.join("woodland icon.png");
    hearth::render::write_png(&path, 1, 1, &[52, 95, 38, 128])?;
    let uri = url::Url::from_file_path(&path)
        .expect("absolute fixture path")
        .to_string();
    let inline = || Value::from((1i32, 1i32, 4i32, true, 8i32, 4i32, vec![10u8, 20, 30, 255]));
    for (hints, expected) in [
        (
            HashMap::from([("image-path", Value::from(uri.as_str()))]),
            vec![52, 95, 38, 128],
        ),
        (
            HashMap::from([
                ("image-data", inline()),
                ("image-path", Value::from(uri.as_str())),
            ]),
            vec![10, 20, 30, 255],
        ),
        (
            HashMap::from([("icon_data", inline())]),
            vec![52, 95, 38, 128],
        ),
        (
            HashMap::from([(
                "image-path",
                Value::from("https://invalid.example/forbidden.png"),
            )]),
            vec![52, 95, 38, 128],
        ),
    ] {
        let id: u32 = p.call(
            "Notify",
            &(
                "test",
                0u32,
                uri.as_str(),
                "local image",
                "body",
                Vec::<&str>::new(),
                hints,
                0i32,
            ),
        )?;
        let snapshot = server.snapshot()?;
        let item = snapshot
            .active
            .iter()
            .find(|item| item.id == id)
            .expect("delivered");
        assert_eq!(item.image.as_ref().expect("resolved image").rgba, expected);
    }
    Ok(())
}
