use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use shilpo_domain::{
    CancellationReason, DomainLifecycle, DomainPortTelemetry, DomainSupervisor, DomainVersion,
    MailboxPolicy, StaleUpdateError, SupervisorState, TimeSource,
};
use tokio::sync::watch;

fn default_true() -> bool {
    true
}

/// All possible actions triggered when an idle timeout is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdleAction {
    /// No action performed.
    None,
    /// Lock the session (handled in #135).
    Lock,
    /// Turn off display outputs (handled in #260).
    ScreenOff,
    /// Turn on display outputs (handled in #260).
    ScreenOn,
    /// Suspend the system via logind.
    Suspend,
    /// Lock session before suspending via logind.
    LockAndSuspend,
    /// Execute a custom shell command.
    Command { command: String },
}

impl IdleAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Lock => "lock",
            Self::ScreenOff => "screen_off",
            Self::ScreenOn => "screen_on",
            Self::Suspend => "suspend",
            Self::LockAndSuspend => "lock_and_suspend",
            Self::Command { .. } => "command",
        }
    }
}

/// Configuration for an individual named idle behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdleBehaviorConfig {
    /// Whether this behavior is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Idle timeout in seconds before this behavior is triggered.
    pub timeout_seconds: f64,
    /// Action to execute upon reaching the timeout.
    pub action: IdleAction,
    /// Whether to attempt locking the screen before suspending (for Suspend action).
    #[serde(default)]
    pub lock_before_suspend: bool,
    /// Optional command to execute when the system resumes from idle.
    #[serde(default)]
    pub resume_command: String,
}

impl Default for IdleBehaviorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_seconds: 600.0,
            action: IdleAction::None,
            lock_before_suspend: false,
            resume_command: String::new(),
        }
    }
}

/// Metadata about an in-progress grace overlay fade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGraceInfo {
    pub grace_generation: u64,
    pub fade_ms: u32,
    pub behaviors: Vec<String>,
}

/// Sources of idle inhibitors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InhibitSource {
    /// org.freedesktop.ScreenSaver client cookie.
    ScreenSaver {
        cookie: u32,
        app: String,
        reason: String,
        sender: String,
    },
    /// org.freedesktop.login1 Manager BlockInhibited contains "idle".
    LogindBlockInhibited,
    /// In-process Caffeine manual toggle.
    Caffeine,
    /// Manual/Custom named inhibit.
    Named(String),
}

impl InhibitSource {
    /// Returns whether two sources identify the same protocol-owned inhibit.
    ///
    /// ScreenSaver clients release an inhibit with only the cookie returned by
    /// `Inhibit`; application and reason are descriptive metadata and are not
    /// repeated by `UnInhibit`. The D-Bus sender remains part of the identity so
    /// one client cannot release another client's cookie.
    pub fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::ScreenSaver { cookie, sender, .. },
                Self::ScreenSaver {
                    cookie: other_cookie,
                    sender: other_sender,
                    ..
                },
            ) => cookie == other_cookie && sender == other_sender,
            _ => self == other,
        }
    }
}

/// Atomic snapshot of the Idle domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleSnapshot {
    pub version: DomainVersion,
    pub lifecycle: DomainLifecycle,
    pub notifier_available: bool,
    pub registered_behaviors: u32,
    pub active_grace: Option<ActiveGraceInfo>,
    pub live_idle_seconds: u64,
    pub inhibit_count: u32,
    pub unsupported_actions: Vec<String>,
    pub last_error: Option<String>,
}

impl Default for IdleSnapshot {
    fn default() -> Self {
        Self {
            version: DomainVersion::ZERO,
            lifecycle: DomainLifecycle::Unavailable,
            notifier_available: false,
            registered_behaviors: 0,
            active_grace: None,
            live_idle_seconds: 0,
            inhibit_count: 0,
            unsupported_actions: Vec::new(),
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

/// Typed commands for the Idle domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IdleCommand {
    /// Update configured behaviors and grace duration.
    ConfigureBehaviors {
        behaviors: BTreeMap<String, IdleBehaviorConfig>,
        grace_seconds: f64,
    },
    /// Add an active inhibitor.
    AddInhibit { source: InhibitSource },
    /// Remove an active inhibitor.
    RemoveInhibit { source: InhibitSource },
    /// Clear all inhibitors from a specific D-Bus unique name sender (client disconnect).
    ClearInhibitsForSender { sender: String },
    /// Report that the UI grace overlay completed its fade.
    ReportGraceCompleted { grace_generation: u64 },
    /// Cancel any active grace overlay without firing actions.
    CancelGrace,
    /// Reset supervisor quarantine.
    ResetQuarantine,
}

impl IdleCommand {
    pub fn policy(&self) -> MailboxPolicy {
        match self {
            Self::ConfigureBehaviors { .. } => MailboxPolicy::ReplaceLatest {
                key: "configure_behaviors".to_string(),
            },
            _ => MailboxPolicy::Lossless,
        }
    }
}

/// Rejection reasons for Idle commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleRejectionReason {
    Unavailable,
    Overloaded,
    Unsupported { action: String },
    InvalidConfig { reason: String },
    NotFound,
}

impl fmt::Display for IdleRejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(f, "idle domain is unavailable"),
            Self::Overloaded => write!(f, "idle command mailbox is full"),
            Self::Unsupported { action } => {
                write!(f, "action '{action}' is not currently supported")
            }
            Self::InvalidConfig { reason } => write!(f, "invalid configuration: {reason}"),
            Self::NotFound => write!(f, "target item not found"),
        }
    }
}

/// Terminal outcome for accepted Idle commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleCommandOutcome {
    Applied {
        version: DomainVersion,
    },
    ReconciledApplied {
        version: DomainVersion,
    },
    Rejected {
        reason: IdleRejectionReason,
    },
    TimedOut {
        last_observed_version: DomainVersion,
    },
    Cancelled {
        reason: CancellationReason,
    },
}

/// Ticket returned upon command submission to monitor terminal outcome.
#[derive(Debug, Clone)]
pub struct CommandTicket {
    outcome: Arc<Mutex<Option<IdleCommandOutcome>>>,
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

    pub fn outcome(&self) -> Option<IdleCommandOutcome> {
        self.outcome.lock().unwrap().clone()
    }

    pub fn is_completed(&self) -> bool {
        self.outcome.lock().unwrap().is_some()
    }

    pub fn wait_timeout(
        &self,
        timeout: Duration,
        last_observed_version: DomainVersion,
    ) -> IdleCommandOutcome {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(outcome) = self.outcome() {
                return outcome;
            }
            if std::time::Instant::now() >= deadline {
                let mut guard = self.outcome.lock().unwrap();
                return guard
                    .get_or_insert_with(|| IdleCommandOutcome::TimedOut {
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
    outcome: Arc<Mutex<Option<IdleCommandOutcome>>>,
}

impl CommandResolver {
    pub fn resolve(&self, outcome: IdleCommandOutcome) -> bool {
        let mut guard = self.outcome.lock().unwrap();
        if guard.is_none() {
            *guard = Some(outcome);
            true
        } else {
            false
        }
    }
}

/// Revisioned domain port interface for Idle Management operations.
pub trait IdlePort: Send + Sync {
    fn snapshot(&self) -> IdleSnapshot;
    fn subscribe(&self) -> watch::Receiver<IdleSnapshot>;
    fn submit_command(&self, command: IdleCommand) -> Result<CommandTicket, IdleCommandOutcome>;
    fn supervisor_state(&self) -> SupervisorState;
    fn telemetry(&self) -> DomainPortTelemetry;
    fn reset_quarantine(&self);
    fn report_grace_completed(&self, grace_generation: u64) {
        let _ = self.submit_command(IdleCommand::ReportGraceCompleted { grace_generation });
    }
}
