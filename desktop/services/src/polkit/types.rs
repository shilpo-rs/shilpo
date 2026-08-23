use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
pub use shilpo_domain::{
    CancellationReason, DomainLifecycle, DomainPortTelemetry, DomainSupervisor, DomainVersion,
    MailboxPolicy, StaleUpdateError, SupervisorState,
};
use tokio::sync::watch;

use crate::secret::SecretString;

/// Represents a candidate identity for authentication (e.g. a Unix user).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolkitIdentity {
    pub kind: String,
    pub uid: u32,
    pub user_name: String,
    pub real_name: Option<String>,
}

impl PolkitIdentity {
    pub fn new(kind: impl Into<String>, uid: u32, user_name: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            uid,
            user_name: user_name.into(),
            real_name: None,
        }
    }

    pub fn with_real_name(mut self, real_name: impl Into<String>) -> Self {
        self.real_name = Some(real_name.into());
        self
    }
}

/// An incoming PolicyKit authentication request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolkitRequest {
    pub action_id: String,
    pub message: String,
    pub icon_name: String,
    pub cookie: String,
    pub is_internal: bool,
    pub identities: Vec<PolkitIdentity>,
    pub selected_identity: Option<String>,
}

/// The current prompt / supplementary state presented to the user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolkitPromptState {
    /// Whether the user needs to provide input (password/response).
    pub response_required: bool,
    /// `true` for `PAM_PROMPT_ECHO_ON` (visible text), `false` for `PAM_PROMPT_ECHO_OFF` (masked password).
    pub response_visible: bool,
    /// The prompt text from PAM (e.g. "Password: ").
    pub input_prompt: Option<String>,
    /// Supplementary message text from PAM (e.g. PAM_ERROR_MSG or PAM_TEXT_INFO).
    pub supplementary_message: Option<String>,
    /// Whether the supplementary message is an error (`true`) or informational (`false`).
    pub supplementary_is_error: bool,
}

/// Atomic snapshot of the PolicyKit agent domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolkitSnapshot {
    pub version: DomainVersion,
    pub lifecycle: DomainLifecycle,
    pub request: Option<PolkitRequest>,
    pub prompt_state: Option<PolkitPromptState>,
    pub last_error: Option<String>,
    pub helper_path: Option<String>,
}

impl Default for PolkitSnapshot {
    fn default() -> Self {
        Self {
            version: DomainVersion::ZERO,
            lifecycle: DomainLifecycle::Unavailable,
            request: None,
            prompt_state: None,
            last_error: None,
            helper_path: None,
        }
    }
}

/// Unique identifier for submitted commands.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandId(pub String);

impl CommandId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

/// Typed PolicyKit domain commands.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolkitCommand {
    /// Select the identity (username) to authenticate as when multiple identities are available.
    SelectIdentity { cookie: String, username: String },
    /// Provide a response string (e.g. password) to the active PAM prompt.
    ProvideResponse {
        cookie: String,
        /// Response value. Note: when processed, this memory is zeroed immediately.
        response: SecretString,
    },
    /// User or authority explicit cancellation of the active request.
    Cancel { cookie: String },
    /// Dismiss the active authentication request dialog.
    Dismiss,
    /// Mark the next incoming authentication request as internally triggered by the shell.
    MarkNextRequestInternal,
}

impl fmt::Debug for PolkitCommand {
    /// Manual impl: `ProvideResponse` carries a raw credential and must never be
    /// printable via `{:?}` (derive would include it verbatim).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelectIdentity { cookie, username } => f
                .debug_struct("SelectIdentity")
                .field("cookie", cookie)
                .field("username", username)
                .finish(),
            Self::ProvideResponse { cookie, .. } => f
                .debug_struct("ProvideResponse")
                .field("cookie", cookie)
                .field("response", &"<redacted>")
                .finish(),
            Self::Cancel { cookie } => f.debug_struct("Cancel").field("cookie", cookie).finish(),
            Self::Dismiss => write!(f, "Dismiss"),
            Self::MarkNextRequestInternal => write!(f, "MarkNextRequestInternal"),
        }
    }
}

impl PolkitCommand {
    pub fn policy(&self) -> MailboxPolicy {
        match self {
            Self::MarkNextRequestInternal => MailboxPolicy::ReplaceLatest {
                key: "mark_internal".to_string(),
            },
            _ => MailboxPolicy::Lossless,
        }
    }
}

/// Rejection reasons for Polkit commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolkitRejectionReason {
    Unavailable,
    Overloaded,
    NotFound,
    InvalidState,
}

impl fmt::Display for PolkitRejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "polkit agent is unavailable"),
            Self::Overloaded => write!(f, "polkit command mailbox is full"),
            Self::NotFound => write!(f, "target polkit request or cookie not found"),
            Self::InvalidState => write!(f, "invalid state for requested polkit command"),
        }
    }
}

/// Terminal outcome for accepted PolicyKit commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolkitCommandOutcome {
    Applied {
        version: DomainVersion,
    },
    ReconciledApplied {
        version: DomainVersion,
    },
    Rejected {
        reason: PolkitRejectionReason,
    },
    TimedOut {
        last_observed_version: DomainVersion,
    },
    Cancelled {
        reason: CancellationReason,
    },
}

/// Handle / Ticket returned when a command is submitted.
#[derive(Debug, Clone)]
pub struct CommandTicket {
    outcome: Arc<Mutex<Option<PolkitCommandOutcome>>>,
}

impl CommandTicket {
    pub fn new() -> (Self, CommandResolver) {
        let outcome = Arc::new(Mutex::new(None));
        let ticket = Self {
            outcome: outcome.clone(),
        };
        let resolver = CommandResolver { outcome };
        (ticket, resolver)
    }

    pub fn outcome(&self) -> Option<PolkitCommandOutcome> {
        self.outcome.lock().unwrap().clone()
    }

    pub fn is_completed(&self) -> bool {
        self.outcome.lock().unwrap().is_some()
    }

    pub fn wait_timeout(
        &self,
        timeout: Duration,
        last_observed_version: DomainVersion,
    ) -> PolkitCommandOutcome {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(outcome) = self.outcome() {
                return outcome;
            }
            if std::time::Instant::now() >= deadline {
                let mut guard = self.outcome.lock().unwrap();
                return guard
                    .get_or_insert_with(|| PolkitCommandOutcome::TimedOut {
                        last_observed_version,
                    })
                    .clone();
            }
            std::thread::yield_now();
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandResolver {
    outcome: Arc<Mutex<Option<PolkitCommandOutcome>>>,
}

impl CommandResolver {
    pub fn resolve(&self, outcome: PolkitCommandOutcome) -> bool {
        let mut guard = self.outcome.lock().unwrap();
        if guard.is_none() {
            *guard = Some(outcome);
            true
        } else {
            false
        }
    }
}

/// Narrow, revisioned domain port interface for PolicyKit agent operations.
pub trait PolkitPort: Send + Sync {
    fn snapshot(&self) -> PolkitSnapshot;
    fn subscribe(&self) -> watch::Receiver<PolkitSnapshot>;
    fn submit_command(&self, command: PolkitCommand)
    -> Result<CommandTicket, PolkitCommandOutcome>;
    fn supervisor_state(&self) -> SupervisorState;
    fn telemetry(&self) -> DomainPortTelemetry;
    fn reset_quarantine(&self);
    fn shutdown(&self);

    fn select_identity(&self, cookie: &str, username: &str) {
        let _ = self.submit_command(PolkitCommand::SelectIdentity {
            cookie: cookie.to_string(),
            username: username.to_string(),
        });
    }

    fn provide_response(&self, cookie: &str, response: String) {
        let _ = self.submit_command(PolkitCommand::ProvideResponse {
            cookie: cookie.to_string(),
            response: response.into(),
        });
    }

    fn cancel_request(&self, cookie: &str) {
        let _ = self.submit_command(PolkitCommand::Cancel {
            cookie: cookie.to_string(),
        });
    }

    fn dismiss(&self) {
        let _ = self.submit_command(PolkitCommand::Dismiss);
    }

    fn mark_next_request_internal(&self) {
        let _ = self.submit_command(PolkitCommand::MarkNextRequestInternal);
    }
}
