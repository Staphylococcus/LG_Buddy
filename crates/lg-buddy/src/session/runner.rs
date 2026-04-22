use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
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
use crate::session::inactivity::{
    InactivityDecision, InactivityEngine, InactivityObservation, InactivityThresholds,
};
use crate::session::{SessionBackend, SessionBackendError, SessionEvent};
use crate::RunError;

const GNOME_WAIT_TIMEOUT_SECS: u64 = 15;
const GNOME_DBUS_NAME: &str = "org.gnome.ScreenSaver";
const GNOME_DBUS_PATH: &str = "/org/gnome/ScreenSaver";
const GNOME_IDLE_MONITOR_NAME: &str = "org.gnome.Mutter.IdleMonitor";
const GNOME_IDLE_MONITOR_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
const GNOME_ACTIVE_THRESHOLD_MS: u64 = 1000;
const GNOME_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);

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

    GnomeBackend::new(SystemGnomeProbe)
        .capabilities()
        .map_err(SessionRunnerError::BackendUnavailable)?;

    writeln!(writer, "LG Buddy Monitor: Using GNOME backend.")?;

    let mut inactivity = InactivityEngine::new(InactivityThresholds {
        blank_threshold_ms: resolve_idle_timeout_ms(),
        active_threshold_ms: GNOME_ACTIVE_THRESHOLD_MS,
    });

    let (sender, receiver) = mpsc::channel();
    let latest_inactivity = Arc::new(LatestInactivityObservation::default());
    let monitor_handle = spawn_gnome_monitor_thread(sender.clone());
    let mut idle_poller = None;
    start_gnome_idle_monitor_poller(
        &mut idle_poller,
        sender.clone(),
        Arc::clone(&latest_inactivity),
    );
    let mut monitor_result = Ok(());

    while let Ok(message) = receiver.recv() {
        match message {
            RunnerMessage::InactivityObservationReady => {
                if let Some(observation) = latest_inactivity.take() {
                    handle_gnome_inactivity_observation(
                        writer,
                        dispatcher,
                        &mut inactivity,
                        observation,
                    )?;
                }
            }
            RunnerMessage::SessionEvent(SessionEvent::Idle) => {
                handle_gnome_inactivity_observation(
                    writer,
                    dispatcher,
                    &mut inactivity,
                    InactivityObservation::ProviderIdle,
                )?;
            }
            RunnerMessage::SessionEvent(SessionEvent::Active) => {
                handle_gnome_inactivity_observation(
                    writer,
                    dispatcher,
                    &mut inactivity,
                    InactivityObservation::ProviderActive,
                )?
            }
            RunnerMessage::SessionEvent(SessionEvent::WakeRequested) => {
                handle_gnome_inactivity_observation(
                    writer,
                    dispatcher,
                    &mut inactivity,
                    InactivityObservation::WakeRequested,
                )?
            }
            RunnerMessage::SessionEvent(SessionEvent::UserActivity) => {
                handle_gnome_inactivity_observation(
                    writer,
                    dispatcher,
                    &mut inactivity,
                    InactivityObservation::UserActivityObserved,
                )?
            }
            RunnerMessage::SessionEvent(event) => {
                dispatcher.dispatch_event(writer, event)?;
            }
            RunnerMessage::MonitorExited(result) => {
                stop_gnome_idle_monitor_poller(&mut idle_poller);
                monitor_result = result;
                break;
            }
        }
    }

    stop_gnome_idle_monitor_poller(&mut idle_poller);
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
    normalize_idle_timeout_secs(
        std::env::var("LG_BUDDY_IDLE_TIMEOUT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                let path = resolve_config_path_from_env().ok()?;
                load_config(&path)
                    .ok()
                    .map(|config| config.screen_idle_timeout)
            })
            .unwrap_or(DEFAULT_IDLE_TIMEOUT),
    )
}

fn resolve_idle_timeout_ms() -> u64 {
    resolve_idle_timeout_secs().saturating_mul(1000)
}

fn normalize_idle_timeout_secs(idle_timeout_secs: u64) -> u64 {
    if idle_timeout_secs == 0 {
        DEFAULT_IDLE_TIMEOUT
    } else {
        idle_timeout_secs
    }
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
    InactivityObservationReady,
    MonitorExited(Result<(), SessionRunnerError>),
}

#[derive(Debug, Default)]
struct LatestInactivityObservation {
    state: Mutex<LatestInactivityObservationState>,
}

#[derive(Debug, Default)]
struct LatestInactivityObservationState {
    observation: Option<InactivityObservation>,
    notification_in_flight: bool,
}

impl LatestInactivityObservation {
    fn publish(
        &self,
        sender: &mpsc::Sender<RunnerMessage>,
        observation: InactivityObservation,
    ) -> bool {
        let should_notify = {
            let mut state = self
                .state
                .lock()
                .expect("latest inactivity observation lock");
            state.observation = Some(observation);
            if state.notification_in_flight {
                false
            } else {
                state.notification_in_flight = true;
                true
            }
        };

        if !should_notify {
            return true;
        }

        if sender
            .send(RunnerMessage::InactivityObservationReady)
            .is_ok()
        {
            return true;
        }

        let mut state = self
            .state
            .lock()
            .expect("latest inactivity observation lock");
        state.notification_in_flight = false;
        false
    }

    fn take(&self) -> Option<InactivityObservation> {
        let mut state = self
            .state
            .lock()
            .expect("latest inactivity observation lock");
        let observation = state.observation.take();
        state.notification_in_flight = false;
        observation
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

struct GnomeIdleMonitorPoller {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

fn start_gnome_idle_monitor_poller(
    poller: &mut Option<GnomeIdleMonitorPoller>,
    sender: mpsc::Sender<RunnerMessage>,
    latest_observation: Arc<LatestInactivityObservation>,
) {
    stop_gnome_idle_monitor_poller(poller);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let probe = SystemGnomeIdleMonitorProbe;
        while !stop_signal.load(Ordering::SeqCst) {
            if let Some(idletime_ms) = probe.current_idletime_ms() {
                if !latest_observation
                    .publish(&sender, InactivityObservation::IdleTimeMs(idletime_ms))
                {
                    break;
                }
            }

            if stop_signal.load(Ordering::SeqCst) {
                break;
            }

            thread::sleep(GNOME_IDLE_POLL_INTERVAL);
        }
    });

    *poller = Some(GnomeIdleMonitorPoller { stop, handle });
}

fn stop_gnome_idle_monitor_poller(poller: &mut Option<GnomeIdleMonitorPoller>) {
    if let Some(poller) = poller.take() {
        poller.stop.store(true, Ordering::SeqCst);
        let _ = poller.handle.join();
    }
}

fn handle_gnome_inactivity_observation<W: Write, E: SessionActionExecutor>(
    writer: &mut W,
    dispatcher: &mut SessionEventDispatcher<E>,
    inactivity: &mut InactivityEngine,
    observation: InactivityObservation,
) -> Result<(), SessionRunnerError> {
    let decision = inactivity.observe(observation);
    let event = match (observation, decision) {
        (_, InactivityDecision::NoOp) => None,
        (_, InactivityDecision::BlankNow) => Some(SessionEvent::Idle),
        (InactivityObservation::ProviderActive, InactivityDecision::RestoreNow) => {
            Some(SessionEvent::Active)
        }
        (InactivityObservation::WakeRequested, InactivityDecision::RestoreNow) => {
            Some(SessionEvent::WakeRequested)
        }
        (
            InactivityObservation::IdleTimeMs(_) | InactivityObservation::UserActivityObserved,
            InactivityDecision::RestoreNow,
        ) => Some(SessionEvent::UserActivity),
        (InactivityObservation::ProviderIdle, InactivityDecision::RestoreNow) => None,
    };

    if let Some(event) = event {
        dispatcher.dispatch_event(writer, event)?;
    }

    Ok(())
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
        handle_gnome_inactivity_observation, normalize_idle_timeout_secs,
        parse_gnome_idletime_output, process_gnome_monitor_output, shell_quote,
        LatestInactivityObservation, RunnerMessage, SessionActionExecutor, SessionEventDispatcher,
    };
    use crate::session::inactivity::{
        InactivityEngine, InactivityObservation, InactivityThresholds,
    };
    use crate::session::SessionEvent;
    use crate::RunError;
    use std::io::Cursor;
    use std::path::Path;
    use std::sync::mpsc;

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
    fn zero_idle_timeout_falls_back_to_default() {
        assert_eq!(
            normalize_idle_timeout_secs(0),
            crate::config::DEFAULT_IDLE_TIMEOUT
        );
        assert_eq!(normalize_idle_timeout_secs(180), 180);
    }

    #[test]
    fn latest_inactivity_observation_coalesces_pending_samples() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();

        assert!(latest.publish(&sender, InactivityObservation::IdleTimeMs(1_000)));
        assert!(latest.publish(&sender, InactivityObservation::IdleTimeMs(2_000)));

        assert!(matches!(
            receiver.recv().expect("notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(InactivityObservation::IdleTimeMs(2_000))
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn latest_inactivity_observation_notifies_again_after_take() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();

        assert!(latest.publish(&sender, InactivityObservation::IdleTimeMs(1_000)));
        assert!(matches!(
            receiver.recv().expect("first notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(InactivityObservation::IdleTimeMs(1_000))
        );

        assert!(latest.publish(&sender, InactivityObservation::IdleTimeMs(3_000)));
        assert!(matches!(
            receiver.recv().expect("second notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(InactivityObservation::IdleTimeMs(3_000))
        );
    }

    #[test]
    fn idletime_observation_blanks_when_threshold_is_crossed() {
        let executor = FakeActionExecutor {
            screen_off_output: "screen-off output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut inactivity = InactivityEngine::new(InactivityThresholds {
            blank_threshold_ms: 1_000,
            active_threshold_ms: 100,
        });
        let mut output = Vec::new();

        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(1_000),
        )
        .expect("blank from idletime observation");
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Session became idle."));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
    }

    #[test]
    fn idletime_observation_restores_when_activity_returns() {
        let executor = FakeActionExecutor {
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut inactivity = InactivityEngine::new(InactivityThresholds {
            blank_threshold_ms: 1_000,
            active_threshold_ms: 100,
        });
        let mut output = Vec::new();

        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(1_000),
        )
        .expect("blank from idletime observation");
        let mut output = Vec::new();

        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(99),
        )
        .expect("restore from idletime observation");
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("user-activity"));
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
    }

    #[test]
    fn failed_blank_from_idletime_is_not_retried_while_session_stays_idle() {
        let executor = FakeActionExecutor {
            screen_off_error: Some("tv did not respond".to_string()),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut inactivity = InactivityEngine::new(InactivityThresholds {
            blank_threshold_ms: 1_000,
            active_threshold_ms: 100,
        });
        let mut output = Vec::new();

        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(1_000),
        )
        .expect("initial blank attempt should be logged");
        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(1_500),
        )
        .expect("repeated idle sample should not retry blank");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen-off action failed. tv did not respond"));
        assert_eq!(dispatcher.executor.screen_off_calls, 1);
    }

    #[test]
    fn failed_restore_from_idletime_is_not_retried_while_session_stays_active() {
        let executor = FakeActionExecutor {
            screen_on_error: Some("tv is still waking".to_string()),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut inactivity = InactivityEngine::new(InactivityThresholds {
            blank_threshold_ms: 1_000,
            active_threshold_ms: 100,
        });
        let mut output = Vec::new();

        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(1_000),
        )
        .expect("blank should succeed");
        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(99),
        )
        .expect("initial restore attempt should be logged");
        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::IdleTimeMs(0),
        )
        .expect("repeated active sample should not retry restore");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("screen restore action failed. tv is still waking"));
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
    }

    #[test]
    fn provider_active_restores_once_from_unknown_state() {
        let executor = FakeActionExecutor {
            screen_on_output: "screen-on output\n".to_string(),
            ..FakeActionExecutor::default()
        };
        let mut dispatcher = SessionEventDispatcher::new(executor);
        let mut inactivity = InactivityEngine::new(InactivityThresholds {
            blank_threshold_ms: 1_000,
            active_threshold_ms: 100,
        });
        let mut output = Vec::new();

        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::ProviderActive,
        )
        .expect("initial provider active should restore");
        handle_gnome_inactivity_observation(
            &mut output,
            &mut dispatcher,
            &mut inactivity,
            InactivityObservation::ProviderActive,
        )
        .expect("repeated provider active should not duplicate restore");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("active"));
        assert_eq!(dispatcher.executor.screen_on_calls, 1);
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
