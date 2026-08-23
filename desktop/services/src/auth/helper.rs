use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Environment variable that, when set, makes this binary run only the PAM conversation
/// child (see `auth::pam_child::run`) instead of the normal CLI. Checked at the very top of
/// `main()`, before any Tokio runtime or Clap parsing, mirroring `SHILPO_WASM_VALIDATOR`.
pub const PAM_HELPER_ENV_VAR: &str = "SHILPO_PAM_HELPER";

/// Events emitted by the PAM helper child over its stdout pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthHelperEvent {
    /// Masked prompt (password) from PAM (`PAM_PROMPT_ECHO_OFF <prompt>`).
    PromptEchoOff(String),
    /// Visible prompt from PAM (`PAM_PROMPT_ECHO_ON <prompt>`).
    PromptEchoOn(String),
    /// Supplementary error message from PAM (`PAM_ERROR_MSG <text>`).
    ErrorMessage(String),
    /// Supplementary informational message from PAM (`PAM_TEXT_INFO <text>`).
    TextInfo(String),
    /// Authentication succeeded terminal notification (`SUCCESS`).
    Success,
    /// Authentication failed terminal notification (`FAILURE <message>`).
    Failure(String),
}

/// Abstraction for a running PAM helper session.
pub trait AuthHelperSession: Send {
    /// Writes a user response (e.g. password) followed by newline to the helper's stdin.
    /// Callers must zeroize `response` after this returns.
    fn write_response(&mut self, response: &str) -> io::Result<()>;
    /// Non-blocking: returns the next event if the background reader has already produced
    /// one, or `None` otherwise. Never blocks the caller, so it is safe to call while
    /// holding a shared lock.
    fn try_recv_event(&mut self) -> Option<AuthHelperEvent>;
    /// Kills the helper child process.
    fn kill(&mut self) -> io::Result<()>;
}

/// Abstraction for launching a PAM conversation child process.
pub trait AuthHelper: Send + Sync {
    /// Spawns a new PAM conversation session for `service` (e.g. `"login"`).
    fn spawn_session(&self, service: &str) -> io::Result<Box<dyn AuthHelperSession>>;
}

/// Real subprocess implementation of `AuthHelper`: re-execs the current binary with
/// `SHILPO_PAM_HELPER` set, per the fork+exec safety rationale in `pam_child.rs`.
#[derive(Debug, Clone, Default)]
pub struct SystemAuthHelper;

impl SystemAuthHelper {
    pub fn new() -> Self {
        Self
    }
}

impl AuthHelper for SystemAuthHelper {
    fn spawn_session(&self, service: &str) -> io::Result<Box<dyn AuthHelperSession>> {
        let exe = std::env::current_exe()?;

        let mut child = Command::new(exe)
            .env(PAM_HELPER_ENV_VAR, service)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let pid = child.id() as i32;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("failed to capture pam helper stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture pam helper stdout"))?;

        let (tx, rx) = mpsc::channel();
        // The reader thread owns `child`/`stdout` exclusively and is the only thing that
        // ever calls `Child::wait`, so the pid captured above stays valid for `kill()` to
        // signal directly without needing shared access to `Child` (same pattern as
        // polkit's helper reader).
        let reader_thread = std::thread::Builder::new()
            .name("shilpo-pam-helper-reader".into())
            .spawn(move || run_helper_reader(child, stdout, tx))
            .map_err(io::Error::other)?;

        Ok(Box::new(SystemAuthHelperSession {
            pid,
            stdin,
            event_rx: rx,
            _reader_thread: Some(reader_thread),
        }))
    }
}

/// Blocking reader loop, run on a dedicated OS thread so that draining the helper's stdout
/// never blocks a caller holding the domain state's lock.
fn run_helper_reader(mut child: Child, stdout: ChildStdout, tx: mpsc::Sender<AuthHelperEvent>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF without an explicit SUCCESS/FAILURE line: infer the terminal event
                // from the process exit status.
                let status = child.wait();
                let event = match status {
                    Ok(s) if s.success() => AuthHelperEvent::Success,
                    _ => AuthHelperEvent::Failure("pam helper exited unexpectedly".into()),
                };
                let _ = tx.send(event);
                return;
            }
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                let event = parse_helper_line(trimmed);
                let is_terminal = matches!(
                    event,
                    AuthHelperEvent::Success | AuthHelperEvent::Failure(_)
                );
                if tx.send(event).is_err() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                if is_terminal {
                    let _ = child.wait();
                    return;
                }
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

struct SystemAuthHelperSession {
    pid: i32,
    stdin: ChildStdin,
    event_rx: mpsc::Receiver<AuthHelperEvent>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl AuthHelperSession for SystemAuthHelperSession {
    fn write_response(&mut self, response: &str) -> io::Result<()> {
        self.stdin.write_all(response.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn try_recv_event(&mut self) -> Option<AuthHelperEvent> {
        self.event_rx.try_recv().ok()
    }

    fn kill(&mut self) -> io::Result<()> {
        // Signal by pid rather than through `Child` (owned by the reader thread): the
        // reader thread only calls `wait()` after observing EOF on stdout, which can't
        // happen before this signal is delivered, so the pid cannot have been reaped and
        // recycled yet.
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
        }
        Ok(())
    }
}

/// Parses a line emitted by the PAM helper child.
pub fn parse_helper_line(line: &str) -> AuthHelperEvent {
    if let Some(prompt) = line.strip_prefix("PAM_PROMPT_ECHO_OFF") {
        AuthHelperEvent::PromptEchoOff(prompt.trim().to_string())
    } else if let Some(prompt) = line.strip_prefix("PAM_PROMPT_ECHO_ON") {
        AuthHelperEvent::PromptEchoOn(prompt.trim().to_string())
    } else if let Some(msg) = line.strip_prefix("PAM_ERROR_MSG") {
        AuthHelperEvent::ErrorMessage(msg.trim().to_string())
    } else if let Some(msg) = line.strip_prefix("PAM_TEXT_INFO") {
        AuthHelperEvent::TextInfo(msg.trim().to_string())
    } else if line == "SUCCESS" {
        AuthHelperEvent::Success
    } else if let Some(msg) = line.strip_prefix("FAILURE") {
        AuthHelperEvent::Failure(msg.trim().to_string())
    } else {
        AuthHelperEvent::TextInfo(line.to_string())
    }
}

/// Mock helper for hermetic testing without real PAM or a subprocess.
#[derive(Debug, Clone, Default)]
pub struct MockAuthHelper {
    scripts: Arc<Mutex<Vec<Vec<AuthHelperEvent>>>>,
    spawned_services: Arc<Mutex<Vec<String>>>,
    written_responses: Arc<Mutex<Vec<String>>>,
    killed_sessions: Arc<Mutex<usize>>,
}

impl MockAuthHelper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the event sequence the next `spawn_session` call will replay, in order.
    /// Multiple calls queue multiple sessions (FIFO).
    pub fn queue_session(&self, events: Vec<AuthHelperEvent>) {
        self.scripts.lock().unwrap().push(events);
    }

    pub fn spawned_services(&self) -> Vec<String> {
        self.spawned_services.lock().unwrap().clone()
    }

    pub fn written_responses(&self) -> Vec<String> {
        self.written_responses.lock().unwrap().clone()
    }

    pub fn killed_count(&self) -> usize {
        *self.killed_sessions.lock().unwrap()
    }
}

impl AuthHelper for MockAuthHelper {
    fn spawn_session(&self, service: &str) -> io::Result<Box<dyn AuthHelperSession>> {
        self.spawned_services
            .lock()
            .unwrap()
            .push(service.to_string());

        let events = {
            let mut scripts = self.scripts.lock().unwrap();
            if scripts.is_empty() {
                Vec::new()
            } else {
                scripts.remove(0)
            }
        };

        Ok(Box::new(MockAuthHelperSession {
            events,
            written_responses: self.written_responses.clone(),
            killed_sessions: self.killed_sessions.clone(),
        }))
    }
}

struct MockAuthHelperSession {
    events: Vec<AuthHelperEvent>,
    written_responses: Arc<Mutex<Vec<String>>>,
    killed_sessions: Arc<Mutex<usize>>,
}

impl AuthHelperSession for MockAuthHelperSession {
    fn write_response(&mut self, response: &str) -> io::Result<()> {
        self.written_responses
            .lock()
            .unwrap()
            .push(response.to_string());
        Ok(())
    }

    fn try_recv_event(&mut self) -> Option<AuthHelperEvent> {
        if self.events.is_empty() {
            None
        } else {
            Some(self.events.remove(0))
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        *self.killed_sessions.lock().unwrap() += 1;
        Ok(())
    }
}
