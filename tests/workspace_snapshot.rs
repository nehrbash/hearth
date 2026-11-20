use anyhow::Result;
use hearth::{gpu::Gpu, render, workspace_panel, workspaces::WorkspaceSnapshot};
use std::path::Path;

#[test]
fn workspace_bar_snapshot_preserves_named_and_special_slots() -> Result<()> {
    let state: WorkspaceSnapshot =
        serde_json::from_slice(include_bytes!("../fixtures/workspaces.json"))?;
    let view = workspace_panel::build(&state, "DP-1", 60, 1440, 7)?;
    assert_eq!(view.slots.len(), 9);
    assert!(view.slots.iter().any(|slot| slot.id == -98));
    assert!(view.slots.iter().any(|slot| slot.id == 12));
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen_panel(&gpu, &view.panel, 60, 1440)?;
    let path = Path::new("snapshots/m5-workspaces.png");
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(path, 60, 1440, &pixels)?;
    }
    let (width, height, expected) = render::read_png(path)?;
    assert_eq!((width, height), (60, 1440));
    if pixels != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/m5-workspaces-actual.png"),
            120,
            1000,
            &pixels,
        )?;
    }
    assert!(pixels == expected, "Workspace bar snapshot differs");
    Ok(())
}
