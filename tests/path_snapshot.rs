use anyhow::Result;
use hearth::{
    gpu::Gpu,
    path::{Command::*, Fill, FillRule, LineCap, LineJoin, PathData, Stroke},
    render,
    scene::{Bounds, LayerEffect, Node, Scene, rect},
};
use std::path::Path;

fn leaf() -> Result<PathData> {
    PathData::new(
        &[
            Move([0.0, 80.0]),
            Cubic([0.0, 0.0], [120.0, 0.0], [140.0, 0.0]),
            Quad([130.0, 100.0], [0.0, 80.0]),
            Close,
        ],
        Some(Fill {
            color: [0.12, 0.58, 0.35, 1.0],
            rule: FillRule::NonZero,
        }),
        Some(Stroke {
            color: [0.7, 0.9, 0.5, 1.0],
            width: 3.0,
            cap: LineCap::Round,
            join: LineJoin::Round,
        }),
    )
}
fn ring(rule: FillRule) -> Result<PathData> {
    PathData::new(
        &[
            Move([0.0, 0.0]),
            Line([100.0, 0.0]),
            Line([100.0, 100.0]),
            Line([0.0, 100.0]),
            Close,
            Move([25.0, 25.0]),
            Line([75.0, 25.0]),
            Line([75.0, 75.0]),
            Line([25.0, 75.0]),
            Close,
        ],
        Some(Fill {
            color: [0.65, 0.3, 0.08, 1.0],
            rule,
        }),
        None,
    )
}
fn specimen() -> Result<Scene> {
    Scene::new(vec![
        Node::Group {
            offset: [30.0, 30.0],
            children: vec![Node::Path(leaf()?)],
        },
        Node::Group {
            offset: [230.0, 20.0],
            children: vec![Node::Clip {
                bounds: Bounds::new(15.0, 15.0, 90.0, 55.0),
                children: vec![Node::Path(leaf()?)],
            }],
        },
        Node::Group {
            offset: [30.0, 150.0],
            children: vec![Node::Path(ring(FillRule::EvenOdd)?)],
        },
        Node::Group {
            offset: [160.0, 150.0],
            children: vec![Node::Path(ring(FillRule::NonZero)?)],
        },
        Node::Layer {
            bounds: Bounds::new(290.0, 150.0, 170.0, 110.0),
            children: vec![Node::Group {
                offset: [10.0, 10.0],
                children: vec![Node::Path(leaf()?)],
            }],
            effect: LayerEffect::None,
        },
        Node::Path(PathData::new(
            &[
                Move([35.0, 310.0]),
                Cubic([150.0, 235.0], [220.0, 385.0], [330.0, 310.0]),
                Line([440.0, 290.0]),
            ],
            None,
            Some(Stroke {
                color: [0.4, 0.7, 0.95, 1.0],
                width: 9.0,
                cap: LineCap::Round,
                join: LineJoin::Bevel,
            }),
        )?),
        rect(230.0, 290.0, 20.0, 40.0, [0.9, 0.1, 0.15, 1.0]),
    ])
}
#[test]
fn paths_fill_stroke_curves_holes_clip_and_layer_match_png() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen(&gpu, &specimen()?, 480, 360)?;
    let sample = |x: usize, y: usize| &pixels[(y * 480 + x) * 4..(y * 480 + x) * 4 + 4];
    assert_eq!(sample(80, 200), sample(470, 350));
    assert_ne!(sample(210, 200), sample(470, 350));
    assert_eq!(sample(235, 50), sample(470, 350));
    assert_ne!(sample(285, 60), sample(470, 350));
    assert_ne!(sample(350, 190), sample(470, 350));
    assert!(sample(240, 310)[0] > sample(240, 310)[2]);
    let path = Path::new("snapshots/m3-paths.png");
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(path, 480, 360, &pixels)?;
    }
    let (w, h, expected) = render::read_png(path)?;
    assert_eq!((w, h), (480, 360));
    assert!(
        pixels == expected,
        "Path PNG differs; regenerate and inspect at 1x"
    );
    Ok(())
}
#[test]
fn transformed_path_replacement_is_atomic() -> Result<()> {
    let path = leaf()?;
    let mut scene = Scene::new(vec![Node::Path(path.clone())])?;
    scene.take_damage();
    assert!(
        scene
            .replace(vec![Node::Group {
                offset: [f32::INFINITY, 0.0],
                children: vec![Node::Path(path)]
            }])
            .is_err()
    );
    assert!(!scene.take_damage());
    assert_eq!(
        scene.paint_ops(Bounds::new(0.0, 0.0, 480.0, 360.0)).len(),
        1
    );
    Ok(())
}
