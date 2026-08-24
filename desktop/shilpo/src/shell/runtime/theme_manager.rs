use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, AppContext};
use shilpo_theme_daemon::{DaemonState, ThemeClient};

use super::shell_surfaces::{self, ShellSurfaces};

#[derive(Debug)]
struct TransitionGate {
    generation: AtomicU64,
    revision: AtomicU64,
}

impl TransitionGate {
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            revision: AtomicU64::new(0),
        }
    }

    fn initialize_revision(&self, revision: u64) {
        self.revision.store(revision, Ordering::SeqCst);
    }

    fn supersede(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn current_revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    fn commit_revision(&self, revision: u64) {
        self.revision.store(revision, Ordering::SeqCst);
    }
}

static TRANSITION_GATE: TransitionGate = TransitionGate::new();

/// M3 Emphasized Easing: cubic-bezier(0.2, 0.0, 0.0, 1.0)
pub fn cubic_bezier_emphasized(t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let mut u = t;
    for _ in 0..8 {
        let x = 0.6 * (1.0 - u) * (1.0 - u) * u + u * u * u;
        let dx_du = 0.6 * (1.0 - u) * (1.0 - 3.0 * u) + 3.0 * u * u;
        if dx_du.abs() < 1e-6 {
            break;
        }
        let err = x - t;
        u -= err / dx_du;
        u = u.clamp(0.0, 1.0);
    }
    let y = 3.0 * (1.0 - u) * u * u + u * u * u;
    y.clamp(0.0, 1.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeTransition {
    pub generation: u64,
    pub from_revision: u64,
    pub to_revision: u64,
    pub start_colors: shilpo_m3e::ThemeColor,
    pub target_colors: shilpo_m3e::ThemeColor,
    pub target_state: DaemonState,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialThemeState {
    pub wallpaper_path: Option<PathBuf>,
    pub mode: String,
    pub scheme_variant: String,
}

impl ThemeTransition {
    pub fn new(
        generation: u64,
        from_revision: u64,
        to_revision: u64,
        start_colors: shilpo_m3e::ThemeColor,
        target_colors: shilpo_m3e::ThemeColor,
        target_state: DaemonState,
        duration_ms: u64,
    ) -> Self {
        Self {
            generation,
            from_revision,
            to_revision,
            start_colors,
            target_colors,
            target_state,
            duration_ms,
        }
    }

    pub fn progress_at(&self, elapsed_ms: u64) -> (f32, shilpo_m3e::ThemeColor, bool) {
        if self.duration_ms == 0 || elapsed_ms >= self.duration_ms {
            (1.0, self.target_colors, true)
        } else {
            let raw_t = elapsed_ms as f32 / self.duration_ms as f32;
            let eased_t = cubic_bezier_emphasized(raw_t);
            let current_colors = self.start_colors.interpolate(&self.target_colors, eased_t);
            (eased_t, current_colors, false)
        }
    }
}

pub fn init(cx: &mut App) -> InitialThemeState {
    let theme_client = futures_lite::future::block_on(ThemeClient::new());
    let initial_theme_state = theme_client.current_state();
    let wallpaper_path = initial_theme_state
        .wallpaper_path
        .clone()
        .filter(|path| path.is_file());
    shilpo_m3e::Theme::global_mut(cx).apply_state(&initial_theme_state);
    TRANSITION_GATE.initialize_revision(initial_theme_state.revision);

    let mut rx = theme_client.subscribe();
    let theme_client_for_task = theme_client.clone();
    cx.spawn(async move |cx| {
        loop {
            let update = match rx.recv().await {
                Ok(update) => update,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    theme_client_for_task.current_update()
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let state = update.state;
            let res = cx.update(|cx: &mut App| {
                let (reduced_motion, duration_ms) = if cx.has_global::<super::ShellRuntime>() {
                    let config = super::ShellRuntime::active_config(cx);
                    (
                        config.theme.reduced_motion,
                        config.theme.transition_duration_ms,
                    )
                } else {
                    (false, 300)
                };

                let target_colors = shilpo_m3e::material_theme_with_variant(
                    state.source_argb,
                    state.scheme_variant,
                    state.resolved_mode.is_dark(),
                );
                let start_colors = shilpo_m3e::Theme::global(cx).colors;

                if reduced_motion || duration_ms == 0 {
                    TRANSITION_GATE.supersede();
                    shilpo_m3e::Theme::global_mut(cx).apply_state(&state);
                    ShellSurfaces::apply_theme_state(cx, &state);
                    super::ShellRuntime::emit_theme_signal(
                        cx,
                        state.resolved_mode.as_str().into(),
                        format!("{:?}", state.resolved_variant),
                    );
                    TRANSITION_GATE.commit_revision(state.revision);
                    return None;
                }

                if target_colors == start_colors {
                    TRANSITION_GATE.supersede();
                    shilpo_m3e::Theme::global_mut(cx).apply_state(&state);
                    ShellSurfaces::apply_theme_state(cx, &state);
                    super::ShellRuntime::emit_theme_signal(
                        cx,
                        state.resolved_mode.as_str().into(),
                        format!("{:?}", state.resolved_variant),
                    );
                    TRANSITION_GATE.commit_revision(state.revision);
                    return None;
                }

                let generation = TRANSITION_GATE.supersede();
                let current_revision = TRANSITION_GATE.current_revision();
                Some(ThemeTransition::new(
                    generation,
                    current_revision,
                    state.revision,
                    start_colors,
                    target_colors,
                    state,
                    duration_ms,
                ))
            });

            let Some(transition) = res else {
                continue;
            };

            let generation = transition.generation;
            cx.spawn(async move |cx| {
                let span = tracing::info_span!(
                    target: "shilpo_profile",
                    "theme_transition",
                    from_revision = transition.from_revision,
                    to_revision = transition.to_revision,
                    duration_ms = transition.duration_ms,
                    outcome = tracing::field::Empty,
                    interrupted = tracing::field::Empty,
                );
                span.in_scope(|| {
                    tracing::debug!(target: "shilpo_profile", "theme transition started");
                });

                let start_time = Instant::now();
                loop {
                    if !TRANSITION_GATE.is_current(generation) {
                        span.record("outcome", "superseded");
                        span.record("interrupted", true);
                        break;
                    }

                    let elapsed_ms = start_time.elapsed().as_millis() as u64;
                    let (_, current_colors, is_complete) = transition.progress_at(elapsed_ms);

                    if !is_complete {
                        let applied = cx.update(|cx: &mut App| {
                            if !TRANSITION_GATE.is_current(generation) {
                                return false;
                            }
                            shilpo_m3e::Theme::global_mut(cx).colors = current_colors;
                            cx.refresh_windows();
                            true
                        });
                        if !applied {
                            span.record("outcome", "superseded");
                            span.record("interrupted", true);
                            break;
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                    } else {
                        let applied = cx.update(|cx: &mut App| {
                            if !TRANSITION_GATE.is_current(generation) {
                                return false;
                            }
                            shilpo_m3e::Theme::global_mut(cx).apply_state(&transition.target_state);
                            ShellSurfaces::apply_theme_state(cx, &transition.target_state);
                            super::ShellRuntime::emit_theme_signal(
                                cx,
                                transition.target_state.resolved_mode.as_str().into(),
                                format!("{:?}", transition.target_state.resolved_variant),
                            );
                            TRANSITION_GATE.commit_revision(transition.target_state.revision);
                            true
                        });
                        if applied {
                            span.record("outcome", "completed");
                            span.record("interrupted", false);
                        } else {
                            span.record("outcome", "superseded");
                            span.record("interrupted", true);
                        }
                        break;
                    }
                }
            })
            .detach();
        }
    })
    .detach();

    InitialThemeState {
        wallpaper_path,
        mode: initial_theme_state.resolved_mode.as_str().to_owned(),
        scheme_variant: format!("{:?}", initial_theme_state.resolved_variant),
    }
}

pub fn sync_wallpaper(cx: &mut App, initial_wallpaper_path: Option<PathBuf>) {
    let wallpaper_probe =
        cx.background_spawn(async { shell_surfaces::query_awww_wallpaper_path() });
    let theme_wallpaper_path = initial_wallpaper_path;
    ThemeClient::spawn_task(async move {
        let client = ThemeClient::new().await;
        if let Some(wallpaper_path) = wallpaper_probe.await {
            let _ = client
                .set_wallpaper(&wallpaper_path.to_string_lossy())
                .await;
        } else if let Some(wallpaper_path) = theme_wallpaper_path {
            let _ = client
                .set_wallpaper(&wallpaper_path.to_string_lossy())
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn theme_init_applies_daemon_state_and_reports_wallpaper() {
        let cx = gpui::TestAppContext::single();
        cx.update(|cx| shilpo_m3e::init_with_source(0xFF006C4C, cx));

        let expected = futures_lite::future::block_on(ThemeClient::new());
        let expected_state = expected.current_state();
        let expected_wallpaper = expected_state
            .wallpaper_path
            .clone()
            .filter(|path| path.is_file());

        let wallpaper = cx.update(init);

        cx.update(|cx| {
            let theme = shilpo_m3e::Theme::global(cx);
            assert_eq!(theme.source_argb, expected_state.source_argb);
            assert_eq!(theme.scheme_variant, expected_state.scheme_variant);
        });
        assert_eq!(wallpaper.wallpaper_path, expected_wallpaper);
        assert_eq!(wallpaper.mode, expected_state.resolved_mode.as_str());
        assert_eq!(
            wallpaper.scheme_variant,
            format!("{:?}", expected_state.resolved_variant)
        );
    }

    #[test]
    fn test_cubic_bezier_emphasized_boundaries() {
        assert_eq!(cubic_bezier_emphasized(0.0), 0.0);
        assert_eq!(cubic_bezier_emphasized(1.0), 1.0);
        assert_eq!(cubic_bezier_emphasized(-0.5), 0.0);
        assert_eq!(cubic_bezier_emphasized(1.5), 1.0);

        let mid = cubic_bezier_emphasized(0.5);
        assert!(mid > 0.5, "M3 emphasized easing accelerates early: {mid}");
    }

    #[test]
    fn test_transition_progress_and_completion() {
        let state1 = DaemonState {
            theme: shilpo_m3e::ThemeState {
                source_argb: 0xff6750a4,
                ..Default::default()
            },
            ..Default::default()
        };
        let state2 = DaemonState {
            theme: shilpo_m3e::ThemeState {
                source_argb: 0xff386a20,
                ..Default::default()
            },
            ..Default::default()
        };

        let start_colors = shilpo_m3e::material_theme(state1.source_argb, false);
        let target_colors = shilpo_m3e::material_theme(state2.source_argb, false);
        let transition =
            ThemeTransition::new(1, 1, 2, start_colors, target_colors, state2.clone(), 300);

        // At t=0 ms
        let (eased, colors, complete) = transition.progress_at(0);
        assert_eq!(eased, 0.0);
        assert_eq!(colors, start_colors);
        assert!(!complete);

        // At t=150 ms (midpoint)
        let (eased_mid, colors_mid, complete_mid) = transition.progress_at(150);
        assert!(eased_mid > 0.5);
        assert_ne!(colors_mid.primary, start_colors.primary);
        assert_ne!(colors_mid.primary, transition.target_colors.primary);
        assert!(!complete_mid);

        // At t=300 ms (completion)
        let (eased_end, colors_end, complete_end) = transition.progress_at(300);
        assert_eq!(eased_end, 1.0);
        assert_eq!(colors_end, transition.target_colors);
        assert!(complete_end);
    }

    #[test]
    fn test_zero_duration_transition_is_immediate() {
        let state1 = DaemonState::default();
        let state2 = DaemonState::default();
        let start_colors = shilpo_m3e::material_theme(state1.source_argb, false);
        let target_colors = shilpo_m3e::material_theme(state2.source_argb, false);
        let transition = ThemeTransition::new(1, 1, 2, start_colors, target_colors, state2, 0);

        let (_, colors, complete) = transition.progress_at(0);
        assert_eq!(colors, transition.target_colors);
        assert!(complete);
    }

    #[test]
    fn transition_gate_is_latest_wins_and_revision_safe() {
        let gate = TransitionGate::new();
        gate.initialize_revision(7);
        assert_eq!(gate.current_revision(), 7);

        let first = gate.supersede();
        assert!(gate.is_current(first));
        let second = gate.supersede();
        assert!(!gate.is_current(first));
        assert!(gate.is_current(second));

        gate.commit_revision(11);
        assert_eq!(gate.current_revision(), 11);
    }

    #[test]
    fn stale_generation_cannot_become_current_again() {
        let gate = TransitionGate::new();
        let stale = gate.supersede();
        let current = gate.supersede();
        assert!(!gate.is_current(stale));
        assert!(gate.is_current(current));
        assert!(!gate.is_current(stale));
    }
}
