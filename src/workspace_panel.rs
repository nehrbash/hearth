use crate::{
    panel::{Panel, color},
    scene::{Bounds, ImageAsset, Node, Scene, rect},
    text::Label,
    workspaces::WorkspaceSnapshot,
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, sync::OnceLock};

pub struct WorkspaceSlot {
    pub id: i64,
    pub name: String,
    pub bounds: Bounds,
    pub active: bool,
    pub occupied: Option<bool>,
    pub other_monitor: bool,
}

pub struct WorkspacePanel {
    pub panel: Panel,
    pub slots: Vec<WorkspaceSlot>,
}

pub fn build(
    snapshot: &WorkspaceSnapshot,
    monitor_name: &str,
    width: u32,
    height: u32,
    shown: u32,
) -> Result<WorkspacePanel> {
    snapshot.validate()?;
    ensure!(
        (60..=512).contains(&width) && height >= 240 && (1..=32).contains(&shown),
        "Invalid workspace bar dimensions or slot count"
    );
    let monitor = snapshot
        .monitors
        .iter()
        .find(|m| m.name == monitor_name)
        .context("Selected monitor is not present; workspace state is unavailable")?;
    let mut ids: Vec<i64> = (1..=i64::from(shown)).collect();
    let active_ids: BTreeSet<_> = snapshot
        .monitors
        .iter()
        .flat_map(|m| [Some(m.active_workspace), m.special_workspace])
        .flatten()
        .collect();
    let mut extra: BTreeSet<_> = active_ids
        .iter()
        .copied()
        .filter(|id| *id < 1 || *id > i64::from(shown))
        .collect();
    for workspace in &snapshot.workspaces {
        if (workspace.id < 1 || workspace.id > i64::from(shown))
            && (workspace.monitor == monitor_name
                || !workspace.name.chars().all(|c| c.is_ascii_digit()))
        {
            extra.insert(workspace.id);
        }
    }
    ids.extend(extra);
    let art = artwork()?;
    let w = width as f32;
    let h = height as f32;
    let x = w / 2.0 - 18.0;
    let required = 167.0
        + ids
            .iter()
            .map(|id| {
                let occupied = snapshot
                    .workspaces
                    .iter()
                    .any(|ws| ws.id == *id && ws.windows > 0);
                (if *id < 0 { 40.0 } else { 60.0 }) + if occupied { 25.0 } else { 12.0 }
            })
            .sum::<f32>();
    ensure!(
        required <= h - 24.0,
        "Workspace bar is too short to show every workspace; enlarge its height"
    );
    let mut nodes = vec![
        rect(0.0, 0.0, w, h, color([15, 11, 8])),
        image(&art.trunk, Bounds::new(0.0, 0.0, w, h)),
    ];
    nodes.push(Node::Image {
        bounds: Bounds::new(w / 2.0 - 15.3, 30.0, 34.6, 53.04),
        image: art.companions.clone(),
        source: Bounds::new(0.0, 140.0, 313.5, 480.0),
        opacity: 1.0,
    });
    let mut slots = Vec::new();
    let mut y = 167.0;
    for id in ids {
        let workspace = snapshot.workspaces.iter().find(|ws| ws.id == id);
        let active = monitor.active_workspace == id || monitor.special_workspace == Some(id);
        let other_monitor = snapshot.monitors.iter().any(|m| {
            m.name != monitor_name && (m.active_workspace == id || m.special_workspace == Some(id))
        });
        let occupied = workspace
            .map(|ws| ws.windows > 0)
            .or_else(|| (!active_ids.contains(&id)).then_some(false));
        let name = workspace.map_or_else(|| id.to_string(), |ws| ws.name.clone());
        let card_height = if id < 0 { 40.0 } else { 60.0 };
        let bounds = Bounds::new(x, y, 40.0, card_height);
        if id >= 0 {
            nodes.push(image(
                if active {
                    &art.cradle_active
                } else {
                    &art.cradle_idle
                },
                Bounds::new(x - 8.0, y - 3.0, 56.0, 72.0),
            ));
            let scale = if active { 1.0 } else { 0.94 };
            let bx = x - 2.0 + (40.0 - 40.0 * scale) / 2.0;
            let by = y + (60.0 - 60.0 * scale) / 2.0;
            let cw = art.deck.width() as f32 / 5.0;
            let ch = art.deck.height() as f32 / 2.0;
            let index = (id - 1).rem_euclid(10) as u32;
            nodes.push(rect(
                bx,
                by,
                40.0 * scale,
                60.0 * scale,
                color([47, 33, 23]),
            ));
            nodes.push(Node::Image {
                bounds: Bounds::new(bx, by, 40.0 * scale, 60.0 * scale),
                image: art.deck.clone(),
                source: Bounds::new((index % 5) as f32 * cw, (index / 5) as f32 * ch, cw, ch),
                opacity: 1.0,
            });
            nodes.push(image(
                if active {
                    &art.mount_active
                } else {
                    &art.mount_idle
                },
                Bounds::new(
                    bx - 5.0 * scale,
                    by - 5.0 * scale,
                    50.0 * scale,
                    70.0 * scale,
                ),
            ));
            if other_monitor {
                nodes.push(rect(
                    bx + 36.0 * scale,
                    by + 18.0 * scale,
                    2.0 * scale,
                    24.0 * scale,
                    color([217, 195, 145]),
                ));
            }
            nodes.push(rect(
                bx + 25.0 * scale,
                by + 44.0 * scale,
                11.0 * scale,
                12.0 * scale,
                [0.008, 0.006, 0.012, 0.87],
            ));
            nodes.push(Node::Text(
                Label::new(
                    id.to_string(),
                    [bx + 27.0 * scale, by + 43.0 * scale, 18.0, 15.0],
                    10.0 * scale,
                    if active {
                        [255, 240, 190, 255]
                    } else {
                        [232, 200, 125, 255]
                    },
                )
                .bold(),
            ));
            if active && monitor.focused {
                nodes.push(image(&art.eye, Bounds::new(x + 5.0, y - 7.0, 30.0, 19.0)));
            }
        } else {
            nodes.push(image(
                &art.socket,
                Bounds::new(x + 5.6, y + 5.6, 28.8, 28.8),
            ));
            nodes.push(image(
                if active {
                    &art.special_active
                } else {
                    &art.special_idle
                },
                Bounds::new(x, y, 40.0, 40.0),
            ));
            let letter = name
                .trim_start_matches("special:")
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            nodes.push(Node::Text(Label::new(
                letter,
                [x + 12.0, y + 8.0, 24.0, 28.0],
                20.0,
                [243, 231, 201, 255],
            )));
        }
        if occupied == Some(true) {
            nodes.push(image(
                &art.socket,
                Bounds::new(x + 9.0, y + card_height - 9.9, 22.0, 22.0),
            ));
            nodes.push(Node::Text(Label::new(
                workspace.map_or(0, |ws| ws.windows).to_string(),
                [x + 16.0, y + card_height - 5.0, 18.0, 16.0],
                10.0,
                [243, 231, 201, 255],
            )));
        }
        slots.push(WorkspaceSlot {
            id,
            name,
            bounds,
            active,
            occupied,
            other_monitor,
        });
        y += card_height + if occupied == Some(true) { 25.0 } else { 12.0 };
    }
    Ok(WorkspacePanel {
        panel: Panel {
            scene: Scene::new(nodes)?,
            labels: Vec::new(),
        },
        slots,
    })
}

struct Artwork {
    trunk: ImageAsset,
    deck: ImageAsset,
    companions: ImageAsset,
    cradle_idle: ImageAsset,
    cradle_active: ImageAsset,
    mount_idle: ImageAsset,
    mount_active: ImageAsset,
    eye: ImageAsset,
    socket: ImageAsset,
    special_idle: ImageAsset,
    special_active: ImageAsset,
}
fn artwork() -> Result<&'static Artwork> {
    static ART: OnceLock<Result<Artwork, String>> = OnceLock::new();
    ART.get_or_init(|| load_artwork().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))
}
fn load_artwork() -> Result<Artwork> {
    Ok(Artwork {
        trunk: ImageAsset::from_png(include_bytes!("../assets/woodland/trunk-60x1440.png"))?,
        deck: ImageAsset::from_png(include_bytes!("../assets/woodland/deck.png"))?,
        companions: ImageAsset::from_png(include_bytes!("../assets/woodland/companions.png"))?,
        cradle_idle: ImageAsset::from_png(include_bytes!(
            "../assets/woodland/BranchCradle-idle.png"
        ))?,
        cradle_active: ImageAsset::from_png(include_bytes!(
            "../assets/woodland/BranchCradle-selected.png"
        ))?,
        mount_idle: ImageAsset::from_png(include_bytes!(
            "../assets/woodland/BranchMount-idle.png"
        ))?,
        mount_active: ImageAsset::from_png(include_bytes!(
            "../assets/woodland/BranchMount-selected.png"
        ))?,
        eye: ImageAsset::from_png(include_bytes!("../assets/woodland/focused-eye.png"))?,
        special_idle: ImageAsset::from_png(include_bytes!(
            "../assets/woodland/special-seal-idle.png"
        ))?,
        special_active: ImageAsset::from_png(include_bytes!(
            "../assets/woodland/special-seal-active.png"
        ))?,
        socket: ImageAsset::from_png(include_bytes!("../assets/woodland/socket.png"))?,
    })
}
fn image(asset: &ImageAsset, bounds: Bounds) -> Node {
    Node::Image {
        bounds,
        image: asset.clone(),
        source: Bounds::new(0.0, 0.0, asset.width() as f32, asset.height() as f32),
        opacity: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::build;
    use crate::workspaces::{Monitor, Workspace, WorkspaceSnapshot};

    fn snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            instance: "current".into(),
            monitors: vec![
                Monitor {
                    name: "DP-1".into(),
                    focused: true,
                    active_workspace: 12,
                    special_workspace: Some(-98),
                },
                Monitor {
                    name: "DP-2".into(),
                    focused: false,
                    active_workspace: 4,
                    special_workspace: None,
                },
            ],
            workspaces: vec![
                Workspace {
                    id: 12,
                    name: "writing".into(),
                    monitor: "DP-1".into(),
                    windows: 2,
                },
                Workspace {
                    id: -98,
                    name: "special:mini".into(),
                    monitor: "DP-1".into(),
                    windows: 1,
                },
                Workspace {
                    id: 4,
                    name: "4".into(),
                    monitor: "DP-2".into(),
                    windows: 1,
                },
            ],
        }
    }

    #[test]
    fn fixed_slots_keep_excess_active_and_special_workspaces_visible() {
        let view = build(&snapshot(), "DP-1", 120, 1000, 7).expect("panel");
        assert_eq!(
            view.slots.iter().take(7).map(|s| s.id).collect::<Vec<_>>(),
            (1..=7).collect::<Vec<_>>()
        );
        assert!(
            view.slots
                .iter()
                .any(|s| s.id == 12 && s.active && s.name == "writing")
        );
        assert!(view.slots.iter().any(|s| s.id == -98 && s.active));
        assert!(
            view.slots
                .iter()
                .any(|s| s.id == 4 && s.other_monitor && s.occupied == Some(true))
        );
    }

    #[test]
    fn missing_monitor_and_insufficient_height_are_explicit_errors() {
        assert!(build(&snapshot(), "missing", 120, 1000, 7).is_err());
        assert!(build(&snapshot(), "DP-1", 120, 240, 7).is_err());
        let mut state = snapshot();
        state.workspaces.clear();
        let view = build(&state, "DP-1", 120, 1000, 7).expect("unknown workspace");
        assert!(
            view.slots
                .iter()
                .any(|s| s.id == 12 && s.occupied.is_none())
        );
    }
}
