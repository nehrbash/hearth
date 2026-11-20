use anyhow::Result;
use hearth::mpris::{self, Action, ActionRequest, LoopStatus};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use zbus::{
    blocking::{Connection, connection::Builder},
    zvariant::{ObjectPath, OwnedValue, Value},
};
const NAME: &str = "org.mpris.MediaPlayer2.hearth_test";
const PATH: &str = "/org/mpris/MediaPlayer2";
struct Bus {
    child: Child,
    address: String,
}
impl Bus {
    fn new() -> Result<Self> {
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
struct Identity {
    owner_loss: Option<Mutex<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
}
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Identity {
    #[zbus(property)]
    fn identity(&self) -> &str {
        if let Some(channels) = &self.owner_loss {
            let (send, recv) = &*channels.lock().expect("test mutex");
            send.send(()).expect("owner loss worker");
            recv.recv_timeout(Duration::from_secs(1))
                .expect("owner released");
        }
        "Test player"
    }
}
type Calls = Arc<Mutex<Vec<String>>>;
struct Player {
    calls: Calls,
    control: bool,
    malformed: bool,
    allowed: bool,
    optional: bool,
}
impl Player {
    fn record(&self, name: &str) {
        self.calls.lock().expect("test mutex").push(name.into());
    }
}
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        self.record("Play");
    }
    fn pause(&self) {
        self.record("Pause");
    }
    fn play_pause(&self) {
        self.record("PlayPause");
    }
    fn stop(&self) {
        self.record("Stop");
    }
    fn next(&self) {
        self.record("Next");
    }
    fn previous(&self) {
        self.record("Previous");
    }
    fn seek(&self, offset: i64) {
        self.record(&format!("Seek:{offset}"));
    }
    fn set_position(&self, track: ObjectPath<'_>, position: i64) {
        self.record(&format!("SetPosition:{track}:{position}"));
    }
    #[zbus(property)]
    fn playback_status(&self) -> &str {
        "Playing"
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        HashMap::from([
            (
                "mpris:trackid".into(),
                Value::from(ObjectPath::try_from("/track/1").expect("valid track path"))
                    .try_into()
                    .expect("owned variant"),
            ),
            (
                "xesam:title".into(),
                Value::from("Test song").try_into().expect("owned variant"),
            ),
            (
                "xesam:artist".into(),
                Value::from(vec!["Artist"])
                    .try_into()
                    .expect("owned variant"),
            ),
            ("mpris:length".into(), OwnedValue::from(10000i64)),
        ])
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        100
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        0.5
    }
    #[zbus(property)]
    fn set_volume(&self, value: f64) {
        self.record(&format!("Volume:{value}"));
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn shuffle(&self) -> zbus::fdo::Result<bool> {
        if self.optional {
            Ok(false)
        } else {
            Err(zbus::fdo::Error::UnknownProperty("Shuffle".into()))
        }
    }
    #[zbus(property)]
    fn set_shuffle(&self, value: bool) {
        self.record(&format!("Shuffle:{value}"));
    }
    #[zbus(property)]
    fn loop_status(&self) -> zbus::fdo::Result<&str> {
        if self.optional {
            Ok("None")
        } else {
            Err(zbus::fdo::Error::UnknownProperty("LoopStatus".into()))
        }
    }
    #[zbus(property)]
    fn set_loop_status(&self, value: &str) {
        self.record(&format!("Loop:{value}"));
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        self.control
    }
    #[zbus(property)]
    fn can_play(&self) -> OwnedValue {
        if self.malformed {
            Value::from("true").try_into().expect("owned variant")
        } else {
            OwnedValue::from(self.allowed)
        }
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.allowed
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.allowed
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.allowed
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.allowed
    }
}
fn serve(bus: &Bus, control: bool, malformed: bool) -> Result<(Connection, Calls)> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c = Builder::address(bus.address.as_str())?
        .name(NAME)?
        .serve_at(PATH, Identity { owner_loss: None })?
        .serve_at(
            PATH,
            Player {
                calls: calls.clone(),
                control,
                malformed,
                allowed: true,
                optional: true,
            },
        )?
        .build()?;
    Ok((c, calls))
}
#[test]
fn discovery_and_all_actions_on_private_bus() -> Result<()> {
    let bus = Bus::new()?;
    assert!(
        mpris::inspect_with_address(&bus.address)?
            .players
            .is_empty()
    );
    let (server, calls) = serve(&bus, true, false)?;
    let snapshot = mpris::inspect_with_address(&bus.address)?;
    assert_eq!(snapshot.players.len(), 1);
    let p = &snapshot.players[0];
    assert_eq!(p.identity, "Test player");
    assert_eq!(p.metadata.artist, ["Artist"]);
    assert_eq!(p.metadata.length_us, Some(10000));
    assert_eq!(p.metadata.album, None);
    let actions = [
        Action::Play,
        Action::Pause,
        Action::PlayPause,
        Action::Stop,
        Action::Next,
        Action::Previous,
        Action::Seek { offset_us: -50 },
        Action::SetPosition {
            track_id: "/track/1".into(),
            position_us: 500,
        },
        Action::Volume { value: 0.75 },
        Action::Shuffle { value: true },
        Action::Loop {
            value: LoopStatus::Playlist,
        },
    ];
    for action in actions {
        mpris::execute_with_address(
            &bus.address,
            &ActionRequest {
                name: p.name.clone(),
                owner: p.owner.clone(),
                action,
            },
        )?;
    }
    assert_eq!(
        *calls.lock().expect("test mutex"),
        [
            "Play",
            "Pause",
            "PlayPause",
            "Stop",
            "Next",
            "Previous",
            "Seek:-50",
            "SetPosition:/track/1:500",
            "Volume:0.75",
            "Shuffle:true",
            "Loop:Playlist"
        ]
    );
    server.close()?;
    assert!(
        mpris::inspect_with_address(&bus.address)?
            .players
            .is_empty()
    );
    Ok(())
}
#[test]
fn stale_owner_and_invalid_arguments_never_dispatch() -> Result<()> {
    let bus = Bus::new()?;
    let (server, calls) = serve(&bus, true, false)?;
    let p = mpris::inspect_with_address(&bus.address)?.players.remove(0);
    for action in [
        Action::Volume { value: f64::NAN },
        Action::Volume { value: -1.0 },
        Action::SetPosition {
            track_id: "/track/stale".into(),
            position_us: 0,
        },
        Action::SetPosition {
            track_id: "/track/1".into(),
            position_us: 10001,
        },
        Action::SetPosition {
            track_id: "/track/1".into(),
            position_us: u64::MAX,
        },
    ] {
        assert!(
            mpris::execute_with_address(
                &bus.address,
                &ActionRequest {
                    name: p.name.clone(),
                    owner: p.owner.clone(),
                    action
                }
            )
            .is_err()
        );
    }
    assert!(calls.lock().expect("test mutex").is_empty());
    server.release_name(NAME)?;
    let (_replacement, new_calls) = serve(&bus, true, false)?;
    assert!(
        mpris::execute_with_address(
            &bus.address,
            &ActionRequest {
                name: p.name,
                owner: p.owner,
                action: Action::Play
            }
        )
        .is_err()
    );
    assert!(new_calls.lock().expect("test mutex").is_empty());
    Ok(())
}
#[test]
fn malformed_capabilities_and_disabled_control_reject() -> Result<()> {
    for (control, malformed) in [(false, false), (true, true)] {
        let bus = Bus::new()?;
        let (server, calls) = serve(&bus, control, malformed)?;
        if malformed {
            assert!(mpris::inspect_with_address(&bus.address).is_err());
        } else {
            let p = mpris::inspect_with_address(&bus.address)?.players.remove(0);
            assert!(
                mpris::execute_with_address(
                    &bus.address,
                    &ActionRequest {
                        name: p.name,
                        owner: p.owner,
                        action: Action::Stop
                    }
                )
                .is_err()
            );
        }
        assert!(calls.lock().expect("test mutex").is_empty());
        server.close()?;
    }
    Ok(())
}

#[test]
fn owner_loss_during_property_read_rejects_snapshot() -> Result<()> {
    let bus = Bus::new()?;
    let (send, recv) = mpsc::sync_channel(1);
    let (ack, acked) = mpsc::sync_channel(1);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = Builder::address(bus.address.as_str())?
        .name(NAME)?
        .serve_at(
            PATH,
            Identity {
                owner_loss: Some(Mutex::new((send, acked))),
            },
        )?
        .serve_at(
            PATH,
            Player {
                calls,
                control: true,
                malformed: false,
                allowed: true,
                optional: true,
            },
        )?
        .build()?;
    let worker = std::thread::spawn(move || -> Result<()> {
        recv.recv_timeout(Duration::from_secs(2))?;
        server.release_name(NAME)?;
        ack.send(())?;
        Ok(())
    });
    assert!(mpris::inspect_with_address(&bus.address).is_err());
    worker.join().expect("owner loss thread")?;
    Ok(())
}

#[test]
fn individual_capabilities_and_optional_controls_are_enforced() -> Result<()> {
    let bus = Bus::new()?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _server = Builder::address(bus.address.as_str())?
        .name(NAME)?
        .serve_at(PATH, Identity { owner_loss: None })?
        .serve_at(
            PATH,
            Player {
                calls: calls.clone(),
                control: true,
                malformed: false,
                allowed: false,
                optional: false,
            },
        )?
        .build()?;
    let player = mpris::inspect_with_address(&bus.address)?.players.remove(0);
    assert_eq!(player.shuffle, None);
    assert_eq!(player.loop_status, None);
    for action in [
        Action::Play,
        Action::Pause,
        Action::PlayPause,
        Action::Next,
        Action::Previous,
        Action::Seek { offset_us: 1 },
        Action::SetPosition {
            track_id: "/track/1".into(),
            position_us: 1,
        },
        Action::Shuffle { value: true },
        Action::Loop {
            value: LoopStatus::Track,
        },
    ] {
        assert!(
            mpris::execute_with_address(
                &bus.address,
                &ActionRequest {
                    name: player.name.clone(),
                    owner: player.owner.clone(),
                    action
                }
            )
            .is_err()
        );
    }
    assert!(calls.lock().expect("test mutex").is_empty());
    Ok(())
}
