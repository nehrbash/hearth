use crate::{
    protocol::{Snapshot, ZoneType},
    scene::{Scene, rect},
    text::Label,
};
use anyhow::Result;

pub struct Panel {
    pub scene: Scene,
    pub labels: Vec<Label>,
}

fn color(rgb: [u8; 3]) -> [f32; 4] {
    let linear = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [linear(rgb[0]), linear(rgb[1]), linear(rgb[2]), 1.0]
}

const INK: [u8; 4] = [226, 232, 240, 255];
const MUTED: [u8; 4] = [147, 161, 180, 255];
const ACCENT: [u8; 4] = [124, 215, 193, 255];

pub fn zones(snapshot: &Snapshot, width: u32, height: u32) -> Result<Panel> {
    snapshot.validate()?;
    anyhow::ensure!(
        width >= 480 && height >= 400,
        "Zones panel needs at least 480×400 pixels"
    );
    let w = width as f32;
    let h = height as f32;
    let mut nodes = vec![
        rect(0.0, 0.0, w, h, color([17, 23, 32])),
        rect(28.0, 30.0, 9.0, 34.0, color([124, 215, 193])),
        rect(28.0, 100.0, w - 56.0, 1.0, color([47, 59, 75])),
    ];
    let mut labels = vec![
        Label::new("Hearth", [50.0, 24.0, w - 220.0, 40.0], 27.0, INK).bold(),
        Label::new(
            format!("{} · shared workspace", snapshot.workspace),
            [51.0, 64.0, w - 100.0, 26.0],
            13.0,
            MUTED,
        ),
        Label::new(
            format!("{} zones", snapshot.zones.len()),
            [w - 150.0, 32.0, 120.0, 28.0],
            14.0,
            ACCENT,
        ),
    ];
    let columns = if width >= 760 { 2usize } else { 1 };
    let gap = 16.0;
    let card_w = (w - 56.0 - gap * (columns - 1) as f32) / columns as f32;
    let card_h = 230.0;
    let rows = ((h - 160.0) / (card_h + gap)).floor().max(1.0) as usize;
    let visible = snapshot.zones.len().min(columns * rows);
    for (index, zone) in snapshot.zones.iter().take(visible).enumerate() {
        let x = 28.0 + (index % columns) as f32 * (card_w + gap);
        let y = 122.0 + (index / columns) as f32 * (card_h + gap);
        nodes.push(rect(x, y, card_w, card_h, color([26, 34, 46])));
        nodes.push(rect(x, y, card_w, 2.0, color([61, 79, 99])));
        labels.push(
            Label::new(
                &zone.name,
                [x + 18.0, y + 15.0, card_w - 112.0, 28.0],
                17.0,
                INK,
            )
            .bold(),
        );
        let kind = match zone.kind {
            ZoneType::Text => "TEXT",
            ZoneType::Metric => "METRIC",
            ZoneType::Table => "TABLE",
            ZoneType::Wiring => "WIRING",
        };
        labels.push(Label::new(
            kind,
            [x + card_w - 83.0, y + 19.0, 70.0, 20.0],
            10.0,
            MUTED,
        ));
        match zone.kind {
            ZoneType::Text => labels.push(
                Label::new(
                    &zone.display,
                    [x + 18.0, y + 65.0, card_w - 36.0, 142.0],
                    20.0,
                    INK,
                )
                .wrap(),
            ),
            ZoneType::Metric => labels.push(Label::new(
                &zone.display,
                [x + 18.0, y + 65.0, card_w - 36.0, 130.0],
                64.0,
                ACCENT,
            )),
            ZoneType::Table => {
                let max_rows = 5;
                let cols = zone
                    .rows
                    .iter()
                    .map(Vec::len)
                    .max()
                    .unwrap_or(1)
                    .clamp(1, 4);
                let col_w = (card_w - 36.0) / cols as f32;
                for (row_index, row) in zone.rows.iter().take(max_rows).enumerate() {
                    let row_y = y + 57.0 + row_index as f32 * 29.0;
                    if row_index == 0 {
                        nodes.push(rect(
                            x + 12.0,
                            row_y - 3.0,
                            card_w - 24.0,
                            27.0,
                            color([37, 48, 64]),
                        ));
                    }
                    for (col, cell) in row.iter().take(cols).enumerate() {
                        let label = Label::new(
                            cell,
                            [x + 18.0 + col as f32 * col_w, row_y, col_w - 12.0, 25.0],
                            13.0,
                            if row_index == 0 { MUTED } else { INK },
                        );
                        labels.push(label);
                    }
                }
                if zone.rows.len() > max_rows || zone.rows.iter().any(|row| row.len() > cols) {
                    labels.push(Label::new(
                        "More cells outside this preview",
                        [x + 18.0, y + 202.0, card_w - 36.0, 20.0],
                        11.0,
                        MUTED,
                    ));
                }
            }
            ZoneType::Wiring => {
                for (row, edge) in zone.rows.iter().take(3).enumerate() {
                    let row_y = y + 62.0 + row as f32 * 46.0;
                    let box_w = (card_w - 88.0) / 2.0;
                    nodes.push(rect(x + 18.0, row_y, box_w, 32.0, color([37, 53, 65])));
                    nodes.push(rect(
                        x + card_w - 18.0 - box_w,
                        row_y,
                        box_w,
                        32.0,
                        color([37, 53, 65]),
                    ));
                    labels.push(Label::new(
                        &edge[0],
                        [x + 26.0, row_y + 5.0, box_w - 16.0, 25.0],
                        12.0,
                        INK,
                    ));
                    labels.push(Label::new(
                        "→",
                        [x + card_w / 2.0 - 10.0, row_y + 2.0, 24.0, 30.0],
                        21.0,
                        ACCENT,
                    ));
                    labels.push(Label::new(
                        &edge[1],
                        [x + card_w - 10.0 - box_w, row_y + 5.0, box_w - 16.0, 25.0],
                        12.0,
                        INK,
                    ));
                }
                if zone.rows.len() > 3 {
                    labels.push(Label::new(
                        format!("{} more connections", zone.rows.len() - 3),
                        [x + 18.0, y + 202.0, card_w - 36.0, 20.0],
                        11.0,
                        MUTED,
                    ));
                }
            }
        }
    }
    if snapshot.zones.is_empty() {
        labels.push(Label::new(
            "No zones published",
            [28.0, 142.0, w - 56.0, 40.0],
            24.0,
            INK,
        ));
        labels.push(Label::new(
            "Publish a zone from Weft to fill this workspace.",
            [28.0, 187.0, w - 56.0, 40.0],
            15.0,
            MUTED,
        ));
    }
    let footer = if visible < snapshot.zones.len() {
        format!(
            "Showing {visible} of {} zones · revision {}",
            snapshot.zones.len(),
            snapshot.revision
        )
    } else {
        format!("Weft · revision {}", snapshot.revision)
    };
    labels.push(Label::new(
        footer,
        [28.0, h - 32.0, w - 56.0, 24.0],
        12.0,
        MUTED,
    ));
    Ok(Panel {
        scene: Scene::new(nodes)?,
        labels,
    })
}
