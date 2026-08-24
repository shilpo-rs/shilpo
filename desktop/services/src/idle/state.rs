use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::actions::{ActionExecutionOutcome, IdleActionSink, MockIdleActionSink};
use super::backend::{IdleBackendEvent, IdleNotifierBackend, MockIdleNotifier};
use super::types::{
    ActiveGraceInfo, CancellationReason, CommandId, CommandResolver, CommandTicket,
    DomainLifecycle, DomainPortTelemetry, DomainSupervisor, DomainVersion, IdleAction,
    IdleBehaviorConfig, IdleCommand, IdleCommandOutcome, IdlePort, IdleRejectionReason,
    IdleSnapshot, InhibitSource, MailboxPolicy, StaleUpdateError, SupervisorState, TimeSource,
};

const DEFAULT_GRACE_SECONDS: f64 = 2.0;
const FALLBACK_GRACE_BUFFER_MS: u64 = 250;
const HEARTBEAT_NOTIFICATION_ID: u32 = 0;
const HEARTBEAT_TIMEOUT_MS: u32 = 1000;

struct MailboxItem {
    _id: CommandId,
    command: IdleCommand,
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

    // Configured behaviors and grace
    grace_seconds: f64,
    behaviors: BTreeMap<String, IdleBehaviorConfig>,
    behavior_id_map: BTreeMap<u32, String>,
    next_behavior_id: u32,

    // Active grace overlay tracking
    grace_generation: u64,
    active_grace: Option<ActiveGraceInfo>,
    fallback_deadline_ms: Option<u64>,

    // Heartbeat & Live Idle state
    heartbeat_idled: bool,
    live_idle_seconds: u64,
    last_second_tick_ms: u64,

    // Inhibitor state
    inhibit_sources: Vec<InhibitSource>,
    idled_while_inhibited: HashSet<String>,

    // Diagnostics / telemetry
    warned_unsupported: HashSet<String>,
    unsupported_actions: Vec<String>,
    had_prior_readiness: bool,
    restarts: u64,
    overloads: u64,
    supersessions: u64,
    stale_updates: u64,
}

/// Core state machine and domain port logic for Idle Management.
pub struct IdleDomainState {
    inner: Mutex<InnerState>,
    backend: Arc<dyn IdleNotifierBackend>,
    action_sink: Arc<dyn IdleActionSink>,
    time_source: Arc<dyn TimeSource>,
    snapshot_tx: watch::Sender<IdleSnapshot>,
}

impl IdleDomainState {
    /// Creates a new `IdleDomainState` with default backend and action sink.
    pub fn new(
        capacity: usize,
        backend: Arc<dyn IdleNotifierBackend>,
        action_sink: Arc<dyn IdleActionSink>,
        time_source: Arc<dyn TimeSource>,
    ) -> Self {
        let initial_version = DomainVersion::new(0, 0);
        let initial_snapshot = IdleSnapshot {
            version: initial_version,
            lifecycle: DomainLifecycle::Unavailable,
            notifier_available: backend.is_available(),
            registered_behaviors: 0,
            active_grace: None,
            live_idle_seconds: 0,
            inhibit_count: 0,
            unsupported_actions: Vec::new(),
            last_error: None,
        };

        let (snapshot_tx, _) = watch::channel(initial_snapshot);

        let mut default_behaviors = BTreeMap::new();
        default_behaviors.insert(
            "lock".to_string(),
            IdleBehaviorConfig {
                enabled: true,
                timeout_seconds: 600.0,
                action: IdleAction::Lock,
                lock_before_suspend: false,
                resume_command: String::new(),
            },
        );
        default_behaviors.insert(
            "screen-off".to_string(),
            IdleBehaviorConfig {
                enabled: false,
                timeout_seconds: 660.0,
                action: IdleAction::ScreenOff,
                lock_before_suspend: false,
                resume_command: String::new(),
            },
        );
        default_behaviors.insert(
            "suspend".to_string(),
            IdleBehaviorConfig {
                enabled: false,
                timeout_seconds: 1800.0,
                action: IdleAction::Suspend,
                lock_before_suspend: true,
                resume_command: String::new(),
            },
        );

        let state = Self {
            inner: Mutex::new(InnerState {
                version: initial_version,
                lifecycle: DomainLifecycle::Unavailable,
                supervisor: DomainSupervisor::new(),
                queue: Vec::with_capacity(capacity),
                capacity,
                last_error: None,
                grace_seconds: DEFAULT_GRACE_SECONDS,
                behaviors: default_behaviors,
                behavior_id_map: BTreeMap::new(),
                next_behavior_id: 1,
                grace_generation: 0,
                active_grace: None,
                fallback_deadline_ms: None,
                heartbeat_idled: false,
                live_idle_seconds: 0,
                last_second_tick_ms: 0,
                inhibit_sources: Vec::new(),
                idled_while_inhibited: HashSet::new(),
                warned_unsupported: HashSet::new(),
                unsupported_actions: Vec::new(),
                had_prior_readiness: false,
                restarts: 0,
                overloads: 0,
                supersessions: 0,
                stale_updates: 0,
            }),
            backend,
            action_sink,
            time_source,
            snapshot_tx,
        };

        state.recompute_unsupported_actions();
        state
    }

    /// Creates an offline ready state for hermetic unit testing.
    pub fn new_ready_for_test(capacity: usize) -> Self {
        let backend = Arc::new(MockIdleNotifier::new());
        let action_sink = Arc::new(MockIdleActionSink::new());
        let time_source: Arc<dyn TimeSource> = Arc::new(shilpo_domain::MonotonicTimeSource::new());

        let state = Self::new(capacity, backend, action_sink, time_source.clone());
        state.begin_start();
        state.mark_ready(time_source.now_ms());
        state
    }

    pub fn time_source(&self) -> Arc<dyn TimeSource> {
        self.time_source.clone()
    }

    pub fn backend(&self) -> Arc<dyn IdleNotifierBackend> {
        self.backend.clone()
    }

    pub fn action_sink(&self) -> Arc<dyn IdleActionSink> {
        self.action_sink.clone()
    }

    pub fn snapshot(&self) -> IdleSnapshot {
        self.snapshot_tx.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<IdleSnapshot> {
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
    // Supervisor and Lifecycle Transitions
    // -----------------------------------------------------------------------

    pub fn begin_start(&self) {
        let mut guard = self.inner.lock().unwrap();
        let old_generation = guard.version.owner_generation;
        let new_generation = old_generation + 1;

        guard.supervisor.mark_starting();
        guard.version = DomainVersion::new(new_generation, 0);
        guard.lifecycle = if guard.had_prior_readiness {
            DomainLifecycle::Reconnecting
        } else {
            DomainLifecycle::Connecting
        };

        // Cancel older generation pending commands
        let old_queue = std::mem::take(&mut guard.queue);
        for item in old_queue {
            item.resolver.resolve(IdleCommandOutcome::Cancelled {
                reason: CancellationReason::OwnerReplaced,
            });
        }

        // Cancel any active grace
        guard.active_grace = None;
        guard.fallback_deadline_ms = None;

        self.publish_snapshot_locked(&mut guard);
    }

    pub fn mark_ready(&self, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        guard.supervisor.mark_running(now_ms);
        guard.had_prior_readiness = true;
        guard.lifecycle = DomainLifecycle::Ready;
        guard.last_error = None;

        // Register notifications with backend
        self.register_all_notifications_locked(&mut guard);

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

        // Unregister backend notifications during failure
        self.backend.unregister_all();
        guard.behavior_id_map.clear();
        guard.active_grace = None;
        guard.fallback_deadline_ms = None;

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

    pub fn tick(&self, now_ms: u64) {
        let mut guard = self.inner.lock().unwrap();
        let prev_state = guard.supervisor.state();
        guard.supervisor.tick(now_ms);
        let new_state = guard.supervisor.state();

        if matches!(prev_state, SupervisorState::Backoff { .. })
            && matches!(new_state, SupervisorState::Starting)
        {
            let old_generation = guard.version.owner_generation;
            let new_generation = old_generation + 1;
            guard.version = DomainVersion::new(new_generation, 0);
            guard.restarts += 1;
            let old_queue = std::mem::take(&mut guard.queue);
            for item in old_queue {
                item.resolver.resolve(IdleCommandOutcome::Cancelled {
                    reason: CancellationReason::OwnerReplaced,
                });
            }
            guard.active_grace = None;
            guard.fallback_deadline_ms = None;
            guard.lifecycle = if guard.had_prior_readiness {
                DomainLifecycle::Reconnecting
            } else {
                DomainLifecycle::Connecting
            };
        }

        // Advance live idle seconds if heartbeat is idled and no inhibits
        let is_inhibited = !guard.inhibit_sources.is_empty();
        if is_inhibited {
            guard.live_idle_seconds = 0;
        } else if guard.heartbeat_idled {
            if guard.last_second_tick_ms == 0 {
                guard.last_second_tick_ms = now_ms;
            } else if now_ms.saturating_sub(guard.last_second_tick_ms) >= 1000 {
                let elapsed_secs = (now_ms - guard.last_second_tick_ms) / 1000;
                guard.live_idle_seconds = guard.live_idle_seconds.saturating_add(elapsed_secs);
                guard.last_second_tick_ms += elapsed_secs * 1000;
            }
        }

        // Check fallback timer for grace overlay
        let mut grace_timed_out = false;
        let mut completed_grace_generation = 0;
        let mut behaviors_to_fire = Vec::new();

        if let Some(ref grace) = guard.active_grace
            && let Some(deadline) = guard.fallback_deadline_ms
            && now_ms >= deadline
        {
            grace_timed_out = true;
            completed_grace_generation = grace.grace_generation;
            behaviors_to_fire = grace.behaviors.clone();
        }

        if grace_timed_out {
            tracing::warn!(
                generation = completed_grace_generation,
                "idle grace overlay fallback timer fired; force-completing grace"
            );
            guard.active_grace = None;
            guard.fallback_deadline_ms = None;

            let next_rev = guard.version.revision + 1;
            guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
            self.publish_snapshot_locked(&mut guard);

            // Execute actions
            self.execute_actions_for_behaviors(&guard, behaviors_to_fire);
        } else {
            self.publish_snapshot_locked(&mut guard);
        }
    }

    // -----------------------------------------------------------------------
    // Backend Event Ingestion
    // -----------------------------------------------------------------------

    pub fn handle_backend_event(&self, event: IdleBackendEvent) {
        let mut guard = self.inner.lock().unwrap();
        let now_ms = self.time_source.now_ms();

        match event {
            IdleBackendEvent::Idled { id } => {
                if id == HEARTBEAT_NOTIFICATION_ID {
                    guard.heartbeat_idled = true;
                    guard.last_second_tick_ms = now_ms;
                    if guard.inhibit_sources.is_empty() && guard.live_idle_seconds == 0 {
                        guard.live_idle_seconds = 1;
                    }
                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    self.publish_snapshot_locked(&mut guard);
                    return;
                }

                let Some(behavior_name) = guard.behavior_id_map.get(&id).cloned() else {
                    return;
                };

                let is_inhibited = !guard.inhibit_sources.is_empty();
                if is_inhibited {
                    tracing::debug!(
                        behavior = %behavior_name,
                        "behavior idled while inhibited; recording for re-arming"
                    );
                    guard.idled_while_inhibited.insert(behavior_name);
                    return;
                }

                // If grace duration is 0, fire action immediately
                if guard.grace_seconds <= 0.0 {
                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    self.publish_snapshot_locked(&mut guard);

                    self.execute_actions_for_behaviors(&guard, vec![behavior_name]);
                    return;
                }

                // Start or join grace overlay
                let fade_ms = (guard.grace_seconds * 1000.0).max(1.0) as u32;

                if let Some(ref mut active) = guard.active_grace {
                    if !active.behaviors.contains(&behavior_name) {
                        active.behaviors.push(behavior_name);
                    }
                } else {
                    guard.grace_generation += 1;
                    let next_gen = guard.grace_generation;
                    guard.active_grace = Some(ActiveGraceInfo {
                        grace_generation: next_gen,
                        fade_ms,
                        behaviors: vec![behavior_name],
                    });
                    guard.fallback_deadline_ms =
                        Some(now_ms + fade_ms as u64 + FALLBACK_GRACE_BUFFER_MS);
                }

                let next_rev = guard.version.revision + 1;
                guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                self.publish_snapshot_locked(&mut guard);
            }
            IdleBackendEvent::Resumed { id } => {
                if id == HEARTBEAT_NOTIFICATION_ID {
                    guard.heartbeat_idled = false;
                    guard.live_idle_seconds = 0;
                    guard.last_second_tick_ms = 0;
                }

                // Cancel active grace overlay on any resume
                if guard.active_grace.is_some() {
                    tracing::debug!("system resumed from idle; cancelling active grace overlay");
                    guard.active_grace = None;
                    guard.fallback_deadline_ms = None;
                }

                guard.live_idle_seconds = 0;
                let next_rev = guard.version.revision + 1;
                guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                self.publish_snapshot_locked(&mut guard);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Command Submission and Processing
    // -----------------------------------------------------------------------

    pub fn submit_command(
        &self,
        command: IdleCommand,
    ) -> Result<CommandTicket, IdleCommandOutcome> {
        let mut guard = self.inner.lock().unwrap();

        if guard.lifecycle == DomainLifecycle::Unavailable {
            return Err(IdleCommandOutcome::Rejected {
                reason: IdleRejectionReason::Unavailable,
            });
        }

        match command.policy() {
            MailboxPolicy::Lossless => {
                if guard.queue.len() >= guard.capacity {
                    guard.overloads += 1;
                    return Err(IdleCommandOutcome::Rejected {
                        reason: IdleRejectionReason::Overloaded,
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
                    old.resolver.resolve(IdleCommandOutcome::Cancelled {
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
        let mut guard = self.inner.lock().unwrap();
        let items = std::mem::take(&mut guard.queue);

        for item in items {
            if item.generation != guard.version.owner_generation {
                item.resolver.resolve(IdleCommandOutcome::Cancelled {
                    reason: CancellationReason::OwnerReplaced,
                });
                continue;
            }

            match item.command {
                IdleCommand::ConfigureBehaviors {
                    behaviors,
                    grace_seconds,
                } => {
                    guard.grace_seconds = grace_seconds;
                    guard.behaviors = behaviors;
                    guard.active_grace = None;
                    guard.fallback_deadline_ms = None;

                    self.register_all_notifications_locked(&mut guard);
                    self.recompute_unsupported_actions_locked(&mut guard);

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
                IdleCommand::AddInhibit { source } => {
                    if !guard.inhibit_sources.contains(&source) {
                        guard.inhibit_sources.push(source);
                    }
                    guard.live_idle_seconds = 0;

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
                IdleCommand::RemoveInhibit { source } => {
                    let prev_count = guard.inhibit_sources.len();
                    guard
                        .inhibit_sources
                        .retain(|candidate| !candidate.same_identity(&source));
                    let new_count = guard.inhibit_sources.len();

                    // If last inhibit was released, recreate all notifications
                    if prev_count > 0 && new_count == 0 {
                        self.register_all_notifications_locked(&mut guard);
                    }

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
                IdleCommand::ClearInhibitsForSender { sender } => {
                    let prev_count = guard.inhibit_sources.len();
                    guard.inhibit_sources.retain(|s| match s {
                        InhibitSource::ScreenSaver {
                            sender: s_sender, ..
                        } => s_sender != &sender,
                        _ => true,
                    });
                    let new_count = guard.inhibit_sources.len();

                    if prev_count > 0 && new_count == 0 {
                        self.register_all_notifications_locked(&mut guard);
                    }

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
                IdleCommand::ReportGraceCompleted { grace_generation } => {
                    let mut behaviors_to_fire = Vec::new();
                    if let Some(ref grace) = guard.active_grace
                        && grace.grace_generation == grace_generation
                    {
                        behaviors_to_fire = grace.behaviors.clone();
                        guard.active_grace = None;
                        guard.fallback_deadline_ms = None;
                    }

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    // Teardown snapshot published BEFORE executing actions
                    self.publish_snapshot_locked(&mut guard);

                    if !behaviors_to_fire.is_empty() {
                        self.execute_actions_for_behaviors(&guard, behaviors_to_fire);
                    }

                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
                IdleCommand::CancelGrace => {
                    guard.active_grace = None;
                    guard.fallback_deadline_ms = None;

                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
                IdleCommand::ResetQuarantine => {
                    guard.supervisor.reset_quarantine();
                    let next_rev = guard.version.revision + 1;
                    guard.version = DomainVersion::new(guard.version.owner_generation, next_rev);
                    let version = guard.version;

                    self.publish_snapshot_locked(&mut guard);
                    item.resolver
                        .resolve(IdleCommandOutcome::Applied { version });
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal Helper Functions
    // -----------------------------------------------------------------------

    fn register_all_notifications_locked(&self, guard: &mut InnerState) {
        self.backend.unregister_all();
        guard.behavior_id_map.clear();
        guard.next_behavior_id = 1;

        if guard.lifecycle != DomainLifecycle::Ready {
            return;
        }

        // Register heartbeat notification (1000 ms)
        let _ = self
            .backend
            .register_notification(HEARTBEAT_NOTIFICATION_ID, HEARTBEAT_TIMEOUT_MS);

        // Register enabled behaviors
        for (name, cfg) in &guard.behaviors {
            if !cfg.enabled || cfg.timeout_seconds <= 0.0 || !cfg.timeout_seconds.is_finite() {
                tracing::debug!(behavior = %name, "skipping disabled or zero-timeout idle behavior");
                continue;
            }

            let id = guard.next_behavior_id;
            guard.next_behavior_id += 1;
            guard.behavior_id_map.insert(id, name.clone());

            let timeout_ms =
                ((cfg.timeout_seconds * 1000.0).ceil() as u64).clamp(1, u32::MAX as u64) as u32;

            if let Err(err) = self.backend.register_notification(id, timeout_ms) {
                tracing::warn!(behavior = %name, %err, "failed to register idle notification with backend");
            }
        }
    }

    fn recompute_unsupported_actions(&self) {
        let mut guard = self.inner.lock().unwrap();
        self.recompute_unsupported_actions_locked(&mut guard);
    }

    fn recompute_unsupported_actions_locked(&self, guard: &mut InnerState) {
        let mut unsupported = Vec::new();
        for (name, cfg) in &guard.behaviors {
            if cfg.enabled && !self.action_sink.has_handler_for(&cfg.action) {
                unsupported.push(name.clone());
                if !guard.warned_unsupported.contains(name) {
                    guard.warned_unsupported.insert(name.clone());
                    tracing::warn!(
                        behavior = %name,
                        action = cfg.action.name(),
                        "configured idle behavior specifies an action with no registered handler"
                    );
                }
            }
        }
        guard.unsupported_actions = unsupported;
    }

    fn execute_actions_for_behaviors(&self, guard: &InnerState, behaviors: Vec<String>) {
        for name in behaviors {
            let Some(cfg) = guard.behaviors.get(&name) else {
                continue;
            };

            let outcome =
                self.action_sink
                    .execute_action(&name, &cfg.action, cfg.lock_before_suspend);

            if matches!(outcome, ActionExecutionOutcome::Unsupported) {
                // The user-facing warning is emitted once per behavior by
                // `recompute_unsupported_actions_locked`; avoid repeating it on every firing.
                tracing::debug!(
                    behavior = %name,
                    action = cfg.action.name(),
                    "unsupported action fired; no handler registered"
                );
            }
        }
    }

    fn publish_snapshot_locked(&self, guard: &mut InnerState) {
        let snapshot = IdleSnapshot {
            version: guard.version,
            lifecycle: guard.lifecycle,
            notifier_available: self.backend.is_available(),
            registered_behaviors: guard.behavior_id_map.len() as u32,
            active_grace: guard.active_grace.clone(),
            live_idle_seconds: guard.live_idle_seconds,
            inhibit_count: guard.inhibit_sources.len() as u32,
            unsupported_actions: guard.unsupported_actions.clone(),
            last_error: guard.last_error.clone(),
        };

        let _ = self.snapshot_tx.send_replace(snapshot);
    }

    // -----------------------------------------------------------------------
    // Test Injection Hooks (Contract Testing)
    // -----------------------------------------------------------------------

    pub fn publish_update(
        &self,
        revision: u64,
        live_idle_seconds: u64,
    ) -> Result<(), StaleUpdateError> {
        let mut guard = self.inner.lock().unwrap();
        let target_version = DomainVersion::new(guard.version.owner_generation, revision);

        if target_version <= guard.version {
            guard.stale_updates += 1;
            return Err(StaleUpdateError::StaleVersion {
                current: guard.version,
                attempted: target_version,
            });
        }

        guard.version = target_version;
        guard.live_idle_seconds = live_idle_seconds;
        self.publish_snapshot_locked(&mut guard);
        Ok(())
    }

    pub fn publish_raw_update(
        &self,
        version: DomainVersion,
        lifecycle: DomainLifecycle,
        live_idle_seconds: u64,
        error: Option<String>,
    ) -> Result<(), StaleUpdateError> {
        let mut guard = self.inner.lock().unwrap();

        if version.owner_generation > guard.version.owner_generation {
            guard.stale_updates += 1;
            return Err(StaleUpdateError::UninstalledGeneration {
                installed: guard.version.owner_generation,
                attempted: version.owner_generation,
            });
        }

        if version < guard.version {
            guard.stale_updates += 1;
            return Err(StaleUpdateError::StaleVersion {
                current: guard.version,
                attempted: version,
            });
        }

        if version == guard.version {
            let current_snap = self.snapshot();
            let matches_identically = current_snap.lifecycle == lifecycle
                && current_snap.live_idle_seconds == live_idle_seconds
                && current_snap.last_error == error;

            if !matches_identically {
                guard.stale_updates += 1;
                return Err(StaleUpdateError::ConflictingSnapshot { version });
            }
            return Ok(());
        }

        guard.version = version;
        guard.lifecycle = lifecycle;
        guard.live_idle_seconds = live_idle_seconds;
        guard.last_error = error;
        self.publish_snapshot_locked(&mut guard);
        Ok(())
    }
}

impl IdlePort for IdleDomainState {
    fn snapshot(&self) -> IdleSnapshot {
        IdleDomainState::snapshot(self)
    }

    fn subscribe(&self) -> watch::Receiver<IdleSnapshot> {
        IdleDomainState::subscribe(self)
    }

    fn submit_command(&self, command: IdleCommand) -> Result<CommandTicket, IdleCommandOutcome> {
        IdleDomainState::submit_command(self, command)
    }

    fn supervisor_state(&self) -> SupervisorState {
        IdleDomainState::supervisor_state(self)
    }

    fn telemetry(&self) -> DomainPortTelemetry {
        IdleDomainState::telemetry(self)
    }

    fn reset_quarantine(&self) {
        IdleDomainState::reset_quarantine(self);
    }
}
