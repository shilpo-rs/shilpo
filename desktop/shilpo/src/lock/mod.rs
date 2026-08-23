use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions, div, point, prelude::FluentBuilder as _, px,
    session_lock::SessionLockOptions,
};
use shilpo_m3e::{
    ActiveTheme, Icon, IconName, StyledExt,
    input::{Input, InputState},
    v_flex,
};
use shilpo_services::auth::{AuthCommand, AuthOutcome, AuthPort, AuthService, AuthSnapshot};

/// Entry point for the `shilpo lock` process role. Owns the session lock and the PAM
/// authentication domain; nothing else. See ADR-0005 and issue #135 for why this is a
/// dedicated process rather than in-process in `shilpo daemon`: a client that dies while
/// the session is locked may leave it locked permanently, so nothing that can crash for
/// unrelated reasons (extensions, Wasmtime, compositor adapters) shares this process.
pub async fn run_lock() {
    let config_path = crate::config::default_config_path();
    let config = crate::config::ShellConfig::load_or_create(&config_path).unwrap_or_default();
    let pam_service = config.lock.pam_service.clone();
    let clear_input_after_ms = (config.lock.clear_input_after_seconds as u64) * 1000;

    let auth: Arc<dyn AuthPort> = Arc::new(AuthService::new());
    // Kick off the PAM conversation immediately so the first prompt is ready by the time
    // the surface is visible.
    auth.begin_authentication(&pam_service);

    let app = gpui_platform::application().with_assets(crate::Assets);

    app.run(move |cx: &mut App| {
        shilpo_m3e::init(cx);

        let lock = match cx.lock_session() {
            Ok(lock) => lock,
            Err(err) => {
                eprintln!("session lock unavailable: {err}");
                cx.quit();
                return;
            }
        };

        let displays = cx.displays();
        if displays.is_empty() {
            eprintln!("no displays reported; nothing to lock");
            cx.quit();
            return;
        }

        let unlocked = Arc::new(std::sync::atomic::AtomicBool::new(false));

        for display in &displays {
            let bounds = display.bounds();
            let auth = auth.clone();
            let lock = lock.clone();
            let unlocked = unlocked.clone();
            let pam_service = pam_service.clone();

            let _ = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.), px(0.)),
                        size: bounds.size,
                    })),
                    app_id: Some("org.shilpo.lock".into()),
                    window_background: WindowBackgroundAppearance::Opaque,
                    display_id: Some(display.id()),
                    kind: WindowKind::SessionLock(SessionLockOptions { lock: lock.clone() }),
                    ..Default::default()
                },
                move |window, cx| {
                    cx.new(|cx| {
                        LockView::new(
                            auth,
                            lock,
                            unlocked,
                            pam_service,
                            clear_input_after_ms,
                            window,
                            cx,
                        )
                    })
                },
            );
        }

        lock.on_locked(Box::new(|| {
            tracing::info!("session locked: every output's surface is committed");
            // Tells a waiting PrepareForSleep watch (if this was a suspend-triggered
            // spawn) that it's now safe to release its delay inhibitor. No-op if
            // SHILPO_LOCK_READY_FD wasn't set (every other trigger).
            shilpo_services::lock_supervisor::signal_lock_ready();
        }));
        lock.on_finished(Box::new(|| {
            // Per protocol this means the lock was denied or lost, never that the
            // session is unlocked. Exiting here is safe: if the compositor denied the
            // lock, nothing was ever shown; if it revoked an active lock, the compositor
            // itself owns fallback behavior from here.
            tracing::error!("session lock finished (denied or lost)");
            std::process::exit(1);
        }));
    });
}

struct LockView {
    auth: Arc<dyn AuthPort>,
    lock: Rc<dyn gpui::session_lock::PlatformSessionLock>,
    unlocked: Arc<std::sync::atomic::AtomicBool>,
    pam_service: String,
    input_state: Entity<InputState>,
    focus_handle: FocusHandle,
    status: Option<(String, bool)>,
    prompt_label: String,
    clear_input_after_ms: u64,
    _poll_task: gpui::Task<()>,
    _clear_timer: Option<gpui::Task<()>>,
    _refocus_task: gpui::Task<()>,
}

impl LockView {
    #[allow(clippy::too_many_arguments)]
    fn new(
        auth: Arc<dyn AuthPort>,
        lock: Rc<dyn gpui::session_lock::PlatformSessionLock>,
        unlocked: Arc<std::sync::atomic::AtomicBool>,
        pam_service: String,
        clear_input_after_ms: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Password:")
        });

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let poll_task = {
            let auth = auth.clone();
            let mut rx = auth.subscribe();
            let this = cx.weak_entity();
            window.spawn(cx, async move |cx| {
                loop {
                    if rx.changed().await.is_err() {
                        return;
                    }
                    let snapshot = rx.borrow().clone();
                    let Some(this) = this.upgrade() else {
                        return;
                    };
                    let result = cx.update(|window, cx| {
                        this.update(cx, |view, cx| {
                            view.apply_snapshot(snapshot, window, cx);
                        })
                    });
                    if result.is_err() {
                        return;
                    }
                }
            })
        };

        // Some compositors (Hyprland, per field reports on similar wlroots-based lockers)
        // drop keyboard focus off the lock surface after resume from suspend for reasons
        // that are hard to detect generically from the client side. Rather than trying to
        // subscribe to a compositor-specific signal, periodically re-assert focus: cheap,
        // self-healing regardless of the cause, and a no-op when focus was never lost.
        let refocus_task = {
            let this = cx.weak_entity();
            window.spawn(cx, async move |cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    let Some(this) = this.upgrade() else { return };
                    let result = cx.update(|window, cx| {
                        this.update(cx, |view, cx| {
                            if !view.focus_handle.is_focused(window) {
                                view.focus_handle.focus(window, cx);
                            }
                        })
                    });
                    if result.is_err() {
                        return;
                    }
                }
            })
        };

        Self {
            auth,
            lock,
            unlocked,
            pam_service,
            input_state,
            focus_handle,
            status: None,
            prompt_label: "Password:".to_string(),
            clear_input_after_ms,
            _poll_task: poll_task,
            _clear_timer: None,
            _refocus_task: refocus_task,
        }
    }

    fn submit_response(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).value().to_string();
        if text.is_empty() {
            return;
        }
        self.auth.provide_response(text);
        self.input_state.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
    }

    fn schedule_input_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let delay = Duration::from_millis(self.clear_input_after_ms);
        let this = cx.weak_entity();
        self._clear_timer = Some(window.spawn(cx, async move |cx| {
            cx.background_executor().timer(delay).await;
            let Some(this) = this.upgrade() else { return };
            let _ = cx.update(|window, cx| {
                this.update(cx, |view, cx| {
                    view.input_state.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                    });
                });
            });
        }));
    }

    fn apply_snapshot(
        &mut self,
        snapshot: AuthSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(prompt) = &snapshot.prompt_state {
            if let Some(label) = &prompt.input_prompt
                && !label.is_empty()
                && *label != self.prompt_label
            {
                self.prompt_label = label.clone();
                let placeholder = self.prompt_label.clone();
                self.input_state.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx);
                });
            }
            self.input_state.update(cx, |state, cx| {
                state.set_masked(!prompt.response_visible, window, cx);
            });
            if let Some(message) = &prompt.supplementary_message {
                self.status = Some((message.clone(), prompt.supplementary_is_error));
            }
        }

        match snapshot.last_outcome {
            Some(AuthOutcome::Succeeded) => {
                if !self
                    .unlocked
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    self.lock.unlock_and_destroy();
                    cx.quit();
                }
            }
            Some(AuthOutcome::Failed { ref message }) => {
                self.status = Some((message.clone(), true));
                self.input_state.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                });
                // Retry automatically: a fresh PAM conversation for the next attempt.
                let _ = self.auth.submit_command(AuthCommand::BeginAuthentication {
                    service: self.pam_service.clone(),
                });
                self.schedule_input_clear(window, cx);
            }
            None => {}
        }
        cx.notify();
    }
}

impl Focusable for LockView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = chrono::Local::now();
        let time_text = now.format("%H:%M").to_string();
        let date_text = now.format("%A, %B %-d").to_string();
        let username = whoami();
        let caps_lock_on = window.capslock().on;
        let keyboard_layout_name = cx.keyboard_layout().name().to_string();

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" {
                    this.submit_response(window, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_8()
            .bg(cx.theme().surface)
            .text_color(cx.theme().on_surface)
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_size(px(72.)).font_bold().child(time_text))
                    .child(
                        div()
                            .text_size(px(18.))
                            .text_color(cx.theme().on_surface_variant)
                            .child(date_text),
                    ),
            )
            .child(
                v_flex()
                    .items_center()
                    .gap_3()
                    .w(px(320.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::Person).size(px(20.)))
                            .child(div().text_base().child(username)),
                    )
                    .child(Input::new(&self.input_state).w_full())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().on_surface_variant)
                            .child(keyboard_layout_name)
                            .when(caps_lock_on, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(cx.theme().error)
                                        .child(Icon::new(IconName::KeyboardArrowUp).size(px(14.)))
                                        .child("Caps Lock is on"),
                                )
                            }),
                    )
                    .children(self.status.as_ref().map(|(message, is_error)| {
                        div()
                            .text_xs()
                            .text_color(if *is_error {
                                cx.theme().error
                            } else {
                                cx.theme().on_surface_variant
                            })
                            .child(message.clone())
                    })),
            )
    }
}

fn whoami() -> String {
    let uid = unsafe { libc::getuid() };
    let mut buf = vec![0i8; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result as *mut _,
        )
    };
    if rc == 0 && !result.is_null() {
        unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }
            .to_string_lossy()
            .into_owned()
    } else {
        "user".to_string()
    }
}
