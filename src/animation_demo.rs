use anyhow::Result;

use crate::{
    animation::{Prop, Px, Seconds, Spring, Transition},
    scene::{Scene, rect},
};

pub const SNAPSHOTS: [(&str, u32); 4] = [
    ("m2-start.png", 0),
    ("m2-150ms.png", 9),
    ("m2-500ms.png", 30),
    ("m2-2s.png", 120),
];

pub struct Demo {
    position: Prop<Px>,
}

impl Demo {
    pub fn new() -> Result<Self> {
        let mut position = Prop::new(Px(56.0))?;
        position.transition_to(Px(480.0), Transition::Spring(Spring::new(1.5, 0.6, 0.02)?))?;
        Ok(Self { position })
    }

    pub fn advance(&mut self, delta: Seconds) -> bool {
        self.position.advance(delta).changed
    }

    pub fn active(&self) -> bool {
        self.position.is_active()
    }

    pub fn position(&self) -> f64 {
        self.position.value().0
    }

    pub fn scene(&self) -> Result<Scene> {
        Scene::new(vec![
            rect(24.0, 24.0, 592.0, 352.0, [0.07, 0.09, 0.14, 1.0]),
            rect(52.0, 56.0, 224.0, 20.0, [0.65, 0.74, 0.87, 1.0]),
            rect(52.0, 92.0, 160.0, 8.0, [0.25, 0.32, 0.44, 1.0]),
            rect(48.0, 152.0, 544.0, 128.0, [0.04, 0.06, 0.10, 1.0]),
            rect(480.0, 172.0, 72.0, 88.0, [0.11, 0.19, 0.24, 1.0]),
            rect(
                self.position() as f32,
                172.0,
                72.0,
                88.0,
                [0.22, 0.72, 0.64, 1.0],
            ),
            rect(52.0, 320.0, 500.0, 4.0, [0.25, 0.32, 0.44, 1.0]),
            rect(
                self.position() as f32,
                316.0,
                4.0,
                12.0,
                [0.65, 0.74, 0.87, 1.0],
            ),
        ])
    }
}

pub fn sampled_scene(frames: u32) -> Result<Scene> {
    let mut demo = Demo::new()?;
    let delta = Seconds::new(1.0 / 60.0)?;
    for _ in 0..frames {
        demo.advance(delta);
    }
    demo.scene()
}
