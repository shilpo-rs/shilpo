use std::{sync::Arc, time::Duration};

use futures_lite::stream::StreamExt;
use tokio::sync::mpsc;

use super::{
    DebugDbusService, ShellCommand, ShellDbusService, ShellStatus, ShellTelemetry,
    test_harness::{TestDbusHarness, wait_for},
};

macro_rules! bounded {
    ($label:literal, $future:expr) => {
        wait_for($label, $future).await
    };
}

fn assert_method_error(error: zbus::Error, expected_name: &str) {
    match error {
        zbus::Error::MethodError(name, _, _) => {
            assert_eq!(name.to_string(), expected_name);
        }
        other => panic!("expected D-Bus method error {expected_name}, got {other:?}"),
    }
}

type ArgContract<'a> = (Option<&'a str>, &'a str, Option<&'a str>);

fn assert_interface_contract(
    document: &roxmltree::Document<'_>,
    interface_name: &str,
    element_name: &str,
    expected: &[(&str, &[ArgContract<'_>])],
) {
    let interface = document
        .descendants()
        .find(|node| {
            node.has_tag_name("interface") && node.attribute("name") == Some(interface_name)
        })
        .unwrap_or_else(|| panic!("missing interface {interface_name}"));
    let members = interface
        .children()
        .filter(|node| node.has_tag_name(element_name))
        .collect::<Vec<_>>();
    assert_eq!(
        members.len(),
        expected.len(),
        "unexpected {element_name} count"
    );

    for (member_name, expected_args) in expected {
        let member = members
            .iter()
            .find(|node| node.attribute("name") == Some(*member_name))
            .unwrap_or_else(|| panic!("missing {element_name} {member_name}"));
        let actual_args = member
            .children()
            .filter(|node| node.has_tag_name("arg"))
            .map(|node| {
                (
                    node.attribute("name"),
                    node.attribute("type").expect("argument type"),
                    node.attribute("direction"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            &actual_args, expected_args,
            "wrong contract for {member_name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_introspection_exact_contract() {
    let harness = TestDbusHarness::new().await;
    let xml = harness.introspect_xml().await;
    let node_start = xml.find("<node>").expect("introspection node element");
    let document = roxmltree::Document::parse(&xml[node_start..]).expect("valid introspection XML");

    assert_interface_contract(
        &document,
        "org.shilpo.Shell",
        "method",
        &[
            ("ReloadConfig", &[]),
            ("ShowBar", &[]),
            ("HideBar", &[]),
            ("ToggleBar", &[]),
            ("ShowOverview", &[]),
            ("HideOverview", &[]),
            ("ToggleOverview", &[]),
            (
                "FocusWorkspace",
                &[
                    (Some("workspace_id"), "t", Some("in")),
                    (None, "(sttstt)", Some("out")),
                ],
            ),
            ("CreateWorkspace", &[(None, "(sttstt)", Some("out"))]),
            (
                "FocusWindow",
                &[
                    (Some("window_id"), "t", Some("in")),
                    (None, "(sttstt)", Some("out")),
                ],
            ),
            ("FocusPreviousWindow", &[(None, "(sttstt)", Some("out"))]),
            (
                "CloseWindow",
                &[
                    (Some("window_id"), "t", Some("in")),
                    (None, "(sttstt)", Some("out")),
                ],
            ),
            (
                "MoveWindowToWorkspace",
                &[
                    (Some("window_id"), "t", Some("in")),
                    (Some("workspace_id"), "t", Some("in")),
                    (None, "(sttstt)", Some("out")),
                ],
            ),
            ("SetBrightness", &[(Some("percentage"), "y", Some("in"))]),
            (
                "SetDisplayBrightness",
                &[
                    (Some("display_id"), "s", Some("in")),
                    (Some("percentage"), "y", Some("in")),
                ],
            ),
            ("GetStatus", &[(None, "(bsussb)", Some("out"))]),
            (
                "GetTelemetry",
                &[(
                    None,
                    "(bsttusbssbssbssbssbssbssbsssbssbuuasbsbsssbts)",
                    Some("out"),
                )],
            ),
            ("Capture", &[(Some("intent"), "s", Some("in"))]),
            (
                "InvokeAction",
                &[
                    (Some("action_id"), "s", Some("in")),
                    (Some("payload_json"), "ms", Some("in")),
                ],
            ),
            ("NextWallpaper", &[]),
            ("Lock", &[]),
            (
                "ForgetSearchResult",
                &[(Some("canonical_id"), "s", Some("in"))],
            ),
            ("ClearSearchLearning", &[]),
            (
                "StartDevSession",
                &[
                    (Some("extension_id"), "s", Some("in")),
                    (Some("source_root"), "s", Some("in")),
                    (None, "s", Some("out")),
                ],
            ),
            (
                "ReloadDevSession",
                &[
                    (Some("session_id"), "s", Some("in")),
                    (Some("build_sequence"), "t", Some("in")),
                    (Some("artifact_path"), "s", Some("in")),
                    (Some("timeout_ms"), "t", Some("in")),
                    (None, "(sttss)", Some("out")),
                ],
            ),
            ("EndDevSession", &[(Some("session_id"), "s", Some("in"))]),
        ],
    );
    assert_interface_contract(
        &document,
        "org.shilpo.Shell",
        "signal",
        &[
            (
                "ShellStarted",
                &[(Some("instance_id"), "s", None), (Some("pid"), "u", None)],
            ),
            ("ShellStopping", &[(Some("instance_id"), "s", None)]),
            (
                "WorkspaceChanged",
                &[
                    (Some("workspace_id"), "t", None),
                    (Some("owner_generation"), "t", None),
                    (Some("revision"), "t", None),
                ],
            ),
            (
                "ThemeChanged",
                &[
                    (Some("mode"), "s", None),
                    (Some("scheme_variant"), "s", None),
                ],
            ),
            (
                "ConfigReloaded",
                &[
                    (Some("success"), "b", None),
                    (Some("changed_components"), "as", None),
                    (Some("diagnostic_count"), "u", None),
                ],
            ),
        ],
    );
    assert_interface_contract(
        &document,
        "org.shilpo.Debug",
        "method",
        &[
            ("SetLogFilter", &[(Some("filter"), "s", Some("in"))]),
            ("GetLogFilter", &[(None, "s", Some("out"))]),
            (
                "EmitTestNotification",
                &[
                    (Some("title"), "s", Some("in")),
                    (Some("body"), "s", Some("in")),
                ],
            ),
            ("ResetNotificationQuarantine", &[]),
            ("ResetDeviceQuarantine", &[]),
        ],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reset_notification_quarantine_dispatch() {
    let mut harness = TestDbusHarness::new().await;

    bounded!(
        "ResetNotificationQuarantine response",
        harness.debug_proxy.reset_notification_quarantine()
    )
    .unwrap();

    let cmd = bounded!("reset command", harness.mailbox_rx.recv()).unwrap();
    assert_eq!(cmd, ShellCommand::ResetNotificationQuarantine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reset_device_quarantine_dispatch() {
    let mut harness = TestDbusHarness::new().await;

    bounded!(
        "ResetDeviceQuarantine response",
        harness.debug_proxy.reset_device_quarantine()
    )
    .unwrap();

    let cmd = bounded!("reset command", harness.mailbox_rx.recv()).unwrap();
    assert_eq!(cmd, ShellCommand::ResetDeviceQuarantine);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_mailbox_command_delivery_and_fifo() {
    let mut harness = TestDbusHarness::new().await;

    bounded!("ReloadConfig response", harness.shell_proxy.reload_config()).unwrap();
    bounded!("ShowBar response", harness.shell_proxy.show_bar()).unwrap();
    bounded!(
        "SetBrightness response",
        harness.shell_proxy.set_brightness(42)
    )
    .unwrap();

    let cmd1 = bounded!("first mailbox command", harness.mailbox_rx.recv()).unwrap();
    let cmd2 = bounded!("second mailbox command", harness.mailbox_rx.recv()).unwrap();
    let cmd3 = bounded!("third mailbox command", harness.mailbox_rx.recv()).unwrap();

    assert_eq!(cmd1, ShellCommand::ReloadConfig);
    assert_eq!(cmd2, ShellCommand::ShowBar);
    assert_eq!(cmd3, ShellCommand::SetBrightness(42));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_forget_and_clear_search_learning_dispatch() {
    let mut harness = TestDbusHarness::new().await;

    bounded!(
        "ForgetSearchResult response",
        harness
            .shell_proxy
            .forget_search_result("app:firefox".into())
    )
    .unwrap();
    bounded!(
        "ClearSearchLearning response",
        harness.shell_proxy.clear_search_learning()
    )
    .unwrap();

    let cmd1 = bounded!("first command", harness.mailbox_rx.recv()).unwrap();
    let cmd2 = bounded!("second command", harness.mailbox_rx.recv()).unwrap();

    assert_eq!(
        cmd1,
        ShellCommand::ForgetSearchResult("app:firefox".to_string())
    );
    assert_eq!(cmd2, ShellCommand::ClearSearchLearning);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_boundary_error_handling() {
    let mut harness = TestDbusHarness::new().await;

    // Invalid brightness (>100) -> InvalidArgs
    let err = bounded!(
        "invalid brightness response",
        harness.shell_proxy.set_brightness(101)
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    // Invalid display brightness (empty display_id or >100) -> InvalidArgs
    let err = bounded!(
        "empty display response",
        harness.shell_proxy.set_display_brightness("".into(), 50)
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    let err = bounded!(
        "invalid display brightness response",
        harness
            .shell_proxy
            .set_display_brightness("DP-1".into(), 101)
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    // Invalid capture intent -> InvalidArgs
    let err = bounded!(
        "invalid capture response",
        harness.shell_proxy.capture("invalid_intent".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    // Invalid debug inputs -> InvalidArgs
    let err = bounded!(
        "empty filter response",
        harness.debug_proxy.set_log_filter("   ".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    let err = bounded!(
        "invalid filter response",
        harness.debug_proxy.set_log_filter("invalid[[syntax".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    let err = bounded!(
        "empty notification title response",
        harness
            .debug_proxy
            .emit_test_notification("".into(), "body".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    let err = bounded!(
        "oversized notification title response",
        harness
            .debug_proxy
            .emit_test_notification("a".repeat(257), "body".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    let err = bounded!(
        "oversized notification body response",
        harness
            .debug_proxy
            .emit_test_notification("title".into(), "b".repeat(4097))
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.InvalidArgs");

    // Mailbox rejected calls enqueue nothing
    assert!(harness.mailbox_rx.try_recv().is_err());

    // Mailbox overflow -> LimitsExceeded
    for _ in 0..128 {
        bounded!("mailbox fill response", harness.shell_proxy.show_bar()).unwrap();
    }
    let overflow_err = bounded!(
        "mailbox overflow response",
        harness
            .debug_proxy
            .emit_test_notification("Overflow".into(), "Mailbox".into())
    )
    .unwrap_err();
    assert_method_error(overflow_err, "org.freedesktop.DBus.Error.LimitsExceeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_closed_mailbox_and_unavailable_controller() {
    let (tx, rx) = mpsc::channel(128);
    let shell_service = ShellDbusService::new(
        tx.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(None)),
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(ShellStatus::default())),
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(ShellTelemetry::default())),
    );
    let debug_service = DebugDbusService::new(None, tx);
    drop(rx); // Close receiver

    let harness =
        TestDbusHarness::new_with_services(shell_service, debug_service, mpsc::channel(1).1).await;

    let err = bounded!(
        "unavailable filter response",
        harness.debug_proxy.get_log_filter()
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.Failed");

    let err = bounded!(
        "unavailable filter update response",
        harness.debug_proxy.set_log_filter("info".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.Failed");

    let err = bounded!(
        "closed mailbox response",
        harness
            .debug_proxy
            .emit_test_notification("Title".into(), "Body".into())
    )
    .unwrap_err();
    assert_method_error(err, "org.freedesktop.DBus.Error.Failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_atomic_status_and_telemetry_snapshots() {
    let harness = TestDbusHarness::new().await;

    let custom_status = ShellStatus {
        running: true,
        instance_id: "test-instance-123".into(),
        pid: 4321,
        readiness: "ready".into(),
        bar_state: "visible".into(),
        overview_visible: true,
    };
    harness.update_status(custom_status.clone());

    let status = bounded!("status response", harness.shell_proxy.get_status()).unwrap();
    assert_eq!(status, custom_status);

    let custom_telemetry = ShellTelemetry {
        compositor_connected: true,
        compositor_state: "connected".into(),
        compositor_owner_generation: 12,
        compositor_revision: 34,
        uptime_seconds: 12345,
        ..Default::default()
    };
    harness.update_telemetry(custom_telemetry.clone());

    let telemetry = bounded!("telemetry response", harness.shell_proxy.get_telemetry()).unwrap();
    assert_eq!(telemetry, custom_telemetry);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_lifecycle_signals() {
    let harness = TestDbusHarness::new().await;

    let mut started_stream = bounded!(
        "ShellStarted subscription",
        harness.shell_proxy.receive_shell_started()
    )
    .unwrap();
    let mut stopping_stream = bounded!(
        "ShellStopping subscription",
        harness.shell_proxy.receive_shell_stopping()
    )
    .unwrap();

    let emitter = harness.signal_emitter().await;
    bounded!(
        "ShellStarted emission",
        ShellDbusService::shell_started(&emitter, "inst-42", 1234)
    )
    .unwrap();
    bounded!(
        "ShellStopping emission",
        ShellDbusService::shell_stopping(&emitter, "inst-42")
    )
    .unwrap();

    let started_sig = tokio::time::timeout(Duration::from_secs(2), started_stream.next())
        .await
        .expect("started signal timeout")
        .expect("started stream item");
    let started_args = started_sig.args().unwrap();
    assert_eq!(started_args.instance_id, "inst-42");
    assert_eq!(started_args.pid, 1234);

    let stopping_sig = tokio::time::timeout(Duration::from_secs(2), stopping_stream.next())
        .await
        .expect("stopping signal timeout")
        .expect("stopping stream item");
    let stopping_args = stopping_sig.args().unwrap();
    assert_eq!(stopping_args.instance_id, "inst-42");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_workspace_changed_dedup_semantics() {
    let harness = TestDbusHarness::new().await;
    let mut stream = bounded!(
        "WorkspaceChanged subscription",
        harness.shell_proxy.receive_workspace_changed()
    )
    .unwrap();
    let emitter = harness.signal_emitter().await;

    // Prime workspace 1 (initial priming emits nothing)
    harness.shell_service.prime_workspace(1);

    // Repeated workspace 1 emits nothing
    bounded!(
        "equal workspace emission",
        harness
            .shell_service
            .emit_workspace_changed_if_needed(&emitter, 1, 10, 100)
    );

    // Change to workspace 2 emits once
    bounded!(
        "changed workspace emission",
        harness
            .shell_service
            .emit_workspace_changed_if_needed(&emitter, 2, 10, 101)
    );

    let sig = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("workspace changed timeout")
        .expect("workspace changed item");
    let args = sig.args().unwrap();
    assert_eq!(args.workspace_id, 2);
    assert_eq!(args.owner_generation, 10);
    assert_eq!(args.revision, 101);

    // Repeat workspace 2 with updated generation/revision emits nothing
    bounded!(
        "repeated workspace emission",
        harness
            .shell_service
            .emit_workspace_changed_if_needed(&emitter, 2, 11, 102)
    );

    let no_sig = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(no_sig.is_err(), "expected no signal for repeated workspace");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_theme_changed_dedup_semantics() {
    let harness = TestDbusHarness::new().await;
    let mut stream = bounded!(
        "ThemeChanged subscription",
        harness.shell_proxy.receive_theme_changed()
    )
    .unwrap();
    let emitter = harness.signal_emitter().await;

    // First call populates initial state and emits nothing
    bounded!(
        "initial theme emission",
        harness
            .shell_service
            .emit_theme_changed_if_needed(&emitter, "dark", "Expressive")
    );

    // Changed mode emits once
    bounded!(
        "changed theme emission",
        harness
            .shell_service
            .emit_theme_changed_if_needed(&emitter, "light", "Expressive")
    );

    let sig = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("theme changed timeout")
        .expect("theme changed item");
    let args = sig.args().unwrap();
    assert_eq!(args.mode, "light");
    assert_eq!(args.scheme_variant, "Expressive");

    // Equal repeat emits nothing
    bounded!(
        "repeated theme emission",
        harness
            .shell_service
            .emit_theme_changed_if_needed(&emitter, "light", "Expressive")
    );

    let no_sig = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(no_sig.is_err(), "expected no signal for repeated theme");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_config_reloaded_signal_semantics() {
    let harness = TestDbusHarness::new().await;
    let mut stream = bounded!(
        "ConfigReloaded subscription",
        harness.shell_proxy.receive_config_reloaded()
    )
    .unwrap();
    let emitter = harness.signal_emitter().await;

    // Successful reload sorts component names
    bounded!(
        "successful ConfigReloaded emission",
        harness.shell_service.emit_config_reloaded(
            &emitter,
            true,
            vec!["theme".into(), "bar".into()],
            0
        )
    );

    let sig1 = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("config reloaded timeout")
        .expect("config reloaded item");
    let args1 = sig1.args().unwrap();
    assert!(args1.success);
    assert_eq!(args1.changed_components, vec!["bar", "theme"]);
    assert_eq!(args1.diagnostic_count, 0);

    // Failed reload clears component list but preserves diagnostic count
    bounded!(
        "failed ConfigReloaded emission",
        harness.shell_service.emit_config_reloaded(
            &emitter,
            false,
            vec!["bar".into(), "theme".into()],
            5
        )
    );

    let sig2 = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("config reloaded timeout")
        .expect("config reloaded item");
    let args2 = sig2.args().unwrap();
    assert!(!args2.success);
    assert!(args2.changed_components.is_empty());
    assert_eq!(args2.diagnostic_count, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_clean_lifecycle_drop() {
    let harness = TestDbusHarness::new().await;
    bounded!(
        "clean-lifecycle status response",
        harness.shell_proxy.get_status()
    )
    .unwrap();
    drop(harness);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_dev_session_start_is_denied_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(
        root.join("extension.toml"),
        r#"
        schema_version = 1
        id = "org.shilpo.default-deny"
        name = "Default Deny"
        version = "0.1.0"
        api_version = "0.1.0"
        min_shilpo_version = "0.1.0"
        "#,
    )
    .unwrap();

    let harness = TestDbusHarness::new().await;
    let error = bounded!(
        "StartDevSession default denial",
        harness.shell_proxy.start_dev_session(
            "org.shilpo.default-deny".into(),
            root.to_string_lossy().to_string()
        )
    )
    .expect_err("developer sessions must be denied unless the daemon opted in");

    assert_method_error(error, "org.freedesktop.DBus.Error.AccessDenied");
    assert!(
        harness
            .shell_service
            .dev_sessions()
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_dev_session_start_reload_end_flow() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let manifest_toml = r#"
        schema_version = 1
        id = "org.shilpo.dev-dbus"
        name = "Dev DBus"
        version = "0.1.0"
        api_version = "0.1.0"
        min_shilpo_version = "0.1.0"

        [[contributions.bar_widgets]]
        id = "widget1"
        name = "Widget 1"
    "#;
    std::fs::write(root.join("extension.toml"), manifest_toml).unwrap();
    std::fs::write(root.join("extension.wasm"), b"DUMMY_BYTECODE").unwrap();

    let harness = TestDbusHarness::new_developer_mode().await;

    // Set up a mock/real supervisor
    let supervisor = crate::extensions::ExtensionSupervisor::new();
    let coordinator =
        Arc::new(crate::extensions::ExtensionCoordinator::new_with_supervisor(supervisor));
    harness
        .shell_service
        .set_extension_coordinator(Some(coordinator));

    // 1. Start Dev Session
    let session_id = bounded!(
        "StartDevSession",
        harness.shell_proxy.start_dev_session(
            "org.shilpo.dev-dbus".into(),
            root.to_string_lossy().to_string()
        )
    )
    .expect("session start must succeed");

    assert!(!session_id.is_empty());

    // 2. Reload Dev Session
    let res = bounded!(
        "ReloadDevSession",
        harness.shell_proxy.reload_dev_session(
            session_id.clone(),
            1,
            "extension.wasm".into(),
            10_000
        )
    )
    .expect("reload call must succeed");

    // Outcome may be applied or host unavailable in mock
    assert!(!res.outcome.is_empty());

    // 3. End Dev Session
    bounded!(
        "EndDevSession",
        harness.shell_proxy.end_dev_session(session_id)
    )
    .expect("end session must succeed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_dev_session_security_and_manifest_validation() {
    let harness = TestDbusHarness::new_developer_mode().await;

    // 1. Non-existent path
    let err1 = bounded!(
        "StartDevSession nonexistent",
        harness.shell_proxy.start_dev_session(
            "org.shilpo.dev-dbus".into(),
            "/nonexistent/path/here".into()
        )
    )
    .unwrap_err();
    assert_method_error(err1, "org.freedesktop.DBus.Error.InvalidArgs");

    // 2. Relative path
    let err2 = bounded!(
        "StartDevSession relative",
        harness
            .shell_proxy
            .start_dev_session("org.shilpo.dev-dbus".into(), "relative/path".into())
    )
    .unwrap_err();
    assert_method_error(err2, "org.freedesktop.DBus.Error.InvalidArgs");

    // 3. Manifest ID mismatch
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(
        root.join("extension.toml"),
        r#"
        schema_version = 1
        id = "org.shilpo.actual-id"
        name = "Actual"
        version = "0.1.0"
        api_version = "0.1.0"
        min_shilpo_version = "0.1.0"
        "#,
    )
    .unwrap();

    let err3 = bounded!(
        "StartDevSession ID mismatch",
        harness.shell_proxy.start_dev_session(
            "org.shilpo.different-id".into(),
            root.to_string_lossy().to_string()
        )
    )
    .unwrap_err();
    assert_method_error(err3, "org.freedesktop.DBus.Error.InvalidArgs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_dev_session_reload_and_end_require_owning_unique_sender() {
    let harness = TestDbusHarness::new_developer_mode().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(
        root.join("extension.toml"),
        r#"
        schema_version = 1
        id = "org.shilpo.owner-check"
        name = "Owner Check"
        version = "0.1.0"
        api_version = "0.1.0"
        min_shilpo_version = "0.1.0"
        "#,
    )
    .unwrap();

    let session_id = bounded!(
        "StartDevSession for owner check",
        harness.shell_proxy.start_dev_session(
            "org.shilpo.owner-check".into(),
            root.to_string_lossy().to_string()
        )
    )
    .expect("developer mode should permit a session");

    harness
        .shell_service
        .dev_sessions()
        .lock()
        .unwrap()
        .get_mut(&session_id)
        .expect("minted session")
        .caller_unique_name = ":different-unique-sender".into();

    let reload_error = bounded!(
        "ReloadDevSession non-owner denial",
        harness.shell_proxy.reload_dev_session(
            session_id.clone(),
            1,
            "extension.wasm".into(),
            1_000
        )
    )
    .expect_err("a non-owner must not reload a developer session");
    assert_method_error(reload_error, "org.freedesktop.DBus.Error.AccessDenied");

    let end_error = bounded!(
        "EndDevSession non-owner denial",
        harness.shell_proxy.end_dev_session(session_id.clone())
    )
    .expect_err("a non-owner must not end a developer session");
    assert_method_error(end_error, "org.freedesktop.DBus.Error.AccessDenied");
    assert!(
        harness
            .shell_service
            .dev_sessions()
            .lock()
            .unwrap()
            .contains_key(&session_id),
        "a denied lifecycle call must preserve the owner's session"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_dev_session_disconnect_cleanup() {
    let harness = TestDbusHarness::new_developer_mode().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::write(
        root.join("extension.toml"),
        r#"
        schema_version = 1
        id = "org.shilpo.cleanup-test"
        name = "Cleanup Test"
        version = "0.1.0"
        api_version = "0.1.0"
        min_shilpo_version = "0.1.0"
        "#,
    )
    .unwrap();

    let session_id = bounded!(
        "StartDevSession",
        harness.shell_proxy.start_dev_session(
            "org.shilpo.cleanup-test".into(),
            root.to_string_lossy().to_string()
        )
    )
    .unwrap();

    assert_eq!(
        harness.shell_service.dev_sessions().lock().unwrap().len(),
        1
    );

    // Simulate NameOwnerChanged disconnect
    let caller_name = harness
        .shell_service
        .dev_sessions()
        .lock()
        .unwrap()
        .get(&session_id)
        .unwrap()
        .caller_unique_name
        .clone();

    harness
        .shell_service
        .handle_name_owner_changed(&caller_name, &caller_name, "");

    assert_eq!(
        harness.shell_service.dev_sessions().lock().unwrap().len(),
        0
    );
}
