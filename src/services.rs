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
pub struct ServiceEnvelope<T = WorkspaceSnapshot> {
    pub version: u32,
    pub service: String,
    pub status: Status,
    pub observed_at_ms: u64,
    pub state: Option<T>,
    pub error: Option<String>,
}

impl ServiceEnvelope<WorkspaceSnapshot> {
    pub fn observed(result: Result<WorkspaceSnapshot>) -> Self {
        Self::observed_service(
            "hyprland",
            result.and_then(|snapshot| {
                snapshot.validate()?;
                Ok(snapshot)
            }),
        )
    }
}

impl<T: Serialize> ServiceEnvelope<T> {
    pub fn observed_service(service: &str, result: Result<T>) -> Self {
        let observed_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let result = result.and_then(|snapshot| {
            ensure!(
                serde_json::to_vec(&snapshot)?.len() < MAX_LINE - 1024,
                "Snapshot exceeds service envelope budget"
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
                        "Service unavailable".into()
                    } else {
                        message[..end].into()
                    }),
                )
            }
        };
        Self {
            version: 1,
            service: service.into(),
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

pub struct ProbeOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
struct ProbeChild(std::process::Child);
impl Drop for ProbeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn capture_probe(
    command: &mut Command,
    stop_fd: i32,
    timeout: Duration,
) -> Result<Option<ProbeOutput>> {
    if cancelled(stop_fd, Duration::ZERO)? {
        return Ok(None);
    }
    let deadline = Instant::now() + timeout;
    let mut child = ProbeChild(
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Start service probe")?,
    );
    let mut stdout = child.0.stdout.take().context("Missing probe stdout")?;
    let mut stderr = child.0.stderr.take().context("Missing probe stderr")?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let (mut output, mut errors) = (Vec::new(), Vec::new());
    let (mut out_closed, mut err_closed) = (false, false);
    loop {
        if cancelled(stop_fd, Duration::ZERO)? {
            return Ok(None);
        }
        ensure!(Instant::now() < deadline, "Service probe deadline exceeded");
        let status = child.0.try_wait()?;
        if out_closed
            && err_closed
            && let Some(status) = status
        {
            return Ok(Some(ProbeOutput {
                status,
                stdout: output,
                stderr: errors,
            }));
        }
        let mut fds = [
            libc::pollfd {
                fd: stop_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if out_closed { -1 } else { stdout.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if err_closed { -1 } else { stderr.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let delay = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 20) as i32;
        if unsafe { libc::poll(fds.as_mut_ptr(), 3, delay) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fds[0].revents != 0 {
            return Ok(None);
        }
        let read = |stream: &mut dyn Read,
                    bytes: &mut Vec<u8>,
                    closed: &mut bool,
                    ready: i16|
         -> Result<()> {
            if ready == 0 {
                return Ok(());
            }
            let mut buffer = [0; 8192];
            match stream.read(&mut buffer) {
                Ok(0) => *closed = true,
                Ok(count) => {
                    ensure!(
                        bytes.len() + count <= MAX_LINE,
                        "Service probe output exceeds 1 MiB"
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
            Ok(())
        };
        read(&mut stdout, &mut output, &mut out_closed, fds[1].revents)?;
        read(&mut stderr, &mut errors, &mut err_closed, fds[2].revents)?;
    }
}

fn cancelled(stop_fd: i32, wait: Duration) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: stop_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                wait.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result >= 0 {
            return Ok(descriptor.revents != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub struct UpowerService {
    executable: std::path::PathBuf,
    due: Instant,
}
impl UpowerService {
    pub fn new(executable: std::path::PathBuf) -> Self {
        Self {
            executable,
            due: Instant::now(),
        }
    }
    pub fn next(
        &mut self,
        stop_fd: i32,
    ) -> Result<Option<ServiceEnvelope<crate::upower::UpowerSnapshot>>> {
        if cancelled(stop_fd, self.due.saturating_duration_since(Instant::now()))? {
            return Ok(None);
        }
        let result = capture_probe(
            Command::new(&self.executable).arg("--probe-upower"),
            stop_fd,
            Duration::from_millis(2500),
        );
        let inspected = match result {
            Ok(None) => return Ok(None),
            Ok(Some(output)) => {
                if output.status.success() {
                    serde_json::from_slice(&output.stdout).context("Decode UPower probe")
                } else {
                    Err(anyhow::anyhow!(
                        "UPower probe failed ({}): {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            Err(error) => Err(error),
        };
        self.due = Instant::now() + HEARTBEAT;
        Ok(Some(ServiceEnvelope::observed_service("upower", inspected)))
    }
}
