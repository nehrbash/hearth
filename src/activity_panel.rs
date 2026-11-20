use crate::{
    panel::{Panel, color},
    protocol::Snapshot,
    scene::{Scene, rect},
    text::Label,
};
use anyhow::Result;

const INK: [u8; 4] = [226, 232, 240, 255];
const MUTED: [u8; 4] = [147, 161, 180, 255];
const ACCENT: [u8; 4] = [124, 215, 193, 255];

fn short(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        text.chars()
            .take(limit.saturating_sub(1))
            .chain(['…'])
            .collect()
    }
}

pub fn build(snapshot: &Snapshot, width: u32, height: u32) -> Result<Panel> {
    snapshot.validate()?;
    anyhow::ensure!(
        width >= 760 && height >= 860,
        "Activity view needs at least 760×860 pixels"
    );
    let w = width as f32;
    let h = height as f32;
    let mut nodes = vec![
        rect(0.0, 0.0, w, h, color([17, 23, 32])),
        rect(28.0, 30.0, 9.0, 34.0, color([124, 215, 193])),
    ];
    let mut labels = vec![
        Label::new(
            "Workspace activity",
            [50.0, 24.0, w - 100.0, 40.0],
            26.0,
            INK,
        )
        .bold(),
        Label::new(
            "Sessions · watches · notices · terminal leases",
            [51.0, 65.0, w - 100.0, 25.0],
            13.0,
            MUTED,
        ),
    ];
    let Some(activity) = &snapshot.activity else {
        labels.push(Label::new(
            "No activity snapshot",
            [28.0, 130.0, w - 56.0, 36.0],
            24.0,
            INK,
        ));
        labels.push(Label::new(
            "The zones connection does not imply host inspection.",
            [28.0, 178.0, w - 56.0, 35.0],
            15.0,
            MUTED,
        ));
        return Ok(Panel {
            scene: Scene::new(nodes)?,
            labels,
        });
    };
    let cards = [
        (28.0, 112.0, w - 56.0, 228.0),
        (28.0, 356.0, (w - 72.0) / 2.0, 188.0),
        (w / 2.0 + 8.0, 356.0, (w - 72.0) / 2.0, 188.0),
        (28.0, 560.0, w - 56.0, 222.0),
    ];
    for (x, y, cw, ch) in cards {
        nodes.push(rect(x, y, cw, ch, color([26, 34, 46])));
    }
    labels.push(
        Label::new(
            format!("Sessions  ·  {} total", activity.sessions.len()),
            [46.0, 126.0, w - 92.0, 28.0],
            17.0,
            INK,
        )
        .bold(),
    );
    for (index, session) in activity.sessions.iter().take(4).enumerate() {
        let y = 164.0 + index as f32 * 39.0;
        labels.push(Label::new(
            short(&session.id, 35),
            [46.0, y, 300.0, 23.0],
            12.0,
            INK,
        ));
        labels.push(Label::new(
            &session.status,
            [350.0, y, 95.0, 23.0],
            12.0,
            ACCENT,
        ));
        let host = session
            .host
            .as_ref()
            .map(|host| format!("{} · {}", host.buffer, host.mode))
            .unwrap_or_else(|| "host unknown".into());
        labels.push(Label::new(
            short(&host, 48),
            [455.0, y, w - 501.0, 23.0],
            12.0,
            INK,
        ));
        labels.push(Label::new(
            format!(
                "Delivery: {}",
                session.delivery.as_deref().unwrap_or("unavailable")
            ),
            [46.0, y + 19.0, w - 92.0, 18.0],
            10.0,
            MUTED,
        ));
    }
    if activity.sessions.is_empty() {
        labels.push(Label::new(
            "No sessions reported",
            [46.0, 172.0, w - 92.0, 25.0],
            14.0,
            MUTED,
        ));
    }
    let half = (w - 72.0) / 2.0;
    labels.push(
        Label::new(
            format!("Watches  ·  {} total", activity.watches.len()),
            [46.0, 370.0, half - 36.0, 28.0],
            17.0,
            INK,
        )
        .bold(),
    );
    for (i, watch) in activity.watches.iter().take(3).enumerate() {
        let y = 408.0 + i as f32 * 38.0;
        let kind = match watch.target {
            crate::activity::WatchTarget::Session(_) => "session",
            crate::activity::WatchTarget::Provider(_) => "provider",
            crate::activity::WatchTarget::Agent(_) => "agent",
            crate::activity::WatchTarget::Topic(_) => "topic",
        };
        labels.push(Label::new(
            short(
                &format!("{} → {kind}: {}", watch.subscriber, watch.target.value()),
                50,
            ),
            [46.0, y, half - 36.0, 21.0],
            12.0,
            INK,
        ));
        labels.push(Label::new(
            format!(
                "Push: {}",
                watch.delivery.as_deref().unwrap_or("not configured")
            ),
            [46.0, y + 18.0, half - 36.0, 19.0],
            10.0,
            MUTED,
        ));
    }
    if activity.watches.is_empty() {
        labels.push(Label::new(
            "No watches",
            [46.0, 412.0, half - 36.0, 25.0],
            14.0,
            MUTED,
        ));
    }
    let lx = w / 2.0 + 26.0;
    labels.push(
        Label::new(
            format!("Terminal leases  ·  {} held", activity.leases.len()),
            [lx, 370.0, half - 36.0, 28.0],
            17.0,
            INK,
        )
        .bold(),
    );
    for (i, lease) in activity.leases.iter().take(3).enumerate() {
        let y = 408.0 + i as f32 * 38.0;
        labels.push(Label::new(
            format!("{} · {}", lease.buffer, lease.holder),
            [lx, y, half - 36.0, 21.0],
            12.0,
            INK,
        ));
        let expiry = lease
            .expires_at_ms
            .map(|ms| format!("Expiry: {ms} ms since epoch"))
            .unwrap_or_else(|| "Expiry unavailable".into());
        labels.push(Label::new(
            expiry,
            [lx, y + 18.0, half - 36.0, 19.0],
            10.0,
            MUTED,
        ));
    }
    if activity.leases.is_empty() {
        labels.push(Label::new(
            "No held leases reported",
            [lx, 412.0, half - 36.0, 25.0],
            14.0,
            MUTED,
        ));
    }
    labels.push(
        Label::new(
            format!(
                "Recent notices  ·  {} retained for watched subscribers",
                activity.notices.len()
            ),
            [46.0, 574.0, w - 92.0, 28.0],
            17.0,
            INK,
        )
        .bold(),
    );
    for (i, notice) in activity.notices.iter().rev().take(3).enumerate() {
        let y = 614.0 + i as f32 * 46.0;
        labels.push(Label::new(
            format!("#{}  {} → {}", notice.id, notice.kind, notice.subscriber),
            [46.0, y, w - 92.0, 22.0],
            12.0,
            ACCENT,
        ));
        labels.push(Label::new(
            short(&notice.text, 115),
            [46.0, y + 21.0, w - 92.0, 21.0],
            12.0,
            INK,
        ));
    }
    if activity.notices.is_empty() {
        labels.push(Label::new(
            "No retained notices for current watch subscribers",
            [46.0, 622.0, w - 92.0, 25.0],
            14.0,
            MUTED,
        ));
    }
    let warnings = if activity.warnings.is_empty() {
        "Preview: up to 4 sessions, 3 watches, 3 leases and 3 notices. History is bounded.".into()
    } else {
        format!("Inspection warning: {}", activity.warnings.join("; "))
    };
    labels.push(
        Label::new(
            warnings,
            [28.0, 798.0, w - 56.0, 40.0],
            12.0,
            [235, 195, 130, 255],
        )
        .wrap(),
    );
    labels.push(Label::new(
        format!(
            "Weft · revision {} · notice history limit {}",
            snapshot.revision, activity.notice_history_limit
        ),
        [28.0, h - 30.0, w - 56.0, 23.0],
        11.0,
        MUTED,
    ));
    Ok(Panel {
        scene: Scene::new(nodes)?,
        labels,
    })
}
