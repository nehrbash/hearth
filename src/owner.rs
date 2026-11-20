use crate::activity::{Activity, EffectState};
use std::collections::HashSet;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Approve,
    Reject,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRequest {
    pub id: u64,
    pub expected: String,
    pub generation: String,
    pub decision: Decision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnerError {
    ReadOnly,
    Busy,
    AlreadyAttempted,
    Stale,
    InvalidSnapshot(String),
    Transport(String),
}

impl fmt::Display for OwnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnly => formatter.write_str("Owner controls are disabled"),
            Self::Busy => formatter.write_str("An owner decision is already in flight"),
            Self::AlreadyAttempted => formatter
                .write_str("This proposal was already submitted; inspect its refreshed state"),
            Self::Stale => {
                formatter.write_str("The displayed proposal changed or is no longer pending")
            }
            Self::InvalidSnapshot(message) | Self::Transport(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for OwnerError {}

impl CommandRequest {
    pub fn capture(activity: &Activity, id: u64, decision: Decision) -> Result<Self, OwnerError> {
        activity
            .validate()
            .map_err(|error| OwnerError::InvalidSnapshot(error.to_string()))?;
        let generation = activity
            .effects_generation
            .as_ref()
            .ok_or(OwnerError::Stale)?;
        let effect = activity
            .effects
            .iter()
            .find(|effect| effect.id == id && effect.state == EffectState::Pending)
            .ok_or(OwnerError::Stale)?;
        Ok(Self {
            id,
            expected: effect.expected.clone(),
            generation: generation.clone(),
            decision,
        })
    }

    pub fn validate_against(&self, activity: &Activity) -> Result<(), OwnerError> {
        activity
            .validate()
            .map_err(|error| OwnerError::InvalidSnapshot(error.to_string()))?;
        if activity.effects_generation.as_deref() != Some(self.generation.as_str())
            || !activity.effects.iter().any(|effect| {
                effect.id == self.id
                    && effect.state == EffectState::Pending
                    && effect.expected == self.expected
            })
        {
            return Err(OwnerError::Stale);
        }
        Ok(())
    }

    pub fn arguments(&self) -> [String; 6] {
        [
            "owner".into(),
            self.decision.as_str().into(),
            "--generation".into(),
            self.generation.clone(),
            self.id.to_string(),
            self.expected.clone(),
        ]
    }
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub request: CommandRequest,
    pub result: Result<String, String>,
}

type Executor = dyn Fn(&CommandRequest) -> Result<String, String> + Send + Sync;

pub struct OwnerActions {
    enabled: bool,
    busy: bool,
    attempted_generation: Option<String>,
    attempted: HashSet<u64>,
    completion: Arc<Mutex<Option<Completion>>>,
    notifications: UnixStream,
    notify: UnixStream,
    executor: Arc<Executor>,
}

impl OwnerActions {
    pub fn new(enabled: bool) -> io::Result<Self> {
        Self::with_executor(enabled, execute)
    }

    pub fn with_executor(
        enabled: bool,
        executor: impl Fn(&CommandRequest) -> Result<String, String> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let (notifications, notify) = UnixStream::pair()?;
        notifications.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        Ok(Self {
            enabled,
            busy: false,
            attempted_generation: None,
            attempted: HashSet::new(),
            completion: Arc::new(Mutex::new(None)),
            notifications,
            notify,
            executor: Arc::new(executor),
        })
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn completion_fd(&self) -> RawFd {
        self.notifications.as_raw_fd()
    }

    pub fn submit(&mut self, request: CommandRequest, latest: &Activity) -> Result<(), OwnerError> {
        if !self.enabled {
            return Err(OwnerError::ReadOnly);
        }
        if self.busy {
            return Err(OwnerError::Busy);
        }
        request.validate_against(latest)?;
        if self.attempted_generation.as_deref() != Some(request.generation.as_str()) {
            self.attempted_generation = Some(request.generation.clone());
            self.attempted.clear();
        }
        self.attempted
            .retain(|id| latest.effects.iter().any(|effect| effect.id == *id));
        if self.attempted.contains(&request.id) {
            return Err(OwnerError::AlreadyAttempted);
        }
        let completion = self.completion.clone();
        let executor = self.executor.clone();
        let mut notify = self
            .notify
            .try_clone()
            .map_err(|error| OwnerError::Transport(error.to_string()))?;
        let id = request.id;
        thread::Builder::new()
            .name("hearth-owner-decision".into())
            .spawn(move || {
                let result = executor(&request);
                if let Ok(mut slot) = completion.lock() {
                    *slot = Some(Completion { request, result });
                }
                let _ = notify.write(&[1]);
            })
            .map_err(|error| OwnerError::Transport(error.to_string()))?;
        // A delayed snapshot must not turn a double click into another broker request.
        self.attempted.insert(id);
        self.busy = true;
        Ok(())
    }

    pub fn drain_notifications(&self) -> io::Result<()> {
        let mut socket = &self.notifications;
        let mut bytes = [0; 32];
        loop {
            match socket.read(&mut bytes) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn take_completion(&mut self) -> Option<Completion> {
        let result = self.completion.try_lock().ok()?.take();
        if result.is_some() {
            self.busy = false;
        }
        result
    }
}

fn bounded(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(16 * 1024)]).into_owned()
}

fn execute(request: &CommandRequest) -> Result<String, String> {
    let output = Command::new("weft")
        .args(request.arguments())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(bounded(&output.stdout))
    } else {
        Err(format!(
            "Generation-bound owner decision failed ({}); refresh the proposal and verify the Weft CLI/broker support --generation. No unguarded retry was sent: {}{}",
            output.status,
            bounded(&output.stderr),
            bounded(&output.stdout)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandRequest, Decision, OwnerActions, OwnerError};
    use crate::activity::{Activity, Effect, EffectState};
    use std::sync::{Arc, Mutex, mpsc};

    fn activity() -> Activity {
        Activity {
            generation: "trusted".into(),
            sessions: vec![],
            watches: vec![],
            notices: vec![],
            leases: vec![],
            effects: vec![Effect {
                id: 3,
                state: EffectState::Pending,
                kind: "notify".into(),
                expected: "(notify \"literal $(touch never)\")".into(),
                receipt: "#f".into(),
            }],
            effects_generation: Some("isolated".into()),
            warnings: vec![],
            notice_history_limit: 128,
            notice_scope: "current-watch-subscribers".into(),
        }
    }

    fn wait(actions: &mut OwnerActions) -> super::Completion {
        let mut descriptor = libc::pollfd {
            fd: actions.completion_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 3000) }, 1);
        actions.drain_notifications().expect("drain completion");
        actions.take_completion().expect("completed request")
    }

    #[test]
    fn keeps_expected_content_in_one_literal_argument() {
        let current = activity();
        let request = CommandRequest::capture(&current, 3, Decision::Approve).expect("request");
        assert_eq!(
            request.arguments(),
            [
                "owner",
                "approve",
                "--generation",
                "isolated",
                "3",
                &current.effects[0].expected
            ]
        );
    }

    #[test]
    fn rejects_changed_generation_payload_and_decided_proposals() {
        let current = activity();
        let request = CommandRequest::capture(&current, 3, Decision::Approve).expect("request");
        let mut changed = current.clone();
        changed.effects_generation = Some("restarted".into());
        assert_eq!(request.validate_against(&changed), Err(OwnerError::Stale));
        changed = current.clone();
        changed.effects[0].expected.push(' ');
        assert_eq!(request.validate_against(&changed), Err(OwnerError::Stale));
        changed = current;
        changed.effects[0].state = EffectState::Interrupted;
        assert_eq!(request.validate_against(&changed), Err(OwnerError::Stale));
        assert_eq!(
            CommandRequest::capture(&changed, 3, Decision::Approve),
            Err(OwnerError::Stale)
        );
    }

    #[test]
    fn read_only_mode_never_calls_the_executor() {
        let current = activity();
        let request = CommandRequest::capture(&current, 3, Decision::Reject).expect("request");
        let mut actions =
            OwnerActions::with_executor(false, |_| panic!("read-only execution")).expect("actions");
        assert_eq!(actions.submit(request, &current), Err(OwnerError::ReadOnly));
        assert!(!actions.is_busy());
    }

    #[test]
    fn one_in_flight_and_duplicate_clicks_are_rejected_even_after_failure() {
        let current = activity();
        let request = CommandRequest::capture(&current, 3, Decision::Approve).expect("request");
        let (release, receive) = mpsc::channel();
        let receive = Arc::new(Mutex::new(receive));
        let mut actions = OwnerActions::with_executor(true, move |_| {
            receive
                .lock()
                .expect("receiver")
                .recv()
                .expect("release executor");
            Err("unreachable broker; execution unconfirmed".into())
        })
        .expect("actions");
        actions.submit(request.clone(), &current).expect("submit");
        assert!(actions.is_busy());
        assert_eq!(
            actions.submit(request.clone(), &current),
            Err(OwnerError::Busy)
        );
        release.send(()).expect("release");
        assert!(wait(&mut actions).result.is_err());
        assert!(!actions.is_busy());
        assert_eq!(
            actions.submit(request, &current),
            Err(OwnerError::AlreadyAttempted)
        );
    }

    #[test]
    fn cancellation_is_an_explicit_reject_request_without_approval() {
        let current = activity();
        let request = CommandRequest::capture(&current, 3, Decision::Reject).expect("reject");
        let mut actions = OwnerActions::with_executor(true, |request| {
            assert_eq!(request.decision, Decision::Reject);
            Ok("broker rejected the pending proposal".into())
        })
        .expect("actions");
        actions.submit(request, &current).expect("submit rejection");
        assert!(wait(&mut actions).result.is_ok());
    }
}
