use crate::{
    animation::{Easing, FrameChange, Prop, Rgba, Seconds, Transition},
    scene::{Bounds, ImageAsset, Node, Scene},
    text::Label,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Rect,
    Text,
    Group,
    Image,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
struct ImageDefinition {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}
impl ImageDefinition {
    fn validate(&self) -> Result<usize> {
        ensure!(
            (1..=256).contains(&self.width) && (1..=256).contains(&self.height),
            "Image dimensions must be 1..256"
        );
        let bytes = self
            .width
            .checked_mul(self.height)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("Image size overflow"))? as usize;
        ensure!(
            self.rgba.len() == bytes,
            "Image RGBA byte length does not match dimensions"
        );
        Ok(bytes)
    }
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Style {
    color: Option<[f32; 4]>,
    offset: Option<[f32; 2]>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
struct Durations {
    color: Option<u32>,
    offset: Option<u32>,
}
fn white() -> [f32; 4] {
    [1.0; 4]
}
fn font_size() -> f32 {
    16.0
}
fn duration() -> u32 {
    150
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Definition {
    key: String,
    kind: Kind,
    #[serde(default)]
    image: Option<ImageDefinition>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    order: i32,
    #[serde(default)]
    bounds: [f32; 4],
    #[serde(default = "white")]
    color: [f32; 4],
    #[serde(default)]
    offset: [f32; 2],
    #[serde(default)]
    text: String,
    #[serde(default = "font_size")]
    font_size: f32,
    #[serde(default)]
    input: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    states: BTreeMap<String, Style>,
    #[serde(default = "duration")]
    transition_ms: u32,
    #[serde(default)]
    transitions: Durations,
}
fn bounded(values: &[f32]) -> bool {
    values
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 1_000_000.0)
}
fn valid_color(color: &[f32; 4]) -> bool {
    color
        .iter()
        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
fn parse(values: &[Value]) -> Result<Vec<Definition>> {
    ensure!(values.len() <= 4096, "Interactive scene exceeds 4096 nodes");
    let nodes: Vec<Definition> = values
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()?;
    let mut keys = HashMap::new();
    let mut image_bytes = 0usize;
    let mut unique_images = HashSet::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.kind == Kind::Image {
            let image = node
                .image
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Image node requires image data"))?;
            unique_images.insert(image);
            ensure!(
                unique_images.len() <= 128,
                "Interactive scene exceeds 128 unique images"
            );
            image_bytes = image_bytes
                .checked_add(image.validate()?)
                .ok_or_else(|| anyhow::anyhow!("Image budget overflow"))?;
            ensure!(
                image_bytes <= 1024 * 1024,
                "Interactive images exceed 1 MiB decoded budget"
            );
            ensure!(
                node.bounds[2] > 0.0 && node.bounds[3] > 0.0,
                "Image display bounds must be positive"
            );
        } else {
            ensure!(
                values[index].get("image").is_none(),
                "Image field requires image kind"
            );
        }
        ensure!(
            !node.key.is_empty() && node.key.len() <= 128,
            "Invalid node key"
        );
        ensure!(
            node.parent
                .as_ref()
                .is_none_or(|parent| !parent.is_empty() && parent.len() <= 128),
            "Invalid parent key"
        );
        ensure!(
            keys.insert(node.key.as_str(), index).is_none(),
            "Duplicate node key"
        );
        ensure!(
            bounded(&node.bounds)
                && node.bounds[2] >= 0.0
                && node.bounds[3] >= 0.0
                && bounded(&node.offset),
            "Invalid node geometry"
        );
        ensure!(valid_color(&node.color), "Invalid node color");
        ensure!(
            node.text.len() <= 4096
                && node.font_size.is_finite()
                && node.font_size > 0.0
                && node.font_size <= 512.0,
            "Invalid text or font size"
        );
        ensure!(
            node.state.len() <= 128 && node.states.len() <= 32,
            "Too many or oversized node states"
        );
        ensure!(
            [
                node.transition_ms,
                node.transitions.color.unwrap_or(0),
                node.transitions.offset.unwrap_or(0)
            ]
            .iter()
            .all(|ms| *ms <= 10000),
            "Transition exceeds 10000 ms"
        );
        for (name, style) in &node.states {
            ensure!(!name.is_empty() && name.len() <= 128, "Invalid state name");
            ensure!(
                style.color.as_ref().is_none_or(valid_color)
                    && style.offset.as_ref().is_none_or(|offset| bounded(offset)),
                "Invalid state style"
            );
        }
    }
    for node in &nodes {
        let mut cursor = node;
        let mut depth = 0;
        while let Some(parent) = &cursor.parent {
            depth += 1;
            ensure!(depth <= 64, "Cyclic hierarchy or depth exceeds 64");
            let index = keys
                .get(parent.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing parent {parent}"))?;
            cursor = &nodes[*index];
            ensure!(cursor.kind == Kind::Group, "Node parent must be a group");
            ensure!(cursor.key != node.key, "Cyclic hierarchy");
        }
    }
    Ok(nodes)
}
pub fn validate_nodes(nodes: &[Value]) -> Result<()> {
    parse(nodes).map(|_| ())
}

#[derive(Clone)]
struct Retained {
    definition: Definition,
    image: Option<ImageAsset>,
    color: Prop<Rgba>,
    x: Prop<f64>,
    y: Prop<f64>,
}
impl Retained {
    fn new(definition: Definition, image: Option<ImageAsset>) -> Result<Self> {
        let (color, offset) = style(&definition, false, false);
        Ok(Self {
            definition,
            image,
            color: Prop::new(Rgba(color.map(f64::from)))?,
            x: Prop::new(f64::from(offset[0]))?,
            y: Prop::new(f64::from(offset[1]))?,
        })
    }
    fn retarget(&mut self, hover: bool, pressed: bool) -> Result<()> {
        let (color, offset) = style(&self.definition, hover, pressed);
        let transition = |ms| -> Result<Transition> {
            Ok(Transition::Tween {
                duration: Seconds::new(f64::from(ms) / 1000.0)?,
                easing: Easing::EaseInOut,
            })
        };
        let target = Rgba(color.map(f64::from));
        if self.color.target() != target {
            self.color.transition_to(
                target,
                transition(
                    self.definition
                        .transitions
                        .color
                        .unwrap_or(self.definition.transition_ms),
                )?,
            )?;
        }
        let transition = transition(
            self.definition
                .transitions
                .offset
                .unwrap_or(self.definition.transition_ms),
        )?;
        if self.x.target() != f64::from(offset[0]) {
            self.x.transition_to(f64::from(offset[0]), transition)?;
        }
        if self.y.target() != f64::from(offset[1]) {
            self.y.transition_to(f64::from(offset[1]), transition)?;
        }
        Ok(())
    }
    fn active(&self) -> bool {
        self.color.is_active() || self.x.is_active() || self.y.is_active()
    }
}
fn style(node: &Definition, hover: bool, pressed: bool) -> ([f32; 4], [f32; 2]) {
    let mut color = node.color;
    let mut offset = node.offset;
    for name in [
        Some(node.state.as_str()),
        hover.then_some("hover"),
        pressed.then_some("pressed"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(style) = node.states.get(name) {
            if let Some(value) = style.color {
                color = value;
            }
            if let Some(value) = style.offset {
                offset = value;
            }
        }
    }
    (color, offset)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Move,
    Press,
    Release,
    Leave,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct InputEvent {
    pub key: String,
    pub event: String,
}
#[derive(Default)]
pub struct InteractiveScene {
    nodes: Vec<Retained>,
    tree: HashMap<String, Vec<usize>>,
    hover: Option<String>,
    pressed: Option<String>,
    dirty: bool,
}
impl InteractiveScene {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn apply(&mut self, nodes: &[Value]) -> Result<bool> {
        let definitions = parse(nodes)?;
        let interactive = |key: &String| definitions.iter().any(|n| n.key == *key && n.input);
        let hover = self.hover.as_ref().filter(|key| interactive(key)).cloned();
        let pressed = self
            .pressed
            .as_ref()
            .filter(|key| interactive(key))
            .cloned();
        let previous: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|n| (n.definition.key.as_str(), n))
            .collect();
        let previous_images: HashMap<_, _> = self
            .nodes
            .iter()
            .filter_map(|node| node.definition.image.as_ref().zip(node.image.as_ref()))
            .collect();
        let mut images: HashMap<ImageDefinition, ImageAsset> = HashMap::new();
        let mut changed = self.nodes.len() != definitions.len();
        let mut retained = Vec::with_capacity(definitions.len());
        for (index, definition) in definitions.into_iter().enumerate() {
            let image = if let Some(data) = &definition.image {
                if let Some(image) = images.get(data) {
                    Some(image.clone())
                } else {
                    let image = if let Some(old) = previous_images.get(data) {
                        (*old).clone()
                    } else {
                        ImageAsset::from_rgba(data.width, data.height, data.rgba.clone())?
                    };
                    images.insert(data.clone(), image.clone());
                    Some(image)
                }
            } else {
                None
            };
            changed |= self
                .nodes
                .get(index)
                .is_none_or(|n| n.definition != definition);
            let mut node = if let Some(old) = previous.get(definition.key.as_str()) {
                let mut node = (*old).clone();
                node.definition = definition;
                node.image = image;
                node
            } else {
                Retained::new(definition, image)?
            };
            node.retarget(
                hover.as_deref() == Some(&node.definition.key),
                pressed.as_deref() == Some(&node.definition.key),
            )?;
            retained.push(node);
        }
        let mut tree: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, node) in retained.iter().enumerate() {
            tree.entry(node.definition.parent.clone().unwrap_or_default())
                .or_default()
                .push(index);
        }
        for children in tree.values_mut() {
            children.sort_by_key(|index| retained[*index].definition.order);
        }
        self.tree = tree;
        self.nodes = retained;
        self.hover = hover;
        self.pressed = pressed;
        self.dirty |= changed;
        Ok(changed)
    }
    pub fn advance(&mut self, delta: Seconds) -> FrameChange {
        let mut changed = false;
        for node in &mut self.nodes {
            changed |= node.color.advance(delta).changed;
            changed |= node.x.advance(delta).changed;
            changed |= node.y.advance(delta).changed;
        }
        self.dirty |= changed;
        FrameChange {
            changed,
            active: self.is_active(),
        }
    }
    pub fn is_active(&self) -> bool {
        self.nodes.iter().any(Retained::active)
    }
    pub fn take_damage(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    fn children(&self, parent: Option<&str>) -> &[usize] {
        self.tree
            .get(parent.unwrap_or_default())
            .map_or(&[], Vec::as_slice)
    }
    pub fn scene(&self) -> Result<Scene> {
        fn collect(
            scene: &InteractiveScene,
            parent: Option<&str>,
            offset: [f32; 2],
            output: &mut Vec<Node>,
        ) -> Result<()> {
            for &index in scene.children(parent) {
                let node = &scene.nodes[index];
                let definition = &node.definition;
                let offset = [
                    offset[0] + node.x.value() as f32,
                    offset[1] + node.y.value() as f32,
                ];
                let [x, y, width, height] = definition.bounds;
                let color = node.color.value().0.map(|v| v as f32);
                match definition.kind {
                    Kind::Rect => output.push(Node::Rect {
                        bounds: Bounds::new(x + offset[0], y + offset[1], width, height),
                        color,
                    }),
                    Kind::Text => output.push(Node::Text(Label::new(
                        &definition.text,
                        [x + offset[0], y + offset[1], width, height],
                        definition.font_size,
                        color.map(|v| (v * 255.0).round() as u8),
                    ))),
                    Kind::Group => collect(scene, Some(&definition.key), offset, output)?,
                    Kind::Image => {
                        let image = node
                            .image
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("Missing retained image"))?;
                        output.push(Node::Image {
                            bounds: Bounds::new(x + offset[0], y + offset[1], width, height),
                            image: image.clone(),
                            source: Bounds::new(
                                0.0,
                                0.0,
                                image.width() as f32,
                                image.height() as f32,
                            ),
                            opacity: color[3],
                        });
                    }
                }
            }
            Ok(())
        }
        let mut output = Vec::new();
        collect(self, None, [0.0; 2], &mut output)?;
        Scene::new(output)
    }
    fn hit(&self, x: f32, y: f32) -> Option<String> {
        fn visit(
            scene: &InteractiveScene,
            parent: Option<&str>,
            offset: [f32; 2],
            point: [f32; 2],
            hit: &mut Option<String>,
        ) {
            for &index in scene.children(parent) {
                let node = &scene.nodes[index];
                let d = &node.definition;
                let offset = [
                    offset[0] + node.x.value() as f32,
                    offset[1] + node.y.value() as f32,
                ];
                let [x, y, w, h] = d.bounds;
                if d.input
                    && point[0] >= offset[0] + x
                    && point[0] < offset[0] + x + w
                    && point[1] >= offset[1] + y
                    && point[1] < offset[1] + y + h
                {
                    *hit = Some(d.key.clone());
                }
                if d.kind == Kind::Group {
                    visit(scene, Some(&d.key), offset, point, hit);
                }
            }
        }
        let mut hit = None;
        if x.is_finite() && y.is_finite() {
            visit(self, None, [0.0; 2], [x, y], &mut hit);
        }
        hit
    }
    pub fn pointer(&mut self, kind: PointerKind, x: f32, y: f32) -> Vec<InputEvent> {
        let hit = if kind == PointerKind::Leave {
            None
        } else {
            self.hit(x, y)
        };
        let mut events = Vec::new();
        let mut emit = |key: String, event: &str| {
            events.push(InputEvent {
                key,
                event: event.into(),
            })
        };
        if self.hover != hit {
            if let Some(key) = self.hover.take() {
                emit(key, "hover_leave");
            }
            if let Some(key) = hit.clone() {
                emit(key, "hover_enter");
            }
            self.hover = hit.clone();
        }
        match kind {
            PointerKind::Press => {
                if self.pressed.is_none() {
                    self.pressed = hit.clone();
                    if let Some(key) = hit {
                        emit(key, "press");
                    }
                }
            }
            PointerKind::Release => {
                if let Some(key) = self.pressed.take() {
                    emit(key.clone(), "release");
                    if hit.as_ref() == Some(&key) {
                        emit(key, "activate");
                    }
                }
            }
            PointerKind::Leave => {
                if let Some(key) = self.pressed.take() {
                    emit(key, "cancel");
                }
            }
            PointerKind::Move => {}
        }
        for node in &mut self.nodes {
            let before = (node.color.value(), node.x.value(), node.y.value());
            // Definitions are validated before entering retained state.
            let result = node.retarget(
                self.hover.as_deref() == Some(&node.definition.key),
                self.pressed.as_deref() == Some(&node.definition.key),
            );
            debug_assert!(result.is_ok());
            self.dirty |= before != (node.color.value(), node.x.value(), node.y.value());
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn button() -> Vec<Value> {
        vec![
            json!({"key":"button","kind":"rect","bounds":[0,0,100,40],"input":true,"color":[0.1,0.2,0.3,1],"state":"off","transition_ms":100,"states":{"on":{"color":[0.3,0.6,0.9,1]},"hover":{"offset":[0,2]},"pressed":{"offset":[0,4]}}}),
        ]
    }
    fn first(scene: &InteractiveScene) -> crate::scene::Quad {
        scene
            .scene()
            .expect("valid scene")
            .quads(Bounds::new(-100.0, -100.0, 1000.0, 1000.0))[0]
    }
    #[test]
    fn local_press_animates_without_producer_and_release_activates() -> Result<()> {
        let mut scene = InteractiveScene::new();
        scene.apply(&button())?;
        scene.take_damage();
        assert!(
            scene
                .pointer(PointerKind::Press, 20.0, 20.0)
                .iter()
                .any(|e| e.event == "press")
        );
        assert!(scene.is_active());
        assert_eq!(first(&scene).bounds.y, 0.0);
        scene.advance(Seconds::new(0.05)?);
        assert_eq!(first(&scene).bounds.y, 2.0);
        assert!(scene.take_damage());
        scene.advance(Seconds::new(0.05)?);
        assert_eq!(first(&scene).bounds.y, 4.0);
        assert!(
            scene
                .pointer(PointerKind::Release, 20.0, 20.0)
                .iter()
                .any(|e| e.event == "activate")
        );
        scene.advance(Seconds::new(0.1)?);
        assert_eq!(first(&scene).bounds.y, 2.0);
        Ok(())
    }
    #[test]
    fn equal_targets_and_label_only_changes_preserve_progress() -> Result<()> {
        let mut scene = InteractiveScene::new();
        let mut nodes = button();
        nodes.push(json!({"key":"label","kind":"text","bounds":[8,8,80,24],"text":"Off"}));
        scene.apply(&nodes)?;
        nodes[0]["state"] = json!("on");
        scene.apply(&nodes)?;
        scene.advance(Seconds::new(0.05)?);
        let middle = first(&scene).color;
        nodes[1]["text"] = json!("On");
        scene.apply(&nodes)?;
        assert_eq!(first(&scene).color, middle);
        scene.advance(Seconds::new(0.05)?);
        assert!(!scene.is_active());
        assert_eq!(first(&scene).color, [0.3, 0.6, 0.9, 1.0]);
        Ok(())
    }
    #[test]
    fn hit_testing_uses_animated_ancestors_and_painter_order() -> Result<()> {
        let mut nodes = button();
        nodes[0]["parent"] = json!("outer");
        nodes.push(json!({"key":"outer","kind":"group","offset":[50,60]}));
        nodes.push(
            json!({"key":"front","kind":"rect","bounds":[60,70,20,20],"input":true,"order":1}),
        );
        let mut scene = InteractiveScene::new();
        scene.apply(&nodes)?;
        assert_eq!(scene.hit(55.0, 65.0).as_deref(), Some("button"));
        assert_eq!(scene.hit(65.0, 75.0).as_deref(), Some("front"));
        assert_eq!(scene.hit(5.0, 5.0), None);
        nodes[1]["offset"] = json!([150, 60]);
        nodes[1]["transition_ms"] = json!(100);
        scene.apply(&nodes)?;
        scene.advance(Seconds::new(0.05)?);
        assert_eq!(scene.hit(105.0, 65.0).as_deref(), Some("button"));
        assert_eq!(scene.hit(55.0, 65.0), None);
        Ok(())
    }
    #[test]
    fn removed_target_and_pointer_leave_cancel_activation() -> Result<()> {
        let mut scene = InteractiveScene::new();
        scene.apply(&button())?;
        scene.pointer(PointerKind::Press, 20.0, 20.0);
        scene.apply(&[])?;
        scene.apply(&button())?;
        assert!(
            !scene
                .pointer(PointerKind::Release, 20.0, 20.0)
                .iter()
                .any(|e| e.event == "activate")
        );
        scene.pointer(PointerKind::Press, 20.0, 20.0);
        assert!(
            scene
                .pointer(PointerKind::Leave, 0.0, 0.0)
                .iter()
                .any(|e| e.event == "cancel")
        );
        assert!(
            !scene
                .pointer(PointerKind::Release, 20.0, 20.0)
                .iter()
                .any(|e| e.event == "activate")
        );
        Ok(())
    }
    #[test]
    fn hierarchy_validation_is_atomic_and_optional_extensions_are_ignored() -> Result<()> {
        let mut scene = InteractiveScene::new();
        let nodes = button();
        scene.apply(&nodes)?;
        scene.take_damage();
        let before = first(&scene);
        let mut bad = nodes.clone();
        bad.push(bad[0].clone());
        assert!(scene.apply(&bad).is_err());
        bad = nodes.clone();
        bad[0]["parent"] = json!("missing");
        assert!(scene.apply(&bad).is_err());
        bad = vec![
            json!({"key":"a","kind":"group","parent":"b"}),
            json!({"key":"b","kind":"group","parent":"a"}),
        ];
        assert!(scene.apply(&bad).is_err());
        bad = nodes.clone();
        bad[0]["bounds"] = json!([0, 0, -1, 2]);
        assert!(scene.apply(&bad).is_err());
        assert_eq!(first(&scene), before);
        assert!(!scene.take_damage());
        bad = nodes;
        bad[0]["future_hint"] = json!(true);
        assert!(!scene.apply(&bad)?);
        Ok(())
    }
    #[test]
    fn property_durations_are_independent() -> Result<()> {
        let mut nodes = button();
        nodes[0]["transitions"] = json!({"color":200,"offset":50});
        let mut scene = InteractiveScene::new();
        scene.apply(&nodes)?;
        nodes[0]["state"] = json!("on");
        scene.apply(&nodes)?;
        scene.pointer(PointerKind::Press, 20.0, 20.0);
        scene.advance(Seconds::new(0.05)?);
        assert_eq!(first(&scene).bounds.y, 4.0);
        assert_ne!(first(&scene).color, [0.3, 0.6, 0.9, 1.0]);
        assert!(scene.is_active());
        scene.advance(Seconds::new(0.15)?);
        assert!(!scene.is_active());
        Ok(())
    }
    fn image_node() -> Value {
        json!({"key":"icon","kind":"image","bounds":[0,0,16,16],"image":{"width":1,"height":1,"rgba":[255,0,0,128]}})
    }
    #[test]
    fn images_validate_atomically_and_reuse_native_assets() -> Result<()> {
        let mut scene = InteractiveScene::new();
        let mut node = image_node();
        scene.apply(&[node.clone()])?;
        scene.take_damage();
        let original = scene.nodes[0].image.as_ref().expect("image").id();
        assert!(!scene.apply(&[node.clone()])?);
        node["offset"] = json!([4, 8]);
        scene.apply(&[node.clone()])?;
        assert_eq!(original, scene.nodes[0].image.as_ref().expect("image").id());
        let mut other = node.clone();
        other["key"] = json!("copy");
        scene.apply(&[node.clone(), other])?;
        assert_eq!(original, scene.nodes[1].image.as_ref().expect("image").id());
        scene.take_damage();
        for (field, value) in [
            ("width", json!(0)),
            ("width", json!(257)),
            ("width", json!(u32::MAX)),
            ("rgba", json!([255, 0, 0])),
            ("rgba", json!([256, 0, 0, 255])),
        ] {
            let mut bad = node.clone();
            bad["image"][field] = value;
            assert!(scene.apply(&[bad]).is_err());
            assert_eq!(scene.nodes.len(), 2);
            assert_eq!(original, scene.nodes[0].image.as_ref().expect("image").id());
            assert!(!scene.take_damage());
        }
        for bad in [
            json!({"key":"x","kind":"image","bounds":[0,0,2,2]}),
            json!({"key":"x","kind":"rect","image":null}),
            json!({"key":"x","kind":"text","image":{"width":1,"height":1,"rgba":[0,0,0,0]}}),
        ] {
            assert!(scene.apply(&[bad]).is_err());
        }
        node["image"]["future"] = json!(true);
        validate_nodes(&[node])?;
        Ok(())
    }
    #[test]
    fn decoded_images_have_an_aggregate_budget() {
        let pixels = vec![0u8; 256 * 256 * 4];
        let nodes:Vec<_>=(0..5).map(|i|json!({"key":format!("image-{i}"),"kind":"image","bounds":[0,0,1,1],"image":{"width":256,"height":256,"rgba":pixels}})).collect();
        assert!(validate_nodes(&nodes[..4]).is_ok());
        assert!(validate_nodes(&nodes).is_err());
    }
    #[test]
    fn unique_image_limit_matches_native_scene_budget() -> Result<()> {
        let mut nodes:Vec<_>=(0..129).map(|i|json!({"key":format!("icon-{i}"),"kind":"image","bounds":[0,0,1,1],"image":{"width":1,"height":1,"rgba":[i,0,0,255]}})).collect();
        assert!(validate_nodes(&nodes).is_err());
        let mut scene = InteractiveScene::new();
        scene.apply(&nodes[..128])?;
        scene.scene()?;
        assert!(scene.apply(&nodes).is_err());
        assert_eq!(scene.nodes.len(), 128);
        for node in &mut nodes {
            node["image"]["rgba"] = json!([0, 0, 0, 255]);
        }
        scene.apply(&nodes)?;
        scene.scene()?;
        assert_eq!(scene.nodes.len(), 129);
        Ok(())
    }
}
