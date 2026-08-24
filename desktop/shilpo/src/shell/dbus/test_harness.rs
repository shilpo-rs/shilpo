//! Reusable headless P2P D-Bus test harness for org.shilpo.Shell and org.shilpo.Debug interfaces.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::{future::Future, time::Duration};

use shilpo_observability::LogFilterController;
use tokio::sync::mpsc;

use super::{
    DebugDbusService, DebugProxy, ShellCommand, ShellDbusService, ShellProxy, ShellStatus,
    ShellTelemetry,
};

pub const TEST_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn wait_for<T>(label: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
}

pub struct TestDbusHarness {
    pub server_conn: zbus::Connection,
    pub client_conn: zbus::Connection,
    pub shell_proxy: ShellProxy<'static>,
    pub debug_proxy: DebugProxy<'static>,
    pub mailbox_rx: mpsc::Receiver<ShellCommand>,
    pub shell_service: ShellDbusService,
    pub debug_service: DebugDbusService,
}

impl TestDbusHarness {
    pub async fn new() -> Self {
        Self::new_with_controller(Some(LogFilterController::new_for_testing("info"))).await
    }

    pub async fn new_developer_mode() -> Self {
        Self::new_with_controller_and_developer_mode(
            Some(LogFilterController::new_for_testing("info")),
            true,
        )
        .await
    }

    pub async fn new_with_controller(controller: Option<LogFilterController>) -> Self {
        Self::new_with_controller_and_developer_mode(controller, false).await
    }

    async fn new_with_controller_and_developer_mode(
        controller: Option<LogFilterController>,
        developer_mode: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel(128);
        let shell_service = ShellDbusService::new_with_developer_mode(
            tx.clone(),
            Arc::new(Mutex::new(None)),
            Arc::new(arc_swap::ArcSwap::from_pointee(ShellStatus::default())),
            Arc::new(arc_swap::ArcSwap::from_pointee(ShellTelemetry::default())),
            developer_mode,
        );
        let debug_service = DebugDbusService::new(controller, tx);

        Self::new_with_services(shell_service, debug_service, rx).await
    }

    pub async fn new_with_services(
        shell_service: ShellDbusService,
        debug_service: DebugDbusService,
        mailbox_rx: mpsc::Receiver<ShellCommand>,
    ) -> Self {
        let (server_stream, client_stream) = UnixStream::pair().expect("UnixStream pair");
        let guid = zbus::Guid::generate();
        let server_builder = zbus::connection::Builder::async_io_unix_stream(server_stream)
            .server(guid)
            .expect("server guid")
            .p2p()
            .serve_at("/org/shilpo/Shell", shell_service.clone())
            .expect("serve ShellDbusService")
            .serve_at("/org/shilpo/Shell", debug_service.clone())
            .expect("serve DebugDbusService");

        let client_builder = zbus::connection::Builder::async_io_unix_stream(client_stream).p2p();

        let (server_conn, client_conn) = wait_for("P2P connection build", async {
            tokio::try_join!(server_builder.build(), client_builder.build())
        })
        .await
        .expect("build p2p connections");

        let shell_proxy = wait_for(
            "ShellProxy build",
            ShellProxy::builder(&client_conn)
                .destination("org.shilpo.Shell")
                .expect("shell proxy destination")
                .build(),
        )
        .await
        .expect("build ShellProxy");

        let debug_proxy = wait_for(
            "DebugProxy build",
            DebugProxy::builder(&client_conn)
                .destination("org.shilpo.Shell")
                .expect("debug proxy destination")
                .build(),
        )
        .await
        .expect("build DebugProxy");

        Self {
            server_conn,
            client_conn,
            shell_proxy,
            debug_proxy,
            mailbox_rx,
            shell_service,
            debug_service,
        }
    }

    pub async fn signal_emitter(&self) -> zbus::object_server::SignalEmitter<'_> {
        let iface = wait_for(
            "ShellDbusService lookup",
            self.server_conn
                .object_server()
                .interface::<_, ShellDbusService>("/org/shilpo/Shell"),
        )
        .await
        .expect("find interface /org/shilpo/Shell");
        iface.signal_emitter().clone()
    }

    pub fn update_status(&self, status: ShellStatus) {
        self.shell_service.update_status(status);
    }

    pub fn update_telemetry(&self, telemetry: ShellTelemetry) {
        self.shell_service.update_telemetry(telemetry);
    }

    pub async fn introspect_xml(&self) -> String {
        let introspect = wait_for(
            "IntrospectableProxy build",
            zbus::fdo::IntrospectableProxy::builder(&self.client_conn)
                .destination("org.shilpo.Shell")
                .expect("introspect destination")
                .path("/org/shilpo/Shell")
                .expect("introspect path")
                .build(),
        )
        .await
        .expect("build IntrospectableProxy");

        wait_for("introspection response", introspect.introspect())
            .await
            .expect("introspect response")
    }
}
