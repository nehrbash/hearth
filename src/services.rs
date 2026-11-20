use crate::workspaces::{Instance, WorkspaceFeed, WorkspaceSnapshot};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    os::fd::AsRawFd,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const HEARTBEAT: Duration = Duration::from_secs(5);
pub const MAX_LINE: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceEnvelope {
    pub version: u32,
    pub service: String,
    pub status: Status,
    pub observed_at_ms: u64,
    pub state: Option<WorkspaceSnapshot>,
    pub error: Option<String>,
}

impl ServiceEnvelope {
    pub fn observed(result: Result<WorkspaceSnapshot>) -> Self {
        let observed_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let result = result.and_then(|snapshot| {
            snapshot.validate()?;
            ensure!(
                serde_json::to_vec(&snapshot)?.len() < MAX_LINE - 1024,
                "Compositor snapshot exceeds service envelope budget"
            );
            Ok(snapshot)
        });
        let (status, state, error) = match result {
            Ok(state) => (Status::Available, Some(state), None),
            Err(error) => {
                let message = format!("{error:#}");
                let mut end = message.len().min(4096);
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                (
                    Status::Unavailable,
                    None,
                    Some(if message.is_empty() {
                        "Compositor unavailable".into()
                    } else {
                        message[..end].into()
                    }),
                )
            }
        };
        Self {
            version: 1,
            service: "hyprland".into(),
            status,
            observed_at_ms,
            state,
            error,
        }
    }

    pub fn json_line(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self)?;
        ensure!(bytes.len() < MAX_LINE, "Service envelope exceeds 1 MiB");
        bytes.push(b'\n');
        Ok(bytes)
    }
}

pub fn discover_hyprland() -> Result<Instance> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is missing")?;
    let display = std::env::var("WAYLAND_DISPLAY").context("WAYLAND_DISPLAY is missing")?;
    let mut child = Command::new("hyprctl")
        .args(["instances", "-j"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Discover compositor instances")?;
    let result = (|| -> Result<Vec<u8>> {
        let mut stdout = child.stdout.take().context("Missing discovery stdout")?;
        nonblocking(stdout.as_raw_fd())?;
        let deadline = Instant::now() + Duration::from_millis(600);
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait()? {
                        ensure!(status.success(), "hyprctl instance discovery failed");
                        return Ok(bytes);
                    }
                }
                Ok(count) => {
                    ensure!(
                        bytes.len() + count <= MAX_LINE,
                        "Instance list exceeds 1 MiB"
                    );
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
            ensure!(
                Instant::now() < deadline,
                "Compositor discovery deadline exceeded"
            );
            // A closed stdout can precede process exit; sleep on no descriptors to avoid spinning.
            let mut poll = libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 10) };
            if ready > 0 && poll.revents & libc::POLLHUP != 0 {
                unsafe {
                    libc::poll(std::ptr::null_mut(), 0, 1);
                }
            }
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    Instance::from_instances_json(Path::new(&runtime), &display, None, &result?)
}

pub(crate) fn nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub struct HyprlandService {
    discover: Box<dyn FnMut() -> Result<Instance>>,
    instance: Option<Instance>,
    feed: Option<WorkspaceFeed>,
    heartbeat: Duration,
    due: Instant,
}

impl Default for HyprlandService {
    fn default() -> Self {
        Self::new()
    }
}
impl HyprlandService {
    pub fn new() -> Self {
        Self::with_discovery(discover_hyprland, HEARTBEAT)
    }
    pub fn with_discovery(
        discover: impl FnMut() -> Result<Instance> + 'static,
        heartbeat: Duration,
    ) -> Self {
        Self {
            discover: Box::new(discover),
            instance: None,
            feed: None,
            heartbeat: heartbeat.max(Duration::from_millis(10)),
            due: Instant::now(),
        }
    }
    pub fn inspect_once() -> ServiceEnvelope {
        ServiceEnvelope::observed(discover_hyprland().and_then(|instance| instance.snapshot()))
    }

    fn refresh(&mut self) -> ServiceEnvelope {
        let result = (|| {
            let instance = (self.discover)()?;
            if self.instance.as_ref().is_none_or(|old| {
                old.signature != instance.signature || old.directory != instance.directory
            }) {
                self.feed.take();
                self.feed = Some(WorkspaceFeed::start(instance.clone())?);
                self.instance = Some(instance.clone());
            }
            instance.snapshot()
        })();
        if result.is_err() {
            self.feed.take();
            self.instance = None;
        }
        self.due = Instant::now() + self.heartbeat;
        ServiceEnvelope::observed(result)
    }

    pub fn next(&mut self, stop_fd: i32) -> Result<Option<ServiceEnvelope>> {
        loop {
            let delay = self.due.saturating_duration_since(Instant::now());
            let mut fds = [
                libc::pollfd {
                    fd: stop_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.feed.as_ref().map_or(-1, WorkspaceFeed::update_fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ready = unsafe {
                libc::poll(
                    fds.as_mut_ptr(),
                    2,
                    delay.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if fds[0].revents != 0 {
                return Ok(None);
            }
            if Instant::now() >= self.due {
                return Ok(Some(self.refresh()));
            }
            if fds[1].revents != 0 {
                let feed = self.feed.as_ref().context("Missing compositor feed")?;
                feed.drain_notifications()?;
                let update = feed.take_update();
                if let Some(error) = update.error {
                    return Ok(Some(ServiceEnvelope::observed(Err(anyhow::anyhow!(error)))));
                }
                if update.snapshot.is_some() {
                    return Ok(Some(ServiceEnvelope::observed(
                        self.instance
                            .as_ref()
                            .context("Missing compositor instance")?
                            .snapshot(),
                    )));
                }
            }
        }
    }
}
