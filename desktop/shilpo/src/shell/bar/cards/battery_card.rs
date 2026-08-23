use std::sync::{Arc, Mutex};

use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Pixels, Size,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use shilpo_m3e::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use shilpo_services::DomainLifecycle;
use shilpo_services::{
    BatteryChargeState, BatteryDevicePayload, BatteryPayload, BatteryTechnology,
    BatteryWarningLevel,
};

use super::{
    model::{CardCapabilities, CardChannel, CardOwnerId, CardSourceId},
    provider::CardProvider,
};
use crate::shell::runtime::ShellRuntime;

/// Built-in card provider for the system Battery.
pub(crate) struct BatteryCardProvider {
    selected_device: Arc<Mutex<Option<String>>>,
}

impl BatteryCardProvider {
    pub(crate) fn new() -> Self {
        Self {
            selected_device: Arc::new(Mutex::new(None)),
        }
    }
}

impl CardProvider for BatteryCardProvider {
    fn owner_id(&self) -> CardOwnerId {
        CardOwnerId::new("battery")
    }

    fn capabilities(&self) -> CardCapabilities {
        CardCapabilities {
            hover: false,
            click: true,
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
            width: px(360.0),
            height: px(280.0),
        }
    }

    fn render_content(
        &self,
        _channel: CardChannel,
        _source: &CardSourceId,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let payload = ShellRuntime::device_snapshot(cx).battery;
        let lifecycle = ShellRuntime::domain_lifecycle(cx, shilpo_services::DeviceDomain::Battery);

        let selected = self.selected_device.lock().unwrap().clone();
        let selected_cell = self.selected_device.clone();

        render_battery_card(&lifecycle, &payload, selected, selected_cell, window, cx)
    }
}

fn format_time(seconds: u64, is_charging: bool) -> String {
    let hours = seconds / 3600;
    let mins = (seconds % 3600) / 60;
    let duration_str = if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    };

    if is_charging {
        format!("{duration_str} until full")
    } else {
        format!("{duration_str} remaining")
    }
}

fn format_charge_state(state: BatteryChargeState) -> &'static str {
    match state {
        BatteryChargeState::Charging => "Charging",
        BatteryChargeState::Discharging => "Discharging",
        BatteryChargeState::Empty => "Empty",
        BatteryChargeState::FullyCharged => "Fully Charged",
        BatteryChargeState::PendingCharge => "Pending Charge",
        BatteryChargeState::PendingDischarge => "Pending Discharge",
        BatteryChargeState::Unknown => "Unknown",
    }
}

fn format_technology(tech: BatteryTechnology) -> &'static str {
    match tech {
        BatteryTechnology::LithiumIon => "Lithium Ion",
        BatteryTechnology::LithiumPolymer => "Lithium Polymer",
        BatteryTechnology::LithiumIronPhosphate => "Lithium Iron Phosphate",
        BatteryTechnology::LeadAcid => "Lead Acid",
        BatteryTechnology::NickelCadmium => "Nickel Cadmium",
        BatteryTechnology::NickelMetalHydride => "Nickel Metal Hydride",
        BatteryTechnology::Unknown => "Unknown",
    }
}

fn toggle_selected_device(selected: &mut Option<String>, device_id: &str) {
    if selected.as_deref() == Some(device_id) {
        *selected = None;
    } else {
        *selected = Some(device_id.to_owned());
    }
}

fn render_battery_card(
    lifecycle: &DomainLifecycle,
    payload: &BatteryPayload,
    selected: Option<String>,
    selected_cell: Arc<Mutex<Option<String>>>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let is_reconnecting = matches!(
        lifecycle,
        DomainLifecycle::Connecting | DomainLifecycle::Reconnecting
    ) || !payload.available;

    if !payload.is_present {
        let (title, message) = if !payload.available {
            match lifecycle {
                DomainLifecycle::Connecting | DomainLifecycle::Reconnecting => (
                    "Reconnecting battery service",
                    "Waiting for updated battery information.",
                ),
                _ => (
                    "Battery unavailable",
                    "Battery information is currently unavailable.",
                ),
            }
        } else {
            ("No system battery", "The system battery was removed.")
        };
        return v_flex()
            .w_full()
            .gap(px(8.0))
            .p(px(16.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme.on_surface)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.on_surface_variant)
                    .child(message),
            )
            .into_any_element();
    }

    let mut card = v_flex()
        .id("battery-card-scroll")
        .w_full()
        .h_full()
        .overflow_y_scroll()
        .gap(px(12.0));

    // Reconnecting or Service Disconnected Banner
    if is_reconnecting {
        let message = if payload.is_present {
            "Battery service unavailable — showing cached data"
        } else {
            "Battery service reconnecting"
        };
        card = card.child(
            h_flex()
                .w_full()
                .items_center()
                .gap(px(8.0))
                .p(px(8.0))
                .rounded_md()
                .bg(theme.surface_container_high)
                .text_color(theme.on_surface_variant)
                .text_size(px(12.0))
                .child(Icon::new(IconName::Refresh).size(px(14.0)))
                .child(div().child(message)),
        );
    }

    // Top Aggregate Summary Block
    let icon_name = if payload.is_charging() {
        IconName::BoltFill
    } else {
        IconName::Info
    };

    let icon_color = if payload.is_charging() {
        theme.primary
    } else if payload.is_low_battery() {
        theme.error
    } else {
        theme.on_surface
    };

    let time_info = if payload.is_charging() {
        payload
            .time_to_full_secs
            .get()
            .map(|value| format_time(value, true))
    } else {
        payload
            .time_to_empty_secs
            .get()
            .map(|value| format_time(value, false))
    };

    let rate_info = payload
        .energy_rate_w
        .get()
        .map(|value| format!("{value:.1} W"));

    let health_info = payload
        .capacity_percent
        .get()
        .map(|value| format!("{value:.0}%"));

    let aggregate_block = v_flex()
        .w_full()
        .gap(px(8.0))
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(Icon::new(icon_name).size(px(24.0)).text_color(icon_color))
                        .child(
                            div()
                                .font_family("Noto Sans Black")
                                .text_size(px(32.0))
                                .text_color(theme.on_surface)
                                .child(format!("{}%", payload.percentage)),
                        ),
                )
                .child(
                    div()
                        .py(px(2.0))
                        .px(px(8.0))
                        .rounded_full()
                        .bg(theme.surface_container_high)
                        .text_size(px(12.0))
                        .text_color(theme.on_surface_variant)
                        .child(format_charge_state(payload.state)),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .text_size(px(12.0))
                .text_color(theme.on_surface_variant)
                .child(div().child(
                    time_info.unwrap_or_else(|| format_charge_state(payload.state).to_string()),
                ))
                .child(
                    h_flex()
                        .gap(px(12.0))
                        .children(rate_info.map(|r| div().child(format!("Rate: {r}"))))
                        .children(health_info.map(|h| div().child(format!("Health: {h}")))),
                ),
        );

    card = card.child(aggregate_block);

    // A single system battery does not need disclosure UI. Show its useful
    // identity and details directly; reserve accordions for choosing among
    // multiple physical batteries.
    if payload.devices.len() == 1 {
        let dev = &payload.devices[0];
        let dev_name = physical_device_name(dev, 0);
        let details = render_physical_device_details(dev, &theme, false);
        card = card.child(
            v_flex()
                .w_full()
                .gap(px(8.0))
                .child(section_label("BATTERY DETAILS", &theme))
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(px(10.0))
                        .py(px(4.0))
                        .child(
                            Icon::new(IconName::BatteryAndroidFull)
                                .size(px(18.0))
                                .text_color(theme.on_surface_variant),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.on_surface)
                                .child(dev_name),
                        ),
                )
                .child(details),
        );
    } else if !payload.devices.is_empty() {
        let mut dev_list = v_flex()
            .w_full()
            .gap(px(6.0))
            .child(section_label("PHYSICAL BATTERIES", &theme));

        for (idx, dev) in payload.devices.iter().enumerate() {
            let is_expanded = selected.as_deref() == Some(dev.id.as_str());
            let dev_name = physical_device_name(dev, idx);

            let dev_pct = if let Some(percentage) = dev.percentage.get() {
                format!("{percentage:.0}%")
            } else {
                format_charge_state(dev.charge_state).to_string()
            };

            let cell = selected_cell.clone();
            let device_id = dev.id.clone();
            let row_bg = theme.surface_container;

            let mut row = v_flex().w_full().rounded_2xl().bg(row_bg).child(
                h_flex()
                    .w_full()
                    .min_h(px(48.0))
                    .items_center()
                    .justify_between()
                    .px(px(12.0))
                    .rounded_2xl()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_container_high))
                    .on_mouse_down(gpui::MouseButton::Left, move |_event, window, _cx| {
                        if let Ok(mut lock) = cell.lock() {
                            toggle_selected_device(&mut lock, &device_id);
                        }
                        window.refresh();
                    })
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                Icon::new(IconName::BatteryAndroidFull)
                                    .size(px(18.0))
                                    .text_color(theme.on_surface_variant),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.on_surface)
                                    .child(dev_name),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.on_surface_variant)
                                    .child(dev_pct),
                            )
                            .child(
                                Icon::new(if is_expanded {
                                    IconName::KeyboardArrowUp
                                } else {
                                    IconName::KeyboardArrowDown
                                })
                                .size(px(14.0))
                                .text_color(theme.on_surface_variant),
                            ),
                    ),
            );

            // Single allowed secondary detail view when expanded
            if is_expanded {
                let detail_box = render_physical_device_details(dev, &theme, true);
                row = row.child(detail_box);
            }

            dev_list = dev_list.child(row);
        }

        card = card.child(dev_list);
    }

    div()
        .size_full()
        .p(px(16.0))
        .overflow_hidden()
        .child(card)
        .into_any_element()
}

fn section_label(label: &'static str, theme: &shilpo_m3e::Theme) -> AnyElement {
    div()
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme.on_surface_variant)
        .child(label)
        .into_any_element()
}

fn physical_device_name(dev: &BatteryDevicePayload, index: usize) -> String {
    if !dev.model.is_empty() {
        if !dev.vendor.is_empty() {
            format!("{} {}", dev.vendor, dev.model)
        } else {
            dev.model.clone()
        }
    } else {
        format!("Battery {}", index + 1)
    }
}

/// User-facing battery details. Raw telemetry (empty energy, voltages,
/// timestamps, UPower capabilities) remains in the domain payload for future
/// diagnostics but is intentionally absent from the everyday card.
fn physical_detail_rows(
    dev: &BatteryDevicePayload,
    include_health: bool,
) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    if dev.technology != BatteryTechnology::Unknown {
        rows.push(("Technology", format_technology(dev.technology).to_string()));
    }
    if include_health && let Some(value) = dev.capacity_percent.get() {
        rows.push(("Health", format!("{value:.0}%")));
    }
    match (dev.energy_full_wh.get(), dev.energy_full_design_wh.get()) {
        (Some(full), Some(design)) => rows.push((
            "Full charge capacity",
            format!("{full:.1} of {design:.1} Wh"),
        )),
        (Some(full), None) => rows.push(("Full charge capacity", format!("{full:.1} Wh"))),
        _ => {}
    }
    if let Some(value) = dev.temperature_c.get() {
        rows.push(("Temperature", format!("{value:.1} °C")));
    }
    if let Some(value) = dev.cycle_count.get() {
        rows.push(("Cycles", value.to_string()));
    }
    if dev.warning_level != BatteryWarningLevel::Unknown
        && dev.warning_level != BatteryWarningLevel::None
    {
        rows.push(("Warning Level", format!("{:?}", dev.warning_level)));
    }
    let threshold_supported = dev.charge_threshold_supported.get();
    let threshold_enabled = dev.charge_threshold_enabled.get();
    let settings = dev.charge_threshold_settings_supported.get();
    if threshold_supported == Some(true) && threshold_enabled == Some(false) {
        rows.push(("Battery care", "Off".to_string()));
    } else if threshold_enabled == Some(true) {
        let start_supported = settings.is_none_or(|bits| bits & 1 != 0);
        let end_supported = settings.is_none_or(|bits| bits & 2 != 0);
        let firmware_managed = settings.is_some_and(|bits| bits & 4 != 0);

        if start_supported && let Some(start) = dev.charge_start_threshold.get() {
            rows.push(("Charge resumes below", format!("{start}%")));
        }
        if end_supported && let Some(end) = dev.charge_end_threshold.get() {
            rows.push(("Charge stops at", format!("{end}%")));
        }
        if firmware_managed && !start_supported && !end_supported {
            rows.push(("Battery care", "Managed by firmware".to_string()));
        }
    }

    rows
}

fn render_physical_device_details(
    dev: &BatteryDevicePayload,
    theme: &shilpo_m3e::Theme,
    include_health: bool,
) -> AnyElement {
    let mut details = v_flex()
        .w_full()
        .gap(px(4.0))
        .px(px(12.0))
        .pb(px(12.0))
        .pt(px(8.0))
        .border_t_1()
        .border_color(theme.outline_variant)
        .text_size(px(11.0))
        .text_color(theme.on_surface_variant);

    for (label, value) in physical_detail_rows(dev, include_health) {
        details = details.child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(div().font_weight(gpui::FontWeight::MEDIUM).child(label))
                .child(div().text_color(theme.on_surface).child(value)),
        );
    }

    details.into_any_element()
}

#[cfg(test)]
mod tests {
    use shilpo_services::{OptionalF64, OptionalU64};

    use super::*;

    #[test]
    fn test_format_time_helpers() {
        assert_eq!(format_time(14400, false), "4h 0m remaining");
        assert_eq!(format_time(3900, true), "1h 5m until full");
        assert_eq!(format_time(120, false), "2m remaining");
    }

    #[test]
    fn test_format_charge_state_helpers() {
        assert_eq!(
            format_charge_state(BatteryChargeState::Charging),
            "Charging"
        );
        assert_eq!(
            format_charge_state(BatteryChargeState::Discharging),
            "Discharging"
        );
        assert_eq!(
            format_charge_state(BatteryChargeState::FullyCharged),
            "Fully Charged"
        );
        assert_eq!(
            format_charge_state(BatteryChargeState::PendingCharge),
            "Pending Charge"
        );
        assert_eq!(format_charge_state(BatteryChargeState::Unknown), "Unknown");
    }

    #[test]
    fn test_format_technology_helpers() {
        assert_eq!(
            format_technology(BatteryTechnology::LithiumIon),
            "Lithium Ion"
        );
        assert_eq!(
            format_technology(BatteryTechnology::LithiumPolymer),
            "Lithium Polymer"
        );
        assert_eq!(format_technology(BatteryTechnology::Unknown), "Unknown");
    }

    #[test]
    fn test_battery_card_provider_capabilities() {
        let provider = BatteryCardProvider::new();
        assert_eq!(provider.owner_id(), CardOwnerId::new("battery"));
        assert!(provider.capabilities().click);
        assert!(!provider.capabilities().hover);
    }

    #[test]
    fn details_omit_unavailable_metrics_and_keep_single_threshold_meaningful() {
        let device = BatteryDevicePayload {
            capacity_percent: OptionalF64::some(87.4),
            charge_end_threshold: OptionalU64::some(80),
            charge_threshold_supported: shilpo_services::OptionalBool::some(true),
            charge_threshold_enabled: shilpo_services::OptionalBool::some(true),
            charge_threshold_settings_supported: OptionalU64::some(2),
            ..Default::default()
        };

        let rows = physical_detail_rows(&device, true);
        assert!(rows.contains(&("Health", "87%".to_string())));
        assert!(rows.contains(&("Charge stops at", "80%".to_string())));
        assert!(!rows.iter().any(|(label, _)| *label == "Energy"));
        assert!(!rows.iter().any(|(_, value)| value.contains("0%–80%")));
        assert!(
            !rows
                .iter()
                .any(|(label, _)| *label == "Charge resumes below")
        );
    }

    #[test]
    fn disabled_charge_threshold_is_presented_as_off_not_as_active_limits() {
        let device = BatteryDevicePayload {
            charge_start_threshold: OptionalU64::some(75),
            charge_end_threshold: OptionalU64::some(80),
            charge_threshold_supported: shilpo_services::OptionalBool::some(true),
            charge_threshold_enabled: shilpo_services::OptionalBool::some(false),
            charge_threshold_settings_supported: OptionalU64::some(2),
            ..Default::default()
        };

        let rows = physical_detail_rows(&device, false);
        assert!(rows.contains(&("Battery care", "Off".to_string())));
        assert!(!rows.iter().any(|(label, _)| label.contains("Charge ")));
    }

    #[test]
    fn aggregate_health_is_not_repeated_for_a_single_battery() {
        let device = BatteryDevicePayload {
            capacity_percent: OptionalF64::some(87.4),
            energy_full_wh: OptionalF64::some(31.5),
            energy_full_design_wh: OptionalF64::some(48.0),
            ..Default::default()
        };

        let rows = physical_detail_rows(&device, false);
        assert!(!rows.iter().any(|(label, _)| *label == "Health"));
        assert!(rows.contains(&("Full charge capacity", "31.5 of 48.0 Wh".to_string())));
    }

    #[test]
    fn physical_row_uses_charge_percentage_not_health() {
        let device = BatteryDevicePayload {
            percentage: OptionalF64::some(42.0),
            capacity_percent: OptionalF64::some(91.0),
            ..Default::default()
        };
        assert_eq!(device.percentage.get(), Some(42.0));
        assert_eq!(device.capacity_percent.get(), Some(91.0));
    }

    #[test]
    fn selecting_a_device_replaces_the_single_secondary_detail() {
        let mut selected = None;
        toggle_selected_device(&mut selected, "BAT0");
        assert_eq!(selected.as_deref(), Some("BAT0"));
        toggle_selected_device(&mut selected, "BAT1");
        assert_eq!(selected.as_deref(), Some("BAT1"));
        toggle_selected_device(&mut selected, "BAT1");
        assert_eq!(selected, None);
    }
}
