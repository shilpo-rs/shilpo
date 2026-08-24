use std::io::{BufReader, BufWriter};
use std::process::{Command, Stdio};

use shilpo_ext_runtime::{
    ExtensionCommand, ExtensionGeneration, HostGeneration, HostMessage, PROTOCOL_VERSION,
    WorkerPayload, recv_worker_message, send_host_message,
};

#[test]
fn ext_status_json_preserves_error_envelope_when_daemon_is_unavailable() {
    let runtime_dir =
        std::env::temp_dir().join(format!("shilpo-cli-status-{}", std::process::id()));
    std::fs::create_dir_all(&runtime_dir).expect("temporary runtime directory should be created");

    let output = Command::new(env!("CARGO_BIN_EXE_shilpo"))
        .args(["--json", "ext", "status"])
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .output()
        .expect("unified shilpo binary should run ext status");
    let _ = std::fs::remove_dir_all(&runtime_dir);

    assert_eq!(output.status.code(), Some(3));
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("status should emit JSON");
    assert_eq!(envelope["schema_version"], 1);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["command"], "ext");
    assert_eq!(envelope["data"], serde_json::Value::Null);
    assert_eq!(envelope["error"]["code"], "extension_operation_failed");
}

#[test]
fn real_extension_host_publishes_snapshot_and_acknowledges_shutdown() {
    let temp_dir =
        std::env::temp_dir().join(format!("shilpo-ext-host-test-{}", std::process::id()));
    let data_dir = temp_dir.join("data");
    let config_dir = temp_dir.join("config");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_shilpo"))
        .arg("extension-host")
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &config_dir)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", temp_dir.join("no-session-bus").display()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("unified shilpo binary should spawn extension-host");

    let stdin = child.stdin.take().expect("extension-host stdin");
    let stdout = child.stdout.take().expect("extension-host stdout");
    let mut writer = BufWriter::new(stdin);
    let mut reader = BufReader::new(stdout);
    let host_generation = HostGeneration(41);

    send_host_message(
        &mut writer,
        &HostMessage {
            protocol_version: PROTOCOL_VERSION,
            host_generation,
            request_id: 1,
            command: ExtensionCommand::SourcesChanged,
        },
    )
    .expect("handshake should be framed successfully");

    let initial = recv_worker_message(&mut reader).expect("initial snapshot should be readable");
    assert_eq!(initial.protocol_version, PROTOCOL_VERSION);
    assert_eq!(initial.host_generation, host_generation);
    assert!(initial.engine_generation >= ExtensionGeneration(0));
    assert!(matches!(initial.payload, WorkerPayload::Update(_)));

    send_host_message(
        &mut writer,
        &HostMessage {
            protocol_version: PROTOCOL_VERSION,
            host_generation,
            request_id: 2,
            command: ExtensionCommand::Shutdown,
        },
    )
    .expect("shutdown should be framed successfully");

    let shutdown =
        recv_worker_message(&mut reader).expect("shutdown acknowledgement should arrive");
    assert_eq!(shutdown.host_generation, host_generation);
    assert_eq!(shutdown.request_id, 2);
    assert!(matches!(shutdown.payload, WorkerPayload::ShutdownAck));

    let status = child.wait().expect("extension-host should exit cleanly");
    let _ = std::fs::remove_dir_all(&temp_dir);
    assert!(status.success(), "extension-host exited with {status}");
}
