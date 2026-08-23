use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use shilpo_domain::{
    CancellationReason, DomainLifecycle, DomainPortTelemetry, DomainSupervisor, DomainVersion,
    MailboxPolicy, MonotonicTimeSource, SupervisorState, TimeSource,
};
use tokio::sync::{oneshot, watch};

use super::helper::{HelperEvent, PolkitHelper, PolkitHelperSession};
use super::types::{
    CommandId, CommandResolver, CommandTicket, PolkitCommand, PolkitCommandOutcome,
    PolkitPromptState, PolkitRejectionReason, PolkitRequest, PolkitSnapshot,
};

/// Default inactivity timeout window (2 minutes).
pub const DEFAULT_INACTIVITY_TIMEOUT_MS: u64 = 120_000;

/// How long a successful authentication stays displayed before the dialog is
/// torn down. Teardown on success is driven by `tick()` observing this window
/// elapse, not by the success event handler itself — the reference agent this
/// design is based on hit a real race where tying teardown directly to the
/// success callback could interfere with a `BeginAuthentication` that arrived
/// before the old session finished closing.
pub const SUCCESS_DISMISS_DELAY_MS: u64 = 900;

struct ActiveSession {
    request: PolkitRequest,
    helper_session: Option<Box<dyn PolkitHelperSession>>,
    completion_tx: Option<oneshot::Sender<Result<(), String>>>,
    last_interaction_ms: u64,
    /// Set once a `Success` event has been observed for this session. `tick()`
    /// clears the session and dialog once `SUCCESS_DISMISS_DELAY_MS` has
    /// elapsed since this timestamp.
    completed_at_ms: Option<u64>,
}

/// Outcome of applying a single helper event to an active session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventOutcome {
    /// A non-terminal prompt or supplementary message was applied.
    Prompt,
    /// Authentication failed; the session and dialog were cleared immediately.
    Failed,
    /// Authentication succeeded; `completion_tx` was resolved but the session
    /// and dialog are intentionally left in place for `tick()` to dismiss.
    Succeeded,
}

struct PendingCommandItem {
    _id: CommandId,
    command: PolkitCommand,
    generation: u64,
    resolver: CommandResolver,
}

struct InnerState {
    supervisor: DomainSupervisor,
    lifecycle: DomainLifecycle,
    owner_generation: u64,
    revision: u64,
    request: Option<PolkitRequest>,
    prompt_state: Option<PolkitPromptState>,
    last_error: Option<String>,
    helper_path: Option<String>,
    mark_next_internal: bool,
    had_prior_readiness: bool,
    queue: VecDeque<PendingCommandItem>,
    overloads: u64,
    supersessions: u64,
    restarts: u64,
    stale_updates: u64,
}

/// Core domain state machine for the PolicyKit authentication agent.
pub struct PolkitDomainState {
    capacity: usize,
    inactivity_timeout_ms: u64,
    time_source: Arc<dyn TimeSource>,
    helper: Arc<dyn PolkitHelper>,
    inner: Mutex<InnerState>,
    active_session: Mutex<Option<ActiveSession>>,
    watch_tx: watch::Sender<PolkitSnapshot>,
}

impl PolkitDomainState {
    pub fn new(capacity: usize, helper: Arc<dyn PolkitHelper>) -> Self {
        Self::with_time_source(
            capacity,
            helper,
            Arc::new(MonotonicTimeSource::new()),
            DEFAULT_INACTIVITY_TIMEOUT_MS,
        )
    }

    pub fn with_time_source(
        capacity: usize,
        helper: Arc<dyn PolkitHelper>,
        time_source: Arc<dyn TimeSource>,
        inactivity_timeout_ms: u64,
    ) -> Self {
        assert!(capacity > 0, "mailbox capacity must be positive");
        let initial_snapshot = PolkitSnapshot::default();
        let (watch_tx, _) = watch::channel(initial_snapshot);
        let helper_path = helper
            .probe_path()
            .map(|p| p.to_string_lossy().into_owned());

        Self {
            capacity,
            inactivity_timeout_ms,
            time_source,
            helper,
            inner: Mutex::new(InnerState {
                supervisor: DomainSupervisor::new(),
                lifecycle: DomainLifecycle::Unavailable,
                owner_generation: 0,
                revision: 0,
                request: None,
                prompt_state: None,
                last_error: None,
                helper_path,
                mark_next_internal: false,
                had_prior_readiness: false,
                queue: VecDeque::new(),
                overloads: 0,
                supersessions: 0,
                restarts: 0,
                stale_updates: 0,
            }),
            active_session: Mutex::new(None),
            watch_tx,
        }
    }

    pub fn snapshot(&self) -> PolkitSnapshot {
        let guard = self.inner.lock().unwrap();
        Self::snapshot_from(&guard)
    }

    fn snapshot_from(guard: &InnerState) -> PolkitSnapshot {
        PolkitSnapshot {
            version: DomainVersion::new(guard.owner_generation, guard.revision),
            lifecycle: guard.lifecycle,
            request: guard.request.clone(),
            prompt_state: guard.prompt_state.clone(),
            last_error: guard.last_error.clone(),
            helper_path: guard.helper_path.clone(),
        }
    }

    /// Builds the current snapshot from `guard` and publishes it to subscribers.
    fn publish_snapshot(&self, guard: &InnerState) {
        let snap = Self::snapshot_from(guard);
        let _ = self.watch_tx.send(snap);
    }

    pub fn subscribe(&self) -> watch::Receiver<PolkitSnapshot> {
        self.watch_tx.subscribe()
    }

    pub fn supervisor_state(&self) -> SupervisorState {
        self.inner.lock().unwrap().supervisor.state()
    }

    pub fn telemetry(&self) -> DomainPortTelemetry {
        let guard = self.inner.lock().unwrap();
        DomainPortTelemetry {
            owner_generation: guard.owner_generation,
            current_queue_depth: guard.queue.len(),
            queue_capacity: self.capacity,
            overloads: guard.overloads,
            supersessions: guard.supersessions,
            restarts: guard.restarts,
            stale_updates: guard.stale_updates,
            last_error: guard.last_error.clone(),
        }
    }

    pub fn time_source(&self) -> &Arc<dyn TimeSource> {
        &self.time_source
    }

    pub fn helper(&self) -> &Arc<dyn PolkitHelper> {
        &self.helper
    }

    pub fn begin_start(&self) -> u64 {
        let mut guard = self.inner.lock().unwrap();
        guard.owner_generation += 1;
        guard.restarts += 1;
        guard.revision = 0;
        guard.lifecycle = if guard.had_prior_readiness {
            DomainLifecycle::Reconnecting
        } else {
            DomainLifecycle::Connecting
        };
        guard.supervisor.mark_starting();
        guard.last_error = None;
        guard.helper_path = self
            .helper
            .probe_path()
            .map(|p| p.to_string_lossy().into_owned());

        self.publish_snapshot(&guard);
        guard.owner_generation
    }

    pub fn mark_ready(&self, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.lifecycle = DomainLifecycle::Ready;
        guard.had_prior_readiness = true;
        guard.last_error = None;
        guard.supervisor.mark_running(now_ms);
        guard.revision += 1;

        self.publish_snapshot(&guard);
    }

    pub fn report_owner_failure(&self, error: String, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.last_error = Some(error);
        guard.lifecycle = DomainLifecycle::Degraded;
        guard.supervisor.record_failure(now_ms);
        guard.revision += 1;

        self.publish_snapshot(&guard);
    }

    pub fn reset_quarantine(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.supervisor.reset_quarantine();
    }

    pub fn shutdown(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.lifecycle = DomainLifecycle::Unavailable;
        guard.supervisor.enter_stopped();
        guard.request = None;
        guard.prompt_state = None;
        guard.revision += 1;

        // Cancel any active session
        if let Some(mut session) = self.active_session.lock().unwrap().take() {
            if let Some(mut helper) = session.helper_session.take() {
                let _ = helper.kill();
            }
            if let Some(tx) = session.completion_tx.take() {
                let _ = tx.send(Err("Service shutdown".into()));
            }
        }

        // Cancel pending queue commands
        while let Some(item) = guard.queue.pop_front() {
            item.resolver.resolve(PolkitCommandOutcome::Cancelled {
                reason: CancellationReason::Shutdown,
            });
        }

        self.publish_snapshot(&guard);
    }

    pub fn tick(&self, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.supervisor.tick(now_ms);

        enum TimeoutKind {
            SuccessDismiss,
            Inactivity,
        }

        let due = {
            let session_opt = self.active_session.lock().unwrap();
            session_opt.as_ref().and_then(|session| {
                if let Some(completed_at) = session.completed_at_ms {
                    (now_ms.saturating_sub(completed_at) >= SUCCESS_DISMISS_DELAY_MS)
                        .then_some(TimeoutKind::SuccessDismiss)
                } else if now_ms.saturating_sub(session.last_interaction_ms)
                    >= self.inactivity_timeout_ms
                {
                    Some(TimeoutKind::Inactivity)
                } else {
                    None
                }
            })
        };

        match due {
            Some(TimeoutKind::SuccessDismiss) => {
                // The session already resolved `completion_tx` and cleared its
                // helper when the `Success` event was observed; just tear down
                // the dialog now that the display window has elapsed.
                *self.active_session.lock().unwrap() = None;
                guard.request = None;
                guard.prompt_state = None;
                guard.revision += 1;
                self.publish_snapshot(&guard);
            }
            Some(TimeoutKind::Inactivity) => {
                if let Some(mut session) = self.active_session.lock().unwrap().take() {
                    if let Some(mut helper) = session.helper_session.take() {
                        let _ = helper.kill();
                    }
                    if let Some(tx) = session.completion_tx.take() {
                        let _ = tx.send(Err("Inactivity timeout".into()));
                    }
                }
                guard.request = None;
                guard.prompt_state = None;
                guard.revision += 1;
                self.publish_snapshot(&guard);
            }
            None => {}
        }
    }

    /// Applies a helper event to `session`'s prompt/completion state.
    ///
    /// On `Success`, `completion_tx` is resolved immediately (the PolicyKit
    /// Authority's `BeginAuthentication` call must not be left hanging) but
    /// `guard.request`/`prompt_state` are deliberately left in place — the
    /// dialog is torn down later by `tick()`, not by this callback.
    fn apply_helper_event(
        guard: &mut InnerState,
        session: &mut ActiveSession,
        event: HelperEvent,
        now_ms: u64,
    ) -> EventOutcome {
        let mut prompt_state = guard.prompt_state.clone().unwrap_or_default();
        Self::apply_helper_event_to_prompt(&mut prompt_state, event.clone());

        match event {
            HelperEvent::Success => {
                if let Some(tx) = session.completion_tx.take() {
                    let _ = tx.send(Ok(()));
                }
                session.helper_session = None;
                session.completed_at_ms = Some(now_ms);
                prompt_state.supplementary_message = Some("Authentication successful.".into());
                prompt_state.supplementary_is_error = false;
                guard.prompt_state = Some(prompt_state);
                EventOutcome::Succeeded
            }
            HelperEvent::Failure => {
                if let Some(tx) = session.completion_tx.take() {
                    let _ = tx.send(Err("Authentication failed".into()));
                }
                guard.request = None;
                guard.prompt_state = None;
                EventOutcome::Failed
            }
            _ => {
                guard.prompt_state = Some(prompt_state);
                EventOutcome::Prompt
            }
        }
    }

    /// Handles an incoming `BeginAuthentication` request from the PolicyKit Authority.
    pub fn begin_authentication(
        &self,
        mut request: PolkitRequest,
        completion_tx: oneshot::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        let now_ms = self.time_source.now_ms();
        let mut guard = self.inner.lock().unwrap();

        // Apply internal-request marking if set
        if guard.mark_next_internal {
            request.is_internal = true;
            guard.mark_next_internal = false;
        }

        // Cancel previous active session if any was hanging
        if let Some(mut old_session) = self.active_session.lock().unwrap().take() {
            if let Some(mut helper) = old_session.helper_session.take() {
                let _ = helper.kill();
            }
            if let Some(tx) = old_session.completion_tx.take() {
                let _ = tx.send(Err("Superseded by new authentication request".into()));
            }
        }

        let mut helper_session = None;

        // If there's exactly one candidate identity, auto-select it and spawn helper session
        if request.identities.len() == 1 {
            let username = request.identities[0].user_name.clone();
            request.selected_identity = Some(username.clone());

            match self.helper.spawn_session(&username, &request.cookie) {
                Ok(session) => {
                    helper_session = Some(session);
                }
                Err(err) => {
                    let err_msg = format!("Failed to spawn polkit-agent-helper-1: {err}");
                    guard.last_error = Some(err_msg.clone());
                    let _ = completion_tx.send(Err(err_msg.clone()));
                    return Err(err_msg);
                }
            }
        } else if request.identities.is_empty() {
            let err_msg = "No identities provided by PolicyKit authority".to_string();
            guard.last_error = Some(err_msg.clone());
            let _ = completion_tx.send(Err(err_msg.clone()));
            return Err(err_msg);
        }

        guard.request = Some(request.clone());
        guard.prompt_state = Some(PolkitPromptState::default());
        guard.revision += 1;

        let mut active_session = ActiveSession {
            request: request.clone(),
            helper_session,
            completion_tx: Some(completion_tx),
            last_interaction_ms: now_ms,
            completed_at_ms: None,
        };

        // Pump any event the helper already produced (typically the first PAM
        // prompt, but in rare cached-credential PAM configurations this can
        // already be a terminal Success/Failure) through the same handling
        // used by later polling, so it isn't silently dropped.
        let mut outcome = None;
        if let Some(ref mut session) = active_session.helper_session
            && let Some(event) = session.try_recv_event()
        {
            outcome = Some(Self::apply_helper_event(
                &mut guard,
                &mut active_session,
                event,
                now_ms,
            ));
            guard.revision += 1;
        }

        if !matches!(outcome, Some(EventOutcome::Failed)) {
            *self.active_session.lock().unwrap() = Some(active_session);
        }

        self.publish_snapshot(&guard);

        Ok(())
    }

    /// Handles `CancelAuthentication` from the PolicyKit Authority.
    pub fn cancel_authentication(&self, cookie: &str) {
        let mut session_guard = self.active_session.lock().unwrap();
        let matches = session_guard
            .as_ref()
            .map(|s| s.request.cookie == cookie)
            .unwrap_or(false);

        if matches {
            if let Some(mut session) = session_guard.take() {
                if let Some(mut helper) = session.helper_session.take() {
                    let _ = helper.kill();
                }
                if let Some(tx) = session.completion_tx.take() {
                    let _ = tx.send(Err("Cancelled by PolicyKit authority".into()));
                }
            }

            let mut guard = self.inner.lock().unwrap();
            guard.request = None;
            guard.prompt_state = None;
            guard.revision += 1;

            self.publish_snapshot(&guard);
        }
    }

    /// Enqueues a typed domain command into the bounded mailbox without immediately draining.
    pub fn enqueue_command(
        &self,
        command: PolkitCommand,
    ) -> Result<CommandTicket, PolkitCommandOutcome> {
        let (ticket, resolver) = CommandTicket::new();
        let mut guard = self.inner.lock().unwrap();

        if guard.lifecycle == DomainLifecycle::Unavailable {
            resolver.resolve(PolkitCommandOutcome::Rejected {
                reason: PolkitRejectionReason::Unavailable,
            });
            return Err(PolkitCommandOutcome::Rejected {
                reason: PolkitRejectionReason::Unavailable,
            });
        }

        let policy = command.policy();
        match policy {
            MailboxPolicy::Lossless => {
                if guard.queue.len() >= self.capacity {
                    guard.overloads += 1;
                    resolver.resolve(PolkitCommandOutcome::Rejected {
                        reason: PolkitRejectionReason::Overloaded,
                    });
                    return Err(PolkitCommandOutcome::Rejected {
                        reason: PolkitRejectionReason::Overloaded,
                    });
                }
            }
            MailboxPolicy::ReplaceLatest { ref key } => {
                // Look for existing pending command with the same key
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
                if let Some(idx) = superseded_idx
                    && let Some(old) = guard.queue.remove(idx)
                {
                    guard.supersessions += 1;
                    old.resolver.resolve(PolkitCommandOutcome::Cancelled {
                        reason: CancellationReason::Superseded,
                    });
                }
            }
        }

        let generation = guard.owner_generation;
        let command_id = CommandId::generate();
        guard.queue.push_back(PendingCommandItem {
            _id: command_id,
            command,
            generation,
            resolver,
        });

        Ok(ticket)
    }

    /// Submits a typed domain command into the bounded mailbox and processes pending commands.
    pub fn submit_command(
        &self,
        command: PolkitCommand,
    ) -> Result<CommandTicket, PolkitCommandOutcome> {
        let ticket = self.enqueue_command(command)?;
        self.process_pending_commands();
        Ok(ticket)
    }

    pub fn process_pending_commands(&self) {
        let now_ms = self.time_source.now_ms();
        let mut guard = self.inner.lock().unwrap();
        let current_gen = guard.owner_generation;

        while let Some(item) = guard.queue.pop_front() {
            if item.generation != current_gen {
                item.resolver.resolve(PolkitCommandOutcome::Cancelled {
                    reason: CancellationReason::OwnerReplaced,
                });
                continue;
            }

            match item.command {
                PolkitCommand::MarkNextRequestInternal => {
                    guard.mark_next_internal = true;
                    guard.revision += 1;
                    let version = DomainVersion::new(guard.owner_generation, guard.revision);
                    item.resolver
                        .resolve(PolkitCommandOutcome::Applied { version });
                }
                PolkitCommand::SelectIdentity { cookie, username } => {
                    let mut session_guard = self.active_session.lock().unwrap();
                    let matches = session_guard
                        .as_ref()
                        .map(|s| s.request.cookie == cookie)
                        .unwrap_or(false);

                    if !matches {
                        item.resolver.resolve(PolkitCommandOutcome::Rejected {
                            reason: PolkitRejectionReason::NotFound,
                        });
                        continue;
                    }

                    let session = session_guard.as_mut().unwrap();
                    session.last_interaction_ms = now_ms;
                    session.request.selected_identity = Some(username.clone());

                    // If helper session is not spawned yet, spawn it now for chosen username
                    if session.helper_session.is_none() {
                        match self.helper.spawn_session(&username, &cookie) {
                            Ok(h_session) => {
                                session.helper_session = Some(h_session);
                            }
                            Err(err) => {
                                let err_msg = format!("Failed to spawn helper: {err}");
                                guard.last_error = Some(err_msg.clone());
                                if let Some(tx) = session.completion_tx.take() {
                                    let _ = tx.send(Err(err_msg));
                                }
                                item.resolver.resolve(PolkitCommandOutcome::Rejected {
                                    reason: PolkitRejectionReason::InvalidState,
                                });
                                continue;
                            }
                        }

                        // Pump any immediately-available first event (see the
                        // equivalent comment in `begin_authentication`).
                        if let Some(ref mut h_session) = session.helper_session
                            && let Some(event) = h_session.try_recv_event()
                        {
                            let outcome =
                                Self::apply_helper_event(&mut guard, session, event, now_ms);
                            guard.revision += 1;
                            if matches!(outcome, EventOutcome::Failed) {
                                drop(session_guard);
                                *self.active_session.lock().unwrap() = None;
                                let version =
                                    DomainVersion::new(guard.owner_generation, guard.revision);
                                item.resolver
                                    .resolve(PolkitCommandOutcome::Applied { version });
                                self.publish_snapshot(&guard);
                                continue;
                            }
                        }
                    }

                    guard.request = Some(session.request.clone());
                    guard.revision += 1;
                    let version = DomainVersion::new(guard.owner_generation, guard.revision);
                    item.resolver
                        .resolve(PolkitCommandOutcome::Applied { version });
                }
                PolkitCommand::ProvideResponse { cookie, response } => {
                    let mut session_guard = self.active_session.lock().unwrap();
                    let matches = session_guard
                        .as_ref()
                        .map(|s| s.request.cookie == cookie)
                        .unwrap_or(false);

                    if !matches {
                        item.resolver.resolve(PolkitCommandOutcome::Rejected {
                            reason: PolkitRejectionReason::NotFound,
                        });
                        continue;
                    }

                    let session = session_guard.as_mut().unwrap();
                    session.last_interaction_ms = now_ms;

                    if let Some(ref mut h_session) = session.helper_session {
                        let res = h_session.write_response(response.as_str());

                        if let Err(err) = res {
                            let err_msg = format!("Failed writing response to helper: {err}");
                            guard.last_error = Some(err_msg);
                            item.resolver.resolve(PolkitCommandOutcome::Rejected {
                                reason: PolkitRejectionReason::InvalidState,
                            });
                            continue;
                        }

                        let mut outcome = EventOutcome::Prompt;
                        if let Some(event) = h_session.try_recv_event() {
                            outcome = Self::apply_helper_event(&mut guard, session, event, now_ms);
                        }

                        guard.revision += 1;
                        let version = DomainVersion::new(guard.owner_generation, guard.revision);
                        item.resolver
                            .resolve(PolkitCommandOutcome::Applied { version });

                        if matches!(outcome, EventOutcome::Failed) {
                            drop(session_guard);
                            *self.active_session.lock().unwrap() = None;
                        }
                    } else {
                        item.resolver.resolve(PolkitCommandOutcome::Rejected {
                            reason: PolkitRejectionReason::InvalidState,
                        });
                    }
                }
                PolkitCommand::Cancel { cookie } => {
                    let mut session_guard = self.active_session.lock().unwrap();
                    let matches = session_guard
                        .as_ref()
                        .map(|s| s.request.cookie == cookie)
                        .unwrap_or(false);

                    if matches {
                        if let Some(mut session) = session_guard.take() {
                            if let Some(mut helper) = session.helper_session.take() {
                                let _ = helper.kill();
                            }
                            if let Some(tx) = session.completion_tx.take() {
                                let _ = tx.send(Err("Cancelled by user".into()));
                            }
                        }
                        guard.request = None;
                        guard.prompt_state = None;
                        guard.revision += 1;
                        let version = DomainVersion::new(guard.owner_generation, guard.revision);
                        item.resolver
                            .resolve(PolkitCommandOutcome::Applied { version });
                    } else {
                        item.resolver.resolve(PolkitCommandOutcome::Rejected {
                            reason: PolkitRejectionReason::NotFound,
                        });
                    }
                }
                PolkitCommand::Dismiss => {
                    if let Some(mut session) = self.active_session.lock().unwrap().take() {
                        if let Some(mut helper) = session.helper_session.take() {
                            let _ = helper.kill();
                        }
                        if let Some(tx) = session.completion_tx.take() {
                            let _ = tx.send(Err("Dismissed by user".into()));
                        }
                    }
                    guard.request = None;
                    guard.prompt_state = None;
                    guard.revision += 1;
                    let version = DomainVersion::new(guard.owner_generation, guard.revision);
                    item.resolver
                        .resolve(PolkitCommandOutcome::Applied { version });
                }
            }
        }

        self.publish_snapshot(&guard);
    }

    /// Advances the active helper event pump and applies event to prompt state.
    /// Never blocks: `try_recv_event` only returns events the background reader
    /// thread has already produced.
    pub fn poll_active_helper_event(&self) -> Option<HelperEvent> {
        let now_ms = self.time_source.now_ms();
        let mut session_guard = self.active_session.lock().unwrap();
        let session = session_guard.as_mut()?;
        let helper = session.helper_session.as_mut()?;
        let event = helper.try_recv_event()?;

        session.last_interaction_ms = now_ms;

        let mut guard = self.inner.lock().unwrap();
        let outcome = Self::apply_helper_event(&mut guard, session, event.clone(), now_ms);
        guard.revision += 1;
        self.publish_snapshot(&guard);
        drop(guard);

        if matches!(outcome, EventOutcome::Failed) {
            drop(session_guard);
            *self.active_session.lock().unwrap() = None;
        }

        Some(event)
    }

    fn apply_helper_event_to_prompt(prompt_state: &mut PolkitPromptState, event: HelperEvent) {
        match event {
            HelperEvent::PromptEchoOff(prompt) => {
                prompt_state.response_required = true;
                prompt_state.response_visible = false;
                prompt_state.input_prompt = Some(prompt);
            }
            HelperEvent::PromptEchoOn(prompt) => {
                prompt_state.response_required = true;
                prompt_state.response_visible = true;
                prompt_state.input_prompt = Some(prompt);
            }
            HelperEvent::ErrorMessage(text) => {
                prompt_state.supplementary_message = Some(text);
                prompt_state.supplementary_is_error = true;
            }
            HelperEvent::TextInfo(text) => {
                prompt_state.supplementary_message = Some(text);
                prompt_state.supplementary_is_error = false;
            }
            HelperEvent::Success | HelperEvent::Failure => {
                prompt_state.response_required = false;
                prompt_state.input_prompt = None;
            }
        }
    }
}
