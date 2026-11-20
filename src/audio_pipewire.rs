use crate::audio::{
    self, Action, ActionRequest, AudioSnapshot, Capabilities, Defaults, Device, Kind, Node,
};
use anyhow::{Context, Result, ensure};
use pipewire as pw;
use pw::{
    permissions::PermissionFlags,
    spa::{
        self,
        param::{ParamInfoFlags, ParamType},
        pod::{Object, Pod, Property, Value, ValueArray},
        utils::result::AsyncSeq,
    },
    types::ObjectType,
};
use serde_json::{Value as Json, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
type RouteIdentity = (u32, Option<u64>, i32, i32);
fn session_nonce() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub enum Command {
    Action {
        request_id: String,
        request: ActionRequest,
    },
    Stop,
}
pub struct AudioService {
    commands: SyncSender<Command>,
    wake: UnixStream,
    pub events: Receiver<Json>,
    stop: std::sync::Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl AudioService {
    pub fn start(remote: Option<String>) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(32);
        let (events, out) = mpsc::sync_channel(32);
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let actor_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            loop {
                if actor_stop.load(Ordering::SeqCst) {
                    return;
                }
                match run_connection(remote.as_deref(), &rx, &read, &events, &actor_stop) {
                    Ok(true) => return,
                    Ok(false) => {}
                    Err(error) => {
                        if events
                            .try_send(
                                serde_json::to_value(crate::services::ServiceEnvelope::<
                                    AudioSnapshot,
                                >::observed_service(
                                    "pipewire", Err(error)
                                ))
                                .unwrap_or(Json::Null),
                            )
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(Command::Action { request_id, .. }) => {
                        if events
                            .try_send(result(
                                &request_id,
                                "rejected",
                                Some("PipeWire unavailable"),
                            ))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        Ok(Self {
            commands: tx,
            wake: write,
            events: out,
            stop,
            thread: Some(thread),
        })
    }
    pub fn send(&mut self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|_| anyhow::anyhow!("PipeWire command queue full or closed"))?;
        match self.wake.write(&[1]) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
impl Drop for AudioService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.wake.write(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn result(id: &str, status: &str, error: Option<&str>) -> Json {
    json!({"type":"result","request_id":id,"status":status,"error":error})
}
#[derive(Clone, Default)]
struct Props {
    volumes: Option<Vec<f32>>,
    mute: Option<bool>,
    channels: Vec<u32>,
}
fn props(value: &Value) -> Result<Props> {
    let Value::Object(object) = value else {
        anyhow::bail!("Invalid PipeWire Props object")
    };
    ensure!(
        object.type_ == spa::sys::SPA_TYPE_OBJECT_Props,
        "Wrong PipeWire Props type"
    );
    let mut p = Props::default();
    for field in &object.properties {
        match field.key {
            spa::sys::SPA_PROP_channelVolumes => {
                let Value::ValueArray(ValueArray::Float(v)) = &field.value else {
                    anyhow::bail!("Invalid channel volumes")
                };
                audio::validate_volumes(v)?;
                p.volumes = Some(v.clone());
            }
            spa::sys::SPA_PROP_mute => {
                let Value::Bool(v) = field.value else {
                    anyhow::bail!("Invalid mute")
                };
                p.mute = Some(v);
            }
            spa::sys::SPA_PROP_channelMap => {
                let Value::ValueArray(ValueArray::Id(v)) = &field.value else {
                    anyhow::bail!("Invalid channel map")
                };
                ensure!(v.len() <= 64, "Too many channels");
                p.channels = v.iter().map(|v| v.0).collect();
            }
            _ => {}
        }
    }
    Ok(p)
}
fn decode(pod: &Pod) -> Result<Value> {
    ensure!(
        pod.as_bytes().len() <= 64 * 1024,
        "PipeWire POD exceeds64KiB"
    );
    let (_, value) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(pod.as_bytes())
        .map_err(|_| anyhow::anyhow!("Invalid PipeWire POD"))?;
    Ok(value)
}
#[derive(Clone)]
struct Route {
    index: i32,
    props: Props,
}
fn route(pod: &Pod) -> Result<(i32, Route)> {
    let Value::Object(object) = decode(pod)? else {
        anyhow::bail!("Invalid Route")
    };
    let (mut index, mut device, mut values) = (None, None, None);
    for p in object.properties {
        match (p.key, p.value) {
            (spa::sys::SPA_PARAM_ROUTE_index, Value::Int(v)) => index = Some(v),
            (spa::sys::SPA_PARAM_ROUTE_device, Value::Int(v)) => device = Some(v),
            (spa::sys::SPA_PARAM_ROUTE_props, v) => values = Some(props(&v)?),
            _ => {}
        }
    }
    Ok((
        device.context("Missing route device")?,
        Route {
            index: index.context("Missing route index")?,
            props: values.context("Missing route properties")?,
        },
    ))
}
struct RouteRefresh {
    seq: i32,
    barrier: AsyncSeq,
    routes: HashMap<i32, Route>,
}
struct DeviceEntry {
    _listener: pw::device::DeviceListener,
    proxy: Rc<pw::device::Device>,
    dto: Device,
    pro: bool,
    writable: bool,
    route_write: bool,
    routes: HashMap<i32, Route>,
    refresh: Option<RouteRefresh>,
}
struct NodeEntry {
    _listener: pw::node::NodeListener,
    proxy: Rc<pw::node::Node>,
    dto: Node,
    profile: Option<i32>,
    writable: bool,
    props_write: bool,
    props: Props,
}
struct MetadataEntry {
    _listener: pw::metadata::MetadataListener,
    proxy: Rc<pw::metadata::Metadata>,
    writable: bool,
}
#[derive(Clone)]
struct Target {
    device: Option<RouteIdentity>,
    volumes: Option<Vec<f32>>,
    mute: Option<bool>,
    default_name: Option<String>,
}
struct Pending {
    id: String,
    request: ActionRequest,
    deadline: Instant,
    barrier: AsyncSeq,
    written: bool,
    synced: bool,
    target: Target,
}
struct State {
    session: String,
    cookie: u32,
    nodes: HashMap<u32, NodeEntry>,
    devices: HashMap<u32, DeviceEntry>,
    metadata: Option<(u32, MetadataEntry)>,
    defaults: Defaults,
    pending: Option<Pending>,
    initial: Option<AsyncSeq>,
    rounds: u8,
    ready: bool,
    stop: bool,
    error: Option<String>,
    seq: i32,
}
fn text(p: &spa::utils::dict::DictRef, key: &str) -> Option<String> {
    p.get(key).map(str::to_owned)
}
fn number<T: std::str::FromStr>(p: &spa::utils::dict::DictRef, key: &str) -> Option<T> {
    p.get(key).and_then(|s| s.parse().ok())
}
impl State {
    fn effective<'a>(&'a self, node: &'a NodeEntry) -> (&'a Props, bool, Option<RouteIdentity>) {
        if let (Some(id), Some(profile)) = (node.dto.device_id, node.profile)
            && let Some(device) = self.devices.get(&id)
            && !device.pro
            && let Some(route) = device.routes.get(&profile)
        {
            return (
                &route.props,
                device.writable && device.route_write,
                Some((id, device.dto.serial, profile, route.index)),
            );
        }
        (&node.props, node.writable && node.props_write, None)
    }
    fn snapshot(&self) -> Result<AudioSnapshot> {
        let mut nodes: Vec<_> = self
            .nodes
            .values()
            .map(|entry| {
                let mut n = entry.dto.clone();
                let (p, writable, _) = self.effective(entry);
                n.channels = p.channels.clone();
                n.channel_volumes_linear = p.volumes.clone();
                n.mute = p.mute;
                n.ready = self.ready && n.state != "creating" && n.state != "error";
                let has_identity = n.serial.is_some();
                n.capabilities = Capabilities {
                    volume: has_identity && writable && p.volumes.is_some(),
                    mute: has_identity && writable && p.mute.is_some(),
                    default: has_identity
                        && matches!(n.kind, Kind::Sink | Kind::Source)
                        && self.metadata.as_ref().is_some_and(|(_, m)| m.writable),
                };
                n
            })
            .collect();
        nodes.sort_by_key(|n| n.id);
        let mut devices: Vec<_> = self.devices.values().map(|d| d.dto.clone()).collect();
        devices.sort_by_key(|d| d.id);
        let result = AudioSnapshot {
            session: self.session.clone(),
            core_cookie: self.cookie,
            devices,
            nodes,
            defaults: self.defaults.clone(),
        };
        result.validate()?;
        Ok(result)
    }
    fn refresh_routes(&mut self, id: u32, core: &pw::core::Core) -> Result<()> {
        self.seq = self.seq.wrapping_add(1).max(1);
        let seq = self.seq;
        if let Some(device) = self.devices.get_mut(&id) {
            device
                .proxy
                .enum_params(seq, Some(ParamType::Route), 0, 256);
            let barrier = core.sync(0)?;
            device.refresh = Some(RouteRefresh {
                seq,
                barrier,
                routes: HashMap::new(),
            });
        }
        Ok(())
    }
    fn refresh_target(&mut self, id: u32, core: &pw::core::Core) -> Result<()> {
        if let Some(node) = self.nodes.get(&id) {
            node.proxy.enum_params(0, Some(ParamType::Props), 0, 1);
            if let Some(device) = node.dto.device_id {
                self.refresh_routes(device, core)?;
            }
        }
        Ok(())
    }
    fn begin(&mut self, id: String, request: ActionRequest, core: &pw::core::Core) -> Result<()> {
        ensure!(self.pending.is_none(), "Audio action already pending");
        let snapshot = self.snapshot()?;
        let node = snapshot.target(&request)?;
        let entry = self.nodes.get(&request.id).context("Node disappeared")?;
        let (_, _, device) = self.effective(entry);
        let target = match request.action {
            Action::Volume { value, unmute } => Target {
                device,
                volumes: Some(audio::scaled_volumes(
                    node.channel_volumes_linear
                        .as_deref()
                        .context("Unknown volumes")?,
                    value,
                )?),
                mute: unmute.then_some(false),
                default_name: None,
            },
            Action::Mute { value } => Target {
                device,
                volumes: None,
                mute: Some(value),
                default_name: None,
            },
            Action::Default => Target {
                device: None,
                volumes: None,
                mute: None,
                default_name: Some(node.name.clone()),
            },
        };
        self.refresh_target(request.id, core)?;
        self.pending = Some(Pending {
            id,
            request,
            deadline: Instant::now() + Duration::from_secs(3),
            barrier: core.sync(0)?,
            written: false,
            synced: false,
            target,
        });
        Ok(())
    }
    fn write_pending(&mut self, core: &pw::core::Core) -> Result<()> {
        let p = self.pending.as_ref().context("No pending action")?;
        let snapshot = self.snapshot()?;
        let node = snapshot.target(&p.request)?;
        let entry = self.nodes.get(&p.request.id).context("Node disappeared")?;
        let (_, _, device) = self.effective(entry);
        ensure!(
            p.target.default_name.is_some() || device == p.target.device,
            "Device route identity changed"
        );
        let mut target = p.target.clone();
        if let Action::Volume { value, .. } = p.request.action {
            target.volumes = Some(audio::scaled_volumes(
                node.channel_volumes_linear
                    .as_deref()
                    .context("Unknown volumes")?,
                value,
            )?);
        }
        if let Some(name) = &target.default_name {
            let (_, metadata) = self
                .metadata
                .as_ref()
                .context("Default metadata unavailable")?;
            let key = if node.kind == Kind::Sink {
                "default.configured.audio.sink"
            } else {
                "default.configured.audio.source"
            };
            metadata.proxy.set_property(
                0,
                key,
                Some("Spa:String:JSON"),
                Some(&serde_json::to_string(&json!({"name":name}))?),
            );
        } else {
            let mut fields = Vec::new();
            if let Some(values) = &target.volumes {
                fields.push(Property::new(
                    spa::sys::SPA_PROP_channelVolumes,
                    Value::ValueArray(ValueArray::Float(values.clone())),
                ));
            }
            if let Some(value) = target.mute {
                fields.push(Property::new(spa::sys::SPA_PROP_mute, Value::Bool(value)));
            }
            let values = Value::Object(Object {
                type_: spa::sys::SPA_TYPE_OBJECT_Props,
                id: ParamType::Props.as_raw(),
                properties: fields,
            });
            let (value, param) = if let Some((device, _, profile, _)) = target.device {
                let d = self.devices.get(&device).context("Device disappeared")?;
                let route = d.routes.get(&profile).context("Route disappeared")?;
                (
                    Value::Object(Object {
                        type_: spa::sys::SPA_TYPE_OBJECT_ParamRoute,
                        id: ParamType::Route.as_raw(),
                        properties: vec![
                            Property::new(spa::sys::SPA_PARAM_ROUTE_device, Value::Int(profile)),
                            Property::new(spa::sys::SPA_PARAM_ROUTE_index, Value::Int(route.index)),
                            Property::new(spa::sys::SPA_PARAM_ROUTE_props, values),
                            Property::new(spa::sys::SPA_PARAM_ROUTE_save, Value::Bool(true)),
                        ],
                    }),
                    ParamType::Route,
                )
            } else {
                (values, ParamType::Props)
            };
            let bytes = spa::pod::serialize::PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &value,
            )
            .map_err(|_| anyhow::anyhow!("Encode audio control"))?
            .0
            .into_inner();
            let pod = Pod::from_bytes(&bytes).context("Encoded POD invalid")?;
            if let Some((id, _, _, _)) = target.device {
                self.devices[&id].proxy.set_param(param, 0, pod);
            } else {
                entry.proxy.set_param(param, 0, pod);
            }
        }
        let id = p.request.id;
        self.pending
            .as_mut()
            .context("Pending action disappeared")?
            .written = true;
        self.refresh_target(id, core)?;
        let pending = self
            .pending
            .as_mut()
            .context("Pending action disappeared")?;
        pending.target = target;
        pending.written = true;
        pending.barrier = core.sync(0)?;
        Ok(())
    }
    fn confirmed(&self) -> bool {
        let Some(p) = &self.pending else {
            return false;
        };
        if !p.written || !p.synced {
            return false;
        }
        let Ok(snapshot) = self.snapshot() else {
            return false;
        };
        let Ok(node) = snapshot.target(&p.request) else {
            return false;
        };
        let target = &p.target;
        if target.default_name.is_none()
            && self
                .nodes
                .get(&p.request.id)
                .is_none_or(|entry| self.effective(entry).2 != target.device)
        {
            return false;
        }
        if let Some(name) = &target.default_name {
            return if node.kind == Kind::Sink {
                snapshot.defaults.configured_sink.as_ref() == Some(name)
            } else {
                snapshot.defaults.configured_source.as_ref() == Some(name)
            };
        }
        target.mute.is_none_or(|v| node.mute == Some(v))
            && target.volumes.as_ref().is_none_or(|target| {
                node.channel_volumes_linear.as_ref().is_some_and(|actual| {
                    actual.len() == target.len()
                        && actual
                            .iter()
                            .zip(target)
                            .all(|(a, b)| (a - b).abs() <= 0.0001)
                })
            })
    }
}
fn run_connection(
    remote: Option<&str>,
    commands: &Receiver<Command>,
    wake: &UnixStream,
    events: &SyncSender<Json>,
    stop: &AtomicBool,
) -> Result<bool> {
    let loop_ = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&loop_, None)?;
    let properties = remote.map(|name| pw::properties::properties! {*pw::keys::REMOTE_NAME=>name});
    let core = context.connect_rc(properties)?;
    let registry = core.get_registry_rc()?;
    let state = Rc::new(RefCell::new(State {
        session: session_nonce()?,
        cookie: 0,
        nodes: HashMap::new(),
        devices: HashMap::new(),
        metadata: None,
        defaults: Defaults::default(),
        pending: None,
        initial: Some(core.sync(0)?),
        rounds: 0,
        ready: false,
        stop: false,
        error: None,
        seq: 1,
    }));
    let weak = Rc::downgrade(&state);
    let done_core = core.clone();
    let done_events = events.clone();
    let error_loop = loop_.clone();
    let error_state = Rc::downgrade(&state);
    let info_state = Rc::downgrade(&state);
    let _core_listener = core
        .add_listener_local()
        .info(move |info| {
            if let Some(s) = info_state.upgrade() {
                s.borrow_mut().cookie = info.cookie();
            }
        })
        .done(move |_, seq| {
            let Some(s) = weak.upgrade() else {
                return;
            };
            let mut s = s.borrow_mut();
            for d in s.devices.values_mut() {
                if d.refresh.as_ref().is_some_and(|r| r.barrier == seq)
                    && let Some(r) = d.refresh.take()
                {
                    d.routes = r.routes;
                }
            }
            if s.initial == Some(seq) {
                s.rounds += 1;
                if s.rounds >= 3 {
                    s.ready = true;
                    s.initial = None;
                } else {
                    s.initial = done_core.sync(0).ok();
                }
            }
            if s.pending.as_ref().is_some_and(|p| p.barrier == seq) {
                if s.pending.as_ref().is_some_and(|p| p.written) {
                    if let Some(p) = &mut s.pending {
                        p.synced = true;
                    }
                } else if let Err(error) = s.write_pending(&done_core)
                    && let Some(p) = s.pending.take()
                {
                    let status = if p.written { "unknown" } else { "rejected" };
                    if done_events
                        .try_send(result(&p.id, status, Some(&error.to_string())))
                        .is_err()
                    {
                        s.stop = true;
                    }
                }
            }
        })
        .error(move |id, _, _, message| {
            if let Some(s) = error_state.upgrade() {
                let mut s = s.borrow_mut();
                s.error = Some(message.chars().take(512).collect());
                if id == 0 {
                    error_loop.quit();
                } else if let Some(p) = s.pending.as_mut() {
                    p.deadline = Instant::now();
                }
            }
        })
        .register();
    let global_state = state.clone();
    let global_registry = registry.clone();
    let global_core = core.clone();
    let global_loop = loop_.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let result = bind_global(&global_state, &global_registry, &global_core, global);
            if let Err(error) = result {
                global_state.borrow_mut().error = Some(error.to_string());
                global_loop.quit();
            }
        })
        .global_remove({
            let state = state.clone();
            move |id| {
                let mut s = state.borrow_mut();
                s.nodes.remove(&id);
                s.devices.remove(&id);
                if s.metadata.as_ref().is_some_and(|(old, _)| *old == id) {
                    s.metadata = None;
                    s.defaults = Defaults::default();
                }
            }
        })
        .register();
    let started = Instant::now();
    let mut last_publication = Instant::now() - Duration::from_secs(5);
    let mut last_snapshot: Option<AudioSnapshot> = None;
    let woke = Rc::new(Cell::new(false));
    let eof = Rc::new(Cell::new(false));
    let notify = woke.clone();
    let closed = eof.clone();
    let _wake_source = loop_.loop_().add_io(
        wake.try_clone()?,
        spa::support::system::IoFlags::IN,
        move |wake| {
            let mut bytes = [0u8; 64];
            loop {
                match wake.read(&mut bytes) {
                    Ok(0) => {
                        closed.set(true);
                        break;
                    }
                    Ok(_) => notify.set(true),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        closed.set(true);
                        break;
                    }
                }
            }
        },
    );
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(true);
        }
        loop_
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_secs(1)));
        if eof.get() || stop.load(Ordering::SeqCst) {
            return Ok(true);
        }
        if woke.replace(false) {
            while let Ok(command) = commands.try_recv() {
                match command {
                    Command::Stop => {
                        state.borrow_mut().stop = true;
                    }
                    Command::Action {
                        request_id,
                        request,
                    } => {
                        if let Err(error) =
                            state.borrow_mut().begin(request_id.clone(), request, &core)
                            && events
                                .try_send(result(&request_id, "rejected", Some(&error.to_string())))
                                .is_err()
                        {
                            return Ok(true);
                        }
                    }
                }
            }
        }
        let mut s = state.borrow_mut();
        if s.stop {
            return Ok(true);
        }
        if let Some(error) = s.error.clone() {
            if let Some(p) = s.pending.take()
                && events
                    .try_send(result(
                        &p.id,
                        if p.written { "unknown" } else { "rejected" },
                        Some("PipeWire reported an error"),
                    ))
                    .is_err()
            {
                return Ok(true);
            }
            return Err(anyhow::anyhow!(error.clone()));
        }
        if s.confirmed() {
            if let Some(p) = s.pending.take()
                && events.try_send(result(&p.id, "applied", None)).is_err()
            {
                return Ok(true);
            }
        } else if s
            .pending
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.deadline)
            && let Some(p) = s.pending.take()
            && events
                .try_send(result(
                    &p.id,
                    if p.written { "unknown" } else { "rejected" },
                    Some("Audio action deadline exceeded; do not retry automatically"),
                ))
                .is_err()
        {
            return Ok(true);
        }
        if s.ready {
            let snapshot = s.snapshot()?;
            if last_snapshot.as_ref() != Some(&snapshot)
                || last_publication.elapsed() >= Duration::from_secs(5)
            {
                let value =
                    serde_json::to_value(crate::services::ServiceEnvelope::observed_service(
                        "pipewire",
                        Ok(snapshot.clone()),
                    ))?;
                if events.try_send(value).is_ok() {
                    last_snapshot = Some(snapshot);
                    last_publication = Instant::now();
                }
            }
        }
        ensure!(
            s.ready || started.elapsed() < Duration::from_secs(3),
            "PipeWire initialization deadline exceeded"
        );
    }
}
fn bind_global(
    state: &Rc<RefCell<State>>,
    registry: &pw::registry::RegistryRc,
    core: &pw::core::CoreRc,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Result<()> {
    let Some(p) = global.props else {
        return Ok(());
    };
    let id = global.id;
    let writable = global
        .permissions
        .contains(PermissionFlags::W | PermissionFlags::X);
    match global.type_ {
        ObjectType::Node => {
            let kind = match p.get("media.class") {
                Some("Audio/Sink") => Kind::Sink,
                Some("Audio/Source") | Some("Audio/Source/Virtual") => Kind::Source,
                Some("Stream/Output/Audio") => Kind::Playback,
                Some("Stream/Input/Audio") => Kind::Capture,
                _ => return Ok(()),
            };
            ensure!(state.borrow().nodes.len() < 256, "Too many audio nodes");
            let proxy = Rc::new(registry.bind::<pw::node::Node, _>(global)?);
            let weak = Rc::downgrade(state);
            let param_state = weak.clone();
            let listener = proxy
                .add_listener_local()
                .info(move |info| {
                    if let Some(s) = weak.upgrade()
                        && let Some(n) = s.borrow_mut().nodes.get_mut(&id)
                    {
                        if info.change_mask().contains(pw::node::NodeChangeMask::STATE) {
                            n.dto.state = match info.state() {
                                pw::node::NodeState::Creating => "creating",
                                pw::node::NodeState::Suspended => "suspended",
                                pw::node::NodeState::Idle => "idle",
                                pw::node::NodeState::Running => "running",
                                pw::node::NodeState::Error(_) => "error",
                            }
                            .into();
                        }
                        if info.change_mask().contains(pw::node::NodeChangeMask::PROPS)
                            && let Some(p) = info.props()
                        {
                            n.profile = number(p, "card.profile.device");
                            n.dto.device_id = number(p, "device.id").or(n.dto.device_id);
                            n.dto.application_name = text(p, "application.name")
                                .or_else(|| n.dto.application_name.clone());
                        }
                        if info
                            .change_mask()
                            .contains(pw::node::NodeChangeMask::PARAMS)
                        {
                            n.props_write = info.params().iter().any(|p| {
                                p.id() == ParamType::Props
                                    && p.flags().contains(ParamInfoFlags::WRITE)
                            });
                        }
                    }
                })
                .param(move |_, param, _, _, pod| {
                    if param != ParamType::Props {
                        return;
                    }
                    if let Some(s) = param_state.upgrade()
                        && let Some(pod) = pod
                    {
                        match decode(pod).and_then(|v| props(&v)) {
                            Ok(p) => {
                                if let Some(n) = s.borrow_mut().nodes.get_mut(&id) {
                                    n.props = p;
                                }
                            }
                            Err(e) => s.borrow_mut().error = Some(e.to_string()),
                        }
                    }
                })
                .register();
            state.borrow_mut().nodes.insert(
                id,
                NodeEntry {
                    _listener: listener,
                    proxy: proxy.clone(),
                    dto: Node {
                        id,
                        serial: number(p, "object.serial"),
                        device_id: number(p, "device.id"),
                        kind,
                        name: text(p, "node.name").unwrap_or_default(),
                        description: text(p, "node.description"),
                        application_name: text(p, "application.name"),
                        state: "creating".into(),
                        ready: false,
                        channels: vec![],
                        channel_volumes_linear: None,
                        mute: None,
                        capabilities: Capabilities::default(),
                    },
                    profile: number(p, "card.profile.device"),
                    writable,
                    props_write: false,
                    props: Props::default(),
                },
            );
            proxy.subscribe_params(&[ParamType::Props]);
            proxy.enum_params(0, Some(ParamType::Props), 0, 1);
        }
        ObjectType::Device => {
            ensure!(state.borrow().devices.len() < 64, "Too many audio devices");
            let proxy = Rc::new(registry.bind::<pw::device::Device, _>(global)?);
            let weak = Rc::downgrade(state);
            let param_state = weak.clone();
            let core = core.clone();
            let listener = proxy
                .add_listener_local()
                .info(move |info| {
                    if let Some(s) = weak.upgrade() {
                        let mut s = s.borrow_mut();
                        let mut readable = false;
                        if let Some(d) = s.devices.get_mut(&id) {
                            if info
                                .change_mask()
                                .contains(pw::device::DeviceChangeMask::PROPS)
                                && let Some(p) = info.props()
                            {
                                d.pro = p.get("device.profile.pro") == Some("true");
                            }
                            if info
                                .change_mask()
                                .contains(pw::device::DeviceChangeMask::PARAMS)
                            {
                                d.route_write = false;
                                if let Some(param) =
                                    info.params().iter().find(|p| p.id() == ParamType::Route)
                                {
                                    d.route_write = param.flags().contains(ParamInfoFlags::WRITE);
                                    readable = param.flags().contains(ParamInfoFlags::READ);
                                } else {
                                    d.routes.clear();
                                }
                            }
                        }
                        if readable && let Err(e) = s.refresh_routes(id, &core) {
                            s.error = Some(e.to_string());
                        }
                    }
                })
                .param(move |seq, param, _, _, pod| {
                    if param != ParamType::Route {
                        return;
                    }
                    if let Some(s) = param_state.upgrade()
                        && let Some(pod) = pod
                    {
                        match route(pod) {
                            Ok((device, route)) => {
                                let mut s = s.borrow_mut();
                                if let Some(d) = s.devices.get_mut(&id)
                                    && let Some(refresh) = &mut d.refresh
                                    && refresh.seq == seq
                                {
                                    if refresh.routes.len() < 256 {
                                        refresh.routes.insert(device, route);
                                    } else {
                                        s.error = Some("Too many device routes".into());
                                    }
                                }
                            }
                            Err(e) => s.borrow_mut().error = Some(e.to_string()),
                        }
                    }
                })
                .register();
            state.borrow_mut().devices.insert(
                id,
                DeviceEntry {
                    _listener: listener,
                    proxy,
                    dto: Device {
                        id,
                        serial: number(p, "object.serial"),
                        name: text(p, "device.name").unwrap_or_default(),
                        description: text(p, "device.description"),
                        api: text(p, "device.api"),
                    },
                    pro: p.get("device.profile.pro") == Some("true"),
                    writable,
                    route_write: false,
                    routes: HashMap::new(),
                    refresh: None,
                },
            );
        }
        ObjectType::Metadata if p.get("metadata.name") == Some("default") => {
            let proxy = Rc::new(registry.bind::<pw::metadata::Metadata, _>(global)?);
            let weak = Rc::downgrade(state);
            let listener = proxy
                .add_listener_local()
                .property(move |subject, key, _, value| {
                    if subject != 0 {
                        return 0;
                    }
                    if let Some(s) = weak.upgrade() {
                        let mut s = s.borrow_mut();
                        let name = value
                            .and_then(|v| serde_json::from_str::<Json>(v).ok())
                            .and_then(|v| v.get("name").and_then(Json::as_str).map(str::to_owned));
                        match key {
                            Some("default.audio.sink") => s.defaults.sink = name,
                            Some("default.audio.source") => s.defaults.source = name,
                            Some("default.configured.audio.sink") => {
                                s.defaults.configured_sink = name
                            }
                            Some("default.configured.audio.source") => {
                                s.defaults.configured_source = name
                            }
                            None => s.defaults = Defaults::default(),
                            _ => {}
                        }
                    }
                    0
                })
                .register();
            state.borrow_mut().metadata = Some((
                id,
                MetadataEntry {
                    _listener: listener,
                    proxy,
                    writable,
                },
            ));
        }
        _ => {}
    }
    Ok(())
}
