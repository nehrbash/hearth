use anyhow::Result;
use hearth::upower::{self, Charge, CoarseLevel, DisplayState, PowerState, SupplyKind};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Mutex, mpsc},
    time::Duration,
};
use zbus::blocking::{Connection, connection::Builder};

const ROOT: &str = "/org/freedesktop/UPower";
const DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const SERVICE: &str = "org.freedesktop.UPower";
struct Bus {
    child: Child,
    address: String,
}
impl Bus {
    fn new() -> Result<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let result = (|| -> Result<String> {
            let mut address = String::new();
            BufReader::new(child.stdout.take().expect("bus stdout")).read_line(&mut address)?;
            anyhow::ensure!(
                !address.trim().is_empty(),
                "Private bus did not report its address"
            );
            Ok(address.trim().to_owned())
        })();
        match result {
            Ok(address) => Ok(Self { child, address }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
    fn connection(&self) -> Result<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_millis(400))
            .build()?)
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct System {
    on_battery: bool,
    owner_loss: Option<Mutex<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
}
#[zbus::interface(name = "org.freedesktop.UPower")]
impl System {
    #[zbus(property)]
    fn on_battery(&self) -> bool {
        if let Some(channels) = &self.owner_loss {
            let (send, recv) = &*channels.lock().expect("owner loss mutex");
            send.send(()).expect("owner loss worker");
            recv.recv_timeout(Duration::from_secs(1))
                .expect("owner released");
        }
        self.on_battery
    }
}
#[derive(Clone)]
struct Device {
    present: bool,
    kind: u32,
    state: u32,
    level: u32,
    percentage: f64,
    empty: i64,
    full: i64,
}
impl Default for Device {
    fn default() -> Self {
        Self {
            present: true,
            kind: 2,
            state: 2,
            level: 1,
            percentage: 62.5,
            empty: 3600,
            full: 0,
        }
    }
}
#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl Device {
    #[zbus(property)]
    fn is_present(&self) -> bool {
        self.present
    }
    #[zbus(property, name = "Type")]
    fn kind(&self) -> u32 {
        self.kind
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn battery_level(&self) -> u32 {
        self.level
    }
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.percentage
    }
    #[zbus(property)]
    fn time_to_empty(&self) -> i64 {
        self.empty
    }
    #[zbus(property)]
    fn time_to_full(&self) -> i64 {
        self.full
    }
}
fn serve(bus: &Bus, device: Device) -> Result<Connection> {
    Ok(Builder::address(bus.address.as_str())?
        .name(SERVICE)?
        .serve_at(
            ROOT,
            System {
                on_battery: true,
                owner_loss: None,
            },
        )?
        .serve_at(DISPLAY, device)?
        .build()?)
}

#[test]
fn absent_battery_and_missing_daemon_are_different() -> Result<()> {
    let bus = Bus::new()?;
    let client = bus.connection()?;
    assert!(upower::inspect_connection(&client).is_err());
    let server = serve(
        &bus,
        Device {
            present: false,
            ..Default::default()
        },
    )?;
    let snapshot = upower::inspect_connection(&client)?;
    assert!(snapshot.on_battery);
    assert_eq!(snapshot.display, DisplayState::Absent);
    assert_eq!(
        serde_json::to_value(&snapshot)?["display"],
        serde_json::json!({"presence":"absent"})
    );
    server.close()?;
    assert!(upower::inspect_connection(&client).is_err());
    Ok(())
}

#[test]
fn percentage_charge_state_and_unknown_times_roundtrip() -> Result<()> {
    let bus = Bus::new()?;
    let _server = serve(&bus, Device::default())?;
    let snapshot = upower::inspect_connection(&bus.connection()?)?;
    assert_eq!(
        snapshot.display,
        DisplayState::Present {
            kind: SupplyKind::Battery,
            raw_kind: 2,
            state: PowerState::Discharging,
            raw_state: 2,
            charge: Charge::Percentage { value: 62.5 },
            time_to_empty_seconds: Some(3600),
            time_to_full_seconds: None
        }
    );
    assert_eq!(
        serde_json::from_slice::<upower::UpowerSnapshot>(&serde_json::to_vec(&snapshot)?)?,
        snapshot
    );
    Ok(())
}

#[test]
fn coarse_level_takes_precedence_and_future_enums_remain_unknown() -> Result<()> {
    for (level, charge) in [
        (
            3,
            Charge::Coarse {
                level: CoarseLevel::Low,
            },
        ),
        (
            8,
            Charge::Coarse {
                level: CoarseLevel::Full,
            },
        ),
        (99, Charge::Unknown { raw_level: 99 }),
    ] {
        let bus = Bus::new()?;
        let _server = serve(
            &bus,
            Device {
                kind: 99,
                state: 99,
                level,
                percentage: f64::NAN,
                ..Default::default()
            },
        )?;
        let snapshot = upower::inspect_connection(&bus.connection()?)?;
        let DisplayState::Present {
            kind,
            raw_kind,
            state,
            raw_state,
            charge: actual,
            ..
        } = snapshot.display
        else {
            panic!("present")
        };
        assert_eq!(
            (kind, raw_kind, state, raw_state),
            (SupplyKind::Unknown, 99, PowerState::Unknown, 99)
        );
        assert_eq!(actual, charge);
    }
    Ok(())
}

#[test]
fn invalid_percentages_and_negative_times_are_rejected() -> Result<()> {
    for device in [
        Device {
            percentage: f64::NAN,
            ..Default::default()
        },
        Device {
            percentage: 101.0,
            ..Default::default()
        },
        Device {
            empty: -1,
            ..Default::default()
        },
    ] {
        let bus = Bus::new()?;
        let _server = serve(&bus, device)?;
        assert!(upower::inspect_connection(&bus.connection()?).is_err());
    }
    Ok(())
}

#[test]
fn owner_loss_between_property_reads_rejects_snapshot() -> Result<()> {
    let bus = Bus::new()?;
    let (request, requests) = mpsc::sync_channel(1);
    let (ack, acknowledged) = mpsc::sync_channel(1);
    let server = Builder::address(bus.address.as_str())?
        .name(SERVICE)?
        .serve_at(
            ROOT,
            System {
                on_battery: true,
                owner_loss: Some(Mutex::new((request, acknowledged))),
            },
        )?
        .serve_at(DISPLAY, Device::default())?
        .build()?;
    let worker = std::thread::spawn(move || -> Result<()> {
        requests.recv_timeout(Duration::from_secs(2))?;
        server.release_name(SERVICE)?;
        ack.send(())?;
        Ok(())
    });
    assert!(upower::inspect_connection(&bus.connection()?).is_err());
    worker.join().expect("owner loss thread")?;
    Ok(())
}
