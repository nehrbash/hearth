use anyhow::{Result, ensure};
use hearth::{
    bluetooth::{Action, ActionRequest, BluetoothClient},
    bluetooth_agent::{self, Challenges, Reply},
    services::ServiceEnvelope,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::mpsc::{self, SyncSender},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Action {
        request_id: String,
        request: ActionRequest,
    },
    Reply {
        challenge_id: u64,
        reply: Reply,
    },
    Cancel,
}
struct Active {
    id: String,
    request: ActionRequest,
    hub: Option<Challenges>,
    deadline: Instant,
    cancelled: bool,
}
fn cancel(active: &mut Active, address: &Option<String>) {
    if active.cancelled {
        return;
    }
    active.cancelled = true;
    if let Some(hub) = &active.hub {
        hub.cancel();
        let addr = address.clone();
        let request = active.request.clone();
        std::thread::spawn(move || {
            let _ = bluetooth_agent::cancel_pairing(addr.as_deref(), &request);
        });
    }
}
fn send(tx: &SyncSender<Value>, value: Value) -> bool {
    tx.try_send(value).is_ok()
}
fn result(id: &str, ok: bool) -> Value {
    json!({"type":"result","request_id":id,"status":if ok{"executed"}else{"rejected"},"error":if ok{None}else{Some("Bluetooth action failed or cancelled; outcome may be unknown")}})
}
fn queue(out: &mut VecDeque<Vec<u8>>, value: Value) -> Result<()> {
    let mut line = serde_json::to_vec(&value)?;
    line.push(b'\n');
    ensure!(
        line.len() <= 128 * 1024
            && out.len() < 32
            && out.iter().map(Vec::len).sum::<usize>() + line.len() <= 256 * 1024,
        "Bluetooth output backlog exceeded"
    );
    out.push_back(line);
    Ok(())
}
fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut address, mut observe, mut once) = (None, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => {
                ensure!(address.is_none(), "Duplicate bus address");
                address = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing bus address"))?,
                );
            }
            "--observe" => observe = true,
            "--once" => {
                once = true;
                observe = true;
            }
            "--help" => {
                println!(
                    "Usage: hearth-bluetooth [--observe] [--once] [--address PRIVATE_TEST_BUS]"
                );
                return Ok(());
            }
            _ => anyhow::bail!(
                "Usage: hearth-bluetooth [--observe] [--once] [--address PRIVATE_TEST_BUS]"
            ),
        }
    }
    let (read, write) = UnixStream::pair()?;
    let mut handlers = Vec::new();
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
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
            "Cannot configure worker pipes"
        );
    }
    let (tx, events) = mpsc::sync_channel(32);
    let (actions, requests) = mpsc::sync_channel::<(String, ActionRequest)>(1);
    let actor_address = address.clone();
    let actor_tx = tx.clone();
    std::thread::spawn(move || {
        let mut client = None;
        loop {
            if client.is_none() {
                client = match actor_address.as_deref() {
                    Some(a) => BluetoothClient::with_address(a),
                    None => BluetoothClient::system(),
                }
                .ok();
            }
            let snapshot = match &client {
                Some(c) => c.inspect(),
                None => Err(anyhow::anyhow!("BlueZ bus unavailable")),
            };
            if snapshot.is_err() {
                client = None;
            }
            if !send(
                &actor_tx,
                serde_json::to_value(ServiceEnvelope::observed_service("bluetooth", snapshot))
                    .unwrap_or(Value::Null),
            ) {
                return;
            }
            match requests.recv_timeout(Duration::from_secs(1)) {
                Ok((id, request)) => {
                    let ok = client.as_ref().is_some_and(|c| c.execute(&request).is_ok());
                    if !send(&actor_tx, result(&id, ok)) {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    let mut active: Option<Active> = None;
    let mut challenges: Option<mpsc::Receiver<bluetooth_agent::Challenge>> = None;
    let mut outgoing = VecDeque::new();
    let mut offset = 0;
    let mut input = Vec::new();
    let mut last_snapshot = Instant::now();
    let mut observed = false;
    let outcome = (|| -> Result<()> {
        loop {
            while let Ok(event) = events.try_recv() {
                if event.get("service").is_some() {
                    last_snapshot = Instant::now();
                    if once && observed {
                        continue;
                    }
                    observed = true;
                    if let Some(a) = &mut active
                        && a.hub.is_some()
                        && event["state"]["owner"].as_str() != Some(a.request.owner.as_str())
                    {
                        cancel(a, &address);
                    }
                } else if event["type"] == "result"
                    && active.as_ref().is_some_and(|a| event["request_id"] == a.id)
                {
                    active = None;
                    challenges = None;
                }
                queue(&mut outgoing, event)?;
            }
            if let Some(receiver) = &challenges {
                while let Ok(challenge) = receiver.try_recv() {
                    let mut value = serde_json::to_value(challenge)?;
                    value["type"] = json!("challenge");
                    value["request_id"] = json!(active.as_ref().map(|action| action.id.as_str()));
                    queue(&mut outgoing, value)?;
                }
            }
            if let Some(a) = &active {
                if let Some(h) = &a.hub {
                    h.expire();
                }
                ensure!(
                    Instant::now() < a.deadline,
                    "Bluetooth action deadline exceeded; outcome unknown"
                );
            }
            ensure!(
                last_snapshot.elapsed() < Duration::from_secs(4),
                "Bluetooth inventory worker deadline exceeded"
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
                    fd: if outgoing.is_empty() {
                        -1
                    } else {
                        libc::STDOUT_FILENO
                    },
                    events: libc::POLLOUT,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), 3, 25) } < 0 {
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
                && let Some(line) = outgoing.front()
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
                        outgoing.pop_front();
                        offset = 0;
                        if once && observed && outgoing.is_empty() {
                            return Ok(());
                        }
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
                let mut bytes = [0u8; 4096];
                let n = unsafe {
                    libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len())
                };
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
                ensure!(
                    input.len() <= 16384 + 4096,
                    "Bluetooth command exceeds 16 KiB"
                );
                while let Some(end) = input.iter().position(|b| *b == b'\n') {
                    ensure!(end < 16384, "Bluetooth command exceeds 16 KiB");
                    let line: Vec<_> = input.drain(..=end).collect();
                    let command: Command = serde_json::from_slice(&line)
                        .map_err(|_| anyhow::anyhow!("Invalid Bluetooth command"))?;
                    match command {
                        Command::Reply {
                            challenge_id,
                            reply,
                        } => {
                            let h = active.as_ref().and_then(|a| a.hub.as_ref());
                            if h.is_none_or(|h| h.reply(challenge_id, reply).is_err()) {
                                queue(
                                    &mut outgoing,
                                    json!({"type":"reply_rejected","challenge_id":challenge_id,"error":"Stale or invalid reply"}),
                                )?;
                            }
                        }
                        Command::Cancel => {
                            if let Some(a) = &mut active {
                                cancel(a, &address);
                            }
                        }
                        Command::Action {
                            request_id,
                            request,
                        } => {
                            ensure!(
                                !request_id.is_empty()
                                    && request_id.len() <= 128
                                    && !request_id.chars().any(char::is_control),
                                "Invalid request ID"
                            );
                            if active.is_some() {
                                queue(&mut outgoing, result(&request_id, false))?;
                                continue;
                            }
                            if matches!(request.action, Action::Pair) {
                                let (hub, receiver) = Challenges::new();
                                challenges = Some(receiver);
                                let addr = address.clone();
                                let pair_request = request.clone();
                                let pair_id = request_id.clone();
                                let pair_tx = tx.clone();
                                let pair_hub = hub.clone();
                                std::thread::spawn(move || {
                                    let ok = bluetooth_agent::pair(
                                        addr.as_deref(),
                                        &pair_request,
                                        pair_hub,
                                    )
                                    .is_ok();
                                    send(&pair_tx, result(&pair_id, ok));
                                });
                                active = Some(Active {
                                    id: request_id,
                                    request,
                                    hub: Some(hub),
                                    cancelled: false,
                                    deadline: Instant::now() + Duration::from_secs(120),
                                });
                            } else {
                                actions.try_send((request_id.clone(), request.clone()))?;
                                active = Some(Active {
                                    id: request_id,
                                    request,
                                    hub: None,
                                    cancelled: false,
                                    deadline: Instant::now() + Duration::from_secs(3),
                                });
                            }
                        }
                    }
                }
                ensure!(input.len() <= 16384, "Bluetooth command exceeds 16 KiB");
            }
        }
    })();
    if let Some(a) = active
        && let Some(h) = a.hub
    {
        h.cancel();
        let addr = address.clone();
        let (done, wait) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = bluetooth_agent::cancel_pairing(addr.as_deref(), &a.request);
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_millis(500));
    }
    for handler in handlers {
        signal_hook::low_level::unregister(handler);
    }
    outcome
}
fn main() {
    if let Err(error) = run() {
        eprintln!("hearth-bluetooth: {error:#}");
        std::process::exit(1);
    }
}
