use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, px,
};
use shilpo_m3e::{
    ActiveTheme, Colorize, ContextMenuExt, Icon, IconName, PopupMenuItem, StyledExt, h_flex,
};

use crate::shell::actions::ActionInvocation;
use crate::shell::runtime::ShellRuntime;

pub type ClickHandler = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>;

/// Module 1: Window Info Capsule (Sparkle icon + App ID + Window Title).
#[derive(IntoElement)]
pub struct WindowInfoCapsule {
    id: ElementId,
    app_id: String,
    title: String,
    style: StyleRefinement,
    on_click: Option<ClickHandler>,
}

impl WindowInfoCapsule {
    pub fn new(
        id: impl Into<ElementId>,
        app_id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            app_id: app_id.into(),
            title: title.into(),
            style: StyleRefinement::default(),
            on_click: None,
        }
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Styled for WindowInfoCapsule {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for WindowInfoCapsule {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let star_btn = div()
            .id("star-btn")
            .w(px(20.))
            .h(px(20.))
            .rounded_full()
            .bg(cx.theme().primary)
            .text_color(cx.theme().on_primary)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().primary.darken(0.1)))
            .child(Icon::new(IconName::Star).size(px(12.)));

        let star_btn = if let Some(handler) = self.on_click {
            star_btn.on_click(handler)
        } else {
            star_btn
        };

        let aria = format!("Active Window: {} - {}", self.app_id, self.title);

        h_flex()
            .id(self.id)
            .role(Role::Button)
            .aria_label(aria)
            .h(px(32.))
            .px_3()
            .items_center()
            .gap_2()
            .rounded_full()
            .bg(cx.theme().surface_container_high.opacity(0.92))
            .border_1()
            .border_color(cx.theme().outline_variant.opacity(0.3))
            .text_color(cx.theme().on_surface)
            .shadow_sm()
            .context_menu(|menu, _, _| {
                menu.item(
                    PopupMenuItem::new("Close Active Overlay")
                        .icon(IconName::Terminal)
                        .on_click(|_, window, cx| {
                            let _ = ShellRuntime::dispatch_action(cx, ActionInvocation::Quit);
                            window.remove_window();
                        }),
                )
            })
            .child(star_btn)
            .child(
                h_flex()
                    .gap_1_5()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().on_surface_variant)
                            .child(format!("{} ·", self.app_id)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().on_surface)
                            .max_w(px(200.))
                            .overflow_hidden()
                            .child(self.title),
                    ),
            )
    }
}
