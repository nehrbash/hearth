use anyhow::{Context, Result, ensure};
use serde_json::{Value as Json, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::fd::AsRawFd,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::{ObjectPath, OwnedValue, Value},
};
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.hearth.TrayTest";
const ITEM_PATH: &str = "/StatusNotifierItem";
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Bus {
    _process: Process,
    address: String,
}
impl Bus {
    fn new() -> Result<Self> {
        let mut process = Process(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let mut address = String::new();
        BufReader::new(process.0.stdout.take().context("bus stdout")?).read_line(&mut address)?;
        Ok(Self {
            _process: process,
            address: address.trim().into(),
        })
    }
    fn connection(&self) -> Result<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_secs(2))
            .build()?)
    }
}
type Calls = Arc<Mutex<Vec<String>>>;
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
type Tooltip = (String, Pixmaps, String, String);
type ItemFixture = (Connection, Calls, Arc<Mutex<u32>>);
struct Watcher {
    items: Vec<String>,
}
#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_host(&self, _service: &str) {}
    fn register_status_notifier_item(&self, _service: &str) {}
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.clone()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}
struct Item {
    calls: Calls,
    oversized: bool,
}
#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, x: i32, y: i32) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("activate:{x}:{y}"));
    }
    fn secondary_activate(&self, x: i32, y: i32) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("secondary:{x}:{y}"));
    }
    fn context_menu(&self, x: i32, y: i32) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("context:{x}:{y}"));
    }
    fn scroll(&self, delta: i32, orientation: &str) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("scroll:{delta}:{orientation}"));
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "hearth-tray-test"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Private tray item"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        if self.oversized {
            vec![(i32::MAX, i32::MAX, vec![0; 4])]
        } else {
            vec![(2, 1, vec![255, 17, 34, 51, 128, 68, 85, 102])]
        }
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'_> {
        ObjectPath::try_from("/Menu").expect("path")
    }
    #[zbus(property)]
    fn tool_tip(&self) -> Tooltip {
        (
            String::new(),
            Vec::new(),
            "Title".into(),
            "Description".into(),
        )
    }
}
type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
struct Menu {
    calls: Calls,
    revision: Arc<Mutex<u32>>,
}
fn string(value: &str) -> OwnedValue {
    Value::from(value).try_into().expect("owned string")
}
fn leaf(id: i32, enabled: bool) -> OwnedValue {
    let mut properties = HashMap::from([
        ("label".into(), string("Action")),
        ("enabled".into(), OwnedValue::from(enabled)),
        ("visible".into(), OwnedValue::from(true)),
    ]);
    if id == 1 || id == 3 {
        let bytes = if id == 1 {
            let mut output = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut output, 1, 1);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().expect("PNG header");
                writer
                    .write_image_data(&[12, 34, 56, 255])
                    .expect("PNG pixels");
            }
            output
        } else {
            vec![1u8, 2, 3]
        };
        properties.insert(
            "icon-data".into(),
            Value::from(bytes).try_into().expect("icon bytes"),
        );
    }
    let value: Layout = (id, properties, Vec::new());
    Value::from(value).try_into().expect("layout variant")
}
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Layout) {
        (
            *self.revision.lock().expect("revision"),
            (
                0,
                HashMap::new(),
                vec![leaf(1, true), leaf(2, false), leaf(3, true)],
            ),
        )
    }
    fn about_to_show(&self, id: i32) -> bool {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("about:{id}"));
        false
    }
    fn event(&self, id: i32, event: &str, _data: Value<'_>, timestamp: u32) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("menu:{id}:{event}:{timestamp}"));
    }
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
}
fn item(bus: &Bus, oversized: bool) -> Result<ItemFixture> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let revision = Arc::new(Mutex::new(7));
    let connection = Builder::address(bus.address.as_str())?
        .name(ITEM)?
        .serve_at(
            ITEM_PATH,
            Item {
                calls: calls.clone(),
                oversized,
            },
        )?
        .serve_at(
            "/Menu",
            Menu {
                calls: calls.clone(),
                revision: revision.clone(),
            },
        )?
        .build()?;
    Ok((connection, calls, revision))
}
fn watcher(bus: &Bus) -> Result<Connection> {
    Ok(Builder::address(bus.address.as_str())?
        .name(WATCHER)?
        .serve_at(
            WATCHER_PATH,
            Watcher {
                items: vec![format!("{ITEM}{ITEM_PATH}")],
            },
        )?
        .build()?)
}
fn snapshot(client: &mut hearth::tray::TrayClient) -> Result<Json> {
    Ok(serde_json::to_value(client.snapshot()?)?)
}
fn execute(client: &mut hearth::tray::TrayClient, state: &Json, action: Json) -> Result<()> {
    let item = &state["items"][0];
    let request = serde_json::from_value(
        json!({"session":state["session"],"service":item["service"],"owner":item["owner"],"path":item["path"],"action":action}),
    )?;
    let outcome = client.execute(&request);
    assert!(
        matches!(outcome.status, "applied" | "rejected"),
        "Unexpected uncertain outcome: {:?}",
        outcome.error
    );
    ensure!(
        outcome.status == "applied",
        "Action {}: {:?}",
        outcome.status,
        outcome.error
    );
    Ok(())
}
#[test]
fn incumbent_watcher_pixmap_actions_and_menu_guards() -> Result<()> {
    let bus = Bus::new()?;
    let (_item, calls, revision) = item(&bus, false)?;
    let incumbent = watcher(&bus)?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    let state = snapshot(&mut client)?;
    assert_eq!(state["owns_watcher"], false);
    assert_eq!(
        state["watcher_owner"],
        incumbent.unique_name().context("owner")?.as_str()
    );
    assert_eq!(state["items"].as_array().context("items")?.len(), 1);
    assert_eq!(
        state["items"][0]["icon"]["rgba"],
        json!([17, 34, 51, 255, 68, 85, 102, 128])
    );
    execute(
        &mut client,
        &state,
        json!({"action":"activate","x":10,"y":20}),
    )?;
    execute(
        &mut client,
        &state,
        json!({"action":"secondary_activate","x":30,"y":40}),
    )?;
    execute(
        &mut client,
        &state,
        json!({"action":"context_menu","x":50,"y":60}),
    )?;
    execute(
        &mut client,
        &state,
        json!({"action":"scroll","delta":-120,"orientation":"vertical"}),
    )?;
    let menu = &state["items"][0]["menu"];
    execute(
        &mut client,
        &state,
        json!({"action":"menu_event","epoch":menu["epoch"],"revision":menu["revision"],"id":1,"event":"clicked","timestamp":123}),
    )?;
    assert!(execute(&mut client,&state,json!({"action":"menu_event","epoch":menu["epoch"],"revision":menu["revision"],"id":2,"event":"clicked","timestamp":123})).is_err());
    assert!(execute(&mut client,&state,json!({"action":"menu_event","epoch":menu["epoch"],"revision":menu["revision"],"id":999,"event":"clicked","timestamp":123})).is_err());
    *revision.lock().expect("revision") += 1;
    assert!(execute(&mut client,&state,json!({"action":"menu_event","epoch":menu["epoch"],"revision":menu["revision"],"id":1,"event":"clicked","timestamp":123})).is_err());
    assert_eq!(
        *calls.lock().expect("calls"),
        [
            "activate:10:20",
            "secondary:30:40",
            "context:50:60",
            "scroll:-120:vertical",
            "menu:1:clicked:123"
        ]
    );
    Ok(())
}
#[test]
fn owned_watcher_registration_removal_and_owner_pinning() -> Result<()> {
    let bus = Bus::new()?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    assert_eq!(snapshot(&mut client)?["owns_watcher"], true);
    let (first, _calls, _revision) = item(&bus, false)?;
    let registration = Proxy::new(&first, WATCHER, WATCHER_PATH, WATCHER)?;
    registration.call::<_, _, ()>("RegisterStatusNotifierItem", &(ITEM))?;
    let state = snapshot(&mut client)?;
    assert_eq!(state["items"].as_array().context("items")?.len(), 1);
    drop(registration);
    drop(first);
    let (replacement, calls, _) = item(&bus, false)?;
    assert!(
        execute(
            &mut client,
            &state,
            json!({"action":"activate","x":0,"y":0})
        )
        .is_err()
    );
    assert!(calls.lock().expect("calls").is_empty());
    drop(replacement);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !snapshot(&mut client)?["items"]
        .as_array()
        .context("items")?
        .is_empty()
    {
        ensure!(Instant::now() < deadline, "Dead item was not removed");
        thread::sleep(Duration::from_millis(10));
    }
    drop(client);
    let connection = bus.connection()?;
    let dbus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owned: bool = dbus.call("NameHasOwner", &(WATCHER))?;
    assert!(!owned);
    Ok(())
}
#[test]
fn invalid_pixmap_is_not_exposed_as_decoded_pixels() -> Result<()> {
    let bus = Bus::new()?;
    let (_item, _, _) = item(&bus, true)?;
    let _watcher = watcher(&bus)?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    let state = snapshot(&mut client)?;
    assert_eq!(state["items"].as_array().context("items")?.len(), 1);
    assert!(state["items"][0]["icon"].is_null());
    Ok(())
}
fn worker(bus: &Bus, args: &[&str]) -> Result<Process> {
    Ok(Process(
        Command::new(env!("CARGO_BIN_EXE_hearth-tray"))
            .args(["--bus-address", &bus.address])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    ))
}
fn read_line(output: &mut std::process::ChildStdout) -> Result<Json> {
    let fd = output.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "nonblocking stdout"
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        match output.read(&mut byte) {
            Ok(0) => anyhow::bail!("Worker stdout closed"),
            Ok(_) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(serde_json::from_slice(&line)?);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2))
            }
            Err(e) => return Err(e.into()),
        }
        ensure!(line.len() <= 1024 * 1024, "Oversized output");
        ensure!(Instant::now() < deadline, "Worker output timed out");
    }
}
fn stop(process: &mut Process) -> Result<()> {
    ensure!(
        unsafe { libc::kill(process.0.id() as i32, libc::SIGTERM) } == 0,
        "SIGTERM failed"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = process.0.try_wait()? {
            ensure!(status.success(), "Worker exit {status}");
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "Worker failed bounded shutdown");
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn worker_action_partial_input_and_shutdown_preserve_incumbent() -> Result<()> {
    let bus = Bus::new()?;
    let (_item, calls, _) = item(&bus, false)?;
    let incumbent = watcher(&bus)?;
    let mut process = worker(&bus, &[])?;
    let mut output = process.0.stdout.take().context("stdout")?;
    let envelope = read_line(&mut output)?;
    assert_eq!(envelope["service"], "tray");
    assert_eq!(envelope["status"], "available");
    let state = &envelope["state"];
    let item = &state["items"][0];
    let request = json!({"type":"action","request_id":"click","request":{"session":state["session"],"service":item["service"],"owner":item["owner"],"path":item["path"],"action":{"action":"activate","x":1,"y":2}}});
    let input = process.0.stdin.as_mut().context("stdin")?;
    input.write_all(&serde_json::to_vec(&request)?)?;
    input.write_all(b"\n")?;
    loop {
        let response = read_line(&mut output)?;
        if response["type"] == "result" {
            assert_eq!(response["request_id"], "click");
            assert_eq!(response["status"], "applied");
            break;
        }
    }
    assert_eq!(*calls.lock().expect("calls"), ["activate:1:2"]);
    process
        .0
        .stdin
        .as_mut()
        .context("stdin")?
        .write_all(b"{\"type\":")?;
    stop(&mut process)?;
    let connection = bus.connection()?;
    let dbus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owner: String = dbus.call("GetNameOwner", &(WATCHER))?;
    assert_eq!(owner, incumbent.unique_name().context("owner")?.as_str());
    Ok(())
}
#[test]
fn blocked_output_does_not_block_worker_shutdown() -> Result<()> {
    let bus = Bus::new()?;
    let (_item, _, _) = item(&bus, false)?;
    let _watcher = watcher(&bus)?;
    let mut process = worker(&bus, &[])?;
    let mut output = process.0.stdout.take().context("stdout")?;
    let initial = read_line(&mut output)?;
    assert_eq!(initial["status"], "available");
    let fd = output.as_raw_fd();
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, 4096) }, 4096);
    let input = process.0.stdin.as_mut().context("stdin")?;
    let request = json!({"type":"action","request_id":"blocked","request":{"session":"stale","service":ITEM,"owner":initial["state"]["items"][0]["owner"],"path":ITEM_PATH,"action":{"action":"activate","x":0,"y":0}}});
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    let input_fd = input.as_raw_fd();
    let flags = unsafe { libc::fcntl(input_fd, libc::F_GETFL) };
    ensure!(
        flags >= 0
            && unsafe { libc::fcntl(input_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "nonblocking stdin"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut previous, mut stalled) = (0, 0);
    loop {
        match input.write(&line) {
            Ok(n) => assert_eq!(n, line.len()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        thread::sleep(Duration::from_millis(25));
        let mut queued: libc::c_int = 0;
        ensure!(
            unsafe { libc::ioctl(fd, libc::FIONREAD, &mut queued) } == 0,
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
        ensure!(Instant::now() < deadline, "Output did not stall");
    }
    stop(&mut process)?;
    Ok(())
}
struct FreedesktopWatcher {
    items: Vec<String>,
}
#[zbus::interface(name = "org.freedesktop.StatusNotifierWatcher")]
impl FreedesktopWatcher {
    fn register_status_notifier_host(&self, _service: &str) {}
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.clone()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}
struct FreedesktopItem {
    calls: Calls,
}
#[zbus::interface(name = "org.freedesktop.StatusNotifierItem")]
impl FreedesktopItem {
    fn activate(&self, x: i32, y: i32) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("activate:{x}:{y}"));
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "standard-item"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Standard item"
    }
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
}
#[test]
fn freedesktop_incumbent_is_used_without_competing_kde_watcher() -> Result<()> {
    let bus = Bus::new()?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _item = Builder::address(bus.address.as_str())?
        .name(ITEM)?
        .serve_at(
            ITEM_PATH,
            FreedesktopItem {
                calls: calls.clone(),
            },
        )?
        .build()?;
    let incumbent = Builder::address(bus.address.as_str())?
        .name("org.freedesktop.StatusNotifierWatcher")?
        .serve_at(
            WATCHER_PATH,
            FreedesktopWatcher {
                items: vec![format!("{ITEM}{ITEM_PATH}")],
            },
        )?
        .build()?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    let state = snapshot(&mut client)?;
    assert_eq!(state["owns_watcher"], false);
    assert_eq!(
        state["watcher_owner"],
        incumbent.unique_name().context("owner")?.as_str()
    );
    assert_eq!(state["items"][0]["id"], "standard-item");
    execute(
        &mut client,
        &state,
        json!({"action":"activate","x":5,"y":6}),
    )?;
    assert_eq!(*calls.lock().expect("calls"), ["activate:5:6"]);
    let connection = bus.connection()?;
    let dbus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owned: bool = dbus.call("NameHasOwner", &(WATCHER))?;
    assert!(!owned);
    Ok(())
}
#[test]
fn menu_property_change_invalidates_epoch_without_layout_revision_change() -> Result<()> {
    let bus = Bus::new()?;
    let (item, _, _) = item(&bus, false)?;
    let _watcher = watcher(&bus)?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    let old = snapshot(&mut client)?;
    item.object_server().remove::<Menu, _>("/Menu")?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    item.object_server().at(
        "/Menu",
        ChangedMenu {
            calls: calls.clone(),
        },
    )?;
    let fresh = snapshot(&mut client)?;
    assert_eq!(
        old["items"][0]["menu"]["revision"],
        fresh["items"][0]["menu"]["revision"]
    );
    assert_ne!(
        old["items"][0]["menu"]["epoch"],
        fresh["items"][0]["menu"]["epoch"]
    );
    let menu = &old["items"][0]["menu"];
    assert!(execute(&mut client,&old,json!({"action":"menu_event","epoch":menu["epoch"],"revision":menu["revision"],"id":1,"event":"clicked","timestamp":123})).is_err());
    assert!(calls.lock().expect("calls").is_empty());
    Ok(())
}
struct ChangedMenu {
    calls: Calls,
}
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl ChangedMenu {
    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Layout) {
        (7, (0, HashMap::new(), vec![leaf(1, false), leaf(2, false)]))
    }
    fn event(&self, id: i32, _event: &str, _data: Value<'_>, _timestamp: u32) {
        self.calls.lock().expect("calls").push(format!("menu:{id}"));
    }
}
#[test]
fn worker_reconnects_when_incumbent_watcher_disappears() -> Result<()> {
    let bus = Bus::new()?;
    let incumbent = watcher(&bus)?;
    let mut process = worker(&bus, &["--observe"])?;
    let mut output = process.0.stdout.take().context("stdout")?;
    let old = read_line(&mut output)?;
    assert_eq!(old["status"], "available");
    assert_eq!(old["state"]["owns_watcher"], false);
    drop(incumbent);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let message = read_line(&mut output)?;
        if message["status"] == "available"
            && message["state"]["session"] != old["state"]["session"]
        {
            assert_eq!(message["state"]["owns_watcher"], true);
            break;
        }
        ensure!(Instant::now() < deadline, "Watcher recovery timed out");
    }
    stop(&mut process)?;
    Ok(())
}
#[test]
fn menu_png_pixels_are_bounded_and_bad_icons_do_not_disable_actions() -> Result<()> {
    let bus = Bus::new()?;
    let (_item, calls, _) = item(&bus, false)?;
    let _watcher = watcher(&bus)?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    let state = snapshot(&mut client)?;
    let menu = &state["items"][0]["menu"];
    let nodes = menu["nodes"].as_array().context("menu nodes")?;
    let valid = nodes
        .iter()
        .find(|n| n["id"] == 1)
        .context("valid icon node")?;
    assert_eq!(valid["icon"]["rgba"], json!([12, 34, 56, 255]));
    let invalid = nodes
        .iter()
        .find(|n| n["id"] == 3)
        .context("bad icon node")?;
    assert!(invalid["icon"].is_null());
    assert_eq!(invalid["enabled"], true);
    execute(
        &mut client,
        &state,
        json!({"action":"menu_event","epoch":menu["epoch"],"revision":menu["revision"],"id":3,"event":"clicked","timestamp":12}),
    )?;
    assert_eq!(*calls.lock().expect("calls"), ["menu:3:clicked:12"]);
    Ok(())
}
#[test]
fn owned_watcher_emits_both_interface_registration_and_removal_signals() -> Result<()> {
    let bus = Bus::new()?;
    let mut client = hearth::tray::TrayClient::connect(Some(&bus.address))?;
    let monitor = bus.connection()?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(WATCHER_PATH)?
        .build();
    let mut messages = zbus::blocking::MessageIterator::for_match_rule(rule, &monitor, Some(16))?;
    let (send, receive) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        for message in &mut messages {
            let Ok(message) = message else {
                break;
            };
            let header = message.header();
            let interface = header
                .interface()
                .map(|i| i.to_string())
                .unwrap_or_default();
            let member = header.member().map(|m| m.to_string()).unwrap_or_default();
            if send.send((interface, member)).is_err() {
                break;
            }
        }
    });
    let result = (|| -> Result<()> {
        let (item, _, _) = item(&bus, false)?;
        let registration = Proxy::new(
            &item,
            "org.freedesktop.StatusNotifierWatcher",
            WATCHER_PATH,
            "org.freedesktop.StatusNotifierWatcher",
        )?;
        registration.call::<_, _, ()>("RegisterStatusNotifierItem", &(ITEM_PATH))?;
        let expect_signals = |member: &str| -> Result<()> {
            let mut interfaces = Vec::new();
            for _ in 0..2 {
                let (interface, actual) = receive.recv_timeout(Duration::from_secs(2))?;
                assert_eq!(actual, member);
                interfaces.push(interface);
            }
            interfaces.sort();
            assert_eq!(
                interfaces,
                ["org.freedesktop.StatusNotifierWatcher", WATCHER]
            );
            Ok(())
        };
        expect_signals("StatusNotifierItemRegistered")?;
        drop(registration);
        drop(item);
        client.snapshot()?;
        expect_signals("StatusNotifierItemUnregistered")?;
        Ok(())
    })();
    monitor.close()?;
    reader.join().expect("signal reader");
    result
}
