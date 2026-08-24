use gpui::{App, ElementId, IntoElement, RenderOnce, StyleRefinement, Styled, Window};
use shilpo_services::{DeviceCommand, MediaAction};
use shilpo_services::{MediaInfo, PlaybackState};

use crate::shell::bar::service_worker::{self, CommandSender, WorkerCommand};
use crate::shell::runtime::ShellSurfaces;
use crate::shell::widgets::MediaControl;

/// MPRIS Media player preview widget for Shilpo status bar.
#[derive(IntoElement)]
pub struct MediaWidget {
    id: ElementId,
    info: MediaInfo,
    vertical: bool,
    commands: CommandSender,
    style: StyleRefinement,
}

impl MediaWidget {
    pub fn new(
        id: impl Into<ElementId>,
        info: MediaInfo,
        vertical: bool,
        commands: CommandSender,
    ) -> Self {
        Self {
            id: id.into(),
            info,
            vertical,
            commands,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for MediaWidget {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for MediaWidget {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_playing = self.info.playback_state == PlaybackState::Playing;
        let progress = self.info.progress();
        let cmd_tx_play = self.commands.clone();
        let cmd_tx_next = self.commands.clone();

        MediaControl::new(self.id)
            .title(self.info.title)
            .artist(self.info.artist)
            .art_url(self.info.art_url)
            .playing(is_playing)
            .can_play_pause(self.info.can_play_pause)
            .can_go_next(self.info.can_go_next)
            .progress(progress)
            .vertical(self.vertical)
            .reduced_motion(ShellSurfaces::overview_reduced_motion(cx))
            .on_play_pause(move |_, _, _| {
                let _ = service_worker::try_send_command(
                    &cmd_tx_play,
                    WorkerCommand::Device(DeviceCommand::Media(MediaAction::PlayPause)),
                );
            })
            .on_next(move |_, _, _| {
                let _ = service_worker::try_send_command(
                    &cmd_tx_next,
                    WorkerCommand::Device(DeviceCommand::Media(MediaAction::Next)),
                );
            })
    }
}
