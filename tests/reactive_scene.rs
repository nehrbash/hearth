use anyhow::Result;
use hearth::{
    binding::BoundScene,
    builder::SceneBuilder,
    gpu::Gpu,
    reactive::Runtime,
    render,
    scene::{Bounds, Node},
    scene_data::{self, SceneData},
    text::Label,
};
use std::path::Path;

const INK: [u8; 4] = [226, 232, 240, 255];
const MUTED: [u8; 4] = [148, 163, 184, 255];
const PANEL: [f32; 4] = [0.025, 0.045, 0.07, 1.0];
fn label(text: impl Into<String>, x: f32, y: f32, width: f32, size: f32, color: [u8; 4]) -> Label {
    Label::new(text, [x, y, width, 36.0], size, color)
}
fn status_widget() -> SceneBuilder {
    SceneBuilder::new()
        .rect(Bounds::new(0.0, 0.0, 592.0, 64.0), PANEL)
        .rect(Bounds::new(14.0, 23.0, 12.0, 12.0), [0.2, 0.7, 0.55, 1.0])
        .text(label("Weft connected", 40.0, 14.0, 300.0, 20.0, INK))
        .text(label("isolated", 450.0, 16.0, 120.0, 16.0, MUTED))
}
fn metric_widget(count: u32) -> SceneBuilder {
    SceneBuilder::new()
        .rect(Bounds::new(0.0, 0.0, 592.0, 84.0), PANEL)
        .text(label("Active sessions", 16.0, 12.0, 360.0, 18.0, MUTED))
        .text(label(count.to_string(), 500.0, 20.0, 70.0, 28.0, INK))
}
fn sessions_widget() -> SceneBuilder {
    SceneBuilder::new()
        .rect(Bounds::new(0.0, 0.0, 592.0, 118.0), PANEL)
        .text(label("SESSION", 16.0, 8.0, 260.0, 14.0, MUTED))
        .text(label("CHANNEL", 330.0, 8.0, 240.0, 14.0, MUTED))
        .text(label("codex", 16.0, 42.0, 260.0, 18.0, INK))
        .text(label("workspace", 330.0, 42.0, 240.0, 18.0, INK))
        .text(label("claude", 16.0, 76.0, 260.0, 18.0, INK))
        .text(label("terminal", 330.0, 76.0, 240.0, 18.0, INK))
}
fn workspace(count: u32) -> Vec<Node> {
    SceneBuilder::new()
        .rect(
            Bounds::new(0.0, 0.0, 640.0, 400.0),
            [0.008, 0.012, 0.02, 1.0],
        )
        .text(label(
            "Hearth / Weft workspace",
            24.0,
            18.0,
            592.0,
            26.0,
            INK,
        ))
        .group([24.0, 70.0], status_widget())
        .group([24.0, 150.0], metric_widget(count))
        .group([24.0, 250.0], sessions_widget())
        .nodes()
}

#[test]
fn scheme_scene_matches_rust_widgets_and_reactive_pngs() -> Result<()> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/scheme-scene.json"))?;
    assert_eq!(fixture["version"], 1);
    assert_eq!(fixture["zones"].as_array().map(Vec::len), Some(2));
    let data: Vec<SceneData> = serde_json::from_value(fixture["scene"].clone())?;
    assert_eq!(scene_data::build_nodes(&data)?, workspace(2));
    let runtime = Runtime::new();
    let count = runtime.signal(2);
    let source = count.clone();
    let mut scene = BoundScene::new(&runtime, move |cx| workspace(cx.read(&source)))?;
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    for state in [2, 5] {
        if state == 5 {
            count.set(3);
            count.set(4);
            count.set(5);
        }
        assert_eq!(scene.flush()?.effects, 1);
        assert!(scene.take_damage());
        assert!(!scene.take_damage());
        let pixels = render::offscreen(&gpu, scene.scene(), 640, 400)?;
        let path = format!("snapshots/m6-reactive-{state}.png");
        if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
            render::write_png(Path::new(&path), 640, 400, &pixels)?;
        }
        let (width, height, expected) = render::read_png(Path::new(&path))?;
        assert_eq!((width, height), (640, 400));
        if pixels != expected {
            std::fs::create_dir_all("artifacts")?;
            render::write_png(
                Path::new(&format!("artifacts/m6-reactive-{state}-actual.png")),
                640,
                400,
                &pixels,
            )?;
        }
        assert!(
            pixels == expected,
            "Reactive scene snapshot differs for state {state}"
        );
    }
    assert_eq!(scene.flush()?.effects, 0);
    assert!(!scene.take_damage());
    Ok(())
}
