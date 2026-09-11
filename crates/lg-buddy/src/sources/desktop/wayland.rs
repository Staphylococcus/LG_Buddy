use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_callback, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

use super::{wait_for_retry, ActivityAdapter, ActivityPublisher, ActivityStatus};
use crate::events::EventSource;
use crate::session::inactivity::InactivityObservation;
use crate::session::SessionObservation;

const REQUIRED_IDLE_NOTIFIER_VERSION: u32 = 2;

pub(crate) struct WaylandSource {
    initial_connection: Mutex<Option<Connection>>,
    status: Mutex<ActivityStatus>,
}

impl Default for WaylandSource {
    fn default() -> Self {
        Self::new(None)
    }
}

impl WaylandSource {
    fn new(connection: Option<Connection>) -> Self {
        Self {
            initial_connection: Mutex::new(connection),
            status: Mutex::default(),
        }
    }

    /// Probe before application threads start, retaining any inherited socket
    /// inside this adapter for monitoring rather than opening it a second time.
    pub(crate) fn probe_capabilities(
        &self,
    ) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
        let mut initial = self
            .initial_connection
            .lock()
            .expect("initial Wayland connection");
        let connection = match initial.as_ref() {
            Some(connection) => connection.clone(),
            None => connect_wayland()?,
        };
        let capabilities = probe_wayland_capabilities_on(connection.clone())?;
        *initial = Some(connection);
        Ok(capabilities)
    }

    fn run_connection(
        &self,
        connection: Connection,
        publish: ActivityPublisher,
        stop: &AtomicBool,
    ) -> Result<(), WaylandProviderError> {
        let (mut event_queue, mut state, _) = initialize_provider(
            connection,
            move |observation| {
                publish(observation);
                true
            },
            stop,
        )?;
        *self.status.lock().expect("Wayland activity status") = ActivityStatus::Available;
        while state.running && !stop.load(Ordering::SeqCst) {
            dispatch_once(&mut event_queue, &mut state)?;
            if let Some(err) = state.take_error() {
                return Err(err);
            }
        }
        Ok(())
    }
}

impl ActivityAdapter for WaylandSource {
    fn run(&self, publish: ActivityPublisher, stop: &AtomicBool) {
        let mut connection = self
            .initial_connection
            .lock()
            .expect("initial Wayland connection")
            .take();
        while !stop.load(Ordering::SeqCst) {
            let result = connection
                .take()
                .map(Ok)
                .unwrap_or_else(reconnect_wayland)
                .and_then(|connection| self.run_connection(connection, Arc::clone(&publish), stop));
            *self.status.lock().expect("Wayland activity status") =
                ActivityStatus::unavailable(result.err().map_or_else(
                    || "activity monitoring stopped".to_string(),
                    |err| err.to_string(),
                ));
            wait_for_retry(stop);
        }
    }

    fn status(&self) -> ActivityStatus {
        self.status.lock().expect("Wayland activity status").clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaylandProviderCapabilities {
    pub idle_notifier_version: u32,
    pub seat_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaylandProviderError {
    Connection(String),
    Dispatch(String),
    MissingIdleNotifier,
    UnsupportedIdleNotifierVersion(u32),
    NoSeats,
    IdleNotifierRemoved,
    LastSeatRemoved,
}

impl fmt::Display for WaylandProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(message) => {
                write!(f, "failed to connect to the Wayland compositor: {message}")
            }
            Self::Dispatch(message) => {
                write!(f, "Wayland event dispatch failed: {message}")
            }
            Self::MissingIdleNotifier => write!(
                f,
                "the compositor does not advertise ext_idle_notifier_v1; version 2 or newer is required"
            ),
            Self::UnsupportedIdleNotifierVersion(version) => write!(
                f,
                "the compositor advertises ext_idle_notifier_v1 version {version}; version 2 or newer is required"
            ),
            Self::NoSeats => write!(f, "the compositor does not advertise a Wayland seat"),
            Self::IdleNotifierRemoved => write!(
                f,
                "the compositor removed the ext_idle_notifier_v1 global"
            ),
            Self::LastSeatRemoved => {
                write!(f, "the compositor removed the last monitored Wayland seat")
            }
        }
    }
}

impl Error for WaylandProviderError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlobalInfo {
    name: u32,
    version: u32,
}

#[derive(Debug, Default)]
struct RegistryFacts {
    idle_notifiers: HashMap<u32, u32>,
    seats: HashMap<u32, u32>,
}

impl RegistryFacts {
    fn add(&mut self, name: u32, interface: &str, version: u32) {
        if interface == ext_idle_notifier_v1::ExtIdleNotifierV1::interface().name {
            self.idle_notifiers.insert(name, version);
        } else if interface == wl_seat::WlSeat::interface().name {
            self.seats.insert(name, version);
        }
    }

    fn remove(&mut self, name: u32) {
        self.idle_notifiers.remove(&name);
        self.seats.remove(&name);
    }

    fn selected_idle_notifier(&self) -> Option<GlobalInfo> {
        self.idle_notifiers
            .iter()
            .filter(|(_, version)| **version >= REQUIRED_IDLE_NOTIFIER_VERSION)
            .max_by_key(|(_, version)| **version)
            .map(|(name, version)| GlobalInfo {
                name: *name,
                version: *version,
            })
    }

    fn maximum_idle_notifier_version(&self) -> Option<u32> {
        self.idle_notifiers.values().copied().max()
    }

    fn capabilities(&self) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
        let Some(version) = self.maximum_idle_notifier_version() else {
            return Err(WaylandProviderError::MissingIdleNotifier);
        };
        if version < REQUIRED_IDLE_NOTIFIER_VERSION {
            return Err(WaylandProviderError::UnsupportedIdleNotifierVersion(
                version,
            ));
        }
        if self.seats.is_empty() {
            return Err(WaylandProviderError::NoSeats);
        }

        Ok(WaylandProviderCapabilities {
            idle_notifier_version: version,
            seat_count: self.seats.len(),
        })
    }
}

struct SeatBinding {
    seat: wl_seat::WlSeat,
    input_notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
}

struct WaylandProviderState<F> {
    registry: Option<wl_registry::WlRegistry>,
    registry_facts: RegistryFacts,
    idle_notifier: Option<(u32, ext_idle_notifier_v1::ExtIdleNotifierV1)>,
    seats: HashMap<u32, SeatBinding>,
    initialized: bool,
    running: bool,
    error: Option<WaylandProviderError>,
    on_observation: F,
}

impl<F> WaylandProviderState<F>
where
    F: FnMut(SessionObservation) -> bool + 'static,
{
    fn new(on_observation: F) -> Self {
        Self {
            registry: None,
            registry_facts: RegistryFacts::default(),
            idle_notifier: None,
            seats: HashMap::new(),
            initialized: false,
            running: true,
            error: None,
            on_observation,
        }
    }

    fn publish(&mut self, observation: SessionObservation) {
        if !(self.on_observation)(observation) {
            self.running = false;
        }
    }

    fn bind_idle_notifier(
        &mut self,
        registry: &wl_registry::WlRegistry,
        queue_handle: &QueueHandle<Self>,
    ) {
        if self.idle_notifier.is_some() {
            return;
        }
        let Some(global) = self.registry_facts.selected_idle_notifier() else {
            return;
        };

        let notifier = registry.bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(
            global.name,
            REQUIRED_IDLE_NOTIFIER_VERSION.min(global.version),
            queue_handle,
            (),
        );
        self.idle_notifier = Some((global.name, notifier));
        self.attach_unmonitored_seats(queue_handle);
    }

    fn bind_seat(
        &mut self,
        registry: &wl_registry::WlRegistry,
        queue_handle: &QueueHandle<Self>,
        name: u32,
        _version: u32,
    ) {
        if self.seats.contains_key(&name) {
            return;
        }

        let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, 1, queue_handle, name);
        self.seats.insert(
            name,
            SeatBinding {
                seat,
                input_notification: None,
            },
        );
        self.attach_seat(name, queue_handle);
    }

    fn attach_unmonitored_seats(&mut self, queue_handle: &QueueHandle<Self>) {
        let seat_names: Vec<u32> = self.seats.keys().copied().collect();
        for name in seat_names {
            self.attach_seat(name, queue_handle);
        }
    }

    fn attach_seat(&mut self, name: u32, queue_handle: &QueueHandle<Self>) {
        let Some((_, notifier)) = self.idle_notifier.as_ref() else {
            return;
        };
        let Some(binding) = self.seats.get_mut(&name) else {
            return;
        };
        if binding.input_notification.is_some() {
            return;
        }

        binding.input_notification =
            Some(notifier.get_input_idle_notification(0, &binding.seat, queue_handle, name));
    }

    fn remove_global(&mut self, name: u32) {
        self.registry_facts.remove(name);

        let removed_bound_notifier = self
            .idle_notifier
            .as_ref()
            .is_some_and(|(global_name, _)| *global_name == name);

        let removed_seat = if let Some(mut binding) = self.seats.remove(&name) {
            if let Some(notification) = binding.input_notification.take() {
                notification.destroy();
            }
            true
        } else {
            false
        };

        if let Some(err) = global_removal_error(
            self.initialized,
            removed_bound_notifier,
            removed_seat,
            self.seats.len(),
        ) {
            self.error = Some(err);
            self.running = false;
        }
    }

    fn take_error(&mut self) -> Option<WaylandProviderError> {
        self.error.take()
    }
}

fn global_removal_error(
    initialized: bool,
    removed_bound_notifier: bool,
    removed_seat: bool,
    remaining_seat_count: usize,
) -> Option<WaylandProviderError> {
    if removed_bound_notifier {
        Some(WaylandProviderError::IdleNotifierRemoved)
    } else if initialized && removed_seat && remaining_seat_count == 0 {
        Some(WaylandProviderError::LastSeatRemoved)
    } else {
        None
    }
}

fn notification_is_activity(event: &ext_idle_notification_v1::Event) -> bool {
    matches!(event, ext_idle_notification_v1::Event::Resumed)
}

impl<F> Dispatch<wl_registry::WlRegistry, ()> for WaylandProviderState<F>
where
    F: FnMut(SessionObservation) -> bool + 'static,
{
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue_handle: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                state.registry_facts.add(name, interface.as_str(), version);
                if interface == ext_idle_notifier_v1::ExtIdleNotifierV1::interface().name {
                    state.bind_idle_notifier(registry, queue_handle);
                } else if interface == wl_seat::WlSeat::interface().name {
                    state.bind_seat(registry, queue_handle, name, version);
                }
            }
            wl_registry::Event::GlobalRemove { name } => state.remove_global(name),
            _ => {}
        }
    }
}

#[derive(Default)]
struct WaylandCapabilityProbeState {
    registry_facts: RegistryFacts,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandCapabilityProbeState {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => state.registry_facts.add(name, interface.as_str(), version),
            wl_registry::Event::GlobalRemove { name } => state.registry_facts.remove(name),
            _ => {}
        }
    }
}

impl<F> Dispatch<wl_seat::WlSeat, u32> for WaylandProviderState<F>
where
    F: FnMut(SessionObservation) -> bool + 'static,
{
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl<F> Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, ()> for WaylandProviderState<F>
where
    F: FnMut(SessionObservation) -> bool + 'static,
{
    fn event(
        _: &mut Self,
        _: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl<F> Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, u32> for WaylandProviderState<F>
where
    F: FnMut(SessionObservation) -> bool + 'static,
{
    fn event(
        state: &mut Self,
        notification: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        seat_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let current = state
            .seats
            .get(seat_name)
            .and_then(|binding| binding.input_notification.as_ref());
        if current == Some(notification) && notification_is_activity(&event) {
            state.publish(SessionObservation::Inactivity {
                observation: InactivityObservation::DesktopActivityObserved,
                source: EventSource::DesktopSession,
                observed_at: Instant::now(),
            });
        }
    }
}

type InitializedWaylandProvider<F> = (
    EventQueue<WaylandProviderState<F>>,
    WaylandProviderState<F>,
    WaylandProviderCapabilities,
);

fn initialize_provider<F>(
    connection: Connection,
    on_observation: F,
    stop: &AtomicBool,
) -> Result<InitializedWaylandProvider<F>, WaylandProviderError>
where
    F: FnMut(SessionObservation) -> bool + 'static,
{
    let display = connection.display();
    let mut event_queue = connection.new_event_queue();
    let queue_handle = event_queue.handle();
    let mut state = WaylandProviderState::new(on_observation);
    state.registry = Some(display.get_registry(&queue_handle, ()));

    roundtrip(&connection, &mut event_queue, &mut state, stop)?;
    let capabilities = state.registry_facts.capabilities()?;
    state.initialized = true;
    roundtrip(&connection, &mut event_queue, &mut state, stop)?;
    if let Some(err) = state.take_error() {
        return Err(err);
    }

    Ok((event_queue, state, capabilities))
}

fn connect_wayland() -> Result<Connection, WaylandProviderError> {
    // `connect_to_env` removes an inherited WAYLAND_SOCKET from the process
    // environment. Monitor startup must call this before spawning any threads.
    Connection::connect_to_env().map_err(|err| WaylandProviderError::Connection(err.to_string()))
}

fn probe_wayland_capabilities_on(
    connection: Connection,
) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
    let display = connection.display();
    let mut event_queue = connection.new_event_queue();
    let queue_handle = event_queue.handle();
    let _registry = display.get_registry(&queue_handle, ());
    let mut state = WaylandCapabilityProbeState::default();
    roundtrip(
        &connection,
        &mut event_queue,
        &mut state,
        &AtomicBool::new(false),
    )?;
    state.registry_facts.capabilities()
}

fn reconnect_wayland() -> Result<Connection, WaylandProviderError> {
    // Unlike connect_to_env, reconnect never consumes or mutates WAYLAND_SOCKET.
    let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
    let display = PathBuf::from(display);
    let path = if display.is_absolute() {
        display
    } else {
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            WaylandProviderError::Connection("XDG_RUNTIME_DIR is unset".to_string())
        })?)
        .join(display)
    };
    let socket = UnixStream::connect(path)
        .map_err(|err| WaylandProviderError::Connection(err.to_string()))?;
    Connection::from_socket(socket).map_err(|err| WaylandProviderError::Connection(err.to_string()))
}

// Bound connection setup as well as idle waits so one stalled interface cannot
// hold the monitor's startup or cancellation indefinitely.
fn roundtrip<State>(
    connection: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    stop: &AtomicBool,
) -> Result<(), WaylandProviderError>
where
    State: Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> + 'static,
{
    let done = Arc::new(AtomicBool::new(false));
    connection
        .display()
        .sync(&queue.handle(), Arc::clone(&done));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done.load(Ordering::SeqCst) {
        if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return Err(WaylandProviderError::Dispatch(
                "Wayland initialization cancelled or timed out".to_string(),
            ));
        }
        dispatch_once(queue, state)?;
    }
    Ok(())
}

fn dispatch_once<State: 'static>(
    queue: &mut EventQueue<State>,
    state: &mut State,
) -> Result<(), WaylandProviderError> {
    queue
        .dispatch_pending(state)
        .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
    queue
        .flush()
        .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
    if let Some(guard) = queue.prepare_read() {
        let mut fd = libc::pollfd {
            fd: guard.connection_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd points to one initialized pollfd, and the read guard owns
        // the descriptor throughout this bounded wait.
        let ready = unsafe { libc::poll(&mut fd, 1, 50) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(WaylandProviderError::Dispatch(err.to_string()));
            }
        } else if ready > 0 {
            guard
                .read()
                .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
        }
    }
    Ok(())
}

impl<F: FnMut(SessionObservation) -> bool + 'static>
    Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> for WaylandProviderState<F>
{
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        done.store(true, Ordering::SeqCst);
    }
}
impl Dispatch<wl_callback::WlCallback, Arc<AtomicBool>> for WaylandCapabilityProbeState {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        done: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        done.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ext_idle_notification_v1, global_removal_error, notification_is_activity, RegistryFacts,
        WaylandProviderCapabilities, WaylandProviderError,
    };

    const NOTIFIER: &str = "ext_idle_notifier_v1";
    const SEAT: &str = "wl_seat";

    #[test]
    fn version_two_notifier_and_any_seat_satisfy_the_contract() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 2);
        facts.add(11, SEAT, 9);

        assert_eq!(
            facts.capabilities(),
            Ok(WaylandProviderCapabilities {
                idle_notifier_version: 2,
                seat_count: 1,
            })
        );
    }

    #[test]
    fn version_one_notifier_is_rejected_precisely() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 1);
        facts.add(11, SEAT, 9);

        assert_eq!(
            facts.capabilities(),
            Err(WaylandProviderError::UnsupportedIdleNotifierVersion(1))
        );
    }

    #[test]
    fn missing_notifier_is_distinct_from_missing_seat() {
        let mut facts = RegistryFacts::default();
        facts.add(11, SEAT, 9);
        assert_eq!(
            facts.capabilities(),
            Err(WaylandProviderError::MissingIdleNotifier)
        );

        facts.add(10, NOTIFIER, 2);
        facts.remove(11);
        assert_eq!(facts.capabilities(), Err(WaylandProviderError::NoSeats));
    }

    #[test]
    fn all_advertised_seats_are_counted_without_capability_filtering() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 2);
        facts.add(11, SEAT, 1);
        facts.add(12, SEAT, 9);

        assert_eq!(facts.capabilities().unwrap().seat_count, 2);
    }

    #[test]
    fn highest_notifier_version_is_selected() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 1);
        facts.add(20, NOTIFIER, 2);

        assert_eq!(facts.selected_idle_notifier().unwrap().name, 20);
    }

    #[test]
    fn registry_churn_updates_the_advertised_seat_set() {
        let mut facts = RegistryFacts::default();
        facts.add(10, NOTIFIER, 2);
        facts.add(11, SEAT, 1);
        facts.add(12, SEAT, 1);
        facts.remove(11);
        assert_eq!(facts.capabilities().unwrap().seat_count, 1);

        facts.add(13, SEAT, 1);
        assert_eq!(facts.capabilities().unwrap().seat_count, 2);
    }

    #[test]
    fn bound_notifier_and_last_seat_removals_are_fatal_after_startup() {
        assert_eq!(
            global_removal_error(true, true, false, 1),
            Some(WaylandProviderError::IdleNotifierRemoved)
        );
        assert_eq!(
            global_removal_error(true, false, true, 0),
            Some(WaylandProviderError::LastSeatRemoved)
        );
        assert_eq!(global_removal_error(true, false, true, 1), None);
        assert_eq!(global_removal_error(false, false, true, 0), None);
    }

    #[test]
    fn only_resumed_notifications_map_to_desktop_activity() {
        assert!(!notification_is_activity(
            &ext_idle_notification_v1::Event::Idled
        ));
        assert!(notification_is_activity(
            &ext_idle_notification_v1::Event::Resumed
        ));
    }
    // Model only the registry, sync and input-notification messages consumed by
    // this adapter. Hold the final sync reply so input definitely precedes setup.
    fn serve_input_during_setup(
        mut peer: std::os::unix::net::UnixStream,
        finish_setup: std::sync::mpsc::Receiver<()>,
    ) {
        use std::io::{Read, Write};
        fn number(bytes: &[u8]) -> u32 {
            u32::from_ne_bytes(bytes[..4].try_into().unwrap())
        }
        fn event(peer: &mut std::os::unix::net::UnixStream, id: u32, opcode: u32, body: &[u8]) {
            peer.write_all(&id.to_ne_bytes()).unwrap();
            peer.write_all(&(((body.len() as u32 + 8) << 16) | opcode).to_ne_bytes())
                .unwrap();
            peer.write_all(body).unwrap();
        }
        fn global(
            peer: &mut std::os::unix::net::UnixStream,
            registry: u32,
            name: u32,
            interface: &str,
            version: u32,
        ) {
            let mut body = name.to_ne_bytes().to_vec();
            body.extend_from_slice(&(interface.len() as u32 + 1).to_ne_bytes());
            body.extend_from_slice(interface.as_bytes());
            body.push(0);
            while !body.len().is_multiple_of(4) {
                body.push(0);
            }
            body.extend_from_slice(&version.to_ne_bytes());
            event(peer, registry, 0, &body);
        }
        peer.set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut registry = 0;
        let mut notifier = 0;
        let mut syncs = 0;
        loop {
            let mut header = [0; 8];
            match peer.read_exact(&mut header) {
                Ok(()) => (),
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return,
                Err(err) => panic!("mock Wayland read: {err}"),
            }
            let id = number(&header);
            let size_opcode = number(&header[4..]);
            let opcode = size_opcode & 0xffff;
            let mut body = vec![0; (size_opcode >> 16) as usize - 8];
            peer.read_exact(&mut body).unwrap();
            if id == 1 && opcode == 1 {
                registry = number(&body);
                global(&mut peer, registry, 10, NOTIFIER, 2);
                global(&mut peer, registry, 11, SEAT, 1);
            } else if id == 1 && opcode == 0 {
                syncs += 1;
                if syncs == 2 {
                    finish_setup
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap();
                }
                let callback = number(&body);
                event(&mut peer, callback, 0, &0u32.to_ne_bytes());
                event(&mut peer, 1, 1, &callback.to_ne_bytes());
            } else if id == registry && opcode == 0 {
                if number(&body) == 10 {
                    notifier = number(&body[body.len() - 4..]);
                }
            } else if id == notifier && opcode == 2 {
                let notification = number(&body);
                event(&mut peer, notification, 0, &[]);
                event(&mut peer, notification, 1, &[]);
            }
        }
    }

    #[test]
    fn input_is_published_before_initialization_completes_and_quiet_shutdown_finishes() {
        use super::{ActivityAdapter, WaylandSource};
        use crate::session::{inactivity::InactivityObservation, SessionObservation};
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        use std::time::{Duration, Instant};
        let (socket, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (finish_setup, setup) = mpsc::channel();
        let server = std::thread::spawn(move || serve_input_during_setup(peer, setup));
        let source = Arc::new(WaylandSource::new(Some(
            wayland_client::Connection::from_socket(socket).unwrap(),
        )));
        let stop = Arc::new(AtomicBool::new(false));
        let (input, observed) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let adapter = Arc::clone(&source);
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            adapter.run(
                Arc::new(move |observation| {
                    input.send(observation).unwrap();
                }),
                &worker_stop,
            );
            finished.send(()).unwrap();
        });
        let observation = observed.recv_timeout(Duration::from_secs(1));
        let status_during_setup = source.status();
        finish_setup.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !source.status().is_available() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let status_after_setup = source.status();
        stop.store(true, Ordering::SeqCst);
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("quiet adapter stops");
        worker.join().unwrap();
        server.join().unwrap();
        assert!(matches!(
            observation.unwrap(),
            SessionObservation::Inactivity {
                observation: InactivityObservation::DesktopActivityObserved,
                ..
            }
        ));
        assert!(!status_during_setup.is_available());
        assert!(status_after_setup.is_available());
    }

    #[test]
    fn stalled_initialization_is_cancellable_without_compositor_events() {
        let (client, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(client).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = std::sync::Arc::clone(&stop);
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let source = super::WaylandSource::new(None);
            let result =
                source.run_connection(connection, std::sync::Arc::new(|_| {}), &worker_stop);
            sender.send(result).unwrap();
        });
        use std::io::Read;
        peer.set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        let mut request = [0; 256];
        assert!(
            peer.read(&mut request).unwrap() > 0,
            "initialization must be pending before cancellation"
        );
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .is_err());
        worker.join().unwrap();
    }
    #[test]
    fn an_obsolete_notification_cannot_report_input_for_a_reused_seat_name() {
        use wayland_client::Dispatch;
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let observations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = std::sync::Arc::clone(&observations);
        let mut state = super::WaylandProviderState::new(move |value| {
            output.lock().unwrap().push(value);
            true
        });
        let queue = connection.new_event_queue();
        let handle = queue.handle();
        let registry = connection.display().get_registry(&handle, ());
        state.registry_facts.add(10, NOTIFIER, 2);
        state.bind_idle_notifier(&registry, &handle);
        state.bind_seat(&registry, &handle, 11, 1);
        let old = state.seats[&11]
            .input_notification
            .as_ref()
            .unwrap()
            .clone();
        state.remove_global(11);
        state.bind_seat(&registry, &handle, 11, 1);
        let current = state.seats[&11]
            .input_notification
            .as_ref()
            .unwrap()
            .clone();
        <super::WaylandProviderState<_> as Dispatch<_, u32>>::event(
            &mut state,
            &old,
            ext_idle_notification_v1::Event::Resumed,
            &11,
            &connection,
            &handle,
        );
        assert!(observations.lock().unwrap().is_empty());
        <super::WaylandProviderState<_> as Dispatch<_, u32>>::event(
            &mut state,
            &current,
            ext_idle_notification_v1::Event::Resumed,
            &11,
            &connection,
            &handle,
        );
        assert_eq!(observations.lock().unwrap().len(), 1);
    }
}
