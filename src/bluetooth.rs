use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue},
};
const SERVICE: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
type Properties = HashMap<String, OwnedValue>;
type Objects = HashMap<OwnedObjectPath, HashMap<String, Properties>>;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BluetoothSnapshot {
    pub owner: String,
    pub adapters: Vec<Adapter>,
    pub devices: Vec<Device>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Adapter {
    pub path: String,
    pub address: String,
    pub name: Option<String>,
    pub alias: String,
    pub powered: bool,
    pub discovering: bool,
    pub pairable: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub path: String,
    pub adapter: String,
    pub address: String,
    pub name: Option<String>,
    pub alias: String,
    pub icon: Option<String>,
    pub connected: bool,
    pub paired: bool,
    pub trusted: bool,
    pub blocked: bool,
    pub rssi: Option<i16>,
    pub battery_percentage: Option<u8>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub owner: String,
    pub path: String,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Power { value: bool },
    Discovery { value: bool },
    Pair,
    Connect,
    Disconnect,
    Trust { value: bool },
    Remove,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOutcome {
    Executed,
    NeedsInteractiveAgent,
    NeedsPersistentSession,
}
fn prop<'a, T>(p: &'a Properties, key: &str) -> Result<T>
where
    T: TryFrom<&'a OwnedValue>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    T::try_from(p.get(key).with_context(|| format!("Missing BlueZ {key}"))?)
        .with_context(|| format!("Invalid BlueZ {key}"))
}
fn text(p: &Properties, key: &str) -> Result<String> {
    let s: &str = prop(p, key)?;
    ensure!(s.len() <= 4096, "BlueZ {key} exceeds 4096 bytes");
    Ok(s.into())
}
fn optional_text(p: &Properties, key: &str) -> Result<Option<String>> {
    p.contains_key(key).then(|| text(p, key)).transpose()
}
fn valid_path(path: &str) -> Result<()> {
    ensure!(
        path.len() <= 4096 && path.starts_with("/org/bluez/"),
        "Invalid BlueZ object path"
    );
    ObjectPath::try_from(path)?;
    Ok(())
}
fn current_owner(connection: &Connection) -> Result<String> {
    Ok(Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?
    .call("GetNameOwner", &(SERVICE,))?)
}
/// Keep this connection alive while discovery is active; BlueZ owns scans per caller.
pub struct BluetoothClient {
    connection: Connection,
}
impl BluetoothClient {
    pub fn system() -> Result<Self> {
        Ok(Self {
            connection: Builder::system()?
                .method_timeout(Duration::from_millis(400))
                .build()?,
        })
    }
    pub fn with_address(address: &str) -> Result<Self> {
        Ok(Self {
            connection: Builder::address(address)?
                .method_timeout(Duration::from_millis(400))
                .build()?,
        })
    }
    pub fn inspect(&self) -> Result<BluetoothSnapshot> {
        let owner = current_owner(&self.connection)?;
        let objects: Objects = Proxy::new(
            &self.connection,
            owner.as_str(),
            "/",
            "org.freedesktop.DBus.ObjectManager",
        )?
        .call("GetManagedObjects", &())?;
        ensure!(
            objects.len() <= 4096,
            "BlueZ object inventory exceeds 4096 objects"
        );
        let mut adapters = Vec::new();
        let mut devices = Vec::new();
        for (path, interfaces) in objects {
            let path = path.to_string();
            if let Some(p) = interfaces.get(ADAPTER) {
                valid_path(&path)?;
                adapters.push(Adapter {
                    path: path.clone(),
                    address: text(p, "Address")?,
                    name: optional_text(p, "Name")?,
                    alias: text(p, "Alias")?,
                    powered: prop(p, "Powered")?,
                    discovering: prop(p, "Discovering")?,
                    pairable: prop(p, "Pairable")?,
                });
            }
            if let Some(p) = interfaces.get(DEVICE) {
                valid_path(&path)?;
                let adapter: ObjectPath<'_> = p
                    .get("Adapter")
                    .context("Missing BlueZ Adapter")?
                    .try_clone()?
                    .try_into()?;
                valid_path(adapter.as_str())?;
                let battery_percentage = interfaces
                    .get("org.bluez.Battery1")
                    .map(|p| prop::<u8>(p, "Percentage"))
                    .transpose()?;
                ensure!(
                    battery_percentage.is_none_or(|n| n <= 100),
                    "Invalid Bluetooth battery percentage"
                );
                devices.push(Device {
                    path,
                    adapter: adapter.to_string(),
                    address: text(p, "Address")?,
                    name: optional_text(p, "Name")?,
                    alias: text(p, "Alias")?,
                    icon: optional_text(p, "Icon")?,
                    connected: prop(p, "Connected")?,
                    paired: prop(p, "Paired")?,
                    trusted: prop(p, "Trusted")?,
                    blocked: prop(p, "Blocked")?,
                    rssi: p
                        .contains_key("RSSI")
                        .then(|| prop(p, "RSSI"))
                        .transpose()?,
                    battery_percentage,
                });
            }
        }
        ensure!(
            adapters.len() <= 16 && devices.len() <= 256,
            "Bluetooth inventory exceeds adapter/device bounds"
        );
        for d in &devices {
            ensure!(
                adapters.iter().any(|a| a.path == d.adapter),
                "Bluetooth device references missing adapter"
            );
        }
        adapters.sort_by(|a, b| a.path.cmp(&b.path));
        devices.sort_by(|a, b| a.path.cmp(&b.path));
        ensure!(
            current_owner(&self.connection)? == owner,
            "BlueZ owner changed during inspection"
        );
        let snapshot = BluetoothSnapshot {
            owner,
            adapters,
            devices,
        };
        ensure!(
            serde_json::to_vec(&snapshot)?.len() <= 120 * 1024,
            "Bluetooth snapshot exceeds 120 KiB"
        );
        Ok(snapshot)
    }
    pub fn execute(&self, request: &ActionRequest) -> Result<ActionOutcome> {
        valid_path(&request.path)?;
        ensure!(
            request.owner.starts_with(':') && request.owner.len() <= 255,
            "Invalid BlueZ owner"
        );
        let snapshot = self.inspect()?;
        ensure!(snapshot.owner == request.owner, "BlueZ owner changed");
        match &request.action {
            Action::Power { value } | Action::Discovery { value } => {
                let adapter = snapshot
                    .adapters
                    .iter()
                    .find(|a| a.path == request.path)
                    .context("Bluetooth adapter not found")?;
                let proxy = Proxy::new(
                    &self.connection,
                    request.owner.as_str(),
                    request.path.as_str(),
                    ADAPTER,
                )?;
                match request.action {
                    Action::Power { .. } => proxy.set_property("Powered", value)?,
                    _ => {
                        ensure!(adapter.powered, "Bluetooth adapter is powered off");
                        let _: () = proxy.call(
                            if *value {
                                "StartDiscovery"
                            } else {
                                "StopDiscovery"
                            },
                            &(),
                        )?;
                    }
                }
            }
            action => {
                let device = snapshot
                    .devices
                    .iter()
                    .find(|d| d.path == request.path)
                    .context("Bluetooth device not found")?;
                let adapter = snapshot
                    .adapters
                    .iter()
                    .find(|a| a.path == device.adapter)
                    .context("Bluetooth adapter not found")?;
                let proxy = Proxy::new(
                    &self.connection,
                    request.owner.as_str(),
                    request.path.as_str(),
                    DEVICE,
                )?;
                match action {
                    Action::Pair => return Ok(ActionOutcome::NeedsInteractiveAgent),
                    Action::Connect => {
                        ensure!(
                            adapter.powered && !device.blocked,
                            "Bluetooth device is blocked or adapter powered off"
                        );
                        let _: () = proxy.call("Connect", &())?;
                    }
                    Action::Disconnect => {
                        let _: () = proxy.call("Disconnect", &())?;
                    }
                    Action::Trust { value } => proxy.set_property("Trusted", value)?,
                    Action::Remove => {
                        let path = ObjectPath::try_from(request.path.as_str())?;
                        let _: () = Proxy::new(
                            &self.connection,
                            request.owner.as_str(),
                            device.adapter.as_str(),
                            ADAPTER,
                        )?
                        .call("RemoveDevice", &(path,))?;
                    }
                    Action::Power { .. } | Action::Discovery { .. } => unreachable!(),
                }
            }
        }
        Ok(ActionOutcome::Executed)
    }
}
pub fn inspect() -> Result<BluetoothSnapshot> {
    BluetoothClient::system()?.inspect()
}
pub fn inspect_with_address(address: &str) -> Result<BluetoothSnapshot> {
    BluetoothClient::with_address(address)?.inspect()
}
pub fn execute(request: &ActionRequest) -> Result<ActionOutcome> {
    if matches!(request.action, Action::Discovery { .. }) {
        return Ok(ActionOutcome::NeedsPersistentSession);
    }
    BluetoothClient::system()?.execute(request)
}
pub fn execute_with_address(address: &str, request: &ActionRequest) -> Result<ActionOutcome> {
    if matches!(request.action, Action::Discovery { .. }) {
        return Ok(ActionOutcome::NeedsPersistentSession);
    }
    BluetoothClient::with_address(address)?.execute(request)
}
