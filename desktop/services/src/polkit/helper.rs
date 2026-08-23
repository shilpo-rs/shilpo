use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Events emitted by the `polkit-agent-helper-1` process over its stdout pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperEvent {
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
    /// Authentication failed terminal notification (`FAILURE`).
    Failure,
}

/// Abstraction for a running helper session.
pub trait PolkitHelperSession: Send {
    /// Writes a user response (e.g. password) followed by newline to the helper's stdin.
    fn write_response(&mut self, response: &str) -> io::Result<()>;
    /// Non-blocking: returns the next event if the background reader has already
    /// produced one, or `None` if nothing new has arrived yet. Never blocks the
    /// caller, so it is always safe to call while holding a shared lock.
    fn try_recv_event(&mut self) -> Option<HelperEvent>;
    /// Kills the helper child process.
    fn kill(&mut self) -> io::Result<()>;
}

/// Abstraction for locating and launching `polkit-agent-helper-1`.
pub trait PolkitHelper: Send + Sync {
    /// Spawns a new helper session for the given username and cookie.
    fn spawn_session(
        &self,
        username: &str,
        cookie: &str,
    ) -> io::Result<Box<dyn PolkitHelperSession>>;
    /// Returns the resolved path to the helper executable, if available.
    fn probe_path(&self) -> Option<PathBuf>;
}

/// Known standard filesystem locations for `polkit-agent-helper-1`.
pub const KNOWN_HELPER_PATHS: &[&str] = &[
    "/usr/lib/polkit-1/polkit-agent-helper-1",
    "/usr/libexec/polkit-agent-helper-1",
    "/usr/lib/polkit-100/polkit-agent-helper-1",
    "/usr/lib/polkit-agent-helper-1",
];

/// Probes for the system `polkit-agent-helper-1` binary.
pub fn probe_system_helper_path() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("POLKIT_AGENT_HELPER_1_PATH") {
        let p = PathBuf::from(override_path);
        if p.exists() {
            return Some(p);
        }
    }

    for path in KNOWN_HELPER_PATHS {
        let p = Path::new(path);
        if p.exists() {
            return Some(p.to_path_buf());
        }
    }

    None
}

/// Real subprocess implementation of `PolkitHelper`.
#[derive(Debug, Clone, Default)]
pub struct SystemPolkitHelper {
    custom_path: Option<PathBuf>,
}

impl SystemPolkitHelper {
    pub fn new() -> Self {
        Self { custom_path: None }
    }

    pub fn with_custom_path(path: impl Into<PathBuf>) -> Self {
        Self {
            custom_path: Some(path.into()),
        }
    }
}

impl PolkitHelper for SystemPolkitHelper {
    fn probe_path(&self) -> Option<PathBuf> {
        if let Some(ref p) = self.custom_path
            && p.exists()
        {
            return Some(p.clone());
        }
        probe_system_helper_path()
    }

    fn spawn_session(
        &self,
        username: &str,
        cookie: &str,
    ) -> io::Result<Box<dyn PolkitHelperSession>> {
        let helper_path = self.probe_path().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "polkit-agent-helper-1 executable not found on system",
            )
        })?;

        let mut child = Command::new(&helper_path)
            .arg(username)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let pid = child.id() as i32;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("failed to capture helper stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture helper stdout"))?;

        // Write cookie followed by newline immediately to helper stdin.
        stdin.write_all(cookie.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;

        let (tx, rx) = mpsc::channel();
        // The reader thread owns `child`/`stdout` exclusively and is the only thing
        // that ever calls `Child::wait`, so the pid captured above stays valid for
        // `kill()` to signal directly without needing shared access to `Child`.
        let reader_thread = std::thread::Builder::new()
            .name("polkit-helper-reader".into())
            .spawn(move || run_helper_reader(child, stdout, tx))
            .map_err(io::Error::other)?;

        Ok(Box::new(SystemPolkitHelperSession {
            pid,
            stdin,
            event_rx: rx,
            _reader_thread: Some(reader_thread),
        }))
    }
}

/// Blocking reader loop, run on a dedicated OS thread so that draining the
/// helper's stdout never blocks a caller holding the domain state's lock.
/// Parsed events are forwarded non-blockingly through `tx`.
fn run_helper_reader(mut child: Child, stdout: ChildStdout, tx: mpsc::Sender<HelperEvent>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // EOF without an explicit SUCCESS/FAILURE line: infer the terminal
                // event from the process exit status.
                let status = child.wait();
                let event = match status {
                    Ok(s) if s.success() => HelperEvent::Success,
                    _ => HelperEvent::Failure,
                };
                let _ = tx.send(event);
                return;
            }
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                let event = parse_helper_line(trimmed);
                let is_terminal = matches!(event, HelperEvent::Success | HelperEvent::Failure);
                if tx.send(event).is_err() {
                    // Receiver dropped: session was torn down. Reap and stop.
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

struct SystemPolkitHelperSession {
    pid: i32,
    stdin: ChildStdin,
    event_rx: mpsc::Receiver<HelperEvent>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl PolkitHelperSession for SystemPolkitHelperSession {
    fn write_response(&mut self, response: &str) -> io::Result<()> {
        self.stdin.write_all(response.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn try_recv_event(&mut self) -> Option<HelperEvent> {
        self.event_rx.try_recv().ok()
    }

    fn kill(&mut self) -> io::Result<()> {
        // Signal by pid rather than through `Child` (owned by the reader thread):
        // the reader thread only calls `wait()` after observing EOF on stdout,
        // which can't happen before this signal is delivered, so the pid cannot
        // have been reaped and recycled yet.
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
        }
        Ok(())
    }
}

/// Parses a line emitted by `polkit-agent-helper-1`.
pub fn parse_helper_line(line: &str) -> HelperEvent {
    if let Some(prompt) = line.strip_prefix("PAM_PROMPT_ECHO_OFF") {
        HelperEvent::PromptEchoOff(prompt.trim().to_string())
    } else if let Some(prompt) = line.strip_prefix("PAM_PROMPT_ECHO_ON") {
        HelperEvent::PromptEchoOn(prompt.trim().to_string())
    } else if let Some(msg) = line.strip_prefix("PAM_ERROR_MSG") {
        HelperEvent::ErrorMessage(msg.trim().to_string())
    } else if let Some(msg) = line.strip_prefix("PAM_TEXT_INFO") {
        HelperEvent::TextInfo(msg.trim().to_string())
    } else if line == "SUCCESS" {
        HelperEvent::Success
    } else if line == "FAILURE" {
        HelperEvent::Failure
    } else {
        // Fallback for unknown message lines
        HelperEvent::TextInfo(line.to_string())
    }
}

/// Mock helper for hermetic testing without real polkit binaries or root permissions.
#[derive(Debug, Clone)]
pub struct MockPolkitHelper {
    events: Arc<Mutex<Vec<HelperEvent>>>,
    spawned_users: Arc<Mutex<Vec<(String, String)>>>,
    written_responses: Arc<Mutex<Vec<String>>>,
    killed_sessions: Arc<Mutex<usize>>,
}

impl MockPolkitHelper {
    pub fn new(events: Vec<HelperEvent>) -> Self {
        Self {
            events: Arc::new(Mutex::new(events)),
            spawned_users: Arc::new(Mutex::new(Vec::new())),
            written_responses: Arc::new(Mutex::new(Vec::new())),
            killed_sessions: Arc::new(Mutex::new(0)),
        }
    }

    pub fn spawned_users(&self) -> Vec<(String, String)> {
        self.spawned_users.lock().unwrap().clone()
    }

    pub fn written_responses(&self) -> Vec<String> {
        self.written_responses.lock().unwrap().clone()
    }

    pub fn killed_count(&self) -> usize {
        *self.killed_sessions.lock().unwrap()
    }
}

impl PolkitHelper for MockPolkitHelper {
    fn probe_path(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/mock/polkit-agent-helper-1"))
    }

    fn spawn_session(
        &self,
        username: &str,
        cookie: &str,
    ) -> io::Result<Box<dyn PolkitHelperSession>> {
        self.spawned_users
            .lock()
            .unwrap()
            .push((username.to_string(), cookie.to_string()));

        let events = self.events.lock().unwrap().clone();

        Ok(Box::new(MockPolkitHelperSession {
            events,
            written_responses: self.written_responses.clone(),
            killed_sessions: self.killed_sessions.clone(),
        }))
    }
}

struct MockPolkitHelperSession {
    events: Vec<HelperEvent>,
    written_responses: Arc<Mutex<Vec<String>>>,
    killed_sessions: Arc<Mutex<usize>>,
}

impl PolkitHelperSession for MockPolkitHelperSession {
    fn write_response(&mut self, response: &str) -> io::Result<()> {
        self.written_responses
            .lock()
            .unwrap()
            .push(response.to_string());
        Ok(())
    }

    fn try_recv_event(&mut self) -> Option<HelperEvent> {
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
