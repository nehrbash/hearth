use anyhow::Result;
use hearth::bluetooth::{self, Action, ActionOutcome, ActionRequest, BluetoothClient};
use zbus::blocking::connection::Builder;
#[path = "support/bluetooth.rs"]
mod support;
use support::*;
#[test]
fn inventory_and_persistent_discovery_actions() -> Result<()> {
    let bus = Bus::new()?;
    assert!(bluetooth::inspect_with_address(&bus.address).is_err());
    let (_server, calls) = serve(&bus, true, false, false, 73)?;
    let client = BluetoothClient::with_address(&bus.address)?;
    let snapshot = client.inspect()?;
    assert_eq!(snapshot.adapters.len(), 1);
    assert_eq!(snapshot.devices[0].battery_percentage, Some(73));
    assert_eq!(snapshot.devices[0].rssi, Some(-48));
    assert_eq!(snapshot.devices[0].name, None);
    for (path, action) in [
        (ADAPTER, Action::Power { value: false }),
        (ADAPTER, Action::Discovery { value: true }),
        (ADAPTER, Action::Discovery { value: false }),
        (DEVICE, Action::Connect),
        (DEVICE, Action::Disconnect),
        (DEVICE, Action::Trust { value: true }),
        (DEVICE, Action::Remove),
    ] {
        assert_eq!(
            client.execute(&ActionRequest {
                owner: snapshot.owner.clone(),
                path: path.into(),
                action
            })?,
            ActionOutcome::Executed
        );
    }
    assert_eq!(
        *calls.lock().expect("calls"),
        [
            "power:false",
            "start",
            "stop",
            "connect",
            "disconnect",
            "trust:true",
            &format!("remove:{DEVICE}")
        ]
    );
    assert_eq!(
        bluetooth::execute_with_address(
            &bus.address,
            &ActionRequest {
                owner: snapshot.owner.clone(),
                path: ADAPTER.into(),
                action: Action::Discovery { value: true }
            }
        )?,
        ActionOutcome::NeedsPersistentSession
    );
    assert_eq!(
        client.execute(&ActionRequest {
            owner: snapshot.owner,
            path: DEVICE.into(),
            action: Action::Pair
        })?,
        ActionOutcome::NeedsInteractiveAgent
    );
    assert_eq!(calls.lock().expect("calls").len(), 7);
    Ok(())
}
#[test]
fn owner_replacement_paths_and_disabled_capabilities_reject() -> Result<()> {
    let bus = Bus::new()?;
    let (server, calls) = serve(&bus, false, true, false, 100)?;
    let client = BluetoothClient::with_address(&bus.address)?;
    let snapshot = client.inspect()?;
    for (path, action) in [
        (ADAPTER, Action::Discovery { value: true }),
        (DEVICE, Action::Connect),
        ("/invalid", Action::Remove),
        (ADAPTER, Action::Connect),
        (DEVICE, Action::Power { value: true }),
    ] {
        assert!(
            client
                .execute(&ActionRequest {
                    owner: snapshot.owner.clone(),
                    path: path.into(),
                    action
                })
                .is_err()
        );
    }
    assert!(calls.lock().expect("calls").is_empty());
    server.release_name("org.bluez")?;
    let (_replacement, replacement_calls) = serve(&bus, true, false, false, 10)?;
    assert!(
        client
            .execute(&ActionRequest {
                owner: snapshot.owner,
                path: DEVICE.into(),
                action: Action::Connect
            })
            .is_err()
    );
    assert!(replacement_calls.lock().expect("calls").is_empty());
    Ok(())
}
#[test]
fn malformed_state_and_battery_bounds_reject() -> Result<()> {
    for (malformed, battery) in [(true, 40), (false, 101)] {
        let bus = Bus::new()?;
        let (_server, _) = serve(&bus, true, false, malformed, battery)?;
        assert!(bluetooth::inspect_with_address(&bus.address).is_err());
    }
    Ok(())
}
#[test]
fn running_daemon_without_adapters_is_available() -> Result<()> {
    let bus = Bus::new()?;
    let _server = Builder::address(bus.address.as_str())?
        .name("org.bluez")?
        .serve_at("/", zbus::fdo::ObjectManager)?
        .build()?;
    let snapshot = bluetooth::inspect_with_address(&bus.address)?;
    assert!(snapshot.adapters.is_empty() && snapshot.devices.is_empty());
    Ok(())
}
