use anyhow::{Context, Result, bail, ensure};
use hearth::notifications::{Config, Server};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut socket, mut state, mut address) = (None::<PathBuf>, None::<PathBuf>, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = Some(args.next().context("--socket requires a path")?.into()),
            "--state" => state = Some(args.next().context("--state requires a path")?.into()),
            "--bus-address" => {
                address = Some(args.next().context("--bus-address requires an address")?)
            }
            "--help" | "-h" => {
                println!(
                    "hearth-notifications --socket PATH --state PATH [--bus-address ADDRESS]\nOwns the notification name only if free; never replaces or queues behind another server."
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
    }
    let server = Server::start(Config {
        bus_address: address,
        socket: socket.context("--socket is required")?,
        state_file: state.context("--state is required")?,
    })?;
    let stopped = Arc::new(AtomicBool::new(false));
    let mut handlers = Vec::new();
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        handlers.push(signal_hook::flag::register(signal, stopped.clone())?);
    }
    while !stopped.load(Ordering::Relaxed) && server.is_running() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let healthy = server.is_running();
    drop(server);
    for handler in handlers {
        signal_hook::low_level::unregister(handler);
    }
    ensure!(healthy, "Notification worker exited unexpectedly");
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("hearth-notifications: {error:#}");
        std::process::exit(1);
    }
}
