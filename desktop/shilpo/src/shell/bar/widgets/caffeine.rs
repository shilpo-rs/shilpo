use gpui::{App, ElementId, IntoElement, RenderOnce, StyleRefinement, Styled, Window};
use shilpo_services::CaffeineInfo;

#[derive(IntoElement)]
pub struct CaffeineWidget {
    id: ElementId,
    info: CaffeineInfo,
    style: StyleRefinement,
}

impl CaffeineWidget {
    pub fn new(id: impl Into<ElementId>, info: CaffeineInfo) -> Self {
        Self {
            id: id.into(),
            info,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for CaffeineWidget {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for CaffeineWidget {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        crate::shell::widgets::CaffeineWidget::new(self.id, self.info.active).render(window, cx)
    }
}
