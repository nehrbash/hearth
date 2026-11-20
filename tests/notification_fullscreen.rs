use anyhow::Result;
use hearth::{
    notification_fullscreen::{Feed, Source, active_fullscreen},
    notifications::{Config, Server},
    workspaces::Instance,
};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fake {
    dir: PathBuf,
    fullscreen: Arc<AtomicBool>,
    events: mpsc::Sender<Option<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fake {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "h-fs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let query = UnixListener::bind(dir.join(".socket.sock"))?;
        let events = UnixListener::bind(dir.join(".socket2.sock"))?;
        query.set_nonblocking(true)?;
        events.set_nonblocking(true)?;
        let fullscreen = Arc::new(AtomicBool::new(false));
        let full = fullscreen.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        let worker = thread::spawn(move || {
            let mut peers = Vec::<UnixStream>::new();
            while !stopping.load(Ordering::Relaxed) {
                while let Ok((p, _)) = events.accept() {
                    p.set_write_timeout(Some(Duration::from_millis(100)))
                        .expect("timeout");
                    peers.push(p);
                }
                while let Ok((mut p, _)) = query.accept() {
                    p.set_read_timeout(Some(Duration::from_millis(100)))
                        .expect("timeout");
                    let mut b = [0; 32];
                    if let Ok(n) = p.read(&mut b) {
                        let value = if &b[..n] == b"j/monitors" {
                            serde_json::json!([{"activeWorkspace":{"id":-12},"specialWorkspace":{"id":-99}}])
                        } else {
                            serde_json::json!([{"workspace":{"id":-12},"fullscreen":if full.load(Ordering::Relaxed){2}else{0}}])
                        };
                        let _ =
                            p.write_all(serde_json::to_string(&value).expect("json").as_bytes());
                    }
                }
                for event in rx.try_iter() {
                    match event {
                        Some(b) => peers.retain_mut(|p| p.write_all(&b).is_ok()),
                        None => peers.clear(),
                    }
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        Ok(Self {
            dir,
            fullscreen,
            events: tx,
            stop,
            worker: Some(worker),
        })
    }
    fn source(&self) -> Source {
        Source::Instance(Instance {
            signature: "private".into(),
            wayland_socket: "private".into(),
            directory: self.dir.clone(),
        })
    }
    fn set(&self, value: bool) {
        self.fullscreen.store(value, Ordering::Relaxed);
        self.events
            .send(Some(b"fullscreen>>1\n".to_vec()))
            .expect("event");
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            w.join().expect("fixture");
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}
#[test]
fn predicate_matches_active_normal_workspaces_not_maximized_or_hidden() {
    let monitors=br#"[{"activeWorkspace":{"id":-12},"specialWorkspace":{"id":-99}},{"activeWorkspace":{"id":4}}]"#;
    for (workspace, fullscreen, want) in [
        (-12, 2, true),
        (4, 3, true),
        (-12, 1, false),
        (7, 2, false),
        (-99, 2, false),
    ] {
        let clients = serde_json::to_vec(
            &serde_json::json!([{"workspace":{"id":workspace},"fullscreen":fullscreen}]),
        )
        .expect("json");
        assert_eq!(
            active_fullscreen(monitors, &clients).expect("predicate"),
            want
        );
    }
    assert!(active_fullscreen(monitors, br#"[{"workspace":{"id":4},"fullscreen":9}]"#).is_err());
    assert!(active_fullscreen(monitors, b"bad").is_err());
}
#[test]
fn feed_tracks_events_partial_lines_disconnect_and_reconnect() -> Result<()> {
    let f = Fake::new()?;
    let (tx, rx) = mpsc::channel();
    let feed = Feed::start(f.source(), move |state| {
        let _ = tx.send(state.map_err(|_| ()));
    })?;
    assert_eq!(rx.recv_timeout(Duration::from_secs(3))?, Ok(false));
    f.fullscreen.store(true, Ordering::Relaxed);
    f.events.send(Some(b"fullscreen>>".to_vec()))?;
    assert!(rx.recv_timeout(Duration::from_millis(120)).is_err());
    f.events.send(Some(b"1\n".to_vec()))?;
    assert_eq!(rx.recv_timeout(Duration::from_secs(2))?, Ok(true));
    f.events.send(None)?;
    assert_eq!(rx.recv_timeout(Duration::from_secs(2))?, Err(()));
    assert_eq!(rx.recv_timeout(Duration::from_secs(3))?, Ok(true));
    f.set(false);
    assert_eq!(rx.recv_timeout(Duration::from_secs(2))?, Ok(false));
    let before = Instant::now();
    drop(feed);
    assert!(before.elapsed() < Duration::from_secs(2));
    Ok(())
}
fn wait(
    server: &Server,
    predicate: impl Fn(&hearth::notifications::Snapshot) -> bool,
) -> Result<hearth::notifications::Snapshot> {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let s = server.snapshot()?;
        if predicate(&s) {
            return Ok(s);
        }
        assert!(Instant::now() < end, "state deadline");
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn automatic_suppression_cannot_be_overridden_by_manual_control() -> Result<()> {
    let f = Fake::new()?;
    let mut bus = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut address = String::new();
    BufReader::new(bus.stdout.take().expect("stdout")).read_line(&mut address)?;
    let _bus = Bus(bus);
    let socket = f.dir.join("control");
    let server = Server::start_with_fullscreen(
        Config {
            bus_address: Some(address.trim().into()),
            socket: socket.clone(),
            state_file: f.dir.join("history"),
        },
        f.source(),
    )?;
    let s = wait(&server, |s| s.fullscreen_source == "available")?;
    assert!(!s.fullscreen);
    f.set(true);
    wait(&server, |s| s.fullscreen_automatic)?;
    let control = |value: bool| -> Result<()> {
        let s = server.snapshot()?;
        let mut p = UnixStream::connect(&socket)?;
        p.set_read_timeout(Some(Duration::from_secs(2)))?;
        serde_json::to_writer(
            &mut p,
            &serde_json::json!({"request_id":1,"epoch":s.epoch,"revision":s.revision,"command":{"type":"set_fullscreen","value":value}}),
        )?;
        p.write_all(b"\n")?;
        let mut reply = String::new();
        BufReader::new(p).read_line(&mut reply)?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reply)?["status"],
            "ok"
        );
        Ok(())
    };
    control(false)?;
    assert!(server.snapshot()?.fullscreen);
    control(true)?;
    f.set(false);
    let s = wait(&server, |s| !s.fullscreen_automatic)?;
    assert!(s.fullscreen && s.fullscreen_manual);
    control(false)?;
    assert!(!server.snapshot()?.fullscreen);
    f.events.send(None)?;
    let s = wait(&server, |s| s.fullscreen_source == "unavailable")?;
    assert!(s.fullscreen && s.fullscreen_automatic && s.fullscreen_error.is_some());
    drop(server);
    Ok(())
}
