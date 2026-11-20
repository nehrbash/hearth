use crate::bluetooth::{Action, ActionRequest, BluetoothClient};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    future::poll_fn,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    task::{Poll, Waker},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zbus::{
    blocking::{Proxy, connection::Builder},
    message::Header,
    zvariant::ObjectPath,
};
const AGENT_PATH: &str = "/org/hearth/BluetoothAgent";
#[derive(Clone, Debug, Serialize)]
pub struct Challenge {
    pub challenge_id: u64,
    pub owner: String,
    pub device: String,
    pub kind: String,
    pub passkey: Option<u32>,
    pub pin: Option<String>,
    pub entered: Option<u16>,
    pub uuid: Option<String>,
    pub deadline_ms: u64,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Accept,
    Reject,
    Pin { value: String },
    Passkey { value: u32 },
}
struct Pending {
    id: u64,
    display_passkey: Option<u32>,
    deadline: Instant,
    reply: Option<Reply>,
    waker: Option<Waker>,
}
#[derive(Clone)]
pub struct Challenges {
    pending: Arc<Mutex<Option<Pending>>>,
    cancelled: Arc<AtomicBool>,
    events: SyncSender<Challenge>,
}
impl Challenges {
    pub fn new() -> (Self, Receiver<Challenge>) {
        let (events, receiver) = mpsc::sync_channel(8);
        (
            Self {
                pending: Arc::new(Mutex::new(None)),
                cancelled: Arc::new(AtomicBool::new(false)),
                events,
            },
            receiver,
        )
    }
    pub fn reply(&self, id: u64, reply: Reply) -> Result<()> {
        if let Reply::Pin { value } = &reply {
            ensure!(
                !value.is_empty()
                    && value.len() <= 16
                    && value.is_ascii()
                    && !value.chars().any(char::is_control),
                "Invalid PIN"
            );
        }
        if let Reply::Passkey { value } = &reply {
            ensure!(*value <= 999999, "Invalid passkey");
        }
        let mut slot = self
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Challenge lock poisoned"))?;
        let pending = slot
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No pending challenge"))?;
        ensure!(
            pending.id == id && Instant::now() < pending.deadline && pending.reply.is_none(),
            "Stale or duplicate challenge reply"
        );
        pending.reply = Some(reply);
        if let Some(w) = pending.waker.take() {
            w.wake();
        }
        Ok(())
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.pending.lock()
            && let Some(p) = slot.as_mut()
        {
            p.reply = Some(Reply::Reject);
            if let Some(w) = p.waker.take() {
                w.wake();
            }
        }
    }
    pub fn expire(&self) {
        if let Ok(mut slot) = self.pending.lock()
            && let Some(p) = slot.as_mut()
            && Instant::now() >= p.deadline
        {
            p.reply = Some(Reply::Reject);
            if let Some(w) = p.waker.take() {
                w.wake();
            }
        }
    }
    async fn ask(&self, mut challenge: Challenge) -> std::result::Result<Reply, AgentError> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(AgentError::Canceled("Cancelled".into()));
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        challenge.challenge_id = id;
        challenge.deadline_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
            + 30000;
        {
            let mut slot = self
                .pending
                .lock()
                .map_err(|_| AgentError::Rejected("Unavailable".into()))?;
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(AgentError::Canceled("Cancelled".into()));
            }
            if let Some(pending) = slot.as_ref() {
                if challenge.kind == "display_passkey"
                    && pending.display_passkey == challenge.passkey
                    && pending.reply.is_none()
                    && Instant::now() < pending.deadline
                {
                    challenge.challenge_id = pending.id;
                    challenge.deadline_ms -= 30000
                        - pending
                            .deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis() as u64;
                    drop(slot);
                    self.events
                        .try_send(challenge)
                        .map_err(|_| AgentError::Rejected("Challenge queue full".into()))?;
                    return Ok(Reply::Accept);
                }
                return Err(AgentError::Rejected("Challenge already pending".into()));
            }
            *slot = Some(Pending {
                id,
                display_passkey: (challenge.kind == "display_passkey")
                    .then_some(challenge.passkey)
                    .flatten(),
                deadline: Instant::now() + Duration::from_secs(30),
                reply: None,
                waker: None,
            });
        }
        if self.events.try_send(challenge).is_err() {
            self.cancel();
        }
        poll_fn(|cx| {
            let Ok(mut slot) = self.pending.lock() else {
                return Poll::Ready(Err(AgentError::Rejected("Unavailable".into())));
            };
            let Some(p) = slot.as_mut() else {
                return Poll::Ready(Err(AgentError::Canceled("Cancelled".into())));
            };
            if p.id != id {
                return Poll::Ready(Err(AgentError::Canceled("Superseded".into())));
            }
            if let Some(reply) = p.reply.take() {
                *slot = None;
                Poll::Ready(match reply {
                    Reply::Reject => Err(AgentError::Rejected("Rejected".into())),
                    other => Ok(other),
                })
            } else {
                p.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    Rejected(String),
    Canceled(String),
}
struct Agent {
    owner: String,
    device: String,
    hub: Challenges,
}
impl Agent {
    fn verify(
        &self,
        header: &Header<'_>,
        device: Option<&ObjectPath<'_>>,
    ) -> std::result::Result<(), AgentError> {
        if header.sender().map(|s| s.as_str()) != Some(self.owner.as_str())
            || device.is_some_and(|d| d.as_str() != self.device)
        {
            return Err(AgentError::Rejected("Unexpected caller or device".into()));
        }
        Ok(())
    }
    async fn ask(
        &self,
        header: Header<'_>,
        device: ObjectPath<'_>,
        kind: &str,
        passkey: Option<u32>,
        pin: Option<String>,
        entered: Option<u16>,
    ) -> std::result::Result<Reply, AgentError> {
        self.verify(&header, Some(&device))?;
        self.hub
            .ask(Challenge {
                challenge_id: 0,
                owner: self.owner.clone(),
                device: self.device.clone(),
                kind: kind.into(),
                passkey,
                pin,
                entered,
                uuid: None,
                deadline_ms: 0,
            })
            .await
    }
}
fn accepted(reply: Reply) -> std::result::Result<(), AgentError> {
    match reply {
        Reply::Accept => Ok(()),
        _ => Err(AgentError::Rejected("Wrong response kind".into())),
    }
}
#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn request_pin_code(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<String, AgentError> {
        match self.ask(header, device, "pin", None, None, None).await? {
            Reply::Pin { value } => Ok(value),
            _ => Err(AgentError::Rejected("PIN required".into())),
        }
    }
    async fn request_passkey(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<u32, AgentError> {
        match self
            .ask(header, device, "passkey", None, None, None)
            .await?
        {
            Reply::Passkey { value } => Ok(value),
            _ => Err(AgentError::Rejected("Passkey required".into())),
        }
    }
    async fn request_confirmation(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), AgentError> {
        if passkey > 999999 {
            return Err(AgentError::Rejected("Invalid passkey".into()));
        }
        accepted(
            self.ask(header, device, "confirmation", Some(passkey), None, None)
                .await?,
        )
    }
    async fn request_authorization(
        &self,
        device: ObjectPath<'_>,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), AgentError> {
        accepted(
            self.ask(header, device, "authorization", None, None, None)
                .await?,
        )
    }
    async fn authorize_service(
        &self,
        device: ObjectPath<'_>,
        _uuid: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), AgentError> {
        self.verify(&header, Some(&device))?;
        if _uuid.len() != 36 || !_uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(AgentError::Rejected("Invalid UUID".into()));
        }
        accepted(
            self.hub
                .ask(Challenge {
                    challenge_id: 0,
                    owner: self.owner.clone(),
                    device: self.device.clone(),
                    kind: "authorize_service".into(),
                    passkey: None,
                    pin: None,
                    entered: None,
                    uuid: Some(_uuid.into()),
                    deadline_ms: 0,
                })
                .await?,
        )
    }
    async fn display_pin_code(
        &self,
        device: ObjectPath<'_>,
        pincode: &str,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), AgentError> {
        if pincode.len() > 16 || !pincode.is_ascii() {
            return Err(AgentError::Rejected("Invalid PIN".into()));
        }
        accepted(
            self.ask(
                header,
                device,
                "display_pin",
                None,
                Some(pincode.into()),
                None,
            )
            .await?,
        )
    }
    async fn display_passkey(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        entered: u16,
        #[zbus(header)] header: Header<'_>,
    ) -> std::result::Result<(), AgentError> {
        if passkey > 999999 || entered > 6 {
            return Err(AgentError::Rejected("Invalid passkey".into()));
        }
        accepted(
            self.ask(
                header,
                device,
                "display_passkey",
                Some(passkey),
                None,
                Some(entered),
            )
            .await?,
        )
    }
    fn cancel(&self, #[zbus(header)] header: Header<'_>) -> std::result::Result<(), AgentError> {
        self.verify(&header, None)?;
        self.hub.cancel();
        Ok(())
    }
    fn release(&self, #[zbus(header)] header: Header<'_>) -> std::result::Result<(), AgentError> {
        self.verify(&header, None)?;
        self.hub.cancel();
        Ok(())
    }
}
pub fn pair(address: Option<&str>, request: &ActionRequest, hub: Challenges) -> Result<()> {
    ensure!(
        matches!(request.action, Action::Pair),
        "Pair action required"
    );
    let check = match address {
        Some(a) => BluetoothClient::with_address(a)?,
        None => BluetoothClient::system()?,
    };
    let snapshot = check.inspect()?;
    ensure!(snapshot.owner == request.owner, "BlueZ owner changed");
    let device = snapshot
        .devices
        .iter()
        .find(|d| d.path == request.path)
        .ok_or_else(|| anyhow::anyhow!("Bluetooth device missing"))?;
    ensure!(
        !device.blocked
            && snapshot
                .adapters
                .iter()
                .any(|a| a.path == device.adapter && a.powered),
        "Device blocked or adapter powered off"
    );
    let connection = match address {
        Some(a) => Builder::address(a)?,
        None => Builder::system()?,
    }
    .method_timeout(Duration::from_secs(120))
    .serve_at(
        AGENT_PATH,
        Agent {
            owner: request.owner.clone(),
            device: request.path.clone(),
            hub: hub.clone(),
        },
    )?
    .build()?;
    let bus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;
    let owner: String = bus.call("GetNameOwner", &("org.bluez",))?;
    ensure!(
        owner == request.owner,
        "BlueZ owner changed before registration"
    );
    let manager = Proxy::new(
        &connection,
        request.owner.as_str(),
        "/org/bluez",
        "org.bluez.AgentManager1",
    )?;
    let path = ObjectPath::try_from(AGENT_PATH)?;
    let _: () = manager.call("RegisterAgent", &(path.clone(), "KeyboardDisplay"))?;
    let owner: String = bus.call("GetNameOwner", &("org.bluez",))?;
    ensure!(owner == request.owner, "BlueZ owner changed before pairing");
    ensure!(
        !hub.cancelled.load(Ordering::SeqCst),
        "Pairing cancelled before dispatch"
    );
    let result: Result<()> = Proxy::new(
        &connection,
        request.owner.as_str(),
        request.path.as_str(),
        "org.bluez.Device1",
    )?
    .call("Pair", &())
    .map_err(Into::into);
    hub.cancel();
    let _: zbus::Result<()> = manager.call("UnregisterAgent", &(path,));
    result
}
pub fn cancel_pairing(address: Option<&str>, request: &ActionRequest) -> Result<()> {
    let connection = match address {
        Some(a) => Builder::address(a)?,
        None => Builder::system()?,
    }
    .method_timeout(Duration::from_millis(400))
    .build()?;
    let _: () = Proxy::new(
        &connection,
        request.owner.as_str(),
        request.path.as_str(),
        "org.bluez.Device1",
    )?
    .call("CancelPairing", &())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn challenge(kind: &str) -> Challenge {
        Challenge {
            challenge_id: 0,
            owner: ":1.7".into(),
            device: "/org/bluez/hci0/dev_0".into(),
            kind: kind.into(),
            passkey: Some(123456),
            pin: None,
            entered: Some(0),
            uuid: None,
            deadline_ms: 0,
        }
    }
    #[test]
    fn stale_duplicate_and_expired_replies_are_rejected() {
        let (hub, _) = Challenges::new();
        *hub.pending.lock().expect("pending") = Some(Pending {
            id: 8,
            display_passkey: None,
            deadline: Instant::now() + Duration::from_secs(1),
            reply: None,
            waker: None,
        });
        assert!(hub.reply(7, Reply::Accept).is_err());
        assert!(hub.reply(8, Reply::Accept).is_ok());
        assert!(hub.reply(8, Reply::Accept).is_err());
        hub.pending
            .lock()
            .expect("pending")
            .as_mut()
            .expect("challenge")
            .deadline = Instant::now();
        assert!(hub.reply(8, Reply::Accept).is_err());
    }
    #[test]
    fn expiry_wakes_waiting_agent_and_cancellation_prevents_new_challenges() {
        let (hub, events) = Challenges::new();
        let waiter = hub.clone();
        let task =
            std::thread::spawn(move || pollster::block_on(waiter.ask(challenge("confirmation"))));
        events
            .recv_timeout(Duration::from_secs(1))
            .expect("challenge");
        hub.pending
            .lock()
            .expect("pending")
            .as_mut()
            .expect("challenge")
            .deadline = Instant::now();
        hub.expire();
        assert!(task.join().expect("waiter").is_err());
        hub.cancel();
        assert!(pollster::block_on(hub.ask(challenge("authorization"))).is_err());
    }
    #[test]
    fn display_progress_updates_keep_one_explicit_acknowledgment() {
        let (hub, events) = Challenges::new();
        let waiter = hub.clone();
        let task = std::thread::spawn(move || {
            pollster::block_on(waiter.ask(challenge("display_passkey")))
        });
        let first = events
            .recv_timeout(Duration::from_secs(1))
            .expect("challenge");
        let mut progress = challenge("display_passkey");
        progress.entered = Some(3);
        assert!(matches!(
            pollster::block_on(hub.ask(progress)),
            Ok(Reply::Accept)
        ));
        let update = events
            .recv_timeout(Duration::from_secs(1))
            .expect("progress");
        assert_eq!(first.challenge_id, update.challenge_id);
        assert_eq!(update.entered, Some(3));
        assert!(!task.is_finished());
        hub.reply(first.challenge_id, Reply::Accept).expect("ack");
        assert!(task.join().expect("waiter").is_ok());
    }
}
