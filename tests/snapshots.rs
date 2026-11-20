use std::path::Path;

use anyhow::Result;
use hearth::{gpu::Gpu, render, scene};

#[test]
fn scene_matches_rgba_snapshot() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let actual = render::offscreen(&gpu, &scene::demo()?, 640, 400)?;
    let (width, height, expected) = render::read_png(Path::new("snapshots/m1-scene.png"))?;
    assert_eq!((width, height), (640, 400));
    if actual != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/m1-scene-actual.png"),
            640,
            400,
            &actual,
        )?;
    }
    assert!(
        actual == expected,
        "GPU output differs; inspect artifacts/m1-scene-actual.png"
    );
    Ok(())
}

#[test]
fn padded_readback_keeps_row_boundaries() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let scene = scene::Scene::new(vec![scene::rect(0.0, 1.0, 3.0, 1.0, [1.0, 0.0, 0.0, 1.0])])?;
    let pixels = render::offscreen(&gpu, &scene, 3, 3)?;
    assert_eq!(pixels.len(), 36);
    assert_eq!(
        &pixels[12..24],
        &[255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255]
    );
    assert_eq!(&pixels[..12], &pixels[24..]);
    Ok(())
}
