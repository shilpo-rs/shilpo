use gpui::{
    AnyElement, App, AvailableSpace, IntoElement, ParentElement, Pixels, Size, Styled, Window,
    deferred, div, px,
};
use shilpo_ext_api::CanonicalId;

use super::{
    model::{CardCapabilities, CardChannel, CardOwnerId, CardSourceId},
    provider::CardProvider,
};
use crate::runtime::ShellRuntime;

pub(crate) struct ExtensionMenuCardProvider {
    pub owner_id: CardOwnerId,
    pub menu_canonical_id: CanonicalId,
    pub _bar_widget_canonical_id: CanonicalId,
    measured_size: std::sync::Arc<std::sync::Mutex<Option<Size<Pixels>>>>,
}

fn update_cached_measurement(
    cached: &std::sync::Mutex<Option<Size<Pixels>>>,
    measured: Size<Pixels>,
) -> bool {
    let mut cached = cached
        .lock()
        .expect("extension menu measurement lock is not poisoned");
    if *cached == Some(measured) {
        false
    } else {
        *cached = Some(measured);
        true
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct BoundedMenuLayout {
    size: Size<Pixels>,
    overflow_x: bool,
    overflow_y: bool,
}

/// Mirrors the existing host-owned measurement policy: `intrinsic` is content,
/// `available` is the monitor work area minus the host's 16 px padding per side.
#[cfg(test)]
fn bound_intrinsic_menu(intrinsic: Size<Pixels>, available: Size<Pixels>) -> BoundedMenuLayout {
    BoundedMenuLayout {
        size: Size {
            width: intrinsic.width.min(available.width) + px(32.0),
            height: intrinsic.height.min(available.height) + px(32.0),
        },
        overflow_x: intrinsic.width > available.width,
        overflow_y: intrinsic.height > available.height,
    }
}

impl ExtensionMenuCardProvider {
    pub fn new(menu_canonical_id: CanonicalId, bar_widget_canonical_id: CanonicalId) -> Self {
        let owner_id = CardOwnerId::new(menu_canonical_id.to_string());
        Self {
            owner_id,
            menu_canonical_id,
            _bar_widget_canonical_id: bar_widget_canonical_id,
            measured_size: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl CardProvider for ExtensionMenuCardProvider {
    fn owner_id(&self) -> CardOwnerId {
        self.owner_id.clone()
    }

    fn capabilities(&self) -> CardCapabilities {
        CardCapabilities {
            hover: false,
            click: true,
        }
    }

    fn source_available(&self, _source: &CardSourceId, cx: &App) -> bool {
        ShellRuntime::extension_view(cx, &self.menu_canonical_id).is_some()
    }

    /// Before the first real content measurement lands, this needs *some* non-degenerate
    /// size for the placement engine to work with. `cx.primary_display()` is not a reliable
    /// source for that: on setups without a clearly-defined primary monitor (multi-display
    /// Wayland compositors routinely have none) it returns `None`, collapsing both dimensions
    /// to the inner `unwrap_or(px(1.0))` fallback -- a 1x1 card, which the placement engine
    /// correctly refuses to show at all. Use the same fixed default every other card-open
    /// fallback in this module already uses instead of depending on a "primary display"
    /// concept that may not exist here.
    fn preferred_size(
        &self,
        _channel: CardChannel,
        _source: &CardSourceId,
        _cx: &App,
    ) -> Size<Pixels> {
        self.measured_size
            .lock()
            .expect("extension menu measurement lock is not poisoned")
            .unwrap_or_else(|| Size {
                width: px(360.0),
                height: px(280.0),
            })
    }

    fn render_content(
        &self,
        _channel: CardChannel,
        source: &CardSourceId,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(tree) = ShellRuntime::extension_view(cx, &self.menu_canonical_id) else {
            return div().into_any_element();
        };

        let measure_element = crate::shell::bar::ext_view_adapter::render_ext_view_tree(
            &self.menu_canonical_id,
            Some(&source.instance_id),
            &tree,
            window,
            cx,
        );

        // The card-band window is only the edge strip, not the monitor. Use the
        // associated display bounds for host constraints so a small previous
        // placement cannot recursively shrink the next measurement.
        let display_bounds = window
            .display(cx)
            .map(|display| display.bounds())
            .unwrap_or_else(|| window.bounds());
        let max_width = (display_bounds.size.width - px(32.0)).max(px(1.0));
        let max_height = (display_bounds.size.height - px(32.0)).max(px(1.0));
        let mut measure_element = deferred(
            div()
                .p_4()
                .max_w(max_width)
                .max_h(max_height)
                .child(measure_element),
        )
        .into_any_element();
        let intrinsic = measure_element.layout_as_root(AvailableSpace::min_size(), window, cx);
        let element = crate::shell::bar::ext_view_adapter::render_ext_view_tree(
            &self.menu_canonical_id,
            Some(&source.instance_id),
            &tree,
            window,
            cx,
        );
        let mut container = div().p_4().max_w(max_width).max_h(max_height);
        if intrinsic.width > max_width {
            container.style().overflow.x = Some(gpui::Overflow::Scroll);
        }
        if intrinsic.height > max_height {
            container.style().overflow.y = Some(gpui::Overflow::Scroll);
        }
        let content = container.child(element).into_any_element();
        let measured = Size {
            width: intrinsic.width.min(max_width) + px(32.0),
            height: intrinsic.height.min(max_height) + px(32.0),
        };
        if update_cached_measurement(&self.measured_size, measured) {
            // `Window::defer` wraps its callback in `handle.update(cx, ...)` on *this same*
            // band window -- see gpui's `Window::defer` impl. Dispatching from inside that
            // would make `ensure_band`'s later `handle.update()` on this same window a
            // re-entrant update call, which gpui correctly refuses. That refusal was being
            // misread as "stale handle, needs recreating", so every single reposition (i.e.
            // every time an extension's card content changes size after first open) spawned
            // a brand new band window while abandoning the old one -- which was never
            // actually stale, just mid-update. `App::defer` runs on the next effects flush
            // without being scoped to any window's update, so it doesn't have this problem;
            // the callback only needs `cx` anyway.
            let source = source.clone();
            cx.defer(move |cx| {
                super::adapter::CardCoordinator::dispatch(
                    cx,
                    super::model::CardRequest::Reposition { source },
                );
            });
        }
        content.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext};
    use shilpo_ext_api::*;
    use shilpo_m3e::ActiveTheme;

    use super::*;
    use crate::shell::bar::ext_view_adapter::render_ext_view_tree;

    struct MeasuredMenu {
        tree: ViewTree,
        measured: std::sync::Arc<std::sync::Mutex<Vec<Size<Pixels>>>>,
    }

    impl Render for MeasuredMenu {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(cx.theme().font_size);
            let element = render_ext_view_tree(
                &CanonicalId::new(
                    ExtensionId::new("io.github.test.weather").unwrap(),
                    ContributionId::new("weather-menu").unwrap(),
                ),
                None,
                &self.tree,
                window,
                cx,
            );
            let mut element = deferred(div().child(element)).into_any_element();
            let size = element.layout_as_root(AvailableSpace::min_size(), window, cx);
            self.measured
                .lock()
                .expect("measurement lock is not poisoned")
                .push(size);
            // Return a fresh element after measurement. A GPUI element is single-use:
            // layout_as_root consumes its request-layout phase.
            div().child("measured")
        }
    }

    #[gpui::test]
    fn dynamic_menu_measurement_uses_gpui_layout(cx: &mut TestAppContext) {
        let measured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tree = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Column,
            children: vec![ViewNode::Text(TextNode {
                content: "A deliberately long menu label".into(),
                style: None,
                font_size: Some(20.0),
                bold: None,
            })],
            style: None,
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: None,
        }));
        let measured_for_view = measured.clone();
        cx.add_window_view(|_, cx| {
            shilpo_m3e::init(cx);
            MeasuredMenu {
                tree,
                measured: measured_for_view,
            }
        });
        cx.run_until_parked();
        let size = measured
            .lock()
            .expect("measurement lock is not poisoned")
            .last()
            .copied()
            .expect("GPUI must measure the menu tree");
        assert!(size.width > px(100.0));
        assert!(size.height > px(10.0));
    }

    fn text_tree(content: &str, font_size: f32) -> ViewTree {
        ViewTree::new(ViewNode::Text(TextNode {
            content: content.into(),
            style: None,
            font_size: Some(font_size),
            bold: None,
        }))
    }

    #[gpui::test]
    fn live_tree_and_font_changes_are_remeasured_by_gpui(cx: &mut TestAppContext) {
        let measured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let measurements = measured.clone();
        let (menu, visual) = cx.add_window_view(|_, cx| {
            shilpo_m3e::init(cx);
            MeasuredMenu {
                tree: text_tree("Short", 12.0),
                measured: measurements,
            }
        });
        visual.run_until_parked();
        let initial = measured.lock().unwrap()[0];

        menu.update(visual, |menu, cx| {
            menu.tree = text_tree("A substantially longer live menu value", 24.0);
            cx.notify();
        });
        visual.run_until_parked();
        let updated = *measured.lock().unwrap().last().unwrap();
        assert!(updated.width > initial.width);
        assert!(updated.height > initial.height);
    }

    #[gpui::test]
    fn host_theme_font_change_remeasures_intrinsic_menu(cx: &mut TestAppContext) {
        let measured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let measurements = measured.clone();
        let (menu, visual) = cx.add_window_view(|_, cx| {
            shilpo_m3e::init(cx);
            MeasuredMenu {
                tree: ViewTree::new(ViewNode::Text(TextNode {
                    content: "Host-sized label".into(),
                    style: None,
                    font_size: None,
                    bold: None,
                })),
                measured: measurements,
            }
        });
        visual.run_until_parked();
        let initial = measured.lock().unwrap()[0];

        visual.update_global::<shilpo_m3e::Theme, _>(|theme, cx| {
            theme.font_size = px(28.0);
            cx.refresh_windows();
        });
        menu.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        let themed = *measured.lock().unwrap().last().unwrap();
        assert!(themed.width > initial.width);
        assert!(themed.height > initial.height);
    }

    #[test]
    fn host_chrome_monitor_bounds_and_per_axis_overflow_are_exact() {
        let available = Size {
            width: px(268.0),
            height: px(168.0),
        };
        let shrink_wrapped = bound_intrinsic_menu(
            Size {
                width: px(100.0),
                height: px(50.0),
            },
            available,
        );
        assert_eq!(
            shrink_wrapped.size,
            Size {
                width: px(132.0),
                height: px(82.0)
            }
        );
        assert!(!shrink_wrapped.overflow_x);
        assert!(!shrink_wrapped.overflow_y);

        let horizontal = bound_intrinsic_menu(
            Size {
                width: px(400.0),
                height: px(50.0),
            },
            available,
        );
        assert_eq!(horizontal.size.width, px(300.0));
        assert_eq!(horizontal.size.height, px(82.0));
        assert!(horizontal.overflow_x);
        assert!(!horizontal.overflow_y);

        let vertical = bound_intrinsic_menu(
            Size {
                width: px(100.0),
                height: px(300.0),
            },
            available,
        );
        assert_eq!(vertical.size.width, px(132.0));
        assert_eq!(vertical.size.height, px(200.0));
        assert!(!vertical.overflow_x);
        assert!(vertical.overflow_y);
    }

    #[test]
    fn unchanged_measurement_does_not_schedule_a_reposition_loop() {
        let cache = std::sync::Mutex::new(None);
        let first = Size {
            width: px(100.0),
            height: px(50.0),
        };
        assert!(update_cached_measurement(&cache, first));
        assert!(!update_cached_measurement(&cache, first));
        assert!(update_cached_measurement(
            &cache,
            Size {
                width: px(180.0),
                height: px(50.0)
            }
        ));
    }

    #[test]
    fn test_card_provider_capabilities() {
        let ext_id = ExtensionId::new("io.github.test.weather").unwrap();
        let menu_id =
            CanonicalId::new(ext_id.clone(), ContributionId::new("weather-menu").unwrap());
        let widget_id = CanonicalId::new(ext_id, ContributionId::new("weather").unwrap());
        let provider = ExtensionMenuCardProvider::new(menu_id, widget_id.clone());
        assert_eq!(provider._bar_widget_canonical_id, widget_id);
        let caps = provider.capabilities();
        assert!(!caps.hover);
        assert!(caps.click);
    }
}
