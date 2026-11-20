use crate::{
    activity::EffectState,
    owner::{CommandRequest, Decision},
    panel::{Panel, color},
    protocol::Snapshot,
    scene::{Bounds, Scene, rect},
    text::{Label, fully_visible},
};
use anyhow::Result;

const INK: [u8; 4] = [226, 232, 240, 255];
const MUTED: [u8; 4] = [147, 161, 180, 255];
const ACCENT: [u8; 4] = [124, 215, 193, 255];
const WARNING: [u8; 4] = [235, 195, 130, 255];

pub struct OwnerButton {
    pub bounds: Bounds,
    pub label: &'static str,
    pub request: Option<CommandRequest>,
}

impl OwnerButton {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.bounds.x
            && x < self.bounds.x + self.bounds.width
            && y >= self.bounds.y
            && y < self.bounds.y + self.bounds.height
    }
}

pub struct OwnerView {
    pub panel: Panel,
    pub buttons: Vec<OwnerButton>,
}

impl OwnerView {
    pub fn request_at(&self, x: f32, y: f32) -> Option<CommandRequest> {
        self.buttons
            .iter()
            .find(|button| button.contains(x, y))?
            .request
            .clone()
    }
}

fn state_name(state: EffectState) -> &'static str {
    match state {
        EffectState::Pending => "pending",
        EffectState::Executing => "executing",
        EffectState::Completed => "completed",
        EffectState::Failed => "failed",
        EffectState::Rejected => "rejected",
        EffectState::Interrupted => "interrupted",
    }
}

fn escaped_preview(value: &str, limit: usize) -> (String, bool) {
    let full = format!("\"{}\"", value.escape_default());
    if full.len() <= limit {
        (full, false)
    } else {
        (format!("{}...", &full[..limit.saturating_sub(3)]), true)
    }
}

pub fn build(snapshot: &Snapshot, width: u32, height: u32) -> Result<OwnerView> {
    build_with_controls(snapshot, width, height, false, false)
}

pub fn build_with_controls(
    snapshot: &Snapshot,
    width: u32,
    height: u32,
    enabled: bool,
    busy: bool,
) -> Result<OwnerView> {
    snapshot.validate()?;
    anyhow::ensure!(
        width >= 760 && height >= 700,
        "Owner view needs at least 760×700 pixels"
    );
    let w = width as f32;
    let h = height as f32;
    let mut nodes = vec![
        rect(0.0, 0.0, w, h, color([17, 23, 32])),
        rect(28.0, 30.0, 9.0, 34.0, color([235, 195, 130])),
    ];
    let mode = if !enabled {
        "Read-only · owner controls disabled"
    } else if busy {
        "Decision submitted · waiting for the broker"
    } else {
        "Owner controls enabled · each decision requires a click"
    };
    let mut labels = vec![
        Label::new("Owner decisions", [50.0, 24.0, w - 100.0, 40.0], 26.0, INK).bold(),
        Label::new(mode, [51.0, 65.0, w - 100.0, 26.0], 13.0, MUTED),
    ];
    let mut buttons = Vec::new();
    let Some(activity) = &snapshot.activity else {
        labels.push(Label::new(
            "No owner snapshot",
            [28.0, 132.0, w - 56.0, 40.0],
            24.0,
            INK,
        ));
        return Ok(OwnerView {
            panel: Panel {
                scene: Scene::new(nodes)?,
                labels,
            },
            buttons,
        });
    };
    let mut effects: Vec<_> = activity.effects.iter().collect();
    effects.sort_by_key(|effect| (effect.state != EffectState::Pending, effect.id));
    let visible = ((h - 170.0) / 316.0).floor() as usize;
    for (index, effect) in effects.iter().take(visible).enumerate() {
        let y = 120.0 + index as f32 * 316.0;
        nodes.push(rect(28.0, y, w - 56.0, 300.0, color([26, 34, 46])));
        labels.push(
            Label::new(
                format!("#{} · {}", effect.id, effect.kind),
                [46.0, y + 14.0, w - 252.0, 28.0],
                17.0,
                INK,
            )
            .bold(),
        );
        labels.push(Label::new(
            state_name(effect.state),
            [w - 180.0, y + 16.0, 134.0, 26.0],
            14.0,
            if effect.state == EffectState::Pending {
                ACCENT
            } else {
                WARNING
            },
        ));
        labels.push(Label::new(
            "Exact proposal · escaped text",
            [46.0, y + 48.0, w - 92.0, 20.0],
            11.0,
            MUTED,
        ));
        let (expected, shortened) = escaped_preview(&effect.expected, 4096);
        let expected_label = Label::new(expected, [46.0, y + 73.0, w - 92.0, 106.0], 13.0, INK)
            .wrap()
            .with_clip([28.0, y, w - 56.0, 300.0]);
        let reviewable = !shortened && fully_visible(&expected_label)?;
        labels.push(expected_label);
        let (receipt, _) = escaped_preview(&effect.receipt, 2048);
        labels.push(
            Label::new(
                format!("Receipt preview: {receipt}"),
                [46.0, y + 188.0, w - 92.0, 36.0],
                11.0,
                MUTED,
            )
            .wrap(),
        );
        let actionable = enabled
            && !busy
            && effect.state == EffectState::Pending
            && activity.effects_generation.is_some();
        for (offset, decision, label) in [
            (0.0, Decision::Approve, "Approve"),
            (132.0, Decision::Reject, "Reject"),
        ] {
            let allowed = actionable && (decision != Decision::Approve || reviewable);
            let bounds = Bounds::new(46.0 + offset, y + 236.0, 116.0, 34.0);
            nodes.push(rect(
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                color(if allowed { [45, 77, 75] } else { [40, 47, 58] }),
            ));
            labels.push(Label::new(
                label,
                [bounds.x + 13.0, bounds.y + 7.0, 90.0, 23.0],
                13.0,
                if allowed { INK } else { MUTED },
            ));
            buttons.push(OwnerButton {
                bounds,
                label,
                request: if allowed {
                    Some(CommandRequest::capture(activity, effect.id, decision)?)
                } else {
                    None
                },
            });
        }
        let reason = if !reviewable {
            "Proposal preview is incomplete · approval disabled; inspect the Weft console."
        } else if effect.state == EffectState::Interrupted {
            "Interrupted proposals are not replayed; submit a fresh proposal in Weft."
        } else if effect.state != EffectState::Pending {
            "This proposal is no longer pending."
        } else if !enabled {
            "Read-only preview; no decisions are sent."
        } else if busy {
            "Wait for the current decision; closing this view does not revoke it."
        } else {
            "Weft verifies the exact proposal and pending state before deciding."
        };
        labels.push(Label::new(
            reason,
            [46.0, y + 275.0, w - 92.0, 20.0],
            11.0,
            MUTED,
        ));
    }
    if effects.is_empty() {
        let text = if activity.effects_generation.is_some() {
            "No owner proposals reported"
        } else {
            "Owner inspection unavailable"
        };
        labels.push(Label::new(text, [28.0, 132.0, w - 56.0, 40.0], 24.0, INK));
    }
    labels.push(Label::new(
        format!(
            "Showing {} of {} proposals · state refreshes with the next snapshot",
            visible.min(effects.len()),
            effects.len()
        ),
        [28.0, h - 33.0, w - 56.0, 25.0],
        12.0,
        MUTED,
    ));
    Ok(OwnerView {
        panel: Panel {
            scene: Scene::new(nodes)?,
            labels,
        },
        buttons,
    })
}

#[cfg(test)]
mod tests {
    use super::{build, build_with_controls, escaped_preview};
    use crate::activity::{Activity, Effect, EffectState};
    use crate::owner::Decision;
    use crate::protocol::Snapshot;

    fn snapshot(expected: &str) -> Snapshot {
        Snapshot {
            scene: None,
            version: 1,
            kind: "snapshot".into(),
            workspace: "isolated".into(),
            generation: "zones".into(),
            revision: 1,
            zones: vec![],
            activity: Some(Activity {
                generation: "trusted".into(),
                sessions: vec![],
                watches: vec![],
                notices: vec![],
                leases: vec![],
                effects: vec![Effect {
                    id: 7,
                    state: EffectState::Pending,
                    kind: "notify".into(),
                    expected: expected.into(),
                    receipt: "#f".into(),
                }],
                effects_generation: Some("isolated".into()),
                warnings: vec![],
                notice_history_limit: 128,
                notice_scope: "current-watch-subscribers".into(),
            }),
        }
    }

    #[test]
    fn rendering_is_read_only_unless_controls_are_explicit() {
        let snapshot = snapshot("(notify \"Ready\")");
        let view = build(&snapshot, 960, 900).expect("read-only view");
        assert!(view.buttons.iter().all(|button| button.request.is_none()));
        let view = build_with_controls(&snapshot, 960, 900, true, false).expect("controls");
        let request = view.request_at(60.0, 365.0).expect("approve button");
        assert_eq!(request.decision, Decision::Approve);
        assert_eq!(request.expected, "(notify \"Ready\")");
        assert!(view.request_at(0.0, 0.0).is_none());
    }

    #[test]
    fn clipped_proposals_cannot_be_approved_and_busy_buttons_have_no_actions() {
        let view = build_with_controls(&snapshot(&"long ".repeat(3000)), 760, 700, true, false)
            .expect("long preview");
        assert!(view.buttons[0].request.is_none());
        assert_eq!(
            view.buttons[1]
                .request
                .as_ref()
                .expect("explicit rejection")
                .decision,
            Decision::Reject
        );
        let view = build_with_controls(&snapshot("(notify \"Ready\")"), 960, 900, true, true)
            .expect("busy preview");
        assert!(view.buttons.iter().all(|button| button.request.is_none()));
    }

    #[test]
    fn wrapped_content_beyond_the_card_disables_approval() {
        let raw = "word ".repeat(500);
        assert!(raw.len() < 4096);
        let view =
            build_with_controls(&snapshot(&raw), 760, 700, true, false).expect("wrapped preview");
        assert!(view.buttons[0].request.is_none());
        assert!(
            view.panel
                .labels
                .iter()
                .any(|label| label.text.contains("preview is incomplete"))
        );
    }

    #[test]
    fn invisible_characters_are_unambiguous_and_broker_content_is_unchanged() {
        let raw = "(notify \"a\u{202e}b\")";
        let (display, clipped) = escaped_preview(raw, 4096);
        assert!(!clipped);
        assert!(display.is_ascii());
        assert!(display.contains("\\u{202e}"));
        assert_ne!(display, escaped_preview("(notify \"a\\u{202e}b\")", 4096).0);
        let view =
            build_with_controls(&snapshot(raw), 960, 900, true, false).expect("escaped preview");
        assert_eq!(
            view.buttons[0]
                .request
                .as_ref()
                .expect("fully visible escaped content")
                .expected,
            raw
        );
    }
}
