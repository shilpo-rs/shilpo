use std::cmp::Ordering;
use std::{collections::HashMap, path::PathBuf, sync::Arc};

use gpui::{
    Animation, AnimationExt as _, App, ElementId, InteractiveElement, IntoElement, MouseButton,
    ObjectFit, ParentElement, RenderOnce, Role, ScrollWheelEvent, StatefulInteractiveElement,
    StyleRefinement, Styled, StyledImage, Window, div, img, px,
};
use shilpo_m3e::{ActiveTheme, StyledExt};
use shilpo_services::{CompositorSnapshot, WindowInfo};

use crate::shell::actions::ActionInvocation;
use crate::shell::bar::widgets::pill_strip::{
    PILL_SLOT_SIZE, PillOrientation, render_active_pill_indicator,
};
use crate::shell::{
    app_icons::{icon_device_pixels, rasterized_app_icon, resolve_app_icon_path},
    runtime::ShellRuntime,
};

const ICON_SIZE: f32 = 18.;

/// Widget displaying a workspace-style running apps pill per monitor.
#[derive(IntoElement)]
pub struct RunningAppsWidget {
    id: ElementId,
    output_name: Option<String>,
    snapshot: CompositorSnapshot,
    app_icons: Arc<HashMap<String, PathBuf>>,
    orientation: PillOrientation,
    reduced_motion: bool,
    style: StyleRefinement,
}

impl RunningAppsWidget {
    pub fn new(
        id: impl Into<ElementId>,
        output_name: Option<String>,
        snapshot: CompositorSnapshot,
        app_icons: Arc<HashMap<String, PathBuf>>,
        orientation: PillOrientation,
        reduced_motion: bool,
    ) -> Self {
        Self {
            id: id.into(),
            output_name,
            snapshot,
            app_icons,
            orientation,
            reduced_motion,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for RunningAppsWidget {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

fn sort_windows_spatially(windows: &mut [WindowInfo]) {
    windows.sort_by(|a, b| {
        let a_visual = a.layout_x.zip(a.layout_y);
        let b_visual = b.layout_x.zip(b.layout_y);
        match (a_visual, b_visual) {
            (Some((ax, ay)), Some((bx, by))) => ax
                .total_cmp(&bx)
                .then_with(|| ay.total_cmp(&by))
                .then_with(|| a.id.cmp(&b.id)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.id.cmp(&b.id),
        }
    });
}

fn app_icon_element(
    icon_path: Option<PathBuf>,
    fallback: &str,
    icon_size: f32,
    scale_factor: f32,
    cx: &App,
) -> gpui::AnyElement {
    if let Some(path) = icon_path {
        let target_size = icon_device_pixels(icon_size, scale_factor);
        let image = rasterized_app_icon(&path, target_size)
            .map(img)
            .unwrap_or_else(|| img(gpui::ImageSource::from(path)));
        div()
            .w(px(icon_size))
            .h(px(icon_size))
            .flex()
            .items_center()
            .justify_center()
            .child(
                image
                    .w(px(icon_size))
                    .h(px(icon_size))
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element()
    } else {
        let initial = fallback
            .chars()
            .find(|character| character.is_alphanumeric())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        div()
            .w(px(icon_size))
            .h(px(icon_size))
            .rounded_full()
            .bg(cx.theme().secondary_container)
            .text_color(cx.theme().on_secondary_container)
            .text_xs()
            .font_bold()
            .flex()
            .items_center()
            .justify_center()
            .child(initial)
            .into_any_element()
    }
}

fn resolve_icon_path(
    app_id: Option<&str>,
    app_icons: &HashMap<String, PathBuf>,
) -> Option<PathBuf> {
    resolve_app_icon_path(app_id, app_icons)
}

fn active_workspace_for_output<'a>(
    snapshot: &'a CompositorSnapshot,
    output_name: Option<&str>,
) -> Option<&'a shilpo_services::WorkspaceInfo> {
    match output_name {
        Some(output) => snapshot
            .workspaces
            .iter()
            .find(|workspace| {
                workspace.output_name.as_deref() == Some(output) && workspace.is_active
            })
            .or_else(|| {
                let active = snapshot
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.is_active)
                    .collect::<Vec<_>>();
                (active.len() == 1).then(|| active[0])
            }),
        None => snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.is_active),
    }
}

#[derive(Clone, Copy, Default)]
struct UrgencyState {
    active: bool,
    generation: u64,
}

fn urgency_generation(window: &mut Window, cx: &mut App, window_id: u64, urgent: bool) -> u64 {
    let state = window.use_keyed_state(format!("running-app-urgency:{window_id}"), cx, |_, _| {
        UrgencyState::default()
    });
    if state.read(cx).active != urgent {
        state.update(cx, |state, _| {
            if urgent {
                state.generation = state.generation.wrapping_add(1).max(1);
            }
            state.active = urgent;
        });
    }
    state.read(cx).generation
}

const MAX_VISIBLE_WINDOWS: usize = 5;

fn viewport_start_for_target(current: usize, target: usize, count: usize) -> usize {
    let max_start = count.saturating_sub(MAX_VISIBLE_WINDOWS);
    let current = current.min(max_start);
    if target < current {
        target
    } else if target >= current + MAX_VISIBLE_WINDOWS {
        (target + 1 - MAX_VISIBLE_WINDOWS).min(max_start)
    } else {
        current
    }
}

impl RenderOnce for RunningAppsWidget {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let active_workspace =
            active_workspace_for_output(&self.snapshot, self.output_name.as_deref());

        let Some(active_ws) = active_workspace else {
            return div().into_any_element();
        };

        let mut workspace_windows: Vec<WindowInfo> = self
            .snapshot
            .windows
            .iter()
            .filter(|w| w.workspace_id == Some(active_ws.id))
            .cloned()
            .collect();

        if workspace_windows.is_empty() {
            return div().into_any_element();
        }

        sort_windows_spatially(&mut workspace_windows);

        let active_window_id = active_ws.active_window_id.or_else(|| {
            self.snapshot
                .focused_window_id
                .filter(|fid| workspace_windows.iter().any(|w| w.id == *fid))
        });

        let active_window_idx =
            active_window_id.and_then(|id| workspace_windows.iter().position(|w| w.id == id));

        let total_windows = workspace_windows.len();
        let viewport_size = MAX_VISIBLE_WINDOWS;

        let viewport_state_key = format!("running-apps-viewport:{}", self.id);
        let viewport_start_cell = window.use_keyed_state(viewport_state_key, cx, |_, _| 0usize);
        let mut viewport_start = *viewport_start_cell.read(cx);

        if total_windows <= viewport_size {
            viewport_start = 0;
        } else if let Some(active_idx) = active_window_idx {
            viewport_start = viewport_start_for_target(viewport_start, active_idx, total_windows);
        } else {
            viewport_start = viewport_start.min(total_windows - viewport_size);
        }

        viewport_start_cell.update(cx, |val, _| *val = viewport_start);

        let visible_windows =
            &workspace_windows[viewport_start..(viewport_start + viewport_size).min(total_windows)];

        let active_slot_idx = active_window_idx.and_then(|idx| idx.checked_sub(viewport_start));

        let active_indicator = render_active_pill_indicator(
            &self.id,
            active_slot_idx,
            self.orientation,
            self.reduced_motion,
            window,
            cx,
        );

        let window_elements: Vec<gpui::AnyElement> = visible_windows
            .iter()
            .enumerate()
            .map(|(slot_idx, win)| {
                let win_id = win.id;
                let title = win.title.as_deref().unwrap_or("Window");
                let icon_path = resolve_icon_path(win.app_id.as_deref(), &self.app_icons);
                let fallback_label = win.app_id.as_deref().unwrap_or(title);

                let icon = app_icon_element(
                    icon_path,
                    fallback_label,
                    ICON_SIZE,
                    window.scale_factor(),
                    cx,
                );

                let is_slot_active = active_slot_idx == Some(slot_idx);
                let is_urgent = win.is_urgent;
                let urgency_generation = urgency_generation(window, cx, win_id, is_urgent);

                let slot = div()
                    .id(("run_app_slot", win_id))
                    .role(Role::Button)
                    .aria_label(format!("Window {}", title))
                    .w(px(PILL_SLOT_SIZE))
                    .h(px(PILL_SLOT_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| if is_slot_active { s } else { s.opacity(0.8) })
                    .on_click(move |_, _, cx| {
                        let _ = ShellRuntime::dispatch_action(
                            cx,
                            ActionInvocation::FocusWindow(win_id),
                        );
                    })
                    .on_mouse_down(MouseButton::Middle, move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = ShellRuntime::dispatch_action(
                            cx,
                            ActionInvocation::CloseWindow(win_id),
                        );
                    })
                    .child(icon);

                if is_urgent {
                    let error_color = cx.theme().error;
                    if self.reduced_motion {
                        slot.rounded_full()
                            .border_2()
                            .border_color(error_color)
                            .into_any_element()
                    } else {
                        let anim_id = format!("urgent-pulse:{}", win_id);
                        slot.rounded_full()
                            .border_2()
                            .border_color(error_color)
                            .with_animation(
                                ElementId::NamedInteger(anim_id.into(), urgency_generation),
                                Animation::new(std::time::Duration::from_millis(750)),
                                move |slot, delta| {
                                    let pulse =
                                        ((delta * std::f32::consts::TAU * 3.0).sin() + 1.0) / 2.0;
                                    let opacity = 0.4 + 0.6 * pulse;
                                    slot.border_color(error_color.opacity(opacity))
                                },
                            )
                            .into_any_element()
                    }
                } else {
                    slot.into_any_element()
                }
            })
            .collect();

        let all_window_ids: Vec<u64> = workspace_windows.iter().map(|w| w.id).collect();
        let current_active_idx = active_window_idx;

        let container = div()
            .id(self.id)
            .relative()
            .flex()
            .items_center()
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _, cx| {
                cx.stop_propagation();
                let (is_prev, has_movement) = match event.delta {
                    gpui::ScrollDelta::Lines(pos) => {
                        let val = if pos.x.abs() > pos.y.abs() {
                            pos.x
                        } else {
                            pos.y
                        };
                        (val > 0.0, val != 0.0)
                    }
                    gpui::ScrollDelta::Pixels(pos) => {
                        let px_val: f32 = if pos.x.abs() > pos.y.abs() {
                            pos.x.into()
                        } else {
                            pos.y.into()
                        };
                        (px_val > 0.0, px_val != 0.0)
                    }
                };

                if !has_movement || all_window_ids.is_empty() {
                    return;
                }

                let cur_idx = current_active_idx.unwrap_or(0);
                let target_idx = if is_prev {
                    cur_idx.saturating_sub(1)
                } else {
                    (cur_idx + 1).min(all_window_ids.len() - 1)
                };

                if target_idx != cur_idx {
                    let target_win_id = all_window_ids[target_idx];
                    let _ = ShellRuntime::dispatch_action(
                        cx,
                        ActionInvocation::FocusWindow(target_win_id),
                    );
                }
            })
            .child(active_indicator)
            .rounded_full()
            .bg(cx.theme().secondary_container.opacity(0.6));

        match self.orientation {
            PillOrientation::Horizontal => container
                .flex_row()
                .h(px(PILL_SLOT_SIZE))
                .children(window_elements)
                .into_any_element(),
            PillOrientation::Vertical => container
                .flex_col()
                .w(px(PILL_SLOT_SIZE))
                .children(window_elements)
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_window(id: u64, x: Option<f64>, y: Option<f64>) -> WindowInfo {
        WindowInfo {
            id,
            title: Some(format!("Win {id}")),
            app_id: Some("foot".into()),
            workspace_id: Some(1),
            is_focused: false,
            is_floating: false,
            is_urgent: false,
            layout_x: x,
            layout_y: y,
        }
    }

    #[test]
    fn test_spatial_window_sorting() {
        let mut windows = vec![
            mock_window(3, Some(100.0), Some(20.0)),
            mock_window(1, Some(10.0), Some(50.0)),
            mock_window(2, Some(10.0), Some(10.0)),
        ];

        sort_windows_spatially(&mut windows);

        assert_eq!(windows[0].id, 2);
        assert_eq!(windows[1].id, 1);
        assert_eq!(windows[2].id, 3);
    }

    #[test]
    fn test_spatial_window_sorting_fallback_to_id() {
        let mut windows = vec![
            mock_window(10, None, None),
            mock_window(5, None, None),
            mock_window(7, None, None),
        ];

        sort_windows_spatially(&mut windows);

        assert_eq!(windows[0].id, 5);
        assert_eq!(windows[1].id, 7);
        assert_eq!(windows[2].id, 10);
    }

    #[test]
    fn test_viewport_minimal_movement_logic() {
        assert_eq!(viewport_start_for_target(0, 9, 10), 5);
        assert_eq!(viewport_start_for_target(5, 8, 10), 5);
        assert_eq!(viewport_start_for_target(5, 1, 10), 1);
        assert_eq!(viewport_start_for_target(99, 8, 10), 5);
    }

    #[test]
    fn spatial_sort_is_total_with_partial_and_non_finite_coordinates() {
        let mut windows = vec![
            mock_window(3, None, Some(2.0)),
            mock_window(1, Some(f64::NAN), Some(0.0)),
            mock_window(2, Some(1.0), Some(0.0)),
        ];
        sort_windows_spatially(&mut windows);
        assert_eq!(
            windows.iter().map(|window| window.id).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
    }

    #[test]
    fn active_workspace_selection_stays_on_requested_output() {
        let snapshot = CompositorSnapshot {
            workspaces: vec![
                shilpo_services::WorkspaceInfo {
                    id: 1,
                    name: None,
                    idx: 1,
                    is_active: true,
                    is_focused: true,
                    is_urgent: false,
                    output_name: Some("eDP-1".into()),
                    active_window_id: None,
                },
                shilpo_services::WorkspaceInfo {
                    id: 2,
                    name: None,
                    idx: 1,
                    is_active: true,
                    is_focused: false,
                    is_urgent: false,
                    output_name: Some("HDMI-A-1".into()),
                    active_window_id: None,
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            active_workspace_for_output(&snapshot, Some("HDMI-A-1")).map(|ws| ws.id),
            Some(2)
        );
        assert_eq!(
            active_workspace_for_output(&snapshot, Some("missing")),
            None
        );

        let mut single_output_snapshot = snapshot.clone();
        single_output_snapshot.workspaces.truncate(1);
        assert_eq!(
            active_workspace_for_output(&single_output_snapshot, Some("missing")).map(|ws| ws.id),
            Some(1)
        );
    }
}
