use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::BackgroundExecutor;
use shilpo_services::DeviceClient;
pub use shilpo_services::DeviceCommand;
use shilpo_services::{self};
use shilpo_services::{AudioInfo, BatteryInfo, BrightnessInfo, MediaInfo, NetworkInfo};
use shilpo_services::{DomainPayload, DomainState};

use crate::config::ShellConfig;

pub type ConfigSender = tokio::sync::mpsc::Sender<ConfigUpdate>;
pub type ConfigReceiver = tokio::sync::mpsc::Receiver<ConfigUpdate>;
pub type CommandSender = tokio::sync::mpsc::Sender<WorkerCommand>;
pub type CommandReceiver = tokio::sync::mpsc::Receiver<WorkerCommand>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceSnapshot {
    pub battery: BatteryInfo,
    pub audio: AudioInfo,
    pub network: NetworkInfo,
    pub media: MediaInfo,
    pub brightness: BrightnessInfo,
}

impl DeviceSnapshot {
    pub fn apply_domain_state(&mut self, state: &DomainState) -> bool {
        match &state.payload {
            DomainPayload::Battery(payload) => {
                let info = payload.clone();
                if !info.available && self.battery.is_present {
                    if self.battery.available {
                        self.battery.available = false;
                        true
                    } else {
                        false
                    }
                } else {
                    replace_if_changed(&mut self.battery, &info)
                }
            }
            DomainPayload::Audio(payload) => {
                let info = audio_info(payload.clone());
                replace_if_changed(&mut self.audio, &info)
            }
            DomainPayload::Network(payload) => {
                let info = network_info(payload.clone());
                replace_if_changed(&mut self.network, &info)
            }
            DomainPayload::Media(payload) => {
                let info = media_info(payload.clone());
                replace_if_changed(&mut self.media, &info)
            }
            DomainPayload::Brightness(payload) => {
                let info = brightness_info(payload.clone());
                replace_if_changed(&mut self.brightness, &info)
            }
            _ => false,
        }
    }
}

fn replace_if_changed<T: PartialEq + Clone>(target: &mut T, value: &T) -> bool {
    if target == value {
        false
    } else {
        *target = value.clone();
        true
    }
}

#[derive(Debug)]
pub enum WorkerCommand {
    ReloadConfig,
    Device(DeviceCommand),
}

#[derive(Debug, Clone)]
pub enum ConfigUpdate {
    Loaded {
        config: Box<ShellConfig>,
        changeset: crate::config::ConfigChangeSet,
    },
    Failed {
        error: String,
        changeset: crate::config::ConfigChangeSet,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadTrigger {
    Manual,
    Watcher { burst_size: usize },
}

impl std::fmt::Display for ReloadTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manual => write!(f, "manual"),
            Self::Watcher { burst_size } => write!(f, "watcher (burst_size: {burst_size})"),
        }
    }
}

pub fn execute_reload_transaction(
    config_path: &Path,
    resolver: &crate::config::ConfigResolver,
    committed_snapshot: &mut crate::config::ConfigSnapshot,
    updates: &ConfigSender,
    trigger: ReloadTrigger,
) {
    let start_time = Instant::now();
    let (trigger_str, burst_size) = match trigger {
        ReloadTrigger::Manual => ("manual", 0),
        ReloadTrigger::Watcher { burst_size } => ("watcher", burst_size),
    };
    let reload_span = tracing::info_span!(
        target: "shilpo_profile",
        "config_reload",
        trigger = trigger_str,
        burst_size,
        outcome = tracing::field::Empty,
        diagnostic_count = tracing::field::Empty,
        theme_changed = tracing::field::Empty,
        bar_changed = tracing::field::Empty,
        desktop_changed = tracing::field::Empty,
        extensions_changed = tracing::field::Empty,
    );
    let _enter = reload_span.enter();

    // 1. ADR-0010 read-only primary status guard
    let migration = crate::config::MigrationService::for_primary_path(config_path);
    let block_reason = match migration.primary_status() {
        Ok(status) => crate::config::reload_block_reason(&status, config_path),
        Err(error) => Some(format!(
            "{}: {error}; run 'shilpo config migrate'",
            config_path.display()
        )),
    };

    if let Some(reason) = block_reason {
        reload_span.record("outcome", "migration_blocked");
        tracing::warn!(
            trigger = %trigger,
            error = %reason,
            elapsed = ?start_time.elapsed(),
            "reload blocked by primary status / migration guard"
        );
        let _ = updates.try_send(ConfigUpdate::Failed {
            error: reason,
            changeset: crate::config::ConfigChangeSet::default(),
        });
        return;
    }

    // 2. Perform resolution
    let (new_snapshot, changeset, report) = resolver.resolve_reload(committed_snapshot);

    // 3. Log unknown key warnings
    crate::config::unknown_keys::log_unknown_key_warnings(&report.unknown_keys);

    let elapsed = start_time.elapsed();

    reload_span.record("diagnostic_count", report.diagnostics.len());
    reload_span.record("theme_changed", changeset.theme);
    reload_span.record("bar_changed", changeset.bar);
    reload_span.record("desktop_changed", changeset.desktop);
    reload_span.record("extensions_changed", changeset.extensions);

    // 4. Handle resolution outcome
    if report.recovery_scope == Some(crate::config::RecoveryScope::RejectCandidate) {
        reload_span.record("outcome", "rejected");
        let msg = if report.diagnostics.is_empty() {
            "Configuration reload rejected".to_string()
        } else {
            report
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.path, d.message))
                .collect::<Vec<_>>()
                .join("; ")
        };

        tracing::error!(
            trigger = %trigger,
            recovery_scope = ?report.recovery_scope,
            diagnostic_count = report.diagnostics.len(),
            elapsed = ?elapsed,
            error = %msg,
            "configuration reload failed (candidate rejected)"
        );

        let _ = updates.try_send(ConfigUpdate::Failed {
            error: msg,
            changeset: crate::config::ConfigChangeSet::default(),
        });
    } else if changeset.is_empty() {
        reload_span.record("outcome", "no_op");
        // Successful candidate is byte/config equivalent; do not send redundant Loaded update
        tracing::debug!(
            trigger = %trigger,
            recovery_scope = ?report.recovery_scope,
            diagnostic_count = report.diagnostics.len(),
            elapsed = ?elapsed,
            "configuration reload completed with no changes (no-op)"
        );
    } else {
        reload_span.record("outcome", "applied");
        // Non-empty successful change set
        tracing::info!(
            trigger = %trigger,
            changed_components = ?changeset,
            recovery_scope = ?report.recovery_scope,
            diagnostic_count = report.diagnostics.len(),
            elapsed = ?elapsed,
            "configuration reloaded successfully"
        );

        *committed_snapshot = new_snapshot;
        let _ = updates.try_send(ConfigUpdate::Loaded {
            config: Box::new(committed_snapshot.config.clone()),
            changeset,
        });
    }
}

pub fn backoff_delay(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => Duration::from_secs(1),
        2 => Duration::from_secs(2),
        3 => Duration::from_secs(4),
        4 => Duration::from_secs(8),
        5 => Duration::from_secs(16),
        _ => Duration::from_secs(30),
    }
}

pub fn channels() -> (ConfigSender, ConfigReceiver, CommandSender, CommandReceiver) {
    let (updates_tx, updates_rx) = tokio::sync::mpsc::channel(64);
    let (commands_tx, commands_rx) = tokio::sync::mpsc::channel(32);
    (updates_tx, updates_rx, commands_tx, commands_rx)
}

pub fn try_send_command(
    sender: &CommandSender,
    command: WorkerCommand,
) -> Result<(), tokio::sync::mpsc::error::TrySendError<WorkerCommand>> {
    sender.try_send(command)
}

pub fn spawn(
    executor: BackgroundExecutor,
    updates: ConfigSender,
    commands: CommandReceiver,
    config_path: PathBuf,
    client: DeviceClient,
) -> gpui::Task<()> {
    let reconnect_client = client.clone();
    tokio::spawn(async move { reconnect_client.maintain_connection().await });
    executor.clone().spawn(async move {
        run(
            executor.clone(),
            updates,
            commands,
            config_path,
            Some(client),
        )
        .await
    })
}

/// Spawns only the shared transactional config publication loop.
pub fn spawn_config_updates(
    executor: BackgroundExecutor,
    config_path: PathBuf,
) -> (ConfigReceiver, gpui::Task<()>) {
    let (updates, receiver) = tokio::sync::mpsc::channel(64);
    let (_command_sender, commands) = tokio::sync::mpsc::channel(1);
    let task = executor
        .clone()
        .spawn(async move { run(executor.clone(), updates, commands, config_path, None).await });
    (receiver, task)
}

async fn run(
    executor: BackgroundExecutor,
    updates: ConfigSender,
    mut commands: CommandReceiver,
    config_path: PathBuf,
    client: Option<DeviceClient>,
) {
    let resolver = crate::config::ConfigResolver::from_primary_path(&config_path);
    let mut committed_snapshot = match resolver.resolve_initial() {
        Ok((snapshot, report)) => {
            crate::config::unknown_keys::log_unknown_key_warnings(&report.unknown_keys);
            let _ = updates.try_send(ConfigUpdate::Loaded {
                config: Box::new(snapshot.config.clone()),
                changeset: crate::config::ConfigChangeSet::all(),
            });
            snapshot
        }
        Err(e) => {
            let _ = updates.try_send(ConfigUpdate::Failed {
                error: e.to_string(),
                changeset: crate::config::ConfigChangeSet::default(),
            });
            crate::config::ConfigSnapshot::default()
        }
    };

    let config_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".config/shilpo"));

    let (watch_tx, watch_rx) = std::sync::mpsc::sync_channel(32);
    let mut watcher = match crate::config::watcher::ConfigWatcher::new(config_dir.clone(), watch_tx)
    {
        Ok(w) => Some(w),
        Err(err) => {
            tracing::error!(error = %err, path = ?config_dir, "failed to initialize configuration watcher");
            let _ = updates.try_send(ConfigUpdate::Failed {
                error: format!("Configuration watcher initialization failed: {err}"),
                changeset: crate::config::ConfigChangeSet::default(),
            });
            None
        }
    };

    let mut debounce =
        crate::config::watcher::DebounceStateMachine::new(Duration::from_millis(100));

    loop {
        let now = Instant::now();
        let mut event_occurred = false;
        if let Some(w) = watcher.as_mut() {
            if w.take_pending() {
                event_occurred = true;
            }
            while let Ok(event) = watch_rx.try_recv() {
                match event {
                    crate::config::watcher::ConfigWatchEvent::FilesystemChanged { paths } => {
                        event_occurred = true;
                        tracing::debug!(?paths, "configuration source changed");
                    }
                    crate::config::watcher::ConfigWatchEvent::RuntimeError(error) => {
                        let _ = updates.try_send(ConfigUpdate::Failed {
                            error: format!("Configuration watcher error: {error}"),
                            changeset: crate::config::ConfigChangeSet::default(),
                        });
                    }
                }
            }
            if event_occurred {
                w.refresh_watches();
                debounce.on_event(now);
            }
        }

        while let Ok(command) = commands.try_recv() {
            match command {
                WorkerCommand::ReloadConfig => {
                    execute_reload_transaction(
                        &config_path,
                        &resolver,
                        &mut committed_snapshot,
                        &updates,
                        ReloadTrigger::Manual,
                    );
                    debounce.on_reload_complete(Instant::now());
                }
                WorkerCommand::Device(command) => {
                    if let Some(client) = client.clone() {
                        tokio::spawn(async move {
                            if let Err(reason) = client.send_command(command).await {
                                tracing::warn!(%reason, "device command rejected");
                            }
                        });
                    }
                }
            }
        }

        let now = Instant::now();
        if let crate::config::watcher::DebounceAction::TriggerReload { burst_size } =
            debounce.tick(now)
        {
            execute_reload_transaction(
                &config_path,
                &resolver,
                &mut committed_snapshot,
                &updates,
                ReloadTrigger::Watcher { burst_size },
            );
            debounce.on_reload_complete(Instant::now());
        }

        executor.timer(Duration::from_millis(25)).await;
    }
}

fn optional(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

pub(crate) fn audio_info(payload: shilpo_services::AudioPayload) -> AudioInfo {
    fn device(v: shilpo_services::AudioDevicePayload) -> shilpo_services::AudioDevice {
        shilpo_services::AudioDevice {
            index: v.index,
            id: v.id,
            name: v.name,
            description: v.description,
            volume_percent: v.volume_percent,
            is_muted: v.is_muted,
            is_default: v.is_default,
            is_input: v.is_input,
            channels: v.channels,
            active_port: optional(v.active_port),
            ports: v
                .ports
                .into_iter()
                .map(|p| shilpo_services::AudioPort {
                    name: p.name,
                    description: p.description,
                    is_active: p.is_active,
                    available: p.available,
                })
                .collect(),
        }
    }
    AudioInfo {
        available: payload.available,
        default_sink_name: payload.default_sink_name,
        default_source_name: payload.default_source_name,
        volume: payload.volume,
        is_muted: payload.is_muted,
        input_volume: payload.input_volume,
        is_input_muted: payload.is_input_muted,
        sinks: payload.sinks.into_iter().map(device).collect(),
        sources: payload.sources.into_iter().map(device).collect(),
        app_streams: payload
            .app_streams
            .into_iter()
            .map(|s| shilpo_services::AudioStream {
                id: s.id,
                index: s.index,
                name: s.name,
                app_name: s.app_name,
                volume_percent: s.volume_percent,
                is_muted: s.is_muted,
            })
            .collect(),
    }
}

pub(crate) fn media_info(p: shilpo_services::MediaPayload) -> MediaInfo {
    let playback_state = match p.playback_state.as_str() {
        "playing" => shilpo_services::PlaybackState::Playing,
        "paused" => shilpo_services::PlaybackState::Paused,
        _ => shilpo_services::PlaybackState::Stopped,
    };
    MediaInfo {
        player_id: p.player_id,
        title: p.title,
        artist: p.artist,
        art_url: p.art_url,
        playback_state,
        can_play_pause: p.can_play_pause,
        can_go_next: p.can_go_next,
        position_secs: p.position_secs,
        length_secs: p.length_secs,
        rate: p.rate,
        observed_at: None,
    }
}

pub(crate) fn brightness_info(p: shilpo_services::BrightnessPayload) -> BrightnessInfo {
    BrightnessInfo {
        percentage: p.percentage,
        available: p.available,
        device_name: optional(p.device_name),
        displays: p
            .displays
            .into_iter()
            .map(|d| shilpo_services::brightness::DisplayBrightnessInfo {
                id: d.id,
                name: d.name,
                connector: optional(d.connector),
                percentage: d.percentage,
                is_primary: d.is_primary,
                backend: serde_json::from_str(&d.backend).unwrap_or_else(|_| {
                    shilpo_services::brightness::BrightnessBackend::DdcciSysfs {
                        sysfs_path: PathBuf::new(),
                    }
                }),
            })
            .collect(),
        primary_display_id: optional(p.primary_display_id),
        permissions_ok: p.permissions_ok,
    }
}

pub(crate) fn network_info(p: shilpo_services::NetworkPayload) -> NetworkInfo {
    use shilpo_services::network::{IpConfig, NetworkDevice, NetworkState, WifiAccessPoint};
    let state = match p.state.as_str() {
        "Asleep" | "asleep" => NetworkState::Asleep,
        "Disconnected" | "disconnected" => NetworkState::Disconnected,
        "Disconnecting" | "disconnecting" => NetworkState::Disconnecting,
        "Connecting" | "connecting" => NetworkState::Connecting,
        "ConnectedLocal" | "connected_local" => NetworkState::ConnectedLocal,
        "ConnectedSite" | "connected_site" => NetworkState::ConnectedSite,
        "ConnectedGlobal" | "connected_global" => NetworkState::ConnectedGlobal,
        _ => NetworkState::Unknown,
    };
    NetworkInfo {
        available: p.available,
        is_connected: p.is_connected,
        connection_type: p.connection_type,
        ssid: optional(p.ssid),
        wifi_enabled: p.wifi_enabled,
        wwan_enabled: p.wwan_enabled,
        airplane_mode: p.airplane_mode,
        state,
        access_points: p
            .access_points
            .into_iter()
            .map(|a| WifiAccessPoint {
                ssid: a.ssid,
                bssid: a.bssid,
                signal_percent: a.signal_percent,
                security_type: a.security_type,
                frequency_mhz: a.frequency_mhz,
                is_connected: a.is_connected,
                object_path: a.object_path,
            })
            .collect(),
        active_vpns: p
            .active_vpns
            .into_iter()
            .map(|v| shilpo_services::VpnConnection {
                id: v.id,
                uuid: v.uuid,
                vpn_type: v.vpn_type,
                is_active: v.is_active,
                object_path: v.object_path,
            })
            .collect(),
        devices: p
            .devices
            .into_iter()
            .map(|d| NetworkDevice {
                interface: d.interface,
                device_type: d.device_type,
                state: d.state,
                carrier: d.carrier,
                object_path: d.object_path,
            })
            .collect(),
        ip_config: p.has_ip_config.then(|| IpConfig {
            ipv4_address: optional(p.ip_config.ipv4_address),
            ipv4_gateway: optional(p.ip_config.ipv4_gateway),
            ipv6_address: optional(p.ip_config.ipv6_address),
            ipv6_gateway: optional(p.ip_config.ipv6_gateway),
            dns_servers: p.ip_config.dns_servers,
        }),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use shilpo_services::{DomainLifecycle, DomainVersion};

    #[test]
    fn device_snapshot_applies_updates_correctly() {
        let mut snapshot = DeviceSnapshot::default();
        let battery = BatteryInfo {
            percentage: 85,
            state: shilpo_services::BatteryChargeState::Charging,
            is_present: true,
            available: true,
            ..Default::default()
        };
        assert!(snapshot.apply_domain_state(&DomainState {
            domain: shilpo_services::DeviceDomain::Battery,
            version: DomainVersion::new(1, 1),
            lifecycle: DomainLifecycle::Ready,
            payload: DomainPayload::Battery(battery.clone()),
            error: None,
        }));
        assert_eq!(snapshot.battery, battery);
        assert!(!snapshot.apply_domain_state(&DomainState {
            domain: shilpo_services::DeviceDomain::Battery,
            version: DomainVersion::new(1, 2),
            lifecycle: DomainLifecycle::Ready,
            payload: DomainPayload::Battery(battery),
            error: None,
        }));
    }

    #[test]
    fn battery_snapshot_keeps_last_known_data_when_service_becomes_unavailable() {
        let mut snapshot = DeviceSnapshot::default();
        snapshot.apply_domain_state(&DomainState {
            domain: shilpo_services::DeviceDomain::Battery,
            version: DomainVersion::new(1, 1),
            lifecycle: DomainLifecycle::Ready,
            payload: DomainPayload::Battery(BatteryInfo {
                available: true,
                is_present: true,
                percentage: 73,
                state: shilpo_services::BatteryChargeState::Discharging,
                ..Default::default()
            }),
            error: None,
        });

        assert!(snapshot.apply_domain_state(&DomainState {
            domain: shilpo_services::DeviceDomain::Battery,
            version: DomainVersion::new(1, 2),
            lifecycle: DomainLifecycle::Unavailable,
            payload: DomainPayload::Battery(BatteryInfo::default()),
            error: None,
        }));
        assert_eq!(snapshot.battery.percentage, 73);
        assert!(snapshot.battery.is_present);
        assert!(!snapshot.battery.available);
    }

    #[test]
    fn backoff_caps_at_thirty_seconds() {
        assert_eq!(backoff_delay(99), Duration::from_secs(30));
    }

    #[test]
    fn reload_transaction_publishes_changeset_and_retains_snapshot_on_failure() {
        let temp = TempDir::new().unwrap();
        let primary = temp.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Inter\"\n").unwrap();

        let resolver = crate::config::ConfigResolver::from_primary_path(&primary);
        let (mut snapshot, _) = resolver.resolve_initial().unwrap();
        let (updates_tx, mut updates_rx) = tokio::sync::mpsc::channel(16);

        // 1. Successful change
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Roboto\"\n").unwrap();
        execute_reload_transaction(
            &primary,
            &resolver,
            &mut snapshot,
            &updates_tx,
            ReloadTrigger::Manual,
        );

        assert_eq!(snapshot.config.theme.font_family, "Roboto");
        let update = updates_rx.try_recv().unwrap();
        if let ConfigUpdate::Loaded { config, changeset } = update {
            assert_eq!(config.theme.font_family, "Roboto");
            assert!(changeset.theme);
            assert!(!changeset.bar);
        } else {
            panic!("expected ConfigUpdate::Loaded");
        }

        // 2. Unchanged reload -> produces no Loaded update
        execute_reload_transaction(
            &primary,
            &resolver,
            &mut snapshot,
            &updates_tx,
            ReloadTrigger::Watcher { burst_size: 1 },
        );
        assert!(updates_rx.try_recv().is_err());

        // 3. Failed syntax edit -> retains snapshot and emits ConfigUpdate::Failed
        std::fs::write(&primary, "invalid syntax {{{\n").unwrap();
        execute_reload_transaction(
            &primary,
            &resolver,
            &mut snapshot,
            &updates_tx,
            ReloadTrigger::Watcher { burst_size: 2 },
        );
        assert_eq!(snapshot.config.theme.font_family, "Roboto");
        let update = updates_rx.try_recv().unwrap();
        if let ConfigUpdate::Failed {
            error: msg,
            changeset,
        } = update
        {
            assert!(changeset.is_empty());
            assert!(
                msg.contains("syntax") || msg.contains("invalid") || msg.contains("config.toml")
            );
        } else {
            panic!("expected ConfigUpdate::Failed");
        }

        // 4. Old/invalid version -> blocks reload with migrate diagnostic
        std::fs::write(&primary, "version = 999\n").unwrap();
        execute_reload_transaction(
            &primary,
            &resolver,
            &mut snapshot,
            &updates_tx,
            ReloadTrigger::Manual,
        );
        assert_eq!(snapshot.config.theme.font_family, "Roboto");
        let update = updates_rx.try_recv().unwrap();
        if let ConfigUpdate::Failed {
            error: msg,
            changeset,
        } = update
        {
            assert!(changeset.is_empty());
            assert!(msg.contains("shilpo config migrate"));
        } else {
            panic!("expected ConfigUpdate::Failed for version 999");
        }
    }
}
