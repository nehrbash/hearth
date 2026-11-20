use std::ptr::NonNull;

use anyhow::{Context, Result};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
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
    protocol::{wl_output, wl_surface},
};

use crate::gpu::Gpu;

pub struct SurfaceOptions {
    pub width: u32,
    pub height: u32,
    pub frames: Option<u32>,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            width: 480,
            height: 320,
            frames: None,
        }
    }
}

pub fn run(options: SurfaceOptions) -> Result<()> {
    anyhow::ensure!(
        options.width > 0 && options.height > 0,
        "Surface dimensions must be positive"
    );
    anyhow::ensure!(options.frames != Some(0), "Frame count must be positive");
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
    let mut app = App {
        surface,
        layer,
        _connection: conn.clone(),
        gpu,
        config,
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        frames: 0,
        limit: options.frames,
        exit: false,
        error: None,
    };
    while !app.exit {
        events.blocking_dispatch(&mut app)?;
    }
    if let Some(error) = app.error {
        return Err(error);
    }
    eprintln!("presented_frames={}", app.frames);
    Ok(())
}

struct App {
    surface: wgpu::Surface<'static>,
    layer: LayerSurface,
    _connection: Connection,
    gpu: Gpu,
    config: wgpu::SurfaceConfiguration,
    registry: RegistryState,
    outputs: OutputState,
    frames: u32,
    limit: Option<u32>,
    exit: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if let Err(error) = self.draw_frame(qh) {
            self.error = Some(error);
            self.exit = true;
        }
    }

    fn draw_frame(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let frame = self
            .surface
            .get_current_texture()
            .context("Cannot acquire surface texture")?;
        let view = frame.texture.create_view(&Default::default());
        self.gpu.clear(
            &view,
            wgpu::Color {
                r: 0.035,
                g: 0.065,
                b: 0.11,
                a: 1.0,
            },
        );
        self.frames += 1;
        if self.limit.is_some_and(|limit| self.frames >= limit) {
            self.exit = true;
        } else if self.limit.is_some() {
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
        self.draw(qh);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
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
        self.draw(qh);
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_compositor!(App);
delegate_layer!(App);
delegate_output!(App);
delegate_registry!(App);
