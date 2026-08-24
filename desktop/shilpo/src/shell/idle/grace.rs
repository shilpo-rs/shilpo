use std::time::Duration;

use gpui::{Context, IntoElement, Render, Styled, Window, div, rgba};

use crate::shell::runtime::ShellRuntime;

/// Fullscreen dimming grace overlay view shown before idle actions execute.
pub struct IdleGraceOverlayView {
    pub(crate) _grace_generation: u64,
    pub(crate) _fade_ms: u32,
}

impl IdleGraceOverlayView {
    pub fn new(
        grace_generation: u64,
        fade_ms: u32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let duration = Duration::from_millis(fade_ms as u64);
        cx.spawn(async move |_this, cx| {
            cx.background_executor().timer(duration).await;
            cx.update(|cx| {
                ShellRuntime::report_idle_grace_completed(cx, grace_generation);
            });
        })
        .detach();

        Self {
            _grace_generation: grace_generation,
            _fade_ms: fade_ms,
        }
    }
}

impl Render for IdleGraceOverlayView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgba(0x000000bb))
    }
}
