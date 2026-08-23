use std::sync::{Arc, Mutex};

use shilpo_domain::{MonotonicTimeSource, TimeSource};
use tokio::sync::watch;

use super::helper::{AuthHelper, AuthHelperEvent, AuthHelperSession};
use super::types::{
    AuthCommand, AuthCommandOutcome, AuthOutcome, AuthPort, AuthPromptState, AuthRejectionReason,
    AuthSnapshot, CancellationReason, CommandId, CommandResolver, CommandTicket, DomainLifecycle,
    DomainPortTelemetry, DomainSupervisor, DomainVersion, MailboxPolicy, SupervisorState,
};

/// Inactivity window after which an in-progress attempt with no interaction is cancelled.
pub const DEFAULT_INACTIVITY_TIMEOUT_MS: u64 = 120_000;

/// How long a successful authentication's `last_outcome` stays before being cleared by
/// `tick()`, so a slow subscriber still observes the success before the domain resets for
/// the next attempt.
pub const SUCCESS_DISMISS_DELAY_MS: u64 = 900;

struct ActiveSession {
    helper_session: Box<dyn AuthHelperSession>,
    last_interaction_ms: u64,
    completed_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventOutcome {
    Prompt,
    Failed,
    Succeeded,
}

struct MailboxItem {
    _id: CommandId,
    command: AuthCommand,
    generation: u64,
    resolver: CommandResolver,
}

struct InnerState {
    version: DomainVersion,
    lifecycle: DomainLifecycle,
    supervisor: DomainSupervisor,
    queue: Vec<MailboxItem>,
    capacity: usize,
    last_error: Option<String>,
    prompt_state: Option<AuthPromptState>,
    last_outcome: Option<AuthOutcome>,
    had_prior_readiness: bool,
    restarts: u64,
    overloads: u64,
    supersessions: u64,
    stale_updates: u64,
}

/// Core state machine and domain port logic for PAM authentication.
pub struct AuthDomainState {
    inner: Mutex<InnerState>,
    active_session: Mutex<Option<ActiveSession>>,
    helper: Arc<dyn AuthHelper>,
    time_source: Arc<dyn TimeSource>,
    snapshot_tx: watch::Sender<AuthSnapshot>,
}

impl AuthDomainState {
    pub fn new(capacity: usize, helper: Arc<dyn AuthHelper>) -> Self {
        Self::with_time_source(capacity, helper, Arc::new(MonotonicTimeSource::new()))
    }

    pub fn with_time_source(
        capacity: usize,
        helper: Arc<dyn AuthHelper>,
        time_source: Arc<dyn TimeSource>,
    ) -> Self {
        assert!(capacity > 0, "mailbox capacity must be positive");
        let (snapshot_tx, _) = watch::channel(AuthSnapshot::default());

        Self {
            inner: Mutex::new(InnerState {
                version: DomainVersion::ZERO,
                lifecycle: DomainLifecycle::Unavailable,
                supervisor: DomainSupervisor::new(),
                queue: Vec::with_capacity(capacity),
                capacity,
                last_error: None,
                prompt_state: None,
                last_outcome: None,
                had_prior_readiness: false,
                restarts: 0,
                overloads: 0,
                supersessions: 0,
                stale_updates: 0,
            }),
            active_session: Mutex::new(None),
            helper,
            time_source,
            snapshot_tx,
        }
    }

    /// Creates an offline ready state for hermetic unit testing.
    pub fn new_ready_for_test(capacity: usize, helper: Arc<dyn AuthHelper>) -> Self {
        let time_source: Arc<dyn TimeSource> = Arc::new(MonotonicTimeSource::new());
        let state = Self::with_time_source(capacity, helper, time_source.clone());
        state.begin_start();
        state.mark_ready(time_source.now_ms());
        state
    }

    pub fn time_source(&self) -> Arc<dyn TimeSource> {
        self.time_source.clone()
    }

    pub fn snapshot(&self) -> AuthSnapshot {
        self.snapshot_tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<AuthSnapshot> {
        self.snapshot_tx.subscribe()
    }

    pub fn supervisor_state(&self) -> SupervisorState {
        self.inner.lock().unwrap().supervisor.state()
    }

    pub fn telemetry(&self) -> DomainPortTelemetry {
        let guard = self.inner.lock().unwrap();
        DomainPortTelemetry {
            owner_generation: guard.version.owner_generation,
            current_queue_depth: guard.queue.len(),
            queue_capacity: guard.capacity,
            overloads: guard.overloads,
            supersessions: guard.supersessions,
            restarts: guard.restarts,
            stale_updates: guard.stale_updates,
            last_error: guard.last_error.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // Supervisor and lifecycle transitions
    // -----------------------------------------------------------------------

    pub fn begin_start(&self) {
        let mut guard = self.inner.lock().unwrap();
        let new_generation = guard.version.owner_generation + 1;

        guard.supervisor.mark_starting();
        guard.version = DomainVersion::new(new_generation, 0);
        guard.lifecycle = if guard.had_prior_readiness {
            DomainLifecycle::Reconnecting
        } else {
            DomainLifecycle::Connecting
        };

        let old_queue = std::mem::take(&mut guard.queue);
        for item in old_queue {
            item.resolver.resolve(AuthCommandOutcome::Cancelled {
                reason: CancellationReason::OwnerReplaced,
            });
        }

        self.clear_active_session_locked("owner restarted");
        self.publish_snapshot_locked(&mut guard);
    }

    pub fn mark_ready(&self, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.supervisor.mark_running(now_ms);
        guard.had_prior_readiness = true;
        guard.lifecycle = DomainLifecycle::Ready;
        guard.last_error = None;
        self.publish_snapshot_locked(&mut guard);
    }

    pub fn report_owner_failure(&self, error: String, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        let new_state = guard.supervisor.record_failure(now_ms);
        guard.last_error = Some(error);
        guard.lifecycle = if matches!(new_state, SupervisorState::Quarantined) {
            DomainLifecycle::Unavailable
        } else if guard.had_prior_readiness {
            DomainLifecycle::Reconnecting
        } else {
            DomainLifecycle::Unavailable
        };
        self.clear_active_session_locked("owner failure");
        self.publish_snapshot_locked(&mut guard);
    }

    pub fn reset_quarantine(&self) {
        let mut guard = self.inner.lock().unwrap();
        if guard.supervisor.reset_quarantine() {
            guard.lifecycle = if guard.had_prior_readiness {
                DomainLifecycle::Reconnecting
            } else {
                DomainLifecycle::Connecting
            };
            self.publish_snapshot_locked(&mut guard);
        }
    }

    /// Kills any running helper session without touching `inner` (caller holds or will
    /// take that lock separately).
    fn clear_active_session_locked(&self, _reason: &str) {
        if let Some(mut session) = self.active_session.lock().unwrap().take() {
            let _ = session.helper_session.kill();
        }
    }

    pub fn tick(&self, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.supervisor.tick(now_ms);

        enum Due {
            SuccessDismiss,
            Inactivity,
        }

        let due = {
            let session_opt = self.active_session.lock().unwrap();
            session_opt.as_ref().and_then(|session| {
                if let Some(completed_at) = session.completed_at_ms {
                    (now_ms.saturating_sub(completed_at) >= SUCCESS_DISMISS_DELAY_MS)
                        .then_some(Due::SuccessDismiss)
                } else if now_ms.saturating_sub(session.last_interaction_ms)
                    >= DEFAULT_INACTIVITY_TIMEOUT_MS
                {
                    Some(Due::Inactivity)
                } else {
                    None
                }
            })
        };

        match due {
            Some(Due::SuccessDismiss) => {
                *self.active_session.lock().unwrap() = None;
                guard.prompt_state = None;
                let next_rev = guard.version.revision + 1;
                guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                self.publish_snapshot_locked(&mut guard);
            }
            Some(Due::Inactivity) => {
                self.clear_active_session_locked("inactivity timeout");
                guard.prompt_state = None;
                guard.last_outcome = Some(AuthOutcome::Failed {
                    message: "inactivity timeout".into(),
                });
                let next_rev = guard.version.revision + 1;
                guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                self.publish_snapshot_locked(&mut guard);
            }
            None => {}
        }
    }

    // -----------------------------------------------------------------------
    // Helper event ingestion
    // -----------------------------------------------------------------------

    /// Polls the active session's helper for a new event, applying it to state if present.
    /// Intended to be called on a short period from the domain owner task, mirroring
    /// `PolkitDomainState::poll_active_helper_event`.
    pub fn poll_active_helper_event(&self) -> Option<AuthHelperEvent> {
        let now_ms = self.time_source.now_ms();
        let mut session_guard = self.active_session.lock().unwrap();
        let session = session_guard.as_mut()?;
        let event = session.helper_session.try_recv_event()?;
        session.last_interaction_ms = now_ms;

        let mut guard = self.inner.lock().unwrap();
        let outcome = Self::apply_helper_event(&mut guard, session, event.clone(), now_ms);
        let next_rev = guard.version.revision + 1;
        guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
        self.publish_snapshot_locked(&mut guard);
        drop(guard);

        if matches!(outcome, EventOutcome::Failed) {
            drop(session_guard);
            *self.active_session.lock().unwrap() = None;
        }

        Some(event)
    }

    fn apply_helper_event(
        guard: &mut InnerState,
        session: &mut ActiveSession,
        event: AuthHelperEvent,
        now_ms: u64,
    ) -> EventOutcome {
        let mut prompt_state = guard.prompt_state.clone().unwrap_or_default();
        Self::apply_helper_event_to_prompt(&mut prompt_state, event.clone());

        match event {
            AuthHelperEvent::Success => {
                session.completed_at_ms = Some(now_ms);
                prompt_state.supplementary_message = Some("Authentication successful.".into());
                prompt_state.supplementary_is_error = false;
                guard.prompt_state = Some(prompt_state);
                guard.last_outcome = Some(AuthOutcome::Succeeded);
                EventOutcome::Succeeded
            }
            AuthHelperEvent::Failure(message) => {
                guard.prompt_state = None;
                guard.last_outcome = Some(AuthOutcome::Failed { message });
                EventOutcome::Failed
            }
            _ => {
                guard.prompt_state = Some(prompt_state);
                EventOutcome::Prompt
            }
        }
    }

    fn apply_helper_event_to_prompt(prompt_state: &mut AuthPromptState, event: AuthHelperEvent) {
        match event {
            AuthHelperEvent::PromptEchoOff(prompt) => {
                prompt_state.response_required = true;
                prompt_state.response_visible = false;
                prompt_state.input_prompt = Some(prompt);
            }
            AuthHelperEvent::PromptEchoOn(prompt) => {
                prompt_state.response_required = true;
                prompt_state.response_visible = true;
                prompt_state.input_prompt = Some(prompt);
            }
            AuthHelperEvent::ErrorMessage(text) => {
                prompt_state.supplementary_message = Some(text);
                prompt_state.supplementary_is_error = true;
            }
            AuthHelperEvent::TextInfo(text) => {
                prompt_state.supplementary_message = Some(text);
                prompt_state.supplementary_is_error = false;
            }
            AuthHelperEvent::Success | AuthHelperEvent::Failure(_) => {
                prompt_state.response_required = false;
                prompt_state.input_prompt = None;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Command submission and processing
    // -----------------------------------------------------------------------

    /// Enqueues a command into the bounded mailbox without applying it. Kept split from
    /// `process_pending_commands` so the ADR-0006 conformance harness can drive the two
    /// steps independently; production callers (`AuthService`) must call
    /// `process_pending_commands` after enqueueing.
    pub fn submit_command(
        &self,
        command: AuthCommand,
    ) -> Result<CommandTicket, AuthCommandOutcome> {
        let mut guard = self.inner.lock().unwrap();

        if guard.lifecycle == DomainLifecycle::Unavailable {
            return Err(AuthCommandOutcome::Rejected {
                reason: AuthRejectionReason::Unavailable,
            });
        }

        match command.policy() {
            MailboxPolicy::Lossless => {
                if guard.queue.len() >= guard.capacity {
                    guard.overloads += 1;
                    return Err(AuthCommandOutcome::Rejected {
                        reason: AuthRejectionReason::Overloaded,
                    });
                }
            }
            MailboxPolicy::ReplaceLatest { ref key } => {
                let mut superseded_idx = None;
                for (idx, item) in guard.queue.iter().enumerate() {
                    if let MailboxPolicy::ReplaceLatest { key: ref item_key } =
                        item.command.policy()
                        && item_key == key
                    {
                        superseded_idx = Some(idx);
                        break;
                    }
                }
                if let Some(idx) = superseded_idx {
                    let old = guard.queue.remove(idx);
                    guard.supersessions += 1;
                    old.resolver.resolve(AuthCommandOutcome::Cancelled {
                        reason: CancellationReason::Superseded,
                    });
                }
            }
        }

        let generation = guard.version.owner_generation;
        let command_id = CommandId::generate();
        let (ticket, resolver) = CommandTicket::new();

        guard.queue.push(MailboxItem {
            _id: command_id,
            command,
            generation,
            resolver,
        });

        Ok(ticket)
    }

    pub fn process_pending_commands(&self) {
        let now_ms = self.time_source.now_ms();
        let mut guard = self.inner.lock().unwrap();
        let items = std::mem::take(&mut guard.queue);

        for item in items {
            if item.generation != guard.version.owner_generation {
                item.resolver.resolve(AuthCommandOutcome::Cancelled {
                    reason: CancellationReason::OwnerReplaced,
                });
                continue;
            }

            match item.command {
                AuthCommand::BeginAuthentication { service } => {
                    self.clear_active_session_locked("superseded by new attempt");
                    guard.prompt_state = Some(AuthPromptState::default());
                    guard.last_outcome = None;

                    match self.helper.spawn_session(&service) {
                        Ok(helper_session) => {
                            let mut session = ActiveSession {
                                helper_session,
                                last_interaction_ms: now_ms,
                                completed_at_ms: None,
                            };

                            // Pump any event the helper already produced (typically the
                            // first PAM prompt, but in rare cached-credential PAM
                            // configurations this can already be terminal) so it isn't
                            // silently dropped.
                            if let Some(event) = session.helper_session.try_recv_event() {
                                Self::apply_helper_event(&mut guard, &mut session, event, now_ms);
                            }

                            let is_failed =
                                matches!(guard.last_outcome, Some(AuthOutcome::Failed { .. }))
                                    && guard.prompt_state.is_none();

                            if !is_failed {
                                *self.active_session.lock().unwrap() = Some(session);
                            }
                        }
                        Err(err) => {
                            let message = format!("failed to spawn PAM helper: {err}");
                            guard.last_error = Some(message.clone());
                            guard.prompt_state = None;
                            guard.last_outcome = Some(AuthOutcome::Failed { message });
                        }
                    }

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;
                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(AuthCommandOutcome::Applied { version });
                }
                AuthCommand::ProvideResponse { response } => {
                    let mut session_guard = self.active_session.lock().unwrap();
                    let Some(session) = session_guard.as_mut() else {
                        item.resolver.resolve(AuthCommandOutcome::Rejected {
                            reason: AuthRejectionReason::NotAuthenticating,
                        });
                        continue;
                    };

                    session.last_interaction_ms = now_ms;
                    let write_result = session.helper_session.write_response(response.as_str());
                    drop(session_guard);

                    if let Err(err) = write_result {
                        guard.last_error = Some(format!("failed to write PAM response: {err}"));
                    }

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;
                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(AuthCommandOutcome::Applied { version });
                }
                AuthCommand::CancelAuthentication => {
                    self.clear_active_session_locked("cancelled");
                    guard.prompt_state = None;
                    guard.last_outcome = Some(AuthOutcome::Failed {
                        message: "cancelled".into(),
                    });

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;
                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(AuthCommandOutcome::Applied { version });
                }
                AuthCommand::ResetQuarantine => {
                    guard.supervisor.reset_quarantine();
                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;
                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(AuthCommandOutcome::Applied { version });
                }
            }
        }
    }

    fn snapshot_from(guard: &InnerState) -> AuthSnapshot {
        AuthSnapshot {
            version: guard.version,
            lifecycle: guard.lifecycle,
            authenticating: guard.prompt_state.is_some() && guard.last_outcome.is_none(),
            prompt_state: guard.prompt_state.clone(),
            last_outcome: guard.last_outcome.clone(),
            last_error: guard.last_error.clone(),
        }
    }

    fn publish_snapshot_locked(&self, guard: &mut InnerState) {
        let snapshot = Self::snapshot_from(guard);
        let _ = self.snapshot_tx.send_replace(snapshot);
    }
}

impl AuthPort for AuthDomainState {
    fn snapshot(&self) -> AuthSnapshot {
        AuthDomainState::snapshot(self)
    }

    fn subscribe(&self) -> watch::Receiver<AuthSnapshot> {
        AuthDomainState::subscribe(self)
    }

    fn submit_command(&self, command: AuthCommand) -> Result<CommandTicket, AuthCommandOutcome> {
        AuthDomainState::submit_command(self, command)
    }

    fn supervisor_state(&self) -> SupervisorState {
        AuthDomainState::supervisor_state(self)
    }

    fn telemetry(&self) -> DomainPortTelemetry {
        AuthDomainState::telemetry(self)
    }

    fn reset_quarantine(&self) {
        AuthDomainState::reset_quarantine(self)
    }
}
