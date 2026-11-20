use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, MessageIterator, Proxy, connection::Builder},
    message::Header,
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedValue, Value},
};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const FDO_WATCHER: &str = "org.freedesktop.StatusNotifierWatcher";
const FDO_ITEM: &str = "org.freedesktop.StatusNotifierItem";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
type Properties = HashMap<String, OwnedValue>;
type WirePixmaps = Vec<(i32, i32, Vec<u8>)>;
type WireTooltip = (String, WirePixmaps, String, String);
type WireLayout = (i32, Properties, Vec<OwnedValue>);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tooltip {
    pub title: String,
    pub description: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuNode {
    pub id: i32,
    pub parent: Option<i32>,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    pub toggle_type: Option<String>,
    pub toggle_state: i32,
    pub children_display: Option<String>,
    pub icon_name: Option<String>,
    pub icon: Option<Icon>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    pub path: String,
    pub revision: u32,
    pub epoch: u64,
    pub nodes: Vec<MenuNode>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub service: String,
    pub owner: String,
    pub path: String,
    pub id: String,
    pub title: String,
    pub category: String,
    pub status: String,
    pub icon_name: Option<String>,
    pub icon: Option<Icon>,
    pub attention_icon_name: Option<String>,
    pub attention_icon: Option<Icon>,
    pub overlay_icon_name: Option<String>,
    pub overlay_icon: Option<Icon>,
    pub tooltip: Option<Tooltip>,
    pub item_is_menu: bool,
    pub menu: Option<Menu>,
    pub error: Option<String>,
    pub menu_error: Option<String>,
    pub icon_error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraySnapshot {
    pub session: String,
    pub watcher_owner: String,
    pub owns_watcher: bool,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub session: String,
    pub service: String,
    pub owner: String,
    pub path: String,
    pub action: Action,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Activate {
        x: i32,
        y: i32,
    },
    SecondaryActivate {
        x: i32,
        y: i32,
    },
    ContextMenu {
        x: i32,
        y: i32,
    },
    Scroll {
        delta: i32,
        orientation: String,
    },
    MenuAboutToShow {
        epoch: u64,
        revision: u32,
        id: i32,
    },
    MenuEvent {
        epoch: u64,
        revision: u32,
        id: i32,
        event: String,
        timestamp: u32,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct ActionOutcome {
    pub status: &'static str,
    pub error: Option<String>,
}
fn nonce() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|b| format!("{b:02x}")).collect())
}
fn bounded(s: &str) -> Result<String> {
    ensure!(
        s.len() <= 4096 && !s.contains('\0'),
        "Tray text exceeds bounds"
    );
    Ok(s.into())
}
fn optional_text(p: &Properties, k: &str) -> Result<Option<String>> {
    p.get(k).map(|v| bounded(<&str>::try_from(v)?)).transpose()
}
fn bool_prop(p: &Properties, k: &str, default: bool) -> Result<bool> {
    Ok(p.get(k).map(bool::try_from).transpose()?.unwrap_or(default))
}
fn get_all(conn: &Connection, owner: &str, path: &str, iface: &str) -> Result<Properties> {
    Ok(
        Proxy::new(conn, owner, path, "org.freedesktop.DBus.Properties")?
            .call("GetAll", &(iface,))?,
    )
}
fn owner(conn: &Connection, name: &str) -> Result<String> {
    ensure!(name.len() <= 255, "Invalid bus name");
    zbus::names::BusName::try_from(name)?;
    Ok(Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?
    .call("GetNameOwner", &(name,))?)
}
fn identity(service: &str, path: &str) -> Result<()> {
    ensure!(
        service.len() <= 255 && path.len() <= 4096,
        "Tray identity exceeds bounds"
    );
    zbus::names::BusName::try_from(service)?;
    ObjectPath::try_from(path)?;
    Ok(())
}

pub fn argb_icon(width: i32, height: i32, argb: &[u8]) -> Result<Icon> {
    ensure!(
        width > 0 && height > 0 && width <= 256 && height <= 256,
        "Invalid tray pixmap dimensions"
    );
    ensure!(
        argb.len() == width as usize * height as usize * 4,
        "Invalid tray pixmap length"
    );
    let scale = (width.max(height) as f32 / 64.0).max(1.0);
    let (w, h) = (
        (width as f32 / scale).round().max(1.0) as u32,
        (height as f32 / scale).round().max(1.0) as u32,
    );
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let i = ((y as usize * height as usize / h as usize) * width as usize
                + x as usize * width as usize / w as usize)
                * 4;
            rgba.extend_from_slice(&[argb[i + 1], argb[i + 2], argb[i + 3], argb[i]]);
        }
    }
    Ok(Icon {
        width: w,
        height: h,
        rgba,
    })
}
fn pixmap(p: &Properties, key: &str) -> Result<Option<Icon>> {
    let Some(value) = p.get(key) else {
        return Ok(None);
    };
    let values: WirePixmaps = value.try_clone()?.try_into()?;
    ensure!(
        values.len() <= 16 && values.iter().map(|v| v.2.len()).sum::<usize>() <= 1024 * 1024,
        "Tray pixmaps exceed bounds"
    );
    let mut best = None;
    for (w, h, bytes) in values {
        let icon = argb_icon(w, h, &bytes)?;
        if best
            .as_ref()
            .is_none_or(|b: &Icon| icon.width * icon.height > b.width * b.height)
        {
            best = Some(icon);
        }
    }
    Ok(best)
}
fn icon(
    p: &Properties,
    name: &str,
    pix: &str,
    remaining: &mut usize,
    diagnostic: &mut Option<String>,
) -> Result<Option<Icon>> {
    let resolved = if let Some(name) = optional_text(p, name)? {
        match crate::icons::resolve(&name, optional_text(p, "IconThemePath")?.as_deref()) {
            Ok(icon) => icon.map(|i| Icon {
                width: i.width,
                height: i.height,
                rgba: i.rgba,
            }),
            Err(e) => {
                *diagnostic = Some(e.to_string().chars().take(512).collect());
                None
            }
        }
    } else {
        None
    };
    let result = match resolved {
        Some(i) => Some(i),
        None => match pixmap(p, pix) {
            Ok(icon) => icon,
            Err(e) => {
                *diagnostic = Some(e.to_string().chars().take(512).collect());
                None
            }
        },
    };
    if let Some(i) = &result {
        if i.rgba.len() > *remaining {
            return Ok(None);
        }
        *remaining -= i.rgba.len();
    }
    Ok(result)
}
struct MenuDecoder<'a> {
    nodes: Vec<MenuNode>,
    seen: HashSet<i32>,
    budget: &'a mut usize,
    diagnostic: &'a mut Option<String>,
    theme: Option<&'a str>,
    deadline: Instant,
}
impl MenuDecoder<'_> {
    fn parse(&mut self, layout: WireLayout, parent: Option<i32>, depth: usize) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "Menu observation deadline exceeded"
        );
        ensure!(
            depth <= 8 && self.nodes.len() < 256 && self.seen.insert(layout.0),
            "Invalid or oversized tray menu"
        );
        let (id, p, children) = layout;
        let toggle_type = optional_text(&p, "toggle-type")?;
        ensure!(
            toggle_type
                .as_deref()
                .is_none_or(|s| matches!(s, "checkmark" | "radio")),
            "Invalid menu toggle type"
        );
        let children_display = optional_text(&p, "children-display")?;
        ensure!(
            children_display.as_deref().is_none_or(|s| s == "submenu"),
            "Invalid menu child display"
        );
        let toggle_state = p
            .get("toggle-state")
            .map(i32::try_from)
            .transpose()?
            .unwrap_or(-1);
        ensure!((-1..=1).contains(&toggle_state), "Invalid toggle state");
        let icon_name = optional_text(&p, "icon-name")?;
        let decoded = (|| -> Result<Option<Icon>> {
            if let Some(name) = &icon_name
                && let Some(i) = crate::icons::resolve(name, self.theme)?
            {
                return Ok(Some(Icon {
                    width: i.width,
                    height: i.height,
                    rgba: i.rgba,
                }));
            }
            if let Some(data) = p.get("icon-data") {
                let bytes: Vec<u8> = data.try_clone()?.try_into()?;
                let i = crate::icons::decode_png(&bytes)?;
                return Ok(Some(Icon {
                    width: i.width,
                    height: i.height,
                    rgba: i.rgba,
                }));
            }
            Ok(None)
        })();
        let icon = match decoded {
            Ok(Some(i)) if i.rgba.len() <= *self.budget => {
                *self.budget -= i.rgba.len();
                Some(i)
            }
            Ok(_) => None,
            Err(e) => {
                *self.diagnostic = Some(e.to_string().chars().take(512).collect());
                None
            }
        };
        self.nodes.push(MenuNode {
            id,
            parent,
            label: optional_text(&p, "label")?.unwrap_or_default(),
            enabled: bool_prop(&p, "enabled", true)?,
            visible: bool_prop(&p, "visible", true)?,
            separator: optional_text(&p, "type")?.as_deref() == Some("separator"),
            toggle_type,
            toggle_state,
            children_display,
            icon_name,
            icon,
        });
        ensure!(children.len() <= 256, "Too many menu children");
        for child in children {
            self.parse(child.try_into()?, Some(id), depth + 1)?;
        }
        Ok(())
    }
}
fn menu(
    conn: &Connection,
    owner: &str,
    path: &str,
    budget: &mut usize,
    diagnostic: &mut Option<String>,
    theme: Option<&str>,
    deadline: Instant,
) -> Result<Menu> {
    identity(owner, path)?;
    let proxy = Proxy::new(conn, owner, path, MENU)?;
    let props = vec![
        "label",
        "enabled",
        "visible",
        "type",
        "toggle-type",
        "toggle-state",
        "children-display",
        "icon-name",
        "icon-data",
    ];
    let (revision, layout): (u32, WireLayout) = proxy.call("GetLayout", &(0i32, 8i32, props))?;
    ensure!(layout.0 == 0, "Menu root must be zero");
    let mut decoder = MenuDecoder {
        nodes: Vec::new(),
        seen: HashSet::new(),
        budget,
        diagnostic,
        theme,
        deadline,
    };
    decoder.parse(layout, None, 0)?;
    Ok(Menu {
        path: path.into(),
        revision,
        epoch: 0,
        nodes: decoder.nodes,
    })
}
#[derive(Clone)]
struct Registration {
    service: String,
    owner: String,
    path: String,
}
struct Watcher {
    items: Arc<Mutex<Vec<Registration>>>,
    hosts: Arc<Mutex<HashSet<String>>>,
    dirty: Arc<AtomicBool>,
}
#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("Missing sender".into()))?
            .to_string();
        let (service, path) = if service.starts_with('/') {
            (sender.clone(), service.to_owned())
        } else {
            (service.to_owned(), "/StatusNotifierItem".into())
        };
        identity(&service, &path).map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let dbus = zbus::fdo::DBusProxy::new(conn).await?;
        let name = zbus::names::BusName::try_from(service.as_str())
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let actual = dbus.get_name_owner(name).await?.to_string();
        if actual != sender {
            return Err(zbus::fdo::Error::AccessDenied(
                "Registration must belong to sender".into(),
            ));
        }
        {
            let mut items = self
                .items
                .lock()
                .map_err(|_| zbus::fdo::Error::Failed("Watcher lock".into()))?;
            if items
                .iter()
                .any(|r| r.service == service && r.path == path && r.owner == actual)
            {
                return Ok(());
            }
            if items.len() >= 64 {
                return Err(zbus::fdo::Error::LimitsExceeded(
                    "Too many tray items".into(),
                ));
            }
            items.retain(|r| r.service != service || r.path != path);
            items.push(Registration {
                service: service.clone(),
                owner: actual,
                path: path.clone(),
            });
        }
        self.dirty.store(true, Ordering::Release);
        Self::status_notifier_item_registered(&emitter, &format!("{service}{path}")).await?;
        conn.emit_signal(
            None::<&str>,
            WATCHER_PATH,
            FDO_WATCHER,
            "StatusNotifierItemRegistered",
            &(format!("{service}{path}"),),
        )
        .await?;
        Ok(())
    }
    async fn register_status_notifier_host(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        if service.len() > 255 {
            return Err(zbus::fdo::Error::InvalidArgs("Host name too long".into()));
        }
        let name = zbus::names::BusName::try_from(service)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let actual = zbus::fdo::DBusProxy::new(conn)
            .await?
            .get_name_owner(name)
            .await?;
        if header
            .sender()
            .is_none_or(|sender| sender.as_str() != actual.as_str())
        {
            return Err(zbus::fdo::Error::AccessDenied(
                "Host must belong to sender".into(),
            ));
        }
        {
            let mut hosts = self
                .hosts
                .lock()
                .map_err(|_| zbus::fdo::Error::Failed("Watcher lock".into()))?;
            if hosts.len() >= 64 {
                return Err(zbus::fdo::Error::LimitsExceeded("Too many hosts".into()));
            }
            hosts.insert(actual.to_string());
        }
        Self::status_notifier_host_registered(&emitter).await?;
        conn.emit_signal(
            None::<&str>,
            WATCHER_PATH,
            FDO_WATCHER,
            "StatusNotifierHostRegistered",
            &(),
        )
        .await?;
        Ok(())
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items
            .lock()
            .map(|v| {
                v.iter()
                    .map(|r| format!("{}{}", r.service, r.path))
                    .collect()
            })
            .unwrap_or_default()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        self.hosts.lock().is_ok_and(|h| !h.is_empty())
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct FdoWatcher(Watcher);
#[zbus::interface(name = "org.freedesktop.StatusNotifierWatcher")]
impl FdoWatcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.0
            .register_status_notifier_item(service, header, conn, emitter)
            .await
    }
    async fn register_status_notifier_host(
        &self,
        service: &str,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.0
            .register_status_notifier_host(service, header, conn, emitter)
            .await
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.registered_status_notifier_items()
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        self.0.is_status_notifier_host_registered()
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

pub struct TrayClient {
    conn: Connection,
    session: String,
    watcher_owner: String,
    watcher_name: &'static str,
    interfaces: HashMap<(String, String), &'static str>,
    owns_watcher: bool,
    registrations: Arc<Mutex<Vec<Registration>>>,
    hosts: Arc<Mutex<HashSet<String>>>,
    dirty: Arc<AtomicBool>,
    signal_thread: Option<JoinHandle<()>>,
    last: Option<TraySnapshot>,
    menu_epoch: u64,
    cancel: Arc<AtomicBool>,
}
impl TrayClient {
    pub fn connect(address: Option<&str>) -> Result<Self> {
        let session = nonce()?;
        let conn = match address {
            Some(a) => Builder::address(a)?,
            None => Builder::session()?,
        }
        .method_timeout(Duration::from_millis(300))
        .max_queued(128)
        .build()?;
        let dirty = Arc::new(AtomicBool::new(true));
        let registrations = Arc::new(Mutex::new(Vec::new()));
        let hosts = Arc::new(Mutex::new(HashSet::new()));
        conn.object_server().at(
            WATCHER_PATH,
            Watcher {
                items: registrations.clone(),
                hosts: hosts.clone(),
                dirty: dirty.clone(),
            },
        )?;
        conn.object_server().at(
            WATCHER_PATH,
            FdoWatcher(Watcher {
                items: registrations.clone(),
                hosts: hosts.clone(),
                dirty: dirty.clone(),
            }),
        )?;
        let watcher_name = if owner(&conn, WATCHER).is_ok() {
            WATCHER
        } else if owner(&conn, FDO_WATCHER).is_ok() {
            FDO_WATCHER
        } else {
            WATCHER
        };
        let owns_watcher = matches!(
            conn.request_name_with_flags(
                watcher_name,
                zbus::fdo::RequestNameFlags::DoNotQueue.into()
            ),
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner
                | zbus::fdo::RequestNameReply::AlreadyOwner)
        );
        let watcher_owner = owner(&conn, watcher_name)?;
        if owns_watcher && watcher_name == WATCHER {
            let _ = conn.request_name_with_flags(
                FDO_WATCHER,
                zbus::fdo::RequestNameFlags::DoNotQueue.into(),
            );
        }
        let host = conn
            .unique_name()
            .context("Missing unique name")?
            .to_string();
        if owns_watcher {
            hosts
                .lock()
                .map_err(|_| anyhow::anyhow!("Host lock"))?
                .insert(host);
        } else {
            let _: () = Proxy::new(&conn, watcher_owner.as_str(), WATCHER_PATH, watcher_name)?
                .call("RegisterStatusNotifierHost", &(host,))?;
        }
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .build();
        let mut signals = MessageIterator::for_match_rule(rule, &conn, Some(128))?;
        let changed = dirty.clone();
        let signal_thread = std::thread::spawn(move || {
            for message in &mut signals {
                let Ok(message) = message else { break };
                if message.header().interface().is_some_and(|i| {
                    matches!(
                        i.as_str(),
                        WATCHER
                            | FDO_WATCHER
                            | FDO_ITEM
                            | ITEM
                            | MENU
                            | "org.freedesktop.DBus.Properties"
                            | "org.freedesktop.DBus"
                    )
                }) {
                    changed.store(true, Ordering::Release);
                }
            }
            changed.store(true, Ordering::Release);
        });
        Ok(Self {
            conn,
            session,
            watcher_owner,
            watcher_name,
            interfaces: HashMap::new(),
            owns_watcher,
            registrations,
            hosts,
            dirty,
            signal_thread: Some(signal_thread),
            last: None,
            menu_epoch: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn changed(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
    fn check_deadline(&self, deadline: Instant) -> Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire) && Instant::now() < deadline,
            "Tray observation cancelled or deadline exceeded"
        );
        Ok(())
    }
    pub fn snapshot(&mut self) -> Result<TraySnapshot> {
        ensure!(
            owner(&self.conn, self.watcher_name)? == self.watcher_owner,
            "Tray watcher owner changed"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let registrations = if self.owns_watcher {
            let old = self
                .registrations
                .lock()
                .map_err(|_| anyhow::anyhow!("Watcher lock"))?
                .clone();
            let mut dead = Vec::new();
            for r in old {
                self.check_deadline(deadline)?;
                if owner(&self.conn, &r.service).ok().as_deref() != Some(r.owner.as_str()) {
                    self.conn.emit_signal(
                        None::<&str>,
                        WATCHER_PATH,
                        WATCHER,
                        "StatusNotifierItemUnregistered",
                        &(format!("{}{}", r.service, r.path),),
                    )?;
                    self.conn.emit_signal(
                        None::<&str>,
                        WATCHER_PATH,
                        FDO_WATCHER,
                        "StatusNotifierItemUnregistered",
                        &(format!("{}{}", r.service, r.path),),
                    )?;
                    dead.push(r);
                }
            }
            let live = {
                let mut current = self
                    .registrations
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Watcher lock"))?;
                current.retain(|r| {
                    !dead
                        .iter()
                        .any(|d| d.service == r.service && d.owner == r.owner && d.path == r.path)
                });
                current.clone()
            };
            let old_hosts = self
                .hosts
                .lock()
                .map_err(|_| anyhow::anyhow!("Host lock"))?
                .clone();
            for host in old_hosts {
                self.check_deadline(deadline)?;
                if owner(&self.conn, &host).is_err() {
                    self.hosts
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Host lock"))?
                        .remove(&host);
                }
            }
            live
        } else {
            let p = get_all(
                &self.conn,
                &self.watcher_owner,
                WATCHER_PATH,
                self.watcher_name,
            )?;
            let values: Vec<String> = p
                .get("RegisteredStatusNotifierItems")
                .context("Missing tray registrations")?
                .try_clone()?
                .try_into()?;
            ensure!(values.len() <= 64, "Too many tray registrations");
            let mut seen = HashSet::new();
            let mut entries = Vec::new();
            for value in values {
                self.check_deadline(deadline)?;
                ensure!(value.len() <= 4351, "Invalid registration");
                let (service, path) = value
                    .find('/')
                    .map(|i| (&value[..i], &value[i..]))
                    .unwrap_or((&value, "/StatusNotifierItem"));
                identity(service, path)?;
                if seen.insert((service.to_owned(), path.to_owned()))
                    && let Ok(actual) = owner(&self.conn, service)
                {
                    entries.push(Registration {
                        service: service.into(),
                        path: path.into(),
                        owner: actual,
                    });
                }
            }
            entries
        };
        let mut items = Vec::new();
        let mut image_budget = 128 * 1024;
        for r in registrations {
            let mut item = Item {
                service: r.service,
                owner: r.owner,
                path: r.path,
                id: String::new(),
                title: String::new(),
                category: String::new(),
                status: "Passive".into(),
                icon_name: None,
                icon: None,
                attention_icon_name: None,
                attention_icon: None,
                overlay_icon_name: None,
                overlay_icon: None,
                tooltip: None,
                item_is_menu: false,
                menu: None,
                error: None,
                menu_error: None,
                icon_error: None,
            };
            match self.read_item(&mut item, &mut image_budget, deadline) {
                Ok(()) => {}
                Err(e) => item.error = Some(e.to_string().chars().take(512).collect()),
            }
            items.push(item);
        }
        items.sort_by(|a, b| (&a.service, &a.path).cmp(&(&b.service, &b.path)));
        let snapshot = TraySnapshot {
            session: self.session.clone(),
            watcher_owner: self.watcher_owner.clone(),
            owns_watcher: self.owns_watcher,
            items,
        };
        ensure!(
            serde_json::to_vec(&snapshot)?.len() <= 1024 * 1024 - 1024,
            "Tray snapshot exceeds1MiB"
        );
        self.interfaces.retain(|(owner, path), _| {
            snapshot
                .items
                .iter()
                .any(|i| &i.owner == owner && &i.path == path)
        });
        self.last = Some(snapshot.clone());
        Ok(snapshot)
    }
    fn read_item(&mut self, item: &mut Item, budget: &mut usize, deadline: Instant) -> Result<()> {
        self.check_deadline(deadline)?;
        let (p, interface) = match get_all(&self.conn, &item.owner, &item.path, ITEM) {
            Ok(p) if !p.is_empty() => (p, ITEM),
            _ => {
                self.check_deadline(deadline)?;
                (
                    get_all(&self.conn, &item.owner, &item.path, FDO_ITEM)?,
                    FDO_ITEM,
                )
            }
        };
        self.interfaces
            .insert((item.owner.clone(), item.path.clone()), interface);
        item.id = optional_text(&p, "Id")?.context("Missing tray ID")?;
        item.title = optional_text(&p, "Title")?.unwrap_or_default();
        item.category = optional_text(&p, "Category")?.unwrap_or_default();
        item.status = optional_text(&p, "Status")?.context("Missing tray status")?;
        ensure!(
            matches!(
                item.status.as_str(),
                "Passive" | "Active" | "NeedsAttention"
            ),
            "Invalid tray status"
        );
        item.icon_name = optional_text(&p, "IconName")?;
        item.attention_icon_name = optional_text(&p, "AttentionIconName")?;
        item.overlay_icon_name = optional_text(&p, "OverlayIconName")?;
        item.icon = icon(&p, "IconName", "IconPixmap", budget, &mut item.icon_error)?;
        item.attention_icon = icon(
            &p,
            "AttentionIconName",
            "AttentionIconPixmap",
            budget,
            &mut item.icon_error,
        )?;
        item.overlay_icon = icon(
            &p,
            "OverlayIconName",
            "OverlayIconPixmap",
            budget,
            &mut item.icon_error,
        )?;
        item.item_is_menu = bool_prop(&p, "ItemIsMenu", false)?;
        if let Some(tooltip) = p.get("ToolTip") {
            let (_, _, title, description): WireTooltip = tooltip.try_clone()?.try_into()?;
            item.tooltip = Some(Tooltip {
                title: bounded(&title)?,
                description: bounded(&description)?,
            });
        }
        self.check_deadline(deadline)?;
        if let Some(value) = p.get("Menu") {
            let path = ObjectPath::try_from(value.try_clone()?)?.to_string();
            if path != "/" {
                match menu(
                    &self.conn,
                    &item.owner,
                    &path,
                    budget,
                    &mut item.icon_error,
                    optional_text(&p, "IconThemePath")?.as_deref(),
                    deadline,
                ) {
                    Ok(mut m) => {
                        let old = self
                            .last
                            .as_ref()
                            .and_then(|s| {
                                s.items.iter().find(|i| {
                                    i.service == item.service
                                        && i.owner == item.owner
                                        && i.path == item.path
                                })
                            })
                            .and_then(|i| i.menu.as_ref());
                        if let Some(old) = old.filter(|o| {
                            o.path == m.path && o.revision == m.revision && o.nodes == m.nodes
                        }) {
                            m.epoch = old.epoch;
                        } else {
                            self.menu_epoch = self
                                .menu_epoch
                                .checked_add(1)
                                .context("Menu epoch exhausted")?;
                            m.epoch = self.menu_epoch;
                        }
                        item.menu = Some(m);
                    }
                    Err(e) => item.menu_error = Some(e.to_string().chars().take(512).collect()),
                }
            }
        }
        ensure!(
            owner(&self.conn, &item.service)? == item.owner,
            "Tray item owner changed"
        );
        Ok(())
    }
    fn preflight(&mut self, r: &ActionRequest) -> Result<Item> {
        identity(&r.service, &r.path)?;
        zbus::names::UniqueName::try_from(r.owner.as_str())?;
        ensure!(r.session == self.session, "Tray session changed");
        let snapshot = self.snapshot()?;
        let item = snapshot
            .items
            .into_iter()
            .find(|i| i.service == r.service && i.owner == r.owner && i.path == r.path)
            .context("Tray item identity changed")?;
        ensure!(item.error.is_none(), "Tray item unavailable");
        match &r.action {
            Action::Scroll { delta, orientation } => ensure!(
                (-12000..=12000).contains(delta)
                    && matches!(orientation.as_str(), "vertical" | "horizontal"),
                "Invalid tray scroll"
            ),
            Action::MenuEvent {
                epoch,
                revision,
                id,
                ..
            }
            | Action::MenuAboutToShow {
                epoch,
                revision,
                id,
            } => {
                let m = item.menu.as_ref().context("Tray menu unavailable")?;
                ensure!(
                    m.epoch == *epoch && m.revision == *revision,
                    "Tray menu changed"
                );
                let n = m
                    .nodes
                    .iter()
                    .find(|n| n.id == *id)
                    .context("Unknown menu item")?;
                ensure!(
                    n.enabled && n.visible && !n.separator,
                    "Menu item disabled or hidden"
                );
                if let Action::MenuEvent { event, .. } = &r.action {
                    ensure!(
                        matches!(event.as_str(), "clicked" | "opened" | "closed"),
                        "Invalid menu event"
                    );
                }
                let mut parent = n.parent;
                while let Some(id) = parent {
                    let p = m
                        .nodes
                        .iter()
                        .find(|n| n.id == id)
                        .context("Missing menu parent")?;
                    ensure!(p.enabled && p.visible, "Menu ancestor disabled");
                    parent = p.parent;
                }
            }
            _ => {}
        }
        Ok(item)
    }
    pub fn execute(&mut self, r: &ActionRequest) -> ActionOutcome {
        let item = match self.preflight(r) {
            Ok(item) => item,
            Err(e) => {
                return ActionOutcome {
                    status: "rejected",
                    error: Some(e.to_string()),
                };
            }
        };
        let result = (|| -> Result<()> {
            let interface = self
                .interfaces
                .get(&(item.owner.clone(), item.path.clone()))
                .copied()
                .context("Tray interface unavailable")?;
            let proxy = Proxy::new(
                &self.conn,
                item.owner.as_str(),
                item.path.as_str(),
                interface,
            )?;
            match &r.action {
                Action::Activate { x, y } => {
                    let _: () = proxy.call("Activate", &(*x, *y))?;
                }
                Action::SecondaryActivate { x, y } => {
                    let _: () = proxy.call("SecondaryActivate", &(*x, *y))?;
                }
                Action::ContextMenu { x, y } => {
                    let _: () = proxy.call("ContextMenu", &(*x, *y))?;
                }
                Action::Scroll { delta, orientation } => {
                    let _: () = proxy.call("Scroll", &(*delta, orientation))?;
                }
                Action::MenuAboutToShow { id, .. } => {
                    let m = item.menu.as_ref().context("Missing menu")?;
                    let _: bool =
                        Proxy::new(&self.conn, item.owner.as_str(), m.path.as_str(), MENU)?
                            .call("AboutToShow", &(*id,))?;
                    let _: () = Proxy::new(&self.conn, item.owner.as_str(), m.path.as_str(), MENU)?
                        .call("Event", &(*id, "opened", Value::I32(0), 0u32))?;
                }
                Action::MenuEvent {
                    id,
                    event,
                    timestamp,
                    ..
                } => {
                    let m = item.menu.as_ref().context("Missing menu")?;
                    let _: () = Proxy::new(&self.conn, item.owner.as_str(), m.path.as_str(), MENU)?
                        .call("Event", &(*id, event, Value::I32(0), *timestamp))?;
                }
            }
            ensure!(
                owner(&self.conn, &item.service)? == item.owner,
                "Tray owner changed after dispatch"
            );
            Ok(())
        })();
        self.dirty.store(true, Ordering::Release);
        match result {
            Ok(()) => ActionOutcome {
                status: "applied",
                error: None,
            },
            Err(e) => ActionOutcome {
                status: "unknown",
                error: Some(e.to_string().chars().take(512).collect()),
            },
        }
    }
}
impl Drop for TrayClient {
    fn drop(&mut self) {
        let _ = self.conn.clone().close();
        if let Some(thread) = self.signal_thread.take() {
            let _ = thread.join();
        }
    }
}

pub enum Command {
    Action {
        request_id: String,
        request: ActionRequest,
    },
    Stop,
}
pub struct TrayService {
    commands: SyncSender<Command>,
    pub events: Receiver<serde_json::Value>,
    stop: Arc<AtomicBool>,
}
impl TrayService {
    pub fn start(address: Option<String>) -> Result<Self> {
        let (commands, rx) = mpsc::sync_channel(32);
        let (events, receiver) = mpsc::sync_channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || actor(address, rx, events, stopped));
        Ok(Self {
            commands,
            events: receiver,
            stop,
        })
    }
    pub fn send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|_| anyhow::anyhow!("Tray command queue unavailable"))
    }
}
impl Drop for TrayService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.commands.try_send(Command::Stop);
    }
}
fn envelope(state: Option<TraySnapshot>, error: Option<String>) -> serde_json::Value {
    serde_json::json!({"version":1,"service":"tray","status":if state.is_some(){"available"}else{"unavailable"},"observed_at_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,"state":state,"error":error})
}
fn actor(
    address: Option<String>,
    commands: Receiver<Command>,
    events: SyncSender<serde_json::Value>,
    stop: Arc<AtomicBool>,
) {
    let mut client = None;
    let mut last = None;
    let mut heartbeat = Instant::now() - Duration::from_secs(5);
    let mut retry = Instant::now();
    while !stop.load(Ordering::Acquire) {
        if client.is_none() && Instant::now() >= retry {
            match TrayClient::connect(address.as_deref()) {
                Ok(mut c) => {
                    c.cancel = stop.clone();
                    client = Some(c);
                    last = None;
                }
                Err(e) => {
                    if events
                        .try_send(envelope(
                            None,
                            Some(e.to_string().chars().take(512).collect()),
                        ))
                        .is_err()
                    {
                        return;
                    }
                    retry = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Command::Action {
                request_id,
                request,
            }) => {
                let outcome = match &mut client {
                    Some(c) => c.execute(&request),
                    None => ActionOutcome {
                        status: "rejected",
                        error: Some("Tray unavailable".into()),
                    },
                };
                if events.try_send(serde_json::json!({"type":"result","request_id":request_id,"status":outcome.status,"error":outcome.error})).is_err(){return}
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(c) = &mut client
            && (c.changed() || heartbeat.elapsed() >= Duration::from_secs(5))
        {
            match c.snapshot() {
                Ok(snapshot) => {
                    if last.as_ref() != Some(&snapshot)
                        || heartbeat.elapsed() >= Duration::from_secs(5)
                    {
                        if events
                            .try_send(envelope(Some(snapshot.clone()), None))
                            .is_ok()
                        {
                            last = Some(snapshot);
                            heartbeat = Instant::now();
                        } else {
                            c.dirty.store(true, Ordering::Release);
                        }
                    }
                }
                Err(e) => {
                    if events
                        .try_send(envelope(
                            None,
                            Some(e.to_string().chars().take(512).collect()),
                        ))
                        .is_err()
                    {
                        return;
                    }
                    client = None;
                    last = None;
                    retry = Instant::now() + Duration::from_secs(1);
                }
            }
        }
    }
}
