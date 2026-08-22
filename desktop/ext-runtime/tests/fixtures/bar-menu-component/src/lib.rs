wit_bindgen::generate!({
    path: "../../../../../core/ext-api/wit",
    world: "extension",
});

use shilpo::extension::{events, types, view};
use std::sync::atomic::{AtomicUsize, Ordering};

struct BarMenuFixture;
static NEXT_CLOSE_REASON: AtomicUsize = AtomicUsize::new(0);

impl Guest for BarMenuFixture {
    fn activate(_activation: types::Activation) -> Result<(), types::Error> {
        Ok(())
    }

    fn deactivate(_reason: types::DeactivateReason) -> Result<(), types::Error> {
        Ok(())
    }

    fn on_event(event: events::ExtensionEvent) -> Result<(), types::Error> {
        match event {
            events::ExtensionEvent::BarMenuOpened(payload) => {
                assert_eq!(payload.contribution_id, "menu");
                assert_eq!(payload.instance_id, "bar:display-1:fixture");
            }
            events::ExtensionEvent::BarMenuClosed(payload) => {
                assert_eq!(payload.contribution_id, "menu");
                assert_eq!(payload.instance_id, "bar:display-1:fixture");
                let expected = [
                    events::BarMenuCloseReason::SourceToggle,
                    events::BarMenuCloseReason::Escape,
                    events::BarMenuCloseReason::FocusLost,
                    events::BarMenuCloseReason::OutsideClick,
                    events::BarMenuCloseReason::OverviewOpened,
                    events::BarMenuCloseReason::BarClosed,
                    events::BarMenuCloseReason::DisplayRemoved,
                    events::BarMenuCloseReason::OwnerRemoved,
                    events::BarMenuCloseReason::SourceUnavailable,
                ];
                let index = NEXT_CLOSE_REASON.fetch_add(1, Ordering::Relaxed);
                assert_eq!(payload.reason, expected[index]);
            }
            _ => {}
        }
        Ok(())
    }

    fn view(contribution_id: String) -> Result<Option<view::ViewTree>, types::Error> {
        if contribution_id != "menu" {
            return Ok(None);
        }

        Ok(Some(view::ViewTree {
            nodes: vec![
                view::ViewNode::Container(view::ContainerNode {
                    direction: view::ContainerDirection::Grid(2),
                    children: vec![1, 2],
                    style: Some(view::ViewStyle {
                        padding: Some(12.0),
                        padding_edges: None,
                        margin: Some(2.0),
                        margin_edges: None,
                        width: None,
                        height: None,
                        corner_radius: Some(16.0),
                        corner_radii: None,
                        opacity: Some(0.95),
                        color: Some(view::SemanticColorToken::OnSurface),
                        background: Some(view::Fill::Solid(view::SemanticColorToken::SurfaceContainer)),
                        flex_grow: None,
                        border_width: Some(1.0),
                        border_edges: None,
                        border_color: Some(view::SemanticColorToken::Outline),
                        shadows: None,
                        min_width: None,
                        max_width: None,
                        min_height: None,
                        max_height: None,
                        overflow: Some(view::Overflow::Scroll),
                    }),
                    gap: Some(8.0),
                    align_items: Some(view::Alignment::Center),
                    justify_content: Some(view::Justification::SpaceBetween),
                    wrap: false,
                    event_id: Some("background".into()),
                }),
                view::ViewNode::Button(view::ButtonNode {
                    label: "Refresh".into(),
                    event_id: "refresh".into(),
                    style: None,
                }),
                view::ViewNode::TextInput(view::TextInputNode {
                    value: "London".into(),
                    placeholder: Some("City".into()),
                    event_id: "city".into(),
                    style: None,
                }),
            ],
            root: 0,
        }))
    }

    fn search(
        _contribution_id: String,
        _request: types::SearchRequest,
    ) -> Result<Vec<types::SearchCandidate>, types::Error> {
        Ok(Vec::new())
    }
}

export!(BarMenuFixture);
