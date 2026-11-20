use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use hearth::{
    gpu::Gpu,
    render, scene,
    wayland::{SurfaceOptions, run},
};

fn main() -> Result<()> {
    let mut options = SurfaceOptions::default();
    let mut snapshot: Option<PathBuf> = None;
    let mut show_scene = false;
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
            "--help" | "-h" => {
                println!(
                    "hearth [--scene] [--frames COUNT] [--snapshot PATH]\nRender a layer-shell surface or a headless scene PNG. COUNT enables a finite render benchmark."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
    }
    if let Some(path) = snapshot {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
        let pixels = render::offscreen(&gpu, &scene::demo()?, 640, 400)?;
        render::write_png(&path, 640, 400, &pixels)?;
        println!("{}", path.display());
        return Ok(());
    }
    if show_scene {
        options.width = 640;
        options.height = 400;
        options.scene = Some(scene::demo()?);
    }
    run(options)
}
