use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
pub use shilpo_domain::{
    CancellationReason, DomainLifecycle, DomainPortTelemetry, DomainSupervisor, DomainVersion,
    MailboxPolicy, StaleUpdateError, SupervisorState, TimeSource,
};
use tokio::sync::watch;

use crate::secret::SecretString;

/// The current prompt / supplementary state presented to the user. Mirrors
/// `PolkitPromptState`, since PAM and polkit-agent-helper-1 both surface the same four
/// PAM message styles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthPromptState {
    /// Whether the user needs to provide input (password/response).
    pub response_required: bool,
    /// `true` for `PAM_PROMPT_ECHO_ON` (visible text), `false` for `PAM_PROMPT_ECHO_OFF`
    /// (masked password).
    pub response_visible: bool,
    /// The prompt text from PAM (e.g. "Password: ").
    pub input_prompt: Option<String>,
    /// Supplementary message text from PAM (e.g. `PAM_ERROR_MSG` or `PAM_TEXT_INFO`).
    pub supplementary_message: Option<String>,
    /// Whether the supplementary message is an error (`true`) or informational (`false`).
    pub supplementary_is_error: bool,
}

/// Terminal outcome of a completed authentication attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthOutcome {
    Succeeded,
    Failed { message: String },
}

/// Atomic snapshot of the PAM authentication domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSnapshot {
    pub version: DomainVersion,
    pub lifecycle: DomainLifecycle,
    /// `true` while a PAM conversation is in flight (a helper child is running).
    pub authenticating: bool,
    pub prompt_state: Option<AuthPromptState>,
    /// Set once a conversation reaches a terminal outcome; cleared when a new attempt
    /// begins.
    pub last_outcome: Option<AuthOutcome>,
    pub last_error: Option<String>,
}

impl Default for AuthSnapshot {
    fn default() -> Self {
        Self {
            version: DomainVersion::ZERO,
            lifecycle: DomainLifecycle::Unavailable,
            authenticating: false,
            prompt_state: None,
            last_outcome: None,
            last_error: None,
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

/// Typed PAM authentication domain commands.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthCommand {
    /// Starts a new PAM conversation for `service` (e.g. `"login"`). Any in-flight
    /// attempt is cancelled first.
    BeginAuthentication { service: String },
    /// Provides a response string (e.g. password) to the active prompt. Note: when
    /// processed, this memory is zeroed immediately.
    ProvideResponse { response: SecretString },
    /// Cancels the active authentication attempt, if any.
    CancelAuthentication,
    /// Resets supervisor quarantine.
    ResetQuarantine,
}

impl fmt::Debug for AuthCommand {
    /// Manual impl: `ProvideResponse` carries a raw credential and must never be
    /// printable via `{:?}` (derive would include it verbatim).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeginAuthentication { service } => f
                .debug_struct("BeginAuthentication")
                .field("service", service)
                .finish(),
            Self::ProvideResponse { .. } => f
                .debug_struct("ProvideResponse")
                .field("response", &"<redacted>")
                .finish(),
            Self::CancelAuthentication => write!(f, "CancelAuthentication"),
            Self::ResetQuarantine => write!(f, "ResetQuarantine"),
        }
    }
}

impl AuthCommand {
    pub fn policy(&self) -> MailboxPolicy {
        match self {
            // A newer BeginAuthentication supersedes an unapplied older one rather than
            // queuing both.
            Self::BeginAuthentication { .. } => MailboxPolicy::ReplaceLatest {
                key: "begin_authentication".to_string(),
            },
            _ => MailboxPolicy::Lossless,
        }
    }
}

/// Rejection reasons for auth commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthRejectionReason {
    Unavailable,
    Overloaded,
    NotAuthenticating,
}

impl fmt::Display for AuthRejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "auth domain is unavailable"),
            Self::Overloaded => write!(f, "auth command mailbox is full"),
            Self::NotAuthenticating => write!(f, "no authentication attempt is in progress"),
        }
    }
}

/// Terminal outcome for accepted auth commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthCommandOutcome {
    Applied {
        version: DomainVersion,
    },
    ReconciledApplied {
        version: DomainVersion,
    },
    Rejected {
        reason: AuthRejectionReason,
    },
    TimedOut {
        last_observed_version: DomainVersion,
    },
    Cancelled {
        reason: CancellationReason,
    },
}

/// Handle / ticket returned when a command is submitted.
#[derive(Debug, Clone)]
pub struct CommandTicket {
    outcome: Arc<Mutex<Option<AuthCommandOutcome>>>,
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

    pub fn outcome(&self) -> Option<AuthCommandOutcome> {
        self.outcome.lock().unwrap().clone()
    }

    pub fn is_completed(&self) -> bool {
        self.outcome.lock().unwrap().is_some()
    }

    pub fn wait_timeout(
        &self,
        timeout: Duration,
        last_observed_version: DomainVersion,
    ) -> AuthCommandOutcome {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(outcome) = self.outcome() {
                return outcome;
            }
            if std::time::Instant::now() >= deadline {
                let mut guard = self.outcome.lock().unwrap();
                return guard
                    .get_or_insert_with(|| AuthCommandOutcome::TimedOut {
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
    outcome: Arc<Mutex<Option<AuthCommandOutcome>>>,
}

impl CommandResolver {
    pub fn resolve(&self, outcome: AuthCommandOutcome) -> bool {
        let mut guard = self.outcome.lock().unwrap();
        if guard.is_none() {
            *guard = Some(outcome);
            true
        } else {
            false
        }
    }
}

/// Revisioned domain port interface for PAM authentication operations.
pub trait AuthPort: Send + Sync {
    fn snapshot(&self) -> AuthSnapshot;
    fn subscribe(&self) -> watch::Receiver<AuthSnapshot>;
    fn submit_command(&self, command: AuthCommand) -> Result<CommandTicket, AuthCommandOutcome>;
    fn supervisor_state(&self) -> SupervisorState;
    fn telemetry(&self) -> DomainPortTelemetry;
    fn reset_quarantine(&self);

    fn begin_authentication(&self, service: &str) {
        let _ = self.submit_command(AuthCommand::BeginAuthentication {
            service: service.to_string(),
        });
    }

    fn provide_response(&self, response: String) {
        let _ = self.submit_command(AuthCommand::ProvideResponse {
            response: response.into(),
        });
    }

    fn cancel_authentication(&self) {
        let _ = self.submit_command(AuthCommand::CancelAuthentication);
    }
}
