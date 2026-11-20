use anyhow::{Result, ensure};
use hearth::{
    audio::ActionRequest,
    audio_pipewire::{AudioService, Command},
};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Action {
        request_id: String,
        request: ActionRequest,
    },
}
fn run() -> Result<()> {
    let (mut observe, mut once, mut remote) = (false, false, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--observe" => observe = true,
            "--once" => {
                once = true;
                observe = true;
            }
            "--remote" => {
                ensure!(remote.is_none(), "Duplicate remote");
                remote = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing remote"))?,
                );
            }
            "--help" => {
                println!("Usage: hearth-audio [--observe] [--once] [--remote SOCKET]");
                return Ok(());
            }
            _ => anyhow::bail!("Usage: hearth-audio [--observe] [--once] [--remote SOCKET]"),
        }
    }
    if let Some(name) = &remote {
        ensure!(
            !name.is_empty() && name.len() <= 4096 && !name.contains('\0'),
            "Invalid PipeWire remote"
        );
    }
    let (read, write) = UnixStream::pair()?;
    let mut handlers = Vec::new();
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        handlers.push(signal_hook::low_level::pipe::register(
            signal,
            write.try_clone()?,
        )?);
    }
    for fd in [libc::STDOUT_FILENO]
        .into_iter()
        .chain((!observe).then_some(libc::STDIN_FILENO))
    {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "Cannot configure audio worker IO"
        );
    }
    let mut service = AudioService::start(remote)?;
    let (mut input, mut output, mut offset) = (Vec::new(), VecDeque::<Vec<u8>>::new(), 0usize);
    let mut observed = false;
    let mut received = Instant::now();
    let outcome = (|| -> Result<()> {
        loop {
            loop {
                let event = match service.events.try_recv() {
                    Ok(event) => event,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        anyhow::bail!("PipeWire worker stopped; action outcome unknown")
                    }
                };
                received = Instant::now();
                if once && observed {
                    continue;
                }
                if event.get("service").is_some() {
                    observed = true;
                }
                let mut line = serde_json::to_vec(&event)?;
                line.push(b'\n');
                ensure!(
                    line.len() <= 128 * 1024
                        && output.len() < 32
                        && output.iter().map(Vec::len).sum::<usize>() + line.len() <= 256 * 1024,
                    "Audio output backlog exceeded"
                );
                output.push_back(line);
            }
            ensure!(
                received.elapsed() < Duration::from_secs(8),
                "Audio worker unresponsive; action outcome unknown"
            );
            let mut fds = [
                libc::pollfd {
                    fd: read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if observe { -1 } else { libc::STDIN_FILENO },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if output.is_empty() {
                        -1
                    } else {
                        libc::STDOUT_FILENO
                    },
                    events: libc::POLLOUT,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), 3, 25) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if fds[0].revents != 0 {
                return Ok(());
            }
            if fds[2].revents != 0
                && let Some(line) = output.front()
            {
                let n = unsafe {
                    libc::write(
                        libc::STDOUT_FILENO,
                        line[offset..].as_ptr().cast(),
                        line.len() - offset,
                    )
                };
                if n > 0 {
                    offset += n as usize;
                    if offset == line.len() {
                        output.pop_front();
                        offset = 0;
                        if once && observed && output.is_empty() {
                            return Ok(());
                        }
                    }
                } else if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    if !matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        return Err(error.into());
                    }
                }
            }
            if fds[1].revents != 0 {
                let mut bytes = [0u8; 4096];
                let n = unsafe {
                    libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len())
                };
                if n == 0 {
                    return Ok(());
                }
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        continue;
                    }
                    return Err(error.into());
                }
                input.extend_from_slice(&bytes[..n as usize]);
                while let Some(end) = input.iter().position(|b| *b == b'\n') {
                    ensure!(end < 16384, "Audio command exceeds16KiB");
                    let line: Vec<_> = input.drain(..=end).collect();
                    let Input::Action {
                        request_id,
                        request,
                    } = serde_json::from_slice(&line)
                        .map_err(|_| anyhow::anyhow!("Invalid audio command"))?;
                    ensure!(
                        !request_id.is_empty()
                            && request_id.len() <= 128
                            && !request_id.chars().any(char::is_control),
                        "Invalid audio request ID"
                    );
                    service.send(Command::Action {
                        request_id,
                        request,
                    })?;
                }
                ensure!(input.len() <= 16384, "Audio command exceeds16KiB");
            }
        }
    })();
    let _ = service.send(Command::Stop);
    for handler in handlers {
        signal_hook::low_level::unregister(handler);
    }
    outcome
}
fn main() {
    if let Err(error) = run() {
        eprintln!("hearth-audio: {error:#}");
        std::process::exit(1);
    }
}
