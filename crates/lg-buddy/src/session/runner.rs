use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{run_screen_off, run_screen_on};
use crate::config::{
    load_config, resolve_config_path_from_env, ScreenBackend, DEFAULT_IDLE_TIMEOUT,
};
use crate::gnome::{map_monitor_line, GnomeBackend, SystemGnomeProbe};
use crate::session::{SessionBackend, SessionBackendError, SessionEvent};
use crate::state::{ScreenOwnershipMarker, StateDirError, StateScope};
use crate::RunError;

const GNOME_WAIT_TIMEOUT_SECS: u64 = 15;
const GNOME_DBUS_NAME: &str = "org.gnome.ScreenSaver";
const GNOME_DBUS_PATH: &str = "/org/gnome/ScreenSaver";
const GNOME_IDLE_MONITOR_NAME: &str = "org.gnome.Mutter.IdleMonitor";
const GNOME_IDLE_MONITOR_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
const GNOME_ACTIVE_THRESHOLD_MS: u64 = 1000;
const GNOME_ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(250);

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
    StateDir(StateDirError),
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
            Self::StateDir(err) => write!(f, "{err}"),
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
            Self::StateDir(err) => Some(err),
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
                match self.executor.screen_off() {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => {
                        writeln!(writer, "LG Buddy Monitor: screen-off action failed. {err}")?
                    }
                }
            }
            SessionEvent::Active | SessionEvent::WakeRequested | SessionEvent::UserActivity => {
                writeln!(
                    writer,
                    "LG Buddy Monitor: Session event `{}` requests screen restore.",
                    event.as_str()
                )?;
                match self.executor.screen_on() {
                    Ok(output) => write_command_output(writer, &output)?,
                    Err(err) => writeln!(
                        writer,
                        "LG Buddy Monitor: screen restore action failed. {err}"
                    )?,
                }
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
        SessionRunnerError::StateDir(err) => RunError::StateDir(err),
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
        ScreenBackend::Swayidle => run_swayidle_monitor(writer),
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

    let marker = if capabilities.early_user_activity {
        Some(
            ScreenOwnershipMarker::from_env(StateScope::Session)
                .map_err(SessionRunnerError::StateDir)?,
        )
    } else {
        None
    };

    let (sender, receiver) = mpsc::channel();
    let monitor_handle = spawn_gnome_monitor_thread(sender.clone());
    let mut activity_watcher = None;
    let mut monitor_result = Ok(());

    while let Ok(message) = receiver.recv() {
        match message {
            RunnerMessage::SessionEvent(SessionEvent::Idle) => {
                dispatcher.dispatch_event(writer, SessionEvent::Idle)?;
                if let Some(marker) = marker.as_ref() {
                    start_gnome_activity_watcher(
                        &mut activity_watcher,
                        sender.clone(),
                        marker.clone(),
                    );
                }
            }
            RunnerMessage::SessionEvent(
                event @ (SessionEvent::Active
                | SessionEvent::WakeRequested
                | SessionEvent::UserActivity),
            ) => {
                stop_gnome_activity_watcher(&mut activity_watcher);
                dispatcher.dispatch_event(writer, event)?;
            }
            RunnerMessage::SessionEvent(event) => {
                dispatcher.dispatch_event(writer, event)?;
            }
            RunnerMessage::MonitorExited(result) => {
                stop_gnome_activity_watcher(&mut activity_watcher);
                monitor_result = result;
                break;
            }
        }
    }

    let _ = monitor_handle.join();
    monitor_result
}

fn run_swayidle_monitor<W: Write>(writer: &mut W) -> Result<(), SessionRunnerError> {
    let idle_timeout_secs = resolve_idle_timeout_secs();
    let current_exe = std::env::current_exe()?;
    let screen_off_command = format!("{} screen-off", shell_quote(&current_exe));
    let screen_on_command = format!("{} screen-on", shell_quote(&current_exe));

    writeln!(
        writer,
        "LG Buddy Monitor: Using swayidle backend (timeout: {idle_timeout_secs}s)."
    )?;

    let status = ProcessCommand::new("swayidle")
        .args([
            "-w",
            "timeout",
            &idle_timeout_secs.to_string(),
            &screen_off_command,
            "resume",
            &screen_on_command,
        ])
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(SessionRunnerError::Failed {
            backend: ScreenBackend::Swayidle,
            message: format!("swayidle exited with status {status}"),
        })
    }
}

#[cfg(test)]
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

fn forward_gnome_monitor_output<R: BufRead>(
    reader: R,
    sender: &mpsc::Sender<RunnerMessage>,
) -> Result<(), SessionRunnerError> {
    for line in reader.lines() {
        let line = line?;
        if let Some(event) = map_monitor_line(&line) {
            if sender.send(RunnerMessage::SessionEvent(event)).is_err() {
                break;
            }
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

fn resolve_idle_timeout_secs() -> u64 {
    std::env::var("LG_BUDDY_IDLE_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            let path = resolve_config_path_from_env().ok()?;
            load_config(&path)
                .ok()
                .map(|config| config.screen_idle_timeout)
        })
        .unwrap_or(DEFAULT_IDLE_TIMEOUT)
}

fn spawn_gnome_monitor_thread(sender: mpsc::Sender<RunnerMessage>) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = run_gnome_monitor_process(&sender);
        let _ = sender.send(RunnerMessage::MonitorExited(result));
    })
}

fn run_gnome_monitor_process(
    sender: &mpsc::Sender<RunnerMessage>,
) -> Result<(), SessionRunnerError> {
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

    forward_gnome_monitor_output(BufReader::new(stdout), sender)?;

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

enum RunnerMessage {
    SessionEvent(SessionEvent),
    MonitorExited(Result<(), SessionRunnerError>),
}

trait ActivityMarker {
    fn exists(&self) -> bool;
}

impl ActivityMarker for ScreenOwnershipMarker {
    fn exists(&self) -> bool {
        ScreenOwnershipMarker::exists(self)
    }
}

trait IdleMonitorProbe {
    fn current_idletime_ms(&self) -> Option<u64>;
}

#[derive(Debug, Clone, Copy, Default)]
struct SystemGnomeIdleMonitorProbe;

impl IdleMonitorProbe for SystemGnomeIdleMonitorProbe {
    fn current_idletime_ms(&self) -> Option<u64> {
        let output = ProcessCommand::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                GNOME_IDLE_MONITOR_NAME,
                "--object-path",
                GNOME_IDLE_MONITOR_PATH,
                "--method",
                "org.gnome.Mutter.IdleMonitor.GetIdletime",
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        parse_gnome_idletime_output(&String::from_utf8_lossy(&output.stdout))
    }
}

struct GnomeActivityWatcher {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

fn start_gnome_activity_watcher(
    watcher: &mut Option<GnomeActivityWatcher>,
    sender: mpsc::Sender<RunnerMessage>,
    marker: ScreenOwnershipMarker,
) {
    stop_gnome_activity_watcher(watcher);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let probe = SystemGnomeIdleMonitorProbe;
        if watch_for_early_user_activity(
            &marker,
            &probe,
            &stop_signal,
            GNOME_ACTIVE_THRESHOLD_MS,
            GNOME_ACTIVE_POLL_INTERVAL,
            thread::sleep,
        ) {
            let _ = sender.send(RunnerMessage::SessionEvent(SessionEvent::UserActivity));
        }
    });

    *watcher = Some(GnomeActivityWatcher { stop, handle });
}

fn stop_gnome_activity_watcher(watcher: &mut Option<GnomeActivityWatcher>) {
    if let Some(watcher) = watcher.take() {
        watcher.stop.store(true, Ordering::SeqCst);
        let _ = watcher.handle.join();
    }
}

fn watch_for_early_user_activity<M, P, S>(
    marker: &M,
    probe: &P,
    stop: &AtomicBool,
    threshold_ms: u64,
    poll_interval: Duration,
    sleep: S,
) -> bool
where
    M: ActivityMarker,
    P: IdleMonitorProbe,
    S: Fn(Duration),
{
    while marker.exists() && !stop.load(Ordering::SeqCst) {
        if let Some(idletime_ms) = probe.current_idletime_ms() {
            if idletime_ms < threshold_ms {
                return true;
            }
        }

        if stop.load(Ordering::SeqCst) || !marker.exists() {
            break;
        }

        sleep(poll_interval);
    }

    false
}

fn parse_gnome_idletime_output(output: &str) -> Option<u64> {
    output
        .trim()
        .strip_prefix("(uint64 ")?
        .strip_suffix(",)")?
        .parse::<u64>()
        .ok()
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    let escaped = rendered.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
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
    use super::{
        parse_gnome_idletime_output, process_gnome_monitor_output, shell_quote,
        watch_for_early_user_activity, ActivityMarker, IdleMonitorProbe, SessionActionExecutor,
        SessionEventDispatcher,
    };
    use crate::session::SessionEvent;
    use crate::RunError;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct FakeActionExecutor {
        screen_off_calls: usize,
        screen_on_calls: usize,
        screen_off_output: String,
        screen_on_output: String,
        screen_off_error: Option<String>,
        screen_on_error: Option<String>,
    }

    impl SessionActionExecutor for FakeActionExecutor {
        fn screen_off(&mut self) -> Result<String, RunError> {
            self.screen_off_calls += 1;
            if let Some(message) = &self.screen_off_error {
                return Err(RunError::Policy(message.clone()));
            }
            Ok(self.screen_off_output.clone())
        }

        fn screen_on(&mut self) -> Result<String, RunError> {
            self.screen_on_calls += 1;
            if let Some(message) = &self.screen_on_error {
                return Err(RunError::Policy(message.clone()));
            }
            Ok(self.screen_on_output.clone())
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FakeMarker {
        exists: bool,
    }

    impl ActivityMarker for FakeMarker {
        fn exists(&self) -> bool {
            self.exists
        }
    }

    struct FakeIdleMonitorProbe {
        idletimes: RefCell<Vec<Option<u64>>>,
    }

    impl IdleMonitorProbe for FakeIdleMonitorProbe {
        fn current_idletime_ms(&self) -> Option<u64> {
            let mut idletimes = self.idletimes.borrow_mut();
            if idletimes.is_empty() {
                None
            } else {
                idletimes.remove(0)
            }
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
    fn screen_restore_failures_are_logged_without_stopping_dispatch() {
        let executor = FakeActionExecutor {
            screen_on_error: Some("tv is still waking".to_string()),
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

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen restore action failed. tv is still waking"));
        assert_eq!(dispatcher.executor.screen_on_calls, 2);
    }

    #[test]
    fn screen_off_failures_are_logged_without_stopping_dispatch() {
        let executor = FakeActionExecutor {
            screen_off_error: Some("tv did not respond".to_string()),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut output = Vec::new();

        dispatcher
            .dispatch_event(&mut output, SessionEvent::Idle)
            .expect("dispatch idle event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen-off action failed. tv did not respond"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
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

    #[test]
    fn parses_gnome_idle_monitor_output() {
        assert_eq!(parse_gnome_idletime_output("(uint64 777,)\n"), Some(777));
        assert_eq!(parse_gnome_idletime_output("unexpected"), None);
    }

    #[test]
    fn early_activity_watch_reports_recent_input_when_marker_exists() {
        let marker = FakeMarker { exists: true };
        let probe = FakeIdleMonitorProbe {
            idletimes: RefCell::new(vec![Some(1500), Some(0)]),
        };
        let stop = AtomicBool::new(false);

        let detected = watch_for_early_user_activity(
            &marker,
            &probe,
            &stop,
            1000,
            Duration::from_millis(1),
            |_| {},
        );

        assert!(detected);
    }

    #[test]
    fn early_activity_watch_stops_when_marker_is_missing() {
        let marker = FakeMarker { exists: false };
        let probe = FakeIdleMonitorProbe {
            idletimes: RefCell::new(vec![Some(0)]),
        };
        let stop = AtomicBool::new(false);

        let detected = watch_for_early_user_activity(
            &marker,
            &probe,
            &stop,
            1000,
            Duration::from_millis(1),
            |_| {},
        );

        assert!(!detected);
    }

    #[test]
    fn shell_quote_wraps_path_for_posix_shell() {
        assert_eq!(shell_quote(Path::new("/tmp/lg buddy")), "'/tmp/lg buddy'");
        assert_eq!(
            shell_quote(Path::new("/tmp/that'one")),
            "'/tmp/that'\"'\"'one'"
        );
    }
}
