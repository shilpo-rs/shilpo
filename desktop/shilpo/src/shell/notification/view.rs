use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, AppContext, Context, ElementId, Entity, InteractiveElement,
    IntoElement, ParentElement, Render, Role, StatefulInteractiveElement, Styled, Window, div, img,
    prelude::FluentBuilder, px,
};
use shilpo_m3e::{
    ActiveTheme, Colorize, Icon, IconName, StyledExt, animation::cubic_bezier, h_flex, v_flex,
};
use shilpo_services::Notification;

use crate::shell::runtime::ShellRuntime;
use crate::shell::runtime::shell_surfaces::NotificationLifecycleCallback;

#[derive(Clone)]
pub(crate) struct ToastEntry {
    pub(crate) notification: Notification,
    pub(crate) generation: NotificationLifecycleCallback,
    pub(crate) timeout: Option<Duration>,
}

/// OSD Toast Notification View.
pub struct NotificationToastView {
    pub(crate) stack: VecDeque<ToastEntry>,
    pub(crate) bar_position: crate::config::BarPosition,
    pub(crate) expanded: bool,
    pub(crate) unfolded_apps: HashSet<String>,
    pub(crate) hovered: bool,
    pub(crate) entering: bool,
    pub(crate) closing: bool,
    pub(crate) autohide_task: Option<gpui::Task<()>>,
}

impl NotificationToastView {
    pub(crate) fn new(
        notification: Notification,
        generation: NotificationLifecycleCallback,
        timeout: Option<Duration>,
        bar_position: crate::config::BarPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        window.on_window_should_close(cx, move |_, cx| {
            generation.forgotten(cx);
            true
        });
        let mut stack = VecDeque::new();
        stack.push_front(ToastEntry {
            notification,
            generation,
            timeout,
        });

        let mut this = Self {
            stack,
            bar_position,
            expanded: false,
            unfolded_apps: HashSet::new(),
            hovered: false,
            entering: true,
            closing: false,
            autohide_task: None,
        };
        this.reset_autohide_timer(window, cx);
        this.schedule_enter_completion(window, cx);

        this
    }

    fn schedule_enter_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            _ = view.update_in(cx, |this, _, cx| {
                this.entering = false;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn view(
        notification: Notification,
        generation: NotificationLifecycleCallback,
        timeout: Option<Duration>,
        bar_position: crate::config::BarPosition,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self::new(notification, generation, timeout, bar_position, window, cx))
    }

    pub(crate) fn push(
        &mut self,
        notification: Notification,
        generation: NotificationLifecycleCallback,
        timeout: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A dismissal may still be animating the window out (closing == true).
        // A notification pushed in that window must restore normal motion, or
        // the toast renders transparent but the window keeps eating pointer
        // input over the top-right corner, and autohide stays disabled. The
        // stale removal task is harmless: its generation no longer matches,
        // so it will not tear the window down. clearing also drops the entry
        // that was already dismissed.
        if self.closing {
            self.stack.clear();
            self.unfolded_apps.clear();
            self.closing = false;
            self.entering = true;
            self.schedule_enter_completion(window, cx);
            tracing::debug!("push notification while closing, resetting flags");
        }

        self.stack.push_front(ToastEntry {
            notification,
            generation,
            timeout,
        });
        tracing::debug!(
            generation = %generation,
            stack_len = self.stack.len(),
            "notification toast pushed"
        );
        self.reset_autohide_timer(window, cx);
        cx.notify();
    }

    pub fn toggle_unfold_app(
        &mut self,
        app_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.unfolded_apps.contains(app_name) {
            self.unfolded_apps.remove(app_name);
        } else {
            self.unfolded_apps.insert(app_name.to_string());
        }
        self.reset_autohide_timer(window, cx);
        cx.notify();
    }

    pub(crate) fn dismiss_gen(
        &mut self,
        generation: NotificationLifecycleCallback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.closing {
            return;
        }

        if self.stack.len() > 1 {
            if let Some(pos) = self.stack.iter().position(|e| e.generation == generation) {
                let popped = self.stack.remove(pos);
                if let Some(entry) = popped {
                    entry.generation.forgotten(cx);
                }
            }
            if self.stack.len() <= 1 {
                self.unfolded_apps.clear();
            }
            self.reset_autohide_timer(window, cx);
            cx.notify();
            return;
        }

        self.dismiss(window, cx);
    }

    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing {
            tracing::debug!("notification toast dismiss rejected: already closing");
            return;
        }

        if self.stack.len() > 1
            && let Some(popped) = self.stack.pop_front()
        {
            tracing::debug!(
                popped_gen = %popped.generation,
                "popped notification toast from stack"
            );
            popped.generation.forgotten(cx);
            if self.stack.len() <= 1 {
                self.unfolded_apps.clear();
            }
            self.reset_autohide_timer(window, cx);
            cx.notify();
            return;
        }

        tracing::debug!("dismissing final notification toast");
        self.closing = true;
        self.autohide_task = None;
        cx.notify();

        let top_gen = self.stack.front().map(|e| e.generation);
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(220))
                .await;
            _ = cx.update(|_window, cx| {
                if let Some(target_gen) = top_gen {
                    target_gen.expired(cx);
                }
            });
        })
        .detach();
    }

    pub fn reset_autohide_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hovered || self.expanded || !self.unfolded_apps.is_empty() || self.closing {
            self.autohide_task = None;
            return;
        }

        let Some(top) = self.stack.front().cloned() else {
            self.autohide_task = None;
            return;
        };

        let Some(timeout) = top.timeout else {
            self.autohide_task = None;
            return;
        };

        let generation = top.generation;
        self.autohide_task = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor().timer(timeout).await;
            _ = view
                .update_in(cx, |this, window, cx| {
                    this.dismiss(window, cx);
                })
                .or_else(|_| cx.update(|_window, cx| generation.expired(cx)));
        }));
    }
}

fn render_icon_badge(
    notification: &Notification,
    cx: &Context<NotificationToastView>,
    size_px: f32,
    img_size_px: f32,
) -> gpui::AnyElement {
    let resolved_path = notification
        .desktop_entry
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(shilpo_services::applications::icons::lookup_icon)
        .or_else(|| {
            notification
                .app_icon
                .as_deref()
                .filter(|s| {
                    !s.is_empty()
                        && *s != "dialog-information"
                        && *s != "dialog-warning"
                        && *s != "dialog-error"
                        && *s != "notification"
                        && *s != "bell"
                })
                .and_then(shilpo_services::applications::icons::lookup_icon)
        })
        .or_else(|| {
            if !notification.app_name.is_empty() {
                shilpo_services::applications::icons::lookup_icon(&notification.app_name)
            } else {
                None
            }
        })
        .or_else(|| {
            notification
                .app_icon
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(shilpo_services::applications::icons::lookup_icon)
        });

    if let Some(path) = resolved_path {
        div()
            .w(px(size_px))
            .h(px(size_px))
            .flex_shrink_0()
            .rounded_2xl()
            .bg(cx.theme().surface_container_highest.opacity(0.5))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .child(img(path).w(px(img_size_px)).h(px(img_size_px)))
            .into_any_element()
    } else {
        div()
            .w(px(size_px))
            .h(px(size_px))
            .flex_shrink_0()
            .rounded_2xl()
            .bg(cx.theme().primary_container)
            .text_color(cx.theme().on_primary_container)
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::Notifications).size(px(img_size_px)))
            .into_any_element()
    }
}

impl Render for NotificationToastView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.stack.is_empty() {
            return div().id("toast-empty-root");
        }

        let is_bottom = self.bar_position == crate::config::BarPosition::Bottom;
        let slide_right = matches!(
            self.bar_position,
            crate::config::BarPosition::Top
                | crate::config::BarPosition::Bottom
                | crate::config::BarPosition::Right
        );
        let closing = self.closing;
        let entering = self.entering;

        let (anim_id, easing, duration) = if closing {
            (
                ElementId::NamedInteger("toast-motion-anim".into(), 2),
                cubic_bezier(0.4, 0.0, 1.0, 1.0),
                Duration::from_millis(220),
            )
        } else if entering {
            (
                ElementId::NamedInteger("toast-motion-anim".into(), 1),
                cubic_bezier(0.05, 0.7, 0.1, 1.0),
                Duration::from_millis(250),
            )
        } else {
            (
                ElementId::NamedInteger("toast-motion-anim".into(), 0),
                cubic_bezier(0.0, 0.0, 1.0, 1.0),
                Duration::from_millis(1),
            )
        };

        let root_flex = if slide_right {
            h_flex()
                .w_full()
                .h_full()
                .when(is_bottom, |this| this.items_end())
                .when(!is_bottom, |this| this.items_start())
                .justify_end()
        } else {
            h_flex()
                .w_full()
                .h_full()
                .when(is_bottom, |this| this.items_end())
                .when(!is_bottom, |this| this.items_start())
                .justify_start()
        };

        // Group notifications strictly by application name
        let mut groups: Vec<(String, Vec<ToastEntry>)> = Vec::new();
        for entry in &self.stack {
            let app_name = if entry.notification.app_name.is_empty() {
                "Notification".to_string()
            } else {
                entry.notification.app_name.clone()
            };
            if let Some(group) = groups.iter_mut().find(|(name, _)| name == &app_name) {
                group.1.push(entry.clone());
            } else {
                groups.push((app_name, vec![entry.clone()]));
            }
        }

        let group_cards = groups.into_iter().enumerate().map(|(g_idx, (app_name, app_entries))| {
            let is_unfolded = self.unfolded_apps.contains(&app_name);
            let count = app_entries.len();
            let app_name_clone1 = app_name.clone();
            let app_name_clone2 = app_name.clone();

            if is_unfolded && count > 1 {
                // Unfolded list for this specific app
                v_flex()
                    .id(ElementId::NamedInteger("toast-app-unfolded".into(), g_idx as u64))
                    .gap_2()
                    .w(px(368.))
                    .children(app_entries.into_iter().enumerate().map(|(idx, entry)| {
                        let notif = &entry.notification;
                        let target_gen = entry.generation;
                        let (card_bg, card_fg) = match notif.urgency {
                            shilpo_services::NotificationUrgency::Critical => (
                                cx.theme().error_container,
                                cx.theme().on_error_container,
                            ),
                            shilpo_services::NotificationUrgency::Normal => (
                                cx.theme().surface_container_high,
                                cx.theme().on_surface,
                            ),
                            shilpo_services::NotificationUrgency::Low => (
                                cx.theme().surface_container,
                                cx.theme().on_surface_variant,
                            ),
                        };
                        let name = if notif.app_name.is_empty() {
                            "Notification".to_string()
                        } else {
                            notif.app_name.clone()
                        };
                        let card_icon = render_icon_badge(notif, cx, 36.0, 26.0);
                        let app_name_collapse = app_name_clone1.clone();
                        let notif_has_image = notif.image_path.is_some();
                        let notif_has_actions = !notif.actions.is_empty();
                        let notif_has_expandable = notif_has_image || notif_has_actions;

                        h_flex()
                            .w(px(368.))
                            .p_3p5()
                            .gap_3()
                            .rounded_3xl()
                            .overflow_hidden()
                            .bg(card_bg)
                            .text_color(card_fg)
                            .border_1()
                            .border_color(cx.theme().outline_variant.opacity(0.3))
                            .shadow_md()
                            .items_start()
                            .child(card_icon)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .items_center()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(cx.theme().primary)
                                                            .child(name),
                                                    )
                                                    .when(idx == 0, |this| {
                                                        this.child(
                                                            div()
                                                                .id(ElementId::NamedInteger("toast-app-collapse-btn".into(), g_idx as u64))
                                                                .role(Role::Button)
                                                                .px_2()
                                                                .py_0p5()
                                                                .rounded_full()
                                                                .bg(cx.theme().primary_container)
                                                                .text_color(cx.theme().on_primary_container)
                                                                .text_xs()
                                                                .font_semibold()
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                                    this.toggle_unfold_app(&app_name_collapse, window, cx);
                                                                }))
                                                                .child("Collapse"),
                                                        )
                                                    }),
                                            )
                                            .child(
                                                h_flex()
                                                    .flex_shrink_0()
                                                    .gap_1()
                                                    .items_center()
                                                    .when(notif_has_expandable, |this| {
                                                        let unf_toggle_btn = div()
                                                            .id(ElementId::NamedInteger("toast-unf-expand-toggle".into(), (g_idx * 100 + idx) as u64))
                                                            .role(Role::Button)
                                                            .p_1()
                                                            .flex_shrink_0()
                                                            .rounded_full()
                                                            .bg(cx.theme().surface_container.opacity(0.6))
                                                            .text_color(cx.theme().on_surface_variant)
                                                            .cursor_pointer()
                                                            .hover(|s| {
                                                                s.bg(cx.theme().surface_container_highest)
                                                                    .text_color(cx.theme().on_surface)
                                                            })
                                                            .on_click(cx.listener(|this, _, window, cx| {
                                                                this.expanded = !this.expanded;
                                                                this.reset_autohide_timer(window, cx);
                                                                cx.notify();
                                                            }))
                                                            .child(Icon::new(if self.expanded { IconName::KeyboardArrowUp } else { IconName::KeyboardArrowDown }).size(px(16.)));
                                                        this.child(unf_toggle_btn)
                                                    })
                                                    .child(
                                                        div()
                                                            .id(ElementId::NamedInteger("toast-unfolded-close".into(), (g_idx * 100 + idx) as u64))
                                                            .role(Role::Button)
                                                            .p_1()
                                                            .rounded_full()
                                                            .bg(cx.theme().surface_container.opacity(0.6))
                                                            .text_color(cx.theme().on_surface_variant)
                                                            .cursor_pointer()
                                                            .hover(|s| s.bg(cx.theme().surface_container_highest))
                                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                                this.dismiss_gen(target_gen, window, cx);
                                                            }))
                                                            .child(Icon::new(IconName::Close).size(px(14.))),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .text_sm()
                                            .font_semibold()
                                            .text_color(cx.theme().on_surface)
                                            .child(notif.summary.clone()),
                                    )
                                    .when(!notif.body.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .w_full()
                                                .text_xs()
                                                .text_color(cx.theme().on_surface_variant)
                                                .child(notif.body.clone()),
                                        )
                                    })
                                    .when(notif_has_expandable && self.expanded, |this| {
                                        let unf_actions = notif.actions.clone();
                                        let unf_notif_id = notif.id;
                                        this.child(
                                            v_flex()
                                                .w_full()
                                                .gap_2()
                                                .pt_2()
                                                .when_some(notif.image_path.clone(), |this, img_path| {
                                                    this.child(
                                                        div()
                                                            .w_full()
                                                            .max_h(px(160.))
                                                            .rounded_2xl()
                                                            .overflow_hidden()
                                                            .child(img(img_path).w_full()),
                                                    )
                                                })
                                                .when(!unf_actions.is_empty(), |this| {
                                                    this.child(
                                                        h_flex()
                                                            .flex_wrap()
                                                            .gap_2()
                                                            .items_center()
                                                            .children(unf_actions.into_iter().enumerate().map(|(i, (key, label))| {
                                                                let key_clone = key.clone();
                                                                div()
                                                                    .id(ElementId::NamedInteger("toast-unf-action-btn".into(), (g_idx * 1000 + idx * 10 + i) as u64))
                                                                    .role(Role::Button)
                                                                    .px_3()
                                                                    .py_1()
                                                                    .rounded_full()
                                                                    .bg(cx.theme().primary_container)
                                                                    .text_color(cx.theme().on_primary_container)
                                                                    .text_xs()
                                                                    .font_semibold()
                                                                    .cursor_pointer()
                                                                    .hover(|s| s.bg(cx.theme().primary_container.darken(0.1)))
                                                                    .on_click(cx.listener(move |this, _, window, cx| {
                                                                        ShellRuntime::invoke_notification_action(cx, unf_notif_id, &key_clone);
                                                                        this.dismiss_gen(target_gen, window, cx);
                                                                    }))
                                                                    .child(label)
                                                            }))
                                                    )
                                                })
                                        )
                                    }),
                            )
                    }))
                    .into_any_element()
            } else {
                // Render Deck Stack for this app
                let top_entry = &app_entries[0];
                let notification = &top_entry.notification;
                let top_gen = top_entry.generation;

                let app_icon = render_icon_badge(notification, cx, 36.0, 26.0);
                let (bg, fg) = match notification.urgency {
                    shilpo_services::NotificationUrgency::Critical => (
                        cx.theme().error_container,
                        cx.theme().on_error_container,
                    ),
                    shilpo_services::NotificationUrgency::Normal => (
                        cx.theme().surface_container_high,
                        cx.theme().on_surface,
                    ),
                    shilpo_services::NotificationUrgency::Low => (
                        cx.theme().surface_container,
                        cx.theme().on_surface_variant,
                    ),
                };

                let name = if notification.app_name.is_empty() {
                    "Notification".to_string()
                } else {
                    notification.app_name.clone()
                };

                let toggle_icon = if self.expanded {
                    IconName::KeyboardArrowUp
                } else {
                    IconName::KeyboardArrowDown
                };

                let toggle_btn = div()
                    .id(ElementId::NamedInteger("toast-expand-toggle-btn".into(), g_idx as u64))
                    .role(Role::Button)
                    .aria_label("Toggle notification actions")
                    .p_1()
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(cx.theme().surface_container.opacity(0.6))
                    .text_color(cx.theme().on_surface_variant)
                    .cursor_pointer()
                    .hover(|s| {
                        s.bg(cx.theme().surface_container_highest)
                            .text_color(cx.theme().on_surface)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.expanded = !this.expanded;
                        this.reset_autohide_timer(window, cx);
                        cx.notify();
                    }))
                    .child(Icon::new(toggle_icon).size(px(16.)));

                let peek_card_2 = if count >= 3 {
                    let app_peek_name = app_name_clone1.clone();
                    Some(
                        div()
                            .id(ElementId::NamedInteger("toast-peek-card-2".into(), g_idx as u64))
                            .absolute()
                            .w(px(328.))
                            .left(px(16.))
                            .bottom(px(-13.))
                            .h(px(24.))
                            .rounded_2xl()
                            .bg(cx.theme().surface_container_lowest)
                            .border_1()
                            .border_color(cx.theme().outline_variant.opacity(0.3))
                            .shadow_sm()
                            .opacity(0.7)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_unfold_app(&app_peek_name, window, cx);
                            })),
                    )
                } else {
                    None
                };

                let peek_card_1 = if count >= 2 {
                    let app_peek_name = app_name_clone1.clone();
                    Some(
                        div()
                            .id(ElementId::NamedInteger("toast-peek-card-1".into(), g_idx as u64))
                            .absolute()
                            .w(px(344.))
                            .left(px(8.))
                            .bottom(px(-7.))
                            .h(px(24.))
                            .rounded_2xl()
                            .bg(cx.theme().surface_container)
                            .border_1()
                            .border_color(cx.theme().outline_variant.opacity(0.4))
                            .shadow_md()
                            .opacity(0.9)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_unfold_app(&app_peek_name, window, cx);
                            })),
                    )
                } else {
                    None
                };

                let summary = notification.summary.clone();
                let body = notification.body.clone();
                let app_unfold_name = app_name_clone2.clone();
                let has_image = notification.image_path.is_some();
                let has_actions = !notification.actions.is_empty();
                let has_expandable = has_image || has_actions;

                let bottom_margin = if count >= 3 {
                    px(16.)
                } else if count >= 2 {
                    px(10.)
                } else {
                    px(0.)
                };

                v_flex().w(px(368.)).mb(bottom_margin).child(
                    div()
                        .relative()
                        .w(px(368.))
                        .children(peek_card_2)
                        .children(peek_card_1)
                        .child(
                            h_flex()
                                .w(px(368.))
                                .relative()
                                .p_3p5()
                                .gap_3()
                                .rounded_3xl()
                                .overflow_hidden()
                                .bg(bg)
                                .text_color(fg)
                                .border_1()
                                .border_color(cx.theme().outline_variant.opacity(0.3))
                                .shadow_md()
                                .items_start()
                                .child(app_icon)
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    h_flex()
                                                        .gap_1p5()
                                                        .items_center()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .font_semibold()
                                                                .text_color(cx.theme().primary)
                                                                .child(name),
                                                        )
                                                        .when(count > 1, |this| {
                                                            this.child(
                                                                div()
                                                                    .id(ElementId::NamedInteger("toast-cycle-badge".into(), g_idx as u64))
                                                                    .role(Role::Button)
                                                                    .px_2()
                                                                    .py_0p5()
                                                                    .rounded_full()
                                                                    .bg(cx.theme().primary_container)
                                                                    .text_color(
                                                                        cx.theme().on_primary_container,
                                                                    )
                                                                    .text_xs()
                                                                    .font_semibold()
                                                                    .cursor_pointer()
                                                                    .hover(|s| {
                                                                        s.bg(cx.theme()
                                                                            .primary_container
                                                                            .darken(0.1))
                                                                    })
                                                                    .on_click(cx.listener(
                                                                        move |this, _, window, cx| {
                                                                            this.toggle_unfold_app(
                                                                                &app_unfold_name,
                                                                                window,
                                                                                cx,
                                                                            );
                                                                        },
                                                                    ))
                                                                    .child(format!("+{} more", count - 1)),
                                                            )
                                                        }),
                                                )
                                                .child(
                                                    h_flex()
                                                        .flex_shrink_0()
                                                        .gap_1()
                                                        .items_center()
                                                        .when(has_expandable, |this| this.child(toggle_btn))
                                                        .child(
                                                            div()
                                                                .id(ElementId::NamedInteger("toast-top-close-btn".into(), g_idx as u64))
                                                                .role(Role::Button)
                                                                .p_1()
                                                                .rounded_full()
                                                                .bg(cx.theme().surface_container.opacity(0.6))
                                                                .text_color(cx.theme().on_surface_variant)
                                                                .cursor_pointer()
                                                                .hover(|s| s.bg(cx.theme().surface_container_highest))
                                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                                    this.dismiss_gen(top_gen, window, cx);
                                                                }))
                                                                .child(Icon::new(IconName::Close).size(px(14.))),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .text_sm()
                                                .font_semibold()
                                                .text_color(cx.theme().on_surface)
                                                .child(summary),
                                        )
                                        .when(!body.is_empty(), |this| {
                                            this.child(
                                                div()
                                                    .w_full()
                                                    .text_xs()
                                                    .text_color(cx.theme().on_surface_variant)
                                                    .child(body),
                                            )
                                        })
                                        .when(has_expandable && self.expanded, |this| {
                                            let actions = notification.actions.clone();
                                            let notif_id = notification.id;
                                            this.child(
                                                v_flex()
                                                    .w_full()
                                                    .gap_2()
                                                    .pt_2()
                                                    .when_some(notification.image_path.clone(), |this, img_path| {
                                                        this.child(
                                                            div()
                                                                .w_full()
                                                                .max_h(px(160.))
                                                                .rounded_2xl()
                                                                .overflow_hidden()
                                                                .child(img(img_path).w_full()),
                                                        )
                                                    })
                                                    .when(has_actions, |this| {
                                                        this.child(
                                                            h_flex()
                                                                .flex_wrap()
                                                                .gap_2()
                                                                .items_center()
                                                                .children(actions.into_iter().enumerate().map(
                                                                    |(i, (key, label))| {
                                                                        let key_clone = key.clone();
                                                                        div()
                                                                            .id(ElementId::NamedInteger("toast-action-btn".into(), (g_idx * 100 + i) as u64))
                                                                            .role(Role::Button)
                                                                            .px_3()
                                                                            .py_1()
                                                                            .rounded_full()
                                                                            .bg(cx.theme().primary_container)
                                                                            .text_color(
                                                                                cx.theme().on_primary_container,
                                                                            )
                                                                            .text_xs()
                                                                            .font_semibold()
                                                                            .cursor_pointer()
                                                                            .hover(|s| {
                                                                                s.bg(cx.theme()
                                                                                    .primary_container
                                                                                    .darken(0.1))
                                                                            })
                                                                            .on_click(cx.listener(
                                                                                move |this, _, window, cx| {
                                                                                    ShellRuntime::invoke_notification_action(
                                                                                        cx,
                                                                                        notif_id,
                                                                                        &key_clone,
                                                                                    );
                                                                                    this.dismiss_gen(top_gen, window, cx);
                                                                                },
                                                                            ))
                                                                            .child(label)
                                                                    },
                                                                )),
                                                        )
                                                    }),
                                            )
                                        }),
                                ),
                        ),
                )
                    .into_any_element()
            }
        });

        let main_card_container = v_flex()
            // This layer-shell window is a transparent 376x420 surface in the
            // corner; by default the whole surface swallows pointer input even
            // where nothing is drawn. Constrain the input region to the cards
            // (plus the fold/peek cards that hang below them) so clicks over
            // the empty part of the window pass through to apps underneath.
            //
            // When closing, clear the input region immediately so the fading-
            // out window no longer blocks pointer events in the corner.
            .on_children_prepainted(move |child_bounds, window, _| {
                if closing {
                    window.set_input_region(Some(&[]));
                    return;
                }
                let mut region: Option<gpui::Bounds<gpui::Pixels>> = None;
                for bounds in child_bounds {
                    region = match region {
                        Some(acc) => Some(acc.union(&bounds)),
                        None => Some(bounds),
                    };
                }
                let Some(region) = region else {
                    window.set_input_region(Some(&[]));
                    return;
                };
                window.set_input_region(Some(&[region.dilate(px(18.))]));
            })
            .id("toast-scroll-container")
            .w(px(368.))
            .max_h(window.bounds().size.height)
            .overflow_y_scroll()
            .pr_0()
            .gap_3p5()
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if this.hovered != *hovered {
                    this.hovered = *hovered;
                    this.reset_autohide_timer(window, cx);
                    cx.notify();
                }
            }))
            .children(group_cards);

        div()
            .id("toast-notification-root")
            .w_full()
            .h_full()
            .child(root_flex.child(main_card_container.with_animation(
                anim_id,
                Animation::new(duration).with_easing(easing),
                move |this, delta| {
                    if closing {
                        let opacity = 1.0 - delta;
                        let x_offset = if slide_right {
                            px(120.) * delta
                        } else {
                            px(-120.) * delta
                        };
                        this.shadow_none().opacity(opacity).left(x_offset)
                    } else if entering {
                        let opacity = delta;
                        let x_offset = if slide_right {
                            px(120.) * (1.0 - delta)
                        } else {
                            px(-120.) * (1.0 - delta)
                        };
                        this.shadow_none().opacity(opacity).left(x_offset)
                    } else {
                        this
                    }
                },
            )))
    }
}
