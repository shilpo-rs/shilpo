use std::{
    fmt,
    io::{self, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::record::MAX_RECORD_BYTES;

pub const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptProcessError {
    Spawn(String),
    Io(String),
    Timeout,
    Cancelled,
    RecordTooLarge,
}

impl fmt::Display for ScriptProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(message) => write!(formatter, "failed to spawn script: {message}"),
            Self::Io(message) => write!(formatter, "script process I/O error: {message}"),
            Self::Timeout => write!(formatter, "script execution timed out"),
            Self::Cancelled => write!(formatter, "script execution cancelled"),
            Self::RecordTooLarge => write!(formatter, "script record exceeds the 1 MiB limit"),
        }
    }
}

impl std::error::Error for ScriptProcessError {}

pub trait ProcessRunner: Send + Sync {
    fn run_poll(
        &self,
        executable: &Path,
        args: &[String],
        cwd: &Path,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ProcessOutput, ScriptProcessError>;

    fn spawn_stream(
        &self,
        executable: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<Box<dyn StreamProcess>, ScriptProcessError>;
}

pub trait StreamProcess: Send {
    fn next_line(
        &mut self,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, ScriptProcessError>;
    fn kill_group(&mut self) -> Result<(), ScriptProcessError>;
    fn stderr_excerpt(&mut self) -> String;
}

#[derive(Default)]
pub struct RealProcessRunner;

impl ProcessRunner for RealProcessRunner {
    fn run_poll(
        &self,
        executable: &Path,
        args: &[String],
        cwd: &Path,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Result<ProcessOutput, ScriptProcessError> {
        let mut child = spawn_child(executable, args, cwd)?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ScriptProcessError::Io("stdout pipe is unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ScriptProcessError::Io("stderr pipe is unavailable".into()))?;
        let stdout_reader = thread::spawn(move || drain_bounded(stdout, MAX_RECORD_BYTES + 2));
        let stderr_reader = thread::spawn(move || drain_bounded(stderr, MAX_STDERR_BYTES));

        let started = Instant::now();
        let status = loop {
            if cancelled.load(Ordering::Acquire) {
                terminate_group(pid, &mut child);
                join_reader(stdout_reader);
                join_reader(stderr_reader);
                return Err(ScriptProcessError::Cancelled);
            }
            if started.elapsed() >= timeout {
                terminate_group(pid, &mut child);
                join_reader(stdout_reader);
                join_reader(stderr_reader);
                return Err(ScriptProcessError::Timeout);
            }
            match child_has_exited(&mut child) {
                Ok(true) => {
                    // `child_has_exited` observes with WNOWAIT, retaining ownership of
                    // the leader PID/PGID until descendants have been signalled.
                    terminate_descendants(pid);
                    let status = child.wait();
                    reap_group(pid);
                    match status {
                        Ok(status) => break status,
                        Err(error) => {
                            join_reader(stdout_reader);
                            join_reader(stderr_reader);
                            return Err(ScriptProcessError::Io(error.to_string()));
                        }
                    }
                }
                Ok(false) => thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    terminate_group(pid, &mut child);
                    join_reader(stdout_reader);
                    join_reader(stderr_reader);
                    return Err(ScriptProcessError::Io(error.to_string()));
                }
            }
        };

        let stdout = join_reader(stdout_reader);
        let stderr = join_reader(stderr_reader);
        Ok(ProcessOutput {
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    fn spawn_stream(
        &self,
        executable: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<Box<dyn StreamProcess>, ScriptProcessError> {
        let mut child = spawn_child(executable, args, cwd)?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ScriptProcessError::Io("stdout pipe is unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ScriptProcessError::Io("stderr pipe is unavailable".into()))?;
        set_nonblocking(&stdout).map_err(|error| ScriptProcessError::Io(error.to_string()))?;
        set_nonblocking(&stderr).map_err(|error| ScriptProcessError::Io(error.to_string()))?;
        Ok(Box::new(RealStreamProcess {
            pid,
            child: Some(child),
            stdout,
            stderr,
            pending: Vec::new(),
            stderr_buf: Vec::new(),
        }))
    }
}

#[cfg(unix)]
fn child_has_exited(child: &mut Child) -> io::Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` points to writable storage for siginfo_t. WNOWAIT observes the
    // specific child without reaping it, preserving the PID/PGID until group cleanup.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: waitid succeeded and initialized `info`; zero si_pid denotes no state change.
    Ok(unsafe { info.assume_init().si_pid() } != 0)
}

#[cfg(not(unix))]
fn child_has_exited(child: &mut Child) -> io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

fn spawn_child(
    executable: &Path,
    args: &[String],
    cwd: &Path,
) -> Result<Child, ScriptProcessError> {
    enable_subreaper();
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        )
        .env(
            "LANG",
            std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()),
        )
        .env(
            "LC_ALL",
            std::env::var("LC_ALL").unwrap_or_else(|_| "C.UTF-8".into()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setpgid is async-signal-safe and touches only the new child.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .spawn()
        .map_err(|error| ScriptProcessError::Spawn(error.to_string()))
}

struct RealStreamProcess {
    pid: u32,
    child: Option<Child>,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    pending: Vec<u8>,
    stderr_buf: Vec<u8>,
}

impl StreamProcess for RealStreamProcess {
    fn next_line(
        &mut self,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, ScriptProcessError> {
        let started = Instant::now();
        loop {
            self.drain_stderr();
            if cancelled.load(Ordering::Acquire) {
                self.kill_group()?;
                return Err(ScriptProcessError::Cancelled);
            }
            if let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=newline).collect();
                line.pop();
                if line.len() > MAX_RECORD_BYTES {
                    return Err(ScriptProcessError::RecordTooLarge);
                }
                return Ok(Some(line));
            }
            if self.pending.len() > MAX_RECORD_BYTES {
                return Err(ScriptProcessError::RecordTooLarge);
            }

            let mut chunk = [0_u8; 8192];
            match self.stdout.read(&mut chunk) {
                Ok(0) => {
                    if self.pending.is_empty() {
                        return Ok(None);
                    }
                    return Ok(Some(std::mem::take(&mut self.pending)));
                }
                Ok(read) => self.pending.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Err(ScriptProcessError::Timeout);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(ScriptProcessError::Io(error.to_string())),
            }
        }
    }

    fn kill_group(&mut self) -> Result<(), ScriptProcessError> {
        if let Some(mut child) = self.child.take() {
            terminate_group(self.pid, &mut child);
        }
        Ok(())
    }

    fn stderr_excerpt(&mut self) -> String {
        self.drain_stderr();
        String::from_utf8_lossy(&self.stderr_buf).into_owned()
    }
}

impl RealStreamProcess {
    fn drain_stderr(&mut self) {
        let mut chunk = [0_u8; 2048];
        loop {
            match self.stderr.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    let available = MAX_STDERR_BYTES.saturating_sub(self.stderr_buf.len());
                    self.stderr_buf
                        .extend_from_slice(&chunk[..read.min(available)]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }
}

impl Drop for RealStreamProcess {
    fn drop(&mut self) {
        let _ = self.kill_group();
    }
}

fn drain_bounded(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut stored = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let available = limit.saturating_sub(stored.len());
                stored.extend_from_slice(&chunk[..read.min(available)]);
            }
        }
    }
    stored
}

fn join_reader(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

#[cfg(unix)]
fn set_nonblocking(stream: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: fd belongs to a live pipe owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this only changes flags on the same live descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_nonblocking(_stream: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    Ok(())
}

fn terminate_group(pid: u32, child: &mut Child) {
    terminate_descendants(pid);
    let _ = child.kill();
    let _ = child.wait();
    reap_group(pid);
}

#[cfg(unix)]
fn terminate_descendants(pid: u32) {
    // SAFETY: negative pid addresses only the process group created for this script.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_descendants(_pid: u32) {}

#[cfg(target_os = "linux")]
fn enable_subreaper() {
    // SAFETY: marks this extension-host process as a child subreaper; no pointers involved.
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

#[cfg(not(target_os = "linux"))]
fn enable_subreaper() {}

#[cfg(unix)]
fn reap_group(pid: u32) {
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        let mut status = 0;
        // SAFETY: waitpid writes to a valid local status and targets only this process group.
        let result = unsafe { libc::waitpid(-(pid as i32), &mut status, libc::WNOHANG) };
        if result == -1 || Instant::now() >= deadline {
            break;
        }
        if result == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(not(unix))]
fn reap_group(_pid: u32) {}

#[cfg(test)]
mod process_group_tests {
    use super::{child_has_exited, reap_group, spawn_child};
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn completed_child_remains_owned_until_process_group_is_signalled() {
        let mut child = spawn_child(
            Path::new("/bin/sh"),
            &["-c".into(), "exit 0".into()],
            Path::new("/"),
        )
        .expect("spawn process-group leader");
        let pid = child.id();

        while !child_has_exited(&mut child).expect("observe child state") {
            thread::sleep(Duration::from_millis(1));
        }

        // Signal 0 does not mutate the group; it proves the leader PID/PGID has not
        // been released for reuse before cleanup targets it.
        let group_is_still_owned = unsafe { libc::kill(-(pid as i32), 0) } == 0;
        let _ = child.wait();
        reap_group(pid);

        assert!(
            group_is_still_owned,
            "observing exit must not reap the leader before group signalling"
        );
    }
}
