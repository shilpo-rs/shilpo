use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
};
use shilpo_ext_api::CanonicalId;
use shilpo_m3e::ActiveTheme;

use crate::shell::{bar::ext_view_adapter::render_ext_view_tree, runtime::ShellRuntime};

/// Generic adapter used by desktop, side-panel, settings, and
/// launcher contributions. Placement and lifecycle stay with the owning shell
/// surface; only the validated declarative tree is shared.
pub struct ExtensionSurfaceView {
    contribution: CanonicalId,
    instance_id: Option<String>,
}

impl ExtensionSurfaceView {
    pub fn view(
        contribution: CanonicalId,
        instance_id: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<shilpo_m3e::Root> {
        let view = cx.new(|_| Self {
            contribution,
            instance_id,
        });
        cx.new(|cx| {
            shilpo_m3e::Root::new(view, window, cx)
                .bordered(false)
                .bg(cx.theme().transparent)
        })
    }
}

impl Render for ExtensionSurfaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(tree) = ShellRuntime::extension_view(cx, &self.contribution) {
            render_ext_view_tree(
                &self.contribution,
                self.instance_id.as_deref(),
                &tree,
                window,
                cx,
            )
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().on_surface_variant)
                .child("Extension contribution unavailable")
                .into_any_element()
        }
    }
}
