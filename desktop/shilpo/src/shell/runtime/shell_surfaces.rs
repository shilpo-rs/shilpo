use std::sync::Mutex;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::{
    App, AppContext, Bounds, DisplayId, Entity, Pixels, Point, Size, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, point, px, size,
};
use shilpo_ext_api::{CanonicalId, ExtensionId};
use shilpo_services::capture::{CaptureIntent, capture_frame, frame_to_rgba};
use shilpo_services::{
    BarState, CompositorAdapter, CompositorCommandBroker, CompositorOutput, CompositorSnapshot,
};
use shilpo_theme_daemon::DaemonState;
use uuid::Uuid;

use super::{ShellRuntime, WallpaperPreviewResource};
use crate::bar::cards::adapter::CardCoordinator;
use crate::bar::cards::model::CardRequest;
use crate::{
    actions::ActionInvocation,
    bar::{BarSpec, BarView, OutputDescriptor, ReconciliationOp, geometry::BarGeometry},
    error::ShellError,
    extensions::ContributionInstance,
    overview::{OverviewCloseReason, WorkspaceOverview},
};

#[derive(Clone, Copy)]
pub(crate) struct OverviewLifecycleCallback {
    instance: u64,
}

impl OverviewLifecycleCallback {
    pub(crate) fn finish(self, cx: &mut App, reason: OverviewCloseReason) {
        ShellSurfaces::finish_overview_close(cx, self.instance, reason);
    }

    pub(crate) fn window_closed(self, cx: &mut App) {
        ShellSurfaces::forget_overview(cx, self.instance);
    }

    pub(crate) fn entity_ready(self, cx: &mut App, entity: Entity<WorkspaceOverview>) {
        if cx
            .global::<ShellRuntime>()
            .shell_surfaces()
            .overview_instance
            == self.instance
        {
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .overview_entity = Some(entity);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NotificationLifecycleCallback {
    generation: u64,
}

impl std::fmt::Display for NotificationLifecycleCallback {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.generation.fmt(formatter)
    }
}

impl NotificationLifecycleCallback {
    pub(crate) fn forgotten(self, cx: &mut App) {
        ShellSurfaces::forget_notification(cx, self.generation);
    }

    pub(crate) fn expired(self, cx: &mut App) {
        ShellSurfaces::expire_notification(cx, self.generation);
    }
}

pub(super) fn bar_window_options(
    geometry: &BarGeometry,
    with_display_geometry: bool,
) -> WindowOptions {
    let bounds = if with_display_geometry {
        geometry.bounds
    } else {
        Bounds::new(point(px(0.), px(0.)), geometry.bounds.size)
    };
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: with_display_geometry.then_some(geometry.display_id),
        app_id: Some("shilpo-bar".to_string()),
        window_background: WindowBackgroundAppearance::Transparent,
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: "bar".to_string(),
            layer: Layer::Top,
            anchor: geometry.anchor,
            exclusive_zone: Some(geometry.exclusive_zone),
            exclusive_edge: Some(geometry.exclusive_edge),
            margin: geometry.margin,
            keyboard_interactivity: KeyboardInteractivity::None,
        }),
        ..Default::default()
    }
}

fn overlay_options(
    app_id: &str,
    namespace: &str,
    window_size: Size<Pixels>,
    origin: Point<Pixels>,
    display_id: Option<DisplayId>,
) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin,
            size: window_size,
        })),
        display_id,
        app_id: Some(app_id.to_string()),
        window_background: WindowBackgroundAppearance::Transparent,
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: namespace.to_string(),
            layer: Layer::Top,
            anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn parse_awww_wallpaper_path(output: &str) -> Option<PathBuf> {
    const MARKER: &str = "currently displaying: image: ";
    output.lines().find_map(|line| {
        let path = line.split_once(MARKER)?.1.trim();
        (!path.is_empty()).then(|| PathBuf::from(path))
    })
}

pub(crate) fn query_awww_wallpaper_path() -> Option<PathBuf> {
    let output = std::process::Command::new("awww")
        .arg("query")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = parse_awww_wallpaper_path(&String::from_utf8_lossy(&output.stdout))?;
    path.is_file().then_some(path)
}

pub(super) fn attach_compositor_stream(
    compositor_broker_slot: &Arc<Mutex<Option<Arc<CompositorCommandBroker>>>>,
    compositor: &Arc<dyn CompositorAdapter>,
) -> Arc<CompositorSnapshot> {
    *compositor_broker_slot.lock().unwrap() = Some(compositor.command_broker());
    compositor.current()
}

pub(super) fn spawn_compositor_stream_loop(cx: &mut App, compositor: &Arc<dyn CompositorAdapter>) {
    let mut rx = compositor.subscribe();
    cx.spawn(async move |cx| {
        while rx.changed().await.is_ok() {
            let snapshot = rx.borrow().clone();
            cx.update(|cx: &mut gpui::App| {
                ShellRuntime::on_compositor_snapshot_changed(cx, snapshot);
            });
        }
    })
    .detach();
}

fn discovered_wallpaper_needs_theme_sync(
    theme_wallpaper_path: Option<&Path>,
    discovered_wallpaper_path: &Path,
) -> bool {
    theme_wallpaper_path != Some(discovered_wallpaper_path)
}

/// Revalidate the prepared image on every successful compositor wallpaper
/// probe, even when the pathname is unchanged. The returned flag controls the
/// separate concern of synchronizing a changed path to the theme daemon.
fn reconcile_discovered_wallpaper(
    resource: &Entity<WallpaperPreviewResource>,
    discovered_wallpaper_path: PathBuf,
    cx: &mut App,
) -> bool {
    let needs_theme_sync =
        discovered_wallpaper_needs_theme_sync(resource.read(cx).path(), &discovered_wallpaper_path);
    resource.update(cx, |resource, cx| {
        resource.set_wallpaper_path(Some(discovered_wallpaper_path), cx);
    });
    needs_theme_sync
}

fn should_restore_overview_prior_focus(
    reason: OverviewCloseReason,
    opened_workspace_id: Option<u64>,
    current_workspace_id: Option<u64>,
) -> bool {
    if reason != OverviewCloseReason::Cancel {
        return false;
    }

    match (opened_workspace_id, current_workspace_id) {
        (Some(opened), Some(current)) => opened == current,
        _ => true,
    }
}

fn remove_window_after_frame_drain<V: 'static>(
    handle: WindowHandle<V>,
    cx: &mut App,
) -> anyhow::Result<()> {
    handle.update(cx, move |_, window, _| {
        window.on_next_frame(move |_, cx| {
            cx.spawn(async move |cx| {
                let result =
                    cx.update(|cx| handle.update(cx, |_, window, _| window.remove_window()));
                if let Err(error) = result {
                    tracing::warn!(
                        ?error,
                        window_id = ?handle.window_id(),
                        "overview window disappeared before deferred teardown"
                    );
                }
            })
            .detach();
        });
        window.refresh();
    })?;
    Ok(())
}

/// Describes a desktop surface contributed by an extension.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionSurfaceSpec {
    pub contribution: CanonicalId,
    pub display_id: DisplayId,
    pub bounds: Bounds<Pixels>,
}

/// Merges the global extension settings with a per-instance override map.
pub(crate) fn extension_settings(
    config: &crate::config::ShellConfig,
    extension_id: &ExtensionId,
    instance_settings: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut settings = config
        .extensions
        .settings
        .get(extension_id.as_str())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let (Some(base), Some(overrides)) = (
        settings.as_object_mut(),
        instance_settings.and_then(serde_json::Value::as_object),
    ) {
        base.extend(overrides.clone());
    }
    settings
}

/// Describes an active desktop surface managed by the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveSurfaceKind {
    Bar,
    Overview,
    NotificationToast,
    Osd,
    ExtensionSurface(String),
    ExtensionPanel(CanonicalId),
    Capture,
    CardBandPersistent(DisplayId),
    CardBandPreview(DisplayId),
}

/// Semantic requests accepted by the shell surface owner. Callers do not
/// learn GPUI handles, geometry, focus handles, or transition generations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceRequest {
    ToggleBars,
    ToggleOverview,
    OpenOverviewOnDisplay(Option<DisplayId>),
    CloseOverview,
    SyncDisplays,
    OpenFallbackBar,
    ShowOsd(crate::osd::OsdKind),
    ShowNotification(shilpo_services::Notification),
    OpenCapture(CaptureIntent),
    OpenExtensionPanel(CanonicalId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OverviewLifecycle {
    #[default]
    Closed,
    Opening {
        generation: u64,
    },
    Open {
        generation: u64,
    },
    Closing {
        generation: u64,
    },
}

/// Lifecycle for transient shell-owned surfaces whose windows may be replaced
/// or dismissed asynchronously.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SurfaceLifecycle {
    #[default]
    Closed,
    Opening {
        generation: u64,
    },
    Open {
        generation: u64,
    },
    Closing {
        generation: u64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceSnapshot {
    pub overview_open: bool,
    pub overview_lifecycle: OverviewLifecycle,
    pub bars_open: bool,
    pub readiness: shilpo_services::ReadinessState,
    pub notification_lifecycle: SurfaceLifecycle,
    pub osd_lifecycle: SurfaceLifecycle,
    pub polkit_lifecycle: SurfaceLifecycle,
    pub idle_grace_lifecycle: SurfaceLifecycle,
    pub extension_surface_count: usize,
    pub extension_lifecycle: SurfaceLifecycle,
    pub capture_lifecycle: SurfaceLifecycle,
    /// Lifecycle of the persistent (click) card channel.
    pub persistent_card_lifecycle: SurfaceLifecycle,
    /// Lifecycle of the preview (hover) card channel.
    pub preview_card_lifecycle: SurfaceLifecycle,
    /// Whether the card coordinator is holding bar visibility.
    pub card_visibility_hold: bool,
}

/// The set of shell windows that must be torn down on shutdown.
pub(crate) struct ShutdownWindows {
    pub(crate) bars: HashMap<DisplayId, (WindowHandle<BarView>, BarSpec)>,
    pub(crate) extension_surfaces:
        HashMap<String, (WindowHandle<shilpo_m3e::Root>, ExtensionSurfaceSpec)>,
    pub(crate) extension_panel: Option<(WindowHandle<shilpo_m3e::Root>, CanonicalId)>,
    pub(crate) notification: Option<(
        u64,
        u32,
        WindowHandle<crate::notification::NotificationToastView>,
    )>,
    pub(crate) polkit: Option<(
        u64,
        WindowHandle<shilpo_m3e::Root>,
        Entity<crate::polkit::PolkitDialogView>,
    )>,
    pub(crate) idle_grace: Option<(
        u64,
        WindowHandle<shilpo_m3e::Root>,
        Entity<crate::shell::idle::IdleGraceOverlayView>,
    )>,
    pub(crate) capture: Option<(u64, WindowHandle<shilpo_m3e::Root>)>,
}

/// What closed when a shell window was destroyed.
pub(crate) enum WindowClosedOutcome {
    Nothing,
    Capture,
    ExtensionPanel(CanonicalId),
}

/// Owns every shell window: the per-display bars, the overview, the
/// notification toast, the on-screen display (OSD), and any open extension
/// views.
///
/// `ShellSurfaces` is the exclusive authority on window creation, geometry,
/// anchor, and destruction. All changes flow through the typed `SurfaceRequest`
/// interface.
pub struct ShellSurfaces {
    /// Two-channel card coordinator (hover + persistent).
    pub(crate) card_coordinator: CardCoordinator,
    bars: HashMap<DisplayId, (WindowHandle<BarView>, BarSpec)>,
    last_bar_specs: Vec<(BarGeometry, bool)>,
    bar_state: BarState,
    overview: Option<WindowHandle<shilpo_m3e::Root>>,
    overview_lifecycle: OverviewLifecycle,
    overview_entity: Option<Entity<WorkspaceOverview>>,
    overview_instance: u64,
    next_overview_instance: u64,
    overview_opened_workspace_id: Option<u64>,
    latest_snapshot: Arc<CompositorSnapshot>,
    notification: Option<(
        u64,
        u32,
        WindowHandle<crate::notification::NotificationToastView>,
    )>,
    notification_generation: u64,
    notification_lifecycle: SurfaceLifecycle,
    prior_window_id: Option<u64>,
    osd: Option<(
        u64,
        WindowHandle<shilpo_m3e::Root>,
        Entity<crate::osd::OsdView>,
    )>,
    osd_generation: u64,
    osd_lifecycle: SurfaceLifecycle,
    polkit: Option<(
        u64,
        WindowHandle<shilpo_m3e::Root>,
        Entity<crate::polkit::PolkitDialogView>,
    )>,
    polkit_generation: u64,
    polkit_lifecycle: SurfaceLifecycle,
    idle_grace: Option<(
        u64,
        WindowHandle<shilpo_m3e::Root>,
        Entity<crate::shell::idle::IdleGraceOverlayView>,
    )>,
    idle_grace_generation: u64,
    idle_grace_lifecycle: SurfaceLifecycle,
    extension_lifecycle: SurfaceLifecycle,
    capture: Option<(u64, WindowHandle<shilpo_m3e::Root>)>,
    capture_generation: u64,
    capture_lifecycle: SurfaceLifecycle,
    extension_surfaces: HashMap<String, (WindowHandle<shilpo_m3e::Root>, ExtensionSurfaceSpec)>,
    extension_panel: Option<(WindowHandle<shilpo_m3e::Root>, CanonicalId)>,
    extension_output_ids: HashSet<DisplayId>,
    readiness: shilpo_services::ReadinessState,
}

impl ShellSurfaces {
    pub fn is_overview_open(cx: &App) -> bool {
        cx.has_global::<ShellRuntime>()
            && cx
                .global::<ShellRuntime>()
                .shell_surfaces()
                .overview_is_open()
    }

    pub fn has_bars(cx: &App) -> bool {
        cx.has_global::<ShellRuntime>()
            && cx.global::<ShellRuntime>().shell_surfaces().bars_are_open()
    }

    pub fn compositor_snapshot(cx: &App) -> Arc<CompositorSnapshot> {
        if cx.has_global::<ShellRuntime>() {
            cx.global::<ShellRuntime>()
                .shell_surfaces()
                .latest_snapshot()
        } else {
            Arc::new(CompositorSnapshot::default())
        }
    }

    pub fn request(cx: &mut App, request: SurfaceRequest) {
        match request {
            SurfaceRequest::ToggleBars => Self::toggle_bar(cx),
            SurfaceRequest::ToggleOverview => Self::toggle_overview(cx),
            SurfaceRequest::OpenOverviewOnDisplay(display_id) => {
                Self::open_or_focus_overview_on_display(cx, display_id)
            }
            SurfaceRequest::CloseOverview => Self::close_overview(cx),
            SurfaceRequest::SyncDisplays => Self::sync_displays(cx),
            SurfaceRequest::OpenFallbackBar => Self::open_fallback_bar(cx),
            SurfaceRequest::ShowOsd(kind) => Self::show_osd(cx, kind),
            SurfaceRequest::ShowNotification(notification) => {
                Self::show_notification(cx, notification)
            }
            SurfaceRequest::OpenCapture(intent) => Self::open_capture(cx, intent),
            SurfaceRequest::OpenExtensionPanel(contribution) => {
                Self::open_extension_panel(cx, contribution)
            }
        }
    }

    pub(crate) fn open_capture(cx: &mut App, intent: CaptureIntent) {
        // A second press of the screenshot shortcut while a selection overlay is already open
        // means "I changed my mind" -- cancel it, the same as pressing Escape, instead of
        // capturing a fresh frame (which briefly stacked a second overlay on top of the first).
        if let Some((_, handle)) = cx.global::<ShellRuntime>().shell_surfaces().capture {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return;
        }
        Self::close_overview(cx);
        Self::close_overview_competitors(cx);
        Self::capture_prior_focus(cx);
        let generation = {
            let surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            surfaces.capture_generation = surfaces.capture_generation.wrapping_add(1);
            surfaces.capture_lifecycle = SurfaceLifecycle::Opening {
                generation: surfaces.capture_generation,
            };
            surfaces.capture_generation
        };
        let config = ShellRuntime::active_config(cx).capture;
        let frame = match capture_frame(None).and_then(|frame| frame_to_rgba(&frame)) {
            Ok(frame) => frame,
            Err(error) => {
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .capture_lifecycle = SurfaceLifecycle::Closed;
                Self::restore_prior_focus(cx);
                tracing::warn!(%error, "failed to prepare screenshot overlay");
                return;
            }
        };
        // Without an explicit `window_bounds`, GPUI falls back to `default_bounds`, which
        // cascades from whatever window is currently active (offsetting by 25px) instead of
        // sizing to the output. On a single press that under-sizes the overlay (skips the real
        // output size entirely); on a double press it cascades off the still-closing previous
        // overlay, producing a visible offset "duplicate". Pin it to the corrected, scale-aware
        // output bounds (see `output_bounds`) so it always covers the full screen exactly once.
        let (display_bounds, display_id) = match cx.primary_display() {
            Some(display) => {
                let snapshot = Self::compositor_snapshot(cx);
                let output_name = Self::output_name_for_display(&*display, &snapshot.outputs);
                let bounds = Self::output_bounds(
                    output_name.as_deref(),
                    &snapshot.outputs,
                    display.bounds(),
                );
                (bounds, Some(display.id()))
            }
            None => (
                Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))),
                None,
            ),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(display_bounds)),
            display_id,
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "capture-overlay".to_string(),
                layer: Layer::Overlay,
                anchor: Anchor::all(),
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                // Anchoring all four edges makes the compositor stretch the surface to fill
                // the space *outside* other layers' exclusive zones -- the bar reserves a
                // strip along its edge, so without this the frozen frame left that strip
                // uncovered and the live bar showed through underneath. -1 tells the
                // compositor to ignore other surfaces' exclusive zones and cover the output.
                exclusive_zone: Some(px(-1.0)),
                ..Default::default()
            }),
            ..Default::default()
        };
        match cx.open_window(options, move |window, cx| {
            crate::capture::CaptureOverlayView::view(frame, intent, config, window, cx)
        }) {
            Ok(handle) => {
                let surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
                surfaces.capture = Some((generation, handle));
                surfaces.capture_lifecycle = SurfaceLifecycle::Open { generation };
            }
            Err(error) => {
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .capture_lifecycle = SurfaceLifecycle::Closed;
                Self::restore_prior_focus(cx);
                tracing::warn!(%error, "failed to open capture overlay");
            }
        }
    }

    fn show_notification(cx: &mut App, notification: shilpo_services::Notification) {
        use crate::config::BarPosition;

        let timeout = crate::bar::view::notification_timeout(&notification);
        let notification_id = notification.id;
        let bar_config = ShellRuntime::active_config(cx).bar;
        let bar_position = bar_config.position;
        let bar_h = bar_config.height as f32;
        let is_float = bar_config.style == crate::config::BarStyle::Float;
        let float_margin_h = if is_float {
            bar_config.margin.horizontal as f32
        } else {
            0.
        };
        let float_margin_v = if is_float {
            bar_config.margin.vertical as f32
        } else {
            0.
        };
        let (display_bounds, display_id) = cx.primary_display().map_or_else(
            || {
                (
                    Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))),
                    None,
                )
            },
            |display| (display.bounds(), Some(display.id())),
        );
        let gap = px(8.);
        let window_size = size(
            px(376.),
            display_bounds.size.height - px(bar_h + float_margin_v) - gap - gap,
        );
        let (anchor, margin, origin) = match bar_position {
            BarPosition::Top => (
                Anchor::TOP | Anchor::RIGHT,
                Some((gap, gap, px(0.), px(0.))),
                point(
                    display_bounds.origin.x + display_bounds.size.width - window_size.width - gap,
                    display_bounds.origin.y + px(bar_h + float_margin_v) + gap,
                ),
            ),
            BarPosition::Bottom => (
                Anchor::BOTTOM | Anchor::RIGHT,
                Some((px(0.), gap, gap, px(0.))),
                point(
                    display_bounds.origin.x + display_bounds.size.width - window_size.width - gap,
                    display_bounds.origin.y + display_bounds.size.height
                        - window_size.height
                        - px(bar_h + float_margin_v)
                        - gap,
                ),
            ),
            BarPosition::Left => (
                Anchor::TOP | Anchor::LEFT,
                Some((gap, px(0.), px(0.), gap)),
                point(
                    display_bounds.origin.x + px(bar_h + float_margin_h) + gap,
                    display_bounds.origin.y + gap,
                ),
            ),
            BarPosition::Right => (
                Anchor::TOP | Anchor::RIGHT,
                Some((gap, gap, px(0.), px(0.))),
                point(
                    display_bounds.origin.x + display_bounds.size.width
                        - window_size.width
                        - px(bar_h + float_margin_h)
                        - gap,
                    display_bounds.origin.y + gap,
                ),
            ),
        };
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin,
                size: window_size,
            })),
            display_id,
            app_id: Some("shilpo-notification".to_string()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "notification".to_string(),
                layer: Layer::Overlay,
                anchor,
                margin,
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let generation = cx
            .global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .next_notification_generation();
        if let Some(handle) = cx
            .global::<ShellRuntime>()
            .shell_surfaces()
            .notification_handle()
            && handle
                .update(cx, |view, window, cx| {
                    view.push(
                        notification.clone(),
                        NotificationLifecycleCallback { generation },
                        timeout,
                        window,
                        cx,
                    );
                })
                .is_ok()
        {
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .set_notification(generation, notification_id, handle);
            return;
        }
        if let Ok(handle) = cx.open_window(options, move |window, cx| {
            crate::notification::NotificationToastView::view(
                notification.clone(),
                NotificationLifecycleCallback { generation },
                timeout,
                bar_position,
                window,
                cx,
            )
        }) {
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .set_notification(generation, notification_id, handle);
        }
    }

    pub fn snapshot(cx: &App) -> SurfaceSnapshot {
        if cx.has_global::<ShellRuntime>() {
            let surfaces = cx.global::<ShellRuntime>().shell_surfaces();
            SurfaceSnapshot {
                overview_open: surfaces.overview_is_open(),
                overview_lifecycle: surfaces.overview_lifecycle,
                bars_open: surfaces.bars_are_open(),
                readiness: surfaces.readiness(),
                notification_lifecycle: surfaces.notification_lifecycle,
                osd_lifecycle: surfaces.osd_lifecycle,
                polkit_lifecycle: surfaces.polkit_lifecycle,
                idle_grace_lifecycle: surfaces.idle_grace_lifecycle,
                extension_surface_count: surfaces.extension_surfaces.len(),
                extension_lifecycle: surfaces.extension_lifecycle,
                capture_lifecycle: surfaces.capture_lifecycle,
                persistent_card_lifecycle: CardCoordinator::persistent_lifecycle(cx),
                preview_card_lifecycle: CardCoordinator::preview_lifecycle(cx),
                card_visibility_hold: CardCoordinator::holds_bar_visibility(cx),
            }
        } else {
            SurfaceSnapshot::default()
        }
    }

    pub fn new(latest_snapshot: Arc<CompositorSnapshot>) -> Self {
        let mut manager = Self {
            card_coordinator: CardCoordinator::default(),
            bars: HashMap::new(),
            last_bar_specs: Vec::new(),
            bar_state: BarState::Starting,
            overview: None,
            overview_lifecycle: OverviewLifecycle::Closed,
            overview_entity: None,
            overview_instance: 0,
            next_overview_instance: 0,
            overview_opened_workspace_id: None,
            latest_snapshot,
            notification: None,
            notification_generation: 0,
            notification_lifecycle: SurfaceLifecycle::Closed,
            prior_window_id: None,
            osd: None,
            osd_generation: 0,
            osd_lifecycle: SurfaceLifecycle::Closed,
            polkit: None,
            polkit_generation: 0,
            polkit_lifecycle: SurfaceLifecycle::Closed,
            idle_grace: None,
            idle_grace_generation: 0,
            idle_grace_lifecycle: SurfaceLifecycle::Closed,
            extension_lifecycle: SurfaceLifecycle::Closed,
            capture: None,
            capture_generation: 0,
            capture_lifecycle: SurfaceLifecycle::Closed,
            extension_surfaces: HashMap::new(),
            extension_panel: None,
            extension_output_ids: HashSet::new(),
            readiness: shilpo_services::ReadinessState::Starting,
        };
        let battery_provider =
            std::sync::Arc::new(crate::bar::cards::battery_card::BatteryCardProvider::new());
        manager
            .card_coordinator
            .register_provider_direct(battery_provider);
        let workspace_provider =
            std::sync::Arc::new(crate::bar::cards::workspace_card::WorkspacePreviewProvider::new());
        manager
            .card_coordinator
            .register_provider_direct(workspace_provider);
        manager.update_readiness();
        manager
    }

    pub fn active_surfaces(&self) -> Vec<ActiveSurfaceKind> {
        let mut surfaces = Vec::new();
        if self.bars_are_open() {
            surfaces.push(ActiveSurfaceKind::Bar);
        }
        if self.overview_is_open() {
            surfaces.push(ActiveSurfaceKind::Overview);
        }
        if self.notification.is_some() {
            surfaces.push(ActiveSurfaceKind::NotificationToast);
        }
        if self.osd.is_some() {
            surfaces.push(ActiveSurfaceKind::Osd);
        }
        for instance_id in self.extension_surfaces.keys() {
            surfaces.push(ActiveSurfaceKind::ExtensionSurface(instance_id.clone()));
        }
        if let Some((_, canonical_id)) = &self.extension_panel {
            surfaces.push(ActiveSurfaceKind::ExtensionPanel(canonical_id.clone()));
        }
        if self.capture.is_some() {
            surfaces.push(ActiveSurfaceKind::Capture);
        }
        surfaces.extend(
            self.card_coordinator
                .persistent_band_displays()
                .map(ActiveSurfaceKind::CardBandPersistent),
        );
        surfaces.extend(
            self.card_coordinator
                .preview_band_displays()
                .map(ActiveSurfaceKind::CardBandPreview),
        );
        surfaces
    }

    pub(crate) fn latest_snapshot(&self) -> Arc<CompositorSnapshot> {
        self.latest_snapshot.clone()
    }

    pub(crate) fn set_latest_snapshot(&mut self, snapshot: Arc<CompositorSnapshot>) {
        self.latest_snapshot = snapshot;
    }

    #[allow(dead_code)]
    pub(crate) fn bar_state(&self) -> BarState {
        self.bar_state.clone()
    }

    pub(crate) fn set_bar_state(&mut self, state: BarState) {
        self.bar_state = state;
        self.update_readiness();
    }

    pub(crate) fn readiness(&self) -> shilpo_services::ReadinessState {
        self.readiness
    }

    pub(crate) fn update_readiness(&mut self) {
        self.readiness = readiness_for(&self.latest_snapshot.connection, &self.bar_state);
    }

    pub(crate) fn store_extension_surface(
        &mut self,
        instance_id: String,
        handle: WindowHandle<shilpo_m3e::Root>,
        spec: ExtensionSurfaceSpec,
    ) {
        self.extension_surfaces.insert(instance_id, (handle, spec));
        self.extension_lifecycle = SurfaceLifecycle::Open {
            generation: self.extension_surfaces.len() as u64,
        };
    }

    pub(crate) fn remove_extension_surface(
        &mut self,
        id: &str,
    ) -> Option<(WindowHandle<shilpo_m3e::Root>, ExtensionSurfaceSpec)> {
        let removed = self.extension_surfaces.remove(id);
        if self.extension_surfaces.is_empty() {
            self.extension_lifecycle = SurfaceLifecycle::Closed;
        }
        removed
    }

    pub(crate) fn stale_extension_surface_ids(
        &self,
        desired: &HashMap<String, ExtensionSurfaceSpec>,
    ) -> Vec<String> {
        self.extension_surfaces
            .iter()
            .filter(|(id, (_, current))| desired.get(*id) != Some(current))
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(crate) fn has_extension_surface(&self, instance_id: &str) -> bool {
        self.extension_surfaces.contains_key(instance_id)
    }

    pub(crate) fn bar_handles(&self) -> Vec<WindowHandle<BarView>> {
        self.bars.values().map(|(handle, _)| *handle).collect()
    }

    pub(crate) fn overview_is_open(&self) -> bool {
        self.overview.is_some()
    }

    pub(crate) fn bars_are_open(&self) -> bool {
        !self.bars.is_empty()
    }

    pub(crate) fn next_overview_instance(&mut self) -> u64 {
        self.next_overview_instance = self.next_overview_instance.wrapping_add(1);
        self.overview_instance = self.next_overview_instance;
        self.overview_instance
    }

    pub(crate) fn begin_overview_close(&mut self) -> Option<u64> {
        let OverviewLifecycle::Open { generation } = self.overview_lifecycle else {
            return None;
        };
        self.overview_lifecycle = OverviewLifecycle::Closing { generation };
        Some(generation)
    }

    pub(crate) fn next_notification_generation(&mut self) -> u64 {
        self.notification_generation = self.notification_generation.wrapping_add(1);
        self.notification_lifecycle = SurfaceLifecycle::Opening {
            generation: self.notification_generation,
        };
        self.notification_generation
    }

    pub(crate) fn set_notification(
        &mut self,
        generation: u64,
        notification_id: u32,
        handle: WindowHandle<crate::notification::NotificationToastView>,
    ) {
        self.notification = Some((generation, notification_id, handle));
        self.notification_lifecycle = SurfaceLifecycle::Open { generation };
        tracing::debug!(generation, notification_id, "registered notification toast");
    }

    pub(crate) fn notification_handle(
        &self,
    ) -> Option<WindowHandle<crate::notification::NotificationToastView>> {
        self.notification.as_ref().map(|(_, _, handle)| *handle)
    }

    /// Removes every surface belonging to a window that was just closed and
    /// reports which overlay was destroyed so the orchestrator can emit events.
    pub(crate) fn handle_window_closed(
        &mut self,
        window_id: gpui::WindowId,
    ) -> WindowClosedOutcome {
        let mut outcome = WindowClosedOutcome::Nothing;
        self.bars
            .retain(|_, (handle, _)| handle.window_id() != window_id);
        self.extension_surfaces
            .retain(|_, (handle, _)| handle.window_id() != window_id);
        if self.bars.is_empty() {
            self.bar_state = BarState::Hidden;
        }
        let panel = self.extension_panel.take();
        if let Some((handle, id)) = panel {
            if handle.window_id() == window_id {
                outcome = WindowClosedOutcome::ExtensionPanel(id);
            } else {
                self.extension_panel = Some((handle, id));
            }
        }
        if self
            .notification
            .as_ref()
            .is_some_and(|(_, _, handle)| handle.window_id() == window_id)
        {
            self.notification = None;
            self.notification_lifecycle = SurfaceLifecycle::Closed;
        }
        if self
            .capture
            .as_ref()
            .is_some_and(|(_, handle)| handle.window_id() == window_id)
        {
            self.capture = None;
            self.capture_lifecycle = SurfaceLifecycle::Closed;
            outcome = WindowClosedOutcome::Capture;
        }
        if self
            .osd
            .as_ref()
            .is_some_and(|(_, handle, _)| handle.window_id() == window_id)
        {
            self.osd = None;
            self.osd_lifecycle = SurfaceLifecycle::Closed;
        }
        if self
            .overview
            .as_ref()
            .is_some_and(|handle| handle.window_id() == window_id)
        {
            self.overview = None;
            self.overview_entity = None;
            self.overview_lifecycle = OverviewLifecycle::Closed;
        }
        self.card_coordinator.handle_window_closed(window_id);
        outcome
    }

    /// Collects every open window so the orchestrator can tear them down during shutdown.
    pub(crate) fn take_windows_for_shutdown(&mut self) -> ShutdownWindows {
        self.bar_state = BarState::Hidden;
        self.notification_lifecycle = SurfaceLifecycle::Closed;
        self.osd_lifecycle = SurfaceLifecycle::Closed;
        self.polkit_lifecycle = SurfaceLifecycle::Closed;
        self.idle_grace_lifecycle = SurfaceLifecycle::Closed;
        self.capture_lifecycle = SurfaceLifecycle::Closed;
        ShutdownWindows {
            bars: std::mem::take(&mut self.bars),
            extension_surfaces: std::mem::take(&mut self.extension_surfaces),
            extension_panel: self.extension_panel.take(),
            notification: self.notification.take(),
            polkit: self.polkit.take(),
            idle_grace: self.idle_grace.take(),
            capture: self.capture.take(),
        }
    }

    pub(crate) fn output_name_for_bounds(
        bounds: Bounds<Pixels>,
        compositor_outputs: &[CompositorOutput],
    ) -> Option<String> {
        let origin_x = bounds.origin.x.as_f32();
        let origin_y = bounds.origin.y.as_f32();
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let close = |left: f32, right: f32| (left - right).abs() <= 2.0;
        let geometry_match = |output: &CompositorOutput, size_only: bool| {
            let scale = output.scale.max(1.0) as f32;
            let position_matches = close(output.logical_position.0 as f32, origin_x)
                && close(output.logical_position.1 as f32, origin_y);
            let size_matches = (close(output.logical_size.0 as f32, width)
                && close(output.logical_size.1 as f32, height))
                || (close(output.logical_size.0 as f32 * scale, width)
                    && close(output.logical_size.1 as f32 * scale, height));
            size_matches && (size_only || position_matches)
        };

        compositor_outputs
            .iter()
            .find(|output| geometry_match(output, false))
            .or_else(|| {
                let size_matches = compositor_outputs
                    .iter()
                    .filter(|output| geometry_match(output, true))
                    .collect::<Vec<_>>();
                (size_matches.len() == 1).then(|| size_matches[0])
            })
            .map(|output| output.name.clone())
    }

    pub(crate) fn output_name_for_display(
        display: &dyn gpui::PlatformDisplay,
        compositor_outputs: &[CompositorOutput],
    ) -> Option<String> {
        let display_uuid = display.uuid().ok();
        display_uuid
            .and_then(|display_uuid| {
                compositor_outputs
                    .iter()
                    .find(|output| {
                        Uuid::new_v5(&Uuid::NAMESPACE_DNS, output.name.as_bytes()) == display_uuid
                    })
                    .map(|output| output.name.clone())
            })
            .or_else(|| Self::output_name_for_bounds(display.bounds(), compositor_outputs))
            .or_else(|| display_uuid.map(|uuid| uuid.to_string()))
    }

    /// Bounds sourced from Hyprland's own monitor data instead of `display.bounds()`/GPUI's
    /// own scale detection: GPUI's Wayland backend does not appear to support fractional
    /// `wl_output` scale (e.g. 1.5), and falls back to the nearest integer (2), reporting a
    /// logical size half the true one on a 1.5x output (a 1920px-wide output came back as
    /// 960px). `CompositorOutput.logical_size`/`logical_position` are actually physical
    /// (unscaled) Hyprland monitor coordinates, so the true logical bounds are those divided
    /// by the compositor's real reported scale.
    ///
    /// This intentionally does not also return a scale for `BarGeometry::calculate_with_scale`:
    /// that scale multiplies `config.height`/`margin` to convert already-physical bounds into
    /// the same unit space (see `reconciliation_scale_factor_adjustment`), but the bounds
    /// returned here are already in that corrected space -- multiplying again double-scaled
    /// the bar's thickness, which showed up as an oversized gap between the bar and windows.
    fn output_bounds(
        output_name: Option<&str>,
        compositor_outputs: &[CompositorOutput],
        fallback_bounds: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let Some(output) = output_name.and_then(|name| {
            compositor_outputs
                .iter()
                .find(|candidate| candidate.name == name)
        }) else {
            return fallback_bounds;
        };
        let scale = output.scale.max(0.1) as f32;
        Bounds::new(
            point(
                px(output.logical_position.0 as f32 / scale),
                px(output.logical_position.1 as f32 / scale),
            ),
            size(
                px(output.logical_size.0 as f32 / scale),
                px(output.logical_size.1 as f32 / scale),
            ),
        )
    }

    pub fn sync_displays(cx: &mut App) {
        if !cx.has_global::<ShellRuntime>() {
            return;
        }

        let snapshot = Self::compositor_snapshot(cx);

        let current_outputs = cx
            .displays()
            .into_iter()
            .enumerate()
            .map(|(index, display)| {
                let display_id = display.id();
                let output_name = Self::output_name_for_display(&*display, &snapshot.outputs);
                let is_primary = index == 0;
                let bounds = Self::output_bounds(
                    output_name.as_deref(),
                    &snapshot.outputs,
                    display.bounds(),
                );

                OutputDescriptor {
                    display_id,
                    bounds,
                    name: output_name,
                    is_primary,
                    scale: None,
                }
            })
            .collect::<Vec<_>>();

        let output_ids = current_outputs
            .iter()
            .map(|o| o.display_id)
            .collect::<HashSet<_>>();
        let changed = {
            let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            if output_ids != shell_surfaces.extension_output_ids {
                shell_surfaces.extension_output_ids = output_ids;
                true
            } else {
                false
            }
        };
        if changed {
            ShellRuntime::dispatch_extension_event(
                cx,
                shilpo_ext_api::ExtensionEvent::OutputsChanged,
            );
        }

        Self::reconcile_extension_surfaces(cx, &current_outputs);
        Self::reconcile_bars(cx);
    }

    pub(crate) fn open_bar_with_spec(cx: &mut App, spec: BarSpec) -> bool {
        let geometry = spec.geometry.clone();
        let display_id = spec.geometry.display_id;
        let with_display_geometry = spec.with_display_geometry;
        let options = bar_window_options(&geometry, with_display_geometry);
        let spec_for_view = spec.clone();
        let view_result = cx.open_window(options, move |window, cx| {
            BarView::view_with_spec(spec_for_view, window, cx)
        });

        match view_result {
            Ok(handle) => {
                let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
                shell_surfaces.bars.insert(display_id, (handle, spec));
                shell_surfaces.bar_state = BarState::Visible;
                ShellRuntime::publish_status(cx);
                true
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to open shell bar window on display {:?}",
                    display_id
                );
                false
            }
        }
    }

    fn open_fallback_bar(cx: &mut App) {
        let geometry = BarGeometry::calculate(
            DisplayId::new(0),
            Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))),
            &ShellRuntime::active_config(cx).bar,
        );
        Self::open_bar(cx, &geometry, false);
    }

    pub fn open_bar(cx: &mut App, geometry: &BarGeometry, with_display_geometry: bool) -> bool {
        let config = ShellRuntime::active_config(cx).bar;
        let spec = BarSpec::new(geometry.clone(), config, with_display_geometry);
        Self::open_bar_with_spec(cx, spec)
    }

    pub(crate) fn reconcile_bars(cx: &mut App) {
        let snapshot = Self::compositor_snapshot(cx);
        let outputs = cx
            .displays()
            .into_iter()
            .enumerate()
            .map(|(index, display)| {
                let display_id = display.id();
                let output_name = Self::output_name_for_display(&*display, &snapshot.outputs);
                let is_primary = index == 0;
                let bounds = Self::output_bounds(
                    output_name.as_deref(),
                    &snapshot.outputs,
                    display.bounds(),
                );

                OutputDescriptor {
                    display_id,
                    bounds,
                    name: output_name,
                    is_primary,
                    scale: None,
                }
            })
            .collect::<Vec<_>>();

        let ops = {
            let current_bars = cx
                .global::<ShellRuntime>()
                .shell_surfaces()
                .bars
                .iter()
                .map(|(id, (_, spec))| (*id, spec.clone()))
                .collect();
            crate::bar::reconciliation::reconcile_output_bars(
                &outputs,
                &ShellRuntime::active_config(cx),
                &current_bars,
            )
        };

        let mut mounted = false;
        for op in ops {
            match op {
                ReconciliationOp::Create(spec) => {
                    mounted |= Self::open_bar_with_spec(cx, spec);
                }
                ReconciliationOp::Recreate(spec) => {
                    let display_id = spec.display_id;
                    CardCoordinator::dispatch(cx, CardRequest::DisplayRemoved { display_id });
                    CardCoordinator::destroy_bands_for_display(cx, display_id);
                    if let Some((handle, _)) = cx
                        .global_mut::<ShellRuntime>()
                        .shell_surfaces_mut()
                        .bars
                        .remove(&display_id)
                    {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                    mounted |= Self::open_bar_with_spec(cx, spec);
                }
                ReconciliationOp::Remove(display_id) => {
                    CardCoordinator::dispatch(cx, CardRequest::DisplayRemoved { display_id });
                    CardCoordinator::destroy_bands_for_display(cx, display_id);
                    if let Some((handle, _)) = cx
                        .global_mut::<ShellRuntime>()
                        .shell_surfaces_mut()
                        .bars
                        .remove(&display_id)
                    {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                }
                ReconciliationOp::Retain(_) => {
                    mounted = true;
                }
            }
        }

        if !mounted && !cx.global::<ShellRuntime>().shell_surfaces().bars.is_empty() {
            mounted = true;
        }

        if !mounted {
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .set_bar_state(BarState::OpenFailed);
            ShellRuntime::publish_status(cx);
        }
    }

    pub fn mark_bar_open_failed(cx: &mut App) {
        cx.global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .set_bar_state(BarState::OpenFailed);
        ShellRuntime::publish_status(cx);
    }

    pub(crate) fn toggle_bar(cx: &mut App) {
        if !cx.global::<ShellRuntime>().shell_surfaces().bars_are_open() {
            Self::reconcile_bars(cx);
        } else {
            // Dismiss any open cards before hiding the bar.
            CardCoordinator::dispatch(cx, CardRequest::BarClosed);
            let old_bars =
                std::mem::take(&mut cx.global_mut::<ShellRuntime>().shell_surfaces_mut().bars);
            for (_, (handle, _)) in old_bars {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            shell_surfaces.bar_state = BarState::Hidden;
            shell_surfaces.last_bar_specs.clear();
            ShellRuntime::publish_status(cx);
        }
    }

    pub(super) fn capture_prior_focus(cx: &mut App) {
        let focused_window_id = Self::compositor_snapshot(cx).focused_window_id;
        let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
        if shell_surfaces.overview.is_none() {
            shell_surfaces.prior_window_id = focused_window_id;
        }
    }

    pub(super) fn restore_prior_focus(cx: &mut App) {
        let prior_window_id = {
            let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            if shell_surfaces.overview.is_some() {
                None
            } else {
                shell_surfaces.prior_window_id.take()
            }
        };
        if let Some(window_id) = prior_window_id {
            let _ = Self::overview_focus_window(cx, window_id);
        }
    }

    pub(crate) fn toggle_overview(cx: &mut App) {
        if cx
            .global::<ShellRuntime>()
            .shell_surfaces()
            .overview
            .is_some()
        {
            Self::close_overview(cx);
        } else {
            Self::open_or_focus_overview(cx);
        }
    }

    pub(crate) fn close_overview(cx: &mut App) {
        let Some(_) = cx
            .global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .begin_overview_close()
        else {
            return;
        };
        let overview = cx
            .global::<ShellRuntime>()
            .shell_surfaces()
            .overview_entity
            .clone();
        if let Some(overview) = overview {
            overview.update(cx, |view, cx| {
                view.begin_close(OverviewCloseReason::Cancel, cx);
            });
        }
    }

    pub(crate) fn finish_overview_close(
        cx: &mut App,
        instance_id: u64,
        reason: OverviewCloseReason,
    ) {
        let (opened_workspace_id, handle) = {
            let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            if shell_surfaces.overview_instance != instance_id {
                return;
            }
            let opened_workspace_id = shell_surfaces.overview_opened_workspace_id.take();
            let handle = shell_surfaces.overview.take();
            shell_surfaces.overview_entity = None;
            shell_surfaces.overview_lifecycle = OverviewLifecycle::Closed;
            (opened_workspace_id, handle)
        };

        if let Some(handle) = handle {
            let handle_id = handle.window_id();
            if let Err(error) = remove_window_after_frame_drain(handle, cx) {
                tracing::warn!(
                    ?error,
                    ?handle_id,
                    instance_id,
                    "failed to schedule overview window teardown"
                );
            }
        }

        if should_restore_overview_prior_focus(
            reason,
            opened_workspace_id,
            Self::compositor_snapshot(cx).focused_workspace_id,
        ) {
            Self::restore_prior_focus(cx);
        }

        ShellRuntime::publish_status(cx);
    }

    pub(crate) fn forget_overview(cx: &mut App, instance_id: u64) {
        let had_handle = {
            let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            if shell_surfaces.overview_instance != instance_id {
                return;
            }
            let had_handle = shell_surfaces.overview.take().is_some();
            shell_surfaces.overview_entity = None;
            shell_surfaces.overview_opened_workspace_id = None;
            shell_surfaces.overview_lifecycle = OverviewLifecycle::Closed;
            had_handle
        };

        if had_handle {
            Self::restore_prior_focus(cx);
        }

        ShellRuntime::publish_status(cx);
    }

    pub(crate) fn overview_focus_workspace(cx: &mut App, ws_id: u64) -> Result<(), ShellError> {
        ShellRuntime::dispatch_action(cx, ActionInvocation::FocusWorkspace(ws_id))
    }

    pub(crate) fn overview_move_window(
        cx: &mut App,
        window_id: u64,
        workspace_id: u64,
    ) -> Result<(), ShellError> {
        ShellRuntime::dispatch_action(
            cx,
            ActionInvocation::MoveWindowToWorkspace {
                window_id,
                workspace_id,
            },
        )
    }

    pub(crate) fn overview_focus_window(cx: &mut App, window_id: u64) -> Result<(), ShellError> {
        ShellRuntime::dispatch_action(cx, ActionInvocation::FocusWindow(window_id))
    }

    pub(crate) fn overview_reduced_motion(cx: &App) -> bool {
        ShellRuntime::active_config(cx).theme.reduced_motion
    }

    pub(crate) fn refresh_bars(cx: &mut App) {
        let handles = cx.global::<ShellRuntime>().shell_surfaces().bar_handles();
        for handle in handles {
            if let Err(error) = handle.update(cx, |_, _, cx| cx.notify()) {
                tracing::debug!(
                    ?error,
                    window_id = ?handle.window_id(),
                    surface = "bar",
                    "stale window handle on bar refresh"
                );
            }
        }
    }

    pub(crate) fn overview_applications(cx: &App) -> Vec<shilpo_services::Application> {
        ShellRuntime::app_scanner(cx)
            .map(|scanner| scanner.applications())
            .unwrap_or_default()
    }

    pub fn normalize_app_key(value: &str) -> String {
        crate::app_icons::normalize_app_key(value)
    }

    pub fn app_icon_index(cx: &App) -> HashMap<String, PathBuf> {
        let apps = Self::overview_applications(cx);
        crate::app_icons::build_app_icon_index(apps)
    }

    pub(crate) fn open_or_focus_overview(cx: &mut App) {
        Self::open_or_focus_overview_on_display(cx, None);
    }

    pub(crate) fn open_or_focus_overview_on_display(
        cx: &mut App,
        target_display_id: Option<DisplayId>,
    ) {
        if let Some(handle) = cx.global::<ShellRuntime>().shell_surfaces().overview {
            if let Err(error) = handle.update(cx, |_, window, _| window.activate_window()) {
                tracing::debug!(
                    ?error,
                    window_id = ?handle.window_id(),
                    surface = "overview",
                    "stale window handle on overview activate"
                );
            }
            return;
        }

        Self::close_overview_competitors(cx);
        Self::capture_prior_focus(cx);
        if let Some(discovered_wallpaper_path) = query_awww_wallpaper_path() {
            let resource = ShellRuntime::wallpaper_preview(cx);
            if reconcile_discovered_wallpaper(&resource, discovered_wallpaper_path.clone(), cx) {
                shilpo_theme_daemon::ThemeClient::spawn_task(async move {
                    let client = shilpo_theme_daemon::ThemeClient::new().await;
                    let _ = client
                        .set_wallpaper(discovered_wallpaper_path.to_str().unwrap_or_default())
                        .await;
                });

                Self::refresh_bars(cx);
            }
        }

        let instance_id = cx
            .global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .next_overview_instance();
        let opened_workspace_id = Self::compositor_snapshot(cx).focused_workspace_id;
        cx.global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .overview_opened_workspace_id = opened_workspace_id;

        let window_size = size(px(1280.), px(720.));
        let (origin, display_id) = if let Some(display) = cx
            .displays()
            .into_iter()
            .find(|d| target_display_id == Some(d.id()))
            .or_else(|| cx.primary_display())
        {
            let bounds = display.bounds();
            (
                point(
                    bounds.origin.x + (bounds.size.width - window_size.width) / 2.0,
                    bounds.origin.y + (bounds.size.height - window_size.height) / 2.0,
                ),
                Some(display.id()),
            )
        } else {
            (point(px(320.), px(180.)), None)
        };

        let options = overlay_options(
            "shilpo-overview",
            "overview",
            window_size,
            origin,
            display_id,
        );

        cx.global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .overview_lifecycle = OverviewLifecycle::Opening {
            generation: instance_id,
        };
        match cx.open_window(options, move |window, cx| {
            WorkspaceOverview::view(
                OverviewLifecycleCallback {
                    instance: instance_id,
                },
                window,
                cx,
            )
        }) {
            Ok(handle) => {
                let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
                shell_surfaces.overview = Some(handle);
                shell_surfaces.overview_lifecycle = OverviewLifecycle::Open {
                    generation: instance_id,
                };
                ShellRuntime::publish_status(cx);
            }
            Err(error) => {
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .overview_lifecycle = OverviewLifecycle::Closed;
                tracing::warn!(error = %error, "failed to open overview overlay");
            }
        }
    }

    /// Overview is an exclusive focused surface. Bars remain mounted, while
    /// transient overlays that could compete for focus are dismissed first.
    fn close_overview_competitors(cx: &mut App) {
        // Dismiss any open cards before Overview opens.
        CardCoordinator::dispatch(cx, CardRequest::OverviewOpened);
        let (notification, osd, panel) = {
            let surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            (
                surfaces.notification.take(),
                surfaces.osd.take(),
                surfaces.extension_panel.take(),
            )
        };
        if let Some((_, notification_id, handle)) = notification {
            Self::dismiss_notification(cx, notification_id);
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        if let Some((_, handle, _)) = osd {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        if let Some((handle, contribution)) = panel {
            cx.global_mut::<ShellRuntime>()
                .extension_host_mut()
                .unmount_contribution(&contribution);
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        let surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
        surfaces.notification_lifecycle = SurfaceLifecycle::Closed;
        surfaces.osd_lifecycle = SurfaceLifecycle::Closed;
    }

    pub(crate) fn expire_notification(cx: &mut App, generation: u64) {
        let entry = cx
            .global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .notification
            .take();
        let Some((current_generation, notification_id, handle)) = entry else {
            tracing::debug!(
                generation,
                "expire_notification called but no active notification entry exists"
            );
            return;
        };
        if current_generation != generation {
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .notification = Some((current_generation, notification_id, handle));
            tracing::debug!(
                generation,
                current_generation,
                "expire_notification called with stale generation"
            );
            return;
        }
        tracing::debug!(
            generation,
            notification_id,
            "expire_notification removing toast window"
        );
        cx.global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .notification_lifecycle = SurfaceLifecycle::Closing { generation };
        Self::expire_notification_id(cx, notification_id);
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        cx.global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .notification_lifecycle = SurfaceLifecycle::Closed;
    }

    pub(crate) fn forget_notification(cx: &mut App, generation: u64) {
        let is_current = cx
            .global::<ShellRuntime>()
            .shell_surfaces()
            .notification
            .as_ref()
            .is_some_and(|(current_generation, _, _)| *current_generation == generation);
        if is_current
            && let Some((_, notification_id, _)) = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .notification
                .take()
        {
            tracing::debug!(
                generation,
                notification_id,
                "forget_notification dropped notification entry"
            );
            Self::dismiss_notification(cx, notification_id);
        } else {
            tracing::debug!(
                generation,
                "forget_notification generation mismatch or empty slot"
            );
        }
    }

    fn dismiss_notification(cx: &App, notification_id: u32) {
        if cx.has_global::<ShellRuntime>()
            && let Some(hub) = cx.global::<ShellRuntime>().service_hub()
        {
            hub.dismiss_notification(notification_id);
        }
    }

    fn expire_notification_id(cx: &App, notification_id: u32) {
        if cx.has_global::<ShellRuntime>()
            && let Some(hub) = cx.global::<ShellRuntime>().service_hub()
        {
            hub.expire_notification(notification_id);
        }
    }

    pub(crate) fn forget_osd(cx: &mut App) {
        if cx.has_global::<ShellRuntime>() {
            let surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            surfaces.osd = None;
            surfaces.osd_lifecycle = SurfaceLifecycle::Closed;
        }
    }

    fn schedule_osd_dismiss(cx: &mut App, generation: u64) {
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            cx.update(|cx: &mut gpui::App| {
                if cx.has_global::<ShellRuntime>() {
                    let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
                    if let Some((current_gen, handle, _)) = &shell_surfaces.osd
                        && *current_gen == generation
                    {
                        let window_handle = *handle;
                        shell_surfaces.osd = None;
                        shell_surfaces.osd_lifecycle = SurfaceLifecycle::Closed;
                        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
                    }
                }
            });
        })
        .detach();
    }

    pub(crate) fn show_osd(cx: &mut App, kind: crate::osd::OsdKind) {
        let existing = cx
            .global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .osd
            .take();
        if let Some((generation, window_handle, view_handle)) = existing
            && window_handle
                .update(cx, |_, _, window_cx| {
                    view_handle.update(window_cx, |view, view_cx| {
                        view.kind = kind.clone();
                        view_cx.notify();
                    });
                })
                .is_ok()
        {
            let next_gen = generation + 1;
            cx.global_mut::<ShellRuntime>().shell_surfaces_mut().osd =
                Some((next_gen, window_handle, view_handle));
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .osd_generation = next_gen;
            cx.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .osd_lifecycle = SurfaceLifecycle::Open {
                generation: next_gen,
            };
            Self::schedule_osd_dismiss(cx, next_gen);
            return;
        }
        // Window was already closed or not existing; fall through to create a fresh surface.

        let (display_bounds, display_id) = if let Some(display) = cx.primary_display() {
            (display.bounds(), Some(display.id()))
        } else {
            (
                Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))),
                None,
            )
        };
        let osd_size = size(px(260.), px(48.));
        let origin = point(
            display_bounds.origin.x + (display_bounds.size.width - osd_size.width) / 2.0,
            display_bounds.origin.y + display_bounds.size.height - px(140.),
        );
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, osd_size))),
            display_id,
            app_id: Some("shilpo-osd".into()),
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "osd".into(),
                layer: Layer::Overlay,
                anchor: Anchor::BOTTOM,
                margin: Some((px(0.), px(0.), px(84.), px(0.))),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            }),
            focus: false,
            show: true,
            ..Default::default()
        };

        let spawned_view: std::sync::Arc<std::sync::Mutex<Option<Entity<crate::osd::OsdView>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let view_cell = spawned_view.clone();
        let window_result = cx.open_window(options, move |window, cx| {
            let (root, view) = crate::osd::OsdView::view(kind, window, cx);
            *view_cell.lock().unwrap() = Some(view);
            root
        });

        if let Ok(window_handle) = window_result
            && let Some(view_handle) = spawned_view.lock().unwrap().take()
        {
            cx.global_mut::<ShellRuntime>().shell_surfaces_mut().osd =
                Some((1, window_handle, view_handle));
            let surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
            surfaces.osd_generation = 1;
            surfaces.osd_lifecycle = SurfaceLifecycle::Open { generation: 1 };
            Self::schedule_osd_dismiss(cx, 1);
        }
    }

    pub(crate) fn forget_polkit(cx: &mut App) {
        if cx.has_global::<ShellRuntime>() {
            let runtime = cx.global_mut::<ShellRuntime>();
            runtime.shell_surfaces_mut().polkit = None;
            runtime.shell_surfaces_mut().polkit_lifecycle = SurfaceLifecycle::Closed;
        }
    }

    pub(crate) fn sync_polkit_dialog(
        cx: &mut App,
        request: Option<shilpo_services::PolkitRequest>,
        prompt_state: Option<shilpo_services::PolkitPromptState>,
    ) {
        if let Some(req) = request {
            let existing = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .polkit
                .take();

            if let Some((generation, window_handle, view_handle)) = existing
                && window_handle
                    .update(cx, |_, window, window_cx| {
                        view_handle.update(window_cx, |view, view_cx| {
                            view.update_state(req.clone(), prompt_state.clone(), window, view_cx);
                        });
                    })
                    .is_ok()
            {
                cx.global_mut::<ShellRuntime>().shell_surfaces_mut().polkit =
                    Some((generation, window_handle, view_handle));
                return;
            }

            // Open new modal overlay window
            let (display_bounds, display_id) = if let Some(display) = cx.primary_display() {
                (display.bounds(), Some(display.id()))
            } else {
                (
                    Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))),
                    None,
                )
            };

            let options = WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(display_bounds)),
                display_id,
                app_id: Some("shilpo-polkit-agent".into()),
                window_background: WindowBackgroundAppearance::Transparent,
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "polkit-agent".into(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    keyboard_interactivity: KeyboardInteractivity::Exclusive,
                    ..Default::default()
                }),
                focus: true,
                show: true,
                ..Default::default()
            };

            let req_clone = req.clone();
            let prompt_clone = prompt_state.clone();
            let spawned_view: std::sync::Arc<
                std::sync::Mutex<Option<Entity<crate::polkit::PolkitDialogView>>>,
            > = std::sync::Arc::new(std::sync::Mutex::new(None));
            let view_cell = spawned_view.clone();

            let window_result = cx.open_window(options, move |window, cx| {
                let view = cx.new(|cx| {
                    crate::polkit::PolkitDialogView::new(req_clone, prompt_clone, window, cx)
                });
                let weak_view = view.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    if let Some(view) = weak_view.upgrade() {
                        view.update(cx, |view, cx| view.clear_input(cx));
                    }
                    ShellSurfaces::forget_polkit(cx);
                    true
                });
                *view_cell.lock().unwrap() = Some(view.clone());
                cx.new(|cx| shilpo_m3e::Root::new(view, window, cx).bordered(false))
            });

            if let Ok(window_handle) = window_result
                && let Some(view_handle) = spawned_view.lock().unwrap().take()
            {
                let generation = cx
                    .global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .polkit_generation
                    + 1;
                cx.global_mut::<ShellRuntime>().shell_surfaces_mut().polkit =
                    Some((generation, window_handle, view_handle));
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .polkit_generation = generation;
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .polkit_lifecycle = SurfaceLifecycle::Open { generation };
            }
        } else {
            // Close polkit window if open
            if let Some((_, window_handle, view_handle)) = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .polkit
                .take()
            {
                view_handle.update(cx, |view, cx| view.clear_input(cx));
                let _ = window_handle.update(cx, |_, window, _| window.remove_window());
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .polkit_lifecycle = SurfaceLifecycle::Closed;
            }
        }
    }

    pub(crate) fn forget_idle_grace(cx: &mut App) {
        if cx.has_global::<ShellRuntime>() {
            let runtime = cx.global_mut::<ShellRuntime>();
            runtime.shell_surfaces_mut().idle_grace = None;
            runtime.shell_surfaces_mut().idle_grace_lifecycle = SurfaceLifecycle::Closed;
        }
    }

    pub(crate) fn sync_idle_grace(
        cx: &mut App,
        active_grace: Option<shilpo_services::ActiveGraceInfo>,
    ) {
        if let Some(grace) = active_grace {
            let existing = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .idle_grace
                .take();

            if let Some((generation, window_handle, view_handle)) = existing {
                if generation == grace.grace_generation {
                    cx.global_mut::<ShellRuntime>()
                        .shell_surfaces_mut()
                        .idle_grace = Some((generation, window_handle, view_handle));
                    return;
                } else {
                    let _ = window_handle.update(cx, |_, window, _| window.remove_window());
                }
            }

            let (display_bounds, display_id) = if let Some(display) = cx.primary_display() {
                (display.bounds(), Some(display.id()))
            } else {
                (
                    Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))),
                    None,
                )
            };

            let options = WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(display_bounds)),
                display_id,
                app_id: Some("shilpo-idle-grace".into()),
                window_background: WindowBackgroundAppearance::Transparent,
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: "idle-grace".into(),
                    layer: Layer::Overlay,
                    anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                focus: false,
                show: true,
                ..Default::default()
            };

            let grace_gen = grace.grace_generation;
            let fade_ms = grace.fade_ms;
            let spawned_view: std::sync::Arc<
                std::sync::Mutex<Option<Entity<crate::shell::idle::IdleGraceOverlayView>>>,
            > = std::sync::Arc::new(std::sync::Mutex::new(None));
            let view_cell = spawned_view.clone();

            let window_result = cx.open_window(options, move |window, cx| {
                window.on_window_should_close(cx, |_, cx| {
                    ShellSurfaces::forget_idle_grace(cx);
                    true
                });
                let view = cx.new(|cx| {
                    crate::shell::idle::IdleGraceOverlayView::new(grace_gen, fade_ms, window, cx)
                });
                *view_cell.lock().unwrap() = Some(view.clone());
                cx.new(|cx| shilpo_m3e::Root::new(view, window, cx).bordered(false))
            });

            if let Ok(window_handle) = window_result
                && let Some(view_handle) = spawned_view.lock().unwrap().take()
            {
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .idle_grace = Some((grace_gen, window_handle, view_handle));
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .idle_grace_generation = grace_gen;
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .idle_grace_lifecycle = SurfaceLifecycle::Open {
                    generation: grace_gen,
                };
            }
        } else {
            // Close idle grace window if open
            if let Some((_, window_handle, _)) = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .idle_grace
                .take()
            {
                let _ = window_handle.update(cx, |_, window, _| window.remove_window());
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .idle_grace_lifecycle = SurfaceLifecycle::Closed;
            }
        }
    }

    pub(crate) fn forget_bar(cx: &mut App) {
        let shell_surfaces = cx.global_mut::<ShellRuntime>().shell_surfaces_mut();
        shell_surfaces.bars.clear();
        shell_surfaces.bar_state = BarState::Hidden;
        ShellRuntime::publish_status(cx);
    }

    pub fn open_extension_panel(cx: &mut App, contribution: CanonicalId) {
        let existing = cx
            .global_mut::<ShellRuntime>()
            .shell_surfaces_mut()
            .extension_panel
            .take();
        if let Some((handle, current)) = existing {
            if current == contribution
                && handle
                    .update(cx, |_, window, _| window.activate_window())
                    .is_ok()
            {
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .extension_panel = Some((handle, current));
                return;
            }
            cx.global_mut::<ShellRuntime>()
                .extension_host_mut()
                .unmount_contribution(&current);
            let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
        }
        let (display_bounds, display_id) = if let Some(display) = cx.primary_display() {
            (display.bounds(), Some(display.id()))
        } else {
            (
                Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))),
                None,
            )
        };
        let panel_size = size(px(420.), px(600.));
        let origin = point(
            display_bounds.origin.x + display_bounds.size.width - panel_size.width - px(16.),
            display_bounds.origin.y + px(56.),
        );
        let options = overlay_options(
            "shilpo-extension-panel",
            "extension-panel",
            panel_size,
            origin,
            display_id,
        );
        let view_id = contribution.clone();
        match cx.open_window(options, move |window, cx| {
            crate::extension_surface::ExtensionSurfaceView::view(view_id, None, window, cx)
        }) {
            Ok(handle) => {
                cx.global_mut::<ShellRuntime>()
                    .extension_host_mut()
                    .mount_contribution(&contribution, 420., 600.);
                cx.global_mut::<ShellRuntime>()
                    .shell_surfaces_mut()
                    .extension_panel = Some((handle, contribution));
            }
            Err(error) => tracing::warn!(error = %error, "failed to open extension side panel"),
        }
    }

    /// Reconciles the extension desktop surfaces (bar widgets + config surfaces)
    /// against the desired instance layout.
    pub(crate) fn reconcile_extension_surfaces(cx: &mut App, outputs: &[OutputDescriptor]) {
        let config = ShellRuntime::active_config(cx);
        let mut instances = Vec::new();

        let bars = cx.global::<ShellRuntime>().shell_surfaces().bars.clone();
        for (display_id, (_, spec)) in &bars {
            for (section, widgets) in [
                ("start", &spec.config.widgets.start),
                ("center", &spec.config.widgets.center),
                ("end", &spec.config.widgets.end),
            ] {
                for (index, widget) in widgets.iter().enumerate() {
                    if let crate::config::BarWidget::Extension(contribution) = widget {
                        instances.push(ContributionInstance {
                            id: format!("bar:{display_id:?}:{section}:{index}"),
                            contribution: contribution.clone(),
                            output: Some(format!("{display_id:?}")),
                            width: spec.geometry.bounds.size.width.as_f32(),
                            height: spec.config.height as f32,
                            settings: extension_settings(&config, &contribution.extension_id, None),
                        });
                    }
                }
            }
        }

        let mut desired_windows = HashMap::new();
        for widget in &config.desktop.widgets {
            let output = if widget.output == "primary" {
                outputs.iter().find(|output| output.is_primary)
            } else {
                outputs.iter().find(|output| {
                    output.name.as_deref() == Some(widget.output.as_str())
                        || format!("{:?}", output.display_id) == widget.output
                })
            };
            let Some(output) = output else {
                continue;
            };
            let bounds = Bounds::new(
                point(
                    output.bounds.origin.x + px(widget.x as f32),
                    output.bounds.origin.y + px(widget.y as f32),
                ),
                size(px(widget.width as f32), px(widget.height as f32)),
            );
            let spec = ExtensionSurfaceSpec {
                contribution: widget.contribution.0.clone(),
                display_id: output.display_id,
                bounds,
            };
            desired_windows.insert(widget.instance.to_string(), spec);
            instances.push(ContributionInstance {
                id: widget.instance.clone(),
                contribution: widget.contribution.0.clone(),
                output: Some(widget.output.clone()),
                width: widget.width as f32,
                height: widget.height as f32,
                settings: extension_settings(
                    &config,
                    &widget.contribution.0.extension_id,
                    Some(&widget.settings),
                ),
            });
        }

        cx.global_mut::<ShellRuntime>()
            .extension_host_mut()
            .send_instance_reconciliation(instances);

        let stale = cx
            .global::<ShellRuntime>()
            .shell_surfaces()
            .stale_extension_surface_ids(&desired_windows);
        for id in stale {
            if let Some((handle, _)) = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .remove_extension_surface(&id)
            {
                let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
            }
        }

        for (instance_id, spec) in desired_windows {
            if cx
                .global::<ShellRuntime>()
                .shell_surfaces()
                .has_extension_surface(&instance_id)
            {
                continue;
            }
            let options = WindowOptions {
                titlebar: None,
                window_bounds: Some(WindowBounds::Windowed(spec.bounds)),
                display_id: Some(spec.display_id),
                app_id: Some(format!("shilpo-extension-{instance_id}")),
                window_background: WindowBackgroundAppearance::Transparent,
                kind: WindowKind::LayerShell(LayerShellOptions {
                    namespace: format!("extension-{instance_id}"),
                    layer: Layer::Bottom,
                    keyboard_interactivity: KeyboardInteractivity::None,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let contribution = spec.contribution.clone();
            let view_instance_id = instance_id.clone();
            match cx.open_window(options, move |window, cx| {
                crate::extension_surface::ExtensionSurfaceView::view(
                    contribution,
                    Some(view_instance_id),
                    window,
                    cx,
                )
            }) {
                Ok(handle) => {
                    cx.global_mut::<ShellRuntime>()
                        .shell_surfaces_mut()
                        .store_extension_surface(instance_id, handle, spec);
                }
                Err(error) => tracing::warn!(
                    error = %error,
                    "failed to open extension desktop surface"
                ),
            }
        }
    }

    /// Reconciles the extension instances contributed through bar widgets.
    pub(crate) fn reconcile_bar_extension_instances(cx: &mut App) {
        let config = ShellRuntime::active_config(cx);
        let bars = cx.global::<ShellRuntime>().shell_surfaces().bars.clone();
        let mut instances = Vec::new();
        for (display_id, (_, spec)) in &bars {
            for (section, widgets) in [
                ("start", &spec.config.widgets.start),
                ("center", &spec.config.widgets.center),
                ("end", &spec.config.widgets.end),
            ] {
                for (index, widget) in widgets.iter().enumerate() {
                    if let crate::config::BarWidget::Extension(contribution) = widget {
                        instances.push(ContributionInstance {
                            id: format!("bar:{display_id:?}:{section}:{index}"),
                            contribution: contribution.clone(),
                            output: Some(format!("{display_id:?}")),
                            width: spec.geometry.bounds.size.width.as_f32(),
                            height: spec.config.height as f32,
                            settings: extension_settings(&config, &contribution.extension_id, None),
                        });
                    }
                }
            }
        }
        cx.global_mut::<ShellRuntime>()
            .extension_host_mut()
            .send_instance_reconciliation(instances);
    }

    /// Applies a theme daemon state change to the wallpaper path and refreshes
    /// every surface that renders the wallpaper.
    pub(crate) fn apply_theme_state(cx: &mut App, state: &DaemonState) {
        if cx.has_global::<ShellRuntime>() {
            ShellRuntime::set_wallpaper_path(cx, state.wallpaper_path.clone());
        }
        let ov_handle = if cx.has_global::<ShellRuntime>() {
            cx.global::<ShellRuntime>().shell_surfaces().overview
        } else {
            None
        };
        Self::refresh_bars(cx);
        if let Some(ov) = ov_handle
            && let Err(error) = ov.update(cx, |_, _, cx| cx.notify())
        {
            tracing::debug!(
                ?error,
                window_id = ?ov.window_id(),
                surface = "overview",
                "stale window handle on theme update"
            );
        }
        cx.refresh_windows();
    }
}

fn readiness_for(
    connection: &shilpo_services::DomainLifecycle,
    bar_state: &BarState,
) -> shilpo_services::ReadinessState {
    match connection {
        shilpo_services::DomainLifecycle::Unavailable
        | shilpo_services::DomainLifecycle::Connecting => shilpo_services::ReadinessState::Starting,
        shilpo_services::DomainLifecycle::Ready => {
            if matches!(bar_state, BarState::Visible | BarState::Hidden) {
                shilpo_services::ReadinessState::Ready
            } else {
                shilpo_services::ReadinessState::Degraded
            }
        }
        shilpo_services::DomainLifecycle::Reconnecting
        | shilpo_services::DomainLifecycle::Degraded => shilpo_services::ReadinessState::Degraded,
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    struct LifecycleTestView;

    impl gpui::Render for LifecycleTestView {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    #[gpui::test]
    fn overview_window_teardown_drains_the_requested_frame_before_removal(
        cx: &mut gpui::TestAppContext,
    ) {
        let frame_drained = Rc::new(Cell::new(false));
        let handle = cx.add_window(|_, _| LifecycleTestView);
        cx.update_window(handle.into(), |_, window, app| window.draw(app).clear())
            .unwrap();

        cx.update(|app| {
            handle
                .update(app, |_, window, _| {
                    let frame_drained = frame_drained.clone();
                    window.on_next_frame(move |_, _| frame_drained.set(true));
                    window.refresh();
                })
                .unwrap();
            remove_window_after_frame_drain(handle, app).unwrap();
        });

        let drained_callbacks = cx
            .update_window(handle.into(), |_, window, app| {
                window.simulate_next_frame(app)
            })
            .unwrap();
        assert_eq!(drained_callbacks, 2);
        cx.run_until_parked();
        assert!(
            frame_drained.get(),
            "teardown removed the window before its requested frame drained"
        );
        assert!(handle.update(cx, |_, _, _| ()).is_err());
    }

    #[test]
    fn test_shell_surfaces_initialization() {
        let snapshot = Arc::new(CompositorSnapshot::default());
        let manager = ShellSurfaces::new(snapshot);
        assert_eq!(manager.bar_state(), BarState::Starting);
        assert!(!manager.bars_are_open());
        assert!(!manager.overview_is_open());
    }

    #[test]
    fn parses_awww_wallpaper_query() {
        let output =
            ": eDP-1: 1920x1080, scale: 1, currently displaying: image: /pictures/wallpaper.png\n";
        assert_eq!(
            parse_awww_wallpaper_path(output),
            Some(PathBuf::from("/pictures/wallpaper.png"))
        );
        assert_eq!(parse_awww_wallpaper_path("no image"), None);
    }

    #[test]
    fn discovered_wallpaper_syncs_when_theme_path_is_missing_or_stale() {
        let discovered = Path::new("/pictures/red-wallpaper.png");

        assert!(discovered_wallpaper_needs_theme_sync(None, discovered));
        assert!(discovered_wallpaper_needs_theme_sync(
            Some(Path::new("/pictures/old-wallpaper.png")),
            discovered
        ));
        assert!(!discovered_wallpaper_needs_theme_sync(
            Some(discovered),
            discovered
        ));
    }

    #[gpui::test]
    fn unchanged_discovered_path_revalidates_replaced_wallpaper(cx: &mut gpui::TestAppContext) {
        let path = std::env::temp_dir().join(format!(
            "shilpo-wallpaper-reconcile-{}.png",
            uuid::Uuid::new_v4()
        ));
        image::RgbImage::new(32, 32)
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let resource = cx.update(|cx| cx.new(WallpaperPreviewResource::new));

        let first_needs_sync =
            cx.update(|cx| reconcile_discovered_wallpaper(&resource, path.clone(), cx));
        assert!(first_needs_sync);
        cx.run_until_parked();
        let first = cx
            .update(|cx| resource.read(cx).snapshot().ready_image())
            .expect("first prepared wallpaper");

        image::RgbImage::new(64, 64)
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let replacement_needs_sync =
            cx.update(|cx| reconcile_discovered_wallpaper(&resource, path.clone(), cx));
        assert!(!replacement_needs_sync, "the pathname did not change");
        cx.run_until_parked();
        let replacement = cx
            .update(|cx| resource.read(cx).snapshot().ready_image())
            .expect("replacement prepared wallpaper");

        assert!(!Arc::ptr_eq(&first, &replacement));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn closing_overview_after_workspace_change_does_not_restore_origin_focus() {
        assert!(!should_restore_overview_prior_focus(
            OverviewCloseReason::Cancel,
            Some(2),
            Some(1),
        ));
        assert!(should_restore_overview_prior_focus(
            OverviewCloseReason::Cancel,
            Some(2),
            Some(2),
        ));
        assert!(!should_restore_overview_prior_focus(
            OverviewCloseReason::Selection,
            Some(2),
            Some(2),
        ));
    }

    #[test]
    fn output_name_matching_uses_geometry_when_uuid_mapping_is_unavailable() {
        let outputs = vec![
            CompositorOutput {
                name: "eDP-1".into(),
                make: None,
                model: None,
                logical_position: (0, 0),
                logical_size: (1920, 1080),
                scale: 1.0,
            },
            CompositorOutput {
                name: "HDMI-A-1".into(),
                make: None,
                model: None,
                logical_position: (1920, 0),
                logical_size: (1920, 1080),
                scale: 1.0,
            },
        ];
        let bounds = Bounds::new(point(px(1920.), px(0.)), size(px(1920.), px(1080.)));
        assert_eq!(
            ShellSurfaces::output_name_for_bounds(bounds, &outputs),
            Some("HDMI-A-1".into())
        );
    }

    #[test]
    fn overview_instances_advance_monotonically() {
        let snapshot = Arc::new(CompositorSnapshot::default());
        let mut manager = ShellSurfaces::new(snapshot);
        let first = manager.next_overview_instance();
        let second = manager.next_overview_instance();
        assert!(second > first);
    }

    #[test]
    fn overview_close_transition_is_idempotent_per_generation() {
        let snapshot = Arc::new(CompositorSnapshot::default());
        let mut manager = ShellSurfaces::new(snapshot);
        let generation = manager.next_overview_instance();
        manager.overview_lifecycle = OverviewLifecycle::Open { generation };

        assert_eq!(manager.begin_overview_close(), Some(generation));
        assert_eq!(
            manager.overview_lifecycle,
            OverviewLifecycle::Closing { generation }
        );
        assert_eq!(manager.begin_overview_close(), None);
    }

    #[test]
    fn notification_generation_reserves_incrementally() {
        let snapshot = Arc::new(CompositorSnapshot::default());
        let mut manager = ShellSurfaces::new(snapshot);
        let first = manager.next_notification_generation();
        let second = manager.next_notification_generation();
        assert!(second > first);
    }

    #[test]
    fn transient_surface_lifecycles_start_closed() {
        let manager = ShellSurfaces::new(Arc::new(CompositorSnapshot::default()));
        assert_eq!(manager.notification_lifecycle, SurfaceLifecycle::Closed);
        assert_eq!(manager.osd_lifecycle, SurfaceLifecycle::Closed);
        assert_eq!(manager.capture_lifecycle, SurfaceLifecycle::Closed);
        assert_eq!(manager.extension_lifecycle, SurfaceLifecycle::Closed);
    }

    #[gpui::test]
    fn semantic_snapshot_without_runtime_is_closed(cx: &mut gpui::TestAppContext) {
        let snapshot = cx.update(|app| ShellSurfaces::snapshot(app));
        assert_eq!(snapshot.overview_lifecycle, OverviewLifecycle::Closed);
        assert_eq!(snapshot.notification_lifecycle, SurfaceLifecycle::Closed);
        assert_eq!(snapshot.osd_lifecycle, SurfaceLifecycle::Closed);
        assert_eq!(snapshot.capture_lifecycle, SurfaceLifecycle::Closed);
    }

    #[gpui::test]
    fn semantic_osd_requests_open_replace_and_close(cx: &mut gpui::TestAppContext) {
        cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
            ShellSurfaces::request(
                app,
                SurfaceRequest::ShowOsd(crate::osd::OsdKind::Volume {
                    level: 20,
                    muted: false,
                }),
            );
        });
        let first = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        let SurfaceLifecycle::Open {
            generation: first_generation,
        } = first
        else {
            panic!("semantic OSD request did not open its lifecycle");
        };
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1_500));
        cx.run_until_parked();
        cx.update(|app| {
            ShellSurfaces::request(
                app,
                SurfaceRequest::ShowOsd(crate::osd::OsdKind::Volume {
                    level: 80,
                    muted: false,
                }),
            )
        });
        let second = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        assert!(
            matches!(second, SurfaceLifecycle::Open { generation } if generation > first_generation)
        );
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        cx.run_until_parked();
        let after_stale_deadline = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        assert_eq!(after_stale_deadline, second);
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1_500));
        cx.run_until_parked();
        let closed = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        assert_eq!(closed, SurfaceLifecycle::Closed);
    }

    #[test]
    fn extension_settings_merge_global_values_with_instance_overrides() {
        let mut config = crate::config::ShellConfig::default();
        config.extensions.settings.insert(
            "org.shilpo.weather".into(),
            serde_json::json!({
                "location": "Kolkata",
                "show_condition": false
            }),
        );
        let id = ExtensionId::new("org.shilpo.weather").unwrap();
        assert_eq!(
            extension_settings(
                &config,
                &id,
                Some(&serde_json::json!({"show_condition": true}))
            ),
            serde_json::json!({
                "location": "Kolkata",
                "show_condition": true
            })
        );
    }

    #[test]
    fn test_performance_frame_budget_compliance() {
        use std::time::Instant;
        let config = crate::config::ShellConfig::default();
        let display_bounds = gpui::Bounds {
            origin: gpui::Point::default(),
            size: gpui::Size {
                width: gpui::px(1920.0),
                height: gpui::px(1080.0),
            },
        };
        let display_id = gpui::DisplayId::from(1u64);

        let start = Instant::now();
        for _ in 0..1000 {
            let _geom = crate::bar::geometry::BarGeometry::calculate_with_scale(
                display_id,
                display_bounds,
                &config.bar,
                Some(1.0),
            );
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 16,
            "1000 geometry calculations took {:?}, exceeding 16.6ms frame budget",
            elapsed
        );
    }

    #[test]
    fn test_multi_output_dpi_resolution_scaling_fixtures() {
        let config = crate::config::ShellConfig::default();
        let display_bounds = Bounds::new(point(px(0.), px(0.)), size(px(3840.), px(2160.)));
        let display_id = gpui::DisplayId::from(2u64);

        let bar_geom = crate::bar::geometry::BarGeometry::calculate_with_scale(
            display_id,
            display_bounds,
            &config.bar,
            Some(2.0),
        );
        assert_eq!(bar_geom.display_id, display_id);
        assert_eq!(bar_geom.bounds.size.width, px(3840.));
    }

    struct ShellSurfacesTestHarness {
        manager: ShellSurfaces,
    }

    impl ShellSurfacesTestHarness {
        fn new_offline() -> Self {
            let snapshot = Arc::new(CompositorSnapshot::default());
            Self {
                manager: ShellSurfaces::new(snapshot),
            }
        }
    }

    #[test]
    fn test_harness_shell_surfaces_readiness_and_extension_surfaces() {
        let mut harness = ShellSurfacesTestHarness::new_offline();
        assert_eq!(
            harness.manager.readiness(),
            shilpo_services::ReadinessState::Starting
        );

        harness.manager.set_bar_state(BarState::Visible);
        harness.manager.update_readiness();
        assert_eq!(
            harness.manager.readiness(),
            shilpo_services::ReadinessState::Starting
        );

        let ready_snapshot = CompositorSnapshot {
            connection: shilpo_services::DomainLifecycle::Ready,
            ..Default::default()
        };
        harness
            .manager
            .set_latest_snapshot(Arc::new(ready_snapshot));
        harness.manager.update_readiness();
        assert_eq!(
            harness.manager.readiness(),
            shilpo_services::ReadinessState::Ready
        );

        assert!(!harness.manager.has_extension_surface("inst-1"));
        let desired = HashMap::new();
        assert!(
            harness
                .manager
                .stale_extension_surface_ids(&desired)
                .is_empty()
        );
        assert!(harness.manager.active_surfaces().is_empty());
    }

    #[gpui::test]
    fn osd_slot_cleared_when_window_externally_closed(cx: &mut gpui::TestAppContext) {
        cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
        });

        // Open an OSD surface.
        cx.update(|app| {
            ShellSurfaces::request(
                app,
                SurfaceRequest::ShowOsd(crate::osd::OsdKind::Volume {
                    level: 50,
                    muted: false,
                }),
            );
        });

        let osd_lifecycle = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        assert!(
            matches!(osd_lifecycle, SurfaceLifecycle::Open { .. }),
            "OSD should be open after request"
        );

        // Simulate external window closure: extract the window ID and call
        // handle_window_closed directly, as the compositor would.
        let window_id = cx.update(|app| {
            let surfaces = app.global::<ShellRuntime>().shell_surfaces();
            surfaces.osd.as_ref().unwrap().1.window_id()
        });
        cx.update(|app| {
            let outcome = app
                .global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .handle_window_closed(window_id);
            assert!(matches!(outcome, WindowClosedOutcome::Nothing));
        });

        // Verify the OSD slot was cleared.
        cx.update(|app| {
            let surfaces = app.global::<ShellRuntime>().shell_surfaces();
            assert!(
                surfaces.osd.is_none(),
                "OSD slot should be None after external close"
            );
            assert_eq!(
                surfaces.osd_lifecycle,
                SurfaceLifecycle::Closed,
                "OSD lifecycle should be Closed"
            );
        });
    }

    #[gpui::test]
    fn overview_slot_cleared_when_window_externally_closed(cx: &mut gpui::TestAppContext) {
        // We cannot easily open a full overview in unit tests, but we can test
        // the struct-level cleanup by inserting a synthetic window handle.
        let raw_handle = cx.add_window(|_, _| LifecycleTestView);
        let handle: WindowHandle<shilpo_m3e::Root> = unsafe { std::mem::transmute(raw_handle) };

        let mut manager = ShellSurfaces::new(Arc::new(CompositorSnapshot::default()));
        let instance = manager.next_overview_instance();
        manager.overview = Some(handle);
        manager.overview_lifecycle = OverviewLifecycle::Open {
            generation: instance,
        };

        let window_id = handle.window_id();
        let outcome = manager.handle_window_closed(window_id);
        assert!(matches!(outcome, WindowClosedOutcome::Nothing));
        assert!(
            manager.overview.is_none(),
            "overview handle should be None after external close"
        );
        assert!(
            manager.overview_entity.is_none(),
            "overview entity should be None after external close"
        );
        assert_eq!(
            manager.overview_lifecycle,
            OverviewLifecycle::Closed,
            "overview lifecycle should be Closed"
        );
    }

    #[gpui::test]
    fn osd_reuse_falls_through_when_window_is_stale(cx: &mut gpui::TestAppContext) {
        cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
        });

        // Open an OSD surface.
        cx.update(|app| {
            ShellSurfaces::request(
                app,
                SurfaceRequest::ShowOsd(crate::osd::OsdKind::Volume {
                    level: 30,
                    muted: false,
                }),
            );
        });

        let first_lifecycle = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        assert!(
            matches!(first_lifecycle, SurfaceLifecycle::Open { .. }),
            "first OSD should be open"
        );

        // Externally close the window through handle_window_closed.
        let window_id = cx.update(|app| {
            let surfaces = app.global::<ShellRuntime>().shell_surfaces();
            surfaces.osd.as_ref().unwrap().1.window_id()
        });
        cx.update(|app| {
            app.global_mut::<ShellRuntime>()
                .shell_surfaces_mut()
                .handle_window_closed(window_id);
        });

        // Now the OSD slot is cleared. The handle_window_closed already
        // cleared the slot, so the next show_osd should create a fresh window.
        cx.run_until_parked();

        // Request a new OSD — the show_osd reuse path should detect the stale
        // slot was already cleared and create a fresh window.
        cx.update(|app| {
            ShellSurfaces::request(
                app,
                SurfaceRequest::ShowOsd(crate::osd::OsdKind::Volume {
                    level: 75,
                    muted: true,
                }),
            );
        });

        let new_lifecycle = cx.update(|app| ShellSurfaces::snapshot(app).osd_lifecycle);
        assert!(
            matches!(new_lifecycle, SurfaceLifecycle::Open { .. }),
            "a fresh OSD should be created after stale window"
        );
    }
}
