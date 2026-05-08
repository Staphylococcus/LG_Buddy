use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply;
use dbus::blocking::Connection as DbusConnection;
use dbus::channel::MatchingReceiver;
use dbus::message::{MatchRule as DbusMatchRule, MessageType as DbusMessageType};
use dbus_crossroads::{Crossroads, MethodErr};
use semver::Version;

use crate::notifications::{
    parse_notification_signal, FreedesktopNotifier, Notification, NotificationAction,
    NotificationError, NotificationId, NotificationSignal, Notifier, NOTIFICATION_INTERFACE,
    NOTIFICATION_PATH, NOTIFICATION_SERVICE,
};
use crate::session_bus::{
    bus_signal_from_dbus_message, SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH,
    DBUS_SERVICE_NAME,
};
use crate::updates::UpdateChannel;
use crate::version::ReleaseChannel;

pub(crate) const SESSION_BUS_NAME: &str = "io.github.Staphylococcus.LGBuddy";
pub(crate) const SESSION_OBJECT_PATH: &str = "/io/github/Staphylococcus/LGBuddy/Session";
pub(crate) const SESSION_INTERFACE: &str = "io.github.Staphylococcus.LGBuddy.Session1";
pub(crate) const SHOW_UPDATE_NOTIFICATION_METHOD: &str = "ShowUpdateNotification";
pub(crate) const VIEW_RELEASE_ACTION_KEY: &str = "view-release";

const SESSION_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const SESSION_START_TIMEOUT: Duration = Duration::from_secs(2);
const NOTIFICATION_OWNER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateNotificationRequest {
    check_channel: UpdateChannel,
    current_version: Version,
    current_channel: ReleaseChannel,
    latest_version: Version,
    latest_channel: UpdateChannel,
    release_url: String,
}

impl UpdateNotificationRequest {
    pub(crate) fn new(
        check_channel: UpdateChannel,
        current_version: Version,
        current_channel: ReleaseChannel,
        latest_version: Version,
        latest_channel: UpdateChannel,
        release_url: impl Into<String>,
    ) -> Result<Self, UpdateNotificationError> {
        let release_url = release_url.into();
        if !valid_release_url(&release_url) {
            return Err(UpdateNotificationError::InvalidRequest(format!(
                "invalid release URL `{release_url}`"
            )));
        }

        Ok(Self {
            check_channel,
            current_version,
            current_channel,
            latest_version,
            latest_channel,
            release_url,
        })
    }

    pub(crate) fn from_dbus_fields(
        check_channel: String,
        current_version: String,
        current_channel: String,
        latest_version: String,
        latest_channel: String,
        release_url: String,
    ) -> Result<Self, UpdateNotificationError> {
        Self::new(
            parse_update_channel(&check_channel)?,
            parse_version("current version", &current_version)?,
            parse_release_channel(&current_channel)?,
            parse_version("latest version", &latest_version)?,
            parse_update_channel(&latest_channel)?,
            release_url,
        )
    }

    pub(crate) fn to_dbus_fields(&self) -> (String, String, String, String, String, String) {
        (
            self.check_channel.as_str().to_string(),
            self.current_version.to_string(),
            self.current_channel.as_str().to_string(),
            self.latest_version.to_string(),
            self.latest_channel.as_str().to_string(),
            self.release_url.clone(),
        )
    }

    pub(crate) fn release_url(&self) -> &str {
        &self.release_url
    }

    fn notification(&self, actions_supported: bool) -> Notification {
        let mut notification = Notification::new(
            "LG Buddy update available",
            format!(
                "LG Buddy {} ({}) is available.\nCurrent: {} ({})\n{}",
                self.latest_version,
                self.latest_channel.as_str(),
                self.current_version,
                self.current_channel.as_str(),
                self.release_url
            ),
        );

        if actions_supported {
            notification.actions = vec![NotificationAction {
                key: VIEW_RELEASE_ACTION_KEY.to_string(),
                label: "View Release".to_string(),
            }];
        }

        notification
    }
}

fn valid_release_url(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
}

fn parse_update_channel(value: &str) -> Result<UpdateChannel, UpdateNotificationError> {
    match value {
        "stable" => Ok(UpdateChannel::Stable),
        "prerelease" => Ok(UpdateChannel::Prerelease),
        other => Err(UpdateNotificationError::InvalidRequest(format!(
            "invalid update channel `{other}`"
        ))),
    }
}

fn parse_release_channel(value: &str) -> Result<ReleaseChannel, UpdateNotificationError> {
    match value {
        "dev" => Ok(ReleaseChannel::Dev),
        "prerelease" => Ok(ReleaseChannel::Prerelease),
        "stable" => Ok(ReleaseChannel::Stable),
        other => Err(UpdateNotificationError::InvalidRequest(format!(
            "invalid release channel `{other}`"
        ))),
    }
}

fn parse_version(label: &'static str, value: &str) -> Result<Version, UpdateNotificationError> {
    Version::parse(value).map_err(|err| {
        UpdateNotificationError::InvalidRequest(format!("invalid {label} `{value}`: {err}"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateNotificationOutcome {
    Sent,
}

impl UpdateNotificationOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
        }
    }

    fn parse(value: &str) -> Result<Self, UpdateNotificationError> {
        match value {
            "sent" => Ok(Self::Sent),
            other => Err(UpdateNotificationError::Transport(format!(
                "LG Buddy session service returned unknown update notification outcome `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateNotificationError {
    InvalidRequest(String),
    Transport(String),
    Notification(NotificationError),
    Session(SessionServiceError),
    OpenRelease { url: String, message: String },
}

impl fmt::Display for UpdateNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => {
                write!(f, "invalid update notification request: {message}")
            }
            Self::Transport(message) => {
                write!(f, "could not request update notification from LG Buddy session service: {message}")
            }
            Self::Notification(err) => write!(f, "{err}"),
            Self::Session(err) => write!(f, "{err}"),
            Self::OpenRelease { url, message } => {
                write!(f, "could not open release URL `{url}`: {message}")
            }
        }
    }
}

impl Error for UpdateNotificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Notification(err) => Some(err),
            Self::Session(err) => Some(err),
            Self::InvalidRequest(_) | Self::Transport(_) | Self::OpenRelease { .. } => None,
        }
    }
}

impl From<NotificationError> for UpdateNotificationError {
    fn from(value: NotificationError) -> Self {
        Self::Notification(value)
    }
}

impl From<SessionServiceError> for UpdateNotificationError {
    fn from(value: SessionServiceError) -> Self {
        Self::Session(value)
    }
}

pub(crate) trait UpdateNotificationHandoff {
    fn show_update_notification(
        &self,
        request: &UpdateNotificationRequest,
    ) -> Result<UpdateNotificationOutcome, UpdateNotificationError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SessionBusUpdateNotificationHandoff;

impl UpdateNotificationHandoff for SessionBusUpdateNotificationHandoff {
    fn show_update_notification(
        &self,
        request: &UpdateNotificationRequest,
    ) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
        let connection = DbusConnection::new_session()
            .map_err(|err| UpdateNotificationError::Transport(err.to_string()))?;
        let proxy = connection.with_proxy(
            SESSION_BUS_NAME,
            SESSION_OBJECT_PATH,
            Duration::from_secs(2),
        );
        let (check_channel, current_version, current_channel, latest_version, latest_channel, url) =
            request.to_dbus_fields();
        let (outcome,): (String,) = proxy
            .method_call(
                SESSION_INTERFACE,
                SHOW_UPDATE_NOTIFICATION_METHOD,
                (
                    check_channel,
                    current_version,
                    current_channel,
                    latest_version,
                    latest_channel,
                    url,
                ),
            )
            .map_err(|err| UpdateNotificationError::Transport(err.to_string()))?;

        UpdateNotificationOutcome::parse(&outcome)
    }
}

pub(crate) trait ReleaseOpener {
    fn open_release(&self, url: &str) -> Result<(), UpdateNotificationError>;
}

#[derive(Debug, Clone)]
pub(crate) struct SystemReleaseOpener {
    command_path: PathBuf,
}

impl Default for SystemReleaseOpener {
    fn default() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_XDG_OPEN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("xdg-open")),
        }
    }
}

impl ReleaseOpener for SystemReleaseOpener {
    fn open_release(&self, url: &str) -> Result<(), UpdateNotificationError> {
        let output = ProcessCommand::new(&self.command_path)
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| UpdateNotificationError::OpenRelease {
                url: url.to_string(),
                message: err.to_string(),
            })?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(UpdateNotificationError::OpenRelease {
                url: url.to_string(),
                message: if stderr.is_empty() {
                    format!("opener exited with status {}", output.status)
                } else {
                    stderr
                },
            })
        }
    }
}

pub(crate) struct SessionUpdateNotificationDispatcher<N, O> {
    notifier: N,
    opener: O,
    pending: HashMap<NotificationId, UpdateNotificationRequest>,
}

impl<N, O> SessionUpdateNotificationDispatcher<N, O> {
    fn new(notifier: N, opener: O) -> Self {
        Self {
            notifier,
            opener,
            pending: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

impl<N, O> SessionUpdateNotificationDispatcher<N, O>
where
    N: Notifier,
    O: ReleaseOpener,
{
    fn show_update_notification(
        &mut self,
        request: UpdateNotificationRequest,
    ) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
        let capabilities = self.notifier.capabilities()?;
        let notification = request.notification(capabilities.actions);
        let notification_id = self.notifier.notify(&notification)?;

        if capabilities.actions {
            self.pending.insert(notification_id, request);
        }

        Ok(UpdateNotificationOutcome::Sent)
    }

    fn handle_notification_signal(
        &mut self,
        signal: NotificationSignal,
    ) -> Result<Option<SessionNotificationEvent>, UpdateNotificationError> {
        match signal {
            NotificationSignal::ActionInvoked { id, action_key } => {
                let Some(request) = self.pending.remove(&id) else {
                    return Ok(None);
                };

                if action_key == VIEW_RELEASE_ACTION_KEY {
                    self.opener.open_release(request.release_url())?;
                    Ok(Some(SessionNotificationEvent::ReleaseOpened))
                } else {
                    Ok(Some(SessionNotificationEvent::UnknownAction))
                }
            }
            NotificationSignal::Closed { id, .. } => {
                if self.pending.remove(&id).is_some() {
                    Ok(Some(SessionNotificationEvent::Closed))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionNotificationEvent {
    ReleaseOpened,
    Closed,
    UnknownAction,
}

#[derive(Debug, Clone)]
pub enum SessionServiceError {
    Transport(String),
    NameUnavailable {
        name: &'static str,
        reply: RequestNameReply,
    },
    StartupTimeout,
    Poisoned,
}

impl fmt::Display for SessionServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => {
                write!(f, "LG Buddy session D-Bus service error: {message}")
            }
            Self::NameUnavailable { name, reply } => {
                write!(
                    f,
                    "could not own LG Buddy session D-Bus name `{name}`: {reply:?}"
                )
            }
            Self::StartupTimeout => write!(f, "timed out starting LG Buddy session D-Bus service"),
            Self::Poisoned => write!(f, "LG Buddy session notification state lock was poisoned"),
        }
    }
}

impl Error for SessionServiceError {}

impl From<SessionBusError> for SessionServiceError {
    fn from(value: SessionBusError) -> Self {
        Self::Transport(value.to_string())
    }
}

pub(crate) struct SessionNotificationServiceThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for SessionNotificationServiceThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn spawn_session_notification_service(
) -> Result<SessionNotificationServiceThread, SessionServiceError> {
    let dispatcher = SessionUpdateNotificationDispatcher::new(
        FreedesktopNotifier,
        SystemReleaseOpener::default(),
    );
    spawn_session_notification_service_with(dispatcher)
}

fn spawn_session_notification_service_with<N, O>(
    dispatcher: SessionUpdateNotificationDispatcher<N, O>,
) -> Result<SessionNotificationServiceThread, SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let started = Arc::new(AtomicBool::new(false));
    let thread_started = Arc::clone(&started);
    let (ready_sender, ready_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let loop_ready = ready_sender.clone();
        let result = run_session_notification_service_loop(
            dispatcher,
            thread_stop,
            loop_ready,
            thread_started,
        );
        if let Err(err) = result {
            if started.load(Ordering::SeqCst) {
                eprintln!("LG Buddy Session: {err}");
            } else {
                let _ = ready_sender.send(Err(err));
            }
        }
    });

    wait_for_session_notification_service_start_with_timeout(
        stop,
        handle,
        ready_receiver,
        SESSION_START_TIMEOUT,
    )
}

fn wait_for_session_notification_service_start_with_timeout(
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    ready_receiver: mpsc::Receiver<Result<(), SessionServiceError>>,
    timeout: Duration,
) -> Result<SessionNotificationServiceThread, SessionServiceError> {
    match ready_receiver.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(SessionNotificationServiceThread {
            stop,
            handle: Some(handle),
        }),
        Ok(Err(err)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            Err(err)
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop.store(true, Ordering::SeqCst);
            drop(handle);
            Err(SessionServiceError::StartupTimeout)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            Err(SessionServiceError::Transport(
                "session service thread exited before startup completed".to_string(),
            ))
        }
    }
}

fn run_session_notification_service_loop<N, O>(
    dispatcher: SessionUpdateNotificationDispatcher<N, O>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), SessionServiceError>>,
    started: Arc<AtomicBool>,
) -> Result<(), SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
{
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }

    let connection = DbusConnection::new_session()
        .map_err(|err| SessionServiceError::Transport(err.to_string()))?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }

    let name_reply = connection
        .request_name(SESSION_BUS_NAME, false, false, true)
        .map_err(|err| SessionServiceError::Transport(err.to_string()))?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }

    match name_reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {}
        reply => {
            return Err(SessionServiceError::NameUnavailable {
                name: SESSION_BUS_NAME,
                reply,
            })
        }
    }

    let dispatcher = Arc::new(Mutex::new(dispatcher));
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }
    register_session_methods(&connection, Arc::clone(&dispatcher))?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }
    register_notification_signal_handler(&connection, dispatcher)?;
    if session_service_startup_stopped(&stop) {
        return Ok(());
    }
    if ready.send(Ok(())).is_err() {
        return Ok(());
    }
    started.store(true, Ordering::SeqCst);

    while !stop.load(Ordering::SeqCst) {
        connection
            .process(SESSION_PROCESS_INTERVAL)
            .map_err(|err| SessionServiceError::Transport(err.to_string()))?;
    }

    Ok(())
}

fn session_service_startup_stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::SeqCst)
}

fn register_session_methods<N, O>(
    connection: &DbusConnection,
    dispatcher: Arc<Mutex<SessionUpdateNotificationDispatcher<N, O>>>,
) -> Result<(), SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
{
    let mut crossroads = Crossroads::new();
    let method_dispatcher = Arc::clone(&dispatcher);
    let iface = crossroads.register(SESSION_INTERFACE, move |builder| {
        let method_dispatcher = Arc::clone(&method_dispatcher);
        builder.method(
            SHOW_UPDATE_NOTIFICATION_METHOD,
            (
                "check_channel",
                "current_version",
                "current_channel",
                "latest_version",
                "latest_channel",
                "release_url",
            ),
            ("outcome",),
            move |_,
                  _,
                  (
                check_channel,
                current_version,
                current_channel,
                latest_version,
                latest_channel,
                release_url,
            ): (String, String, String, String, String, String)| {
                let request = UpdateNotificationRequest::from_dbus_fields(
                    check_channel,
                    current_version,
                    current_channel,
                    latest_version,
                    latest_channel,
                    release_url,
                )
                .map_err(|err| MethodErr::failed(&err.to_string()))?;

                let outcome = method_dispatcher
                    .lock()
                    .map_err(|_| MethodErr::failed(&SessionServiceError::Poisoned.to_string()))?
                    .show_update_notification(request)
                    .map_err(|err| MethodErr::failed(&err.to_string()))?;

                Ok((outcome.as_str().to_string(),))
            },
        );
    });
    crossroads.insert(SESSION_OBJECT_PATH, &[iface], ());

    let shared_crossroads = Arc::new(Mutex::new(crossroads));
    let crossroads_receiver = Arc::clone(&shared_crossroads);
    connection.start_receive(
        DbusMatchRule::new_method_call(),
        Box::new(move |message, conn| {
            let result = crossroads_receiver
                .lock()
                .map_err(|_| SessionServiceError::Poisoned)
                .and_then(|mut crossroads| {
                    crossroads
                        .handle_message(message, conn)
                        .map(|_| ())
                        .map_err(|_| {
                            SessionServiceError::Transport(
                                "failed to handle session D-Bus method call".to_string(),
                            )
                        })
                });

            if let Err(err) = result {
                eprintln!("LG Buddy Session: {err}");
            }
            true
        }),
    );

    Ok(())
}

fn register_notification_signal_handler<N, O>(
    connection: &DbusConnection,
    dispatcher: Arc<Mutex<SessionUpdateNotificationDispatcher<N, O>>>,
) -> Result<(), SessionServiceError>
where
    N: Notifier + Send + 'static,
    O: ReleaseOpener + Send + 'static,
{
    let signal_rule = DbusMatchRule::new()
        .with_type(DbusMessageType::Signal)
        .with_path(NOTIFICATION_PATH)
        .with_interface(NOTIFICATION_INTERFACE);
    connection
        .add_match_no_cb(&signal_rule.match_str())
        .map_err(|err| SessionServiceError::Transport(err.to_string()))?;

    connection.start_receive(
        signal_rule,
        Box::new(move |message, conn| {
            let bus_signal = match bus_signal_from_dbus_message(message) {
                Ok(signal) => signal,
                Err(err) => {
                    eprintln!("LG Buddy Session: notification signal parse failed: {err}");
                    return true;
                }
            };

            let Some(notification_signal) = parse_notification_signal(&bus_signal) else {
                return true;
            };

            let notification_service_owner = match current_notification_service_owner(conn) {
                Ok(owner) => owner,
                Err(err) => {
                    eprintln!("LG Buddy Session: notification signal owner check failed: {err}");
                    return true;
                }
            };
            if !notification_signal_sender_matches_owner(&bus_signal, &notification_service_owner) {
                return true;
            }

            let result = dispatcher
                .lock()
                .map_err(|_| UpdateNotificationError::Session(SessionServiceError::Poisoned))
                .and_then(|mut dispatcher| {
                    dispatcher.handle_notification_signal(notification_signal)
                });

            if let Err(err) = result {
                eprintln!("LG Buddy Session: update notification action failed: {err}");
            }

            true
        }),
    );

    Ok(())
}

fn current_notification_service_owner(
    connection: &DbusConnection,
) -> Result<String, SessionServiceError> {
    let proxy = connection.with_proxy(
        DBUS_SERVICE_NAME,
        DBUS_OBJECT_PATH,
        NOTIFICATION_OWNER_LOOKUP_TIMEOUT,
    );
    let (owner,): (String,) = proxy
        .method_call(DBUS_INTERFACE, "GetNameOwner", (NOTIFICATION_SERVICE,))
        .map_err(|err| {
            SessionServiceError::Transport(format!(
                "could not resolve `{NOTIFICATION_SERVICE}` owner: {err}"
            ))
        })?;

    Ok(owner)
}

fn notification_signal_sender_matches_owner(
    signal: &crate::session_bus::BusSignal,
    owner: &str,
) -> bool {
    signal.sender.as_deref() == Some(owner)
}

#[cfg(test)]
mod tests {
    use super::{
        notification_signal_sender_matches_owner,
        wait_for_session_notification_service_start_with_timeout, ReleaseOpener,
        SessionServiceError, SessionUpdateNotificationDispatcher, UpdateNotificationError,
        UpdateNotificationOutcome, UpdateNotificationRequest, VIEW_RELEASE_ACTION_KEY,
    };
    use crate::notifications::{
        Notification, NotificationCapabilities, NotificationError, NotificationId,
        NotificationSignal, Notifier, NOTIFICATION_INTERFACE, NOTIFICATION_PATH,
    };
    use crate::session_bus::BusSignal;
    use crate::updates::UpdateChannel;
    use crate::version::ReleaseChannel;
    use semver::Version;
    use std::cell::RefCell;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Debug)]
    struct RecordingNotifier {
        capabilities: NotificationCapabilities,
        result: Result<NotificationId, NotificationError>,
        notifications: RefCell<Vec<Notification>>,
    }

    impl RecordingNotifier {
        fn new(actions: bool) -> Self {
            Self {
                capabilities: NotificationCapabilities { actions },
                result: Ok(NotificationId(7)),
                notifications: RefCell::new(Vec::new()),
            }
        }

        fn failing() -> Self {
            Self {
                capabilities: NotificationCapabilities { actions: true },
                result: Err(NotificationError::Transport("bus unavailable".to_string())),
                notifications: RefCell::new(Vec::new()),
            }
        }

        fn notifications(&self) -> Vec<Notification> {
            self.notifications.borrow().clone()
        }
    }

    impl Notifier for RecordingNotifier {
        fn capabilities(&self) -> Result<NotificationCapabilities, NotificationError> {
            Ok(self.capabilities)
        }

        fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError> {
            self.notifications.borrow_mut().push(notification.clone());
            self.result.clone()
        }
    }

    #[derive(Debug, Default)]
    struct RecordingOpener {
        opened: RefCell<Vec<String>>,
        fail: bool,
    }

    impl RecordingOpener {
        fn failing() -> Self {
            Self {
                opened: RefCell::new(Vec::new()),
                fail: true,
            }
        }

        fn opened(&self) -> Vec<String> {
            self.opened.borrow().clone()
        }
    }

    impl ReleaseOpener for RecordingOpener {
        fn open_release(&self, url: &str) -> Result<(), UpdateNotificationError> {
            self.opened.borrow_mut().push(url.to_string());
            if self.fail {
                Err(UpdateNotificationError::OpenRelease {
                    url: url.to_string(),
                    message: "open failed".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    fn request() -> UpdateNotificationRequest {
        UpdateNotificationRequest::new(
            UpdateChannel::Stable,
            Version::parse("1.1.0").unwrap(),
            ReleaseChannel::Stable,
            Version::parse("1.1.1").unwrap(),
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
        )
        .unwrap()
    }

    #[test]
    fn request_round_trips_through_dbus_fields() {
        let request = request();
        let fields = request.to_dbus_fields();

        let parsed = UpdateNotificationRequest::from_dbus_fields(
            fields.0, fields.1, fields.2, fields.3, fields.4, fields.5,
        )
        .expect("request should parse");

        assert_eq!(parsed, request);
    }

    #[test]
    fn invalid_request_fields_are_rejected() {
        assert!(UpdateNotificationRequest::from_dbus_fields(
            "nightly".to_string(),
            "1.1.0".to_string(),
            "stable".to_string(),
            "1.1.1".to_string(),
            "stable".to_string(),
            "https://github.test/releases/tag/v1.1.1".to_string(),
        )
        .is_err());
        assert!(UpdateNotificationRequest::from_dbus_fields(
            "stable".to_string(),
            "not-semver".to_string(),
            "stable".to_string(),
            "1.1.1".to_string(),
            "stable".to_string(),
            "https://github.test/releases/tag/v1.1.1".to_string(),
        )
        .is_err());
        assert!(UpdateNotificationRequest::from_dbus_fields(
            "stable".to_string(),
            "1.1.0".to_string(),
            "stable".to_string(),
            "1.1.1".to_string(),
            "stable".to_string(),
            "".to_string(),
        )
        .is_err());
    }

    #[test]
    fn action_capable_notification_attaches_view_release_button() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let mut dispatcher = SessionUpdateNotificationDispatcher::new(notifier, opener);

        let outcome = dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        assert_eq!(outcome, UpdateNotificationOutcome::Sent);
        let notifications = dispatcher.notifier.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].actions.len(), 1);
        assert_eq!(notifications[0].actions[0].key, VIEW_RELEASE_ACTION_KEY);
        assert_eq!(dispatcher.pending_len(), 1);
    }

    #[test]
    fn passive_notification_does_not_store_pending_action_state() {
        let notifier = RecordingNotifier::new(false);
        let opener = RecordingOpener::default();
        let mut dispatcher = SessionUpdateNotificationDispatcher::new(notifier, opener);

        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        assert!(dispatcher.notifier.notifications()[0].actions.is_empty());
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn notification_failure_does_not_store_pending_state() {
        let notifier = RecordingNotifier::failing();
        let opener = RecordingOpener::default();
        let mut dispatcher = SessionUpdateNotificationDispatcher::new(notifier, opener);

        dispatcher
            .show_update_notification(request())
            .expect_err("notification should fail");

        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn startup_timeout_does_not_join_blocked_worker() {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ready_sender = ready_sender;
            let _ = release_receiver.recv_timeout(Duration::from_secs(5));
        });

        let started = Instant::now();
        let result = wait_for_session_notification_service_start_with_timeout(
            Arc::clone(&stop),
            handle,
            ready_receiver,
            Duration::from_millis(20),
        );

        assert!(matches!(result, Err(SessionServiceError::StartupTimeout)));
        assert!(stop.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_millis(500));
        release_sender
            .send(())
            .expect("blocked worker should still be releasable");
    }

    #[test]
    fn notification_signal_sender_must_match_notification_daemon_owner() {
        let signal = BusSignal::new(NOTIFICATION_PATH, NOTIFICATION_INTERFACE, "ActionInvoked")
            .with_sender(":1.42");

        assert!(notification_signal_sender_matches_owner(&signal, ":1.42"));
        assert!(!notification_signal_sender_matches_owner(&signal, ":1.99"));
        assert!(!notification_signal_sender_matches_owner(
            &BusSignal::new(NOTIFICATION_PATH, NOTIFICATION_INTERFACE, "ActionInvoked"),
            ":1.42",
        ));
    }

    #[test]
    fn view_release_action_opens_url_and_clears_pending_state() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let mut dispatcher = SessionUpdateNotificationDispatcher::new(notifier, opener);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::ActionInvoked {
                id: NotificationId(7),
                action_key: VIEW_RELEASE_ACTION_KEY.to_string(),
            })
            .expect("action should succeed");

        assert_eq!(
            dispatcher.opener.opened(),
            vec!["https://github.test/releases/tag/v1.1.1".to_string()]
        );
        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn opener_failure_clears_pending_state_and_reports_error() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::failing();
        let mut dispatcher = SessionUpdateNotificationDispatcher::new(notifier, opener);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::ActionInvoked {
                id: NotificationId(7),
                action_key: VIEW_RELEASE_ACTION_KEY.to_string(),
            })
            .expect_err("open should fail");

        assert_eq!(dispatcher.pending_len(), 0);
    }

    #[test]
    fn closed_signal_clears_pending_state() {
        let notifier = RecordingNotifier::new(true);
        let opener = RecordingOpener::default();
        let mut dispatcher = SessionUpdateNotificationDispatcher::new(notifier, opener);
        dispatcher
            .show_update_notification(request())
            .expect("notification should send");

        dispatcher
            .handle_notification_signal(NotificationSignal::Closed {
                id: NotificationId(7),
                reason: crate::notifications::NotificationCloseReason::Dismissed,
            })
            .expect("close should succeed");

        assert_eq!(dispatcher.pending_len(), 0);
    }
}
