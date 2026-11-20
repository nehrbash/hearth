use anyhow::{Context, Result};
use hearth::{gpu::Gpu, interactive::InteractiveScene, render};
use std::path::Path;

#[test]
fn tray_items_menu_and_unavailable_match_snapshots() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    for name in ["tray", "tray-menu", "tray-unavailable"] {
        let output = std::process::Command::new("guile")
            .args([
                "--no-auto-compile",
                "-L",
                "scheme",
                "tests/tray_fixture.scm",
                name,
            ])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "Tray fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let nodes = value["nodes"].as_array().context("Fixture nodes missing")?;
        let mut scene = InteractiveScene::new();
        scene.apply(nodes)?;
        let actual = render::offscreen(&gpu, &scene.scene()?, 720, 760)?;
        let baseline = format!("snapshots/{name}.png");
        if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
            render::write_png(Path::new(&baseline), 720, 760, &actual)?;
        }
        let (width, height, expected) = render::read_png(Path::new(&baseline))?;
        assert_eq!((width, height), (720, 760));
        if actual != expected {
            render::write_png(
                Path::new(&format!("artifacts/{name}-actual.png")),
                720,
                760,
                &actual,
            )?;
        }
        assert!(actual == expected, "Tray {name} PNG differs");
    }
    Ok(())
}
