use anyhow::Result;
use hearth::{
    gpu::Gpu,
    render,
    scene::{Bounds, Node, Scene, rect},
    text::Label,
};
use std::path::Path;

fn specimen() -> Result<Scene> {
    let ink = [226, 232, 240, 255];
    Scene::new(vec![
        rect(0.0, 0.0, 800.0, 560.0, [0.008, 0.012, 0.02, 1.0]),
        Node::Text(Label::new("Hearth · retained text", [28.0, 22.0, 740.0, 42.0], 28.0, ink).bold()),
        Node::Text(Label::new("Shaped scripts · العربية · עברית · Ελληνικά", [28.0, 90.0, 740.0, 42.0], 23.0, ink)),
        Node::Text(Label::new("Combining marks: cafe\u{301} · A\u{30a} · naïve", [28.0, 143.0, 740.0, 40.0], 23.0, ink)),
        Node::Text(Label::new("Serif family: office affinity — Ω", [28.0, 194.0, 740.0, 40.0], 23.0, ink).with_family("DejaVu Serif")),
        Node::Text(Label::new("A long workspace title truncates at a shaped grapheme boundary", [28.0, 248.0, 355.0, 38.0], 22.0, ink).ellipsis()),
        Node::Group { offset: [28.0, 306.0], children: vec![
            rect(0.0, 0.0, 355.0, 100.0, [0.04, 0.075, 0.10, 1.0]),
            Node::Clip { bounds: Bounds::new(10.0, 10.0, 335.0, 80.0), children: vec![
                Node::Text(Label::new("Nested text obeys translated clipping. This wrapped sentence continues beyond the visible area.", [10.0, 10.0, 335.0, 150.0], 21.0, ink).wrap()),
            ]},
        ]},
        Node::Text(Label::new("Painter order: covered text", [420.0, 314.0, 350.0, 38.0], 24.0, ink)),
        rect(420.0, 352.0, 350.0, 54.0, [0.025, 0.045, 0.07, 1.0]),
        Node::Text(Label::new("BEHIND", [28.0, 447.0, 300.0, 60.0], 40.0, ink)),
        rect(115.0, 440.0, 90.0, 70.0, [0.10, 0.25, 0.22, 1.0]),
        Node::Text(Label::new("front", [125.0, 459.0, 78.0, 36.0], 21.0, ink)),
    ])
}

#[test]
fn retained_text_shaping_clipping_and_painter_order_match_png() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen(&gpu, &specimen()?, 800, 560)?;
    let path = Path::new("snapshots/m3-text-scene.png");
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(path, 800, 560, &pixels)?;
    }
    let (w, h, expected) = render::read_png(path)?;
    assert_eq!((w, h), (800, 560));
    if pixels != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/m3-text-scene-actual.png"),
            800,
            560,
            &pixels,
        )?;
    }
    assert!(
        pixels == expected,
        "Inspect artifacts/m3-text-scene-actual.png"
    );
    Ok(())
}
