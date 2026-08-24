use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Path, PathBuilder, Pixels, Point,
    Render, Styled, Window, div, prelude::*, px,
};
use shilpo_m3e::ElementExt;
use shilpo_m3e::{ActiveTheme, h_flex, v_flex};
use shilpo_services::{
    Application, AudioInfo, BatteryInfo, BluetoothInfo, MediaInfo, NetworkInfo, Notification,
};

use super::geometry::HUG_CORNER_RADIUS;
use crate::config::{BarPosition, BarWidget, ShellConfig};
use crate::shell::bar::cards::{
    adapter::CardCoordinator,
    model::{CardRequest, CardSourceId},
};
use crate::shell::bar::service_worker::{self, WorkerCommand};
use crate::shell::bar::widgets::clock::{format_clock, format_date};
use crate::shell::battery::BatteryIndicator;
use crate::shell::osd::OsdKind;
use crate::shell::runtime::{ShellRuntime, ShellSurfaces, SurfaceRequest};

fn build_hug_corner(
    start: Point<Pixels>,
    edge_end: Point<Pixels>,
    arc_end: Point<Pixels>,
    control_a: Point<Pixels>,
    control_b: Point<Pixels>,
) -> Option<Path<Pixels>> {
    let mut builder = PathBuilder::fill();
    builder.move_to(start);
    builder.line_to(edge_end);
    builder.cubic_bezier_to(arc_end, control_a, control_b);
    builder.close();
    builder.build().ok()
}

pub fn parse_hex_color(hex: &str) -> Option<u32> {
    let clean = hex.trim_start_matches('#');
    if clean.len() == 6 {
        let rgb = u32::from_str_radix(clean, 16).ok()?;
        Some(0xff000000 | rgb)
    } else if clean.len() == 8 {
        u32::from_str_radix(clean, 16).ok()
    } else {
        None
    }
}

pub fn apply_config_theme(_config: &ShellConfig, _window: Option<&mut Window>, cx: &mut App) {
    if let Some(state) = shilpo_theme_daemon::read_state_snapshot() {
        shilpo_m3e::Theme::global_mut(cx).apply_state(&state);
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
enum BarViewEffect {
    ShowOsd(OsdKind),
    ShowNotificationToast(Notification),
    ApplyConfigTheme(ShellConfig),
}

#[derive(Debug, Clone, PartialEq, Default)]
struct BarUpdateResult {
    changed: bool,
    effects: Vec<BarViewEffect>,
}

/// Status Bar GPUI View (Multi-Capsule Segmented Bar with IPC integration).
pub struct BarView {
    pub config: ShellConfig,
    pub battery: BatteryInfo,
    pub audio: AudioInfo,
    pub network: NetworkInfo,
    pub bluetooth: BluetoothInfo,
    pub caffeine_service: Arc<shilpo_services::CaffeineService>,
    pub app_id: String,
    pub active_title: String,
    pub media_info: Option<MediaInfo>,
    pub time_str: String,
    pub date_str: String,
    pub cpu_percent: u8,
    pub ram_percent: u8,
    pub cat_frame_index: usize,
    pub last_error: Option<String>,
    pub last_service_update: Instant,

    service_commands: service_worker::CommandSender,
    _datetime_task: Option<gpui::Task<()>>,
    _sysinfo_task: Option<gpui::Task<()>>,
    extension_instance_prefix: Option<String>,
    display_id: Option<gpui::DisplayId>,
    output_name: Option<String>,

    app_icons_cache: Arc<std::collections::HashMap<String, std::path::PathBuf>>,
    app_icons_cache_apps: Vec<Application>,
}

impl BarView {
    fn new_with_commands(
        config: ShellConfig,
        service_commands: service_worker::CommandSender,
    ) -> Self {
        let battery = BatteryInfo::default();
        let audio = AudioInfo::default();
        let network = NetworkInfo::default();
        let bluetooth = shilpo_services::BluetoothService::new()
            .map(|s| s.info())
            .unwrap_or_default();
        let caffeine_service = Arc::new(shilpo_services::CaffeineService::new());

        let now = chrono::Local::now();
        let time_str = format_clock(&now, config.clock_format.as_deref());
        let date_str = format_date(&now);

        Self {
            config,
            battery,
            audio,
            network,
            bluetooth,
            caffeine_service,
            app_id: "shilpo.shell".into(),
            active_title: "Shilpo Shell".into(),
            media_info: None,
            time_str,
            date_str,
            cpu_percent: 0,
            ram_percent: 0,
            cat_frame_index: 0,
            last_error: None,
            last_service_update: Instant::now(),
            service_commands,
            _datetime_task: None,
            _sysinfo_task: None,
            extension_instance_prefix: None,
            display_id: None,
            output_name: None,
            app_icons_cache: Arc::new(std::collections::HashMap::new()),
            app_icons_cache_apps: Vec::new(),
        }
    }

    pub fn update_datetime(&mut self) -> bool {
        let now = chrono::Local::now();
        let fmt = self.config.clock_format.as_deref();
        let new_time = format_clock(&now, fmt);
        let new_date = format_date(&now);
        let mut changed = false;

        if self.time_str != new_time {
            self.time_str = new_time;
            changed = true;
        }
        if self.date_str != new_date {
            self.date_str = new_date;
            changed = true;
        }

        changed
    }

    pub fn is_stale(&self) -> bool {
        self.last_service_update.elapsed() > Duration::from_secs(30)
    }

    pub fn reload_config(&self, _cx: &mut App) -> Result<(), String> {
        service_worker::try_send_command(&self.service_commands, WorkerCommand::ReloadConfig)
            .map_err(|e| format!("Failed to send worker command: {}", e))
    }

    pub fn new_with_config(
        window: &mut Window,
        cx: &mut Context<Self>,
        config: ShellConfig,
    ) -> Self {
        window.on_window_should_close(cx, |_, cx| {
            ShellSurfaces::forget_bar(cx);
            true
        });

        // Dynamic theme synchronization with OS appearance and config
        apply_config_theme(&config, Some(window), cx);

        let service_commands = ShellRuntime::service_commands(cx).unwrap_or_else(|| {
            let (_, _, tx, _) = service_worker::channels();
            tx
        });

        let mut view = Self::new_with_commands(config, service_commands);

        // The device daemon publishes its initial snapshot for each domain once, right
        // after connecting -- it does not repeat it. If that broadcast lands before this
        // bar window exists (a real race: the daemon round-trip is fast, bar construction
        // is not), `view.network` is stuck at `NetworkInfo::default()` forever, since
        // nothing about the network needs to change again to trigger another update.
        // Hydrate from whatever the service hub already has cached so a bar created after
        // the fact still reflects the current state instead of a stale "off" default.
        if cx.has_global::<ShellRuntime>()
            && let Some(hub) = cx.global::<ShellRuntime>().service_hub()
        {
            let state = hub.domain_state(shilpo_services::DeviceDomain::Network);
            if let shilpo_services::DomainPayload::Network(ref payload) = state.payload {
                view.network = service_worker::network_info(payload.clone());
            }
        }

        let _datetime_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let res = this.update(cx, |view, cx| {
                    if view.update_datetime() {
                        cx.notify();
                    }
                });
                if res.is_err() {
                    break;
                }
            }
        }));

        let _sysinfo_task = Some(cx.spawn(async move |this, cx| {
            use sysinfo::System;
            let mut sys = System::new();
            let mut last_cpu_sample = Instant::now() - Duration::from_secs(2);
            let mut cpu_pct: u8 = 0;
            let mut ram_pct: u8 = 0;

            loop {
                if last_cpu_sample.elapsed() >= Duration::from_secs(1) {
                    sys.refresh_cpu_usage();
                    sys.refresh_memory();
                    cpu_pct = sys.global_cpu_usage().round().clamp(0.0, 100.0) as u8;
                    let total_mem = sys.total_memory();
                    let used_mem = sys.used_memory();
                    ram_pct = if total_mem > 0 {
                        ((used_mem as f64 / total_mem as f64) * 100.0)
                            .round()
                            .clamp(0.0, 100.0) as u8
                    } else {
                        0
                    };
                    last_cpu_sample = Instant::now();
                }

                let speed = ((cpu_pct as f32) / 5.0).clamp(1.0, 20.0);
                let interval_ms = (500.0 / speed) as u64;

                cx.background_executor()
                    .timer(Duration::from_millis(interval_ms))
                    .await;

                let res = this.update(cx, |view, cx| {
                    view.cpu_percent = cpu_pct;
                    view.ram_percent = ram_pct;
                    view.cat_frame_index = (view.cat_frame_index + 1) % 5;
                    cx.notify();
                });
                if res.is_err() {
                    break;
                }
            }
        }));

        view._datetime_task = _datetime_task;
        view._sysinfo_task = _sysinfo_task;
        view
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_config(window, cx, ShellConfig::default())
    }

    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self::new(window, cx))
    }

    pub fn view_with_config(
        window: &mut Window,
        cx: &mut App,
        config: ShellConfig,
    ) -> Entity<Self> {
        cx.new(|cx| Self::new_with_config(window, cx, config))
    }

    pub fn view_with_config_on_display(
        window: &mut Window,
        cx: &mut App,
        config: ShellConfig,
        display_id: gpui::DisplayId,
    ) -> Entity<Self> {
        Self::view_with_config_on_display_with_output(window, cx, config, display_id, None)
    }

    pub fn view_with_config_on_display_with_output(
        window: &mut Window,
        cx: &mut App,
        config: ShellConfig,
        display_id: gpui::DisplayId,
        output_name: Option<String>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let mut view = Self::new_with_config(window, cx, config);
            view.extension_instance_prefix = Some(format!("bar:{display_id:?}"));
            view.display_id = Some(display_id);
            view.output_name = output_name;
            view
        })
    }

    fn compute_domain_update(
        &mut self,
        domain: shilpo_services::DeviceDomain,
        state: &shilpo_services::DomainState,
    ) -> BarUpdateResult {
        self.last_service_update = Instant::now();
        let mut changed = false;
        let mut effects = Vec::new();

        match domain {
            shilpo_services::DeviceDomain::Battery => {
                if let shilpo_services::DomainPayload::Battery(ref value) = state.payload
                    && &self.battery != value
                {
                    self.battery = value.clone();
                    changed = true;
                }
            }
            shilpo_services::DeviceDomain::Audio => {
                if let shilpo_services::DomainPayload::Audio(ref payload) = state.payload {
                    let value = service_worker::audio_info(payload.clone());
                    if self.audio != value {
                        let show_osd = self.audio.available
                            && value.available
                            && (self.audio.volume != value.volume
                                || self.audio.is_muted != value.is_muted);
                        self.audio = value.clone();
                        if show_osd {
                            effects.push(BarViewEffect::ShowOsd(OsdKind::Volume {
                                level: value.volume as u32,
                                muted: value.is_muted,
                            }));
                        }
                        changed = true;
                    }
                }
            }
            shilpo_services::DeviceDomain::Network => {
                if let shilpo_services::DomainPayload::Network(ref payload) = state.payload {
                    let value = service_worker::network_info(payload.clone());
                    if self.network != value {
                        self.network = value;
                        changed = true;
                    }
                }
            }
            shilpo_services::DeviceDomain::Media => {
                if let shilpo_services::DomainPayload::Media(ref payload) = state.payload {
                    let value = service_worker::media_info(payload.clone());
                    if self.media_info.as_ref() != Some(&value) {
                        self.media_info = Some(value);
                        changed = true;
                    }
                }
            }
            _ => {}
        }

        BarUpdateResult { changed, effects }
    }

    fn compute_config_loaded(
        &mut self,
        config: &ShellConfig,
        changeset: &crate::config::ConfigChangeSet,
    ) -> BarUpdateResult {
        self.last_service_update = Instant::now();
        let mut effects = Vec::new();
        self.config = config.clone();
        self.last_error = None;
        if changeset.theme {
            effects.push(BarViewEffect::ApplyConfigTheme(self.config.clone()));
        }
        if changeset.clock_format || changeset.temperature_unit || changeset.locale {
            self.update_datetime();
        }
        BarUpdateResult {
            changed: true,
            effects,
        }
    }

    fn compute_config_failed(&mut self, error: &str) -> BarUpdateResult {
        self.last_service_update = Instant::now();
        tracing::error!(error = %error, "config reload failed");
        self.last_error = Some(error.to_string());
        BarUpdateResult {
            changed: true,
            effects: vec![BarViewEffect::ShowNotificationToast(Notification::new(
                "Configuration Warning",
                error,
            ))],
        }
    }

    pub fn apply_domain_update(
        &mut self,
        domain: shilpo_services::DeviceDomain,
        state: &shilpo_services::DomainState,
        cx: &mut Context<Self>,
    ) {
        let result = self.compute_domain_update(domain, state);
        self.dispatch_effects(result.effects, cx);
        if result.changed {
            cx.notify();
        }
    }

    pub fn apply_config_loaded(
        &mut self,
        config: &ShellConfig,
        changeset: &crate::config::ConfigChangeSet,
        cx: &mut Context<Self>,
    ) {
        let result = self.compute_config_loaded(config, changeset);
        self.dispatch_effects(result.effects, cx);
        if result.changed {
            cx.notify();
        }
    }

    pub fn apply_config_failed(&mut self, error: &str, cx: &mut Context<Self>) {
        let result = self.compute_config_failed(error);
        self.dispatch_effects(result.effects, cx);
        if result.changed {
            cx.notify();
        }
    }

    fn dispatch_effects(&mut self, effects: Vec<BarViewEffect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                BarViewEffect::ShowOsd(kind) => {
                    ShellSurfaces::request(cx, SurfaceRequest::ShowOsd(kind));
                }
                BarViewEffect::ShowNotificationToast(notification) => {
                    ShellSurfaces::request(cx, SurfaceRequest::ShowNotification(notification));
                }
                BarViewEffect::ApplyConfigTheme(config) => {
                    apply_config_theme(&config, None, cx);
                }
            }
        }
    }

    pub fn build(
        spec: crate::shell::bar::BarSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new_with_config(window, cx, ShellConfig::default());
        view.config.bar = spec.config;
        view.extension_instance_prefix = Some(format!("bar:{:?}", spec.display_id));
        view.display_id = Some(spec.display_id);
        view.output_name = spec.output_name;
        view
    }

    pub fn view_with_spec(
        spec: crate::shell::bar::BarSpec,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self::build(spec, window, cx))
    }
}

impl BarView {
    fn build_section(
        &mut self,
        section_name: &str,
        widget_names: &[BarWidget],
        side: bool,
        is_floating: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        use crate::config::{BarWidget, BuiltinBarWidget};
        let mut elements: Vec<gpui::AnyElement> = Vec::new();
        let desired_menus = ShellRuntime::descriptors_for(
            cx,
            crate::shell::extensions::ContributionSurface::BarMenu,
        )
        .into_iter()
        .map(|descriptor| descriptor.id)
        .collect();
        CardCoordinator::reconcile_extension_menu_providers(cx, &desired_menus);

        for (index, name) in widget_names.iter().enumerate() {
            match name {
                BarWidget::Builtin(BuiltinBarWidget::Workspaces) => {
                    let snapshot = ShellSurfaces::compositor_snapshot(cx);
                    let is_vertical = matches!(
                        self.config.bar.position,
                        crate::config::BarPosition::Left | crate::config::BarPosition::Right
                    );
                    let pill_orientation = if is_vertical {
                        super::widgets::pill_strip::PillOrientation::Vertical
                    } else {
                        super::widgets::pill_strip::PillOrientation::Horizontal
                    };
                    elements.push(
                        super::widgets::WorkspacesWidget::new(
                            format!(
                                "workspaces_{:?}_{}_{}_{}",
                                self.display_id,
                                self.output_name.as_deref().unwrap_or("unknown-output"),
                                section_name,
                                index
                            ),
                            self.display_id,
                            snapshot.workspaces.clone(),
                            snapshot.connection,
                            pill_orientation,
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::RunningApps) => {
                    let snapshot = ShellSurfaces::compositor_snapshot(cx);
                    // build_app_icon_index does a full icon-theme scan; this method runs on
                    // every render (i.e. every compositor frame callback), so rebuilding it
                    // unconditionally here pegged a CPU core doing tens of thousands of
                    // statx() calls per second. Only rebuild when the running app list
                    // actually changed.
                    let applications = ShellSurfaces::overview_applications(cx);
                    if self.app_icons_cache_apps != applications {
                        self.app_icons_cache = std::sync::Arc::new(
                            crate::shell::app_icons::build_app_icon_index(applications.clone()),
                        );
                        self.app_icons_cache_apps = applications;
                    }
                    let app_icons = self.app_icons_cache.clone();
                    let reduced_motion = ShellSurfaces::overview_reduced_motion(cx);
                    let is_vertical = matches!(
                        self.config.bar.position,
                        crate::config::BarPosition::Left | crate::config::BarPosition::Right
                    );
                    let pill_orientation = if is_vertical {
                        super::widgets::pill_strip::PillOrientation::Vertical
                    } else {
                        super::widgets::pill_strip::PillOrientation::Horizontal
                    };
                    elements.push(
                        super::widgets::RunningAppsWidget::new(
                            format!("running_apps_{section_name}_{index}"),
                            self.output_name.clone(),
                            (*snapshot).clone(),
                            app_icons,
                            pill_orientation,
                            reduced_motion,
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::Clock) => {
                    let is_vertical = matches!(
                        self.config.bar.position,
                        crate::config::BarPosition::Left | crate::config::BarPosition::Right
                    );
                    elements.push(
                        super::widgets::ClockWidget::new(
                            format!("clock_{section_name}_{index}"),
                            self.time_str.clone(),
                            is_vertical,
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::Date) => {
                    let is_vertical = matches!(
                        self.config.bar.position,
                        crate::config::BarPosition::Left | crate::config::BarPosition::Right
                    );
                    elements.push(
                        super::widgets::DateWidget::new(
                            format!("date_{section_name}_{index}"),
                            self.date_str.clone(),
                            is_vertical,
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::Media) => {
                    if let Some(info) = &self.media_info
                        && !info.is_empty()
                    {
                        let is_vertical = matches!(
                            self.config.bar.position,
                            crate::config::BarPosition::Left | crate::config::BarPosition::Right
                        );
                        elements.push(
                            super::widgets::MediaWidget::new(
                                format!("media_{section_name}_{index}"),
                                info.clone(),
                                is_vertical,
                                self.service_commands.clone(),
                            )
                            .into_any_element(),
                        );
                    }
                }
                BarWidget::Builtin(BuiltinBarWidget::Battery) => {
                    if self.battery.is_present {
                        elements.push(
                            BatteryIndicator::new(
                                format!("battery_{section_name}_{index}"),
                                self.battery.clone(),
                                self.display_id,
                            )
                            .into_any_element(),
                        );
                    }
                }
                BarWidget::Builtin(BuiltinBarWidget::Sysinfo) => {
                    elements.push(
                        super::widgets::SysInfoWidget::new(
                            format!("sysinfo_{section_name}_{index}"),
                            self.cat_frame_index,
                            self.cpu_percent,
                            self.ram_percent,
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::Network) => {
                    elements.push(
                        super::widgets::NetworkWidget::new(
                            format!("network_{section_name}_{index}"),
                            self.network.clone(),
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::Bluetooth) => {
                    elements.push(
                        super::widgets::BluetoothWidget::new(
                            format!("bluetooth_{section_name}_{index}"),
                            self.bluetooth.clone(),
                        )
                        .into_any_element(),
                    );
                }
                BarWidget::Builtin(BuiltinBarWidget::Caffeine) => {
                    let caffeine_svc = self.caffeine_service.clone();
                    let is_active = caffeine_svc.is_active();
                    elements.push(
                        crate::shell::widgets::CaffeineWidget::new(
                            format!("caffeine_{section_name}_{index}"),
                            is_active,
                        )
                        .on_click(move |_, _, _cx| {
                            caffeine_svc.toggle();
                        })
                        .into_any_element(),
                    );
                }
                BarWidget::Extension(ext_ref) => {
                    if let Some(tree) = ShellRuntime::extension_view(cx, ext_ref) {
                        let instance_id_str = self
                            .extension_instance_prefix
                            .as_ref()
                            .map(|prefix| format!("{prefix}:{section_name}:{index}"))
                            .unwrap_or_else(|| format!("bar:{section_name}:{index}"));

                        let menu_descriptor = ShellRuntime::descriptors_for(
                            cx,
                            crate::shell::extensions::ContributionSurface::BarMenu,
                        )
                        .into_iter()
                        .find(|desc| desc.bar_widget.as_ref() == Some(ext_ref));

                        let view_element = super::ext_view_adapter::render_ext_view_tree(
                            ext_ref,
                            Some(&instance_id_str),
                            &tree,
                            window,
                            cx,
                        );

                        if let Some(menu_desc) = menu_descriptor {
                            let provider = std::sync::Arc::new(
                                super::cards::extension_menu::ExtensionMenuCardProvider::new(
                                    menu_desc.id.clone(),
                                    ext_ref.clone(),
                                ),
                            );
                            CardCoordinator::register_extension_menu_provider(
                                cx,
                                provider,
                                menu_desc.id.clone(),
                            );

                            let source_id = CardSourceId::new(
                                menu_desc.id,
                                instance_id_str,
                                Option::<gpui::SharedString>::None,
                            );
                            // This wrapper is purely the click target -- consistent padding and
                            // a pill-shaped hit region. It does not draw its own "active" state:
                            // the extension's palette now covers the full Material role set
                            // (SemanticColorToken mirrors ThemeColor directly), so an extension
                            // that wants a battery-style selected look can recolor its own
                            // icon/text as a matched pair (e.g. secondary_container/
                            // on_secondary_container) using the `BarMenuOpened`/`BarMenuClosed`
                            // events it already receives, exactly like a native widget would.
                            // A second, host-drawn highlight layered on top of that would just
                            // double up as a visually conflicting extra ring around it.
                            let mut wrapper = div()
                                .id(format!("ext_widget_menu_{section_name}_{index}"))
                                .px(px(6.0))
                                .py(px(2.0))
                                .rounded_full();
                            let source_id_clone = source_id.clone();
                            let source_id_prepaint = source_id.clone();
                            let current_display = self.display_id;
                            wrapper = wrapper
                                .cursor_pointer()
                                .on_prepaint(move |bounds, _, cx| {
                                    if let Some(display_id) = current_display {
                                        CardCoordinator::dispatch(
                                            cx,
                                            CardRequest::AnchorUpdate {
                                                source: source_id_prepaint.clone(),
                                                bounds,
                                                display_id,
                                            },
                                        );
                                    }
                                })
                                .on_click(move |_, _, cx| {
                                    if let Some(display_id) = current_display {
                                        CardCoordinator::dispatch(
                                            cx,
                                            CardRequest::PersistentToggle {
                                                source: source_id_clone.clone(),
                                            },
                                        );
                                        let _ = display_id;
                                    }
                                });
                            elements.push(wrapper.child(view_element).into_any_element());
                        } else {
                            elements.push(view_element);
                        }
                    }
                }
                _ => {}
            }
        }

        if elements.is_empty() {
            return div().into_any_element();
        }

        let flex = if side { v_flex() } else { h_flex() };
        let section = flex
            .gap(px(self.config.bar.widget_spacing as f32))
            .items_center()
            .children(elements);

        if is_floating {
            div()
                .p_1()
                .px_2()
                .rounded_full()
                .bg(cx.theme().surface_container_high.opacity(0.92))
                .border_1()
                .border_color(cx.theme().outline_variant.opacity(0.3))
                .shadow_md()
                .child(section)
                .into_any_element()
        } else {
            section.into_any_element()
        }
    }

    fn render_hug_corners(
        &self,
        position: BarPosition,
        radius: gpui::Pixels,
        bg_color: gpui::Hsla,
    ) -> impl IntoElement {
        use gpui::{canvas, point};

        let bar_height = px(self.config.bar.height as f32);

        canvas(
            move |_, _, _| {},
            move |bounds, _, window, _| {
                let r = radius.as_f32();
                let k = r * 0.552_284_8;

                match position {
                    BarPosition::Top => {
                        let y0 = bounds.origin.y.as_f32() + bar_height.as_f32() - 0.5;

                        let x0 = bounds.origin.x.as_f32();
                        if let Some(path) = build_hug_corner(
                            point(px(x0), px(y0)),
                            point(px(x0), px(y0 + r)),
                            point(px(x0 + r), px(y0)),
                            point(px(x0), px(y0 + r - k)),
                            point(px(x0 + r - k), px(y0)),
                        ) {
                            window.paint_path(path, bg_color);
                        }

                        let x1 = bounds.origin.x.as_f32() + bounds.size.width.as_f32();
                        if let Some(path) = build_hug_corner(
                            point(px(x1), px(y0)),
                            point(px(x1), px(y0 + r)),
                            point(px(x1 - r), px(y0)),
                            point(px(x1), px(y0 + r - k)),
                            point(px(x1 - r + k), px(y0)),
                        ) {
                            window.paint_path(path, bg_color);
                        }
                    }
                    BarPosition::Bottom => {
                        let y0 = bounds.origin.y.as_f32() + bounds.size.height.as_f32()
                            - bar_height.as_f32()
                            + 0.5;

                        let x0 = bounds.origin.x.as_f32();
                        if let Some(path) = build_hug_corner(
                            point(px(x0), px(y0)),
                            point(px(x0), px(y0 - r)),
                            point(px(x0 + r), px(y0)),
                            point(px(x0), px(y0 - r + k)),
                            point(px(x0 + r - k), px(y0)),
                        ) {
                            window.paint_path(path, bg_color);
                        }

                        let x1 = bounds.origin.x.as_f32() + bounds.size.width.as_f32();
                        if let Some(path) = build_hug_corner(
                            point(px(x1), px(y0)),
                            point(px(x1), px(y0 - r)),
                            point(px(x1 - r), px(y0)),
                            point(px(x1), px(y0 - r + k)),
                            point(px(x1 - r + k), px(y0)),
                        ) {
                            window.paint_path(path, bg_color);
                        }
                    }
                    BarPosition::Left => {
                        let x0 = bounds.origin.x.as_f32() + bar_height.as_f32() - 0.5;
                        let y0 = bounds.origin.y.as_f32();
                        let y1 = bounds.origin.y.as_f32() + bounds.size.height.as_f32();

                        if let Some(path) = build_hug_corner(
                            point(px(x0), px(y0)),
                            point(px(x0 + r), px(y0)),
                            point(px(x0), px(y0 + r)),
                            point(px(x0 + r - k), px(y0)),
                            point(px(x0), px(y0 + r - k)),
                        ) {
                            window.paint_path(path, bg_color);
                        }

                        if let Some(path) = build_hug_corner(
                            point(px(x0), px(y1)),
                            point(px(x0 + r), px(y1)),
                            point(px(x0), px(y1 - r)),
                            point(px(x0 + r - k), px(y1)),
                            point(px(x0), px(y1 - r + k)),
                        ) {
                            window.paint_path(path, bg_color);
                        }
                    }
                    BarPosition::Right => {
                        let x0 = bounds.origin.x.as_f32() + bounds.size.width.as_f32()
                            - bar_height.as_f32()
                            + 0.5;
                        let y0 = bounds.origin.y.as_f32();
                        let y1 = bounds.origin.y.as_f32() + bounds.size.height.as_f32();

                        if let Some(path) = build_hug_corner(
                            point(px(x0), px(y0)),
                            point(px(x0 - r), px(y0)),
                            point(px(x0), px(y0 + r)),
                            point(px(x0 - r + k), px(y0)),
                            point(px(x0), px(y0 + r - k)),
                        ) {
                            window.paint_path(path, bg_color);
                        }

                        if let Some(path) = build_hug_corner(
                            point(px(x0), px(y1)),
                            point(px(x0 - r), px(y1)),
                            point(px(x0), px(y1 - r)),
                            point(px(x0 - r + k), px(y1)),
                            point(px(x0), px(y1 - r + k)),
                        ) {
                            window.paint_path(path, bg_color);
                        }
                    }
                }
            },
        )
        .size_full()
        .absolute()
        .inset_0()
    }
}

pub(crate) fn compute_bar_input_region(
    child_bounds: &[gpui::Bounds<gpui::Pixels>],
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let mut region: Option<gpui::Bounds<gpui::Pixels>> = None;
    for bounds in child_bounds {
        region = match region {
            Some(acc) => Some(acc.union(bounds)),
            None => Some(*bounds),
        };
    }
    region
}

impl Render for BarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::config::BarStyle;

        let style = self.config.bar.style;
        let side = matches!(
            self.config.bar.position,
            BarPosition::Left | BarPosition::Right
        );
        let opacity = if style == BarStyle::Hug {
            1.0
        } else {
            self.config.bar.opacity.clamp(0.0, 1.0)
        };
        let compositor_stale = !ShellSurfaces::compositor_snapshot(cx).connection.is_ready();
        let bg_color = cx
            .theme()
            .surface_container_high
            .opacity(if compositor_stale {
                opacity * 0.65
            } else {
                opacity
            });
        let widgets = self.config.bar.widgets.clone();
        let is_floating = style == BarStyle::Float;
        let start = self.build_section("start", &widgets.start, side, is_floating, window, cx);
        let center = self.build_section("center", &widgets.center, side, is_floating, window, cx);
        let end = self.build_section("end", &widgets.end, side, is_floating, window, cx);

        let bar_container = div()
            .when(side, |this| {
                this.w(px(self.config.bar.height as f32))
                    .h_full()
                    .flex_col()
            })
            .when(!side, |this| {
                this.w_full()
                    .h(px(self.config.bar.height as f32))
                    .flex_row()
            })
            .flex()
            .items_center()
            .justify_between()
            .bg(bg_color)
            .child(start)
            .child(center)
            .child(end);

        let set_bar_input_region = move |child_bounds: Vec<gpui::Bounds<gpui::Pixels>>,
                                         window: &mut Window,
                                         _: &mut App| {
            if let Some(region) = compute_bar_input_region(&child_bounds) {
                window.set_input_region(Some(&[region]));
            } else {
                window.set_input_region(Some(&[]));
            }
        };

        match style {
            BarStyle::Hug => {
                let hug_corners = self.render_hug_corners(
                    self.config.bar.position,
                    px(HUG_CORNER_RADIUS),
                    bg_color,
                );
                let main_bar = bar_container;
                let aligned_bar = match self.config.bar.position {
                    BarPosition::Top => div().absolute().top_0().w_full().child(main_bar),
                    BarPosition::Bottom => div().absolute().bottom_0().w_full().child(main_bar),
                    BarPosition::Left => div().absolute().left_0().h_full().child(main_bar),
                    BarPosition::Right => div().absolute().right_0().h_full().child(main_bar),
                }
                .on_children_prepainted(set_bar_input_region);

                div()
                    .relative()
                    .size_full()
                    .child(aligned_bar)
                    .child(hug_corners)
                    .into_any_element()
            }
            BarStyle::Float => div()
                .relative()
                .size_full()
                .on_children_prepainted(set_bar_input_region)
                .child(
                    bar_container
                        .px(px(self.config.bar.margin.horizontal as f32))
                        .py(px(self.config.bar.margin.vertical as f32))
                        .rounded_2xl()
                        .shadow_md(),
                )
                .into_any_element(),
            BarStyle::Rect => div()
                .relative()
                .size_full()
                .on_children_prepainted(set_bar_input_region)
                .child(bar_container.rounded_none().shadow_sm())
                .into_any_element(),
        }
    }
}

pub(crate) fn notification_timeout(notification: &Notification) -> Option<Duration> {
    (notification.expire_timeout_ms > 0)
        .then(|| Duration::from_millis(notification.expire_timeout_ms as u64))
}

#[cfg(test)]
mod notification_tests {
    use super::*;

    #[test]
    fn zero_timeout_never_schedules_expiry() {
        let mut notification = Notification::new("Critical", "Keep visible");
        notification.expire_timeout_ms = 0;

        assert_eq!(notification_timeout(&notification), None);
    }
}

#[cfg(test)]
mod hug_corner_tests {
    use gpui::point;

    use super::*;

    #[test]
    fn fills_the_complete_quarter_circle_bounds() {
        let radius = HUG_CORNER_RADIUS;
        let control_offset = radius * 0.552_284_8;
        let path = build_hug_corner(
            point(px(0.0), px(0.0)),
            point(px(0.0), px(radius)),
            point(px(radius), px(0.0)),
            point(px(0.0), px(radius - control_offset)),
            point(px(radius - control_offset), px(0.0)),
        )
        .expect("valid Hug corner path");

        assert_eq!(path.bounds.origin, point(px(0.0), px(0.0)));
        assert_eq!(path.bounds.size.width, px(radius));
        assert_eq!(path.bounds.size.height, px(radius));
    }
}

#[cfg(test)]
mod bar_input_region_tests {
    use gpui::{Bounds, point, px, size};

    use crate::shell::bar::view::compute_bar_input_region;

    #[test]
    fn calculates_union_of_child_bounds_for_input_region() {
        let child1 = Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(48.0)));
        let child2 = Bounds::new(point(px(200.0), px(0.0)), size(px(100.0), px(48.0)));

        let region = compute_bar_input_region(&[child1, child2]).unwrap();
        assert_eq!(region.origin, point(px(0.0), px(0.0)));
        assert_eq!(region.size, size(px(300.0), px(48.0)));
    }

    #[test]
    fn returns_none_when_child_bounds_empty() {
        assert_eq!(compute_bar_input_region(&[]), None);
    }
}

#[cfg(test)]
mod bar_view_tests {
    use super::*;

    /// Builds a `BarView` with a live command channel so the sender is genuinely connected.
    fn test_view(config: ShellConfig) -> (BarView, service_worker::CommandReceiver) {
        let (_, _, commands_tx, commands_rx) = service_worker::channels();
        (BarView::new_with_commands(config, commands_tx), commands_rx)
    }

    #[test]
    fn test_bar_view_default_construction() {
        let (view, _commands_rx) = test_view(ShellConfig::default());
        assert_eq!(view.app_id, "shilpo.shell");
        assert_eq!(view.active_title, "Shilpo Shell");
        assert_eq!(view.cpu_percent, 0);
        assert_eq!(view.ram_percent, 0);
        assert_eq!(view.cat_frame_index, 0);
        assert_eq!(view.media_info, None);
        assert_eq!(view.last_error, None);
        assert!(!view.is_stale());
    }

    #[test]
    fn test_apply_worker_update_battery() {
        let (mut view, _commands_rx) = test_view(ShellConfig::default());
        let battery = BatteryInfo {
            is_present: true,
            percentage: 85,
            ..Default::default()
        };

        let result = view.compute_domain_update(
            shilpo_services::DeviceDomain::Battery,
            &shilpo_services::DomainState {
                domain: shilpo_services::DeviceDomain::Battery,
                version: shilpo_services::DomainVersion::new(1, 1),
                lifecycle: shilpo_services::DomainLifecycle::Ready,
                payload: shilpo_services::DomainPayload::Battery(battery.clone()),
                error: None,
            },
        );
        assert!(result.changed);
        assert_eq!(view.battery.percentage, 85);
        assert!(result.effects.is_empty());
    }

    #[test]
    fn test_apply_worker_update_audio_triggers_osd() {
        let (mut view, _commands_rx) = test_view(ShellConfig::default());
        view.audio = AudioInfo {
            available: true,
            volume: 50,
            is_muted: false,
            ..Default::default()
        };

        let new_audio = shilpo_services::AudioPayload {
            available: true,
            volume: 70,
            is_muted: false,
            default_sink_name: view.audio.default_sink_name.clone(),
            default_source_name: view.audio.default_source_name.clone(),
            input_volume: view.audio.input_volume,
            is_input_muted: view.audio.is_input_muted,
            sinks: Vec::new(),
            sources: Vec::new(),
            app_streams: Vec::new(),
        };

        let result = view.compute_domain_update(
            shilpo_services::DeviceDomain::Audio,
            &shilpo_services::DomainState {
                domain: shilpo_services::DeviceDomain::Audio,
                version: shilpo_services::DomainVersion::new(1, 1),
                lifecycle: shilpo_services::DomainLifecycle::Ready,
                payload: shilpo_services::DomainPayload::Audio(new_audio),
                error: None,
            },
        );
        assert!(result.changed);
        assert_eq!(view.audio.volume, 70);
        assert_eq!(
            result.effects,
            vec![BarViewEffect::ShowOsd(OsdKind::Volume {
                level: 70,
                muted: false,
            })]
        );
    }

    #[test]
    fn test_apply_worker_update_network() {
        let (mut view, _commands_rx) = test_view(ShellConfig::default());
        let net = shilpo_services::NetworkPayload {
            available: true,
            is_connected: true,
            ssid: "WiFi-Home".into(),
            connection_type: "wifi".into(),
            wifi_enabled: true,
            wwan_enabled: false,
            airplane_mode: false,
            state: "ConnectedGlobal".into(),
            access_points: Vec::new(),
            active_vpns: Vec::new(),
            devices: Vec::new(),
            has_ip_config: false,
            ip_config: Default::default(),
        };

        let result = view.compute_domain_update(
            shilpo_services::DeviceDomain::Network,
            &shilpo_services::DomainState {
                domain: shilpo_services::DeviceDomain::Network,
                version: shilpo_services::DomainVersion::new(1, 1),
                lifecycle: shilpo_services::DomainLifecycle::Ready,
                payload: shilpo_services::DomainPayload::Network(net),
                error: None,
            },
        );
        assert!(result.changed);
        assert_eq!(view.network.ssid.as_deref(), Some("WiFi-Home"));
    }

    #[test]
    fn test_apply_worker_update_media() {
        let (mut view, _commands_rx) = test_view(ShellConfig::default());
        let media = shilpo_services::MediaPayload {
            available: true,
            player_id: "spotify".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            art_url: "".into(),
            playback_state: "playing".into(),
            can_play_pause: true,
            can_go_next: true,
            position_secs: 0.0,
            length_secs: 180.0,
            rate: 1.0,
        };

        let result = view.compute_domain_update(
            shilpo_services::DeviceDomain::Media,
            &shilpo_services::DomainState {
                domain: shilpo_services::DeviceDomain::Media,
                version: shilpo_services::DomainVersion::new(1, 1),
                lifecycle: shilpo_services::DomainLifecycle::Ready,
                payload: shilpo_services::DomainPayload::Media(media.clone()),
                error: None,
            },
        );
        assert!(result.changed);
        assert_eq!(view.media_info, Some(service_worker::media_info(media)));
    }

    #[test]
    fn test_apply_worker_update_config_loaded() {
        let (mut view, _commands_rx) = test_view(ShellConfig::default());
        let new_config = ShellConfig {
            clock_format: Some("%H:%M".into()),
            ..Default::default()
        };

        let result =
            view.compute_config_loaded(&new_config, &crate::config::ConfigChangeSet::all());
        assert!(result.changed);
        assert_eq!(view.config.clock_format.as_deref(), Some("%H:%M"));
        assert_eq!(
            result.effects,
            vec![BarViewEffect::ApplyConfigTheme(new_config)]
        );
    }

    #[test]
    fn test_apply_worker_update_config_failed() {
        let (mut view, _commands_rx) = test_view(ShellConfig::default());
        let err_msg = "Invalid TOML syntax";

        let result = view.compute_config_failed(err_msg);
        assert!(result.changed);
        assert_eq!(view.last_error, Some(err_msg.to_string()));
        assert_eq!(result.effects.len(), 1);
        if let BarViewEffect::ShowNotificationToast(notif) = &result.effects[0] {
            assert_eq!(notif.summary, "Configuration Warning");
            assert_eq!(notif.body, err_msg);
        } else {
            panic!("expected ShowNotificationToast effect");
        }
    }
}
