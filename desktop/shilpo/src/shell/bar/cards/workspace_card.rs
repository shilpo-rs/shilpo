use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use gpui::{
    AnyElement, App, ImageSource, IntoElement, ParentElement, Pixels, Size, Styled, Window, div, px,
};
use shilpo_m3e::{ActiveTheme, Icon, IconName, StyledExt};
use shilpo_services::{Application, DomainLifecycle, WorkspaceInfo};

use super::{
    model::{CardCapabilities, CardChannel, CardOwnerId, CardSourceId},
    provider::CardProvider,
};
use crate::{
    runtime::{ShellRuntime, ShellSurfaces},
    workspace_miniature::{
        PREVIEW_HEIGHT, PREVIEW_WIDTH, WorkspaceMiniature, WorkspaceMiniatureModel,
    },
};

/// Built-in card provider for Workspace hover previews.
pub(crate) struct WorkspacePreviewProvider {
    icon_cache: Mutex<WorkspaceIconCache>,
}

struct WorkspaceIconCache {
    applications: Vec<Application>,
    index: Arc<HashMap<String, PathBuf>>,
}

pub(crate) const WORKSPACE_CARD_OWNER: &str = "workspaces";

pub(crate) fn workspace_owner_id() -> CardOwnerId {
    CardOwnerId::new(WORKSPACE_CARD_OWNER)
}

pub(crate) fn workspace_source(
    instance_id: impl Into<gpui::SharedString>,
    workspace_id: u64,
) -> CardSourceId {
    CardSourceId::new(
        workspace_owner_id(),
        instance_id,
        Some(workspace_id.to_string()),
    )
}

impl WorkspacePreviewProvider {
    pub(crate) fn new() -> Self {
        Self {
            icon_cache: Mutex::new(WorkspaceIconCache {
                applications: Vec::new(),
                index: Arc::new(HashMap::new()),
            }),
        }
    }

    fn icon_index(&self, cx: &App) -> Arc<HashMap<String, PathBuf>> {
        let applications = ShellSurfaces::overview_applications(cx);
        let mut cache = self
            .icon_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cache.applications != applications {
            cache.index = Arc::new(crate::app_icons::build_app_icon_index(applications.clone()));
            cache.applications = applications;
        }
        cache.index.clone()
    }
}

impl CardProvider for WorkspacePreviewProvider {
    fn owner_id(&self) -> CardOwnerId {
        workspace_owner_id()
    }

    fn capabilities(&self) -> CardCapabilities {
        CardCapabilities {
            hover: true,
            click: false,
            needs_focus: true,
        }
    }

    fn preferred_size(
        &self,
        _channel: CardChannel,
        _source: &CardSourceId,
        _cx: &App,
    ) -> Size<Pixels> {
        Size {
            width: px(PREVIEW_WIDTH),
            height: px(PREVIEW_HEIGHT),
        }
    }

    fn source_available(&self, source: &CardSourceId, cx: &App) -> bool {
        let snapshot = ShellSurfaces::compositor_snapshot(cx);
        if !matches!(snapshot.connection, DomainLifecycle::Ready) {
            return true;
        }

        source
            .content_key
            .as_ref()
            .and_then(|key| key.parse::<u64>().ok())
            .is_some_and(|workspace_id| {
                snapshot
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == workspace_id)
            })
    }

    fn render_content(
        &self,
        _channel: CardChannel,
        source: &CardSourceId,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let target_ws_id = source
            .content_key
            .as_ref()
            .and_then(|k| k.parse::<u64>().ok());

        let Some(ws_id) = target_ws_id else {
            return div().into_any_element();
        };

        let snapshot = ShellSurfaces::compositor_snapshot(cx);
        let connection = snapshot.connection;

        let wallpaper_snapshot = if cx.has_global::<ShellRuntime>() {
            ShellRuntime::wallpaper_preview_snapshot(cx)
        } else {
            crate::runtime::WallpaperPreviewSnapshot::Empty
        };
        let wallpaper_source: Option<ImageSource> =
            wallpaper_snapshot.ready_image().map(ImageSource::from);

        let icon_index = self.icon_index(cx);

        let matching_ws: Option<&WorkspaceInfo> =
            snapshot.workspaces.iter().find(|ws| ws.id == ws_id);

        match (matching_ws, &connection) {
            (Some(workspace), connection) => {
                let model = WorkspaceMiniatureModel::new(workspace, &snapshot.windows);
                let miniature = WorkspaceMiniature::new(&model, wallpaper_source, icon_index)
                    .corner_radii(24.0, 24.0);

                let status_overlay = match connection {
                    DomainLifecycle::Reconnecting => Some("Reconnecting".to_string()),
                    DomainLifecycle::Unavailable | DomainLifecycle::Degraded => {
                        Some("Compositor Unavailable".to_string())
                    }
                    _ => None,
                };

                if let Some(status_label) = status_overlay {
                    div()
                        .relative()
                        .w(px(PREVIEW_WIDTH))
                        .h(px(PREVIEW_HEIGHT))
                        .child(miniature)
                        .child(
                            div()
                                .absolute()
                                .bottom_2()
                                .right_2()
                                .px_2()
                                .py_1()
                                .rounded_full()
                                .bg(cx.theme().surface_container_highest.opacity(0.85))
                                .text_color(cx.theme().on_surface_variant)
                                .text_xs()
                                .font_semibold()
                                .child(status_label),
                        )
                        .into_any_element()
                } else {
                    div()
                        .w(px(PREVIEW_WIDTH))
                        .h(px(PREVIEW_HEIGHT))
                        .child(miniature)
                        .into_any_element()
                }
            }
            (None, DomainLifecycle::Ready) => {
                // The coordinator reconciles authoritative source removal before rendering.
                div().into_any_element()
            }
            (None, connection) => {
                // Compositor disconnected / stopped and no workspace cached.
                let status_text = match connection {
                    DomainLifecycle::Connecting => "Connecting to compositor...",
                    DomainLifecycle::Reconnecting => "Reconnecting to compositor...",
                    DomainLifecycle::Unavailable | DomainLifecycle::Degraded => {
                        "Compositor unavailable"
                    }
                    DomainLifecycle::Ready => "Workspace unavailable",
                };

                div()
                    .w(px(PREVIEW_WIDTH))
                    .h(px(PREVIEW_HEIGHT))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_4()
                    .bg(cx.theme().surface_container_low)
                    .text_color(cx.theme().on_surface_variant)
                    .child(Icon::new(IconName::Info).size(px(24.)))
                    .child(div().text_xs().font_medium().child(status_text))
                    .into_any_element()
            }
        }
    }
}
