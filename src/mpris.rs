use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::{ObjectPath, OwnedValue},
};

const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
type Properties = HashMap<String, OwnedValue>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MprisSnapshot {
    pub players: Vec<Player>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Player {
    pub name: String,
    pub owner: String,
    pub identity: String,
    pub playback_status: PlaybackStatus,
    pub metadata: Metadata,
    pub position_us: u64,
    pub volume: f64,
    pub rate: f64,
    pub shuffle: Option<bool>,
    pub loop_status: Option<LoopStatus>,
    pub capabilities: Capabilities,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub track_id: Option<String>,
    pub title: Option<String>,
    pub artist: Vec<String>,
    pub album: Option<String>,
    pub length_us: Option<u64>,
    pub art_url: Option<String>,
    pub url: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub control: bool,
    pub play: bool,
    pub pause: bool,
    pub next: bool,
    pub previous: bool,
    pub seek: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatus {
    None,
    Track,
    Playlist,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub name: String,
    pub owner: String,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    Seek { offset_us: i64 },
    SetPosition { track_id: String, position_us: u64 },
    Volume { value: f64 },
    Shuffle { value: bool },
    Loop { value: LoopStatus },
}
fn prop<'a, T>(p: &'a Properties, key: &str) -> Result<T>
where
    T: TryFrom<&'a OwnedValue>,
    T::Error: std::error::Error + Send + Sync + 'static,
{
    T::try_from(p.get(key).with_context(|| format!("Missing MPRIS {key}"))?)
        .with_context(|| format!("Invalid MPRIS {key}"))
}
fn text(p: &Properties, key: &str) -> Result<String> {
    let value: &str = prop(p, key)?;
    ensure!(value.len() <= 4096, "MPRIS {key} exceeds 4096 bytes");
    Ok(value.into())
}
fn optional_text(p: &Properties, key: &str) -> Result<Option<String>> {
    p.contains_key(key).then(|| text(p, key)).transpose()
}
fn nonnegative(p: &Properties, key: &str) -> Result<u64> {
    let n: i64 = prop(p, key)?;
    ensure!(n >= 0, "Negative MPRIS {key}");
    Ok(n as u64)
}
fn loop_status(value: &str) -> Result<LoopStatus> {
    Ok(match value {
        "None" => LoopStatus::None,
        "Track" => LoopStatus::Track,
        "Playlist" => LoopStatus::Playlist,
        _ => bail!("Invalid MPRIS LoopStatus"),
    })
}
fn bus(connection: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?)
}
fn owner(connection: &Connection, name: &str) -> Result<String> {
    Ok(bus(connection)?.call("GetNameOwner", &(name,))?)
}
fn properties(connection: &Connection, owner: &str, interface: &str) -> Result<Properties> {
    Ok(
        Proxy::new(connection, owner, PATH, "org.freedesktop.DBus.Properties")?
            .call("GetAll", &(interface,))?,
    )
}
fn read_player(connection: &Connection, name: &str, unique: &str) -> Result<Player> {
    let root = properties(connection, unique, ROOT)?;
    let p = properties(connection, unique, PLAYER)?;
    let metadata: Properties = p
        .get("Metadata")
        .context("Missing MPRIS Metadata")?
        .try_clone()?
        .try_into()?;
    let track_id = metadata
        .get("mpris:trackid")
        .map(|v| -> Result<String> {
            let path: ObjectPath<'_> = v.try_clone()?.try_into()?;
            let s = path.to_string();
            ensure!(s.len() <= 4096, "Track ID too long");
            Ok(s)
        })
        .transpose()?;
    let artist = metadata
        .get("xesam:artist")
        .map(|v| -> Result<Vec<String>> { Ok(v.try_clone()?.try_into()?) })
        .transpose()?
        .unwrap_or_default();
    ensure!(
        artist.len() <= 32 && artist.iter().all(|s| s.len() <= 4096),
        "MPRIS artist list exceeds bounds"
    );
    let volume: f64 = prop(&p, "Volume")?;
    let rate: f64 = prop(&p, "Rate")?;
    ensure!(
        volume.is_finite() && volume >= 0.0 && rate.is_finite() && rate != 0.0,
        "Invalid MPRIS volume/rate"
    );
    let playback_status = match prop::<&str>(&p, "PlaybackStatus")? {
        "Playing" => PlaybackStatus::Playing,
        "Paused" => PlaybackStatus::Paused,
        "Stopped" => PlaybackStatus::Stopped,
        _ => bail!("Invalid MPRIS PlaybackStatus"),
    };
    let result = Player {
        name: name.into(),
        owner: unique.into(),
        identity: text(&root, "Identity")?,
        playback_status,
        metadata: Metadata {
            track_id,
            title: optional_text(&metadata, "xesam:title")?,
            artist,
            album: optional_text(&metadata, "xesam:album")?,
            length_us: metadata
                .contains_key("mpris:length")
                .then(|| nonnegative(&metadata, "mpris:length"))
                .transpose()?,
            art_url: optional_text(&metadata, "mpris:artUrl")?,
            url: optional_text(&metadata, "xesam:url")?,
        },
        position_us: nonnegative(&p, "Position")?,
        volume,
        rate,
        shuffle: p
            .contains_key("Shuffle")
            .then(|| prop(&p, "Shuffle"))
            .transpose()?,
        loop_status: p
            .contains_key("LoopStatus")
            .then(|| loop_status(prop(&p, "LoopStatus")?))
            .transpose()?,
        capabilities: Capabilities {
            control: prop(&p, "CanControl")?,
            play: prop(&p, "CanPlay")?,
            pause: prop(&p, "CanPause")?,
            next: prop(&p, "CanGoNext")?,
            previous: prop(&p, "CanGoPrevious")?,
            seek: prop(&p, "CanSeek")?,
        },
    };
    ensure!(
        owner(connection, name)? == unique,
        "MPRIS owner changed during inspection"
    );
    Ok(result)
}
fn connect(address: Option<&str>) -> Result<Connection> {
    Ok(match address {
        Some(address) => Builder::address(address)?,
        None => Builder::session()?,
    }
    .method_timeout(Duration::from_millis(400))
    .build()?)
}
/// Run inside a deadline-bounded worker: D-Bus authentication can block.
pub fn inspect() -> Result<MprisSnapshot> {
    inspect_connection(&connect(None)?)
}
pub fn inspect_with_address(address: &str) -> Result<MprisSnapshot> {
    inspect_connection(&connect(Some(address))?)
}
fn inspect_connection(connection: &Connection) -> Result<MprisSnapshot> {
    let names: Vec<String> = bus(connection)?.call("ListNames", &())?;
    let mut names: Vec<_> = names
        .into_iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .collect();
    names.sort();
    ensure!(names.len() <= 32, "More than 32 MPRIS players");
    let mut players = Vec::new();
    for name in names {
        let unique = owner(connection, &name)?;
        players.push(read_player(connection, &name, &unique)?);
    }
    let snapshot = MprisSnapshot { players };
    ensure!(
        serde_json::to_vec(&snapshot)?.len() <= 120 * 1024,
        "MPRIS snapshot exceeds 120 KiB"
    );
    Ok(snapshot)
}
/// Send one action to the observed owner; a timeout leaves its outcome unknown.
pub fn execute(request: &ActionRequest) -> Result<()> {
    execute_connection(&connect(None)?, request)
}
pub fn execute_with_address(address: &str, request: &ActionRequest) -> Result<()> {
    execute_connection(&connect(Some(address))?, request)
}
fn execute_connection(connection: &Connection, request: &ActionRequest) -> Result<()> {
    ensure!(
        request.name.starts_with("org.mpris.MediaPlayer2.")
            && request.name.len() <= 255
            && request.owner.starts_with(':')
            && request.owner.len() <= 255,
        "Invalid MPRIS target"
    );
    ensure!(
        owner(connection, &request.name)? == request.owner,
        "MPRIS target owner changed"
    );
    let player = read_player(connection, &request.name, &request.owner)?;
    let caps = &player.capabilities;
    ensure!(caps.control, "MPRIS CanControl is false");
    let proxy = Proxy::new(connection, request.owner.as_str(), PATH, PLAYER)?;
    let method = match &request.action {
        Action::Play => {
            ensure!(caps.play, "MPRIS CanPlay is false");
            "Play"
        }
        Action::Pause => {
            ensure!(caps.pause, "MPRIS CanPause is false");
            "Pause"
        }
        Action::PlayPause => {
            ensure!(
                caps.pause && (player.playback_status == PlaybackStatus::Playing || caps.play),
                "MPRIS PlayPause unsupported"
            );
            "PlayPause"
        }
        Action::Stop => "Stop",
        Action::Next => {
            ensure!(caps.next, "MPRIS CanGoNext is false");
            "Next"
        }
        Action::Previous => {
            ensure!(caps.previous, "MPRIS CanGoPrevious is false");
            "Previous"
        }
        Action::Seek { offset_us } => {
            ensure!(caps.seek, "MPRIS CanSeek is false");
            return Ok(proxy.call("Seek", &(*offset_us,))?);
        }
        Action::SetPosition {
            track_id,
            position_us,
        } => {
            ensure!(caps.seek, "MPRIS CanSeek is false");
            ensure!(
                track_id.len() <= 4096
                    && track_id != "/org/mpris/MediaPlayer2/TrackList/NoTrack"
                    && player.metadata.track_id.as_ref() == Some(track_id),
                "Stale MPRIS track"
            );
            ensure!(
                *position_us <= i64::MAX as u64
                    && player.metadata.length_us.is_none_or(|n| *position_us <= n),
                "MPRIS position outside track"
            );
            let path = ObjectPath::try_from(track_id.as_str())?;
            return Ok(proxy.call("SetPosition", &(path, *position_us as i64))?);
        }
        Action::Volume { value } => {
            ensure!(value.is_finite() && *value >= 0.0, "Invalid MPRIS volume");
            return Ok(proxy.set_property("Volume", value)?);
        }
        Action::Shuffle { value } => {
            ensure!(player.shuffle.is_some(), "MPRIS Shuffle unsupported");
            return Ok(proxy.set_property("Shuffle", value)?);
        }
        Action::Loop { value } => {
            ensure!(player.loop_status.is_some(), "MPRIS LoopStatus unsupported");
            return Ok(proxy.set_property(
                "LoopStatus",
                match value {
                    LoopStatus::None => "None",
                    LoopStatus::Track => "Track",
                    LoopStatus::Playlist => "Playlist",
                },
            )?);
        }
    };
    Ok(proxy.call(method, &())?)
}
