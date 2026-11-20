use anyhow::{Context, Result, bail};
use hearth::wayland::{SurfaceOptions, run};

fn main() -> Result<()> {
    let mut options = SurfaceOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => {
                options.frames = Some(args.next().context("--frames requires a count")?.parse()?)
            }
            "--help" | "-h" => {
                println!(
                    "hearth [--frames COUNT]\nOpen a nonexclusive layer-shell surface; omit COUNT to remain open."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
    }
    run(options)
}
