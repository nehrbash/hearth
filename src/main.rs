use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use hearth::{
    animation_demo,
    gpu::Gpu,
    render, scene,
    wayland::{SurfaceOptions, run},
};

fn main() -> Result<()> {
    let mut options = SurfaceOptions::default();
    let mut snapshot: Option<PathBuf> = None;
    let mut show_scene = false;
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
                    "hearth [--scene | --animate] [--frames COUNT] [--snapshot PATH] [--animation-snapshots DIR]\nRender a layer-shell surface or a headless scene PNG. COUNT enables a finite render benchmark."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
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
        let pixels = render::offscreen(&gpu, &scene::demo()?, 640, 400)?;
        render::write_png(&path, 640, 400, &pixels)?;
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
