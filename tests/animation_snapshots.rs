use std::path::Path;

use anyhow::Result;
use hearth::{animation_demo, gpu::Gpu, render};

#[test]
fn spring_sequence_matches_60hz_snapshots() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    for (name, frames) in animation_demo::SNAPSHOTS {
        let actual = render::offscreen(&gpu, &animation_demo::sampled_scene(frames)?, 640, 400)?;
        let (width, height, expected) = render::read_png(&Path::new("snapshots").join(name))?;
        assert_eq!((width, height), (640, 400));
        if actual != expected {
            std::fs::create_dir_all("artifacts")?;
            render::write_png(&Path::new("artifacts").join(name), 640, 400, &actual)?;
        }
        assert!(
            actual == expected,
            "Animation differs: inspect artifacts/{name}"
        );
    }
    Ok(())
}
