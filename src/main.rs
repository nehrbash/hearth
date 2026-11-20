use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use hearth::{
    animation_demo,
    gpu::Gpu,
    panel,
    protocol::Snapshot,
    render, scene,
    wayland::{SurfaceOptions, run},
};

fn main() -> Result<()> {
    let mut options = SurfaceOptions::default();
    let mut snapshot: Option<PathBuf> = None;
    let mut show_scene = false;
    let mut zones: Option<PathBuf> = None;
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
                    .context("--view requires zones or activity")?
                    .as_str()
                {
                    "zones" => panel::View::Zones,
                    "activity" => panel::View::Activity,
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
                    "hearth [--scene | --animate | --state FILE] [--view zones|activity] [--listen SOCKET] [--frames COUNT] [--snapshot PATH] [--animation-snapshots DIR]\nRender a layer-shell surface or a headless PNG. COUNT enables a finite render benchmark. --zones is an alias for --state."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
    }
    let zones_mode = zones.is_some() || options.listen.is_some();
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
        options.height = if options.view == panel::View::Activity {
            900
        } else {
            680
        };
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
        let (width, height, pixels) = if let Some(snapshot) = &options.snapshot {
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
