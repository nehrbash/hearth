use anyhow::Result;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};
use zbus::{
    blocking::{Connection, connection::Builder},
    zvariant::{ObjectPath, OwnedValue},
};
pub const ADAPTER: &str = "/org/bluez/hci0";
pub const DEVICE: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
pub struct Bus {
    child: Child,
    pub address: String,
}
impl Bus {
    pub fn new() -> Result<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("bus stdout")).read_line(&mut address)?;
        Ok(Self {
            child,
            address: address.trim().into(),
        })
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub type Calls = Arc<Mutex<Vec<String>>>;
struct Adapter {
    discovery_caller: Mutex<String>,
    calls: Calls,
    powered: bool,
}
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn address(&self) -> &str {
        "00:11:22:33:44:00"
    }
    #[zbus(property)]
    fn alias(&self) -> &str {
        "Test adapter"
    }
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered
    }
    #[zbus(property)]
    fn set_powered(&self, value: bool) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("power:{value}"));
    }
    #[zbus(property)]
    fn discovering(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn pairable(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn discovery_caller(&self) -> String {
        self.discovery_caller.lock().expect("caller").clone()
    }
    fn start_discovery(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        *self.discovery_caller.lock().expect("caller") =
            header.sender().expect("sender").to_string();
        self.calls.lock().expect("calls").push("start".into());
    }
    fn stop_discovery(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if header.sender().expect("sender").as_str()
            != self.discovery_caller.lock().expect("caller").as_str()
        {
            return Err(zbus::fdo::Error::Failed("Wrong discovery caller".into()));
        }
        self.calls.lock().expect("calls").push("stop".into());
        Ok(())
    }
    fn remove_device(&self, path: ObjectPath<'_>) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("remove:{path}"));
    }
}
type AgentState = Arc<Mutex<Option<(String, String)>>>;
struct Device {
    agent: AgentState,
    challenge: &'static str,
    calls: Calls,
    blocked: bool,
    malformed: bool,
}
#[zbus::interface(name = "org.bluez.Device1")]
impl Device {
    #[zbus(property)]
    fn address(&self) -> &str {
        "00:11:22:33:44:55"
    }
    #[zbus(property)]
    fn alias(&self) -> &str {
        "Headphones"
    }
    #[zbus(property)]
    fn adapter(&self) -> ObjectPath<'_> {
        ObjectPath::try_from(ADAPTER).expect("adapter path")
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn connected(&self) -> OwnedValue {
        if self.malformed {
            OwnedValue::from(1u8)
        } else {
            OwnedValue::from(false)
        }
    }
    #[zbus(property)]
    fn trusted(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn set_trusted(&self, value: bool) {
        self.calls
            .lock()
            .expect("calls")
            .push(format!("trust:{value}"));
    }
    #[zbus(property)]
    fn blocked(&self) -> bool {
        self.blocked
    }
    #[zbus(property, name = "RSSI")]
    fn rssi(&self) -> i16 {
        -48
    }
    fn connect(&self) {
        self.calls.lock().expect("calls").push("connect".into());
    }
    fn disconnect(&self) {
        self.calls.lock().expect("calls").push("disconnect".into());
    }
    async fn pair(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let (destination, path) = self
            .agent
            .lock()
            .expect("agent")
            .clone()
            .ok_or_else(|| zbus::fdo::Error::Failed("No agent".into()))?;
        let proxy = zbus::Proxy::new(
            connection,
            destination.as_str(),
            path.as_str(),
            "org.bluez.Agent1",
        )
        .await?;
        let device = ObjectPath::try_from(DEVICE).expect("device path");
        match self.challenge {
            "pin" => {
                let _: String = proxy.call("RequestPinCode", &(device,)).await?;
            }
            "passkey" => {
                let _: u32 = proxy.call("RequestPasskey", &(device,)).await?;
            }
            "authorization" => {
                let _: () = proxy.call("RequestAuthorization", &(device,)).await?;
            }
            "display_pin" => {
                let _: () = proxy.call("DisplayPinCode", &(device, "123456")).await?;
            }
            "display_passkey" => {
                let _: () = proxy
                    .call("DisplayPasskey", &(device, 123456u32, 2u16))
                    .await?;
            }
            "authorize_service" => {
                let _: () = proxy
                    .call(
                        "AuthorizeService",
                        &(device, "0000110b-0000-1000-8000-00805f9b34fb"),
                    )
                    .await?;
            }
            _ => {
                let _: () = proxy
                    .call("RequestConfirmation", &(device, 123456u32))
                    .await?;
            }
        }
        self.calls.lock().expect("calls").push("pair".into());
        Ok(())
    }
    fn cancel_pairing(&self) {
        self.calls
            .lock()
            .expect("calls")
            .push("cancel_pairing".into());
    }
}
struct Manager {
    agent: AgentState,
}
#[zbus::interface(name = "org.bluez.AgentManager1")]
impl Manager {
    #[zbus(property)]
    fn agent_owner(&self) -> String {
        self.agent
            .lock()
            .expect("agent")
            .as_ref()
            .map(|a| a.0.clone())
            .unwrap_or_default()
    }
    fn register_agent(
        &self,
        path: ObjectPath<'_>,
        capability: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if capability != "KeyboardDisplay" {
            return Err(zbus::fdo::Error::InvalidArgs("capability".into()));
        }
        let sender = header
            .sender()
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("sender".into()))?;
        *self.agent.lock().expect("agent") = Some((sender.to_string(), path.to_string()));
        Ok(())
    }
    fn unregister_agent(&self, _path: ObjectPath<'_>) {
        *self.agent.lock().expect("agent") = None;
    }
}
struct Battery(u8);
#[zbus::interface(name = "org.bluez.Battery1")]
impl Battery {
    #[zbus(property)]
    fn percentage(&self) -> u8 {
        self.0
    }
}
pub fn serve(
    bus: &Bus,
    powered: bool,
    blocked: bool,
    malformed: bool,
    battery: u8,
) -> Result<(Connection, Calls)> {
    serve_challenge(bus, powered, blocked, malformed, battery, "confirmation")
}
pub fn serve_challenge(
    bus: &Bus,
    powered: bool,
    blocked: bool,
    malformed: bool,
    battery: u8,
    challenge: &'static str,
) -> Result<(Connection, Calls)> {
    let agent = Arc::new(Mutex::new(None));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = Builder::address(bus.address.as_str())?
        .name("org.bluez")?
        .serve_at("/", zbus::fdo::ObjectManager)?
        .serve_at(
            "/org/bluez",
            Manager {
                agent: agent.clone(),
            },
        )?
        .serve_at(
            ADAPTER,
            Adapter {
                discovery_caller: Mutex::new(String::new()),
                calls: calls.clone(),
                powered,
            },
        )?
        .serve_at(
            DEVICE,
            Device {
                agent,
                challenge,
                calls: calls.clone(),
                blocked,
                malformed,
            },
        )?
        .serve_at(DEVICE, Battery(battery))?
        .build()?;
    Ok((server, calls))
}
