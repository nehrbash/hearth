use anyhow::Result;
use hearth::{
    gpu::Gpu,
    render,
    scene::{Bounds, LayerEffect, Node, Scene, ShaderKind, rect},
    text::Label,
};
use std::path::Path;

fn specimen() -> Result<Scene> {
    let ink = [235, 231, 216, 255];
    Scene::new(vec![
        rect(0.0, 0.0, 800.0, 560.0, [0.012, 0.018, 0.028, 1.0]),
        Node::Text(
            Label::new(
                "Hearth · layers and shaders",
                [24.0, 16.0, 752.0, 40.0],
                27.0,
                ink,
            )
            .bold(),
        ),
        Node::Group {
            offset: [24.0, 76.0],
            children: vec![Node::Clip {
                bounds: Bounds::new(0.0, 0.0, 350.0, 240.0),
                children: vec![Node::Shader {
                    bounds: Bounds::new(-20.0, -20.0, 390.0, 280.0),
                    kind: ShaderKind::Moon,
                    time: 2.0,
                    parameters: [0.0; 4],
                }],
            }],
        },
        Node::Shader {
            bounds: Bounds::new(400.0, 76.0, 376.0, 240.0),
            kind: ShaderKind::Joined,
            time: 0.0,
            parameters: [0.0, 0.0, 0.0, 40.0],
        },
        Node::Text(Label::new(
            "Clipped quarter moon",
            [24.0, 319.0, 350.0, 28.0],
            18.0,
            ink,
        )),
        Node::Text(Label::new(
            "SDF joined panels",
            [400.0, 319.0, 376.0, 28.0],
            18.0,
            ink,
        )),
        Node::Layer {
            bounds: Bounds::new(24.0, 370.0, 350.0, 150.0),
            effect: LayerEffect::None,
            children: vec![
                rect(0.0, 0.0, 350.0, 150.0, [0.1, 0.24, 0.2, 0.6]),
                Node::Text(Label::new(
                    "Transparent layer",
                    [16.0, 16.0, 318.0, 32.0],
                    24.0,
                    ink,
                )),
                rect(20.0, 80.0, 300.0, 40.0, [0.8, 0.45, 0.1, 0.7]),
            ],
        },
        Node::Layer {
            bounds: Bounds::new(400.0, 370.0, 376.0, 150.0),
            effect: LayerEffect::Blur { radius: 2.0 },
            children: vec![
                rect(12.0, 12.0, 352.0, 126.0, [0.1, 0.24, 0.2, 0.6]),
                Node::Text(Label::new(
                    "Two-pass blur",
                    [28.0, 28.0, 320.0, 38.0],
                    28.0,
                    ink,
                )),
                rect(28.0, 85.0, 310.0, 30.0, [0.8, 0.45, 0.1, 0.7]),
            ],
        },
        rect(24.0, 510.0, 752.0, 10.0, [0.3, 0.5, 0.4, 1.0]),
    ])
}

#[test]
fn scene_effects_preserve_placement_clips_transparency_and_order() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen(&gpu, &specimen()?, 800, 560)?;
    let path = Path::new("snapshots/m4-effects-scene.png");
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(path, 800, 560, &pixels)?;
    }
    let (w, h, expected) = render::read_png(path)?;
    assert_eq!((w, h), (800, 560));
    if pixels != expected {
        std::fs::create_dir_all("artifacts")?;
        render::write_png(
            Path::new("artifacts/m4-effects-scene-actual.png"),
            800,
            560,
            &pixels,
        )?;
    }
    assert!(
        pixels == expected,
        "Inspect artifacts/m4-effects-scene-actual.png"
    );
    let pixel = |x: usize, y: usize| &pixels[(y * 800 + x) * 4..(y * 800 + x) * 4 + 4];
    assert_eq!(pixel(25, 515), pixel(775, 515));
    assert_eq!(pixel(10, 350), pixel(790, 350));
    assert_ne!(pixel(35, 500), pixel(10, 500));
    Ok(())
}
