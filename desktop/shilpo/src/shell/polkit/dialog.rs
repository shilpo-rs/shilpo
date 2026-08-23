use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement, Render, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use shilpo_m3e::{
    ActiveTheme, FocusTrapElement as _, Icon, IconName, StyledExt,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use shilpo_services::{PolkitPromptState, PolkitRequest};

use crate::shell::runtime::ShellRuntime;

/// Polkit authentication agent modal dialog surface.
pub struct PolkitDialogView {
    pub(crate) request: PolkitRequest,
    pub(crate) prompt_state: Option<PolkitPromptState>,
    pub(crate) input_state: Entity<InputState>,
    pub(crate) focus_handle: FocusHandle,
}

impl PolkitDialogView {
    pub fn new(
        request: PolkitRequest,
        prompt_state: Option<PolkitPromptState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let is_visible = prompt_state
            .as_ref()
            .map(|p| p.response_visible)
            .unwrap_or(false);

        let input_state = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            if !is_visible {
                state = state.masked(true);
            }
            state.placeholder("Enter response...")
        });

        let focus_handle = cx.focus_handle();

        Self {
            request,
            prompt_state,
            input_state,
            focus_handle,
        }
    }

    pub fn update_state(
        &mut self,
        request: PolkitRequest,
        prompt_state: Option<PolkitPromptState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_visible = prompt_state
            .as_ref()
            .map(|p| p.response_visible)
            .unwrap_or(false);

        self.input_state.update(cx, |this, cx| {
            this.set_masked(!is_visible, window, cx);
        });

        self.request = request;
        self.prompt_state = prompt_state;
        cx.notify();
    }

    fn submit_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).text().to_string();
        let cookie = self.request.cookie.clone();

        // Clear input state immediately
        self.input_state.update(cx, |this, cx| {
            this.set_value("", window, cx);
        });

        let polkit = ShellRuntime::polkit(cx);
        polkit.provide_response(&cookie, text);
    }

    fn cancel_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let cookie = self.request.cookie.clone();
        let polkit = ShellRuntime::polkit(cx);
        polkit.cancel_request(&cookie);
    }

    fn select_identity(&mut self, username: String, cx: &mut Context<Self>) {
        let cookie = self.request.cookie.clone();
        let polkit = ShellRuntime::polkit(cx);
        polkit.select_identity(&cookie, &username);
    }
}

impl Focusable for PolkitDialogView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PolkitDialogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prompt_req = self
            .prompt_state
            .as_ref()
            .map(|p| p.response_required)
            .unwrap_or(false);

        let input_prompt = self
            .prompt_state
            .as_ref()
            .and_then(|p| p.input_prompt.clone())
            .unwrap_or_else(|| "Password:".to_string());

        let supp_msg = self
            .prompt_state
            .as_ref()
            .and_then(|p| p.supplementary_message.clone());

        let is_supp_err = self
            .prompt_state
            .as_ref()
            .map(|p| p.supplementary_is_error)
            .unwrap_or(false);

        let has_selected_id =
            self.request.selected_identity.is_some() || self.request.identities.len() <= 1;

        // Container card
        v_flex()
            .track_focus(&self.focus_handle)
            .focus_trap("polkit-dialog-trap", &self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_dialog(window, cx);
                } else if event.keystroke.key == "enter" {
                    this.submit_response(window, cx);
                }
            }))
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .bg(cx.theme().scrim.opacity(0.45))
            .child(
                v_flex()
                    .w(px(460.))
                    .p_6()
                    .gap_4()
                    .rounded_3xl()
                    .bg(cx.theme().surface_container_high)
                    .text_color(cx.theme().on_surface)
                    .border_1()
                    .border_color(cx.theme().outline_variant.opacity(0.4))
                    .shadow_2xl()
                    // Header section
                    .child(
                        h_flex()
                            .items_start()
                            .gap_4()
                            .child(
                                div()
                                    .p_3()
                                    .rounded_2xl()
                                    .bg(cx.theme().primary_container)
                                    .text_color(cx.theme().on_primary_container)
                                    .child(
                                        shilpo_services::applications::icons::lookup_icon(
                                            &self.request.icon_name,
                                        )
                                        .map(|path| {
                                            gpui::img(path).w(px(24.)).h(px(24.)).into_any_element()
                                        })
                                        .unwrap_or_else(
                                            || {
                                                Icon::new(IconName::Info)
                                                    .size(px(24.))
                                                    .into_any_element()
                                            },
                                        ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap_2()
                                            .child(div().text_base().font_bold().child(
                                                if self.request.is_internal {
                                                    "System Action Authorization"
                                                } else {
                                                    "Authentication Required"
                                                },
                                            ))
                                            .when(self.request.is_internal, |this| {
                                                this.child(
                                                    div()
                                                        .px_2()
                                                        .py_0p5()
                                                        .rounded_full()
                                                        .bg(cx.theme().secondary_container)
                                                        .text_color(
                                                            cx.theme().on_secondary_container,
                                                        )
                                                        .text_xs()
                                                        .child("Internal"),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().on_surface_variant)
                                            .child(self.request.message.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().outline)
                                            .child(format!("Action: {}", self.request.action_id)),
                                    ),
                            ),
                    )
                    // Multi-identity selector if not chosen yet
                    .when(!has_selected_id, |this| {
                        let identities = self.request.identities.clone();
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(cx.theme().on_surface_variant)
                                        .child("Select identity to authenticate as:"),
                                )
                                .children(identities.into_iter().map(|id| {
                                    let username = id.user_name.clone();
                                    let display_name = id
                                        .real_name
                                        .clone()
                                        .unwrap_or_else(|| id.user_name.clone());

                                    let user_clone = username.clone();
                                    Button::new(format!("user-{}", username))
                                        .label(format!("{} ({})", display_name, username))
                                        .icon(IconName::Person)
                                        .outlined()
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            this.select_identity(user_clone.clone(), cx);
                                        }))
                                })),
                        )
                    })
                    // Supplementary message (e.g. error or info)
                    .when_some(supp_msg, |this, msg| {
                        let (bg, fg) = if is_supp_err {
                            (cx.theme().error_container, cx.theme().on_error_container)
                        } else {
                            (cx.theme().surface_container_highest, cx.theme().on_surface)
                        };

                        this.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded_xl()
                                .bg(bg)
                                .text_color(fg)
                                .text_xs()
                                .child(msg),
                        )
                    })
                    // Prompt Input
                    .when(has_selected_id && prompt_req, |this| {
                        this.child(
                            v_flex()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(cx.theme().on_surface_variant)
                                        .child(input_prompt),
                                )
                                .child(Input::new(&self.input_state).cleanable(true)),
                        )
                    })
                    // Footer Actions
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .pt_2()
                            .child(Button::new("cancel-btn").label("Cancel").text().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.cancel_dialog(window, cx);
                                }),
                            ))
                            .when(has_selected_id && prompt_req, |this| {
                                this.child(
                                    Button::new("auth-btn")
                                        .label("Authenticate")
                                        .filled()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_response(window, cx);
                                        })),
                                )
                            }),
                    ),
            )
    }
}
