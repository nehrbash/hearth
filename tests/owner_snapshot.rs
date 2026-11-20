use anyhow::Result;
use hearth::{gpu::Gpu, owner_panel, protocol::Snapshot, render};
use std::path::Path;

#[test]
fn owner_snapshots_match_without_executing_decisions() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/weft-owner.json"))?;
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    for (name, enabled) in [("w2-owner-readonly", false), ("w2-owner-controls", true)] {
        let view = owner_panel::build_with_controls(&snapshot, 920, 900, enabled, false)?;
        assert_eq!(
            view.buttons.iter().filter(|b| b.request.is_some()).count(),
            if enabled { 2 } else { 0 }
        );
        let pixels = render::offscreen_panel(&gpu, &view.panel, 920, 900)?;
        let path = format!("snapshots/{name}.png");
        if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
            render::write_png(Path::new(&path), 920, 900, &pixels)?;
        }
        let (w, h, expected) = render::read_png(Path::new(&path))?;
        assert_eq!((w, h), (920, 900));
        if pixels != expected {
            std::fs::create_dir_all("artifacts")?;
            render::write_png(
                Path::new(&format!("artifacts/{name}-actual.png")),
                920,
                900,
                &pixels,
            )?;
        }
        assert!(pixels == expected, "Owner snapshot differs: {name}");
    }
    Ok(())
}
