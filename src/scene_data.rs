use crate::scene::{Bounds, Node, Scene};
use crate::text::{Label, Overflow};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const MAX_SCENE_NODES: usize = 4096;
pub const MAX_SCENE_DEPTH: usize = 64;
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_FAMILY_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOverflow {
    #[default]
    Clip,
    Wrap,
    Ellipsis,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneData {
    Rect {
        bounds: [f32; 4],
        color: [f32; 4],
    },
    Text {
        bounds: [f32; 4],
        text: String,
        font_size: f32,
        color: [u8; 4],
        #[serde(default = "default_family")]
        family: String,
        #[serde(default = "default_weight")]
        weight: u16,
        #[serde(default)]
        overflow: TextOverflow,
        #[serde(default)]
        clip: Option<[f32; 4]>,
    },
    Group {
        offset: [f32; 2],
        children: Vec<SceneData>,
    },
    Clip {
        bounds: [f32; 4],
        children: Vec<SceneData>,
    },
}

fn default_family() -> String {
    "DejaVu Sans".into()
}
fn default_weight() -> u16 {
    400
}
fn bounds([x, y, width, height]: [f32; 4]) -> Bounds {
    Bounds::new(x, y, width, height)
}

pub fn build_nodes(data: &[SceneData]) -> Result<Vec<Node>> {
    fn visit(data: &[SceneData], depth: usize, count: &mut usize) -> Result<Vec<Node>> {
        ensure!(depth <= MAX_SCENE_DEPTH, "Scene nesting exceeds 64");
        *count += data.len();
        ensure!(*count <= MAX_SCENE_NODES, "Scene exceeds 4096 nodes");
        data.iter()
            .map(|node| {
                Ok(match node {
                    SceneData::Rect { bounds: b, color } => Node::Rect {
                        bounds: bounds(*b),
                        color: *color,
                    },
                    SceneData::Text {
                        bounds,
                        text,
                        font_size,
                        color,
                        family,
                        weight,
                        overflow,
                        clip,
                    } => {
                        ensure!(
                            text.len() <= MAX_TEXT_BYTES,
                            "Scene text exceeds 4096 bytes"
                        );
                        ensure!(
                            family.len() <= MAX_FAMILY_BYTES,
                            "Font family exceeds 256 bytes"
                        );
                        ensure!(*font_size <= 512.0, "Font size exceeds 512 pixels");
                        let mut label =
                            Label::new(text, *bounds, *font_size, *color).with_family(family);
                        label.weight = *weight;
                        label.clip = *clip;
                        label.overflow = match overflow {
                            TextOverflow::Clip => Overflow::Clip,
                            TextOverflow::Wrap => Overflow::Wrap,
                            TextOverflow::Ellipsis => Overflow::Ellipsis,
                        };
                        Node::Text(label)
                    }
                    SceneData::Group { offset, children } => Node::Group {
                        offset: *offset,
                        children: visit(children, depth + 1, count)?,
                    },
                    SceneData::Clip {
                        bounds: b,
                        children,
                    } => Node::Clip {
                        bounds: bounds(*b),
                        children: visit(children, depth + 1, count)?,
                    },
                })
            })
            .collect()
    }
    let nodes = visit(data, 0, &mut 0)?;
    Scene::new(nodes.clone())?;
    Ok(nodes)
}

pub fn build_scene(data: &[SceneData]) -> Result<Scene> {
    Scene::new(build_nodes(data)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect() -> SceneData {
        SceneData::Rect {
            bounds: [0.0, 0.0, 10.0, 10.0],
            color: [1.0; 4],
        }
    }

    #[test]
    fn wire_roundtrip_and_text_defaults() -> Result<()> {
        let text: SceneData = serde_json::from_str(
            r#"{"type":"text","text":"hello","bounds":[0,0,100,30],"font_size":16,"color":[255,255,255,255]}"#,
        )?;
        let data = vec![SceneData::Group {
            offset: [2.0, 3.0],
            children: vec![rect(), text],
        }];
        let encoded = serde_json::to_vec(&data)?;
        assert_eq!(serde_json::from_slice::<Vec<SceneData>>(&encoded)?, data);
        assert_eq!(
            build_scene(&data)?
                .paint_ops(Bounds::new(0.0, 0.0, 200.0, 200.0))
                .len(),
            2
        );
        assert!(
            serde_json::from_str::<SceneData>(
                r#"{"type":"rect","bounds":[0,0,1,1],"color":[1,1,1,1],"evaluate":"anything"}"#
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn limits_and_invalid_geometry_are_rejected() -> Result<()> {
        assert!(build_nodes(&vec![rect(); 4097]).is_err());
        let mut nested = rect();
        for _ in 0..65 {
            nested = SceneData::Group {
                offset: [0.0; 2],
                children: vec![nested],
            };
        }
        assert!(build_nodes(&[nested]).is_err());
        let mut text: SceneData = serde_json::from_str(
            r#"{"type":"text","text":"hello","bounds":[0,0,100,30],"font_size":16,"color":[255,255,255,255]}"#,
        )?;
        if let SceneData::Text { text, .. } = &mut text {
            *text = "a".repeat(4097);
        }
        assert!(build_nodes(&[text]).is_err());
        assert!(
            build_nodes(&[SceneData::Rect {
                bounds: [f32::MAX, 0.0, f32::MAX, 1.0],
                color: [1.0; 4]
            }])
            .is_err()
        );
        Ok(())
    }
}
