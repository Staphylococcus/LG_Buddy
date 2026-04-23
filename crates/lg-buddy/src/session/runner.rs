use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{run_screen_off, run_screen_on};
use crate::config::{
    load_config, resolve_config_path_from_env, ScreenBackend, DEFAULT_IDLE_TIMEOUT,
};
use crate::gnome::{
    current_idle_monitor_idletime_ms, map_screen_saver_signal, resolve_screen_saver_owner,
    screen_saver_owner_changed, GnomeBackend, SystemGnomeProbe, GNOME_SCREEN_SAVER_INTERFACE,
    GNOME_SCREEN_SAVER_PATH, GNOME_SHELL_NAME,
};
use crate::session::inactivity::{
    InactivityDecision, InactivityEngine, InactivityObservation, InactivityThresholds,
};
use crate::session::{SessionBackend, SessionBackendError, SessionEvent};
use crate::session_bus::{
    new_session_bus_client, BusSignal, BusSignalMatch, SessionBusClient, DBUS_INTERFACE,
    DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};
use crate::RunError;

const GNOME_WAIT_TIMEOUT_SECS: u64 = 15;
const GNOME_ACTIVE_THRESHOLD_MS: u64 = 1000;
const GNOME_BUS_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const GNOME_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV: &str = "LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS";

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
    let monitor_handle = spawn_gnome_monitor_thread(sender.clone(), Arc::clone(&latest_inactivity));
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

fn wait_for_gnome_shell() -> Result<(), SessionRunnerError> {
    let mut bus = new_session_bus_client().map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to open GNOME session bus client: {err}"),
    })?;
    bus.wait_for_name(
        GNOME_SHELL_NAME,
        Duration::from_secs(GNOME_WAIT_TIMEOUT_SECS),
    )
    .map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed waiting for GNOME Shell on the session bus: {err}"),
    })
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

fn resolve_gnome_monitor_test_timeout() -> Option<Duration> {
    std::env::var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .map(Duration::from_secs_f64)
}

fn spawn_gnome_monitor_thread(
    sender: mpsc::Sender<RunnerMessage>,
    latest_observation: Arc<LatestInactivityObservation>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let result = run_gnome_monitor_process(&sender, &latest_observation);
        let _ = sender.send(RunnerMessage::MonitorExited(result));
    })
}

fn run_gnome_monitor_process(
    sender: &mpsc::Sender<RunnerMessage>,
    latest_observation: &LatestInactivityObservation,
) -> Result<(), SessionRunnerError> {
    let mut bus = new_session_bus_client().map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to open GNOME session bus client: {err}"),
    })?;
    bus.add_signal_match(BusSignalMatch {
        sender: None,
        path: Some(GNOME_SCREEN_SAVER_PATH),
        interface: Some(GNOME_SCREEN_SAVER_INTERFACE),
        member: None,
    })
    .map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to subscribe to GNOME ScreenSaver signals: {err}"),
    })?;
    bus.add_signal_match(BusSignalMatch {
        sender: Some(DBUS_SERVICE_NAME),
        path: Some(DBUS_OBJECT_PATH),
        interface: Some(DBUS_INTERFACE),
        member: Some("NameOwnerChanged"),
    })
    .map_err(|err| SessionRunnerError::Failed {
        backend: ScreenBackend::Gnome,
        message: format!("failed to subscribe to D-Bus owner changes: {err}"),
    })?;
    let mut trusted_screen_saver_signals = TrustedScreenSaverSignals::new(Some(
        resolve_screen_saver_owner(&mut bus).map_err(|err| SessionRunnerError::Failed {
            backend: ScreenBackend::Gnome,
            message: format!("failed to resolve GNOME ScreenSaver owner: {err}"),
        })?,
    ));

    let started = Instant::now();
    let test_timeout = resolve_gnome_monitor_test_timeout();
    let mut next_idle_poll = Instant::now();

    loop {
        if let Some(timeout) = test_timeout {
            if started.elapsed() >= timeout {
                return Ok(());
            }
        }

        let now = Instant::now();
        if now >= next_idle_poll {
            if !poll_gnome_idle_monitor_once(&mut bus, sender, latest_observation) {
                return Ok(());
            }
            next_idle_poll = now + GNOME_IDLE_POLL_INTERVAL;
        }

        let now = Instant::now();
        let mut process_timeout = next_idle_poll
            .saturating_duration_since(now)
            .min(GNOME_BUS_PROCESS_INTERVAL);
        if let Some(timeout) = test_timeout {
            process_timeout = process_timeout.min(timeout.saturating_sub(started.elapsed()));
        }

        let Some(signal) =
            bus.process(process_timeout)
                .map_err(|err| SessionRunnerError::Failed {
                    backend: ScreenBackend::Gnome,
                    message: format!("GNOME session bus processing failed: {err}"),
                })?
        else {
            continue;
        };

        let Some(event) = trusted_screen_saver_signals.observe(&signal) else {
            continue;
        };

        if sender.send(RunnerMessage::SessionEvent(event)).is_err() {
            return Ok(());
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustedScreenSaverSignals {
    owner: Option<String>,
}

impl TrustedScreenSaverSignals {
    fn new(owner: Option<String>) -> Self {
        Self { owner }
    }

    fn observe(&mut self, signal: &BusSignal) -> Option<SessionEvent> {
        if signal.path == DBUS_OBJECT_PATH
            && signal.interface == DBUS_INTERFACE
            && signal.member == "NameOwnerChanged"
        {
            if signal.sender.as_deref() != Some(DBUS_SERVICE_NAME) {
                return None;
            }
            if let Some(new_owner) = screen_saver_owner_changed(signal) {
                self.owner = new_owner;
            }
            return None;
        }

        if signal.sender.as_deref() != self.owner.as_deref() {
            return None;
        }

        map_screen_saver_signal(signal)
    }
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

fn poll_gnome_idle_monitor_once(
    bus: &mut impl SessionBusClient,
    sender: &mpsc::Sender<RunnerMessage>,
    latest_observation: &LatestInactivityObservation,
) -> bool {
    let Ok(idletime_ms) = current_idle_monitor_idletime_ms(bus) else {
        return true;
    };

    latest_observation.publish(sender, InactivityObservation::IdleTimeMs(idletime_ms))
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
        poll_gnome_idle_monitor_once, shell_quote, LatestInactivityObservation, RunnerMessage,
        SessionActionExecutor, SessionEventDispatcher, TrustedScreenSaverSignals,
    };
    use crate::gnome::{
        GNOME_SCREEN_SAVER_INTERFACE, GNOME_SCREEN_SAVER_NAME, GNOME_SCREEN_SAVER_PATH,
    };
    use crate::session::inactivity::{
        InactivityEngine, InactivityObservation, InactivityThresholds,
    };
    use crate::session::SessionEvent;
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use crate::RunError;
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

    #[derive(Debug, Default)]
    struct FakeSessionBus {
        method_replies: Vec<Result<BusReply, SessionBusError>>,
        method_calls: Vec<(String, String, String, String)>,
    }

    impl SessionBusClient for FakeSessionBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            let _ = name;
            unreachable!("name probing is not used in runner poller tests")
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.method_calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            self.method_replies.remove(0)
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            let _ = rule;
            unreachable!("signal matches are not used in runner poller tests")
        }

        fn process(
            &mut self,
            timeout: std::time::Duration,
        ) -> Result<Option<BusSignal>, SessionBusError> {
            let _ = timeout;
            unreachable!("message pumping is not used in runner poller tests")
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
    fn gnome_idle_monitor_poller_publishes_idletime_from_session_bus() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let mut bus = FakeSessionBus {
            method_replies: vec![Ok(BusReply::new(vec![BusValue::U64(1_500)]))],
            ..FakeSessionBus::default()
        };

        assert!(poll_gnome_idle_monitor_once(&mut bus, &sender, &latest));
        assert!(matches!(
            receiver.recv().expect("notification"),
            RunnerMessage::InactivityObservationReady
        ));
        assert_eq!(
            latest.take(),
            Some(InactivityObservation::IdleTimeMs(1_500))
        );
    }

    #[test]
    fn gnome_idle_monitor_poller_ignores_bus_errors() {
        let (sender, receiver) = mpsc::channel();
        let latest = LatestInactivityObservation::default();
        let mut bus = FakeSessionBus {
            method_replies: vec![Err(SessionBusError::Transport(
                "simulated bus failure".to_string(),
            ))],
            ..FakeSessionBus::default()
        };

        assert!(poll_gnome_idle_monitor_once(&mut bus, &sender, &latest));
        assert!(receiver.try_recv().is_err());
        assert_eq!(latest.take(), None);
    }

    #[test]
    fn trusted_screen_saver_signals_accept_current_owner_events() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let signal = BusSignal::new(
            GNOME_SCREEN_SAVER_PATH,
            GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::Bool(true)]);

        assert_eq!(trusted.observe(&signal), Some(SessionEvent::Idle));
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_spoofed_senders() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let signal = BusSignal::new(
            GNOME_SCREEN_SAVER_PATH,
            GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        )
        .with_sender(":1.99");

        assert_eq!(trusted.observe(&signal), None);
    }

    #[test]
    fn trusted_screen_saver_signals_update_owner_after_bus_notification() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.42")
                .with_body(vec![BusValue::Bool(true)])
            ),
            None
        );
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.43")
                .with_body(vec![BusValue::Bool(true)])
            ),
            Some(SessionEvent::Idle)
        );
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_untrusted_owner_change_senders() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(":1.99")
            .with_body(vec![
                BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "WakeUpScreen",
                )
                .with_sender(":1.42")
            ),
            Some(SessionEvent::WakeRequested)
        );
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "WakeUpScreen",
                )
                .with_sender(":1.43")
            ),
            None
        );
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_events_after_owner_loss() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(String::new()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    GNOME_SCREEN_SAVER_PATH,
                    GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.42")
                .with_body(vec![BusValue::Bool(false)])
            ),
            None
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
