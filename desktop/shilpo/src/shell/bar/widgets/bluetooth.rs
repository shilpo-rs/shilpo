use gpui::{App, ElementId, IntoElement, RenderOnce, StyleRefinement, Styled, Window};
use shilpo_services::BluetoothInfo;

#[derive(IntoElement)]
pub struct BluetoothWidget {
    id: ElementId,
    info: BluetoothInfo,
    style: StyleRefinement,
}

impl BluetoothWidget {
    pub fn new(id: impl Into<ElementId>, info: BluetoothInfo) -> Self {
        Self {
            id: id.into(),
            info,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for BluetoothWidget {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for BluetoothWidget {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        crate::shell::widgets::BluetoothWidget::new(self.id, self.info.powered, self.info.connected)
            .render(window, cx)
    }
}
