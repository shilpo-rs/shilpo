use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use shilpo_domain::{CancellationReason, DomainLifecycle, SupervisorState, TimeSource};
use tokio::sync::oneshot;

use super::agent::{AuthorityClient, authorize_polkit_caller_with};
use super::helper::{
    HelperEvent, MockPolkitHelper, probe_system_helper_path, zeroize_bytes, zeroize_string,
};
use super::state::{PolkitDomainState, SUCCESS_DISMISS_DELAY_MS};
use super::types::{
    PolkitCommand, PolkitCommandOutcome, PolkitIdentity, PolkitRejectionReason, PolkitRequest,
};
use crate::secret::{SecretString, zeroize_bytes, zeroize_string};

#[derive(Debug, Clone, Default)]
struct ManualClock {
    now_ms: Arc<AtomicU64>,
}

impl ManualClock {
    fn new() -> Self {
        Self {
            now_ms: Arc::new(AtomicU64::new(1000)),
        }
    }

    fn advance_ms(&self, ms: u64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl TimeSource for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn test_polkit_caller_authorization_accepts_root() {
    let result = authorize_polkit_caller_with(Some(":1.42"), |_| async { Ok(0) }).await;
    assert!(result.is_ok(), "polkitd's root caller must be accepted");
}

#[tokio::test]
async fn test_polkit_caller_authorization_rejects_non_root() {
    let result = authorize_polkit_caller_with(Some(":1.42"), |_| async { Ok(1000) }).await;
    assert!(matches!(result, Err(zbus::fdo::Error::AccessDenied(_))));
}

#[tokio::test]
async fn test_polkit_caller_authorization_fails_closed_without_sender() {
    let result = authorize_polkit_caller_with(None, |_| async { Ok(0) }).await;
    assert!(matches!(result, Err(zbus::fdo::Error::AccessDenied(_))));
}

#[tokio::test]
async fn test_polkit_caller_authorization_fails_closed_on_uid_lookup_error() {
    let result = authorize_polkit_caller_with(Some(":1.42"), |_| async {
        Err("D-Bus daemon unavailable".to_string())
    })
    .await;
    assert!(matches!(result, Err(zbus::fdo::Error::AccessDenied(_))));
}

#[test]
fn test_production_helper_probe_ignores_environment_override() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    // SAFETY: nextest runs each test in its own process, so mutating a
    // process-global env var here cannot race with another test.
    unsafe {
        std::env::set_var("POLKIT_AGENT_HELPER_1_PATH", temp.path());
    }

    let resolved = probe_system_helper_path();

    unsafe {
        std::env::remove_var("POLKIT_AGENT_HELPER_1_PATH");
    }
    assert_ne!(resolved.as_deref(), Some(temp.path()));
}

#[test]
fn test_zeroize_memory() {
    let mut password = "SuperSecretPassword123!".to_string();

    zeroize_string(&mut password);
    assert_eq!(password, "");

    let mut bytes = [1u8, 2, 3, 4, 5];
    zeroize_bytes(&mut bytes);
    assert_eq!(bytes, [0, 0, 0, 0, 0]);
}

#[test]
fn test_helper_state_machine_echo_off_success() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![
        HelperEvent::PromptEchoOff("Password: ".to_string()),
        HelperEvent::Success,
    ]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock.clone(), 120_000);
    state.begin_start();
    state.mark_ready(clock.now_ms());

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Authentication is required to run /bin/bash as root".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-123".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "alice")],
        selected_identity: None,
    };

    let res = state.begin_authentication(request, tx);
    assert!(res.is_ok());

    let snap = state.snapshot();
    assert!(snap.request.is_some());
    let prompt = snap.prompt_state.unwrap();
    assert!(prompt.response_required);
    assert!(!prompt.response_visible);
    assert_eq!(prompt.input_prompt.as_deref(), Some("Password: "));

    // Provide response
    let ticket = state
        .submit_command(PolkitCommand::ProvideResponse {
            cookie: "cookie-123".to_string(),
            response: "alice_password".to_string().into(),
        })
        .unwrap();

    assert!(matches!(
        ticket.outcome().unwrap(),
        PolkitCommandOutcome::Applied { .. }
    ));

    // Helper should receive response
    assert_eq!(helper.written_responses(), vec!["alice_password"]);

    // Auth succeeded -> the Authority's BeginAuthentication call is resolved
    // immediately, regardless of when (or whether) the dialog closes.
    let completion = rx.try_recv().unwrap();
    assert!(completion.is_ok());

    // The dialog must NOT auto-close the instant Success arrives — the exact
    // race the reference agent hit tying teardown directly to the success
    // callback. It stays open, showing a success message, until tick() (not
    // this callback) decides to tear it down.
    let post_success_snap = state.snapshot();
    assert!(
        post_success_snap.request.is_some(),
        "dialog must remain open immediately after success"
    );
    let post_success_prompt = post_success_snap.prompt_state.unwrap();
    assert!(!post_success_prompt.response_required);
    assert_eq!(
        post_success_prompt.supplementary_message.as_deref(),
        Some("Authentication successful.")
    );
    assert!(!post_success_prompt.supplementary_is_error);

    // Before the dismiss window elapses, a tick must not tear it down.
    clock.advance_ms(SUCCESS_DISMISS_DELAY_MS - 1);
    state.tick(clock.now_ms());
    assert!(state.snapshot().request.is_some());

    // Once the window elapses, tick() tears it down.
    clock.advance_ms(1);
    state.tick(clock.now_ms());
    assert!(state.snapshot().request.is_none());
}

#[test]
fn test_helper_state_machine_echo_on_and_text_info() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![
        HelperEvent::TextInfo("Insert hardware token or enter PIN:".to_string()),
        HelperEvent::PromptEchoOn("PIN: ".to_string()),
        HelperEvent::Success,
    ]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Run admin command".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-456".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "bob")],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();

    let snap = state.snapshot();
    let prompt = snap.prompt_state.unwrap();
    assert_eq!(
        prompt.supplementary_message.as_deref(),
        Some("Insert hardware token or enter PIN:")
    );
    assert!(!prompt.supplementary_is_error);

    // Poll next event (echo on PIN)
    state.poll_active_helper_event();

    let snap2 = state.snapshot();
    let prompt2 = snap2.prompt_state.unwrap();
    assert!(prompt2.response_required);
    assert!(prompt2.response_visible);
    assert_eq!(prompt2.input_prompt.as_deref(), Some("PIN: "));

    // Provide response
    state
        .submit_command(PolkitCommand::ProvideResponse {
            cookie: "cookie-456".to_string(),
            response: "1234".to_string().into(),
        })
        .unwrap();

    assert_eq!(helper.written_responses(), vec!["1234"]);
    let completion = rx.try_recv().unwrap();
    assert!(completion.is_ok());
}

#[test]
fn test_helper_state_machine_failure_and_retry() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![
        HelperEvent::PromptEchoOff("Password: ".to_string()),
        HelperEvent::ErrorMessage("Incorrect password".to_string()),
        HelperEvent::Failure,
    ]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Perform system update".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-retry-1".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "charlie")],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();

    // Send wrong password
    state
        .submit_command(PolkitCommand::ProvideResponse {
            cookie: "cookie-retry-1".to_string(),
            response: "wrong_password".to_string().into(),
        })
        .unwrap();

    // Next event is failure
    state.poll_active_helper_event();

    let completion = rx.try_recv().unwrap();
    assert!(completion.is_err());
    assert_eq!(completion.unwrap_err(), "Authentication failed");

    let snap = state.snapshot();
    assert!(snap.request.is_none());

    // Retry flow: Second BeginAuthentication arrives with new cookie
    let helper2 = Arc::new(MockPolkitHelper::new(vec![
        HelperEvent::PromptEchoOff("Password: ".to_string()),
        HelperEvent::Success,
    ]));
    let state2 = PolkitDomainState::with_time_source(
        10,
        helper2.clone(),
        Arc::new(ManualClock::new()),
        120_000,
    );
    state2.begin_start();
    state2.mark_ready(2000);

    let (tx2, mut rx2) = oneshot::channel();
    let request2 = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Perform system update".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-retry-2".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "charlie")],
        selected_identity: None,
    };

    state2.begin_authentication(request2, tx2).unwrap();
    state2
        .submit_command(PolkitCommand::ProvideResponse {
            cookie: "cookie-retry-2".to_string(),
            response: "correct_password".to_string().into(),
        })
        .unwrap();

    let completion2 = rx2.try_recv().unwrap();
    assert!(completion2.is_ok());
}

#[test]
fn test_authority_cancel_authentication() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![HelperEvent::PromptEchoOff(
        "Password: ".to_string(),
    )]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Run privileged app".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-auth-cancel".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "dave")],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();
    assert!(state.snapshot().request.is_some());

    // Authority cancels
    state.cancel_authentication("cookie-auth-cancel");

    let completion = rx.try_recv().unwrap();
    assert!(completion.is_err());
    assert_eq!(completion.unwrap_err(), "Cancelled by PolicyKit authority");

    assert!(state.snapshot().request.is_none());
    assert_eq!(helper.killed_count(), 1);
}

#[test]
fn test_user_cancel_command() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![HelperEvent::PromptEchoOff(
        "Password: ".to_string(),
    )]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Run privileged app".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-user-cancel".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "dave")],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();
    assert!(state.snapshot().request.is_some());

    // User cancels
    state
        .submit_command(PolkitCommand::Cancel {
            cookie: "cookie-user-cancel".to_string(),
        })
        .unwrap();

    let completion = rx.try_recv().unwrap();
    assert!(completion.is_err());
    assert_eq!(completion.unwrap_err(), "Cancelled by user");

    assert!(state.snapshot().request.is_none());
    assert_eq!(helper.killed_count(), 1);
}

#[test]
fn test_inactivity_timeout_and_timer_reset() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![
        HelperEvent::PromptEchoOff("Password: ".to_string()),
        HelperEvent::PromptEchoOff("Try again: ".to_string()),
    ]));

    let timeout_ms = 30_000;
    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock.clone(), timeout_ms);
    state.begin_start();
    state.mark_ready(clock.now_ms());

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Install package".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-inactivity".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "eve")],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();

    // Advance clock by 20s (less than 30s timeout) with no interaction.
    clock.advance_ms(20_000);
    state.tick(clock.now_ms());
    assert!(state.snapshot().request.is_some());

    // An interaction at t=20s must reset the inactivity timer.
    state
        .submit_command(PolkitCommand::ProvideResponse {
            cookie: "cookie-inactivity".to_string(),
            response: "typo".to_string().into(),
        })
        .unwrap();

    // Advance by another 25s (45s since start, but only 25s since the t=20s
    // interaction): must NOT time out, proving the reset took effect rather
    // than the timer running off the original session-start time.
    clock.advance_ms(25_000);
    state.tick(clock.now_ms());
    assert!(
        state.snapshot().request.is_some(),
        "interaction must reset the inactivity timer"
    );

    // 30s since the last interaction with no further activity: times out.
    clock.advance_ms(10_000);
    state.tick(clock.now_ms());

    assert!(state.snapshot().request.is_none());
    let completion = rx.try_recv().unwrap();
    assert_eq!(completion.unwrap_err(), "Inactivity timeout");
    assert_eq!(helper.killed_count(), 1);
}

#[test]
fn test_multi_identity_selection() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![
        HelperEvent::PromptEchoOff("Admin Password: ".to_string()),
        HelperEvent::Success,
    ]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    let (tx, mut rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Change system configuration".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-multi".to_string(),
        is_internal: false,
        identities: vec![
            PolkitIdentity::new("unix-user", 0, "root").with_real_name("Administrator"),
            PolkitIdentity::new("unix-user", 1000, "frank").with_real_name("Frank"),
        ],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();

    // Before selection, helper should NOT be spawned yet
    assert_eq!(helper.spawned_users().len(), 0);
    let snap = state.snapshot();
    assert_eq!(snap.request.as_ref().unwrap().selected_identity, None);

    // User selects "root"
    state
        .submit_command(PolkitCommand::SelectIdentity {
            cookie: "cookie-multi".to_string(),
            username: "root".to_string(),
        })
        .unwrap();

    // Helper is now spawned for "root"
    assert_eq!(
        helper.spawned_users(),
        vec![("root".to_string(), "cookie-multi".to_string())]
    );

    // Provide password
    state
        .submit_command(PolkitCommand::ProvideResponse {
            cookie: "cookie-multi".to_string(),
            response: "root_secret".to_string().into(),
        })
        .unwrap();

    let completion = rx.try_recv().unwrap();
    assert!(completion.is_ok());
}

#[test]
fn test_mark_next_request_internal() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![HelperEvent::Success]));

    let state = PolkitDomainState::with_time_source(10, helper.clone(), clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    // Mark next request internal
    state
        .submit_command(PolkitCommand::MarkNextRequestInternal)
        .unwrap();

    let (tx, _rx) = oneshot::channel();
    let request = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "Change wallpaper".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-internal".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "grace")],
        selected_identity: None,
    };

    state.begin_authentication(request, tx).unwrap();

    let snap = state.snapshot();
    assert!(snap.request.unwrap().is_internal);

    // Next request should NOT be internal
    let (tx2, _rx2) = oneshot::channel();
    let request2 = PolkitRequest {
        action_id: "org.freedesktop.policykit.exec".to_string(),
        message: "External privileged operation".to_string(),
        icon_name: "dialog-password".to_string(),
        cookie: "cookie-external".to_string(),
        is_internal: false,
        identities: vec![PolkitIdentity::new("unix-user", 1000, "grace")],
        selected_identity: None,
    };

    state.begin_authentication(request2, tx2).unwrap();
    let snap2 = state.snapshot();
    assert!(!snap2.request.unwrap().is_internal);
}

#[test]
fn test_mailbox_lossless_overflow_and_replace_latest() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![]));

    // Capacity 2
    let state = PolkitDomainState::with_time_source(2, helper, clock, 120_000);
    state.begin_start();
    state.mark_ready(1000);

    // ReplaceLatest test: multiple MarkNextRequestInternal commands supersede previous when enqueued
    let t1 = state
        .enqueue_command(PolkitCommand::MarkNextRequestInternal)
        .unwrap();
    let _t2 = state
        .enqueue_command(PolkitCommand::MarkNextRequestInternal)
        .unwrap();

    assert!(matches!(
        t1.outcome().unwrap(),
        PolkitCommandOutcome::Cancelled {
            reason: CancellationReason::Superseded
        }
    ));

    let telemetry = state.telemetry();
    assert_eq!(telemetry.supersessions, 1);

    // Enqueue a second command to fill capacity 2
    let _t3 = state
        .enqueue_command(PolkitCommand::Cancel {
            cookie: "c1".to_string(),
        })
        .unwrap();

    // Next Lossless command overflows capacity 2 and is rejected
    let overflow_res = state.enqueue_command(PolkitCommand::Cancel {
        cookie: "c2".to_string(),
    });
    assert!(matches!(
        overflow_res,
        Err(PolkitCommandOutcome::Rejected {
            reason: PolkitRejectionReason::Overloaded
        })
    ));

    let telemetry2 = state.telemetry();
    assert_eq!(telemetry2.overloads, 1);
}

#[test]
fn test_registration_failure_supervisor_backoff() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![]));

    let state = PolkitDomainState::with_time_source(10, helper, clock.clone(), 120_000);

    state.report_owner_failure(
        "An authentication agent already exists for the given subject".to_string(),
        clock.now_ms(),
    );

    let snap = state.snapshot();
    assert_eq!(snap.lifecycle, DomainLifecycle::Degraded);
    assert_eq!(
        snap.last_error.as_deref(),
        Some("An authentication agent already exists for the given subject")
    );

    let sup_state = state.supervisor_state();
    assert!(matches!(
        sup_state,
        SupervisorState::Backoff { attempt: 1, .. }
    ));
}

#[test]
fn test_domain_version_fencing_rejects_stale_generation_commands() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![]));

    let state = PolkitDomainState::with_time_source(10, helper, clock.clone(), 120_000);
    state.begin_start();
    state.mark_ready(clock.now_ms());

    // Enqueue without draining: captures the current owner_generation (1).
    let ticket = state
        .enqueue_command(PolkitCommand::MarkNextRequestInternal)
        .unwrap();
    assert!(!ticket.is_completed());

    // Owner restarts before the command drains, bumping owner_generation to 2.
    state.begin_start();
    state.mark_ready(clock.now_ms());

    // Draining now must fence the stale-generation command out rather than
    // silently applying it under the new owner.
    state.process_pending_commands();

    assert!(matches!(
        ticket.outcome().unwrap(),
        PolkitCommandOutcome::Cancelled {
            reason: CancellationReason::OwnerReplaced
        }
    ));
}

#[test]
fn test_provide_response_debug_output_redacts_password() {
    let command = PolkitCommand::ProvideResponse {
        cookie: "cookie-debug".to_string(),
        response: "SuperSecretPassword123!".to_string().into(),
    };
    let debug_str = format!("{command:?}");
    assert!(!debug_str.contains("SuperSecretPassword123!"));
    assert!(debug_str.contains("<redacted>"));
}

#[test]
fn queued_response_is_zeroized_when_owner_is_replaced() {
    let clock = Arc::new(ManualClock::new());
    let helper = Arc::new(MockPolkitHelper::new(vec![]));
    let state = PolkitDomainState::with_time_source(4, helper, clock.clone(), 120_000);
    state.begin_start();
    state.mark_ready(clock.now_ms());
    let (response, wipe) = SecretString::new_observed_for_test("polkit-password");

    state
        .enqueue_command(PolkitCommand::ProvideResponse {
            cookie: "cookie".into(),
            response,
        })
        .expect("queued response");
    state.begin_start();
    state.process_pending_commands();

    assert_eq!(wipe.bytes(), Some(vec![0; "polkit-password".len()]));
}

#[test]
fn response_command_serde_remains_string_shaped() {
    let command = PolkitCommand::ProvideResponse {
        cookie: "cookie".into(),
        response: "serde-password".into(),
    };
    let json = serde_json::to_string(&command).unwrap();
    assert_eq!(
        json,
        r#"{"ProvideResponse":{"cookie":"cookie","response":"serde-password"}}"#
    );
    let round_trip: PolkitCommand = serde_json::from_str(&json).unwrap();
    assert_eq!(round_trip, command);
}

fn dict_str(
    dict: &std::collections::HashMap<String, zbus::zvariant::Value<'static>>,
    key: &str,
) -> String {
    match dict.get(key).unwrap() {
        zbus::zvariant::Value::Str(s) => s.to_string(),
        other => panic!("expected Value::Str for {key:?}, got {other:?}"),
    }
}

async fn build_p2p_pair() -> (zbus::Connection, zbus::Connection) {
    let (server_stream, client_stream) = std::os::unix::net::UnixStream::pair().unwrap();
    let guid = zbus::Guid::generate();
    let server_builder = zbus::connection::Builder::async_io_unix_stream(server_stream)
        .server(guid)
        .unwrap()
        .p2p();
    let client_builder = zbus::connection::Builder::async_io_unix_stream(client_stream).p2p();
    // The P2P handshake requires both ends to negotiate concurrently; awaiting
    // one `.build()` before starting the other deadlocks (each side blocks
    // waiting for a greeting the other hasn't sent yet).
    tokio::try_join!(server_builder.build(), client_builder.build()).unwrap()
}

/// Minimal mock of `org.freedesktop.login1.Manager.GetSessionByPID`, enough
/// to exercise `AuthorityClient::resolve_subject`'s steps 2 and 3 without a
/// real logind or system bus.
struct MockLogin1 {
    /// `Some(id)` returns a session object path; `None` returns an error.
    /// When `Some` but the string is empty, returns an unrelated error to
    /// prove non-"no session" errors are propagated rather than swallowed.
    outcome: MockLogin1Outcome,
}

enum MockLogin1Outcome {
    Session(String),
    NoSessionForPid,
    OtherError,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl MockLogin1 {
    #[zbus(name = "GetSessionByPID")]
    async fn get_session_by_pid(
        &self,
        _pid: u32,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
        match &self.outcome {
            MockLogin1Outcome::Session(id) => zbus::zvariant::OwnedObjectPath::try_from(format!(
                "/org/freedesktop/login1/session/_3{id}"
            ))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string())),
            MockLogin1Outcome::NoSessionForPid => Err(zbus::fdo::Error::Failed(
                "No session for pid 999999".to_string(),
            )),
            MockLogin1Outcome::OtherError => Err(zbus::fdo::Error::Failed(
                "org.freedesktop.DBus.Error.ServiceUnknown: unrelated failure".to_string(),
            )),
        }
    }
}

async fn build_p2p_pair_with_login1(
    outcome: MockLogin1Outcome,
) -> (zbus::Connection, zbus::Connection) {
    let (server_stream, client_stream) = std::os::unix::net::UnixStream::pair().unwrap();
    let guid = zbus::Guid::generate();
    let server_builder = zbus::connection::Builder::async_io_unix_stream(server_stream)
        .server(guid)
        .unwrap()
        .p2p()
        .serve_at("/org/freedesktop/login1", MockLogin1 { outcome })
        .unwrap();
    let client_builder = zbus::connection::Builder::async_io_unix_stream(client_stream).p2p();
    tokio::try_join!(server_builder.build(), client_builder.build()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_resolve_subject_prefers_xdg_session_id() {
    // SAFETY: nextest runs each test in its own process, so mutating a
    // process-global env var here cannot race with any other test.
    unsafe {
        std::env::set_var("XDG_SESSION_ID", "42");
    }

    let (_server, client) = build_p2p_pair().await;
    let (kind, dict) = AuthorityClient::resolve_subject(&client).await.unwrap();

    unsafe {
        std::env::remove_var("XDG_SESSION_ID");
    }

    assert_eq!(kind, "unix-session");
    assert_eq!(dict_str(&dict, "session-id"), "42");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_resolve_subject_falls_back_to_session_for_pid() {
    unsafe {
        std::env::remove_var("XDG_SESSION_ID");
    }

    let (_server, client) =
        build_p2p_pair_with_login1(MockLogin1Outcome::Session("7".to_string())).await;
    let (kind, dict) = AuthorityClient::resolve_subject(&client).await.unwrap();

    assert_eq!(kind, "unix-session");
    assert_eq!(dict_str(&dict, "session-id"), "_37");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_resolve_subject_falls_back_to_unix_user_on_no_session() {
    unsafe {
        std::env::remove_var("XDG_SESSION_ID");
    }

    let (_server, client) = build_p2p_pair_with_login1(MockLogin1Outcome::NoSessionForPid).await;
    let (kind, dict) = AuthorityClient::resolve_subject(&client).await.unwrap();

    assert_eq!(kind, "unix-user");
    assert!(dict.contains_key("uid"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_resolve_subject_propagates_unrelated_session_lookup_errors() {
    unsafe {
        std::env::remove_var("XDG_SESSION_ID");
    }

    let (_server, client) = build_p2p_pair_with_login1(MockLogin1Outcome::OtherError).await;
    let result = AuthorityClient::resolve_subject(&client).await;

    // Anything other than a "no session for pid"-shaped error must be
    // propagated rather than silently substituting a unix-user subject that
    // may not actually be authorized for the action.
    assert!(result.is_err());
}
