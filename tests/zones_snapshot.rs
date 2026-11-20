use std::path::Path;

use anyhow::Result;
use hearth::{gpu::Gpu, panel, protocol::Snapshot, render};

#[test]
fn all_zone_types_match_png() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-zones.json"))?;
    let panel = panel::zones(&snapshot, 920, 680)?;
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen_panel(&gpu, &panel, 920, 680)?;
    let (width, height, expected) = render::read_png(Path::new("snapshots/w0-zones.png"))?;
    assert_eq!((width, height), (920, 680));
    if pixels != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/w0-zones-actual.png"),
            920,
            680,
            &pixels,
        )?;
    }
    assert!(pixels == expected, "Inspect artifacts/w0-zones-actual.png");
    Ok(())
}

#[test]
fn empty_and_truncated_panels_are_explicit() -> Result<()> {
    let mut snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-zones.json"))?;
    snapshot.zones.clear();
    let empty = panel::zones(&snapshot, 920, 680)?;
    assert!(
        empty
            .labels
            .iter()
            .any(|label| label.text == "No zones published")
    );
    let fixture = Snapshot::decode(include_bytes!("../fixtures/weft-zones.json"))?;
    snapshot.zones = (0..5)
        .map(|i| {
            let mut zone = fixture.zones[0].clone();
            zone.name = format!("Zone {i}");
            zone
        })
        .collect();
    let clipped = panel::zones(&snapshot, 920, 680)?;
    assert!(
        clipped
            .labels
            .iter()
            .any(|label| label.text.starts_with("Showing 4 of 5"))
    );
    Ok(())
}
