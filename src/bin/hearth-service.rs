use anyhow::{Context, Result};
use hearth::services::{HyprlandService, ServiceEnvelope};
use std::{
    io,
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
fn emit(envelope: &ServiceEnvelope, stop: &UnixStream) -> Result<bool> {
    let bytes = envelope.json_line()?;
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
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("Usage: hearth-service hyprland [--once]");
        return Ok(());
    }
    anyhow::ensure!(
        args == ["hyprland"] || args == ["hyprland", "--once"],
        "Usage: hearth-service hyprland [--once]"
    );
    let shutdown = Shutdown::new()?;
    let fd = io::stdout().as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    anyhow::ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "Cannot configure service output"
    );
    let _output_flags = OutputFlags { fd, flags };
    if args.len() == 2 {
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
