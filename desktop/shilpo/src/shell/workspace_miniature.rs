use std::{cmp::Ordering, collections::HashMap, path::PathBuf, sync::Arc};

use gpui::{
    App, ImageSource, InteractiveElement, IntoElement, ObjectFit, ParentElement, RenderOnce, Role,
    SharedString, StatefulInteractiveElement, Styled, StyledImage as _, Window, div, img,
    prelude::FluentBuilder as _, px,
};
use shilpo_m3e::{ActiveTheme, StyledExt};
use shilpo_services::{WindowInfo, WorkspaceInfo};

use crate::shell::app_icons::{app_icon, resolve_app_icon_path};

/// Dimension constants for workspace miniature previews.
pub(crate) const PREVIEW_WIDTH: f32 = 326.0;
pub(crate) const PREVIEW_HEIGHT: f32 = 183.0;
pub(crate) const MAX_MINIATURE_REGIONS: usize = 5;
pub(crate) const APP_ICON_SIZE: f32 = 52.0;

/// Spatial comparison helper for deterministic window ordering.
pub(crate) fn compare_windows_spatially(a: &WindowInfo, b: &WindowInfo) -> Ordering {
    fn compare_f64_opt(a: Option<f64>, b: Option<f64>) -> Ordering {
        match (a, b) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }

    compare_f64_opt(a.layout_x, b.layout_x)
        .then_with(|| compare_f64_opt(a.layout_y, b.layout_y))
        .then_with(|| a.id.cmp(&b.id))
}

/// Helper for window accessibility label generation.
pub(crate) fn window_accessibility_label(title: Option<&str>, app_id: Option<&str>) -> String {
    let clean_title = title.filter(|t| !t.trim().is_empty());
    let clean_app_id = app_id.filter(|a| !a.trim().is_empty());

    match (clean_title, clean_app_id) {
        (Some(t), Some(a)) => format!("{a} - {t}"),
        (Some(t), None) => t.to_string(),
        (None, Some(a)) => a.to_string(),
        (None, None) => "Window".to_string(),
    }
}

/// Helper to calculate distance from a window to the active window for retention ranking.
fn calculate_distance_to_active(
    win: &WindowInfo,
    spatial_idx: usize,
    active_win_id: Option<u64>,
    sorted_windows: &[&WindowInfo],
    active_spatial_idx: Option<usize>,
) -> f64 {
    let Some(active_win_id) = active_win_id else {
        return spatial_idx as f64;
    };

    if win.id == active_win_id {
        return 0.0;
    }

    let active_win = sorted_windows.iter().find(|w| w.id == active_win_id);
    if let (Some(_), Some((ax, ay)), Some((wx, wy))) = (
        active_win,
        active_win.and_then(|a| a.layout_x.zip(a.layout_y)),
        win.layout_x.zip(win.layout_y),
    ) {
        let dx = wx - ax;
        let dy = wy - ay;
        (dx * dx + dy * dy).sqrt()
    } else if let Some(active_idx) = active_spatial_idx {
        (spatial_idx as f64 - active_idx as f64).abs()
    } else {
        spatial_idx as f64
    }
}

/// Read-only projection of a visible window within the miniature.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisibleWindowProjection {
    pub id: u64,
    pub title: SharedString,
    pub app_id: Option<String>,
    pub region_index: usize,
    pub accessibility_label: String,
}

/// Pure data model for a presentation-neutral workspace miniature.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceMiniatureModel {
    workspace_id: u64,
    total_window_count: usize,
    visible_windows: Vec<VisibleWindowProjection>,
    is_active: bool,
    accessibility_label: String,
}

impl WorkspaceMiniatureModel {
    pub(crate) fn new(workspace: &WorkspaceInfo, windows: &[WindowInfo]) -> Self {
        let ws_windows: Vec<&WindowInfo> = windows
            .iter()
            .filter(|w| w.workspace_id == Some(workspace.id))
            .collect();
        let total_window_count = ws_windows.len();

        // 1. Sort all workspace windows spatially
        let mut sorted_windows = ws_windows;
        sorted_windows.sort_by(|a, b| compare_windows_spatially(a, b));

        // Active window ID
        let active_win_id = workspace
            .active_window_id
            .or_else(|| sorted_windows.iter().find(|w| w.is_focused).map(|w| w.id));

        // Calculate visible slots and hidden count
        let visible_slots = total_window_count.min(MAX_MINIATURE_REGIONS);

        // Select visible windows based on priority:
        // Priority 1: Active window
        // Priority 2: Urgent windows
        // Priority 3: Spatially nearest windows to active window
        // Priority 4: Original spatial index
        let active_spatial_idx =
            active_win_id.and_then(|id| sorted_windows.iter().position(|w| w.id == id));

        let mut ranked_windows: Vec<(usize, &WindowInfo)> =
            sorted_windows.iter().copied().enumerate().collect();

        ranked_windows.sort_by(|(idx_a, win_a), (idx_b, win_b)| {
            let is_active_a = active_win_id == Some(win_a.id) || win_a.is_focused;
            let is_active_b = active_win_id == Some(win_b.id) || win_b.is_focused;
            if is_active_a != is_active_b {
                return if is_active_a {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }

            if win_a.is_urgent != win_b.is_urgent {
                return if win_a.is_urgent {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }

            let dist_a = calculate_distance_to_active(
                win_a,
                *idx_a,
                active_win_id,
                &sorted_windows,
                active_spatial_idx,
            );
            let dist_b = calculate_distance_to_active(
                win_b,
                *idx_b,
                active_win_id,
                &sorted_windows,
                active_spatial_idx,
            );

            dist_a
                .partial_cmp(&dist_b)
                .unwrap_or(Ordering::Equal)
                .then_with(|| idx_a.cmp(idx_b))
        });

        // Take top `visible_slots` candidates
        let selected: Vec<(usize, &WindowInfo)> =
            ranked_windows.into_iter().take(visible_slots).collect();

        // Re-sort selected candidates back into original spatial order
        let mut final_visible = selected;
        final_visible.sort_by_key(|(spatial_idx, _)| *spatial_idx);

        let visible_windows: Vec<VisibleWindowProjection> = final_visible
            .into_iter()
            .enumerate()
            .map(|(region_index, (_, win))| {
                let title: SharedString =
                    win.title.as_deref().unwrap_or("Window").to_string().into();
                let acc_label =
                    window_accessibility_label(win.title.as_deref(), win.app_id.as_deref());
                VisibleWindowProjection {
                    id: win.id,
                    title,
                    app_id: win.app_id.clone(),
                    region_index,
                    accessibility_label: acc_label,
                }
            })
            .collect();

        let workspace_name = workspace
            .name
            .clone()
            .unwrap_or_else(|| workspace.idx.to_string());
        let window_count_label = match total_window_count {
            1 => "1 window".to_string(),
            count => format!("{count} windows"),
        };

        let accessibility_label = format!("Workspace {workspace_name}, {window_count_label}");

        Self {
            workspace_id: workspace.id,
            total_window_count,
            visible_windows,
            is_active: workspace.is_active || workspace.is_focused,
            accessibility_label,
        }
    }

    pub(crate) fn visible_windows(&self) -> &[VisibleWindowProjection] {
        &self.visible_windows
    }

    pub(crate) fn region_count(&self) -> usize {
        self.visible_windows.len()
    }

    pub(crate) fn accessibility_label(&self) -> &str {
        &self.accessibility_label
    }
}

/// Non-interactive GPUI visual component for a workspace miniature.
#[derive(IntoElement)]
pub(crate) struct WorkspaceMiniature {
    model: WorkspaceMiniatureModel,
    wallpaper: Option<ImageSource>,
    icon_index: Arc<HashMap<String, PathBuf>>,
    top_radius: f32,
    bottom_radius: f32,
    expose_accessibility: bool,
}

impl WorkspaceMiniature {
    pub(crate) fn new(
        model: &WorkspaceMiniatureModel,
        wallpaper: Option<ImageSource>,
        icon_index: Arc<HashMap<String, PathBuf>>,
    ) -> Self {
        Self {
            model: model.clone(),
            wallpaper,
            icon_index,
            top_radius: 20.0,
            bottom_radius: 20.0,
            expose_accessibility: true,
        }
    }

    pub(crate) fn corner_radii(mut self, top: f32, bottom: f32) -> Self {
        self.top_radius = top;
        self.bottom_radius = bottom;
        self
    }

    /// Avoid duplicate semantics when an interactive host supplies equivalent labels.
    pub(crate) fn accessibility_managed_by_host(mut self) -> Self {
        self.expose_accessibility = false;
        self
    }
}

impl RenderOnce for WorkspaceMiniature {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let top_radius = px(self.top_radius);
        let bottom_radius = px(self.bottom_radius);
        let inner_radius = px(4.0);

        let region_count = self.model.region_count().max(1);
        let is_active = self.model.is_active;

        // Content layer: empty state or the selected window icons.
        let content: gpui::AnyElement = if self.model.total_window_count == 0 {
            div()
                .id(("workspace-miniature-empty", self.model.workspace_id))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .when(self.expose_accessibility, |empty| {
                    empty.role(Role::Status).aria_label("No open windows")
                })
                .child(
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(theme.on_surface_variant.opacity(0.8))
                        .child("No open windows"),
                )
                .into_any_element()
        } else {
            let scale_factor = window.scale_factor();

            let region_elements: Vec<gpui::AnyElement> = self
                .model
                .visible_windows
                .iter()
                .map(|win_proj| {
                    let icon_path =
                        resolve_app_icon_path(win_proj.app_id.as_deref(), self.icon_index.as_ref());
                    let fallback_label = win_proj
                        .app_id
                        .as_deref()
                        .unwrap_or_else(|| win_proj.title.as_ref());

                    let icon_size =
                        ((PREVIEW_WIDTH / region_count as f32) - 24.0).clamp(30.0, APP_ICON_SIZE);

                    let icon = app_icon(
                        icon_path,
                        fallback_label,
                        px(icon_size),
                        scale_factor,
                        theme.surface_container_highest,
                        theme.on_surface,
                    );

                    div()
                        .id(("workspace-miniature-window", win_proj.id))
                        .h_full()
                        .min_w(px(0.0))
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_tl(if win_proj.region_index == 0 {
                            top_radius
                        } else {
                            inner_radius
                        })
                        .rounded_bl(if win_proj.region_index == 0 {
                            bottom_radius
                        } else {
                            inner_radius
                        })
                        .rounded_tr(if win_proj.region_index + 1 == region_count {
                            top_radius
                        } else {
                            inner_radius
                        })
                        .rounded_br(if win_proj.region_index + 1 == region_count {
                            bottom_radius
                        } else {
                            inner_radius
                        })
                        .when(self.expose_accessibility, |region| {
                            region
                                .role(Role::Image)
                                .aria_label(win_proj.accessibility_label.clone())
                        })
                        .child(icon)
                        .into_any_element()
                })
                .collect();

            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .children(region_elements)
                .into_any_element()
        };

        div()
            .id(("workspace-miniature", self.model.workspace_id))
            .w(px(PREVIEW_WIDTH))
            .h(px(PREVIEW_HEIGHT))
            .relative()
            .when(self.expose_accessibility, |preview| {
                preview
                    .role(Role::Image)
                    .aria_label(self.model.accessibility_label.clone())
            })
            .rounded_tl(top_radius)
            .rounded_tr(top_radius)
            .rounded_bl(bottom_radius)
            .rounded_br(bottom_radius)
            .overflow_hidden()
            .flex_none()
            .bg(theme.surface_container_high)
            .when_some(self.wallpaper, |preview, wallpaper_source| {
                preview.child(
                    img(wallpaper_source)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .rounded_tl(top_radius)
                        .rounded_tr(top_radius)
                        .rounded_bl(bottom_radius)
                        .rounded_br(bottom_radius)
                        .object_fit(ObjectFit::Cover),
                )
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_tl(top_radius)
                    .rounded_tr(top_radius)
                    .rounded_bl(bottom_radius)
                    .rounded_br(bottom_radius)
                    .bg(theme.scrim.opacity(if is_active { 0.24 } else { 0.26 })),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_tl(top_radius)
                    .rounded_tr(top_radius)
                    .rounded_bl(bottom_radius)
                    .rounded_br(bottom_radius)
                    .bg(theme.surface.opacity(0.08)),
            )
            .child(content)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_tl(top_radius)
                    .rounded_tr(top_radius)
                    .rounded_bl(bottom_radius)
                    .rounded_br(bottom_radius)
                    .border_1()
                    .when(is_active, |outline| outline.border_2())
                    .border_color(if is_active {
                        theme.primary
                    } else {
                        theme.outline_variant.opacity(0.18)
                    }),
            )
            .shadow_sm()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_workspace(id: u64, idx: u32, name: Option<&str>, is_active: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            id,
            name: name.map(String::from),
            idx,
            is_active,
            is_focused: is_active,
            is_urgent: false,
            output_name: None,
            active_window_id: None,
        }
    }

    fn make_window(
        id: u64,
        ws_id: u64,
        app_id: Option<&str>,
        title: Option<&str>,
        x: Option<f64>,
        y: Option<f64>,
    ) -> WindowInfo {
        WindowInfo {
            id,
            title: title.map(String::from),
            app_id: app_id.map(String::from),
            workspace_id: Some(ws_id),
            is_focused: false,
            is_floating: false,
            is_urgent: false,
            layout_x: x,
            layout_y: y,
        }
    }

    #[test]
    fn test_empty_workspace_model() {
        let ws = make_workspace(1, 1, Some("Main"), true);
        let windows: Vec<WindowInfo> = Vec::new();

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        assert_eq!(model.total_window_count, 0);
        assert_eq!(model.visible_windows().len(), 0);
        assert_eq!(model.region_count(), 0);
        assert!(model.is_active);
        assert_eq!(model.accessibility_label(), "Workspace Main, 0 windows");
    }

    #[test]
    fn test_one_window_workspace_model() {
        let ws = make_workspace(1, 1, None, false);
        let windows = vec![make_window(
            101,
            1,
            Some("firefox"),
            Some("Mozilla Firefox"),
            Some(0.0),
            Some(0.0),
        )];

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        assert_eq!(model.total_window_count, 1);
        assert_eq!(model.visible_windows().len(), 1);
        assert_eq!(model.region_count(), 1);
        assert_eq!(
            model.visible_windows()[0].accessibility_label,
            "firefox - Mozilla Firefox"
        );
    }

    #[test]
    fn test_five_windows_workspace_model() {
        let ws = make_workspace(1, 1, Some("Dev"), false);
        let windows: Vec<WindowInfo> = (1..=5)
            .map(|i| {
                make_window(
                    i,
                    1,
                    Some("terminal"),
                    Some(&format!("Term {i}")),
                    Some(i as f64 * 100.0),
                    Some(0.0),
                )
            })
            .collect();

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        assert_eq!(model.total_window_count, 5);
        assert_eq!(model.visible_windows().len(), 5);
        assert_eq!(model.region_count(), 5);
    }

    #[test]
    fn test_overflow_workspace_model() {
        let ws = make_workspace(1, 1, Some("Overflow"), false);
        let windows: Vec<WindowInfo> = (1..=7)
            .map(|i| {
                make_window(
                    i,
                    1,
                    Some("editor"),
                    Some(&format!("Editor {i}")),
                    Some(i as f64 * 100.0),
                    Some(0.0),
                )
            })
            .collect();

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        assert_eq!(model.total_window_count, 7);
        assert_eq!(model.visible_windows().len(), 5);
        assert_eq!(model.region_count(), 5);
        assert_eq!(model.total_window_count - model.visible_windows().len(), 2);
    }

    #[test]
    fn test_spatial_ordering_complete_and_missing_metadata() {
        let mut w1 = make_window(1, 1, None, None, Some(200.0), Some(0.0));
        let mut w2 = make_window(2, 1, None, None, Some(100.0), Some(0.0));
        assert_eq!(compare_windows_spatially(&w1, &w2), Ordering::Greater);
        assert_eq!(compare_windows_spatially(&w2, &w1), Ordering::Less);

        // Layout coords vs missing
        w1.layout_x = None;
        w1.layout_y = None;

        w2.layout_x = Some(10.0);
        w2.layout_y = Some(0.0);

        assert_eq!(compare_windows_spatially(&w1, &w2), Ordering::Greater);
        assert_eq!(compare_windows_spatially(&w2, &w1), Ordering::Less);

        // Missing metadata tie breaker by ID
        w2.layout_x = None;
        w2.layout_y = None;
        assert_eq!(compare_windows_spatially(&w1, &w2), Ordering::Less);
        assert_eq!(compare_windows_spatially(&w2, &w1), Ordering::Greater);
    }

    #[test]
    fn test_active_window_retention_outside_first_five() {
        let mut ws = make_workspace(1, 1, None, true);
        ws.active_window_id = Some(7);

        let windows: Vec<WindowInfo> = (1..=7)
            .map(|i| {
                make_window(
                    i,
                    1,
                    Some("app"),
                    Some(&format!("App {i}")),
                    Some(i as f64 * 10.0),
                    Some(0.0),
                )
            })
            .collect();

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        assert_eq!(model.total_window_count, 7);

        let visible_ids: Vec<u64> = model.visible_windows().iter().map(|w| w.id).collect();
        assert!(
            visible_ids.contains(&7),
            "Active window (7) must be retained"
        );
    }

    #[test]
    fn test_priority_selection_returns_to_spatial_order() {
        let mut ws = make_workspace(1, 1, None, true);
        ws.active_window_id = Some(4);
        let mut windows: Vec<WindowInfo> = (1..=7)
            .map(|i| {
                make_window(
                    i,
                    1,
                    Some("app"),
                    Some(&format!("App {i}")),
                    Some(i as f64 * 10.0),
                    Some(0.0),
                )
            })
            .collect();
        windows[6].is_urgent = true;

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        let visible_ids: Vec<u64> = model
            .visible_windows()
            .iter()
            .map(|window| window.id)
            .collect();

        assert_eq!(visible_ids, vec![2, 3, 4, 5, 7]);
    }

    #[test]
    fn test_urgent_window_retention_under_crowding() {
        let ws = make_workspace(1, 1, None, false);
        let mut windows: Vec<WindowInfo> = (1..=7)
            .map(|i| {
                make_window(
                    i,
                    1,
                    Some("app"),
                    Some(&format!("App {i}")),
                    Some(i as f64 * 10.0),
                    Some(0.0),
                )
            })
            .collect();

        // Mark window 6 and 7 as urgent
        windows[5].is_urgent = true;
        windows[6].is_urgent = true;

        let model = WorkspaceMiniatureModel::new(&ws, &windows);
        let visible_ids: Vec<u64> = model.visible_windows().iter().map(|w| w.id).collect();
        assert!(
            visible_ids.contains(&6),
            "Urgent window (6) must be retained"
        );
        assert!(
            visible_ids.contains(&7),
            "Urgent window (7) must be retained"
        );

        // When there are 6 urgent windows and max 5 slots.
        for w in &mut windows[1..=6] {
            w.is_urgent = true;
        }
        let crowded_model = WorkspaceMiniatureModel::new(&ws, &windows);
        assert_eq!(crowded_model.visible_windows().len(), 5);
        assert_eq!(
            crowded_model.total_window_count - crowded_model.visible_windows().len(),
            2
        );
    }

    #[test]
    fn test_header_identity_and_count_labels() {
        let ws1 = make_workspace(1, 1, Some("Work"), false);
        let model1 = WorkspaceMiniatureModel::new(&ws1, &[]);
        assert_eq!(model1.accessibility_label(), "Workspace Work, 0 windows");

        let ws2 = make_workspace(2, 3, None, false);
        let win = make_window(10, 2, None, None, None, None);
        let model2 = WorkspaceMiniatureModel::new(&ws2, &[win]);
        assert_eq!(model2.accessibility_label(), "Workspace 3, 1 window");
    }

    #[test]
    fn test_accessibility_labels() {
        assert_eq!(
            window_accessibility_label(Some("Terminal"), Some("org.gnome.Terminal")),
            "org.gnome.Terminal - Terminal"
        );
        assert_eq!(
            window_accessibility_label(Some("Untitled"), None),
            "Untitled"
        );
        assert_eq!(window_accessibility_label(None, Some("firefox")), "firefox");
        assert_eq!(window_accessibility_label(None, None), "Window");
    }

    #[test]
    fn test_independent_construction() {
        let ws = make_workspace(1, 1, Some("Independent"), true);
        let win = make_window(1, 1, Some("app"), Some("App"), None, None);
        let model = WorkspaceMiniatureModel::new(&ws, &[win]);
        let icon_index = Arc::new(HashMap::new());

        let _miniature = WorkspaceMiniature::new(&model, None, icon_index)
            .accessibility_managed_by_host()
            .corner_radii(16.0, 16.0);
    }
}
