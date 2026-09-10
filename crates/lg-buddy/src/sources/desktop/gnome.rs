use std::fmt;
use std::time::{Duration, Instant};

use crate::events::EventSource;
use crate::session::inactivity::InactivityObservation;
use crate::session::{SessionEvent, SessionObservation};
use crate::session_bus::{
    get_name_owner, new_session_bus_client, parse_name_owner_changed_signal, BusMethodCall,
    BusSignal, BusSignalMatch, BusValue, SessionBusClient, SessionBusError, DBUS_INTERFACE,
    DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};

const GNOME_WAIT_TIMEOUT: Duration = Duration::from_secs(15);
const GNOME_BUS_PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const GNOME_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const GNOME_DESKTOP_ACTIVITY_WINDOW: Duration = Duration::from_millis(500);
const GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV: &str = "LG_BUDDY_GNOME_MONITOR_TEST_TIMEOUT_SECS";

pub const GNOME_SHELL_NAME: &str = "org.gnome.Shell";
pub const GNOME_SCREEN_SAVER_NAME: &str = "org.gnome.ScreenSaver";
pub const GNOME_SCREEN_SAVER_PATH: &str = "/org/gnome/ScreenSaver";
pub const GNOME_SCREEN_SAVER_INTERFACE: &str = "org.gnome.ScreenSaver";
pub const GNOME_IDLE_MONITOR_NAME: &str = "org.gnome.Mutter.IdleMonitor";
pub const GNOME_IDLE_MONITOR_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
pub const GNOME_IDLE_MONITOR_INTERFACE: &str = "org.gnome.Mutter.IdleMonitor";
pub const GNOME_SESSION_MANAGER_NAME: &str = "org.gnome.SessionManager";
pub const GNOME_SESSION_MANAGER_PATH: &str = "/org/gnome/SessionManager";
pub const GNOME_SESSION_MANAGER_INTERFACE: &str = "org.gnome.SessionManager";
pub const GNOME_IDLE_INHIBITION_FLAG: u32 = 8;
pub const GNOME_REQUIRED_SERVICES_REASON: &str =
    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required";
pub const GNOME_IDLE_INHIBITORS_REQUIRED_REASON: &str =
    "GNOME SessionManager is required when idle inhibitor honoring is enabled";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct GnomeServiceStatus {
    shell_available: bool,
    screen_saver_available: bool,
    idle_monitor_available: bool,
}

impl GnomeServiceStatus {
    fn can_start(&self) -> bool {
        self.shell_available && self.screen_saver_available && self.idle_monitor_available
    }
}

pub(crate) struct GnomeSource {
    bus: Box<dyn SessionBusClient + Send>,
    trusted_screen_saver_signals: TrustedScreenSaverSignals,
    trusted_session_manager_signals: Option<TrustedSessionManagerSignals>,
    initial_idle_blanking_allowed: Option<bool>,
    activity_watch: Option<GnomeActivityWatch>,
}

#[derive(Debug)]
pub(crate) enum GnomeSourceError {
    Unavailable(&'static str),
    Failed(String),
}

impl fmt::Display for GnomeSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "{reason}"),
            Self::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for GnomeSourceError {}

impl GnomeSource {
    pub(crate) fn connect(honor_idle_inhibitors: bool) -> Result<Self, GnomeSourceError> {
        let mut bus = new_session_bus_client().map_err(|err| {
            GnomeSourceError::Failed(format!("failed to open GNOME session bus client: {err}"))
        })?;
        bus.wait_for_name(GNOME_SHELL_NAME, GNOME_WAIT_TIMEOUT)
            .map_err(|err| {
                GnomeSourceError::Failed(format!(
                    "failed waiting for GNOME Shell on the session bus: {err}"
                ))
            })?;

        let status = gnome_service_status_from_session_bus(&mut bus);
        if !status.can_start() {
            return Err(GnomeSourceError::Unavailable(
                GNOME_REQUIRED_SERVICES_REASON,
            ));
        }

        if honor_idle_inhibitors && !gnome_session_manager_available_from_session_bus(&mut bus) {
            return Err(GnomeSourceError::Unavailable(
                GNOME_IDLE_INHIBITORS_REQUIRED_REASON,
            ));
        }

        subscribe_to_gnome_signals(&mut bus, honor_idle_inhibitors)?;
        let activity_watch = honor_idle_inhibitors
            .then(|| GnomeActivityWatch::connect(&mut bus))
            .transpose()?;
        let owner = resolve_screen_saver_owner(&mut bus).map_err(|err| {
            GnomeSourceError::Failed(format!("failed to resolve GNOME ScreenSaver owner: {err}"))
        })?;
        let (trusted_session_manager_signals, initial_idle_blanking_allowed) =
            if honor_idle_inhibitors {
                let owner = resolve_session_manager_owner(&mut bus).map_err(|err| {
                    GnomeSourceError::Failed(format!(
                        "failed to resolve GNOME SessionManager owner: {err}"
                    ))
                })?;
                let allowed = current_idle_blanking_allowed(&mut bus).map_err(|err| {
                    GnomeSourceError::Failed(format!(
                        "failed to read GNOME idle inhibitor state: {err}"
                    ))
                })?;
                (
                    Some(TrustedSessionManagerSignals::new(Some(owner))),
                    Some(allowed),
                )
            } else {
                (None, None)
            };

        Ok(Self {
            bus,
            trusted_screen_saver_signals: TrustedScreenSaverSignals::new(Some(owner)),
            trusted_session_manager_signals,
            initial_idle_blanking_allowed,
            activity_watch,
        })
    }

    pub(crate) fn run<F>(mut self, mut publish: F) -> Result<(), GnomeSourceError>
    where
        F: FnMut(SessionObservation) -> bool,
    {
        if let Some(allowed) = self.initial_idle_blanking_allowed {
            if !publish(SessionObservation::IdleBlankingPermission {
                allowed,
                source: EventSource::DesktopSession,
                observed_at: Instant::now(),
            }) {
                return Ok(());
            }
        }

        run_gnome_monitor_process(
            &mut self.bus,
            &mut self.trusted_screen_saver_signals,
            self.trusted_session_manager_signals.as_mut(),
            self.activity_watch.as_mut(),
            &mut publish,
        )
    }
}

pub fn map_screen_saver_signal(signal: &BusSignal) -> Option<SessionEvent> {
    if signal.path != GNOME_SCREEN_SAVER_PATH || signal.interface != GNOME_SCREEN_SAVER_INTERFACE {
        return None;
    }

    match (signal.member.as_str(), signal.body.as_slice()) {
        ("ActiveChanged", [BusValue::Bool(true)]) => Some(SessionEvent::Idle),
        ("ActiveChanged", [BusValue::Bool(false)]) => Some(SessionEvent::Active),
        ("WakeUpScreen", []) => Some(SessionEvent::WakeRequested),
        _ => None,
    }
}

pub fn resolve_screen_saver_owner(
    bus: &mut impl SessionBusClient,
) -> Result<String, SessionBusError> {
    get_name_owner(bus, GNOME_SCREEN_SAVER_NAME)
}

pub fn resolve_session_manager_owner(
    bus: &mut impl SessionBusClient,
) -> Result<String, SessionBusError> {
    get_name_owner(bus, GNOME_SESSION_MANAGER_NAME)
}

pub fn screen_saver_owner_changed(signal: &BusSignal) -> Option<Option<String>> {
    let owner_change = parse_name_owner_changed_signal(signal)?;
    if owner_change.name != GNOME_SCREEN_SAVER_NAME {
        return None;
    }

    Some(owner_change.new_owner)
}

pub fn session_manager_owner_changed(signal: &BusSignal) -> Option<Option<String>> {
    let owner_change = parse_name_owner_changed_signal(signal)?;
    if owner_change.name != GNOME_SESSION_MANAGER_NAME {
        return None;
    }

    Some(owner_change.new_owner)
}

pub fn current_idle_monitor_idletime_ms(
    bus: &mut impl SessionBusClient,
) -> Result<u64, SessionBusError> {
    bus.call_method(BusMethodCall::new(
        GNOME_IDLE_MONITOR_NAME,
        GNOME_IDLE_MONITOR_PATH,
        GNOME_IDLE_MONITOR_INTERFACE,
        "GetIdletime",
    ))?
    .single_u64()
}

pub fn current_idle_blanking_allowed(
    bus: &mut impl SessionBusClient,
) -> Result<bool, SessionBusError> {
    let inhibited = bus
        .call_method(
            BusMethodCall::new(
                GNOME_SESSION_MANAGER_NAME,
                GNOME_SESSION_MANAGER_PATH,
                GNOME_SESSION_MANAGER_INTERFACE,
                "IsInhibited",
            )
            .with_body(vec![BusValue::U32(GNOME_IDLE_INHIBITION_FLAG)]),
        )?
        .single_bool()?;

    Ok(!inhibited)
}

fn gnome_service_status_from_session_bus(bus: &mut impl SessionBusClient) -> GnomeServiceStatus {
    GnomeServiceStatus {
        shell_available: bus.name_has_owner(GNOME_SHELL_NAME).unwrap_or(false),
        screen_saver_available: bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false),
        idle_monitor_available: bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false),
    }
}

fn subscribe_to_gnome_signals(
    bus: &mut impl SessionBusClient,
    honor_idle_inhibitors: bool,
) -> Result<(), GnomeSourceError> {
    bus.add_signal_match(BusSignalMatch {
        sender: None,
        path: Some(GNOME_SCREEN_SAVER_PATH),
        interface: Some(GNOME_SCREEN_SAVER_INTERFACE),
        member: None,
    })
    .map_err(|err| {
        GnomeSourceError::Failed(format!(
            "failed to subscribe to GNOME ScreenSaver signals: {err}"
        ))
    })?;
    bus.add_signal_match(BusSignalMatch {
        sender: Some(DBUS_SERVICE_NAME),
        path: Some(DBUS_OBJECT_PATH),
        interface: Some(DBUS_INTERFACE),
        member: Some("NameOwnerChanged"),
    })
    .map_err(|err| {
        GnomeSourceError::Failed(format!("failed to subscribe to D-Bus owner changes: {err}"))
    })?;

    if honor_idle_inhibitors {
        bus.add_signal_match(BusSignalMatch {
            sender: None,
            path: Some(GNOME_IDLE_MONITOR_PATH),
            interface: Some(GNOME_IDLE_MONITOR_INTERFACE),
            member: Some("WatchFired"),
        })
        .map_err(|err| {
            GnomeSourceError::Failed(format!("failed to subscribe to Mutter activity: {err}"))
        })?;
        bus.add_signal_match(BusSignalMatch {
            sender: None,
            path: Some(GNOME_SESSION_MANAGER_PATH),
            interface: Some(GNOME_SESSION_MANAGER_INTERFACE),
            member: None,
        })
        .map_err(|err| {
            GnomeSourceError::Failed(format!(
                "failed to subscribe to GNOME SessionManager signals: {err}"
            ))
        })?;
    }

    Ok(())
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionManagerSignal {
    OwnerChanged(Option<String>),
    InhibitorChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustedSessionManagerSignals {
    owner: Option<String>,
}

impl TrustedSessionManagerSignals {
    fn new(owner: Option<String>) -> Self {
        Self { owner }
    }

    fn observe(&mut self, signal: &BusSignal) -> Option<SessionManagerSignal> {
        if signal.path == DBUS_OBJECT_PATH
            && signal.interface == DBUS_INTERFACE
            && signal.member == "NameOwnerChanged"
        {
            if signal.sender.as_deref() != Some(DBUS_SERVICE_NAME) {
                return None;
            }

            if let Some(new_owner) = session_manager_owner_changed(signal) {
                self.owner = new_owner.clone();
                return Some(SessionManagerSignal::OwnerChanged(new_owner));
            }
            return None;
        }

        if signal.sender.as_deref() != self.owner.as_deref() {
            return None;
        }

        map_session_manager_signal(signal).map(|_| SessionManagerSignal::InhibitorChanged)
    }
}

fn map_session_manager_signal(signal: &BusSignal) -> Option<()> {
    if signal.path != GNOME_SESSION_MANAGER_PATH
        || signal.interface != GNOME_SESSION_MANAGER_INTERFACE
    {
        return None;
    }

    match (signal.member.as_str(), signal.body.as_slice()) {
        ("InhibitorAdded" | "InhibitorRemoved", [BusValue::ObjectPath(_)]) => Some(()),
        _ => None,
    }
}

fn gnome_session_manager_available_from_session_bus(bus: &mut impl SessionBusClient) -> bool {
    bus.name_has_owner(GNOME_SESSION_MANAGER_NAME)
        .unwrap_or(false)
}

/// Mutter resets GetIdletime when inhibition ends, without firing user-active
/// watches. Use those watches to distinguish input from the counter reset.
struct GnomeActivityWatch {
    owner: String,
    id: u32,
}

impl GnomeActivityWatch {
    fn connect(bus: &mut impl SessionBusClient) -> Result<Self, GnomeSourceError> {
        let owner = get_name_owner(bus, GNOME_IDLE_MONITOR_NAME).map_err(|err| {
            GnomeSourceError::Failed(format!("failed to resolve Mutter IdleMonitor owner: {err}"))
        })?;
        let mut watch = Self { owner, id: 0 };
        watch.arm(bus)?;
        Ok(watch)
    }

    fn arm(&mut self, bus: &mut impl SessionBusClient) -> Result<(), GnomeSourceError> {
        self.id = bus
            .call_method(BusMethodCall::new(
                &self.owner,
                GNOME_IDLE_MONITOR_PATH,
                GNOME_IDLE_MONITOR_INTERFACE,
                "AddUserActiveWatch",
            ))
            .and_then(|reply| reply.single_u32())
            .map_err(|err| {
                GnomeSourceError::Failed(format!("failed to watch Mutter user activity: {err}"))
            })?;
        Ok(())
    }

    fn fired(&self, signal: &BusSignal) -> bool {
        signal.sender.as_deref() == Some(self.owner.as_str())
            && signal.path == GNOME_IDLE_MONITOR_PATH
            && signal.interface == GNOME_IDLE_MONITOR_INTERFACE
            && signal.member == "WatchFired"
            && signal.body == [BusValue::U32(self.id)]
    }

    fn owner_changed(&self, signal: &BusSignal) -> bool {
        signal.sender.as_deref() == Some(DBUS_SERVICE_NAME)
            && parse_name_owner_changed_signal(signal).is_some_and(|change| {
                change.name == GNOME_IDLE_MONITOR_NAME
                    && change.new_owner.as_deref() != Some(self.owner.as_str())
            })
    }
}

fn run_gnome_monitor_process<F>(
    bus: &mut impl SessionBusClient,
    trusted_screen_saver_signals: &mut TrustedScreenSaverSignals,
    mut trusted_session_manager_signals: Option<&mut TrustedSessionManagerSignals>,
    mut activity_watch: Option<&mut GnomeActivityWatch>,
    publish: &mut F,
) -> Result<(), GnomeSourceError>
where
    F: FnMut(SessionObservation) -> bool,
{
    let started = Instant::now();
    let test_timeout = monitor_test_timeout();
    let mut next_idle_poll = Instant::now();

    loop {
        if test_timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            return Ok(());
        }

        let now = Instant::now();
        if activity_watch.is_none() && now >= next_idle_poll {
            if !poll_idle_monitor_once(bus, publish) {
                return Ok(());
            }
            next_idle_poll = now + GNOME_IDLE_POLL_INTERVAL;
        }

        let now = Instant::now();
        let mut process_timeout = if activity_watch.is_some() {
            GNOME_BUS_PROCESS_INTERVAL
        } else {
            next_idle_poll
                .saturating_duration_since(now)
                .min(GNOME_BUS_PROCESS_INTERVAL)
        };
        if let Some(timeout) = test_timeout {
            process_timeout = process_timeout.min(timeout.saturating_sub(started.elapsed()));
        }

        let Some(signal) = bus.process(process_timeout).map_err(|err| {
            GnomeSourceError::Failed(format!("GNOME session bus processing failed: {err}"))
        })?
        else {
            continue;
        };

        if let Some(watch) = activity_watch.as_deref_mut() {
            if watch.owner_changed(&signal) {
                return Err(GnomeSourceError::Failed(
                    "Mutter IdleMonitor owner changed while watching user activity".to_string(),
                ));
            }
            if watch.fired(&signal) {
                if !publish(SessionObservation::Inactivity {
                    observation: InactivityObservation::DesktopActivityObserved,
                    source: EventSource::DesktopSession,
                    observed_at: Instant::now(),
                }) {
                    return Ok(());
                }
                // User-active watches are one-shot. Keep listening even while
                // inhibition is active, so real input can always restore.
                watch.arm(bus)?;
            }
        }

        if let Some(trusted_session_manager_signals) =
            trusted_session_manager_signals.as_deref_mut()
        {
            match trusted_session_manager_signals.observe(&signal) {
                Some(SessionManagerSignal::OwnerChanged(None)) => {
                    return Err(GnomeSourceError::Failed(
                        "GNOME SessionManager disappeared while idle inhibitor honoring was enabled"
                            .to_string(),
                    ));
                }
                Some(SessionManagerSignal::OwnerChanged(Some(_)))
                | Some(SessionManagerSignal::InhibitorChanged) => {
                    // The runner's timer is independent of this blocking call.
                    // Suspend it before querying, without inventing an inhibitor.
                    if !publish(SessionObservation::IdleBlankingPermissionPending {
                        source: EventSource::DesktopSession,
                    }) {
                        return Ok(());
                    }
                    let allowed = current_idle_blanking_allowed(bus).map_err(|err| {
                        GnomeSourceError::Failed(format!(
                            "failed to read GNOME idle inhibitor state after a SessionManager change: {err}"
                        ))
                    })?;
                    if !publish(SessionObservation::IdleBlankingPermission {
                        allowed,
                        source: EventSource::DesktopSession,
                        observed_at: Instant::now(),
                    }) {
                        return Ok(());
                    }
                }
                None => {}
            }
        }

        let Some(event) = trusted_screen_saver_signals.observe(&signal) else {
            continue;
        };

        if !publish(SessionObservation::Event {
            event,
            source: EventSource::DesktopSession,
            observed_at: Instant::now(),
        }) {
            return Ok(());
        }
    }
}

fn poll_idle_monitor_once<F>(bus: &mut impl SessionBusClient, publish: &mut F) -> bool
where
    F: FnMut(SessionObservation) -> bool,
{
    let Ok(idletime_ms) = current_idle_monitor_idletime_ms(bus) else {
        return true;
    };

    let activity_age = Duration::from_millis(idletime_ms);
    if activity_age > GNOME_DESKTOP_ACTIVITY_WINDOW {
        return true;
    }

    let observed_at = Instant::now();
    let activity_at = observed_at.checked_sub(activity_age).unwrap_or(observed_at);

    publish(SessionObservation::Inactivity {
        observation: InactivityObservation::DesktopActivityObserved,
        source: EventSource::DesktopSession,
        observed_at: activity_at,
    })
}

pub(crate) fn monitor_test_timeout() -> Option<Duration> {
    std::env::var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .and_then(|value| Duration::try_from_secs_f64(value).ok())
}

#[cfg(test)]
mod tests {
    use super::{
        current_idle_blanking_allowed, current_idle_monitor_idletime_ms,
        gnome_service_status_from_session_bus, map_screen_saver_signal, map_session_manager_signal,
        monitor_test_timeout, poll_idle_monitor_once, resolve_screen_saver_owner,
        run_gnome_monitor_process, screen_saver_owner_changed, session_manager_owner_changed,
        subscribe_to_gnome_signals, GnomeActivityWatch, GnomeServiceStatus, GnomeSourceError,
        SessionManagerSignal, TrustedScreenSaverSignals, TrustedSessionManagerSignals,
        GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV,
    };
    use crate::events::EventSource;
    use crate::session::inactivity::InactivityObservation;
    use crate::session::{SessionEvent, SessionObservation};
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use std::collections::VecDeque;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    };
    use std::time::{Duration, Instant};

    #[derive(Debug, Default)]
    struct FakeSessionBus {
        shell_available: bool,
        screen_saver_available: bool,
        idle_monitor_available: bool,
        idletime_ms: Option<u64>,
        screen_saver_owner: Option<String>,
        session_manager_owner: Option<String>,
        idle_monitor_owner: Option<String>,
        watch_ids: VecDeque<u32>,
        inhibited: Option<bool>,
        inhibited_plan: VecDeque<bool>,
        permission_pending: Option<Arc<AtomicBool>>,
        method_calls: Vec<(String, String, String, String)>,
        method_bodies: Vec<Vec<BusValue>>,
        signal_matches: Vec<[Option<String>; 4]>,
        process_results: VecDeque<Result<Option<BusSignal>, SessionBusError>>,
        failed_names: Vec<String>,
    }

    impl SessionBusClient for FakeSessionBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            if self.failed_names.iter().any(|failed| failed == name) {
                return Err(SessionBusError::Transport(
                    "simulated bus failure".to_string(),
                ));
            }

            match name {
                super::GNOME_SHELL_NAME => Ok(self.shell_available),
                super::GNOME_SCREEN_SAVER_NAME => Ok(self.screen_saver_available),
                super::GNOME_IDLE_MONITOR_NAME => Ok(self.idle_monitor_available),
                super::GNOME_SESSION_MANAGER_NAME => Ok(self.session_manager_owner.is_some()),
                _ => Ok(false),
            }
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.method_calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            self.method_bodies.push(call.body.clone());
            if call.destination == DBUS_SERVICE_NAME
                && call.path == DBUS_OBJECT_PATH
                && call.interface == DBUS_INTERFACE
                && call.member == "GetNameOwner"
            {
                let [BusValue::String(name)] = call.body.as_slice() else {
                    return Err(SessionBusError::Transport(
                        "missing GetNameOwner test name".to_string(),
                    ));
                };
                let owner = match name.as_str() {
                    super::GNOME_SCREEN_SAVER_NAME => self.screen_saver_owner.as_deref(),
                    super::GNOME_SESSION_MANAGER_NAME => self.session_manager_owner.as_deref(),
                    super::GNOME_IDLE_MONITOR_NAME => self.idle_monitor_owner.as_deref(),
                    _ => None,
                };
                return owner
                    .map(|value| {
                        BusReply::new(vec![crate::session_bus::BusValue::String(
                            value.to_string(),
                        )])
                    })
                    .ok_or_else(|| {
                        SessionBusError::Transport("no queued GNOME owner reply".to_string())
                    });
            }

            if call.path == super::GNOME_IDLE_MONITOR_PATH
                && call.interface == super::GNOME_IDLE_MONITOR_INTERFACE
                && call.member == "AddUserActiveWatch"
                && Some(call.destination) == self.idle_monitor_owner.as_deref()
            {
                assert!(call.body.is_empty());
                return self
                    .watch_ids
                    .pop_front()
                    .map(|id| BusReply::new(vec![BusValue::U32(id)]))
                    .ok_or_else(|| SessionBusError::Transport("no queued watch id".to_string()));
            }
            if call.member == "IsInhibited" {
                if let Some(pending) = &self.permission_pending {
                    assert!(
                        pending.load(Ordering::SeqCst),
                        "permission must be suspended before querying"
                    );
                }
            }

            match (
                call.destination,
                call.path,
                call.interface,
                call.member,
                self.idletime_ms,
            ) {
                (
                    super::GNOME_IDLE_MONITOR_NAME,
                    super::GNOME_IDLE_MONITOR_PATH,
                    super::GNOME_IDLE_MONITOR_INTERFACE,
                    "GetIdletime",
                    Some(value),
                ) => Ok(BusReply::new(vec![BusValue::U64(value)])),
                (
                    super::GNOME_SESSION_MANAGER_NAME,
                    super::GNOME_SESSION_MANAGER_PATH,
                    super::GNOME_SESSION_MANAGER_INTERFACE,
                    "IsInhibited",
                    _,
                ) => self
                    .inhibited_plan
                    .pop_front()
                    .or(self.inhibited)
                    .map(|value| BusReply::new(vec![BusValue::Bool(value)]))
                    .ok_or_else(|| {
                        SessionBusError::Transport("no queued inhibition reply".to_string())
                    }),
                _ => Err(SessionBusError::Transport(
                    "no queued GNOME method reply".to_string(),
                )),
            }
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.signal_matches.push([
                rule.sender.map(str::to_owned),
                rule.path.map(str::to_owned),
                rule.interface.map(str::to_owned),
                rule.member.map(str::to_owned),
            ]);
            Ok(())
        }

        fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            let _ = timeout;
            self.process_results.pop_front().unwrap_or(Ok(None))
        }
    }

    #[test]
    fn active_changed_true_signal_maps_to_idle_event() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_body(vec![BusValue::Bool(true)]);

        assert_eq!(map_screen_saver_signal(&signal), Some(SessionEvent::Idle));
    }

    #[test]
    fn active_changed_false_signal_maps_to_active_event() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_body(vec![BusValue::Bool(false)]);

        assert_eq!(map_screen_saver_signal(&signal), Some(SessionEvent::Active));
    }

    #[test]
    fn wakeup_signal_maps_to_wake_requested_event_via_bus_signal() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        );

        assert_eq!(
            map_screen_saver_signal(&signal),
            Some(SessionEvent::WakeRequested)
        );
    }

    #[test]
    fn resolve_screen_saver_owner_uses_generic_name_owner_lookup() {
        let mut bus = FakeSessionBus {
            screen_saver_owner: Some(":1.42".to_string()),
            ..FakeSessionBus::default()
        };

        assert_eq!(
            resolve_screen_saver_owner(&mut bus),
            Ok(":1.42".to_string())
        );
        assert_eq!(
            bus.method_calls,
            vec![(
                DBUS_SERVICE_NAME.to_string(),
                DBUS_OBJECT_PATH.to_string(),
                DBUS_INTERFACE.to_string(),
                "GetNameOwner".to_string(),
            )]
        );
    }

    #[test]
    fn screen_saver_owner_changed_returns_new_owner_for_gnome_service() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.41".to_string()),
                BusValue::String(":1.42".to_string()),
            ]);

        assert_eq!(
            screen_saver_owner_changed(&signal),
            Some(Some(":1.42".to_string()))
        );
    }

    #[test]
    fn screen_saver_owner_changed_ignores_other_services() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String("org.example.Other".to_string()),
                BusValue::String(":1.41".to_string()),
                BusValue::String(":1.42".to_string()),
            ]);

        assert_eq!(screen_saver_owner_changed(&signal), None);
    }

    #[test]
    fn session_manager_is_inhibited_flag_maps_to_blanking_permission() {
        let mut bus = FakeSessionBus {
            inhibited: Some(true),
            ..FakeSessionBus::default()
        };

        assert_eq!(current_idle_blanking_allowed(&mut bus), Ok(false));
        assert_eq!(
            bus.method_bodies,
            vec![vec![BusValue::U32(super::GNOME_IDLE_INHIBITION_FLAG)]]
        );
    }

    #[test]
    fn overlapping_inhibitors_keep_blanking_suppressed_until_last_release() {
        let mut bus = FakeSessionBus {
            inhibited_plan: [true, true, false].into_iter().collect(),
            ..FakeSessionBus::default()
        };

        assert_eq!(current_idle_blanking_allowed(&mut bus), Ok(false));
        assert_eq!(current_idle_blanking_allowed(&mut bus), Ok(false));
        assert_eq!(current_idle_blanking_allowed(&mut bus), Ok(true));
    }

    #[test]
    fn session_manager_signals_require_current_owner_and_valid_inhibitor_path() {
        let mut trusted = TrustedSessionManagerSignals::new(Some(":1.42".to_string()));
        let added = BusSignal::new(
            super::GNOME_SESSION_MANAGER_PATH,
            super::GNOME_SESSION_MANAGER_INTERFACE,
            "InhibitorAdded",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::ObjectPath(
            "/org/gnome/SessionManager/Inhibitor1".to_string(),
        )]);
        let spoofed = added.clone().with_sender(":1.99");
        let malformed = added.clone().with_body(vec![BusValue::String(
            "/org/gnome/SessionManager/Inhibitor1".to_string(),
        )]);

        assert_eq!(map_session_manager_signal(&added), Some(()));
        assert_eq!(
            trusted.observe(&added),
            Some(SessionManagerSignal::InhibitorChanged)
        );
        assert_eq!(trusted.observe(&spoofed), None);
        assert_eq!(trusted.observe(&malformed), None);
    }

    #[test]
    fn session_manager_owner_changes_rebind_and_reject_old_sender() {
        let mut trusted = TrustedSessionManagerSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_SESSION_MANAGER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(
            session_manager_owner_changed(&owner_change),
            Some(Some(":1.43".to_string()))
        );
        assert_eq!(
            trusted.observe(&owner_change),
            Some(SessionManagerSignal::OwnerChanged(Some(
                ":1.43".to_string()
            )))
        );

        let signal = BusSignal::new(
            super::GNOME_SESSION_MANAGER_PATH,
            super::GNOME_SESSION_MANAGER_INTERFACE,
            "InhibitorRemoved",
        )
        .with_body(vec![BusValue::ObjectPath(
            "/org/gnome/SessionManager/Inhibitor1".to_string(),
        )]);
        assert_eq!(trusted.observe(&signal.clone().with_sender(":1.42")), None);
        assert_eq!(
            trusted.observe(&signal.with_sender(":1.43")),
            Some(SessionManagerSignal::InhibitorChanged)
        );

        let owner_loss = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_SESSION_MANAGER_NAME.to_string()),
                BusValue::String(":1.43".to_string()),
                BusValue::String(String::new()),
            ]);
        assert_eq!(
            trusted.observe(&owner_loss),
            Some(SessionManagerSignal::OwnerChanged(None))
        );
    }

    #[test]
    fn idle_inhibitor_read_failures_are_errors_without_allowed_true() {
        let mut bus = FakeSessionBus::default();
        assert!(current_idle_blanking_allowed(&mut bus).is_err());
    }

    #[test]
    fn disabled_idle_inhibitor_honoring_keeps_session_manager_out_of_signal_matches() {
        let mut bus = FakeSessionBus::default();
        subscribe_to_gnome_signals(&mut bus, false).expect("subscribe GNOME signals");

        assert_eq!(bus.signal_matches.len(), 2);
        assert!(!bus
            .signal_matches
            .iter()
            .any(|[_, path, _, _]| { path.as_deref() == Some(super::GNOME_SESSION_MANAGER_PATH) }));
    }

    #[test]
    fn enabled_idle_inhibitor_honoring_subscribes_to_session_manager_signals() {
        let mut bus = FakeSessionBus::default();
        subscribe_to_gnome_signals(&mut bus, true).expect("subscribe GNOME signals");

        assert_eq!(bus.signal_matches.len(), 4);
        assert!(bus.signal_matches.iter().any(|[_, path, interface, _]| {
            path.as_deref() == Some(super::GNOME_SESSION_MANAGER_PATH)
                && interface.as_deref() == Some(super::GNOME_SESSION_MANAGER_INTERFACE)
        }));
    }

    #[test]
    fn inhibitor_transition_publishes_permission_without_activity_or_restore() {
        let pending = Arc::new(AtomicBool::new(false));
        let signal = BusSignal::new(
            super::GNOME_SESSION_MANAGER_PATH,
            super::GNOME_SESSION_MANAGER_INTERFACE,
            "InhibitorAdded",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::ObjectPath(
            "/org/gnome/SessionManager/Inhibitor1".to_string(),
        )]);
        let mut bus = FakeSessionBus {
            permission_pending: Some(Arc::clone(&pending)),
            inhibited_plan: [true].into_iter().collect(),
            process_results: [
                Ok(Some(signal)),
                Err(SessionBusError::Transport("stop test loop".to_string())),
            ]
            .into_iter()
            .collect(),
            ..FakeSessionBus::default()
        };
        let mut screen_saver = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let mut session_manager = TrustedSessionManagerSignals::new(Some(":1.42".to_string()));
        let mut observations = Vec::new();

        assert!(matches!(
            run_gnome_monitor_process(
                &mut bus,
                &mut screen_saver,
                Some(&mut session_manager),
                None,
                &mut |observation| {
                    if matches!(observation, SessionObservation::IdleBlankingPermissionPending { .. }) {
                        pending.store(true, Ordering::SeqCst);
                    }
                    observations.push(observation);
                    true
                },
            ),
            Err(GnomeSourceError::Failed(message)) if message.contains("processing failed")
        ));
        assert!(matches!(
            observations.as_slice(),
            [
                SessionObservation::IdleBlankingPermissionPending {
                    source: EventSource::DesktopSession
                },
                SessionObservation::IdleBlankingPermission {
                    allowed: false,
                    source: EventSource::DesktopSession,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn status_from_session_bus_uses_required_gnome_service_names() {
        let mut bus = FakeSessionBus {
            shell_available: true,
            screen_saver_available: true,
            idle_monitor_available: false,
            ..FakeSessionBus::default()
        };

        assert_eq!(
            gnome_service_status_from_session_bus(&mut bus),
            GnomeServiceStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: false,
            }
        );
    }

    #[test]
    fn status_from_session_bus_treats_bus_errors_as_unavailable() {
        let mut bus = FakeSessionBus {
            shell_available: true,
            screen_saver_available: true,
            idle_monitor_available: true,
            failed_names: vec![super::GNOME_IDLE_MONITOR_NAME.to_string()],
            ..FakeSessionBus::default()
        };

        assert_eq!(
            gnome_service_status_from_session_bus(&mut bus),
            GnomeServiceStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: false,
            }
        );
    }

    #[test]
    fn current_idle_monitor_idletime_uses_gnome_idle_monitor_endpoint() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(1_500),
            ..FakeSessionBus::default()
        };

        assert_eq!(current_idle_monitor_idletime_ms(&mut bus), Ok(1_500));
        assert_eq!(
            bus.method_calls,
            vec![(
                super::GNOME_IDLE_MONITOR_NAME.to_string(),
                super::GNOME_IDLE_MONITOR_PATH.to_string(),
                super::GNOME_IDLE_MONITOR_INTERFACE.to_string(),
                "GetIdletime".to_string(),
            )]
        );
    }

    fn watch_fired(id: u32) -> BusSignal {
        BusSignal::new(
            super::GNOME_IDLE_MONITOR_PATH,
            super::GNOME_IDLE_MONITOR_INTERFACE,
            "WatchFired",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::U32(id)])
    }

    #[test]
    fn activity_watch_requires_current_owner_id_and_signal_shape() {
        let watch = GnomeActivityWatch {
            owner: ":1.42".to_string(),
            id: 7,
        };
        assert!(watch.fired(&watch_fired(7)));
        assert!(!watch.fired(&watch_fired(8)));
        assert!(!watch.fired(&watch_fired(7).with_sender(":1.99")));
        assert!(!watch.fired(&watch_fired(7).with_body(vec![BusValue::U64(7)])));
        let mut wrong_path = watch_fired(7);
        wrong_path.path = "/untrusted".to_string();
        assert!(!watch.fired(&wrong_path));
    }

    #[test]
    fn activity_watch_rearms_after_input_without_polling_the_resettable_counter() {
        let mut bus = FakeSessionBus {
            idle_monitor_owner: Some(":1.42".to_string()),
            watch_ids: [7, 8, 9].into_iter().collect(),
            idletime_ms: Some(0),
            process_results: [
                Ok(Some(watch_fired(7).with_sender(":1.99"))),
                Ok(Some(watch_fired(7))),
                Ok(Some(watch_fired(7))), // One-shot watch has already expired.
                Ok(Some(watch_fired(8))),
                Err(SessionBusError::Transport("stop test loop".to_string())),
            ]
            .into_iter()
            .collect(),
            ..FakeSessionBus::default()
        };
        let mut watch = GnomeActivityWatch::connect(&mut bus).expect("register activity watch");
        let mut screen_saver = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let mut observations = Vec::new();
        assert!(run_gnome_monitor_process(
            &mut bus,
            &mut screen_saver,
            None,
            Some(&mut watch),
            &mut |observation| {
                observations.push(observation);
                true
            },
        )
        .is_err());
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().all(|observation| matches!(
            observation,
            SessionObservation::Inactivity {
                observation: InactivityObservation::DesktopActivityObserved,
                source: EventSource::DesktopSession,
                ..
            }
        )));
        assert_eq!(watch.id, 9);
        assert!(!bus
            .method_calls
            .iter()
            .any(|(_, _, _, member)| member == "GetIdletime"));
    }

    #[test]
    fn activity_watch_owner_loss_is_an_error_instead_of_silent_input_loss() {
        let change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_IDLE_MONITOR_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(String::new()),
            ]);
        let mut watch = GnomeActivityWatch {
            owner: ":1.42".to_string(),
            id: 7,
        };
        assert!(!watch.owner_changed(&change.clone().with_sender(":1.99")));
        let mut bus = FakeSessionBus {
            process_results: [Ok(Some(change))].into_iter().collect(),
            ..FakeSessionBus::default()
        };
        let mut screen_saver = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let result = run_gnome_monitor_process(
            &mut bus,
            &mut screen_saver,
            None,
            Some(&mut watch),
            &mut |_| panic!("owner loss is not activity"),
        );
        assert!(
            matches!(result, Err(GnomeSourceError::Failed(message)) if message.contains("owner changed"))
        );
    }

    #[test]
    fn activity_watch_rearm_failure_is_an_error_instead_of_polling_for_input() {
        let mut bus = FakeSessionBus {
            process_results: [Ok(Some(watch_fired(7)))].into_iter().collect(),
            ..FakeSessionBus::default()
        };
        let mut watch = GnomeActivityWatch {
            owner: ":1.42".to_string(),
            id: 7,
        };
        let mut screen_saver = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let mut observations = Vec::new();
        let result = run_gnome_monitor_process(
            &mut bus,
            &mut screen_saver,
            None,
            Some(&mut watch),
            &mut |observation| {
                observations.push(observation);
                true
            },
        );
        assert!(
            matches!(result, Err(GnomeSourceError::Failed(message)) if message.contains("failed to watch"))
        );
        assert_eq!(observations.len(), 1);
    }

    #[test]
    fn idle_monitor_poller_publishes_recent_desktop_activity() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(250),
            ..FakeSessionBus::default()
        };
        let before_poll = Instant::now();
        let mut observations = Vec::new();

        assert!(poll_idle_monitor_once(&mut bus, &mut |observation| {
            observations.push(observation);
            true
        }));

        let [SessionObservation::Inactivity {
            observation,
            source,
            observed_at,
        }] = observations.as_slice()
        else {
            panic!("expected one inactivity observation");
        };
        assert_eq!(*observation, InactivityObservation::DesktopActivityObserved);
        assert_eq!(*source, EventSource::DesktopSession);
        assert!(*observed_at < before_poll);
        assert!(*observed_at <= Instant::now());
    }

    #[test]
    fn idle_monitor_poller_does_not_publish_old_activity() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(1_500),
            ..FakeSessionBus::default()
        };
        let mut observations = Vec::new();

        assert!(poll_idle_monitor_once(&mut bus, &mut |observation| {
            observations.push(observation);
            true
        }));
        assert!(observations.is_empty());
    }

    #[test]
    fn idle_monitor_poller_ignores_bus_errors() {
        let mut bus = FakeSessionBus::default();
        let mut observations = Vec::new();

        assert!(poll_idle_monitor_once(&mut bus, &mut |observation| {
            observations.push(observation);
            true
        }));
        assert!(observations.is_empty());
    }

    #[test]
    fn trusted_screen_saver_signals_accept_only_the_current_owner() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let current = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_sender(":1.42")
        .with_body(vec![BusValue::Bool(true)]);
        let spoofed = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        )
        .with_sender(":1.99");

        assert_eq!(trusted.observe(&current), Some(SessionEvent::Idle));
        assert_eq!(trusted.observe(&spoofed), None);
    }

    #[test]
    fn trusted_screen_saver_signals_follow_owner_changes() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let owner_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&owner_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
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
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.43")
                .with_body(vec![BusValue::Bool(true)])
            ),
            Some(SessionEvent::Idle)
        );
    }

    #[test]
    fn trusted_screen_saver_signals_ignore_untrusted_owner_changes_and_owner_loss() {
        let mut trusted = TrustedScreenSaverSignals::new(Some(":1.42".to_string()));
        let untrusted_change = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(":1.99")
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(":1.43".to_string()),
            ]);

        assert_eq!(trusted.observe(&untrusted_change), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
                    "WakeUpScreen",
                )
                .with_sender(":1.42")
            ),
            Some(SessionEvent::WakeRequested)
        );

        let owner_loss = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(super::GNOME_SCREEN_SAVER_NAME.to_string()),
                BusValue::String(":1.42".to_string()),
                BusValue::String(String::new()),
            ]);
        assert_eq!(trusted.observe(&owner_loss), None);
        assert_eq!(
            trusted.observe(
                &BusSignal::new(
                    super::GNOME_SCREEN_SAVER_PATH,
                    super::GNOME_SCREEN_SAVER_INTERFACE,
                    "ActiveChanged",
                )
                .with_sender(":1.42")
                .with_body(vec![BusValue::Bool(false)])
            ),
            None
        );
    }

    #[test]
    fn invalid_monitor_timeout_env_values_are_ignored() {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        std::env::set_var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, "0.5");
        assert_eq!(monitor_test_timeout(), Some(Duration::from_millis(500)));

        for invalid in ["NaN", "inf", "0", "-1"] {
            std::env::set_var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV, invalid);
            assert_eq!(monitor_test_timeout(), None);
        }

        std::env::remove_var(GNOME_MONITOR_TEST_TIMEOUT_SECS_ENV);
    }
}
