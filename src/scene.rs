use crate::text::Label;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Bounds {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then(|| Self::new(x, y, right - x, bottom - y))
    }

    fn translated(self, offset: [f32; 2]) -> Self {
        Self {
            x: self.x + offset[0],
            y: self.y + offset[1],
            ..self
        }
    }

    fn valid(self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
        ]
        .iter()
        .all(|v| v.is_finite())
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(Label),
    Rect {
        bounds: Bounds,
        color: [f32; 4],
    },
    Group {
        offset: [f32; 2],
        children: Vec<Node>,
    },
    Clip {
        bounds: Bounds,
        children: Vec<Node>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub bounds: Bounds,
    pub color: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaintOp {
    Rect(Quad),
    Text(Label),
}

#[derive(Default)]
pub struct Scene {
    roots: Vec<Node>,
    dirty: bool,
}

impl Scene {
    pub fn new(roots: Vec<Node>) -> anyhow::Result<Self> {
        let mut scene = Self::default();
        scene.replace(roots)?;
        scene.dirty = true;
        Ok(scene)
    }

    pub fn replace(&mut self, roots: Vec<Node>) -> anyhow::Result<bool> {
        fn validate(
            nodes: &[Node],
            depth: usize,
            count: &mut usize,
            offset: [f32; 2],
        ) -> anyhow::Result<()> {
            anyhow::ensure!(depth <= 64, "Scene nesting exceeds 64");
            *count += nodes.len();
            anyhow::ensure!(*count <= 4096, "Scene exceeds 4096 nodes");
            for node in nodes {
                match node {
                    Node::Text(label) => {
                        label.validate()?;
                        translated_label(label, offset).validate()?;
                    }
                    Node::Rect { bounds, color } => {
                        anyhow::ensure!(
                            bounds.valid() && bounds.translated(offset).valid(),
                            "Invalid rectangle geometry"
                        );
                        anyhow::ensure!(
                            color
                                .iter()
                                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                            "Invalid color"
                        );
                    }
                    Node::Group {
                        offset: local,
                        children,
                    } => {
                        let translated = [offset[0] + local[0], offset[1] + local[1]];
                        anyhow::ensure!(
                            local.iter().chain(&translated).all(|v| v.is_finite()),
                            "Invalid translation"
                        );
                        validate(children, depth + 1, count, translated)?;
                    }
                    Node::Clip { bounds, children } => {
                        anyhow::ensure!(
                            bounds.valid() && bounds.translated(offset).valid(),
                            "Invalid clip geometry"
                        );
                        validate(children, depth + 1, count, offset)?;
                    }
                }
            }
            Ok(())
        }
        validate(&roots, 0, &mut 0, [0.0; 2])?;
        let changed = self.roots != roots;
        if changed {
            self.roots = roots;
            self.dirty = true;
        }
        Ok(changed)
    }

    pub fn take_damage(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn quads(&self, viewport: Bounds) -> Vec<Quad> {
        self.paint_ops(viewport)
            .into_iter()
            .filter_map(|op| match op {
                PaintOp::Rect(quad) => Some(quad),
                PaintOp::Text(_) => None,
            })
            .collect()
    }

    pub fn paint_ops(&self, viewport: Bounds) -> Vec<PaintOp> {
        fn visit(nodes: &[Node], offset: [f32; 2], clip: Bounds, out: &mut Vec<PaintOp>) {
            for node in nodes {
                match node {
                    Node::Text(label) => {
                        let mut label = translated_label(label, offset);
                        let [x, y, width, height] =
                            label
                                .clip
                                .unwrap_or([clip.x, clip.y, clip.width, clip.height]);
                        if let Some(visible) = Bounds::new(x, y, width, height).intersection(clip) {
                            let [x, y, width, height] = label.bounds;
                            if Bounds::new(x, y, width, height)
                                .intersection(visible)
                                .is_some()
                            {
                                label.clip =
                                    Some([visible.x, visible.y, visible.width, visible.height]);
                                out.push(PaintOp::Text(label));
                            }
                        }
                    }
                    Node::Rect { bounds, color } => {
                        if let Some(bounds) = bounds.translated(offset).intersection(clip) {
                            out.push(PaintOp::Rect(Quad {
                                bounds,
                                color: *color,
                            }));
                        }
                    }
                    Node::Group {
                        offset: local,
                        children,
                    } => {
                        visit(
                            children,
                            [offset[0] + local[0], offset[1] + local[1]],
                            clip,
                            out,
                        );
                    }
                    Node::Clip { bounds, children } => {
                        if let Some(clip) = bounds.translated(offset).intersection(clip) {
                            visit(children, offset, clip, out);
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.roots, [0.0; 2], viewport, &mut out);
        out
    }
}

fn translated_label(label: &Label, offset: [f32; 2]) -> Label {
    let mut label = label.clone();
    label.bounds[0] += offset[0];
    label.bounds[1] += offset[1];
    if let Some(clip) = &mut label.clip {
        clip[0] += offset[0];
        clip[1] += offset[1];
    }
    label
}

pub fn rect(x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) -> Node {
    Node::Rect {
        bounds: Bounds::new(x, y, width, height),
        color,
    }
}

pub fn demo() -> anyhow::Result<Scene> {
    Scene::new(vec![
        rect(24.0, 24.0, 592.0, 352.0, [0.07, 0.09, 0.14, 1.0]),
        rect(24.0, 24.0, 6.0, 352.0, [0.22, 0.72, 0.64, 1.0]),
        Node::Group {
            offset: [52.0, 56.0],
            children: vec![
                rect(0.0, 0.0, 224.0, 20.0, [0.65, 0.74, 0.87, 1.0]),
                rect(0.0, 36.0, 160.0, 8.0, [0.25, 0.32, 0.44, 1.0]),
                rect(0.0, 88.0, 248.0, 168.0, [0.12, 0.17, 0.25, 1.0]),
                Node::Clip {
                    bounds: Bounds::new(280.0, 88.0, 248.0, 168.0),
                    children: vec![
                        rect(250.0, 60.0, 320.0, 240.0, [0.18, 0.25, 0.36, 1.0]),
                        rect(304.0, 112.0, 200.0, 120.0, [0.34, 0.50, 0.85, 0.7]),
                    ],
                },
            ],
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_layout_origin_and_intersects_translated_clips_in_paint_order()
    -> anyhow::Result<()> {
        let label = Label::new("nested", [1.0, 2.0, 100.0, 40.0], 16.0, [255; 4])
            .with_clip([2.0, 3.0, 30.0, 20.0]);
        let scene = Scene::new(vec![Node::Group {
            offset: [10.0, 20.0],
            children: vec![Node::Clip {
                bounds: Bounds::new(0.0, 0.0, 20.0, 20.0),
                children: vec![Node::Group {
                    offset: [3.0, 4.0],
                    children: vec![
                        rect(0.0, 0.0, 20.0, 20.0, [1.0; 4]),
                        Node::Text(label),
                        rect(1.0, 1.0, 2.0, 2.0, [0.0; 4]),
                    ],
                }],
            }],
        }])?;
        let operations = scene.paint_ops(Bounds::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(operations.len(), 3);
        assert!(matches!(operations[0], PaintOp::Rect(_)));
        let PaintOp::Text(label) = &operations[1] else {
            panic!("Text must remain between rectangles")
        };
        assert_eq!(label.bounds, [14.0, 26.0, 100.0, 40.0]);
        assert_eq!(label.clip, Some([15.0, 27.0, 15.0, 13.0]));
        assert!(matches!(operations[2], PaintOp::Rect(_)));
        assert!(
            scene
                .paint_ops(Bounds::new(80.0, 80.0, 10.0, 10.0))
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn invalid_text_update_preserves_previous_scene_and_damage() -> anyhow::Result<()> {
        let nodes = vec![Node::Text(Label::new(
            "kept",
            [0.0, 0.0, 100.0, 30.0],
            16.0,
            [255; 4],
        ))];
        let mut scene = Scene::new(nodes.clone())?;
        scene.take_damage();
        assert!(!scene.replace(nodes)?);
        let before = scene.paint_ops(Bounds::new(0.0, 0.0, 200.0, 200.0));
        assert!(
            scene
                .replace(vec![Node::Text(Label::new(
                    "invalid",
                    [0.0; 4],
                    f32::NAN,
                    [255; 4]
                ))])
                .is_err()
        );
        assert!(
            scene
                .replace(vec![Node::Group {
                    offset: [f32::MAX, 0.0],
                    children: vec![Node::Text(Label::new(
                        "overflow",
                        [f32::MAX, 0.0, 10.0, 10.0],
                        16.0,
                        [255; 4]
                    ))],
                }])
                .is_err()
        );
        assert_eq!(scene.paint_ops(Bounds::new(0.0, 0.0, 200.0, 200.0)), before);
        assert!(!scene.take_damage());
        Ok(())
    }

    #[test]
    fn nested_clip_and_translation_preserve_paint_order() -> anyhow::Result<()> {
        let scene = Scene::new(vec![Node::Group {
            offset: [10.0, 5.0],
            children: vec![Node::Clip {
                bounds: Bounds::new(0.0, 0.0, 10.0, 10.0),
                children: vec![
                    rect(-4.0, 3.0, 20.0, 20.0, [1.0; 4]),
                    rect(2.0, 2.0, 1.0, 1.0, [0.0; 4]),
                ],
            }],
        }])?;
        let quads = scene.quads(Bounds::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(quads[0].bounds, Bounds::new(10.0, 8.0, 10.0, 7.0));
        assert_eq!(quads[1].bounds, Bounds::new(12.0, 7.0, 1.0, 1.0));
        Ok(())
    }

    #[test]
    fn invalid_update_is_atomic_and_identical_update_stays_idle() -> anyhow::Result<()> {
        let nodes = vec![rect(0.0, 0.0, 4.0, 4.0, [1.0; 4])];
        let mut scene = Scene::new(nodes.clone())?;
        assert!(scene.take_damage());
        assert!(!scene.replace(nodes)?);
        assert!(!scene.take_damage());
        assert!(
            scene
                .replace(vec![rect(f32::NAN, 0.0, 1.0, 1.0, [1.0; 4])])
                .is_err()
        );
        assert_eq!(scene.quads(Bounds::new(0.0, 0.0, 10.0, 10.0)).len(), 1);
        assert!(!scene.take_damage());
        Ok(())
    }

    #[test]
    fn overflowing_accumulated_geometry_is_rejected_atomically() -> anyhow::Result<()> {
        let mut scene = Scene::new(vec![rect(0.0, 0.0, 10.0, 10.0, [1.0; 4])])?;
        scene.take_damage();
        let original = scene.quads(Bounds::new(0.0, 0.0, 100.0, 100.0));
        let mut nested = rect(0.0, 0.0, 10.0, 10.0, [1.0; 4]);
        for offset in [-f32::MAX, -f32::MAX, f32::MAX, f32::MAX] {
            nested = Node::Group {
                offset: [offset, 0.0],
                children: vec![nested],
            };
        }
        assert!(scene.replace(vec![nested]).is_err());
        assert_eq!(scene.quads(Bounds::new(0.0, 0.0, 100.0, 100.0)), original);
        assert!(!scene.take_damage());
        assert!(
            scene
                .replace(vec![rect(f32::MAX, 0.0, f32::MAX, 10.0, [1.0; 4])])
                .is_err()
        );
        assert!(
            scene
                .replace(vec![Node::Group {
                    offset: [f32::MAX, 0.0],
                    children: vec![Node::Clip {
                        bounds: Bounds::new(f32::MAX, 0.0, 10.0, 10.0),
                        children: vec![],
                    }],
                }])
                .is_err()
        );
        assert_eq!(scene.quads(Bounds::new(0.0, 0.0, 100.0, 100.0)), original);
        Ok(())
    }
}
