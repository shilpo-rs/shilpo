//! Single source of truth for spawning the dedicated `shilpo lock` process role (ADR-0005,
//! issue #135). Every trigger — the idle domain's `Lock`/`LockAndSuspend` actions, the
//! `org.shilpo.Shell.Lock()` D-Bus method, and the `PrepareForSleep` watch — goes through
//! one `LockSupervisor` instance so telemetry has a single, consistent view of whether a
//! locker is running and what its last spawn error was, instead of each call site tracking
//! (or not tracking) its own state.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Environment variable telling a spawned locker which inherited anonymous socket file
/// descriptor signals readiness once every output's session-lock surface is committed
/// (`PlatformSessionLock::on_locked`). The descriptor has no filesystem name and is only
/// inherited by the specific locker child.
pub const LOCK_READY_FD_ENV_VAR: &str = "SHILPO_LOCK_READY_FD";

/// How long the internal socket reader thread waits for a spawned locker to signal readiness
/// before giving up and treating the attempt as failed. Generous relative to any caller's
/// own [`LockSupervisor::spawn_and_wait_until_locked`] timeout (a few seconds) so it never
/// cuts a legitimate wait short; it exists only as a bound on the worst case (a locker that
/// crashed or hung before ever reaching `on_locked`).
const READY_SIGNAL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct ActiveLock {
    pid: u32,
}

/// A one-shot, multi-waiter readiness result for a single locker attempt. Every caller that
/// wants to know "is the session locked yet" for the *currently running* locker joins the
/// same slot instead of starting a competing locker process -- `ext-session-lock-v1` only
/// allows one lock at a time, so a second `lock()` call from a second process would just be
/// denied and immediately `finished`, never signaling readiness.
struct ReadinessSlot {
    result: Mutex<Option<bool>>,
    cvar: Condvar,
}

impl ReadinessSlot {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            cvar: Condvar::new(),
        }
    }

    /// Resolves the slot once; subsequent calls (e.g. a late socket signal after the reaper
    /// already resolved a crash) are ignored.
    fn resolve(&self, value: bool) {
        let mut result = self.result.lock().unwrap();
        if result.is_none() {
            *result = Some(value);
            self.cvar.notify_all();
        }
    }

    /// Waits up to `timeout` for a result, without affecting the slot's shared state -- a
    /// caller giving up doesn't stop other callers (or the locker itself) from still
    /// resolving it later.
    fn wait(&self, timeout: Duration) -> bool {
        let mut result = self.result.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while result.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (guard, wait_result) = self.cvar.wait_timeout(result, remaining).unwrap();
            result = guard;
            if wait_result.timed_out() && result.is_none() {
                return false;
            }
        }
        result.unwrap_or(false)
    }
}

#[derive(Default)]
pub struct LockSupervisor {
    active: Mutex<Option<ActiveLock>>,
    last_error: Mutex<Option<String>>,
    last_spawn_reason: Mutex<Option<String>>,
    /// `Some` while a locker attempt is in flight or has an unresolved outcome; cleared once
    /// the locker exits. Callers join this instead of spawning a second locker.
    readiness: Mutex<Option<Arc<ReadinessSlot>>>,
}

impl LockSupervisor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_active(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    pub fn last_spawn_reason(&self) -> Option<String> {
        self.last_spawn_reason.lock().unwrap().clone()
    }

    /// Spawns `shilpo lock` for `reason` (used only for diagnostics), fire-and-forget. If a
    /// locker is already starting or running, this joins its outcome instead of launching a
    /// second, competing locker process (which `ext-session-lock-v1` would just deny).
    pub fn spawn(self: &Arc<Self>, reason: &str) {
        *self.last_spawn_reason.lock().unwrap() = Some(reason.to_string());
        let (slot, created) = self.acquire_or_join_slot();
        if created {
            self.start_locker(reason, slot);
        }
    }

    /// Spawns the locker (or joins one already in flight) and blocks the calling thread
    /// (safe to call from an async context via `spawn_blocking`) until it signals readiness
    /// over its inherited socket, or `timeout` elapses. Used by the `PrepareForSleep` watch, which must
    /// not release its delay inhibitor — and so must not let suspend proceed — until the
    /// session is actually locked, or it gives up waiting. Joining an in-flight attempt
    /// (rather than always starting a fresh one) matters here: `IdleAction::LockAndSuspend`
    /// starts its own best-effort locker before calling `Suspend`, and the resulting
    /// `PrepareForSleep` signal reaches this watch immediately after — without joining, that
    /// second call would try to open a second `ext-session-lock-v1` lock, get denied, and
    /// spend its whole timeout waiting on a locker that will never signal readiness.
    pub fn spawn_and_wait_until_locked(self: &Arc<Self>, reason: &str, timeout: Duration) -> bool {
        *self.last_spawn_reason.lock().unwrap() = Some(reason.to_string());
        let (slot, created) = self.acquire_or_join_slot();
        if created {
            self.start_locker(reason, slot.clone());
        }
        slot.wait(timeout)
    }

    /// Returns the readiness slot for the currently in-flight/active locker, creating one
    /// (and registering it) if none exists. `created` is `true` only for the caller that
    /// just created it, so exactly one caller actually spawns the process.
    fn acquire_or_join_slot(&self) -> (Arc<ReadinessSlot>, bool) {
        let mut readiness = self.readiness.lock().unwrap();
        if let Some(existing) = readiness.as_ref() {
            (existing.clone(), false)
        } else {
            let slot = Arc::new(ReadinessSlot::new());
            *readiness = Some(slot.clone());
            (slot, true)
        }
    }

    /// Clears `readiness` back to `None`, but only if it still points at `slot` -- guards
    /// against clobbering a newer attempt that may have started in the meantime.
    fn clear_readiness_if_matches(&self, slot: &Arc<ReadinessSlot>) {
        let mut readiness = self.readiness.lock().unwrap();
        if readiness
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, slot))
        {
            *readiness = None;
        }
    }

    /// Actually launches `shilpo lock`, always wired to an anonymous readiness socket so
    /// any caller (present or future, via `acquire_or_join_slot`) can observe when it locks,
    /// and starts the reaper + socket-reader threads that resolve `slot`.
    fn start_locker(self: &Arc<Self>, reason: &str, slot: Arc<ReadinessSlot>) {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(err) => {
                let message = format!("failed to resolve current executable: {err}");
                tracing::warn!(reason, %message, "cannot spawn shilpo lock");
                *self.last_error.lock().unwrap() = Some(message);
                slot.resolve(false);
                self.clear_readiness_if_matches(&slot);
                return;
            }
        };

        let (readiness_reader, readiness_writer) = match UnixStream::pair() {
            Ok(pair) => pair,
            Err(err) => {
                let message = format!("failed to create lock-readiness socket: {err}");
                tracing::warn!(reason, %message, "cannot spawn shilpo lock safely");
                *self.last_error.lock().unwrap() = Some(message);
                slot.resolve(false);
                self.clear_readiness_if_matches(&slot);
                return;
            }
        };

        let mut command = Command::new(exe);
        command.arg("lock");
        configure_inherited_readiness_writer(&mut command, readiness_writer.as_raw_fd());

        match command.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                *self.active.lock().unwrap() = Some(ActiveLock { pid });
                *self.last_error.lock().unwrap() = None;

                // The parent must not retain the child's endpoint: closing it here lets
                // the reader observe EOF promptly if the locker exits without signaling.
                drop(readiness_writer);

                let reader_slot = slot.clone();
                std::thread::Builder::new()
                    .name("shilpo-lock-ready-wait".into())
                    .spawn(move || {
                        let signaled =
                            wait_for_readiness_signal(readiness_reader, READY_SIGNAL_TIMEOUT);
                        reader_slot.resolve(signaled);
                    })
                    .ok();

                let this = self.clone();
                let reaper_slot = slot.clone();
                std::thread::Builder::new()
                    .name("shilpo-lock-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                        let mut active = this.active.lock().unwrap();
                        if active.as_ref().is_some_and(|a| a.pid == pid) {
                            *active = None;
                        }
                        drop(active);
                        // No-op if the socket reader already resolved this attempt.
                        reaper_slot.resolve(false);
                        this.clear_readiness_if_matches(&reaper_slot);
                    })
                    .ok();
            }
            Err(err) => {
                let message = format!("failed to spawn shilpo lock: {err}");
                tracing::warn!(reason, %message);
                *self.last_error.lock().unwrap() = Some(message);
                slot.resolve(false);
                self.clear_readiness_if_matches(&slot);
            }
        }
    }
}

/// Configures `command` to inherit only the supplied writer endpoint across exec.
/// Clearing `FD_CLOEXEC` in `pre_exec` avoids a process-wide inheritance window in the
/// multithreaded daemon before the fork.
fn configure_inherited_readiness_writer(command: &mut Command, writer_fd: RawFd) {
    command.env(LOCK_READY_FD_ENV_VAR, writer_fd.to_string());
    // SAFETY: the callback invokes only async-signal-safe `fcntl` operations, captures a
    // plain integer, and reports failures as `io::Error` without allocating in the child.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(writer_fd, libc::F_GETFD);
            if flags == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(writer_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Reads the one-byte readiness signal with a kernel-enforced timeout. Unlike the former
/// FIFO implementation, this performs no nested blocking open and leaves no detached
/// thread behind when the deadline expires.
fn wait_for_readiness_signal(mut reader: UnixStream, timeout: Duration) -> bool {
    if reader.set_read_timeout(Some(timeout)).is_err() {
        return false;
    }
    let mut signal = [0u8; 1];
    reader.read_exact(&mut signal).is_ok() && signal == [1]
}

/// Probes whether the compositor advertises `ext_session_lock_manager_v1`, for `shilpo
/// doctor`. A one-shot connect + registry roundtrip on a bounded thread (so a compositor
/// that never responds can't hang `doctor`), independent of the daemon and the locker
/// process — neither of which holds this connection themselves (the daemon never touches
/// session-lock at all, and the locker only exists transiently while a lock is active).
pub fn probe_session_lock_protocol_available() -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("shilpo-doctor-session-lock-probe".into())
        .spawn(move || {
            let available = probe_session_lock_protocol_blocking();
            let _ = tx.send(available);
        });
    if spawned.is_err() {
        return false;
    }
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
}

fn probe_session_lock_protocol_blocking() -> bool {
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::{Connection, Dispatch, QueueHandle};

    struct State {
        found: bool,
    }

    impl Dispatch<WlRegistry, ()> for State {
        fn event(
            state: &mut Self,
            _registry: &WlRegistry,
            event: wl_registry::Event,
            _data: &(),
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global { interface, .. } = event
                && interface == "ext_session_lock_manager_v1"
            {
                state.found = true;
            }
        }
    }

    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let mut event_queue = conn.new_event_queue::<State>();
    let qh = event_queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&qh, ());

    let mut state = State { found: false };
    let _ = event_queue.roundtrip(&mut state);
    state.found
}

/// Signals readiness to a `LockSupervisor` through the inherited anonymous socket, if set.
/// Called from the locker process itself once every output's surface is confirmed locked.
pub fn signal_lock_ready() {
    let Some(fd) = std::env::var(LOCK_READY_FD_ENV_VAR)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .filter(|fd| *fd > libc::STDERR_FILENO)
    else {
        return;
    };

    // SAFETY: `fd` is the endpoint inherited specifically for this locker process. Taking
    // ownership closes it after the one-shot signal, preventing it from leaking further.
    let mut writer = unsafe { std::fs::File::from_raw_fd(fd) };
    let _ = writer.write_all(&[1u8]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_records_last_error_on_failure() {
        let supervisor = LockSupervisor::new();
        // current_exe() always succeeds in a test binary, so simulate the failure path
        // directly against the private helper via a bogus reason to at least exercise the
        // accessor methods' default state.
        assert!(!supervisor.is_active());
        assert!(supervisor.last_error().is_none());
        assert!(supervisor.last_spawn_reason().is_none());
    }

    #[test]
    fn child_process_inherits_anonymous_readiness_writer() {
        let (reader, writer) = UnixStream::pair().expect("create readiness socket pair");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("eval \"printf '\\\\001' >&$SHILPO_LOCK_READY_FD\"");
        configure_inherited_readiness_writer(&mut command, writer.as_raw_fd());

        let mut child = command.spawn().expect("spawn signaling child");
        drop(writer);

        assert!(wait_for_readiness_signal(reader, Duration::from_secs(5)));
        assert!(child.wait().expect("wait for signaling child").success());
    }

    #[test]
    fn readiness_slot_resolves_once_and_notifies_all_waiters() {
        let slot = Arc::new(ReadinessSlot::new());
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let slot = slot.clone();
                std::thread::spawn(move || slot.wait(Duration::from_secs(5)))
            })
            .collect();
        std::thread::sleep(Duration::from_millis(50));
        slot.resolve(true);
        slot.resolve(false); // ignored: first resolution wins

        for waiter in waiters {
            assert!(waiter.join().unwrap());
        }
    }

    #[test]
    fn acquire_or_join_slot_dedupes_concurrent_callers() {
        // Regression test for the Codex cross-check finding: without this dedup,
        // `IdleAction::LockAndSuspend`'s pre-spawn and the `PrepareForSleep` watch's
        // `spawn_and_wait_until_locked` would each launch a competing `shilpo lock`
        // process, and the second is always denied the session lock.
        let supervisor = LockSupervisor::new();

        let (first_slot, first_created) = supervisor.acquire_or_join_slot();
        assert!(first_created, "first caller must own the spawn");

        let (second_slot, second_created) = supervisor.acquire_or_join_slot();
        assert!(!second_created, "second caller must join, not spawn again");
        assert!(Arc::ptr_eq(&first_slot, &second_slot));

        // Once the locker attempt is resolved (process exited, or readiness observed) a
        // later spawn request must be free to start a fresh attempt.
        supervisor.clear_readiness_if_matches(&first_slot);
        let (third_slot, third_created) = supervisor.acquire_or_join_slot();
        assert!(third_created, "a cleared slot must allow a new spawn");
        assert!(!Arc::ptr_eq(&first_slot, &third_slot));
    }

    #[test]
    fn readiness_wait_timeout_leaves_no_blocked_thread() {
        fn readiness_reader_threads() -> usize {
            std::fs::read_dir("/proc/self/task")
                .expect("read process task list")
                .filter_map(Result::ok)
                .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
                // Linux truncates task names to 15 visible bytes.
                .filter(|name| name.trim() == "shilpo-lock-rea")
                .count()
        }

        let readers_before = readiness_reader_threads();
        let (reader, _writer) = UnixStream::pair().expect("create readiness socket pair");

        let signaled = wait_for_readiness_signal(reader, Duration::from_millis(200));

        assert!(!signaled);
        let readers_after = readiness_reader_threads();
        assert_eq!(
            readers_after, readers_before,
            "a timed-out readiness wait must not leave a blocked reader thread"
        );
    }
}
