//! Deliberately no separate `tests/auth_domain_port_contract.rs` using the shared
//! `support::domain_port_contract::DomainPortDriver` harness (unlike idle/notification/
//! device/compositor). That harness assumes a domain that receives free-form externally
//! *published* payload updates (`publish_update`/`publish_raw_update`) alongside typed
//! commands — the shape every other domain in this codebase has, because each reacts to
//! push-based events from an external source (Wayland, D-Bus, a compositor). The auth
//! domain has no such external payload stream: every state change here originates from a
//! typed command or from the helper's own conversation events, both already exercised
//! below. Forcing the generic harness on would mean adding a `publish_update` no one calls
//! for real, for a property the harness exists to test that doesn't apply here. The same
//! ADR-0006 properties it would check — snapshot version monotonicity, stale-generation
//! rejection, mailbox overflow, `ReplaceLatest` supersession, exactly-one-terminal-outcome,
//! and supervisor backoff/quarantine — are covered directly below instead.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::helper::{AuthHelper, AuthHelperEvent, AuthHelperSession, MockAuthHelper};
use super::state::AuthDomainState;
use super::types::{
    AuthCommand, AuthCommandOutcome, AuthOutcome, AuthRejectionReason, CancellationReason,
    DomainLifecycle, SupervisorState, TimeSource,
};
use crate::secret::SecretString;

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

fn setup() -> (Arc<AuthDomainState>, Arc<MockAuthHelper>, TestClock) {
    let clock = TestClock::new(10_000);
    let helper = Arc::new(MockAuthHelper::new());
    let state = Arc::new(AuthDomainState::with_time_source(
        8,
        helper.clone(),
        Arc::new(clock.clone()),
    ));
    state.begin_start();
    state.mark_ready(clock.now_ms());
    (state, helper, clock)
}

fn submit_and_process(state: &AuthDomainState, command: AuthCommand) -> AuthCommandOutcome {
    let ticket = state.submit_command(command).expect("command accepted");
    state.process_pending_commands();
    ticket.outcome().expect("command resolved synchronously")
}

// -----------------------------------------------------------------------
// PAM conversation state machine
// -----------------------------------------------------------------------

#[test]
fn single_prompt_response_success_flow() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![
        AuthHelperEvent::PromptEchoOff("Password:".into()),
        AuthHelperEvent::Success,
    ]);

    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );

    let snapshot = state.snapshot();
    assert!(snapshot.authenticating);
    let prompt = snapshot.prompt_state.expect("prompt present");
    assert!(prompt.response_required);
    assert!(!prompt.response_visible);
    assert_eq!(prompt.input_prompt.as_deref(), Some("Password:"));

    submit_and_process(
        &state,
        AuthCommand::ProvideResponse {
            response: "hunter2".into(),
        },
    );
    assert_eq!(helper.written_responses(), vec!["hunter2".to_string()]);

    // Drain the queued Success event.
    let event = state.poll_active_helper_event();
    assert_eq!(event, Some(AuthHelperEvent::Success));

    let snapshot = state.snapshot();
    assert_eq!(snapshot.last_outcome, Some(AuthOutcome::Succeeded));
    // Success stays visible until tick() dismisses it, not immediately cleared.
    assert!(snapshot.prompt_state.is_some());
}

#[test]
fn multi_prompt_sequence_is_not_one_shot() {
    // Proves the design isn't limited to a single stored password: distinct prompts get
    // distinct responses in order (e.g. password, then a one-time code).
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![
        AuthHelperEvent::PromptEchoOff("Password:".into()),
        AuthHelperEvent::PromptEchoOn("One-time code:".into()),
        AuthHelperEvent::Success,
    ]);

    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );

    submit_and_process(
        &state,
        AuthCommand::ProvideResponse {
            response: "hunter2".into(),
        },
    );
    let event = state.poll_active_helper_event();
    assert_eq!(
        event,
        Some(AuthHelperEvent::PromptEchoOn("One-time code:".into()))
    );
    let prompt = state.snapshot().prompt_state.unwrap();
    assert!(prompt.response_visible);
    assert_eq!(prompt.input_prompt.as_deref(), Some("One-time code:"));

    submit_and_process(
        &state,
        AuthCommand::ProvideResponse {
            response: "123456".into(),
        },
    );
    assert_eq!(
        helper.written_responses(),
        vec!["hunter2".to_string(), "123456".to_string()]
    );

    let event = state.poll_active_helper_event();
    assert_eq!(event, Some(AuthHelperEvent::Success));
    assert_eq!(state.snapshot().last_outcome, Some(AuthOutcome::Succeeded));
}

#[test]
fn failure_clears_prompt_and_records_message() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![
        AuthHelperEvent::PromptEchoOff("Password:".into()),
        AuthHelperEvent::Failure("Authentication failure".into()),
    ]);

    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );
    submit_and_process(
        &state,
        AuthCommand::ProvideResponse {
            response: "wrong".into(),
        },
    );

    let event = state.poll_active_helper_event();
    assert_eq!(
        event,
        Some(AuthHelperEvent::Failure("Authentication failure".into()))
    );

    let snapshot = state.snapshot();
    assert!(!snapshot.authenticating);
    assert!(snapshot.prompt_state.is_none());
    assert_eq!(
        snapshot.last_outcome,
        Some(AuthOutcome::Failed {
            message: "Authentication failure".into()
        })
    );
}

#[test]
fn error_msg_and_text_info_are_supplementary_not_prompts() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![
        AuthHelperEvent::ErrorMessage("Password expired".into()),
        AuthHelperEvent::PromptEchoOff("New password: ".into()),
        AuthHelperEvent::Success,
    ]);

    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );

    // The first event (ErrorMessage) is pumped synchronously by BeginAuthentication.
    let snapshot = state.snapshot();
    let prompt = snapshot.prompt_state.unwrap();
    assert!(
        !prompt.response_required,
        "supplementary message must not require a response"
    );
    assert_eq!(
        prompt.supplementary_message.as_deref(),
        Some("Password expired")
    );
    assert!(prompt.supplementary_is_error);
}

// -----------------------------------------------------------------------
// Credential handling
// -----------------------------------------------------------------------

#[test]
fn queued_response_is_zeroized_when_owner_is_replaced() {
    let (state, _helper, _clock) = setup();
    let (response, wipe) = SecretString::new_observed_for_test("superseded-password");

    state
        .submit_command(AuthCommand::ProvideResponse { response })
        .expect("queued response");
    state.begin_start();

    assert_eq!(wipe.bytes(), Some(vec![0; "superseded-password".len()]));
}

#[test]
fn rejected_response_is_zeroized() {
    let (state, _helper, _clock) = setup();
    let (response, wipe) = SecretString::new_observed_for_test("rejected-password");

    let outcome = submit_and_process(&state, AuthCommand::ProvideResponse { response });

    assert_eq!(
        outcome,
        AuthCommandOutcome::Rejected {
            reason: AuthRejectionReason::NotAuthenticating
        }
    );
    assert_eq!(wipe.bytes(), Some(vec![0; "rejected-password".len()]));
}

#[test]
fn successful_response_is_zeroized_after_helper_write() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::PromptEchoOff("Password:".into())]);
    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );
    let (response, wipe) = SecretString::new_observed_for_test("accepted-password");

    let outcome = submit_and_process(&state, AuthCommand::ProvideResponse { response });

    assert!(matches!(outcome, AuthCommandOutcome::Applied { .. }));
    assert_eq!(wipe.bytes(), Some(vec![0; "accepted-password".len()]));
}

struct WriteFailingHelper;

impl AuthHelper for WriteFailingHelper {
    fn spawn_session(&self, _service: &str) -> io::Result<Box<dyn AuthHelperSession>> {
        Ok(Box::new(WriteFailingSession { first_event: true }))
    }
}

struct WriteFailingSession {
    first_event: bool,
}

impl AuthHelperSession for WriteFailingSession {
    fn write_response(&mut self, _response: &str) -> io::Result<()> {
        Err(io::Error::other("injected write failure"))
    }

    fn try_recv_event(&mut self) -> Option<AuthHelperEvent> {
        self.first_event.then(|| {
            self.first_event = false;
            AuthHelperEvent::PromptEchoOff("Password:".into())
        })
    }

    fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_error_response_is_zeroized() {
    let state = AuthDomainState::new_ready_for_test(8, Arc::new(WriteFailingHelper));
    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );
    let (response, wipe) = SecretString::new_observed_for_test("write-error-password");

    let outcome = submit_and_process(&state, AuthCommand::ProvideResponse { response });

    assert!(matches!(outcome, AuthCommandOutcome::Applied { .. }));
    assert_eq!(wipe.bytes(), Some(vec![0; "write-error-password".len()]));
}

#[test]
fn response_command_serde_remains_string_shaped() {
    let command = AuthCommand::ProvideResponse {
        response: "serde-password".into(),
    };
    let json = serde_json::to_string(&command).unwrap();
    assert_eq!(json, r#"{"ProvideResponse":{"response":"serde-password"}}"#);
    let round_trip: AuthCommand = serde_json::from_str(&json).unwrap();
    assert_eq!(round_trip, command);
}

#[test]
fn provide_response_debug_output_redacts_password() {
    let command = AuthCommand::ProvideResponse {
        response: "super-secret".into(),
    };
    let debug = format!("{command:?}");
    assert!(!debug.contains("super-secret"));
    assert!(debug.contains("<redacted>"));
}

// -----------------------------------------------------------------------
// ADR-0006 domain port invariants
// -----------------------------------------------------------------------

#[test]
fn snapshot_version_increments_monotonically_per_applied_command() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::PromptEchoOff("Password:".into())]);

    let before = state.snapshot().version;
    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );
    let after = state.snapshot().version;
    assert!(after > before);
    assert_eq!(after.owner_generation, before.owner_generation);
}

#[test]
fn stale_generation_commands_are_cancelled_not_applied() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::PromptEchoOff("Password:".into())]);

    // Enqueue without draining, then restart the owner before processing: the queued
    // command's captured generation is now stale.
    let ticket = state
        .submit_command(AuthCommand::BeginAuthentication {
            service: "login".into(),
        })
        .expect("accepted");

    state.begin_start();
    state.mark_ready(state.time_source().now_ms());
    state.process_pending_commands();

    assert_eq!(
        ticket.outcome(),
        Some(AuthCommandOutcome::Cancelled {
            reason: CancellationReason::OwnerReplaced
        })
    );
}

#[test]
fn lossless_mailbox_rejects_overflow() {
    let (state, helper, _clock) = setup();
    // Fill the 8-slot capacity with Lossless commands (ProvideResponse is Lossless).
    for _ in 0..8 {
        state
            .submit_command(AuthCommand::ProvideResponse {
                response: "x".into(),
            })
            .expect("accepted under capacity");
    }
    let (response, wipe) = SecretString::new_observed_for_test("overflow-password");
    let rejected = state.submit_command(AuthCommand::ProvideResponse { response });
    assert!(matches!(
        rejected,
        Err(AuthCommandOutcome::Rejected {
            reason: AuthRejectionReason::Overloaded
        })
    ));
    assert_eq!(wipe.bytes(), Some(vec![0; "overflow-password".len()]));
    let _ = helper; // helper unused directly; kept for setup() destructuring symmetry
}

#[test]
fn replace_latest_supersedes_pending_begin_authentication() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::Success]);
    helper.queue_session(vec![AuthHelperEvent::Success]);

    let first = state
        .submit_command(AuthCommand::BeginAuthentication {
            service: "login".into(),
        })
        .expect("accepted");
    let second = state
        .submit_command(AuthCommand::BeginAuthentication {
            service: "login".into(),
        })
        .expect("accepted");

    state.process_pending_commands();

    assert_eq!(
        first.outcome(),
        Some(AuthCommandOutcome::Cancelled {
            reason: CancellationReason::Superseded
        })
    );
    assert!(matches!(
        second.outcome(),
        Some(AuthCommandOutcome::Applied { .. })
    ));
}

#[test]
fn exactly_one_terminal_outcome_per_command() {
    let (state, helper, _clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::Success]);
    let ticket = state
        .submit_command(AuthCommand::BeginAuthentication {
            service: "login".into(),
        })
        .expect("accepted");
    state.process_pending_commands();
    // Draining again must not re-resolve or panic; the ticket already has a terminal
    // outcome and further processing is a no-op for it.
    state.process_pending_commands();
    let outcome_first = ticket.outcome();
    assert!(outcome_first.is_some());
}

#[test]
fn supervisor_backoff_and_quarantine_reachable_via_report_owner_failure() {
    let (state, _helper, clock) = setup();
    for _ in 0..5 {
        state.report_owner_failure("simulated failure".into(), clock.now_ms());
        clock.advance_ms(100);
    }
    assert_eq!(state.supervisor_state(), SupervisorState::Quarantined);
    assert_eq!(state.snapshot().lifecycle, DomainLifecycle::Unavailable);

    state.reset_quarantine();
    assert!(!matches!(
        state.supervisor_state(),
        SupervisorState::Quarantined
    ));
}

// -----------------------------------------------------------------------
// Timeouts
// -----------------------------------------------------------------------

#[test]
fn success_dismiss_timer_clears_prompt_after_delay() {
    let (state, helper, clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::Success]);
    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );
    // BeginAuthentication already pumped the sole queued event synchronously.
    assert_eq!(state.snapshot().last_outcome, Some(AuthOutcome::Succeeded));
    assert!(state.snapshot().prompt_state.is_some());

    clock.advance_ms(super::state::SUCCESS_DISMISS_DELAY_MS + 1);
    state.tick(clock.now_ms());
    assert!(state.snapshot().prompt_state.is_none());
}

#[test]
fn inactivity_timeout_cancels_session_and_kills_helper() {
    let (state, helper, clock) = setup();
    helper.queue_session(vec![AuthHelperEvent::PromptEchoOff("Password:".into())]);
    submit_and_process(
        &state,
        AuthCommand::BeginAuthentication {
            service: "login".into(),
        },
    );

    clock.advance_ms(super::state::DEFAULT_INACTIVITY_TIMEOUT_MS + 1);
    state.tick(clock.now_ms());

    let snapshot = state.snapshot();
    assert!(snapshot.prompt_state.is_none());
    assert!(matches!(
        snapshot.last_outcome,
        Some(AuthOutcome::Failed { .. })
    ));
    assert_eq!(helper.killed_count(), 1);
}

#[test]
fn provide_response_without_active_attempt_is_rejected() {
    let (state, _helper, _clock) = setup();
    let outcome = submit_and_process(
        &state,
        AuthCommand::ProvideResponse {
            response: "x".into(),
        },
    );
    assert_eq!(
        outcome,
        AuthCommandOutcome::Rejected {
            reason: AuthRejectionReason::NotAuthenticating
        }
    );
}
