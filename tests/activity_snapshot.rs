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
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(Path::new("snapshots/w1-activity.png"), 920, 900, &pixels)?;
    }
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

#[test]
fn overflow_counts_and_diagnostics_match_png() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-activity-overflow.json"))?;
    let panel = activity_panel::build(&snapshot, 920, 900)?;
    for (text, y) in [("+3 more", 322.0), ("+1 more", 525.0), ("+2 more", 758.0)] {
        assert!(
            panel
                .labels
                .iter()
                .any(|label| label.text == text && label.bounds[1] == y)
        );
    }
    assert_eq!(
        panel
            .labels
            .iter()
            .filter(|label| label.text == "+1 more" && label.bounds[1] == 525.0)
            .count(),
        2
    );
    assert!(
        panel
            .labels
            .iter()
            .any(|label| label.text == "+2 more inspection warnings")
    );
    assert!(
        panel
            .labels
            .iter()
            .any(|label| label.text.contains("current watch subscribers only"))
    );
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen_panel(&gpu, &panel, 920, 900)?;
    let path = Path::new("snapshots/w1-activity-overflow.png");
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(path, 920, 900, &pixels)?;
    }
    let (width, height, expected) = render::read_png(path)?;
    assert_eq!((width, height), (920, 900));
    if pixels != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/w1-activity-overflow-actual.png"),
            920,
            900,
            &pixels,
        )?;
    }
    assert!(pixels == expected, "Activity overflow snapshot differs");
    Ok(())
}

#[test]
fn minimum_height_keeps_warnings_clear_of_footer_and_uses_pixel_ellipsis() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-activity-overflow.json"))?;
    let panel = activity_panel::build(&snapshot, 760, 860)?;
    let footer = panel
        .labels
        .iter()
        .find(|label| label.text.starts_with("Weft · revision"))
        .expect("history footer");
    let diagnostics: Vec<_> = panel
        .labels
        .iter()
        .filter(|label| label.text.starts_with("Inspection"))
        .collect();
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].text.starts_with("Inspection (+3 more):"));
    assert!(diagnostics[0].bounds[1] + diagnostics[0].bounds[3] <= footer.bounds[1]);
    assert!(
        panel
            .labels
            .iter()
            .all(|label| label.bounds[1] + label.bounds[3] <= 860.0)
    );
    assert!(
        panel
            .labels
            .iter()
            .all(|label| label.overflow == hearth::text::Overflow::Ellipsis)
    );
    let raw = &snapshot.activity.as_ref().expect("activity").sessions[0].id;
    assert!(panel.labels.iter().any(|label| &label.text == raw));
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen_panel(&gpu, &panel, 760, 860)?;
    std::fs::create_dir_all("artifacts")?;
    render::write_png(
        Path::new("artifacts/w1-activity-minimum.png"),
        760,
        860,
        &pixels,
    )?;
    Ok(())
}
