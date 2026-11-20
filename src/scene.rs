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
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
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
        fn validate(nodes: &[Node], depth: usize, count: &mut usize) -> anyhow::Result<()> {
            anyhow::ensure!(depth <= 64, "Scene nesting exceeds 64");
            *count += nodes.len();
            anyhow::ensure!(*count <= 4096, "Scene exceeds 4096 nodes");
            for node in nodes {
                match node {
                    Node::Rect { bounds, color } => {
                        anyhow::ensure!(bounds.valid(), "Invalid rectangle geometry");
                        anyhow::ensure!(
                            color
                                .iter()
                                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                            "Invalid color"
                        );
                    }
                    Node::Group { offset, children } => {
                        anyhow::ensure!(
                            offset.iter().all(|v| v.is_finite()),
                            "Invalid translation"
                        );
                        validate(children, depth + 1, count)?;
                    }
                    Node::Clip { bounds, children } => {
                        anyhow::ensure!(bounds.valid(), "Invalid clip geometry");
                        validate(children, depth + 1, count)?;
                    }
                }
            }
            Ok(())
        }
        validate(&roots, 0, &mut 0)?;
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
        fn visit(nodes: &[Node], offset: [f32; 2], clip: Bounds, out: &mut Vec<Quad>) {
            for node in nodes {
                match node {
                    Node::Rect { bounds, color } => {
                        if let Some(bounds) = bounds.translated(offset).intersection(clip) {
                            out.push(Quad {
                                bounds,
                                color: *color,
                            });
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
}
