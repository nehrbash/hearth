use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::OwnedValue,
};

const SERVICE: &str = "org.freedesktop.UPower";
const ROOT: &str = "/org/freedesktop/UPower";
const DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE: &str = "org.freedesktop.UPower.Device";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpowerSnapshot {
    pub on_battery: bool,
    pub display: DisplayState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "presence", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayState {
    Absent,
    Present {
        kind: SupplyKind,
        raw_kind: u32,
        state: PowerState,
        raw_state: u32,
        charge: Charge,
        time_to_empty_seconds: Option<u64>,
        time_to_full_seconds: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupplyKind {
    Battery,
    Ups,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    Unknown,
    Charging,
    Discharging,
    Empty,
    FullyCharged,
    PendingCharge,
    PendingDischarge,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoarseLevel {
    Low,
    Critical,
    Normal,
    High,
    Full,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Charge {
    Percentage { value: f64 },
    Coarse { level: CoarseLevel },
    Unknown { raw_level: u32 },
}

type Properties = HashMap<String, OwnedValue>;

fn property<'a, T>(values: &'a Properties, name: &str) -> Result<T>
where
    T: TryFrom<&'a OwnedValue>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    T::try_from(
        values
            .get(name)
            .with_context(|| format!("UPower property {name} is missing"))?,
    )
    .with_context(|| format!("UPower property {name} has the wrong type"))
}
fn remaining(values: &Properties, name: &str) -> Result<Option<u64>> {
    let seconds: i64 = property(values, name)?;
    ensure!(seconds >= 0, "UPower {name} must be nonnegative");
    Ok((seconds > 0).then_some(seconds as u64))
}
fn decode(root: &Properties, display: &Properties) -> Result<UpowerSnapshot> {
    let on_battery = property(root, "OnBattery")?;
    if !property::<bool>(display, "IsPresent")? {
        return Ok(UpowerSnapshot {
            on_battery,
            display: DisplayState::Absent,
        });
    }
    let raw_kind = property(display, "Type")?;
    let kind = match raw_kind {
        2 => SupplyKind::Battery,
        3 => SupplyKind::Ups,
        _ => SupplyKind::Unknown,
    };
    let raw_state = property(display, "State")?;
    let state = match raw_state {
        1 => PowerState::Charging,
        2 => PowerState::Discharging,
        3 => PowerState::Empty,
        4 => PowerState::FullyCharged,
        5 => PowerState::PendingCharge,
        6 => PowerState::PendingDischarge,
        _ => PowerState::Unknown,
    };
    let raw_level = if display.contains_key("BatteryLevel") {
        property(display, "BatteryLevel")?
    } else {
        1
    };
    let charge = match raw_level {
        1 => {
            let value: f64 = property(display, "Percentage")?;
            ensure!(
                value.is_finite() && (0.0..=100.0).contains(&value),
                "UPower percentage must be finite and within 0..100"
            );
            Charge::Percentage { value }
        }
        3 => Charge::Coarse {
            level: CoarseLevel::Low,
        },
        4 => Charge::Coarse {
            level: CoarseLevel::Critical,
        },
        6 => Charge::Coarse {
            level: CoarseLevel::Normal,
        },
        7 => Charge::Coarse {
            level: CoarseLevel::High,
        },
        8 => Charge::Coarse {
            level: CoarseLevel::Full,
        },
        raw_level => Charge::Unknown { raw_level },
    };
    Ok(UpowerSnapshot {
        on_battery,
        display: DisplayState::Present {
            kind,
            raw_kind,
            state,
            raw_state,
            charge,
            time_to_empty_seconds: remaining(display, "TimeToEmpty")?,
            time_to_full_seconds: remaining(display, "TimeToFull")?,
        },
    })
}

/// Inspect the running daemon without activating an absent service.
pub fn inspect() -> Result<UpowerSnapshot> {
    let connection = Builder::system()?
        .method_timeout(Duration::from_millis(400))
        .build()?;
    inspect_connection(&connection)
}

/// Inspect an existing connection, including a private test bus.
pub fn inspect_connection(connection: &Connection) -> Result<UpowerSnapshot> {
    let bus = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owner: String = bus
        .call("GetNameOwner", &(SERVICE,))
        .context("UPower daemon is not running")?;
    // Unique destinations cannot activate a replacement daemon between property reads.
    let get_all = |path| -> Result<Properties> {
        let proxy = Proxy::new(
            connection,
            owner.as_str(),
            path,
            "org.freedesktop.DBus.Properties",
        )?;
        Ok(proxy.call("GetAll", &(if path == ROOT { SERVICE } else { DEVICE },))?)
    };
    let root = get_all(ROOT).context("Read UPower system properties")?;
    let display = get_all(DISPLAY).context("Read UPower display device")?;
    let after: String = bus
        .call("GetNameOwner", &(SERVICE,))
        .context("UPower daemon stopped during inspection")?;
    ensure!(owner == after, "UPower daemon changed during inspection");
    decode(&root, &display)
}
