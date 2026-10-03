//! A flow-scoped, unprivileged owner for native Polkit or sudo authorization.
//! No passwords or transferable authorization tokens are stored by LG Buddy.
use super::flow::AuthorizationMode;
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio},
    sync::{Arc, Mutex},
};

const RUNNER: &str = include_str!("authorization.sh");

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

    fn run(
        &self,
        operation: &str,
        path: &Path,
        option: bool,
        lock: Option<&Arc<File>>,
    ) -> io::Result<Output> {
        let mut state = self.0.lock().unwrap();
        if state.closed {
            return Err(io::Error::other("setup authorization session is closed"));
        }
        if state.process.is_none() {
            state.process = Some(SessionProcess::start(RUNNER, state.mode, lock)?);
        }
        let result = state.process.as_mut().unwrap().run(operation, path, option);
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
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
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
        super::lock::inherit_command_lock(&mut command, lock);
        let mut child = command.spawn()?;
        Ok(Self {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        })
    }

    fn run(&mut self, operation: &str, path: &Path, option: bool) -> io::Result<Output> {
        let fields = [
            OsStr::new(operation),
            path.as_os_str(),
            OsStr::new(if option { "1" } else { "0" }),
        ];
        if fields.iter().any(|field| field.as_bytes().contains(&0)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in setup argument",
            ));
        }
        let input = self.input.as_mut().unwrap();
        for field in fields {
            input.write_all(field.as_bytes())?;
            input.write_all(&[0])?;
        }
        input.flush()?;
        let status = self.number()?;
        if status > 255 {
            return Err(io::Error::other("invalid setup exit status"));
        }
        let stdout_len = self.number()?;
        let stderr_len = self.number()?;
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
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests;
