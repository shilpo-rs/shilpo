use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, px, relative,
};
use shilpo_m3e::{ActiveTheme, Icon, IconName, StyledExt, black, green_500, h_flex};
use shilpo_services::BatteryInfo;

use crate::shell::bar::cards::{
    adapter::CardCoordinator,
    model::{CardRequest, CardSourceId, CardSourceState},
};

// Android SystemUI renders the unified battery at 20.6 × 12 in the phone status
// bar. Shilpo's desktop bar and neighboring icons are larger, so scale that
// geometry up slightly while preserving its proportions.
const BATTERY_DESKTOP_SCALE: f32 = 1.5;
const BATTERY_BODY_WIDTH: f32 = 17.6 * BATTERY_DESKTOP_SCALE;
const BATTERY_BODY_HEIGHT: f32 = 10.7 * BATTERY_DESKTOP_SCALE;
const BATTERY_TERMINAL_WIDTH: f32 = 3.0;
const BATTERY_TERMINAL_HEIGHT: f32 = 7.0;
const BATTERY_TERMINAL_GAP: f32 = 1.2;
const BATTERY_CORNER_RADIUS: f32 = 3.6 * BATTERY_DESKTOP_SCALE;
const BATTERY_PERCENT_TEXT_SIZE: f32 = 8.6 * BATTERY_DESKTOP_SCALE;
const BATTERY_CHARGING_TEXT_SIZE: f32 = 7.7 * BATTERY_DESKTOP_SCALE;
const BATTERY_CHARGING_THREE_DIGIT_TEXT_SIZE: f32 = 5.15 * BATTERY_DESKTOP_SCALE;
const BATTERY_CHARGING_ICON_SIZE: f32 = 5.15 * BATTERY_DESKTOP_SCALE;
const BATTERY_INDICATOR_WIDTH: f32 = 48.0;
const BATTERY_INDICATOR_HEIGHT: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatteryVisualMode {
    Normal,
    Low,
    Charging,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BatteryVisualState {
    percentage: u8,
    fill_ratio: f32,
    mode: BatteryVisualMode,
}

impl BatteryVisualState {
    fn from_info(info: &BatteryInfo) -> Option<Self> {
        if !info.is_present {
            return None;
        }

        let percentage = info.percentage.min(100);
        let mode = if info.is_charging() {
            BatteryVisualMode::Charging
        } else if info.is_low_battery() {
            BatteryVisualMode::Low
        } else {
            BatteryVisualMode::Normal
        };

        Some(Self {
            percentage,
            fill_ratio: percentage as f32 / 100.0,
            mode,
        })
    }
}

/// Shared Pixel-style battery indicator for the Shilpo shell bar.
#[derive(IntoElement)]
pub(crate) struct BatteryIndicator {
    id: ElementId,
    info: BatteryInfo,
    display_id: Option<gpui::DisplayId>,
    style: StyleRefinement,
}

impl BatteryIndicator {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        info: BatteryInfo,
        display_id: Option<gpui::DisplayId>,
    ) -> Self {
        Self {
            id: id.into(),
            info,
            display_id,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for BatteryIndicator {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for BatteryIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(state) = BatteryVisualState::from_info(&self.info) else {
            return div().id(self.id).into_any_element();
        };

        let source = CardSourceId::singleton("battery");
        let is_selected =
            CardCoordinator::source_state(cx, &source) == CardSourceState::PersistentOpen;
        let (fill_color, filled_content_color) = match state.mode {
            BatteryVisualMode::Normal if is_selected => (
                cx.theme().on_secondary_container,
                cx.theme().secondary_container,
            ),
            BatteryVisualMode::Normal => (cx.theme().on_surface, cx.theme().surface),
            BatteryVisualMode::Low => (cx.theme().error, cx.theme().on_error),
            BatteryVisualMode::Charging => (green_500(), black()),
        };
        let track_color = if is_selected {
            cx.theme().on_secondary_container.opacity(0.18)
        } else {
            cx.theme().surface_container_highest
        };
        let nub_color = if state.percentage == 100 {
            if is_selected {
                cx.theme().on_secondary_container
            } else {
                cx.theme().on_surface
            }
        } else {
            track_color
        };
        let fill_width = px(BATTERY_BODY_WIDTH * state.fill_ratio);
        let fill_right_radius = if state.percentage >= 80 {
            px(BATTERY_CORNER_RADIUS)
        } else {
            px(0.)
        };
        let is_charging = state.mode == BatteryVisualMode::Charging;
        let text_size = if is_charging {
            if state.percentage == 100 {
                BATTERY_CHARGING_THREE_DIGIT_TEXT_SIZE
            } else {
                BATTERY_CHARGING_TEXT_SIZE
            }
        } else {
            BATTERY_PERCENT_TEXT_SIZE
        };

        let render_content = move |text_color: gpui::Hsla| {
            let mut content = h_flex()
                .items_center()
                .justify_center()
                .gap(px(0.3))
                .text_color(text_color)
                .child(
                    div()
                        .font_family("Noto Sans Black")
                        .text_size(px(text_size))
                        .line_height(relative(1.))
                        .font_weight(gpui::FontWeight::BLACK)
                        .child(state.percentage.to_string()),
                );

            if is_charging {
                content = content.child(
                    Icon::new(IconName::BoltFill)
                        .size(px(BATTERY_CHARGING_ICON_SIZE))
                        .text_color(text_color),
                );
            }

            content
        };

        let body = div()
            .relative()
            .w(px(BATTERY_BODY_WIDTH))
            .h(px(BATTERY_BODY_HEIGHT))
            .rounded(px(BATTERY_CORNER_RADIUS))
            .bg(track_color)
            .overflow_hidden()
            // Unfilled base layer (track background, unfilled text color)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(render_content(cx.theme().on_surface)),
            )
            // Filled overlay layer (clipped to fill_width, fill background, filled text color)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(fill_width)
                    .rounded_tl(px(BATTERY_CORNER_RADIUS))
                    .rounded_bl(px(BATTERY_CORNER_RADIUS))
                    .rounded_tr(fill_right_radius)
                    .rounded_br(fill_right_radius)
                    .bg(fill_color)
                    .overflow_hidden()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(BATTERY_BODY_WIDTH))
                            .h(px(BATTERY_BODY_HEIGHT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(render_content(filled_content_color)),
                    ),
            );

        let nub = div()
            .w(px(BATTERY_TERMINAL_WIDTH))
            .h(px(BATTERY_TERMINAL_HEIGHT))
            .rounded(px(BATTERY_TERMINAL_WIDTH / 2.))
            .bg(nub_color);

        let source_prepaint = source.clone();
        let display_id = self.display_id;
        h_flex()
            .on_children_prepainted(move |child_bounds, _window, cx| {
                // Layer-shell bar windows don't reliably expose their output through
                // `Window::display` (see WorkspacesWidget) -- use the display id the
                // owning BarView already knows instead of querying it here.
                let Some(display_id) = display_id else {
                    return;
                };
                let Some(bounds) = child_bounds
                    .into_iter()
                    .reduce(|bounds, child| bounds.union(&child))
                else {
                    return;
                };

                CardCoordinator::dispatch(
                    cx,
                    CardRequest::AnchorUpdate {
                        source: source_prepaint.clone(),
                        bounds,
                        display_id,
                    },
                );
            })
            .id(self.id)
            .role(Role::Button)
            .aria_label("Battery status")
            .w(px(BATTERY_INDICATOR_WIDTH))
            .h(px(BATTERY_INDICATOR_HEIGHT))
            .rounded_full()
            .items_center()
            .justify_center()
            .gap(px(BATTERY_TERMINAL_GAP))
            .cursor_pointer()
            .bg(if is_selected {
                cx.theme().secondary_container
            } else {
                gpui::transparent_black()
            })
            .hover(|style| {
                style.bg(if is_selected {
                    cx.theme().secondary_container
                } else {
                    cx.theme().surface_container_high
                })
            })
            .on_click(move |_, _, cx| {
                CardCoordinator::dispatch(
                    cx,
                    CardRequest::PersistentToggle {
                        source: source.clone(),
                    },
                );
            })
            .refine_style(&self.style)
            .child(body)
            .child(nub)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use shilpo_services::BatteryChargeState;

    use super::*;

    #[test]
    fn absent_battery_has_no_visual_state() {
        assert_eq!(BatteryVisualState::from_info(&BatteryInfo::default()), None);
    }

    #[test]
    fn normal_percentage_and_fill_are_derived_from_production_state() {
        let state = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 60,
            state: BatteryChargeState::Discharging,
            ..Default::default()
        })
        .expect("present battery");

        assert_eq!(state.percentage, 60);
        assert_eq!(state.fill_ratio, 0.6);
        assert_eq!(state.mode, BatteryVisualMode::Normal);
    }

    #[test]
    fn empty_and_full_batteries_map_to_fill_bounds() {
        let empty = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 0,
            state: BatteryChargeState::Empty,
            ..Default::default()
        })
        .expect("present battery");
        let full = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 100,
            state: BatteryChargeState::FullyCharged,
            ..Default::default()
        })
        .expect("present battery");

        assert_eq!(empty.fill_ratio, 0.0);
        assert_eq!(full.fill_ratio, 1.0);
    }

    #[test]
    fn low_battery_threshold_uses_battery_service_policy() {
        let low = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 14,
            state: BatteryChargeState::Discharging,
            ..Default::default()
        })
        .expect("present battery");
        let normal = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 15,
            state: BatteryChargeState::Discharging,
            ..Default::default()
        })
        .expect("present battery");

        assert_eq!(low.mode, BatteryVisualMode::Low);
        assert_eq!(normal.mode, BatteryVisualMode::Normal);
    }

    #[test]
    fn charging_takes_precedence_over_low_battery() {
        let charging = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 58,
            state: BatteryChargeState::Charging,
            ..Default::default()
        })
        .expect("present battery");
        let charging_low = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 10,
            state: BatteryChargeState::Charging,
            ..Default::default()
        })
        .expect("present battery");

        assert_eq!(charging.percentage, 58);
        assert_eq!(charging.fill_ratio, 0.58);
        assert_eq!(charging.mode, BatteryVisualMode::Charging);
        assert_eq!(charging_low.mode, BatteryVisualMode::Charging);
    }

    #[test]
    fn malformed_percentage_is_clamped() {
        let state = BatteryVisualState::from_info(&BatteryInfo {
            available: true,
            is_present: true,
            percentage: 150,
            state: BatteryChargeState::Discharging,
            ..Default::default()
        })
        .expect("present battery");

        assert_eq!(state.percentage, 100);
        assert_eq!(state.fill_ratio, 1.0);
    }
}
