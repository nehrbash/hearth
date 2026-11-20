use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT_IMAGE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct ImageAsset(Arc<ImagePixels>);

#[derive(Debug)]
struct ImagePixels {
    id: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl PartialEq for ImageAsset {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}
impl ImageAsset {
    pub fn from_png(bytes: &[u8]) -> anyhow::Result<Self> {
        anyhow::ensure!(bytes.len() <= 16 * 1024 * 1024, "Image PNG exceeds 16 MiB");
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
        let info = reader.info();
        anyhow::ensure!(
            info.width > 0
                && info.height > 0
                && info.width <= 4096
                && info.height <= 4096
                && u64::from(info.width) * u64::from(info.height) <= 4 * 1024 * 1024,
            "Image exceeds pixel budget"
        );
        anyhow::ensure!(
            info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
            "Image must be RGBA8"
        );
        let mut rgba = vec![
            0;
            reader
                .output_buffer_size()
                .ok_or_else(|| anyhow::anyhow!("Image exceeds decoder limits"))?
        ];
        let frame = reader.next_frame(&mut rgba)?;
        rgba.truncate(frame.buffer_size());
        Ok(Self(Arc::new(ImagePixels {
            id: NEXT_IMAGE.fetch_add(1, Ordering::Relaxed),
            width: frame.width,
            height: frame.height,
            rgba,
        })))
    }
    pub fn width(&self) -> u32 {
        self.0.width
    }
    pub fn height(&self) -> u32 {
        self.0.height
    }
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }
    pub(crate) fn rgba(&self) -> &[u8] {
        &self.0.rgba
    }
}

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
pub enum ShaderKind {
    Moon,
    Joined,
    Custom(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LayerEffect {
    None,
    Blur { radius: f32 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Path(crate::path::PathData),
    Image {
        bounds: Bounds,
        image: ImageAsset,
        source: Bounds,
        opacity: f32,
    },
    Shader {
        bounds: Bounds,
        kind: ShaderKind,
        time: f32,
        parameters: [f32; 4],
    },
    Layer {
        bounds: Bounds,
        children: Vec<Node>,
        effect: LayerEffect,
    },

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
    Path {
        path: crate::path::PathData,
        offset: [f32; 2],
        clip: Bounds,
    },
    Image {
        bounds: Bounds,
        clip: Bounds,
        image: ImageAsset,
        source: Bounds,
        opacity: f32,
    },
    Surface {
        bounds: Bounds,
        clip: Bounds,
        node: Node,
    },
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
            pixels: &mut (u64, u32),
        ) -> anyhow::Result<()> {
            anyhow::ensure!(depth <= 64, "Scene nesting exceeds 64");
            *count += nodes.len();
            anyhow::ensure!(*count <= 4096, "Scene exceeds 4096 nodes");
            for node in nodes {
                match node {
                    Node::Path(path) => {
                        anyhow::ensure!(
                            path.bounds().translated(offset).valid(),
                            "Invalid translated path bounds"
                        );
                    }
                    Node::Image {
                        bounds,
                        image,
                        source,
                        opacity,
                    } => {
                        anyhow::ensure!(
                            bounds.valid()
                                && bounds.translated(offset).valid()
                                && bounds.width > 0.0
                                && bounds.height > 0.0,
                            "Invalid image bounds"
                        );
                        anyhow::ensure!(
                            source.valid()
                                && source.x >= 0.0
                                && source.y >= 0.0
                                && source.width > 0.0
                                && source.height > 0.0
                                && source.x + source.width <= image.width() as f32
                                && source.y + source.height <= image.height() as f32,
                            "Image crop exceeds source"
                        );
                        anyhow::ensure!(
                            opacity.is_finite() && (0.0..=1.0).contains(opacity),
                            "Invalid image opacity"
                        );
                    }
                    Node::Shader {
                        bounds,
                        kind,
                        time,
                        parameters,
                    } => {
                        validate_surface(*bounds, offset, pixels)?;
                        anyhow::ensure!(
                            time.is_finite()
                                && *time >= 0.0
                                && parameters.iter().all(|v| v.is_finite()),
                            "Invalid shader uniforms"
                        );
                        if let ShaderKind::Custom(source) = kind {
                            anyhow::ensure!(source.len() <= 65536, "Shader exceeds 64 KiB");
                        }
                    }
                    Node::Layer {
                        bounds,
                        children,
                        effect,
                    } => {
                        validate_surface(*bounds, offset, pixels)?;
                        if let LayerEffect::Blur { radius } = effect {
                            anyhow::ensure!(
                                radius.is_finite() && (0.0..=64.0).contains(radius),
                                "Invalid blur radius"
                            );
                        }
                        validate(children, depth + 1, count, [0.0; 2], pixels)?;
                    }
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
                        validate(children, depth + 1, count, translated, pixels)?;
                    }
                    Node::Clip { bounds, children } => {
                        anyhow::ensure!(
                            bounds.valid() && bounds.translated(offset).valid(),
                            "Invalid clip geometry"
                        );
                        validate(children, depth + 1, count, offset, pixels)?;
                    }
                }
            }
            Ok(())
        }
        validate(&roots, 0, &mut 0, [0.0; 2], &mut (0, 0))?;
        fn path_budget(nodes: &[Node], totals: &mut (usize, usize)) -> anyhow::Result<()> {
            for node in nodes {
                match node {
                    Node::Path(path) => {
                        totals.0 += path.vertices().len();
                        totals.1 += path.indices().len();
                        anyhow::ensure!(
                            totals.0 <= 1_048_576 && totals.1 <= 6_291_456,
                            "Scene exceeds path mesh budget"
                        );
                    }
                    Node::Group { children, .. }
                    | Node::Clip { children, .. }
                    | Node::Layer { children, .. } => path_budget(children, totals)?,
                    _ => {}
                }
            }
            Ok(())
        }
        path_budget(&roots, &mut (0, 0))?;
        fn images(nodes: &[Node], seen: &mut std::collections::BTreeMap<u64, u64>) {
            for node in nodes {
                match node {
                    Node::Image { image, .. } => {
                        seen.insert(
                            image.id(),
                            u64::from(image.width()) * u64::from(image.height()),
                        );
                    }
                    Node::Group { children, .. }
                    | Node::Clip { children, .. }
                    | Node::Layer { children, .. } => images(children, seen),
                    _ => {}
                }
            }
        }
        let mut assets = std::collections::BTreeMap::new();
        images(&roots, &mut assets);
        anyhow::ensure!(
            assets.len() <= 128 && assets.values().sum::<u64>() <= 16 * 1024 * 1024,
            "Scene exceeds image asset budget"
        );
        let changed = self.roots != roots;
        if changed {
            self.roots = roots;
            self.dirty = true;
        }
        Ok(changed)
    }

    pub(crate) fn image_ids(&self) -> std::collections::BTreeSet<u64> {
        fn visit(nodes: &[Node], ids: &mut std::collections::BTreeSet<u64>) {
            for node in nodes {
                match node {
                    Node::Image { image, .. } => {
                        ids.insert(image.id());
                    }
                    Node::Group { children, .. }
                    | Node::Clip { children, .. }
                    | Node::Layer { children, .. } => visit(children, ids),
                    _ => {}
                }
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        visit(&self.roots, &mut ids);
        ids
    }

    pub fn take_damage(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn quads(&self, viewport: Bounds) -> Vec<Quad> {
        self.paint_ops(viewport)
            .into_iter()
            .filter_map(|op| match op {
                PaintOp::Rect(quad) => Some(quad),
                _ => None,
            })
            .collect()
    }

    pub fn paint_ops(&self, viewport: Bounds) -> Vec<PaintOp> {
        fn visit(nodes: &[Node], offset: [f32; 2], clip: Bounds, out: &mut Vec<PaintOp>) {
            for node in nodes {
                match node {
                    Node::Path(path) => {
                        if path
                            .bounds()
                            .translated(offset)
                            .intersection(clip)
                            .is_some()
                        {
                            out.push(PaintOp::Path {
                                path: path.clone(),
                                offset,
                                clip,
                            });
                        }
                    }
                    Node::Image {
                        bounds,
                        image,
                        source,
                        opacity,
                    } => {
                        let bounds = bounds.translated(offset);
                        if let Some(clip) = bounds.intersection(clip) {
                            out.push(PaintOp::Image {
                                bounds,
                                clip,
                                image: image.clone(),
                                source: *source,
                                opacity: *opacity,
                            });
                        }
                    }
                    Node::Shader { bounds, .. } | Node::Layer { bounds, .. } => {
                        let bounds = bounds.translated(offset);
                        if let Some(clip) = bounds.intersection(clip) {
                            out.push(PaintOp::Surface {
                                bounds,
                                clip,
                                node: node.clone(),
                            });
                        }
                    }
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

fn validate_surface(
    bounds: Bounds,
    offset: [f32; 2],
    pixels: &mut (u64, u32),
) -> anyhow::Result<()> {
    anyhow::ensure!(
        bounds.valid()
            && bounds.translated(offset).valid()
            && bounds.width > 0.0
            && bounds.height > 0.0
            && bounds.width <= 4096.0
            && bounds.height <= 4096.0,
        "Invalid layer geometry"
    );
    pixels.0 += bounds.width.ceil() as u64 * bounds.height.ceil() as u64 * 3;
    pixels.1 += 1;
    anyhow::ensure!(
        pixels.0 <= 16 * 1024 * 1024 && pixels.1 <= 128,
        "Scene exceeds offscreen pixel or node budget"
    );
    Ok(())
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
    fn image_validation_is_atomic_and_clones_share_identity() -> anyhow::Result<()> {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()?
                .write_image_data(&[255, 0, 0, 128, 0, 0, 0, 0])?;
        }
        let image = ImageAsset::from_png(&png)?;
        assert_eq!(image, image.clone());
        assert!(ImageAsset::from_png(b"not a PNG").is_err());
        let valid = Node::Image {
            bounds: Bounds::new(0.0, 0.0, 20.0, 10.0),
            image: image.clone(),
            source: Bounds::new(0.0, 0.0, 2.0, 1.0),
            opacity: 1.0,
        };
        let mut scene = Scene::new(vec![valid.clone()])?;
        scene.take_damage();
        let before = scene.paint_ops(Bounds::new(0.0, 0.0, 100.0, 100.0));
        let mut invalid = valid.clone();
        if let Node::Image { source, .. } = &mut invalid {
            source.width = 3.0;
        }
        assert!(scene.replace(vec![invalid]).is_err());
        let mut invalid = valid.clone();
        if let Node::Image { opacity, .. } = &mut invalid {
            *opacity = f32::NAN;
        }
        assert!(scene.replace(vec![invalid]).is_err());
        assert_eq!(before, scene.paint_ops(Bounds::new(0.0, 0.0, 100.0, 100.0)));
        assert!(!scene.take_damage());
        assert!(!scene.replace(vec![valid])?);
        Ok(())
    }

    #[test]
    fn offscreen_budgets_and_uniforms_are_validated_atomically() -> anyhow::Result<()> {
        let mut scene = Scene::new(vec![rect(0.0, 0.0, 10.0, 10.0, [1.0; 4])])?;
        scene.take_damage();
        let layer = |width, height| Node::Layer {
            bounds: Bounds::new(0.0, 0.0, width, height),
            children: vec![],
            effect: LayerEffect::None,
        };
        assert!(scene.replace(vec![layer(4097.0, 1.0)]).is_err());
        assert!(scene.replace(vec![layer(4096.0, 4096.0)]).is_err());
        assert!(scene.replace(vec![layer(1.0, 1.0); 129]).is_err());
        assert!(
            scene
                .replace(vec![Node::Shader {
                    bounds: Bounds::new(0.0, 0.0, 1.0, 1.0),
                    kind: ShaderKind::Joined,
                    time: 0.0,
                    parameters: [f32::NAN; 4],
                }])
                .is_err()
        );
        assert_eq!(scene.quads(Bounds::new(0.0, 0.0, 20.0, 20.0)).len(), 1);
        assert!(!scene.take_damage());
        Ok(())
    }

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
