use gpui::{
    App, AppContext, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use shilpo_ext_api::{
    Alignment, CanonicalId, ContainerDirection, Justification, Overflow, SemanticColorToken,
    ViewNode, ViewStyle, ViewTree,
};
use shilpo_m3e::{
    ActiveTheme, Icon, Sizable, Size,
    input::{Input, InputEvent, InputState},
    progress::LoadingIndicator,
    slider::{Slider, SliderEvent, SliderState, SliderValue},
};

use crate::runtime::ShellRuntime;

#[derive(Clone, Debug, PartialEq)]
pub struct ContainerDescriptor {
    pub direction: ContainerDirection,
    pub align_items: Option<Alignment>,
    pub justify_content: Option<Justification>,
    pub wrap: bool,
    pub gap: Option<f32>,
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StyleDescriptor {
    pub padding: Option<f32>,
    pub margin: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub corner_radius: Option<f32>,
    pub opacity: Option<f32>,
    pub color: Option<SemanticColorToken>,
    pub background: Option<SemanticColorToken>,
    pub flex_grow: Option<f32>,
    pub border_width: Option<f32>,
    pub border_color: Option<SemanticColorToken>,
    pub min_width: Option<f32>,
    pub max_width: Option<f32>,
    pub min_height: Option<f32>,
    pub max_height: Option<f32>,
    pub overflow: Option<Overflow>,
}

pub fn map_container_descriptor(c: &shilpo_ext_api::ContainerNode) -> ContainerDescriptor {
    ContainerDescriptor {
        direction: c.direction,
        align_items: c.align_items,
        justify_content: c.justify_content,
        wrap: c.wrap,
        gap: c.gap,
        event_id: c.event_id.clone(),
    }
}

pub fn map_style_descriptor(s: &ViewStyle) -> StyleDescriptor {
    StyleDescriptor {
        padding: s.padding,
        margin: s.margin,
        width: s.width,
        height: s.height,
        corner_radius: s.corner_radius,
        opacity: s.opacity,
        color: s.color,
        background: s.background,
        flex_grow: s.flex_grow,
        border_width: s.border_width,
        border_color: s.border_color,
        min_width: s.min_width,
        max_width: s.max_width,
        min_height: s.min_height,
        max_height: s.max_height,
        overflow: s.overflow,
    }
}

pub fn render_ext_view_tree(
    contribution: &CanonicalId,
    instance_id: Option<&str>,
    tree: &ViewTree,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    render_view_node(contribution, instance_id, &tree.root, window, cx)
}

fn render_view_node(
    contribution: &CanonicalId,
    instance_id: Option<&str>,
    node: &ViewNode,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    match node {
        ViewNode::Container(c) => {
            let mut container = match c.direction {
                ContainerDirection::Row => div().flex().flex_row(),
                ContainerDirection::Column => div().flex().flex_col(),
                ContainerDirection::Grid { columns } => div().grid().grid_cols(columns),
                ContainerDirection::Stack => div().relative(),
            };

            if c.wrap
                && matches!(
                    c.direction,
                    ContainerDirection::Row | ContainerDirection::Column
                )
            {
                container = container.flex_wrap();
            }

            if let Some(align) = c.align_items {
                container = match align {
                    Alignment::Start => container.items_start(),
                    Alignment::Center => container.items_center(),
                    Alignment::End => container.items_end(),
                    Alignment::Stretch => container.items_stretch(),
                };
            }

            if let Some(just) = c.justify_content {
                container = match just {
                    Justification::Start => container.justify_start(),
                    Justification::Center => container.justify_center(),
                    Justification::End => container.justify_end(),
                    Justification::SpaceBetween => container.justify_between(),
                    Justification::SpaceAround => container.justify_around(),
                };
            }

            if let Some(gap) = c.gap {
                container = container.gap(px(gap));
            }

            if let Some(style) = &c.style {
                container = apply_view_style(container, style, cx);
            }

            for child in &c.children {
                let child = render_view_node(contribution, instance_id, child, window, cx);
                container = if c.direction == ContainerDirection::Stack {
                    container.child(div().absolute().inset_0().child(child))
                } else {
                    container.child(child)
                };
            }

            if let Some(event_id) = &c.event_id {
                let contribution = contribution.clone();
                let instance_id_owned = instance_id.map(ToOwned::to_owned);
                let event_id = event_id.clone();
                let container_element_id = format!(
                    "ext:{contribution}:{}:container:{event_id}",
                    instance_id.unwrap_or("shared")
                );
                container
                    .id(container_element_id)
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        ShellRuntime::dispatch_extension_input(
                            cx,
                            &contribution,
                            instance_id_owned.as_deref(),
                            event_id.clone(),
                            None,
                        );
                    })
                    .into_any_element()
            } else {
                container.into_any_element()
            }
        }
        ViewNode::Text(t) => {
            let mut el = div().child(t.content.clone());
            if let Some(style) = &t.style {
                el = apply_view_style(el, style, cx);
            }
            if let Some(sz) = t.font_size {
                el = el.text_size(px(sz));
            }
            if t.bold == Some(true) {
                el = el.font_weight(gpui::FontWeight::BOLD);
            }
            el.into_any_element()
        }
        ViewNode::Icon(icon) => {
            let mut glyph = Icon::default().path(format!("icons/{}.svg", icon.name));
            if let Some(size) = icon.size {
                glyph = glyph.size(px(size));
            }
            let mut el = div().child(glyph);
            if let Some(style) = &icon.style {
                el = apply_view_style(el, style, cx);
            }
            el.into_any_element()
        }
        ViewNode::Image(img) => {
            let asset = ShellRuntime::extension_asset_path(cx, contribution, &img.asset_path).ok();
            let mut el = div();
            if let Some(asset) = asset {
                el = el.child(gpui::img(asset));
            }
            if let Some(style) = &img.style {
                el = apply_view_style(el, style, cx);
            }
            if let Some(w) = img.width {
                el = el.w(px(w));
            }
            if let Some(h) = img.height {
                el = el.h(px(h));
            }
            el.into_any_element()
        }
        ViewNode::Button(btn) => {
            let contribution = contribution.clone();
            let instance_id = instance_id.map(ToOwned::to_owned);
            let event_id = btn.event_id.clone();
            let mut base = div()
                .px_3()
                .py_1()
                .rounded_md()
                .bg(cx.theme().primary)
                .text_color(cx.theme().on_primary)
                .child(btn.label.clone());
            if let Some(style) = &btn.style {
                base = apply_view_style(base, style, cx);
            }
            base.id(format!("ext:{contribution}:button:{event_id}"))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    ShellRuntime::dispatch_extension_input(
                        cx,
                        &contribution,
                        instance_id.as_deref(),
                        event_id.clone(),
                        None,
                    );
                })
                .into_any_element()
        }
        ViewNode::IconButton(ibtn) => {
            let contribution = contribution.clone();
            let instance_id = instance_id.map(ToOwned::to_owned);
            let event_id = ibtn.event_id.clone();
            let mut base = div()
                .p_1()
                .rounded_full()
                .bg(cx.theme().surface_container)
                .child(Icon::default().path(format!("icons/{}.svg", ibtn.icon_name)));
            if let Some(style) = &ibtn.style {
                base = apply_view_style(base, style, cx);
            }
            base.id(format!("ext:{contribution}:icon-button:{event_id}"))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    ShellRuntime::dispatch_extension_input(
                        cx,
                        &contribution,
                        instance_id.as_deref(),
                        event_id.clone(),
                        None,
                    );
                })
                .into_any_element()
        }
        ViewNode::Toggle(t) => {
            let contribution = contribution.clone();
            let instance_id = instance_id.map(ToOwned::to_owned);
            let event_id = t.event_id.clone();
            let value = !t.value;
            let bg = if t.value {
                cx.theme().primary
            } else {
                cx.theme().surface_container
            };
            let mut base = div().w(px(36.0)).h(px(20.0)).rounded_full().bg(bg);
            if let Some(style) = &t.style {
                base = apply_view_style(base, style, cx);
            }
            base.id(format!("ext:{contribution}:toggle:{event_id}"))
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    ShellRuntime::dispatch_extension_input(
                        cx,
                        &contribution,
                        instance_id.as_deref(),
                        event_id.clone(),
                        Some(value.into()),
                    );
                })
                .into_any_element()
        }
        ViewNode::Slider(s) => {
            let contribution = contribution.clone();
            let instance_id = instance_id.map(ToOwned::to_owned);
            let event_id = s.event_id.clone();
            let state = cx.new(|_| {
                SliderState::new()
                    .min(s.min)
                    .max(s.max)
                    .step(((s.max - s.min) / 100.0).max(f32::EPSILON))
                    .default_value(s.value)
            });
            cx.subscribe(&state, move |_, event: &SliderEvent, cx| {
                if let SliderEvent::Change(SliderValue::Single(value))
                | SliderEvent::Release(SliderValue::Single(value)) = event
                {
                    ShellRuntime::dispatch_extension_input(
                        cx,
                        &contribution,
                        instance_id.as_deref(),
                        event_id.clone(),
                        Some((*value).into()),
                    );
                }
            })
            .detach();
            let mut base = div()
                .w_full()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(Slider::new(&state));
            if let Some(style) = &s.style {
                base = apply_view_style(base, style, cx);
            }
            base.into_any_element()
        }
        ViewNode::TextInput(input) => {
            let contribution = contribution.clone();
            let instance_id = instance_id.map(ToOwned::to_owned);
            let event_id = input.event_id.clone();
            let placeholder = input.placeholder.clone().unwrap_or_default();
            let value = input.value.clone();
            let state = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            });
            cx.subscribe(&state, move |state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = state.read(cx).value().to_string();
                    ShellRuntime::dispatch_extension_input(
                        cx,
                        &contribution,
                        instance_id.as_deref(),
                        event_id.clone(),
                        Some(value.into()),
                    );
                }
            })
            .detach();
            let mut base = div()
                .w_full()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(Input::new(&state));
            if let Some(style) = &input.style {
                base = apply_view_style(base, style, cx);
            }
            base.into_any_element()
        }
        ViewNode::List(l) => {
            let mut el = div().flex().flex_col();
            if let Some(style) = &l.style {
                el = apply_view_style(el, style, cx);
            }
            for item in &l.items {
                el = el.child(render_view_node(
                    contribution,
                    instance_id,
                    item,
                    window,
                    cx,
                ));
            }
            el.into_any_element()
        }
        ViewNode::Spacer(sp) => {
            let sz = sp.size.unwrap_or(8.0);
            div().w(px(sz)).h(px(sz)).into_any_element()
        }
        ViewNode::Divider => div()
            .h(px(1.0))
            .w_full()
            .bg(cx.theme().outline)
            .into_any_element(),
        ViewNode::Badge(b) => {
            let mut el = div()
                .px_2()
                .py_0p5()
                .rounded_full()
                .bg(cx.theme().secondary)
                .text_color(cx.theme().on_primary)
                .text_xs()
                .child(b.label.clone());
            if let Some(style) = &b.style {
                el = apply_view_style(el, style, cx);
            }
            el.into_any_element()
        }
        ViewNode::Progress(p) => {
            let progress_pct = p.value.clamp(0.0, 1.0);
            let mut el = div()
                .h(px(4.0))
                .w_full()
                .rounded_full()
                .bg(cx.theme().surface_container)
                .child(
                    div()
                        .h_full()
                        .w(gpui::DefiniteLength::Fraction(progress_pct))
                        .bg(cx.theme().primary)
                        .rounded_full(),
                );
            if let Some(style) = &p.style {
                el = apply_view_style(el, style, cx);
            }
            el.into_any_element()
        }
        ViewNode::LoadingIndicator(indicator) => {
            let id = format!(
                "ext:{contribution}:{}:loading-indicator",
                instance_id.unwrap_or("shared")
            );
            let size = indicator
                .size
                .map_or(Size::XSmall, |size| Size::Size(px(size)));
            let color = indicator
                .color
                .map(|token| resolve_color_token(token, cx))
                .unwrap_or(cx.theme().primary);
            let mut el = div()
                .flex()
                .items_center()
                .justify_center()
                .child(LoadingIndicator::new(id).with_size(size).color(color));
            if let Some(style) = &indicator.style {
                el = apply_view_style(el, style, cx);
            }
            el.into_any_element()
        }
    }
}

fn apply_view_style(mut div: gpui::Div, style: &ViewStyle, cx: &App) -> gpui::Div {
    if let Some(p) = style.padding {
        div = div.p(px(p));
    }
    if let Some(m) = style.margin {
        div = div.m(px(m));
    }
    if let Some(w) = style.width {
        div = div.w(px(w));
    }
    if let Some(h) = style.height {
        div = div.h(px(h));
    }
    if let Some(r) = style.corner_radius {
        div = div.rounded(px(r));
    }
    if let Some(o) = style.opacity {
        div = div.opacity(o);
    }
    if let Some(fg) = style.flex_grow {
        div = div.flex_grow(fg);
    }
    if let Some(c) = style.color {
        div = div.text_color(resolve_color_token(c, cx));
    }
    if let Some(bg) = style.background {
        div = div.bg(resolve_color_token(bg, cx));
    }
    if let Some(bw) = style.border_width
        && bw > 0.0
    {
        let width = px(bw).into();
        div.style().border_widths.top = Some(width);
        div.style().border_widths.bottom = Some(width);
        div.style().border_widths.left = Some(width);
        div.style().border_widths.right = Some(width);
        let color_token = style.border_color.unwrap_or(SemanticColorToken::Outline);
        div = div.border_color(resolve_color_token(color_token, cx));
    }
    if let Some(min_w) = style.min_width {
        div = div.min_w(px(min_w));
    }
    if let Some(max_w) = style.max_width {
        div = div.max_w(px(max_w));
    }
    if let Some(min_h) = style.min_height {
        div = div.min_h(px(min_h));
    }
    if let Some(max_h) = style.max_height {
        div = div.max_h(px(max_h));
    }
    if let Some(overflow) = style.overflow {
        let (x, y) = match overflow {
            Overflow::Visible => (gpui::Overflow::Visible, gpui::Overflow::Visible),
            Overflow::Hidden => (gpui::Overflow::Hidden, gpui::Overflow::Hidden),
            Overflow::Scroll => (gpui::Overflow::Scroll, gpui::Overflow::Scroll),
        };
        div.style().overflow.x = Some(x);
        div.style().overflow.y = Some(y);
    }
    div
}

fn resolve_color_token(token: SemanticColorToken, cx: &App) -> gpui::Hsla {
    let theme = cx.theme();
    match token {
        SemanticColorToken::Surface => theme.surface,
        SemanticColorToken::OnSurface => theme.on_surface,
        SemanticColorToken::SurfaceDim => theme.surface_dim,
        SemanticColorToken::SurfaceBright => theme.surface_bright,
        SemanticColorToken::SurfaceContainerLowest => theme.surface_container_lowest,
        SemanticColorToken::SurfaceContainerLow => theme.surface_container_low,
        SemanticColorToken::SurfaceContainer => theme.surface_container,
        SemanticColorToken::SurfaceContainerHigh => theme.surface_container_high,
        SemanticColorToken::SurfaceContainerHighest => theme.surface_container_highest,
        SemanticColorToken::SurfaceVariant => theme.surface_variant,
        SemanticColorToken::OnSurfaceVariant => theme.on_surface_variant,
        SemanticColorToken::InverseSurface => theme.inverse_surface,
        SemanticColorToken::InverseOnSurface => theme.inverse_on_surface,
        SemanticColorToken::Outline => theme.outline,
        SemanticColorToken::OutlineVariant => theme.outline_variant,
        // `Theme::shadow` is a "shadows enabled" bool, not a color -- the color lives on the
        // nested `ThemeColor` that `Theme` derefs to, so it needs disambiguating explicitly.
        SemanticColorToken::Shadow => theme.colors.shadow,
        SemanticColorToken::Scrim => theme.scrim,
        SemanticColorToken::SurfaceTint => theme.surface_tint,
        SemanticColorToken::Primary => theme.primary,
        SemanticColorToken::OnPrimary => theme.on_primary,
        SemanticColorToken::PrimaryContainer => theme.primary_container,
        SemanticColorToken::OnPrimaryContainer => theme.on_primary_container,
        SemanticColorToken::InversePrimary => theme.inverse_primary,
        SemanticColorToken::PrimaryFixed => theme.primary_fixed,
        SemanticColorToken::PrimaryFixedDim => theme.primary_fixed_dim,
        SemanticColorToken::OnPrimaryFixed => theme.on_primary_fixed,
        SemanticColorToken::OnPrimaryFixedVariant => theme.on_primary_fixed_variant,
        SemanticColorToken::Secondary => theme.secondary,
        SemanticColorToken::OnSecondary => theme.on_secondary,
        SemanticColorToken::SecondaryContainer => theme.secondary_container,
        SemanticColorToken::OnSecondaryContainer => theme.on_secondary_container,
        SemanticColorToken::SecondaryFixed => theme.secondary_fixed,
        SemanticColorToken::SecondaryFixedDim => theme.secondary_fixed_dim,
        SemanticColorToken::OnSecondaryFixed => theme.on_secondary_fixed,
        SemanticColorToken::OnSecondaryFixedVariant => theme.on_secondary_fixed_variant,
        SemanticColorToken::Tertiary => theme.tertiary,
        SemanticColorToken::OnTertiary => theme.on_tertiary,
        SemanticColorToken::TertiaryContainer => theme.tertiary_container,
        SemanticColorToken::OnTertiaryContainer => theme.on_tertiary_container,
        SemanticColorToken::TertiaryFixed => theme.tertiary_fixed,
        SemanticColorToken::TertiaryFixedDim => theme.tertiary_fixed_dim,
        SemanticColorToken::OnTertiaryFixed => theme.on_tertiary_fixed,
        SemanticColorToken::OnTertiaryFixedVariant => theme.on_tertiary_fixed_variant,
        SemanticColorToken::Error => theme.error,
        SemanticColorToken::OnError => theme.on_error,
        SemanticColorToken::ErrorContainer => theme.error_container,
        SemanticColorToken::OnErrorContainer => theme.on_error_container,
    }
}

pub fn create_showcase_view_tree() -> ViewTree {
    use shilpo_ext_api::*;

    let grid_container = ViewNode::Container(ContainerNode {
        direction: ContainerDirection::Grid { columns: 3 },
        children: vec![
            ViewNode::Text(TextNode {
                content: "Grid Item 1".into(),
                font_size: Some(14.0),
                bold: Some(true),
                style: None,
            }),
            ViewNode::Text(TextNode {
                content: "Grid Item 2".into(),
                font_size: Some(14.0),
                bold: None,
                style: None,
            }),
            ViewNode::Button(ButtonNode {
                label: "Nested Button".into(),
                event_id: "nested_btn_click".into(),
                style: None,
            }),
        ],
        style: Some(ViewStyle {
            border_width: Some(1.0),
            border_color: Some(SemanticColorToken::Outline),
            padding: Some(8.0),
            ..ViewStyle::default()
        }),
        gap: Some(6.0),
        align_items: Some(Alignment::Center),
        justify_content: Some(Justification::SpaceBetween),
        wrap: false,
        event_id: Some("grid_container_click".into()),
    });

    let flex_wrap_row = ViewNode::Container(ContainerNode {
        direction: ContainerDirection::Row,
        children: vec![
            ViewNode::Text(TextNode {
                content: "Flex Row Item".into(),
                font_size: None,
                bold: None,
                style: None,
            }),
            ViewNode::Badge(BadgeNode {
                label: "Wrap Badge".into(),
                style: None,
            }),
        ],
        style: Some(ViewStyle {
            min_width: Some(100.0),
            max_width: Some(300.0),
            min_height: Some(40.0),
            max_height: Some(150.0),
            overflow: Some(Overflow::Scroll),
            border_width: Some(2.0),
            border_color: Some(SemanticColorToken::Primary),
            ..ViewStyle::default()
        }),
        gap: Some(4.0),
        align_items: Some(Alignment::Start),
        justify_content: Some(Justification::Center),
        wrap: true,
        event_id: Some("flex_row_click".into()),
    });

    let stack_container = ViewNode::Container(ContainerNode {
        direction: ContainerDirection::Stack,
        children: vec![
            ViewNode::Text(TextNode {
                content: "Stack Background".into(),
                font_size: None,
                bold: None,
                style: None,
            }),
            ViewNode::Text(TextNode {
                content: "Stack Foreground".into(),
                font_size: None,
                bold: Some(true),
                style: None,
            }),
        ],
        style: Some(ViewStyle {
            overflow: Some(Overflow::Hidden),
            border_width: Some(1.0),
            border_color: None,
            ..ViewStyle::default()
        }),
        gap: None,
        align_items: None,
        justify_content: None,
        wrap: false,
        event_id: None,
    });

    let root_container = ContainerNode {
        direction: ContainerDirection::Column,
        children: vec![grid_container, flex_wrap_row, stack_container],
        style: Some(ViewStyle {
            padding: Some(12.0),
            ..ViewStyle::default()
        }),
        gap: Some(8.0),
        align_items: Some(Alignment::Stretch),
        justify_content: Some(Justification::Start),
        wrap: false,
        event_id: None,
    };

    ViewTree::new(ViewNode::Container(root_container))
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, VisualTestContext, point};
    use shilpo_ext_api::{
        ButtonNode, ContainerNode, ContributionId, ExtensionId, SliderNode, TextInputNode,
        ToggleNode, ViewLimits,
    };

    use super::*;
    use crate::shell::runtime::ShellRuntime;

    struct TestExtensionView {
        contribution: CanonicalId,
        tree: ViewTree,
    }

    impl Render for TestExtensionView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            render_ext_view_tree(&self.contribution, None, &self.tree, window, cx)
        }
    }

    fn extension_id() -> CanonicalId {
        CanonicalId::new(
            ExtensionId::new("io.example.test").unwrap(),
            ContributionId::new("widget").unwrap(),
        )
    }

    #[gpui::test]
    fn rendered_clickable_container_dispatches_without_bubbling_nested_button(
        cx: &mut TestAppContext,
    ) {
        let id = extension_id();
        let tree = ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Column,
            children: vec![ViewNode::Button(ButtonNode {
                label: "child".into(),
                event_id: "child".into(),
                style: Some(ViewStyle {
                    width: Some(120.0),
                    height: Some(40.0),
                    ..ViewStyle::default()
                }),
            })],
            style: Some(ViewStyle {
                width: Some(200.0),
                height: Some(100.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: Some("parent".into()),
        }));

        let recorder = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
            ShellRuntime::take_test_extension_inputs(app)
        });
        let (_, visual): (_, &mut VisualTestContext) =
            cx.add_window_view(|_, _| TestExtensionView {
                contribution: id,
                tree,
            });
        visual.simulate_click(point(px(20.0), px(20.0)), gpui::Modifiers::default());

        let inputs = recorder.lock().unwrap();
        let events: Vec<_> = inputs
            .iter()
            .filter_map(|command| match command {
                crate::extensions::ExtensionCommand::Input {
                    event_id, value, ..
                } => Some((event_id.as_str(), value.is_none())),
                _ => None,
            })
            .collect();
        assert_eq!(events, vec![("child", true)]);
    }

    fn clickable_parent(child: ViewNode) -> ViewTree {
        ViewTree::new(ViewNode::Container(ContainerNode {
            direction: ContainerDirection::Column,
            children: vec![child],
            style: Some(ViewStyle {
                width: Some(240.0),
                height: Some(120.0),
                ..ViewStyle::default()
            }),
            gap: None,
            align_items: None,
            justify_content: None,
            wrap: false,
            event_id: Some("parent".into()),
        }))
    }

    fn recorded_event_ids(
        recorder: &std::sync::Arc<std::sync::Mutex<Vec<crate::extensions::ExtensionCommand>>>,
    ) -> Vec<String> {
        recorder
            .lock()
            .unwrap()
            .iter()
            .filter_map(|command| match command {
                crate::extensions::ExtensionCommand::Input { event_id, .. } => {
                    Some(event_id.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn add_rooted_extension_view(
        cx: &mut TestAppContext,
        tree: ViewTree,
    ) -> &mut VisualTestContext {
        let (_, visual) = cx.add_window_view(move |window, cx| {
            let view = cx.new(|_| TestExtensionView {
                contribution: extension_id(),
                tree,
            });
            shilpo_m3e::Root::new(view, window, cx)
        });
        visual
    }

    #[gpui::test]
    fn rendered_container_background_dispatches_exactly_once(cx: &mut TestAppContext) {
        let recorder = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
            ShellRuntime::take_test_extension_inputs(app)
        });
        let (_, visual): (_, &mut VisualTestContext) =
            cx.add_window_view(|_, _| TestExtensionView {
                contribution: extension_id(),
                tree: clickable_parent(ViewNode::Spacer(shilpo_ext_api::SpacerNode {
                    size: Some(20.0),
                })),
            });
        visual.simulate_click(point(px(200.0), px(90.0)), gpui::Modifiers::default());
        assert_eq!(recorded_event_ids(&recorder), ["parent"]);
    }

    #[gpui::test]
    fn nested_toggle_dispatches_only_its_own_event(cx: &mut TestAppContext) {
        let recorder = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
            ShellRuntime::take_test_extension_inputs(app)
        });
        let (_, visual): (_, &mut VisualTestContext) =
            cx.add_window_view(|_, _| TestExtensionView {
                contribution: extension_id(),
                tree: clickable_parent(ViewNode::Toggle(ToggleNode {
                    value: false,
                    event_id: "toggle".into(),
                    style: None,
                })),
            });
        visual.simulate_click(point(px(18.0), px(10.0)), gpui::Modifiers::default());
        assert_eq!(recorded_event_ids(&recorder), ["toggle"]);
    }

    #[gpui::test]
    fn nested_slider_pointer_input_never_reaches_parent(cx: &mut TestAppContext) {
        let recorder = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
            ShellRuntime::take_test_extension_inputs(app)
        });
        let visual = add_rooted_extension_view(
            cx,
            clickable_parent(ViewNode::Slider(SliderNode {
                value: 25.0,
                min: 0.0,
                max: 100.0,
                event_id: "slider".into(),
                style: Some(ViewStyle {
                    width: Some(180.0),
                    ..ViewStyle::default()
                }),
            })),
        );
        visual.simulate_mouse_down(
            point(px(150.0), px(30.0)),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        visual.simulate_mouse_up(
            point(px(150.0), px(30.0)),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        let ids = recorded_event_ids(&recorder);
        assert!(ids.iter().all(|id| id == "slider"));
    }

    #[gpui::test]
    fn nested_text_input_types_without_dispatching_parent(cx: &mut TestAppContext) {
        let recorder = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_test(app);
            ShellRuntime::take_test_extension_inputs(app)
        });
        let visual = add_rooted_extension_view(
            cx,
            clickable_parent(ViewNode::TextInput(TextInputNode {
                placeholder: Some("City".into()),
                value: String::new(),
                event_id: "input".into(),
                style: Some(ViewStyle {
                    width: Some(180.0),
                    height: Some(36.0),
                    ..ViewStyle::default()
                }),
            })),
        );
        visual.simulate_click(point(px(40.0), px(30.0)), gpui::Modifiers::default());
        visual.simulate_keystrokes("x");
        let ids = recorded_event_ids(&recorder);
        assert!(ids.iter().all(|id| id == "input"));
    }

    #[test]
    fn pure_mapping_descriptors_cover_all_branches() {
        let container = shilpo_ext_api::ContainerNode {
            direction: ContainerDirection::Grid { columns: 3 },
            children: vec![],
            style: Some(ViewStyle {
                padding: Some(8.0),
                margin: Some(4.0),
                width: Some(200.0),
                height: Some(100.0),
                corner_radius: Some(6.0),
                opacity: Some(0.9),
                color: Some(SemanticColorToken::Primary),
                background: Some(SemanticColorToken::Surface),
                flex_grow: Some(1.0),
                border_width: Some(1.5),
                border_color: Some(SemanticColorToken::Outline),
                min_width: Some(100.0),
                max_width: Some(400.0),
                min_height: Some(50.0),
                max_height: Some(300.0),
                overflow: Some(Overflow::Scroll),
            }),
            gap: Some(12.0),
            align_items: Some(Alignment::Center),
            justify_content: Some(Justification::SpaceBetween),
            wrap: false,
            event_id: Some("card".into()),
        };

        let container_desc = map_container_descriptor(&container);
        assert_eq!(
            container_desc,
            ContainerDescriptor {
                direction: ContainerDirection::Grid { columns: 3 },
                align_items: Some(Alignment::Center),
                justify_content: Some(Justification::SpaceBetween),
                wrap: false,
                gap: Some(12.0),
                event_id: Some("card".into()),
            }
        );

        let style_desc = map_style_descriptor(container.style.as_ref().unwrap());
        assert_eq!(
            style_desc,
            StyleDescriptor {
                padding: Some(8.0),
                margin: Some(4.0),
                width: Some(200.0),
                height: Some(100.0),
                corner_radius: Some(6.0),
                opacity: Some(0.9),
                color: Some(SemanticColorToken::Primary),
                background: Some(SemanticColorToken::Surface),
                flex_grow: Some(1.0),
                border_width: Some(1.5),
                border_color: Some(SemanticColorToken::Outline),
                min_width: Some(100.0),
                max_width: Some(400.0),
                min_height: Some(50.0),
                max_height: Some(300.0),
                overflow: Some(Overflow::Scroll),
            }
        );
    }

    #[test]
    fn showcase_tree_validates_cleanly_and_covers_all_layout_features() {
        let tree = create_showcase_view_tree();
        assert!(tree.validate(ViewLimits::default()).is_ok());
    }

    #[test]
    fn pure_mapping_covers_all_alignment_justification_overflow_branches() {
        let alignments = [
            Alignment::Start,
            Alignment::Center,
            Alignment::End,
            Alignment::Stretch,
        ];
        let justifications = [
            Justification::Start,
            Justification::Center,
            Justification::End,
            Justification::SpaceBetween,
            Justification::SpaceAround,
        ];
        let overflows = [Overflow::Visible, Overflow::Hidden, Overflow::Scroll];

        for &align in &alignments {
            for &just in &justifications {
                let node = shilpo_ext_api::ContainerNode {
                    direction: ContainerDirection::Row,
                    children: vec![],
                    style: None,
                    gap: Some(5.0),
                    align_items: Some(align),
                    justify_content: Some(just),
                    wrap: true,
                    event_id: Some("id".into()),
                };
                let desc = map_container_descriptor(&node);
                assert_eq!(desc.align_items, Some(align));
                assert_eq!(desc.justify_content, Some(just));
            }
        }

        for &overflow in &overflows {
            let style = ViewStyle {
                overflow: Some(overflow),
                ..ViewStyle::default()
            };
            let desc = map_style_descriptor(&style);
            assert_eq!(desc.overflow, Some(overflow));
        }
    }
}
