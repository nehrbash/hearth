use crate::scene::{Bounds, Node, Scene};
use crate::text::Label;

#[derive(Default)]
pub struct SceneBuilder {
    nodes: Vec<Node>,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }
    pub fn rect(self, bounds: Bounds, color: [f32; 4]) -> Self {
        self.node(Node::Rect { bounds, color })
    }
    pub fn text(self, label: Label) -> Self {
        self.node(Node::Text(label))
    }
    pub fn group(self, offset: [f32; 2], children: Self) -> Self {
        self.node(Node::Group {
            offset,
            children: children.nodes,
        })
    }
    pub fn clip(self, bounds: Bounds, children: Self) -> Self {
        self.node(Node::Clip {
            bounds,
            children: children.nodes,
        })
    }
    pub fn nodes(self) -> Vec<Node> {
        self.nodes
    }
    pub fn build(self) -> anyhow::Result<Scene> {
        Scene::new(self.nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builder_preserves_order_and_nested_clipping() -> anyhow::Result<()> {
        let scene = SceneBuilder::new()
            .group(
                [10.0, 20.0],
                SceneBuilder::new().clip(
                    Bounds::new(0.0, 0.0, 5.0, 5.0),
                    SceneBuilder::new().rect(Bounds::new(-1.0, -1.0, 10.0, 10.0), [1.0; 4]),
                ),
            )
            .text(Label::new("last", [0.0, 0.0, 50.0, 30.0], 16.0, [255; 4]))
            .build()?;
        let viewport = Bounds::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            scene.quads(viewport)[0].bounds,
            Bounds::new(10.0, 20.0, 5.0, 5.0)
        );
        assert!(matches!(
            scene.paint_ops(viewport)[1],
            crate::scene::PaintOp::Text(_)
        ));
        Ok(())
    }
}
