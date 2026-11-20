use anyhow::Result;
use hearth::{gpu::Gpu, panel, protocol::Snapshot, render};
use std::path::Path;

#[test]
fn media_state_preview_matches_snapshot() -> Result<()> {
    let snapshot = Snapshot::decode(include_bytes!("../fixtures/mpris.json"))?;
    let panel = panel::zones(&snapshot, 920, 680)?;
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let actual = render::offscreen_panel(&gpu, &panel, 920, 680)?;
    let (width, height, expected) = render::read_png(Path::new("snapshots/mpris.png"))?;
    assert_eq!((width, height), (920, 680));
    if actual != expected {
        render::write_png(Path::new("artifacts/mpris-actual.png"), 920, 680, &actual)?;
    }
    assert_eq!(actual, expected);
    Ok(())
}
