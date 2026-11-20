use anyhow::Result;
use hearth::{activity_panel, gpu::Gpu, protocol::Snapshot, render};
use std::path::Path;

#[test]
fn activity_snapshot_preserves_unknowns_and_matches_png() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-activity.json"))?;
    let panel = activity_panel::build(&snapshot, 920, 900)?;
    assert!(
        panel
            .labels
            .iter()
            .any(|label| label.text == "host unknown")
    );
    assert!(
        panel
            .labels
            .iter()
            .any(|label| label.text == "Expiry unavailable")
    );
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen_panel(&gpu, &panel, 920, 900)?;
    let (w, h, expected) = render::read_png(Path::new("snapshots/w1-activity.png"))?;
    assert_eq!((w, h), (920, 900));
    if pixels != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/w1-activity-actual.png"),
            920,
            900,
            &pixels,
        )?;
    }
    assert!(
        pixels == expected,
        "Inspect artifacts/w1-activity-actual.png"
    );
    Ok(())
}

#[test]
fn missing_inspection_is_not_an_empty_host_inventory() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-zones.json"))?;
    let panel = activity_panel::build(&snapshot, 920, 900)?;
    assert!(
        panel
            .labels
            .iter()
            .any(|label| label.text == "No activity snapshot")
    );
    assert!(
        !panel
            .labels
            .iter()
            .any(|label| label.text == "No sessions reported")
    );
    Ok(())
}
