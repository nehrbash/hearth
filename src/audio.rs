use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioSnapshot {
    pub session: String,
    pub core_cookie: u32,
    pub devices: Vec<Device>,
    pub nodes: Vec<Node>,
    pub defaults: Defaults,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: u32,
    pub serial: Option<u64>,
    pub name: String,
    pub description: Option<String>,
    pub api: Option<String>,
    #[serde(default)]
    pub bluetooth_address: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Sink,
    Source,
    Playback,
    Capture,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u32,
    pub serial: Option<u64>,
    pub device_id: Option<u32>,
    #[serde(default)]
    pub bluetooth_address: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub description: Option<String>,
    pub application_name: Option<String>,
    pub state: String,
    pub ready: bool,
    pub channels: Vec<u32>,
    pub channel_volumes_linear: Option<Vec<f32>>,
    pub mute: Option<bool>,
    pub capabilities: Capabilities,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub volume: bool,
    pub mute: bool,
    pub default: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Defaults {
    pub sink: Option<String>,
    pub source: Option<String>,
    pub configured_sink: Option<String>,
    pub configured_source: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub session: String,
    pub id: u32,
    pub serial: u64,
    pub action: Action,
    #[serde(default)]
    pub expected_bluetooth_address: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Volume {
        value: f32,
        #[serde(default)]
        unmute: bool,
    },
    Mute {
        value: bool,
    },
    Default,
}
impl AudioSnapshot {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.session.is_empty() && self.session.len() <= 128,
            "Invalid PipeWire session"
        );
        ensure!(
            self.devices.len() <= 64 && self.nodes.len() <= 256,
            "PipeWire inventory exceeds bounds"
        );
        let mut ids = std::collections::HashSet::new();
        let valid = |s: &str| s.len() <= 4096 && !s.contains('\0');
        for d in &self.devices {
            ensure!(
                d.bluetooth_address
                    .as_deref()
                    .is_none_or(valid_bluetooth_address),
                "Invalid Bluetooth address"
            );
            ensure!(ids.insert(d.id), "Duplicate PipeWire object ID");
            ensure!(
                valid(&d.name)
                    && d.description.as_deref().is_none_or(valid)
                    && d.api.as_deref().is_none_or(valid),
                "Invalid device text"
            );
        }
        for n in &self.nodes {
            ensure!(
                n.bluetooth_address
                    .as_deref()
                    .is_none_or(valid_bluetooth_address),
                "Invalid Bluetooth address"
            );
            ensure!(ids.insert(n.id), "Duplicate PipeWire object ID");
            ensure!(
                ["creating", "suspended", "idle", "running", "error"].contains(&n.state.as_str()),
                "Invalid PipeWire node state"
            );
            ensure!(
                !n.capabilities.volume
                    || (n.serial.is_some() && n.channel_volumes_linear.is_some()),
                "Volume capability without observed values"
            );
            ensure!(
                !n.capabilities.mute || (n.serial.is_some() && n.mute.is_some()),
                "Mute capability without observed value"
            );
            ensure!(
                !n.capabilities.default
                    || (n.serial.is_some() && matches!(n.kind, Kind::Sink | Kind::Source)),
                "Invalid default capability"
            );
            ensure!(
                valid(&n.name)
                    && n.description.as_deref().is_none_or(valid)
                    && n.application_name.as_deref().is_none_or(valid),
                "Invalid node text"
            );
            ensure!(n.channels.len() <= 64, "Too many audio channels");
            if let Some(v) = &n.channel_volumes_linear {
                validate_volumes(v)?;
                ensure!(
                    n.channels.is_empty() || n.channels.len() == v.len(),
                    "Channel map/volume length mismatch"
                );
            }
        }
        for name in [
            &self.defaults.sink,
            &self.defaults.source,
            &self.defaults.configured_sink,
            &self.defaults.configured_source,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(valid(name), "Invalid default node name");
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= 120 * 1024,
            "PipeWire state exceeds120KiB"
        );
        Ok(())
    }
    pub fn target(&self, request: &ActionRequest) -> Result<&Node> {
        ensure!(self.session == request.session, "PipeWire session changed");
        let node = self
            .nodes
            .iter()
            .find(|n| n.id == request.id && n.serial == Some(request.serial))
            .context("PipeWire node identity changed")?;
        if let Some(expected) = &request.expected_bluetooth_address {
            ensure!(
                valid_bluetooth_address(expected)
                    && node.bluetooth_address.as_ref() == Some(expected),
                "Bluetooth audio association changed"
            );
        }
        ensure!(node.ready, "PipeWire node is not ready");
        match request.action {
            Action::Volume { value, unmute } => {
                ensure!(
                    value.is_finite() && (0.0..=1.0).contains(&value),
                    "Invalid perceptual volume"
                );
                ensure!(
                    node.capabilities.volume && (!unmute || node.capabilities.mute),
                    "Volume control unavailable"
                );
            }
            Action::Mute { .. } => ensure!(node.capabilities.mute, "Mute control unavailable"),
            Action::Default => ensure!(
                node.capabilities.default && matches!(node.kind, Kind::Sink | Kind::Source),
                "Default selection unavailable"
            ),
        };
        Ok(node)
    }
}
pub fn validate_volumes(values: &[f32]) -> Result<()> {
    ensure!(
        !values.is_empty()
            && values.len() <= 64
            && values
                .iter()
                .all(|v| v.is_finite() && (0.0..=64.0).contains(v)),
        "Invalid channel volumes"
    );
    Ok(())
}
pub fn scaled_volumes(current: &[f32], target: f32) -> Result<Vec<f32>> {
    validate_volumes(current)?;
    ensure!(
        target.is_finite() && (0.0..=1.0).contains(&target),
        "Invalid perceptual volume"
    );
    let mean = current.iter().map(|v| v.cbrt()).sum::<f32>() / current.len() as f32;
    let result: Vec<_> = current
        .iter()
        .map(|v| {
            if mean == 0.0 {
                target.powi(3)
            } else {
                (v.cbrt() * target / mean).powi(3)
            }
        })
        .collect();
    validate_volumes(&result)?;
    Ok(result)
}

pub fn bluetooth_address(value: &str) -> Option<String> {
    if value.len() != 17
        || !value.bytes().enumerate().all(|(i, b)| {
            if i % 3 == 2 {
                b == b':'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    Some(value.to_ascii_uppercase())
}
fn valid_bluetooth_address(value: &str) -> bool {
    bluetooth_address(value).as_deref() == Some(value)
}
