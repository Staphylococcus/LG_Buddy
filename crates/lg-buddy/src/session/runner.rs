use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command as ProcessCommand, Stdio};

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{run_screen_off, run_screen_on};
use crate::config::ScreenBackend;
use crate::gnome::{map_monitor_line, GnomeBackend, SystemGnomeProbe};
use crate::session::{SessionBackend, SessionBackendError, SessionEvent};
use crate::RunError;

const GNOME_WAIT_TIMEOUT_SECS: u64 = 15;
const GNOME_DBUS_NAME: &str = "org.gnome.ScreenSaver";
const GNOME_DBUS_PATH: &str = "/org/gnome/ScreenSaver";

pub trait SessionActionExecutor {
    fn screen_off(&mut self) -> Result<String, RunError>;
    fn screen_on(&mut self) -> Result<String, RunError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimeActionExecutor;

impl SessionActionExecutor for RuntimeActionExecutor {
    fn screen_off(&mut self) -> Result<String, RunError> {
        run_action(run_screen_off)
    }

    fn screen_on(&mut self) -> Result<String, RunError> {
        run_action(run_screen_on)
    }
}

#[derive(Debug)]
pub enum SessionRunnerError {
    Io(String),
    BackendUnavailable(SessionBackendError),
    BackendSelection(BackendSelectionError),
    BackendDetection(BackendDetectionError),
    UnsupportedBackend {
        backend: ScreenBackend,
        reason: &'static str,
    },
    Action(RunError),
    Failed {
        backend: ScreenBackend,
        message: String,
    },
}

impl fmt::Display for SessionRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "{message}"),
            Self::BackendUnavailable(err) => write!(f, "{err}"),
            Self::BackendSelection(err) => write!(f, "{err}"),
            Self::BackendDetection(err) => write!(f, "{err}"),
            Self::UnsupportedBackend { backend, reason } => write!(
                f,
                "session runner for backend `{}` is not implemented yet: {reason}",
                backend.as_str()
            ),
            Self::Action(err) => write!(f, "{err}"),
            Self::Failed { backend, message } => {
                write!(
                    f,
                    "session runner for backend `{}` failed: {message}",
                    backend.as_str()
                )
            }
        }
    }
}

impl Error for SessionRunnerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BackendUnavailable(err) => Some(err),
            Self::BackendSelection(err) => Some(err),
            Self::BackendDetection(err) => Some(err),
            Self::Action(err) => Some(err),
            Self::Io(_) | Self::UnsupportedBackend { .. } | Self::Failed { .. } => None,
        }
    }
}

impl From<io::Error> for SessionRunnerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

pub struct SessionEventDispatcher<E> {
    executor: E,
}

impl<E> SessionEventDispatcher<E> {
    pub fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: SessionActionExecutor> SessionEventDispatcher<E> {
    pub fn dispatch_event<W: Write>(
        &mut self,
        writer: &mut W,
        event: SessionEvent,
    ) -> Result<(), SessionRunnerError> {
        match event {
            SessionEvent::Idle => {
                writeln!(writer, "LG Buddy Monitor: Session became idle.")?;
                let output = self
                    .executor
                    .screen_off()
                    .map_err(SessionRunnerError::Action)?;
                write_command_output(writer, &output)?;
            }
            SessionEvent::Active | SessionEvent::WakeRequested | SessionEvent::UserActivity => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `{}` requests screen restore.",
                    event.as_str()
                )?;
                let output = self
                    .executor
                    .screen_on()
                    .map_err(SessionRunnerError::Action)?;
                write_command_output(writer, &output)?;
            }
            SessionEvent::BeforeSleep
            | SessionEvent::AfterResume
            | SessionEvent::Lock
            | SessionEvent::Unlock => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `{}` is not handled yet.",
                    event.as_str()
                )?;
            }
        }

        Ok(())
    }
}

pub fn run_monitor<W: Write>(writer: &mut W) -> Result<(), RunError> {
    run_monitor_with_executor(writer, RuntimeActionExecutor).map_err(|err| match err {
        SessionRunnerError::BackendSelection(err) => RunError::BackendSelection(err),
        SessionRunnerError::BackendDetection(err) => RunError::BackendDetection(err),
        other => RunError::Policy(other.to_string()),
    })
}

fn run_monitor_with_executor<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    executor: E,
) -> Result<(), SessionRunnerError> {
    let configured =
        configured_backend_from_env_or_config().map_err(SessionRunnerError::BackendSelection)?;
    let backend =
        detect_backend_from_system(configured).map_err(SessionRunnerError::BackendDetection)?;
    let mut dispatcher = SessionEventDispatcher::new(executor);

    match backend {
        ScreenBackend::Gnome => run_gnome_monitor(writer, &mut dispatcher),
        ScreenBackend::Swayidle => Err(SessionRunnerError::UnsupportedBackend {
            backend,
            reason: "delegated swayidle monitor support and hook IPC are still pending",
        }),
        ScreenBackend::Auto => Err(SessionRunnerError::Failed {
            backend,
            message: "auto backend should be resolved before starting the runner".to_string(),
        }),
    }
}

fn run_gnome_monitor<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
) -> Result<(), SessionRunnerError> {
    wait_for_gnome_shell()?;

    let capabilities = GnomeBackend::new(SystemGnomeProbe)
        .capabilities()
        .map_err(SessionRunnerError::BackendUnavailable)?;

    writeln!(writer, "LG Buddy Monitor: Using GNOME backend.")?;

    if capabilities.early_user_activity {
        writeln!(
            writer,
            "LG Buddy Monitor: Mutter idle-monitor support detected; early user activity handling is not wired yet."
        )?;
    }

    let mut child = ProcessCommand::new("gdbus")
        .args([
            "monitor",
            "--session",
            "--dest",
            GNOME_DBUS_NAME,
            "--object-path",
            GNOME_DBUS_PATH,
        ])
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SessionRunnerError::Failed {
            backend: ScreenBackend::Gnome,
            message: "gdbus monitor did not expose stdout".to_string(),
        })?;

    process_gnome_monitor_output(BufReader::new(stdout), writer, dispatcher)?;

    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(SessionRunnerError::Failed {
            backend: ScreenBackend::Gnome,
            message: format!("gdbus monitor exited with status {status}"),
        })
    }
}

fn process_gnome_monitor_output<R: BufRead, W: Write, E: SessionActionExecutor>(
    reader: R,
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
) -> Result<(), SessionRunnerError> {
    for line in reader.lines() {
        let line = line?;
        if let Some(event) = map_monitor_line(&line) {
            dispatcher.dispatch_event(writer, event)?;
        }
    }

    Ok(())
}

fn wait_for_gnome_shell() -> Result<(), SessionRunnerError> {
    let status = ProcessCommand::new("gdbus")
        .args([
            "wait",
            "--session",
            "--timeout",
            &GNOME_WAIT_TIMEOUT_SECS.to_string(),
            "org.gnome.Shell",
        ])
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(SessionRunnerError::Failed {
            backend: ScreenBackend::Gnome,
            message: "timed out waiting for GNOME Shell on the session bus".to_string(),
        })
    }
}

fn run_action<F>(action: F) -> Result<String, RunError>
where
    F: FnOnce(&mut Vec<u8>) -> Result<(), RunError>,
{
    let mut output = Vec::new();
    action(&mut output)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn write_command_output<W: Write>(writer: &mut W, output: &str) -> io::Result<()> {
    if output.is_empty() {
        return Ok(());
    }

    write!(writer, "{output}")?;
    if !output.ends_with('\n') {
        writeln!(writer)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{process_gnome_monitor_output, SessionActionExecutor, SessionEventDispatcher};
    use crate::session::SessionEvent;
    use crate::RunError;
    use std::io::Cursor;

    #[derive(Debug, Default)]
    struct FakeActionExecutor {
        screen_off_calls: usize,
        screen_on_calls: usize,
        screen_off_output: String,
        screen_on_output: String,
    }

    impl SessionActionExecutor for FakeActionExecutor {
        fn screen_off(&mut self) -> Result<String, RunError> {
            self.screen_off_calls += 1;
            Ok(self.screen_off_output.clone())
        }

        fn screen_on(&mut self) -> Result<String, RunError> {
            self.screen_on_calls += 1;
            Ok(self.screen_on_output.clone())
        }
    }

    #[test]
    fn idle_event_dispatches_screen_off() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Idle)
            .expect("dispatch idle event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Session became idle."));
        assert!(output.contains("screen-off output"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
        assert_eq!(dispatcher.executor.screen_on_calls, 0);
    }

    #[test]
    fn active_and_wake_events_dispatch_screen_on() {
        let executor = FakeActionExecutor {
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Active)
            .expect("dispatch active event");
        dispatcher
            .dispatch_event(&mut output, SessionEvent::WakeRequested)
            .expect("dispatch wake-requested event");
        dispatcher
            .dispatch_event(&mut output, SessionEvent::UserActivity)
            .expect("dispatch user-activity event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("active"));
        assert!(output.contains("wake-requested"));
        assert!(output.contains("user-activity"));
        assert_eq!(dispatcher.executor.screen_off_calls, 0);
        assert_eq!(dispatcher.executor.screen_on_calls, 3);
    }

    #[test]
    fn unhandled_events_are_logged_without_running_actions() {
        let executor = FakeActionExecutor::default();
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        for event in [
            SessionEvent::BeforeSleep,
            SessionEvent::AfterResume,
            SessionEvent::Lock,
            SessionEvent::Unlock,
        ] {
            dispatcher
                .dispatch_event(&mut output, event)
                .expect("dispatch noop event");
        }

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("before-sleep"));
        assert!(output.contains("after-resume"));
        assert!(output.contains("lock"));
        assert!(output.contains("unlock"));
        assert_eq!(dispatcher.executor.screen_off_calls, 0);
        assert_eq!(dispatcher.executor.screen_on_calls, 0);
    }

    #[test]
    fn gnome_monitor_output_dispatches_known_events() {
        let input = "\
signal org.gnome.ScreenSaver.ActiveChanged (true,)\n\
member=WakeUpScreen\n\
signal org.gnome.ScreenSaver.ActiveChanged (false,)\n\
unrelated\n";
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        process_gnome_monitor_output(Cursor::new(input), &mut output, &mut dispatcher)
            .expect("process gnome monitor lines");

        let output = String::from_utf8(output).expect("utf8");
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
        assert_eq!(dispatcher.executor.screen_on_calls, 2);
        assert!(output.contains("Session became idle."));
        assert!(output.contains("wake-requested"));
        assert!(output.contains("active"));
    }
}
