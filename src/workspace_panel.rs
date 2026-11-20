use crate::{
    panel::{Panel, color},
    scene::{Bounds, Scene, rect},
    text::Label,
    workspaces::WorkspaceSnapshot,
};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;

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
        (64..=512).contains(&width) && height >= 240 && (1..=32).contains(&shown),
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
    let w = width as f32;
    let h = height as f32;
    let stride = ((h - 100.0) / ids.len() as f32).min(96.0);
    ensure!(
        stride >= 36.0,
        "Workspace bar is too short to show every workspace; enlarge its height"
    );
    let mut nodes = vec![rect(0.0, 0.0, w, h, color([17, 23, 32]))];
    let ink = [226, 232, 240, 255];
    let muted = [147, 161, 180, 255];
    let mut labels = vec![
        Label::new("Hearth", [8.0, 10.0, w - 16.0, 25.0], 14.0, ink).bold(),
        Label::new(monitor_name, [8.0, 37.0, w - 16.0, 20.0], 10.0, muted),
    ];
    let mut slots = Vec::new();
    for (index, id) in ids.into_iter().enumerate() {
        let workspace = snapshot.workspaces.iter().find(|ws| ws.id == id);
        let active = monitor.active_workspace == id || monitor.special_workspace == Some(id);
        let other_monitor = snapshot.monitors.iter().any(|m| {
            m.name != monitor_name && (m.active_workspace == id || m.special_workspace == Some(id))
        });
        let occupied = workspace
            .map(|ws| ws.windows > 0)
            .or_else(|| (!active_ids.contains(&id)).then_some(false));
        let name = workspace.map_or_else(|| id.to_string(), |ws| ws.name.clone());
        let bounds = Bounds::new(8.0, 68.0 + index as f32 * stride, w - 16.0, stride - 8.0);
        nodes.push(rect(
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            color(if active {
                [45, 77, 75]
            } else if occupied == Some(true) {
                [37, 47, 62]
            } else {
                [24, 31, 42]
            }),
        ));
        if active {
            nodes.push(rect(
                bounds.x,
                bounds.y,
                3.0,
                bounds.height,
                color([124, 215, 193]),
            ));
            if monitor.focused {
                nodes.push(rect(
                    w / 2.0 - 6.0,
                    bounds.y + 3.0,
                    12.0,
                    2.0,
                    color([235, 195, 130]),
                ));
            }
        }
        if other_monitor {
            nodes.push(rect(
                w - 11.0,
                bounds.y,
                3.0,
                bounds.height,
                color([166, 151, 224]),
            ));
        }
        labels.push(
            Label::new(
                &name,
                [
                    14.0,
                    bounds.y + 10.0,
                    w - 28.0,
                    (bounds.height - 13.0).min(22.0),
                ],
                if name == id.to_string() { 14.0 } else { 12.0 },
                ink,
            )
            .ellipsis(),
        );
        if bounds.height >= 54.0 {
            let detail = match workspace {
                Some(ws) => {
                    let count = format!(
                        "{} {}",
                        ws.windows,
                        if ws.windows == 1 { "window" } else { "windows" }
                    );
                    if name != id.to_string() {
                        format!("{id} · {count}")
                    } else {
                        count
                    }
                }
                None if occupied.is_none() => "state unknown".into(),
                None => "empty".into(),
            };
            labels.push(
                Label::new(detail, [14.0, bounds.y + 35.0, w - 28.0, 18.0], 10.0, muted).ellipsis(),
            );
        }
        slots.push(WorkspaceSlot {
            id,
            name,
            bounds,
            active,
            occupied,
            other_monitor,
        });
    }
    labels.push(Label::new(
        "Read-only",
        [8.0, h - 25.0, w - 16.0, 18.0],
        10.0,
        muted,
    ));
    Ok(WorkspacePanel {
        panel: Panel {
            scene: Scene::new(nodes)?,
            labels,
        },
        slots,
    })
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
