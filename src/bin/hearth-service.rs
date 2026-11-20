use anyhow::{Context, Result};
use hearth::services::{HyprlandService, MprisService, ServiceEnvelope, UpowerService};
use std::{
    io::{self, Read},
    os::{fd::AsRawFd, unix::net::UnixStream},
};

struct Shutdown {
    read: UnixStream,
    handlers: Vec<signal_hook::SigId>,
}
impl Shutdown {
    fn new() -> io::Result<Self> {
        let (read, write) = UnixStream::pair()?;
        let mut result = Self {
            read,
            handlers: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            result.handlers.push(signal_hook::low_level::pipe::register(
                signal,
                write.try_clone()?,
            )?);
        }
        Ok(result)
    }
}
impl Drop for Shutdown {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            signal_hook::low_level::unregister(handler);
        }
    }
}

struct OutputFlags {
    fd: i32,
    flags: i32,
}
impl Drop for OutputFlags {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(self.fd, libc::F_SETFL, self.flags);
        }
    }
}
fn emit<T: serde::Serialize>(envelope: &ServiceEnvelope<T>, stop: &UnixStream) -> Result<bool> {
    emit_bytes(&envelope.json_line()?, stop)
}
fn emit_bytes(bytes: &[u8], stop: &UnixStream) -> Result<bool> {
    let fd = libc::STDOUT_FILENO;
    let mut offset = 0;
    while offset < bytes.len() {
        let mut fds = [
            libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            },
            libc::pollfd {
                fd: stop.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fds[1].revents != 0 {
            return Ok(false);
        }
        let written =
            unsafe { libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
        let result = if written < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(written as usize)
        };
        match result {
            Ok(0) => return Ok(false),
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(false),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}
fn nonblocking_flags(fd: i32) -> Result<OutputFlags> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    anyhow::ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "Cannot configure descriptor"
    );
    Ok(OutputFlags { fd, flags })
}
fn action_input(stop: &UnixStream) -> Result<Vec<u8>> {
    let _flags = nonblocking_flags(libc::STDIN_FILENO)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut bytes = Vec::new();
    loop {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Action input deadline exceeded"
        );
        let mut fds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, 20) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        anyhow::ensure!(fds[1].revents == 0, "Action input cancelled");
        if fds[0].revents != 0 {
            let mut chunk = [0u8; 4096];
            let count =
                unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), chunk.len()) };
            if count == 0 {
                return Ok(bytes);
            }
            if count < 0 {
                let error = io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) {
                    continue;
                }
                return Err(error.into());
            }
            anyhow::ensure!(
                bytes.len() + count as usize <= 16384,
                "MPRIS action exceeds 16 KiB"
            );
            bytes.extend_from_slice(&chunk[..count as usize]);
        }
    }
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--probe-upower"] {
        println!("{}", serde_json::to_string(&hearth::upower::inspect()?)?);
        return Ok(());
    }
    if args == ["--probe-mpris"] {
        println!("{}", serde_json::to_string(&hearth::mpris::inspect()?)?);
        return Ok(());
    }
    if args == ["--execute-mpris"] {
        let mut bytes = Vec::new();
        io::stdin().take(16385).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 16384, "MPRIS action exceeds 16 KiB");
        let request: hearth::mpris::ActionRequest = serde_json::from_slice(&bytes)?;
        hearth::mpris::execute(&request)?;
        println!("{}", serde_json::json!({"status":"executed"}));
        return Ok(());
    }
    if args == ["mpris", "--action"] {
        let shutdown = Shutdown::new()?;
        let bytes = action_input(&shutdown.read)?;
        let _: hearth::mpris::ActionRequest = serde_json::from_slice(&bytes)?;
        let output = hearth::services::capture_probe_input(
            std::process::Command::new(std::env::current_exe()?).arg("--execute-mpris"),
            shutdown.read.as_raw_fd(),
            std::time::Duration::from_millis(2500),
            &bytes,
        )
        .context("MPRIS action failed; outcome may be unknown; do not retry automatically")?
        .context("MPRIS action cancelled; outcome may be unknown")?;
        anyhow::ensure!(
            output.status.success(),
            "MPRIS action failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let _flags = nonblocking_flags(libc::STDOUT_FILENO)?;
        emit_bytes(&output.stdout, &shutdown.read)?;
        return Ok(());
    }

    if args == ["--help"] || args == ["-h"] {
        println!(
            "Usage: hearth-service (hyprland|upower|mpris) [--once]; mpris --action reads one JSON action from stdin"
        );
        return Ok(());
    }
    anyhow::ensure!(
        args == ["hyprland"]
            || args == ["hyprland", "--once"]
            || args == ["upower"]
            || args == ["upower", "--once"]
            || args == ["mpris"]
            || args == ["mpris", "--once"],
        "Usage: hearth-service (hyprland|upower|mpris) [--once]; mpris --action reads one JSON action from stdin"
    );
    let shutdown = Shutdown::new()?;
    let fd = io::stdout().as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    anyhow::ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "Cannot configure service output"
    );
    let _output_flags = OutputFlags { fd, flags };
    if args[0] == "mpris" {
        let mut service = MprisService::new(std::env::current_exe()?);
        while let Some(envelope) = service.next(shutdown.read.as_raw_fd())? {
            if !emit(&envelope, &shutdown.read)? || args.len() == 2 {
                break;
            }
        }
    } else if args[0] == "upower" {
        let mut service = UpowerService::new(std::env::current_exe()?);
        while let Some(envelope) = service.next(shutdown.read.as_raw_fd())? {
            if !emit(&envelope, &shutdown.read)? || args.len() == 2 {
                break;
            }
        }
    } else if args.len() == 2 {
        emit(&HyprlandService::inspect_once(), &shutdown.read)?;
    } else {
        let mut service = HyprlandService::new();
        while let Some(envelope) = service.next(shutdown.read.as_raw_fd())? {
            if !emit(&envelope, &shutdown.read)? {
                break;
            }
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run().context("hearth-service") {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
