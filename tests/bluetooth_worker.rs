use anyhow::{Result, ensure};
use hearth::bluetooth;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
#[path = "support/bluetooth.rs"]
mod support;
use support::*;
struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    events: Receiver<Value>,
}
impl Worker {
    fn new(bus: &Bus) -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hearth-bluetooth"))
            .args(["--address", &bus.address])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout");
        let (tx, events) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    return;
                };
                let Ok(value) = serde_json::from_str(&line) else {
                    return;
                };
                if tx.send(value).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            input,
            events,
        })
    }
    fn send(&mut self, value: Value) -> Result<()> {
        let input = self.input.as_mut().expect("stdin");
        serde_json::to_writer(&mut *input, &value)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }
    fn event(&self, kind: &str) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let value = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            if value["type"] == kind || value["service"] == kind {
                return Ok(value);
            }
        }
    }
    fn exited(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "Worker exited unsuccessfully: {status}");
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "Worker did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn pair(worker: &mut Worker, owner: &str, id: &str) -> Result<()> {
    worker.send(json!({"type":"action","request_id":id,"request":{"owner":owner,"path":DEVICE,"action":{"action":"pair"}}}))
}
#[test]
fn explicit_pairing_replies_cover_all_agent_challenges() -> Result<()> {
    for (kind, reply) in [
        ("confirmation", json!({"kind":"accept"})),
        ("authorization", json!({"kind":"accept"})),
        ("pin", json!({"kind":"pin","value":"aB12"})),
        ("passkey", json!({"kind":"passkey","value":123456})),
        ("display_pin", json!({"kind":"accept"})),
        ("display_passkey", json!({"kind":"accept"})),
        ("authorize_service", json!({"kind":"accept"})),
    ] {
        let bus = Bus::new()?;
        let (_server, calls) = serve_challenge(&bus, true, false, false, 60, kind)?;
        let mut worker = Worker::new(&bus)?;
        let state = worker.event("bluetooth")?;
        let owner = state["state"]["owner"].as_str().expect("owner");
        pair(&mut worker, owner, "pair-1")?;
        let challenge = worker.event("challenge")?;
        assert_eq!(challenge["kind"], kind);
        assert_eq!(challenge["request_id"], "pair-1");
        assert_eq!(challenge["device"], DEVICE);
        assert!(!calls.lock().expect("calls").iter().any(|c| c == "pair"));
        worker
            .send(json!({"type":"reply","challenge_id":challenge["challenge_id"],"reply":reply}))?;
        let result = worker.event("result")?;
        assert_eq!(result["status"], "executed");
        assert_eq!(result["request_id"], "pair-1");
        assert!(calls.lock().expect("calls").iter().any(|c| c == "pair"));
        worker.input.take();
        worker.exited()?;
    }
    Ok(())
}
#[test]
fn stale_reply_rejection_and_cancel_never_accept_pairing() -> Result<()> {
    let bus = Bus::new()?;
    let (_server, calls) = serve(&bus, true, false, false, 50)?;
    let mut worker = Worker::new(&bus)?;
    let state = worker.event("bluetooth")?;
    pair(
        &mut worker,
        state["state"]["owner"].as_str().expect("owner"),
        "pair-2",
    )?;
    let challenge = worker.event("challenge")?;
    worker.send(json!({"type":"reply","challenge_id":challenge["challenge_id"].as_u64().expect("id")+100,"reply":{"kind":"accept"}}))?;
    assert_eq!(worker.event("reply_rejected")?["type"], "reply_rejected");
    worker.send(json!({"type":"cancel"}))?;
    assert_eq!(worker.event("result")?["status"], "rejected");
    assert!(!calls.lock().expect("calls").iter().any(|c| c == "pair"));
    worker.input.take();
    worker.exited()?;
    Ok(())
}
#[test]
fn owner_loss_and_eof_cancel_pending_pairing() -> Result<()> {
    for owner_loss in [false, true] {
        let bus = Bus::new()?;
        let (server, calls) = serve(&bus, true, false, false, 50)?;
        let mut worker = Worker::new(&bus)?;
        let state = worker.event("bluetooth")?;
        pair(
            &mut worker,
            state["state"]["owner"].as_str().expect("owner"),
            "pair-3",
        )?;
        worker.event("challenge")?;
        if owner_loss {
            server.release_name("org.bluez")?;
            assert_eq!(worker.event("result")?["status"], "rejected");
        }
        worker.input.take();
        worker.exited()?;
        assert!(!calls.lock().expect("calls").iter().any(|c| c == "pair"));
    }
    Ok(())
}
#[test]
fn discovery_remains_on_worker_connection_and_readonly_startup() -> Result<()> {
    let bus = Bus::new()?;
    let (_server, calls) = serve(&bus, true, false, false, 50)?;
    let mut worker = Worker::new(&bus)?;
    let state = worker.event("bluetooth")?;
    assert!(calls.lock().expect("calls").is_empty());
    let owner = state["state"]["owner"].as_str().expect("owner");
    for (value, id) in [(true, "scan"), (false, "stop")] {
        worker.send(json!({"type":"action","request_id":id,"request":{"owner":owner,"path":ADAPTER,"action":{"action":"discovery","value":value}}}))?;
        assert_eq!(worker.event("result")?["status"], "executed");
    }
    assert_eq!(*calls.lock().expect("calls"), ["start", "stop"]);
    let client = zbus::blocking::connection::Builder::address(bus.address.as_str())?.build()?;
    let adapter = zbus::blocking::Proxy::new(&client, owner, ADAPTER, "org.bluez.Adapter1")?;
    let caller: String = adapter.get_property("DiscoveryCaller")?;
    let bus_proxy = zbus::blocking::Proxy::new(
        &client,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let present: bool = bus_proxy.call("NameHasOwner", &(caller.as_str(),))?;
    assert!(present);
    worker.input.take();
    worker.exited()?;
    let present: bool = bus_proxy.call("NameHasOwner", &(caller.as_str(),))?;
    assert!(!present);
    assert!(
        !bluetooth::inspect_with_address(&bus.address)?
            .devices
            .is_empty()
    );
    Ok(())
}

#[test]
fn agent_rejects_other_senders_and_devices() -> Result<()> {
    use zbus::{
        blocking::{Proxy, connection::Builder},
        zvariant::ObjectPath,
    };
    let bus = Bus::new()?;
    let (server, calls) = serve(&bus, true, false, false, 50)?;
    let mut worker = Worker::new(&bus)?;
    let state = worker.event("bluetooth")?;
    let owner = state["state"]["owner"].as_str().expect("owner");
    pair(&mut worker, owner, "pair-security")?;
    let challenge = worker.event("challenge")?;
    let manager = Proxy::new(&server, owner, "/org/bluez", "org.bluez.AgentManager1")?;
    let agent_owner: String = manager.get_property("AgentOwner")?;
    let other = Builder::address(bus.address.as_str())?.build()?;
    let impostor = Proxy::new(
        &other,
        agent_owner.as_str(),
        "/org/hearth/BluetoothAgent",
        "org.bluez.Agent1",
    )?;
    let rejected: zbus::Result<()> = impostor.call(
        "RequestConfirmation",
        &(ObjectPath::try_from(DEVICE)?, 123456u32),
    );
    assert!(rejected.is_err());
    let real_sender = Proxy::new(
        &server,
        agent_owner.as_str(),
        "/org/hearth/BluetoothAgent",
        "org.bluez.Agent1",
    )?;
    let rejected: zbus::Result<()> = real_sender.call(
        "RequestConfirmation",
        &(
            ObjectPath::try_from("/org/bluez/hci0/dev_other")?,
            123456u32,
        ),
    );
    assert!(rejected.is_err());
    worker.send(
        json!({"type":"reply","challenge_id":challenge["challenge_id"],"reply":{"kind":"reject"}}),
    )?;
    assert_eq!(worker.event("result")?["status"], "rejected");
    worker.send(
        json!({"type":"reply","challenge_id":challenge["challenge_id"],"reply":{"kind":"accept"}}),
    )?;
    worker.event("reply_rejected")?;
    assert!(!calls.lock().expect("calls").iter().any(|c| c == "pair"));
    worker.input.take();
    worker.exited()?;
    Ok(())
}

#[test]
fn sigterm_cancels_pending_pair_and_exits_promptly() -> Result<()> {
    let bus = Bus::new()?;
    let (_server, calls) = serve(&bus, true, false, false, 50)?;
    let mut worker = Worker::new(&bus)?;
    let state = worker.event("bluetooth")?;
    pair(
        &mut worker,
        state["state"]["owner"].as_str().expect("owner"),
        "signal",
    )?;
    worker.event("challenge")?;
    assert_eq!(
        unsafe { libc::kill(worker.child.id() as i32, libc::SIGTERM) },
        0
    );
    worker.exited()?;
    assert!(!calls.lock().expect("calls").iter().any(|c| c == "pair"));
    assert!(
        calls
            .lock()
            .expect("calls")
            .iter()
            .any(|c| c == "cancel_pairing")
    );
    Ok(())
}

#[test]
fn readonly_once_ignores_closed_stdin_and_emits_one_snapshot() -> Result<()> {
    let bus = Bus::new()?;
    for running in [false, true] {
        let _server = running
            .then(|| serve(&bus, true, false, false, 50))
            .transpose()?;
        let output = Command::new(env!("CARGO_BIN_EXE_hearth-bluetooth"))
            .args(["--once", "--address", &bus.address])
            .stdin(Stdio::null())
            .output()?;
        assert!(output.status.success());
        let lines: Vec<_> = output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.len(), 1);
        let envelope: Value = serde_json::from_slice(lines[0])?;
        assert_eq!(envelope["service"], "bluetooth");
        assert_eq!(
            envelope["status"],
            if running { "available" } else { "unavailable" }
        );
        assert!(envelope.get("type").is_none());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_hearth-bluetooth"))
        .args([
            "--once",
            "--address",
            "unix:path=/nonexistent/hearth-test-bus",
        ])
        .stdin(Stdio::null())
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["status"],
        "unavailable"
    );
    Ok(())
}
#[test]
fn observer_ignores_stdin_eof_and_stops_on_sigterm() -> Result<()> {
    let bus = Bus::new()?;
    let (_server, calls) = serve(&bus, true, false, false, 50)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_hearth-bluetooth"))
        .args(["--observe", "--address", &bus.address])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout")).read_line(&mut line)?;
    assert_eq!(serde_json::from_str::<Value>(&line)?["status"], "available");
    assert!(child.try_wait()?.is_none());
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Observer did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(calls.lock().expect("calls").is_empty());
    Ok(())
}
