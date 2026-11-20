use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use hearth::{
    animation_demo,
    gpu::Gpu,
    panel,
    protocol::Snapshot,
    render, scene,
    wayland::{SurfaceOptions, run},
    workspace_panel,
    workspaces::{Instance, WorkspaceSnapshot},
};

fn main() -> Result<()> {
    let mut options = SurfaceOptions::default();
    let mut snapshot: Option<PathBuf> = None;
    let mut show_scene = false;
    let mut zones: Option<PathBuf> = None;
    let mut workspace_file: Option<PathBuf> = None;
    let mut monitor: Option<String> = None;
    let mut animation_snapshots: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => {
                options.frames = Some(args.next().context("--frames requires a count")?.parse()?)
            }
            "--snapshot" => {
                snapshot = Some(
                    args.next()
                        .context("--snapshot requires a PNG path")?
                        .into(),
                )
            }
            "--scene" => show_scene = true,
            "--view" => {
                options.view = match args
                    .next()
                    .context("--view requires zones, activity, owner, or scene")?
                    .as_str()
                {
                    "zones" => panel::View::Zones,
                    "activity" => panel::View::Activity,
                    "owner" => panel::View::Owner,
                    "scene" => panel::View::Scene,
                    other => bail!("Unknown view: {other}"),
                };
            }
            "--zones" | "--state" => {
                zones = Some(args.next().context("--state requires a JSON path")?.into())
            }
            "--listen" => {
                options.listen = Some(
                    args.next()
                        .context("--listen requires a socket path")?
                        .into(),
                )
            }
            "--workspaces" => options.workspaces = true,
            "--workspace-state" => {
                workspace_file = Some(
                    args.next()
                        .context("--workspace-state requires a JSON path")?
                        .into(),
                )
            }
            "--hyprland-instance" => {
                options.hyprland_instance = Some(
                    args.next()
                        .context("--hyprland-instance requires a signature")?,
                )
            }
            "--monitor" => {
                monitor = Some(args.next().context("--monitor requires an output name")?)
            }
            "--owner-controls" => options.owner_controls = true,
            "--animate" => options.animate = true,
            "--animation-snapshots" => {
                animation_snapshots = Some(
                    args.next()
                        .context("--animation-snapshots requires a directory")?
                        .into(),
                )
            }
            "--help" | "-h" => {
                println!(
                    "hearth [--scene | --animate | --state FILE] [--view zones|activity|owner|scene] [--owner-controls] [--listen SOCKET] [--frames COUNT] [--snapshot PATH] [--animation-snapshots DIR]\nWorkspace bar: --workspaces [--hyprland-instance SIG], or --workspace-state FILE --snapshot PNG [--monitor NAME].\nRender a layer-shell surface or a headless PNG. COUNT enables a finite render benchmark. --zones is an alias for --state."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
    }
    anyhow::ensure!(
        !options.owner_controls || (options.view == panel::View::Owner && snapshot.is_none()),
        "--owner-controls requires a native owner view"
    );
    let workspace_mode = options.workspaces || workspace_file.is_some();
    anyhow::ensure!(
        !(options.workspaces && workspace_file.is_some()),
        "Choose live --workspaces or --workspace-state"
    );
    anyhow::ensure!(
        !workspace_mode
            || !(zones.is_some()
                || options.listen.is_some()
                || show_scene
                || options.animate
                || animation_snapshots.is_some()
                || options.owner_controls
                || options.view != panel::View::Zones),
        "Workspace bar cannot be combined with Weft or scene modes"
    );
    anyhow::ensure!(
        monitor.is_none() || (workspace_mode && snapshot.is_some()),
        "--monitor is supported only for headless workspace snapshots; native bars follow their actual output"
    );
    anyhow::ensure!(
        options.hyprland_instance.is_none() || options.workspaces,
        "--hyprland-instance requires --workspaces"
    );
    if let Some(path) = workspace_file {
        let bytes =
            std::fs::read(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        anyhow::ensure!(
            bytes.len() <= 1024 * 1024,
            "Workspace snapshot exceeds 1 MiB"
        );
        let state: WorkspaceSnapshot = serde_json::from_slice(&bytes)?;
        state.validate()?;
        options.workspace_snapshot = Some(state);
    }
    if workspace_mode {
        options.width = 60;
        options.height = 1440;
        if snapshot.is_some() && options.workspaces {
            options.workspace_snapshot =
                Some(Instance::discover(options.hyprland_instance.as_deref())?.snapshot()?);
        }
    }
    let zones_mode = zones.is_some() || options.listen.is_some();
    anyhow::ensure!(
        options.view == panel::View::Zones || zones_mode,
        "--view requires --state or --listen"
    );
    anyhow::ensure!(
        !(zones_mode && (show_scene || options.animate || animation_snapshots.is_some())),
        "Zones mode cannot be combined with a scene or animation demo"
    );
    anyhow::ensure!(
        !(options.listen.is_some() && snapshot.is_some()),
        "--listen requires a native surface"
    );
    if let Some(path) = zones {
        let bytes =
            std::fs::read(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        options.snapshot = Some(Snapshot::decode(&bytes)?);
    }
    if zones_mode {
        options.width = 920;
        options.height = if options.view != panel::View::Zones {
            900
        } else {
            680
        };
    }
    if zones_mode && options.view == panel::View::Scene {
        options.width = 640;
        options.height = 400;
    }
    if let Some(directory) = animation_snapshots {
        std::fs::create_dir_all(&directory)?;
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
        for (name, frames) in animation_demo::SNAPSHOTS {
            let pixels =
                render::offscreen(&gpu, &animation_demo::sampled_scene(frames)?, 640, 400)?;
            let path = directory.join(name);
            render::write_png(&path, 640, 400, &pixels)?;
            println!("{}", path.display());
        }
        return Ok(());
    }
    if let Some(path) = snapshot {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
        let (width, height, pixels) = if let Some(state) = &options.workspace_snapshot {
            let monitor = monitor
                .as_deref()
                .or_else(|| {
                    state
                        .monitors
                        .iter()
                        .find(|m| m.focused)
                        .map(|m| m.name.as_str())
                })
                .context("No focused monitor; specify --monitor for this snapshot")?;
            let view = workspace_panel::build(state, monitor, options.width, options.height, 7)?;
            (
                options.width,
                options.height,
                render::offscreen_panel(&gpu, &view.panel, options.width, options.height)?,
            )
        } else if let Some(snapshot) = &options.snapshot {
            (
                options.width,
                options.height,
                render::offscreen_panel(
                    &gpu,
                    &panel::build(snapshot, options.width, options.height, options.view)?,
                    options.width,
                    options.height,
                )?,
            )
        } else {
            (
                640,
                400,
                render::offscreen(&gpu, &scene::demo()?, 640, 400)?,
            )
        };
        render::write_png(&path, width, height, &pixels)?;
        println!("{}", path.display());
        return Ok(());
    }
    if show_scene || options.animate {
        options.width = 640;
        options.height = 400;
        options.scene = Some(scene::demo()?);
    }
    run(options)
}
