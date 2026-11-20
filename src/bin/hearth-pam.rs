use anyhow::{Result, ensure};
use hearth::pam::{self, Input, Policy, Supervisor};
use std::{
    collections::VecDeque,
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};
fn run() -> Result<()> {
    pam::harden()?;
    let mut service = None;
    let mut config_dir = None;
    let mut timeout = 60000u64;
    let mut child = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--service" => {
                ensure!(service.is_none(), "Duplicate service");
                service = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing service"))?,
                );
            }
            "--config-dir" => {
                ensure!(config_dir.is_none(), "Duplicate config directory");
                config_dir = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing config directory"))?
                        .into(),
                );
            }
            "--timeout-ms" => {
                timeout = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("Missing deadline"))?
                    .parse()?
            }
            "--pam-attempt" => child = true,
            "--help" => {
                println!(
                    "Usage: hearth-pam --service NAME [--config-dir PRIVATE_TEST_DIR] [--timeout-ms 100..120000]"
                );
                return Ok(());
            }
            _ => anyhow::bail!("Invalid PAM worker arguments"),
        }
    }
    let policy = Policy {
        service: service.ok_or_else(|| anyhow::anyhow!("Explicit PAM service required"))?,
        config_dir,
        timeout: Duration::from_millis(timeout),
    };
    if child {
        return pam::run_attempt(&policy);
    }
    let mut supervisor = Supervisor::new(policy)?;
    let (signal_read, signal_write) = UnixStream::pair()?;
    let mut handlers = Vec::new();
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        handlers.push(signal_hook::low_level::pipe::register(
            signal,
            signal_write.try_clone()?,
        )?);
    }
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "Cannot configure PAM worker IO"
        );
    }
    let mut input = Zeroizing::new(Vec::new());
    let mut output = VecDeque::<Zeroizing<Vec<u8>>>::new();
    let mut offset = 0;
    let outcome = (|| -> Result<()> {
        loop {
            supervisor.tick()?;
            while let Some(event) = supervisor.events.pop_front() {
                let mut line = Zeroizing::new(serde_json::to_vec(&event)?);
                line.push(b'\n');
                ensure!(
                    line.len() <= 16384
                        && output.len() < 64
                        && output.iter().map(|b| b.len()).sum::<usize>() + line.len() <= 128 * 1024,
                    "PAM output backlog exceeded"
                );
                output.push_back(line);
            }
            let mut fds = [
                libc::pollfd {
                    fd: signal_read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if output.is_empty() { -1 } else { 1 },
                    events: libc::POLLOUT,
                    revents: 0,
                },
                libc::pollfd {
                    fd: supervisor.active_fd().unwrap_or(-1),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 20) } < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e.into());
            }
            if fds[0].revents != 0 {
                return Ok(());
            }
            if fds[2].revents != 0
                && let Some(line) = output.front()
            {
                let n =
                    unsafe { libc::write(1, line[offset..].as_ptr().cast(), line.len() - offset) };
                if n > 0 {
                    offset += n as usize;
                    if offset == line.len() {
                        output.pop_front();
                        offset = 0;
                    }
                } else if n < 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        return Err(e.into());
                    }
                }
            }
            if fds[1].revents != 0 {
                let mut bytes = Zeroizing::new([0u8; 4096]);
                let n = unsafe { libc::read(0, bytes.as_mut_ptr().cast(), bytes.len()) };
                if n == 0 {
                    return Ok(());
                }
                if n < 0 {
                    let e = io::Error::last_os_error();
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        continue;
                    }
                    return Err(e.into());
                }
                input.extend_from_slice(&bytes[..n as usize]);
                bytes.zeroize();
                while let Some(line) = pam::take_line(&mut input)? {
                    let command: Input = serde_json::from_slice(&line)
                        .map_err(|_| anyhow::anyhow!("Invalid PAM command"))?;
                    supervisor.command(&command)?;
                }
            }
        }
    })();
    drop(supervisor);
    for handler in handlers {
        signal_hook::low_level::unregister(handler);
    }
    outcome
}
fn main() {
    if run().is_err() {
        eprintln!("hearth-pam: worker failed closed");
        std::process::exit(1);
    }
}
