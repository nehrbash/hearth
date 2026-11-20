use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub generation: String,
    pub sessions: Vec<Session>,
    pub watches: Vec<Watch>,
    pub notices: Vec<Notice>,
    pub leases: Vec<Lease>,
    pub effects: Vec<Effect>,
    pub effects_generation: Option<String>,
    pub warnings: Vec<String>,
    pub notice_history_limit: u16,
    pub notice_scope: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub id: String,
    pub provider: String,
    pub status: String,
    pub host: Option<SessionHost>,
    pub delivery: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHost {
    pub buffer: String,
    pub mode: String,
    pub tty: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Watch {
    pub subscriber: String,
    pub target: WatchTarget,
    pub delivery: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum WatchTarget {
    Session(String),
    Provider(String),
    Agent(String),
    Topic(String),
}

impl WatchTarget {
    pub fn value(&self) -> &str {
        match self {
            Self::Session(value)
            | Self::Provider(value)
            | Self::Agent(value)
            | Self::Topic(value) => value,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Notice {
    pub id: u64,
    pub subscriber: String,
    pub kind: String,
    pub source: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub buffer: String,
    pub holder: String,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectState {
    Pending,
    Executing,
    Completed,
    Failed,
    Rejected,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub id: u64,
    pub state: EffectState,
    #[serde(rename = "type")]
    pub kind: String,
    pub expected: String,
    pub receipt: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityError(String);

impl fmt::Display for ActivityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ActivityError {}

fn text(value: &str, limit: usize) -> Result<(), ActivityError> {
    if value.len() > limit {
        Err(ActivityError("activity string exceeds limit".into()))
    } else {
        Ok(())
    }
}

fn identifier(value: &str) -> Result<(), ActivityError> {
    if value.is_empty() {
        return Err(ActivityError("empty activity identifier".into()));
    }
    text(value, 1024)
}

impl Activity {
    pub fn decode(bytes: &[u8]) -> Result<Self, ActivityError> {
        if bytes.len() > 1024 * 1024 {
            return Err(ActivityError("activity exceeds 1 MiB".into()));
        }
        let value: Self = serde_json::from_slice(bytes)
            .map_err(|error| ActivityError(format!("invalid activity JSON: {error}")))?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ActivityError> {
        identifier(&self.generation)?;
        if self.sessions.len() > 512
            || self.watches.len() > 128
            || self.notices.len() > 128
            || self.leases.len() > 80
            || self.effects.len() > 256
            || self.warnings.len() > 64
        {
            return Err(ActivityError("activity collection exceeds limit".into()));
        }
        if self.notice_history_limit != 128 || self.notice_scope != "current-watch-subscribers" {
            return Err(ActivityError("unsupported notice history semantics".into()));
        }
        let mut session_ids = HashSet::new();
        for session in &self.sessions {
            identifier(&session.id)?;
            identifier(&session.provider)?;
            identifier(&session.status)?;
            if !session_ids.insert(&session.id) {
                return Err(ActivityError("duplicate session ID".into()));
            }
            if let Some(delivery) = &session.delivery {
                identifier(delivery)?;
            }
            if let Some(host) = &session.host {
                identifier(&host.buffer)?;
                identifier(&host.mode)?;
                if let Some(tty) = &host.tty {
                    identifier(tty)?;
                }
            }
        }
        for watch in &self.watches {
            identifier(&watch.subscriber)?;
            identifier(watch.target.value())?;
            if let Some(delivery) = &watch.delivery {
                identifier(delivery)?;
            }
        }
        let mut notice_ids = HashSet::new();
        for notice in &self.notices {
            if !notice_ids.insert(notice.id) {
                return Err(ActivityError("duplicate notice ID".into()));
            }
            identifier(&notice.subscriber)?;
            identifier(&notice.kind)?;
            text(&notice.source, 16 * 1024)?;
            text(&notice.text, 16 * 1024)?;
        }
        let mut leased_buffers = HashSet::new();
        for lease in &self.leases {
            identifier(&lease.buffer)?;
            identifier(&lease.holder)?;
            if !leased_buffers.insert(&lease.buffer) {
                return Err(ActivityError("duplicate leased buffer".into()));
            }
        }
        if let Some(generation) = &self.effects_generation {
            identifier(generation)?;
        } else if !self.effects.is_empty() {
            return Err(ActivityError(
                "owner effects require their isolated generation".into(),
            ));
        }
        let mut effect_ids = HashSet::new();
        for effect in &self.effects {
            if !effect_ids.insert(effect.id) {
                return Err(ActivityError("duplicate effect ID".into()));
            }
            if !matches!(
                effect.kind.as_str(),
                "notify" | "write-file" | "run" | "session-send"
            ) {
                return Err(ActivityError("unknown owner effect type".into()));
            }
            text(&effect.expected, 128 * 1024)?;
            text(&effect.receipt, 128 * 1024)?;
        }
        for warning in &self.warnings {
            text(warning, 16 * 1024)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Activity, EffectState};

    const FIXTURE: &str = r##"{
        "generation":"trusted-1",
        "sessions":[{"id":"codex:one","provider":"codex","status":"active","host":null,"delivery":"codex-send"}],
        "watches":[{"subscriber":"claude","target":{"kind":"session","value":"codex:one"},"delivery":null}],
        "notices":[{"id":7,"subscriber":"claude","kind":"turn-finished","source":"codex:one","text":"Ready"}],
        "leases":[{"buffer":"terminal","holder":"codex","expires_at_ms":null}],
        "effects":[{"id":2,"state":"pending","type":"notify","expected":"(notify \"Ready\")","receipt":"#f"}],
        "effects_generation":"isolated-1",
        "warnings":["host enrichment unavailable"],
        "notice_history_limit":128,
        "notice_scope":"current-watch-subscribers"
    }"##;

    #[test]
    fn preserves_unknown_host_expiry_and_exact_proposal() {
        let activity = Activity::decode(FIXTURE.as_bytes()).expect("fixture");
        assert!(activity.sessions[0].host.is_none());
        assert!(activity.leases[0].expires_at_ms.is_none());
        assert_eq!(activity.effects[0].state, EffectState::Pending);
        assert_eq!(activity.effects[0].expected, "(notify \"Ready\")");
        assert_eq!(
            Activity::decode(&serde_json::to_vec(&activity).expect("JSON")).expect("round trip"),
            activity
        );
    }

    #[test]
    fn rejects_unknown_variants_and_lossy_notice_semantics() {
        for invalid in [
            FIXTURE.replace("\"session\",\"value\"", "\"command\",\"value\""),
            FIXTURE.replace("\"state\":\"pending\"", "\"state\":\"approved\""),
            FIXTURE.replace("\"type\":\"notify\"", "\"type\":\"evaluate\""),
            FIXTURE.replace("current-watch-subscribers", "complete-history"),
        ] {
            assert!(Activity::decode(invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn rejects_duplicate_identity_and_oversized_collections() {
        let mut activity = Activity::decode(FIXTURE.as_bytes()).expect("fixture");
        activity.sessions.push(activity.sessions[0].clone());
        assert!(activity.validate().is_err());
        activity.sessions.pop();
        activity.notices.push(activity.notices[0].clone());
        assert!(activity.validate().is_err());
        activity.notices.pop();
        activity.warnings = vec!["warning".into(); 65];
        assert!(activity.validate().is_err());
        assert!(Activity::decode(&vec![b' '; 1024 * 1024 + 1]).is_err());
    }
}
