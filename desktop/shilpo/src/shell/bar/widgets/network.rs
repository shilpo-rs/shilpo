use gpui::{App, ElementId, IntoElement, RenderOnce, StyleRefinement, Styled, Window};
use shilpo_services::NetworkInfo;

#[derive(IntoElement)]
pub struct NetworkWidget {
    id: ElementId,
    info: NetworkInfo,
    style: StyleRefinement,
}

impl NetworkWidget {
    pub fn new(id: impl Into<ElementId>, info: NetworkInfo) -> Self {
        Self {
            id: id.into(),
            info,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for NetworkWidget {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for NetworkWidget {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        crate::shell::widgets::NetworkWidget::new(
            self.id,
            self.info.wifi_enabled,
            self.info.is_connected,
        )
        .render(window, cx)
    }
}
