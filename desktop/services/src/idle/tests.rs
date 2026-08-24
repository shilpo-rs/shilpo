use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::actions::MockIdleActionSink;
use super::backend::{IdleBackendEvent, MockIdleNotifier};
use super::state::IdleDomainState;
use super::types::{
    DomainLifecycle, IdleAction, IdleBehaviorConfig, IdleCommand, InhibitSource, TimeSource,
};

#[derive(Debug, Clone, Default)]
struct TestClock {
    now_ms: Arc<AtomicU64>,
}

impl TestClock {
    fn new(start_ms: u64) -> Self {
        Self {
            now_ms: Arc::new(AtomicU64::new(start_ms)),
        }
    }

    fn advance_ms(&self, ms: u64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl TimeSource for TestClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

fn setup_test_domain(
    grace_seconds: f64,
) -> (
    Arc<IdleDomainState>,
    Arc<MockIdleNotifier>,
    Arc<MockIdleActionSink>,
    TestClock,
) {
    let clock = TestClock::new(10_000);
    let backend = Arc::new(MockIdleNotifier::new());
    let action_sink = Arc::new(MockIdleActionSink::new());

    let state = Arc::new(IdleDomainState::new(
        32,
        backend.clone(),
        action_sink.clone(),
        Arc::new(clock.clone()),
    ));

    state.begin_start();
    state.mark_ready(clock.now_ms());

    // Configure test behaviors
    let mut behaviors = BTreeMap::new();
    behaviors.insert(
        "lock".to_string(),
        IdleBehaviorConfig {
            enabled: true,
            timeout_seconds: 600.0,
            action: IdleAction::Lock,
            lock_before_suspend: false,
            resume_command: String::new(),
        },
    );
    behaviors.insert(
        "suspend".to_string(),
        IdleBehaviorConfig {
            enabled: true,
            timeout_seconds: 1800.0,
            action: IdleAction::Suspend,
            lock_before_suspend: true,
            resume_command: String::new(),
        },
    );
    behaviors.insert(
        "cmd".to_string(),
        IdleBehaviorConfig {
            enabled: true,
            timeout_seconds: 300.0,
            action: IdleAction::Command {
                command: "echo test".to_string(),
            },
            lock_before_suspend: false,
            resume_command: "echo resume".to_string(),
        },
    );
    behaviors.insert(
        "disabled".to_string(),
        IdleBehaviorConfig {
            enabled: false,
            timeout_seconds: 120.0,
            action: IdleAction::ScreenOff,
            lock_before_suspend: false,
            resume_command: String::new(),
        },
    );
    behaviors.insert(
        "zero_time".to_string(),
        IdleBehaviorConfig {
            enabled: true,
            timeout_seconds: 0.0,
            action: IdleAction::ScreenOff,
            lock_before_suspend: false,
            resume_command: String::new(),
        },
    );

    let _ = state.submit_command(IdleCommand::ConfigureBehaviors {
        behaviors,
        grace_seconds,
    });
    state.process_pending_commands();

    (state, backend, action_sink, clock)
}

#[test]
fn test_behavior_registration_filtering_and_timeouts() {
    let (state, backend, _, _) = setup_test_domain(2.0);

    let snap = state.snapshot();
    assert_eq!(snap.lifecycle, DomainLifecycle::Ready);
    // 3 enabled behaviors with positive timeout + 1 heartbeat
    assert_eq!(snap.registered_behaviors, 3);

    let registered = backend.registered_map();
    // Heartbeat ID 0 at 1000ms
    assert_eq!(registered.get(&0), Some(&1000));
    // Check timeouts in ms
    assert!(registered.values().any(|&ms| ms == 300_000));
    assert!(registered.values().any(|&ms| ms == 600_000));
    assert!(registered.values().any(|&ms| ms == 1_800_000));
    // Disabled and zero_time are not registered
    assert_eq!(registered.len(), 4); // 3 behaviors + 1 heartbeat
}

#[test]
fn test_unsupported_actions_diagnostics_and_dispatch() {
    let (state, _, action_sink, _) = setup_test_domain(2.0);

    let _snap = state.snapshot();
    // "lock" action has no mock handler registered by default
    action_sink.set_supported("lock", false);
    state
        .submit_command(IdleCommand::ConfigureBehaviors {
            behaviors: {
                let mut b = BTreeMap::new();
                b.insert(
                    "lock_beh".into(),
                    IdleBehaviorConfig {
                        enabled: true,
                        timeout_seconds: 10.0,
                        action: IdleAction::Lock,
                        lock_before_suspend: false,
                        resume_command: "".into(),
                    },
                );
                b
            },
            grace_seconds: 0.0,
        })
        .unwrap();
    state.process_pending_commands();

    let snap = state.snapshot();
    assert!(snap.unsupported_actions.contains(&"lock_beh".to_string()));

    // Idling should execute through sink and produce Unsupported outcome
    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    let executed = action_sink.executed_actions();
    assert!(executed.is_empty());
}

#[test]
fn test_lock_and_suspend_with_no_lock_handler_still_suspends() {
    let (state, _, action_sink, _) = setup_test_domain(0.0);

    action_sink.set_supported("lock", false);
    action_sink.set_supported("suspend", true);

    state
        .submit_command(IdleCommand::ConfigureBehaviors {
            behaviors: {
                let mut b = BTreeMap::new();
                b.insert(
                    "suspend_beh".into(),
                    IdleBehaviorConfig {
                        enabled: true,
                        timeout_seconds: 10.0,
                        action: IdleAction::LockAndSuspend,
                        lock_before_suspend: true,
                        resume_command: "".into(),
                    },
                );
                b
            },
            grace_seconds: 0.0,
        })
        .unwrap();
    state.process_pending_commands();

    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    let executed = action_sink.executed_actions();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].0, "suspend_beh");
    assert_eq!(executed[0].1, IdleAction::LockAndSuspend);
}

#[test]
fn test_grace_overlay_join_model_and_completion() {
    let (state, _, action_sink, _) = setup_test_domain(2.0);
    action_sink.set_supported("lock", true);

    // Behavior 1 idles -> starts grace
    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    let snap = state.snapshot();
    assert!(snap.active_grace.is_some());
    let grace1 = snap.active_grace.unwrap();
    assert_eq!(grace1.fade_ms, 2000);
    assert_eq!(grace1.behaviors.len(), 1);

    // Behavior 2 idles mid-fade -> joins grace
    state.handle_backend_event(IdleBackendEvent::Idled { id: 2 });
    let snap = state.snapshot();
    let grace2 = snap.active_grace.unwrap();
    assert_eq!(grace2.grace_generation, grace1.grace_generation);
    assert_eq!(grace2.behaviors.len(), 2);

    // Report grace completed with matching generation
    state
        .submit_command(IdleCommand::ReportGraceCompleted {
            grace_generation: grace2.grace_generation,
        })
        .unwrap();
    state.process_pending_commands();

    // Overlay is torn down before actions execute
    let snap = state.snapshot();
    assert!(snap.active_grace.is_none());

    // Both behaviors fired
    let executed = action_sink.executed_actions();
    assert_eq!(executed.len(), 2);
}

#[test]
fn test_grace_cancelled_on_resume() {
    let (state, _, action_sink, _) = setup_test_domain(2.0);

    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    assert!(state.snapshot().active_grace.is_some());

    // Resume event arrives during fade
    state.handle_backend_event(IdleBackendEvent::Resumed { id: 1 });
    assert!(state.snapshot().active_grace.is_none());

    // No actions should fire
    let executed = action_sink.executed_actions();
    assert!(executed.is_empty());
}

#[test]
fn test_grace_fallback_timer() {
    let (state, _, action_sink, clock) = setup_test_domain(2.0);

    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    assert!(state.snapshot().active_grace.is_some());

    // Advance clock past fade_ms (2000) + fallback buffer (250)
    clock.advance_ms(2300);
    state.tick(clock.now_ms());

    // Grace completed via fallback timer
    assert!(state.snapshot().active_grace.is_none());
    assert_eq!(action_sink.executed_actions().len(), 1);
}

#[test]
fn test_grace_superseded_generation_callback_ignored() {
    let (state, _, action_sink, _) = setup_test_domain(2.0);

    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    let old_gen = state.snapshot().active_grace.unwrap().grace_generation;

    // Cancel and restart a new grace
    state.handle_backend_event(IdleBackendEvent::Resumed { id: 1 });
    state.handle_backend_event(IdleBackendEvent::Idled { id: 2 });
    let new_gen = state.snapshot().active_grace.unwrap().grace_generation;
    assert_ne!(old_gen, new_gen);

    // Stale completion for old_gen should be ignored
    state
        .submit_command(IdleCommand::ReportGraceCompleted {
            grace_generation: old_gen,
        })
        .unwrap();
    state.process_pending_commands();

    // New grace is still active and no actions fired yet
    assert!(state.snapshot().active_grace.is_some());
    assert!(action_sink.executed_actions().is_empty());
}

#[test]
fn test_inhibits_cookie_accounting_and_action_suppression() {
    let (state, _, action_sink, _) = setup_test_domain(0.0);

    let cookie_source = InhibitSource::ScreenSaver {
        cookie: 42,
        app: "firefox".into(),
        reason: "video".into(),
        sender: ":1.100".into(),
    };

    state
        .submit_command(IdleCommand::AddInhibit {
            source: cookie_source.clone(),
        })
        .unwrap();
    state.process_pending_commands();

    let snap = state.snapshot();
    assert_eq!(snap.inhibit_count, 1);
    assert_eq!(snap.live_idle_seconds, 0);

    // Idle event while inhibited is suppressed
    state.handle_backend_event(IdleBackendEvent::Idled { id: 1 });
    assert!(action_sink.executed_actions().is_empty());

    // Releasing inhibit
    state
        .submit_command(IdleCommand::RemoveInhibit {
            source: cookie_source,
        })
        .unwrap();
    state.process_pending_commands();

    assert_eq!(state.snapshot().inhibit_count, 0);
}

#[test]
fn screensaver_uninhibit_matches_protocol_identity_not_metadata() {
    let (state, _, _, _) = setup_test_domain(0.0);

    state
        .submit_command(IdleCommand::AddInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 42,
                app: "firefox".into(),
                reason: "video playback".into(),
                sender: ":1.100".into(),
            },
        })
        .unwrap();
    state.process_pending_commands();

    state
        .submit_command(IdleCommand::RemoveInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 42,
                app: String::new(),
                reason: String::new(),
                sender: ":1.100".into(),
            },
        })
        .unwrap();
    state.process_pending_commands();

    assert_eq!(state.snapshot().inhibit_count, 0);
}

#[test]
fn screensaver_uninhibit_cannot_release_another_senders_cookie() {
    let (state, _, _, _) = setup_test_domain(0.0);

    state
        .submit_command(IdleCommand::AddInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 42,
                app: "firefox".into(),
                reason: "video playback".into(),
                sender: ":1.100".into(),
            },
        })
        .unwrap();
    state.process_pending_commands();

    state
        .submit_command(IdleCommand::RemoveInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 42,
                app: String::new(),
                reason: String::new(),
                sender: ":1.101".into(),
            },
        })
        .unwrap();
    state.process_pending_commands();

    assert_eq!(state.snapshot().inhibit_count, 1);
}

#[test]
fn test_sender_disconnect_clears_inhibits() {
    let (state, _, _, _) = setup_test_domain(2.0);

    state
        .submit_command(IdleCommand::AddInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 1,
                app: "app1".into(),
                reason: "r1".into(),
                sender: ":1.50".into(),
            },
        })
        .unwrap();
    state
        .submit_command(IdleCommand::AddInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 2,
                app: "app2".into(),
                reason: "r2".into(),
                sender: ":1.50".into(),
            },
        })
        .unwrap();
    state
        .submit_command(IdleCommand::AddInhibit {
            source: InhibitSource::ScreenSaver {
                cookie: 3,
                app: "other".into(),
                reason: "r3".into(),
                sender: ":1.99".into(),
            },
        })
        .unwrap();
    state.process_pending_commands();

    assert_eq!(state.snapshot().inhibit_count, 3);

    // Client :1.50 disconnects
    state
        .submit_command(IdleCommand::ClearInhibitsForSender {
            sender: ":1.50".into(),
        })
        .unwrap();
    state.process_pending_commands();

    assert_eq!(state.snapshot().inhibit_count, 1);
}

#[test]
fn test_heartbeat_idle_counter_increment_and_reset() {
    let (state, _, _, clock) = setup_test_domain(2.0);

    assert_eq!(state.snapshot().live_idle_seconds, 0);

    // Heartbeat idles (ID 0)
    state.handle_backend_event(IdleBackendEvent::Idled { id: 0 });
    assert_eq!(state.snapshot().live_idle_seconds, 1);

    // Advance clock 3 seconds and tick
    clock.advance_ms(3000);
    state.tick(clock.now_ms());
    assert_eq!(state.snapshot().live_idle_seconds, 4);

    // Resumed resets counter to 0
    state.handle_backend_event(IdleBackendEvent::Resumed { id: 0 });
    assert_eq!(state.snapshot().live_idle_seconds, 0);
}
