use std::{
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::PathBuf,
    ptr::NonNull,
    time::Instant,
};

use anyhow::{Context, Result};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
};

use crate::{
    animation::Seconds,
    animation_demo::Demo,
    gpu::Gpu,
    owner::{CommandRequest, OwnerActions},
    owner_panel::{self, OwnerButton},
    panel::{self, Panel},
    protocol::{Snapshot, SnapshotListener, SnapshotStore},
    render::{PanelRenderer, Renderer},
    scene::Scene,
    text::Label,
    workspace_panel,
    workspaces::{Instance, WorkspaceFeed, WorkspaceSnapshot},
};

pub struct SurfaceOptions {
    pub workspaces: bool,
    pub workspace_snapshot: Option<WorkspaceSnapshot>,
    pub hyprland_instance: Option<String>,
    pub owner_controls: bool,
    pub view: panel::View,
    pub width: u32,
    pub height: u32,
    pub frames: Option<u32>,
    pub scene: Option<Scene>,
    pub animate: bool,
    pub snapshot: Option<Snapshot>,
    pub listen: Option<PathBuf>,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            workspaces: false,
            workspace_snapshot: None,
            hyprland_instance: None,
            owner_controls: false,
            view: panel::View::Zones,
            width: 480,
            height: 320,
            frames: None,
            scene: None,
            animate: false,
            snapshot: None,
            listen: None,
        }
    }
}

struct Shutdown {
    read: UnixStream,
    handlers: Vec<signal_hook::SigId>,
}

impl Shutdown {
    fn new() -> io::Result<Self> {
        let (read, write) = UnixStream::pair()?;
        let mut shutdown = Self {
            read,
            handlers: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            shutdown
                .handlers
                .push(signal_hook::low_level::pipe::register(
                    signal,
                    write.try_clone()?,
                )?);
        }
        Ok(shutdown)
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            signal_hook::low_level::unregister(handler);
        }
    }
}

pub fn run(options: SurfaceOptions) -> Result<()> {
    anyhow::ensure!(
        options.width > 0 && options.height > 0,
        "Surface dimensions must be positive"
    );
    anyhow::ensure!(options.frames != Some(0), "Frame count must be positive");
    anyhow::ensure!(
        !options.owner_controls || matches!(options.view, panel::View::Owner),
        "Owner controls require the owner view"
    );
    let workspace_mode = options.workspaces || options.workspace_snapshot.is_some();
    let workspace_feed = if options.workspaces {
        let instance = Instance::discover(options.hyprland_instance.as_deref())?;
        eprintln!("workspace_instance={}", instance.signature);
        Some(WorkspaceFeed::start(instance)?)
    } else {
        None
    };
    let shutdown = Shutdown::new()?;
    let listener = options
        .listen
        .as_ref()
        .map(SnapshotListener::bind)
        .transpose()?;
    let mut snapshots = SnapshotStore::default();
    if let Some(snapshot) = options.snapshot {
        snapshots.apply(snapshot)?;
    }
    let zones_mode = listener.is_some() || snapshots.current().is_some();
    let conn = Connection::connect_to_env().context("Cannot connect to Wayland")?;
    let (globals, mut events) = registry_queue_init(&conn)?;
    let qh = events.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = LayerShell::bind(&globals, &qh).context("Compositor has no layer-shell")?;
    let layer = shell.create_layer_surface(
        &qh,
        compositor.create_surface(&qh),
        Layer::Top,
        Some("hearth"),
        None,
    );
    layer.set_size(options.width, options.height);
    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    layer.set_margin(40, 40, 0, 0);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(0);
    if workspace_mode {
        let region = Region::new(&compositor)?;
        layer
            .wl_surface()
            .set_input_region(Some(region.wl_region()));
        eprintln!("workspace_input_region=empty exclusive_zone=0");
    }
    layer.commit();
    let instance = Gpu::instance();
    let display =
        NonNull::new(conn.backend().display_ptr().cast()).context("Missing Wayland display")?;
    let window =
        NonNull::new(layer.wl_surface().id().as_ptr().cast()).context("Missing Wayland surface")?;
    // App drops the GPU surface before the layer and its retained connection.
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display)),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(window)),
        })?
    };
    let gpu = pollster::block_on(Gpu::request(&instance, Some(&surface)))?;
    let info = gpu.adapter.get_info();
    eprintln!(
        "adapter={} backend={:?} driver={}",
        info.name, info.backend, info.driver
    );
    let config = surface
        .get_default_config(&gpu.adapter, options.width, options.height)
        .context("No supported surface configuration")?;
    let animation = if options.animate {
        Some(Demo::new()?)
    } else {
        None
    };
    let scene = match &animation {
        Some(demo) => Some(demo.scene()?),
        None => options.scene,
    };
    let renderer = scene.as_ref().map(|_| Renderer::new(&gpu, config.format));
    let panel_renderer =
        (zones_mode || workspace_mode).then(|| PanelRenderer::new(&gpu, config.format));
    let mut app = App {
        workspace_mode,
        workspace_feed,
        workspace_snapshot: options.workspace_snapshot,
        workspace_error: None,
        workspace_outputs: Vec::new(),
        surface,
        layer,
        _connection: conn.clone(),
        gpu,
        config,
        scene,
        animation,
        last_frame: None,
        frame_pending: false,
        renderer,
        panel_renderer,
        panel: None,
        owner_controls: options.owner_controls,
        owner_actions: OwnerActions::new(options.owner_controls)?,
        owner_buttons: Vec::new(),
        pressed: None,
        seats: SeatState::new(&globals, &qh),
        pointer: None,
        snapshots,
        view: options.view,
        listener,
        configured: false,
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        frames: 0,
        limit: options.frames,
        exit: false,
        error: None,
    };
    while !app.exit {
        events.dispatch_pending(&mut app)?;
        app.apply_workspace_updates(&qh)?;
        app.apply_updates(&qh)?;
        app.apply_completions(&qh)?;
        if app.exit {
            break;
        }
        let Some(guard) = events.prepare_read() else {
            continue;
        };
        let mut interests = libc::POLLIN;
        match events.flush() {
            Ok(()) => {}
            Err(wayland_client::backend::WaylandError::Io(error))
                if error.kind() == io::ErrorKind::WouldBlock =>
            {
                interests |= libc::POLLOUT
            }
            Err(error) => return Err(error.into()),
        }
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: interests,
                revents: 0,
            },
            libc::pollfd {
                fd: app
                    .listener
                    .as_ref()
                    .map_or(-1, SnapshotListener::update_fd),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: app.owner_actions.completion_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: app
                    .workspace_feed
                    .as_ref()
                    .map_or(-1, WorkspaceFeed::update_fd),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: shutdown.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // The prepared Wayland read and listener own these descriptors through poll.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fds[4].revents & libc::POLLIN != 0 {
            break;
        }
        anyhow::ensure!(
            fds.iter()
                .all(|fd| fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0),
            "Wayland or update channel disconnected"
        );
        if fds[0].revents & libc::POLLIN != 0 {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(error))
                    if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    if let Some(error) = app.error {
        return Err(error);
    }
    eprintln!("presented_frames={}", app.frames);
    Ok(())
}

struct App {
    workspace_mode: bool,
    workspace_feed: Option<WorkspaceFeed>,
    workspace_snapshot: Option<WorkspaceSnapshot>,
    workspace_error: Option<String>,
    workspace_outputs: Vec<wl_output::WlOutput>,
    owner_controls: bool,
    owner_actions: OwnerActions,
    owner_buttons: Vec<OwnerButton>,
    pressed: Option<CommandRequest>,
    seats: SeatState,
    pointer: Option<(wl_seat::WlSeat, wl_pointer::WlPointer)>,
    view: panel::View,
    surface: wgpu::Surface<'static>,
    layer: LayerSurface,
    _connection: Connection,
    gpu: Gpu,
    config: wgpu::SurfaceConfiguration,
    scene: Option<Scene>,
    renderer: Option<Renderer>,
    panel_renderer: Option<PanelRenderer>,
    panel: Option<Panel>,
    snapshots: SnapshotStore,
    listener: Option<SnapshotListener>,
    configured: bool,
    animation: Option<Demo>,
    last_frame: Option<Instant>,
    frame_pending: bool,
    registry: RegistryState,
    outputs: OutputState,
    frames: u32,
    limit: Option<u32>,
    exit: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn rebuild_panel(&mut self) -> Result<()> {
        if self.workspace_mode {
            self.panel = Some(self.build_workspace_panel()?);
            return Ok(());
        }
        if self.panel_renderer.is_some() {
            let waiting = Snapshot {
                version: 1,
                kind: "snapshot".into(),
                workspace: "Awaiting Weft".into(),
                generation: "awaiting".into(),
                revision: 0,
                zones: Vec::new(),
                activity: None,
            };
            let current = self.snapshots.current();
            let (panel, buttons) = self.build_panel(current.as_deref().unwrap_or(&waiting))?;
            self.panel = Some(panel);
            self.owner_buttons = buttons;
        }
        Ok(())
    }

    fn build_workspace_panel(&self) -> Result<Panel> {
        let names: Vec<_> = self
            .workspace_outputs
            .iter()
            .filter_map(|output| self.outputs.info(output).and_then(|info| info.name))
            .collect();
        let result = self
            .workspace_snapshot
            .as_ref()
            .context("Awaiting compositor state")
            .and_then(|state| {
                anyhow::ensure!(
                    names.len() == 1,
                    "Awaiting one actual output for this surface"
                );
                workspace_panel::build(state, &names[0], self.config.width, self.config.height, 7)
                    .map(|view| view.panel)
            });
        let (mut panel, error) = match result {
            Ok(panel) => (panel, self.workspace_error.as_deref()),
            Err(error) => {
                return self.workspace_status_panel(&format!("Unavailable: {error}"));
            }
        };
        if let Some(error) = error {
            if let Some(title) = panel.labels.first_mut() {
                title.text = "Stale".into();
                title.color = [235, 195, 130, 255];
            }
            if let Some(footer) = panel.labels.last_mut() {
                footer.text = format!("Unavailable: {error}");
                footer.color = [235, 195, 130, 255];
                *footer = footer.clone().ellipsis();
            }
        }
        Ok(panel)
    }

    fn workspace_status_panel(&self, message: &str) -> Result<Panel> {
        let width = self.config.width as f32;
        let height = self.config.height as f32;
        Ok(Panel {
            scene: Scene::new(vec![crate::scene::rect(
                0.0,
                0.0,
                width,
                height,
                panel::color([17, 23, 32]),
            )])?,
            labels: vec![
                Label::new(
                    "Unavailable",
                    [8.0, 12.0, width - 16.0, 24.0],
                    12.0,
                    [235, 195, 130, 255],
                ),
                Label::new(
                    message,
                    [8.0, 46.0, width - 16.0, height - 64.0],
                    11.0,
                    [226, 232, 240, 255],
                )
                .wrap(),
            ],
        })
    }

    fn apply_workspace_updates(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(feed) = &self.workspace_feed else {
            return Ok(());
        };
        feed.drain_notifications()?;
        let update = feed.take_update();
        if update.snapshot.is_none() && update.error.is_none() {
            return Ok(());
        }
        if let Some(snapshot) = update.snapshot {
            eprintln!(
                "workspace_update instance={} workspaces={}",
                snapshot.instance,
                snapshot.workspaces.len()
            );
            self.workspace_snapshot = Some(snapshot);
            self.workspace_error = None;
        }
        if let Some(error) = update.error {
            eprintln!("workspace_unavailable={error}");
            self.workspace_error = Some(error);
        }
        self.rebuild_panel()?;
        if self.configured {
            self.draw(qh);
        }
        Ok(())
    }

    fn workspace_output_changed(&mut self, qh: &QueueHandle<Self>) {
        if !self.workspace_mode {
            return;
        }
        if let Err(error) = self.rebuild_panel() {
            self.error = Some(error);
            self.exit = true;
        } else if self.configured {
            self.draw(qh);
        }
    }

    fn build_panel(&self, snapshot: &Snapshot) -> Result<(Panel, Vec<OwnerButton>)> {
        if matches!(self.view, panel::View::Owner) {
            let view = owner_panel::build_with_controls(
                snapshot,
                self.config.width,
                self.config.height,
                self.owner_controls,
                self.owner_actions.is_busy(),
            )?;
            Ok((view.panel, view.buttons))
        } else {
            Ok((
                panel::build(snapshot, self.config.width, self.config.height, self.view)?,
                Vec::new(),
            ))
        }
    }

    fn apply_completions(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        self.owner_actions.drain_notifications()?;
        if let Some(completion) = self.owner_actions.take_completion() {
            match completion.result {
                Ok(response) => eprintln!(
                    "owner decision #{} broker response: {:?}; waiting for snapshot state",
                    completion.request.id, response
                ),
                Err(error) => eprintln!(
                    "owner decision #{} unconfirmed: {:?}",
                    completion.request.id, error
                ),
            }
            self.rebuild_panel()?;
            if self.configured {
                self.draw(qh);
            }
        }
        Ok(())
    }

    fn request_at(&self, position: (f64, f64)) -> Option<CommandRequest> {
        self.owner_buttons
            .iter()
            .find(|button| button.contains(position.0 as f32, position.1 as f32))?
            .request
            .clone()
    }

    fn apply_updates(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some(listener) = &self.listener else {
            return Ok(());
        };
        listener.drain_notifications()?;
        if let Some(error) = listener.take_error() {
            eprintln!("snapshot rejected: {error}");
        }
        let Some(snapshot) = listener.take_latest() else {
            return Ok(());
        };
        let (next, buttons) = match self.build_panel(&snapshot) {
            Ok(panel) => panel,
            Err(error) => {
                eprintln!("snapshot rejected: {error}");
                return Ok(());
            }
        };
        match self.snapshots.apply((*snapshot).clone()) {
            Ok(_) => {
                eprintln!(
                    "snapshot workspace={} generation={} revision={}",
                    snapshot.workspace, snapshot.generation, snapshot.revision
                );
                self.panel = Some(next);
                self.owner_buttons = buttons;
                if self.configured {
                    self.draw(qh);
                }
            }
            Err(error) => eprintln!("snapshot rejected: {error}"),
        }
        Ok(())
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if let Err(error) = self.draw_frame(qh) {
            self.error = Some(error);
            self.exit = true;
        }
    }

    fn draw_frame(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let now = Instant::now();
        let delta = self
            .last_frame
            .map_or(0.0, |previous| now.duration_since(previous).as_secs_f64());
        self.last_frame = Some(now);
        if let Some(animation) = &mut self.animation
            && animation.advance(Seconds::new(delta)?)
        {
            self.scene = Some(animation.scene()?);
        }
        let frame = self
            .surface
            .get_current_texture()
            .context("Cannot acquire surface texture")?;
        let view = frame.texture.create_view(&Default::default());
        if let (Some(panel), Some(renderer)) = (&self.panel, &mut self.panel_renderer) {
            renderer.draw(
                &self.gpu,
                &view,
                panel,
                [self.config.width, self.config.height],
            )?;
        } else if let (Some(scene), Some(renderer)) = (&mut self.scene, &mut self.renderer) {
            renderer.draw(
                &self.gpu,
                &view,
                scene,
                [self.config.width, self.config.height],
            )?;
            scene.take_damage();
        } else {
            self.gpu.clear(
                &view,
                wgpu::Color {
                    r: 0.035,
                    g: 0.065,
                    b: 0.11,
                    a: 1.0,
                },
            );
        }
        self.frames += 1;
        eprintln!(
            "presented_frame={} interval_ms={:.3} animation_active={} x={:.3}",
            self.frames,
            delta * 1000.0,
            self.animation.as_ref().is_some_and(Demo::active),
            self.animation.as_ref().map_or(0.0, Demo::position)
        );
        if self.limit.is_some_and(|limit| self.frames >= limit) {
            self.exit = true;
        } else if (self.limit.is_some() || self.animation.as_ref().is_some_and(Demo::active))
            && !self.frame_pending
        {
            self.frame_pending = true;
            self.layer
                .wl_surface()
                .frame(qh, self.layer.wl_surface().clone());
        }
        frame.present();
        Ok(())
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        self.draw(qh);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if surface == self.layer.wl_surface() && !self.workspace_outputs.contains(output) {
            self.workspace_outputs.push(output.clone());
            eprintln!(
                "workspace_surface_output={:?}",
                self.outputs.info(output).and_then(|info| info.name)
            );
            self.workspace_output_changed(qh);
        }
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if surface == self.layer.wl_surface() {
            self.workspace_outputs.retain(|existing| existing != output);
            self.workspace_output_changed(qh);
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &LayerSurface,
        config: LayerSurfaceConfigure,
        _: u32,
    ) {
        if config.new_size.0 > 0 {
            self.config.width = config.new_size.0;
        }
        if config.new_size.1 > 0 {
            self.config.height = config.new_size.1;
        }
        self.surface.configure(&self.gpu.device, &self.config);
        self.configured = true;
        if let Err(error) = self.rebuild_panel() {
            self.error = Some(error);
            self.exit = true;
            return;
        }
        self.draw(qh);
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.workspace_outputs.contains(&output) {
            self.workspace_output_changed(qh);
        }
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.workspace_outputs.contains(&output) {
            self.workspace_outputs
                .retain(|existing| existing != &output);
            self.workspace_output_changed(qh);
        }
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_layer!(App);
delegate_output!(App);
delegate_registry!(App);

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if self.owner_controls && capability == Capability::Pointer && self.pointer.is_none() {
            match self.seats.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some((seat, pointer)),
                Err(error) => eprintln!("Cannot create owner pointer: {error}"),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.remove_pointer(&seat);
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_pointer(&seat);
    }
}
impl App {
    fn remove_pointer(&mut self, seat: &wl_seat::WlSeat) {
        if self
            .pointer
            .as_ref()
            .is_some_and(|(owner, _)| owner == seat)
        {
            if let Some((_, pointer)) = self.pointer.take() {
                pointer.release();
            }
            self.pressed = None;
        }
    }
}
impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Leave { .. } => self.pressed = None,
                PointerEventKind::Press { button: 0x110, .. } => {
                    self.pressed = self.request_at(event.position)
                }
                PointerEventKind::Release { button: 0x110, .. } => {
                    let Some(request) = self.pressed.take() else {
                        continue;
                    };
                    if let Err(error) = self.apply_updates(qh) {
                        eprintln!("Owner decision cancelled: {error}");
                        continue;
                    }
                    if self.request_at(event.position).as_ref() != Some(&request) {
                        continue;
                    }
                    let current = self.snapshots.current();
                    let Some(activity) = current
                        .as_ref()
                        .and_then(|snapshot| snapshot.activity.as_ref())
                    else {
                        continue;
                    };
                    if let Err(error) = self.owner_actions.submit(request, activity) {
                        eprintln!("Owner decision not submitted: {error}");
                    }
                    if let Err(error) = self.rebuild_panel() {
                        self.error = Some(error);
                        self.exit = true;
                    } else if self.configured {
                        self.draw(qh);
                    }
                }
                _ => {}
            }
        }
    }
}
delegate_seat!(App);
delegate_pointer!(App);
