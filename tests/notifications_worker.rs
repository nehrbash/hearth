use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::Value,
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    _bus: Process,
    address: String,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hearth-notifications-worker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let mut bus = Process(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().context("bus stdout")?).read_line(&mut address)?;
        Ok(Self {
            _bus: bus,
            address: address.trim().into(),
            dir,
        })
    }
    fn spawn(&self) -> Result<Process> {
        Ok(Process(
            Command::new(env!("CARGO_BIN_EXE_hearth-notifications"))
                .arg("--no-hyprland-fullscreen")
                .arg("--socket")
                .arg(self.dir.join("control.sock"))
                .arg("--state")
                .arg(self.dir.join("history.json"))
                .arg("--bus-address")
                .arg(&self.address)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    }
    fn connection(&self) -> Result<Connection> {
        Ok(Builder::address(self.address.as_str())?
            .method_timeout(Duration::from_secs(2))
            .build()?)
    }
    fn client(&self, process: &mut Process) -> Result<UnixStream> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(s) = UnixStream::connect(self.dir.join("control.sock")) {
                s.set_read_timeout(Some(Duration::from_secs(2)))?;
                return Ok(s);
            }
            anyhow::ensure!(process.0.try_wait()?.is_none(), "Server exited at startup");
            anyhow::ensure!(Instant::now() < deadline, "Socket not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn snapshot(&self, p: &mut Process) -> Result<serde_json::Value> {
        let mut s = self.client(p)?;
        s.write_all(b"{\"request_id\":1,\"command\":{\"type\":\"inspect\"}}\n")?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line)?;
        Ok(serde_json::from_str::<serde_json::Value>(&line)?["snapshot"].clone())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn stop(p: &mut Process) -> Result<()> {
    assert_eq!(unsafe { libc::kill(p.0.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = p.0.try_wait()? {
            anyhow::ensure!(status.success(), "Server exit failed: {status}");
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "Server did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn worker_survives_renderer_disconnect_and_restores_read_only_history() -> Result<()> {
    let f = Fixture::new()?;
    let mut p = f.spawn()?;
    drop(f.client(&mut p)?);
    let connection = f.connection()?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    let id: u32 = proxy.call(
        "Notify",
        &(
            "app",
            0u32,
            "",
            "summary",
            "persisted",
            Vec::<String>::new(),
            HashMap::<String, Value>::new(),
            0i32,
        ),
    )?;
    let before = f.snapshot(&mut p)?;
    assert_eq!(before["active"][0]["id"], id);
    assert_eq!(
        fs::metadata(f.dir.join("control.sock"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    stop(&mut p)?;
    assert!(!f.dir.join("control.sock").exists());
    assert_eq!(
        fs::metadata(f.dir.join("history.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut p = f.spawn()?;
    let after = f.snapshot(&mut p)?;
    assert_ne!(before["epoch"], after["epoch"]);
    assert_eq!(after["active"], serde_json::json!([]));
    assert_eq!(after["history"][0]["body"], "persisted");
    assert_eq!(after["history"][0]["actionable"], false);
    stop(&mut p)?;
    Ok(())
}
#[test]
fn slow_subscriber_and_partial_input_do_not_block_another_client() -> Result<()> {
    let f = Fixture::new()?;
    let mut p = f.spawn()?;
    let mut slow = f.client(&mut p)?;
    slow.write_all(b"{\"request_id\":1,\"command\":{\"type\":\"subscribe\"}}\n")?;
    let mut partial = f.client(&mut p)?;
    partial.write_all(b"{\"request_id\":")?;
    let c = f.connection()?;
    let proxy = Proxy::new(
        &c,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    for _ in 0..20 {
        let _: u32 = proxy.call(
            "Notify",
            &(
                "app",
                0u32,
                "",
                "summary",
                "x".repeat(16000),
                Vec::<String>::new(),
                HashMap::<String, Value>::new(),
                0i32,
            ),
        )?;
    }
    assert_eq!(
        f.snapshot(&mut p)?["active"].as_array().map(Vec::len),
        Some(20)
    );
    drop(slow);
    drop(partial);
    stop(&mut p)?;
    Ok(())
}
#[test]
fn malformed_persistence_is_not_overwritten() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.dir.join("history.json");
    fs::write(&path, b"not json")?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let mut p = f.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = p.0.try_wait()? {
            assert!(!status.success());
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "Corrupt state startup hung");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read(path)?, b"not json");
    assert!(!f.dir.join("control.sock").exists());
    Ok(())
}
