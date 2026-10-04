//! A flow-scoped, unprivileged owner for native Polkit or sudo authorization.
//! No passwords or transferable authorization tokens are stored by LG Buddy.
use super::flow::AuthorizationMode;
use std::os::unix::process::CommandExt;
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    os::fd::AsRawFd,
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const RUNNER: &str = include_str!("authorization.sh");
const HELPER_OBSERVATION: Duration = Duration::from_secs(15 * 60);
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Shared only by an explicit setup flow; permission is acquired lazily by
/// the native authenticator when an operation needs administrator privileges.
#[derive(Debug, Default)]
pub struct AuthorizationSession(Mutex<SessionState>);

#[derive(Debug)]
struct SessionState {
    process: Option<SessionProcess>,
    closed: bool,
    mode: AuthorizationMode,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            process: None,
            closed: false,
            mode: AuthorizationMode::Interactive,
        }
    }
}

impl AuthorizationSession {
    pub(crate) fn new(mode: AuthorizationMode) -> Self {
        Self(Mutex::new(SessionState {
            mode,
            ..SessionState::default()
        }))
    }

    pub(crate) fn services(&self, config: &Path, lock: Option<&Arc<File>>) -> io::Result<Output> {
        self.run("services", config, false, lock)
    }

    pub(crate) fn plasma(
        &self,
        helper: &Path,
        allow_dependencies: bool,
        lock: Option<&Arc<File>>,
    ) -> io::Result<Output> {
        self.run("plasma", helper, allow_dependencies, lock)
    }

    pub(crate) fn services_cancellable(
        &self,
        config: &Path,
        lock: Option<&Arc<File>>,
        cancellation: &super::StepCancellation,
        mutation: &mut dyn FnMut(),
    ) -> io::Result<Output> {
        self.run_cancellable(
            "services",
            config,
            false,
            lock,
            Some(cancellation),
            mutation,
        )
    }
    pub(crate) fn plasma_cancellable(
        &self,
        helper: &Path,
        allow: bool,
        lock: Option<&Arc<File>>,
        cancellation: &super::StepCancellation,
        mutation: &mut dyn FnMut(),
    ) -> io::Result<Output> {
        self.run_cancellable("plasma", helper, allow, lock, Some(cancellation), mutation)
    }

    fn run(
        &self,
        operation: &str,
        path: &Path,
        option: bool,
        lock: Option<&Arc<File>>,
    ) -> io::Result<Output> {
        self.run_cancellable(operation, path, option, lock, None, &mut || {})
    }

    fn run_cancellable(
        &self,
        operation: &str,
        path: &Path,
        option: bool,
        lock: Option<&Arc<File>>,
        cancellation: Option<&super::StepCancellation>,
        mutation: &mut dyn FnMut(),
    ) -> io::Result<Output> {
        let mut state = self.0.lock().unwrap();
        if state.closed {
            return Err(io::Error::other("setup authorization session is closed"));
        }
        if state.process.is_none() {
            state.process = Some(SessionProcess::start(RUNNER, state.mode, lock)?);
        }
        let result = state.process.as_mut().unwrap().run(
            operation,
            path,
            option,
            cancellation,
            mutation,
            HELPER_OBSERVATION,
        );
        if result.is_err() {
            // A broken channel is not authorization. A fresh session can only
            // be started by a later, explicit step attempt.
            state.process.take();
        }
        result
    }

    pub(crate) fn close(&self) {
        let mut state = self.0.lock().unwrap();
        state.closed = true;
        state.process.take();
    }
}

#[derive(Debug)]
struct SessionProcess {
    child: Option<Child>,
    isolated_group: bool,
    lease: Option<Arc<File>>,
    input: Option<ChildStdin>,
    output: BufReader<SessionOutput>,
}

#[derive(Debug)]
struct SessionOutput {
    pipe: ChildStdout,
    deadline: Instant,
    cancellation: Option<super::StepCancellation>,
    mutating: bool,
}
impl Read for SessionOutput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if !self.mutating && self.cancellation.as_ref().is_some_and(|c| c.is_cancelled()) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "setup preparation cancelled",
                ));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "setup helper remains active; recheck before starting another repair",
                ));
            }
            let mut fd = libc::pollfd {
                fd: self.pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fd, 1, remaining.as_millis().min(50) as i32) };
            if ready > 0 {
                return self.pipe.read(buffer);
            }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

impl SessionProcess {
    fn start(script: &str, mode: AuthorizationMode, lock: Option<&Arc<File>>) -> io::Result<Self> {
        let mut command = Command::new("bash");
        command
            .args(["-c", script, "lg-buddy-setup-authorization"])
            .arg(match mode {
                AuthorizationMode::Interactive => "interactive",
                AuthorizationMode::Terminal => "terminal",
                AuthorizationMode::Noninteractive => "noninteractive",
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let isolated_group = mode != AuthorizationMode::Terminal;
        if isolated_group {
            command.process_group(0);
        }
        super::lock::inherit_command_lock(&mut command, lock);
        let mut child = command.spawn()?;
        Ok(Self {
            input: child.stdin.take(),
            output: BufReader::new(SessionOutput {
                pipe: child.stdout.take().unwrap(),
                deadline: Instant::now() + HELPER_OBSERVATION,
                cancellation: None,
                mutating: false,
            }),
            child: Some(child),
            isolated_group,
            lease: lock.cloned(),
        })
    }

    fn run(
        &mut self,
        operation: &str,
        path: &Path,
        option: bool,
        cancellation: Option<&super::StepCancellation>,
        mutation: &mut dyn FnMut(),
        timeout: Duration,
    ) -> io::Result<Output> {
        let operation = if cancellation.is_some() {
            format!("{operation}-cancellable")
        } else {
            operation.into()
        };
        let fields = [
            OsStr::new(&operation),
            path.as_os_str(),
            OsStr::new(if option { "1" } else { "0" }),
        ];
        if fields.iter().any(|field| field.as_bytes().contains(&0)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in setup argument",
            ));
        }
        let reader = self.output.get_mut();
        reader.deadline = Instant::now() + timeout;
        reader.cancellation = cancellation.cloned();
        reader.mutating = cancellation.is_none();
        let mut request = Vec::new();
        for field in fields {
            request.extend_from_slice(field.as_bytes());
            request.push(0);
        }
        if request.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "setup request too long",
            ));
        }
        self.input.as_mut().unwrap().write_all(&request)?;
        let result = self.response(cancellation, mutation);
        if result.is_err() && !self.output.get_ref().mutating {
            // Only preparation is terminated. Once acknowledged, the owner
            // finishes the helper and retains its inherited execution lease.
            unsafe {
                let pid = self.child.as_ref().unwrap().id() as i32;
                libc::kill(if self.isolated_group { -pid } else { pid }, libc::SIGTERM);
            }
        }
        result
    }

    fn response(
        &mut self,
        cancellation: Option<&super::StepCancellation>,
        mutation: &mut dyn FnMut(),
    ) -> io::Result<Output> {
        let status = loop {
            let mut line = String::new();
            if self.output.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "setup authorization worker exited",
                ));
            }
            if line.trim() == "mutation" {
                if cancellation.is_some_and(|c| !c.protect()) {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "setup preparation cancelled",
                    ));
                }
                self.output.get_mut().mutating = true;
                self.input.as_mut().unwrap().write_all(b"continue\0")?;
                mutation();
            } else {
                break line
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| io::Error::other("invalid setup response"))?;
            }
        };
        if status > 255 {
            return Err(io::Error::other("invalid setup exit status"));
        }
        let stdout_len = self.number()?;
        let stderr_len = self.number()?;
        if stdout_len.saturating_add(stderr_len) > MAX_RESPONSE_BYTES {
            return Err(io::Error::other(
                "setup response exceeds its safe size limit",
            ));
        }
        let mut stdout = vec![0; stdout_len];
        let mut stderr = vec![0; stderr_len];
        self.output.read_exact(&mut stdout)?;
        self.output.read_exact(&mut stderr)?;
        Ok(Output {
            status: ExitStatus::from_raw((status as i32) << 8),
            stdout,
            stderr,
        })
    }

    fn number(&mut self) -> io::Result<usize> {
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "setup authorization worker exited",
            ));
        }
        line.trim()
            .parse()
            .map_err(|_| io::Error::other("invalid setup response"))
    }
}

impl Drop for SessionProcess {
    fn drop(&mut self) {
        self.input.take();
        // EOF closes an idle owner. If the frontend dies during a mutation, the
        // shell finishes the helper first, retaining the flow lock throughout.
        if let Some(mut child) = self.child.take() {
            // Waiting must not freeze closing a frontend. The shell itself
            // retains the lease until its active helper has exited.
            if child.try_wait().ok().flatten().is_none() {
                let lease = self.lease.take();
                std::thread::spawn(move || {
                    let _ = child.wait();
                    drop(lease);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;
