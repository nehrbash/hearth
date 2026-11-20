use crate::reactive::{Bound, Context, FlushStats, Owner, Runtime};
use crate::scene::{Node, Scene};

pub struct BoundScene {
    runtime: Runtime,
    owner: Option<Owner>,
    nodes: Bound<Vec<Node>>,
    scene: Scene,
}

impl BoundScene {
    pub fn new(
        runtime: &Runtime,
        compute: impl FnMut(&mut Context) -> Vec<Node> + 'static,
    ) -> anyhow::Result<Self> {
        let owner = runtime.owner();
        let nodes = owner.bind(Vec::new(), compute)?;
        Ok(Self {
            runtime: runtime.clone(),
            owner: Some(owner),
            nodes,
            scene: Scene::default(),
        })
    }

    pub fn flush(&mut self) -> anyhow::Result<FlushStats> {
        let stats = self.runtime.flush()?;
        if self.nodes.take_damage() {
            self.scene.replace(self.nodes.get())?;
        }
        Ok(stats)
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    pub fn take_damage(&mut self) -> bool {
        self.scene.take_damage()
    }
    pub fn dispose(&mut self) {
        self.owner.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Bounds, rect};
    use std::rc::Rc;

    #[test]
    fn signal_changes_coalesce_into_scene_damage() -> anyhow::Result<()> {
        let runtime = Runtime::new();
        let x = runtime.signal(0.0);
        let source = x.clone();
        let mut scene = BoundScene::new(&runtime, move |cx| {
            vec![rect(cx.read(&source), 0.0, 10.0, 10.0, [1.0; 4])]
        })?;
        scene.flush()?;
        assert!(scene.take_damage());
        x.set(1.0);
        x.set(2.0);
        x.set(3.0);
        assert_eq!(scene.flush()?.effects, 1);
        assert!(scene.take_damage());
        assert!(!scene.take_damage());
        assert_eq!(
            scene.scene().quads(Bounds::new(0.0, 0.0, 100.0, 100.0))[0]
                .bounds
                .x,
            3.0
        );
        x.set(3.0);
        assert_eq!(scene.flush()?.effects, 0);
        assert!(!scene.take_damage());
        Ok(())
    }

    #[test]
    fn invalid_update_is_atomic_and_disposal_releases_callback() -> anyhow::Result<()> {
        let runtime = Runtime::new();
        let x = runtime.signal(0.0);
        let source = x.clone();
        let marker = Rc::new(());
        let weak = Rc::downgrade(&marker);
        let mut scene = BoundScene::new(&runtime, move |cx| {
            let _keep = &marker;
            vec![rect(cx.read(&source), 0.0, 10.0, 10.0, [1.0; 4])]
        })?;
        scene.flush()?;
        scene.take_damage();
        x.set(f32::NAN);
        assert!(scene.flush().is_err());
        assert!(!scene.take_damage());
        assert_eq!(
            scene.scene().quads(Bounds::new(0.0, 0.0, 100.0, 100.0))[0]
                .bounds
                .x,
            0.0
        );
        x.set(2.0);
        scene.flush()?;
        assert!(scene.take_damage());
        scene.dispose();
        assert!(weak.upgrade().is_none());
        x.set(4.0);
        assert_eq!(scene.flush()?.effects, 0);
        assert!(!scene.take_damage());
        Ok(())
    }
}
