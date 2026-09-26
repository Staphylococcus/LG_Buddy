use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::mem::offset_of;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
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
    Cancelled,
    InitializationTimeout,
    InheritedSocketRequiresOwner,
    InvalidSocketPath,
    InvalidWaylandDisplay,
    InvalidRuntimeDirectory,
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
            Self::Cancelled => write!(f, "the Wayland readiness check was cancelled"),
            Self::InitializationTimeout => {
                write!(f, "Wayland initialization timed out")
            }
            Self::InheritedSocketRequiresOwner => write!(
                f,
                "an inherited Wayland socket requires the correctly owned adapter connection or context; a separate foreground client must not share it"
            ),
            Self::InvalidSocketPath => write!(
                f,
                "the resolved Wayland socket path is empty, contains a NUL byte, or is too long for a Unix socket address"
            ),
            Self::InvalidWaylandDisplay => {
                write!(f, "the supplied WAYLAND_DISPLAY value is empty")
            }
            Self::InvalidRuntimeDirectory => write!(
                f,
                "a relative or default WAYLAND_DISPLAY requires a nonempty absolute XDG_RUNTIME_DIR"
            ),
        }
    }
}

impl Error for WaylandProviderError {}

/// Selects the Wayland socket path for a caller-owned foreground client from
/// captured environment values.
///
/// The values are the caller's captured context: an inherited
/// `WAYLAND_SOCKET`, `WAYLAND_DISPLAY`, and `XDG_RUNTIME_DIR`. This helper is
/// pure: it performs no environment, filesystem, socket, or file-descriptor
/// I/O. An inherited socket is always rejected: inherited connections require
/// the correctly owned adapter connection or context, and a separate
/// foreground client must not share an inherited socket by dup. This helper
/// intentionally fails closed for that context, without changing startup
/// behavior.
fn resolve_foreground_wayland_path(
    inherited_socket: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
    runtime_dir: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, WaylandProviderError> {
    if inherited_socket.is_some() {
        return Err(WaylandProviderError::InheritedSocketRequiresOwner);
    }

    let display_name = match display {
        None => std::ffi::OsStr::new("wayland-0"),
        Some(value) if value.is_empty() => return Err(WaylandProviderError::InvalidWaylandDisplay),
        Some(value) if value.as_bytes().first() == Some(&b'/') => return Ok(PathBuf::from(value)),
        Some(value) => value,
    };

    let runtime_dir = match runtime_dir {
        Some(dir) if !dir.is_empty() && dir.as_bytes().first() == Some(&b'/') => dir,
        _ => return Err(WaylandProviderError::InvalidRuntimeDirectory),
    };

    Ok(PathBuf::from(runtime_dir).join(display_name))
}

/// sun_path holds the NUL-terminated pathname, so a usable name is strictly
/// shorter than the field; the terminator must fit inside it.
const SUN_PATH_CAPACITY: usize =
    std::mem::size_of::<libc::sockaddr_un>() - offset_of!(libc::sockaddr_un, sun_path);

/// Opens a dedicated nonblocking Unix stream to the resolved foreground
/// Wayland socket. The socket is created with `SOCK_NONBLOCK |
/// SOCK_CLOEXEC`, wrapped in an owned descriptor immediately, and closed on
/// every error or cancellation. A single nonblocking `connect` attempt is
/// made; only an actual 0 return connects. Linux returns `EAGAIN` when a
/// nonblocking connect cannot complete immediately, which is not proof of a
/// pending successful connection, so every negative result fails promptly
/// rather than polling, retrying, or waiting. A caller may retry the overall
/// readiness operation.
fn connect_foreground_wayland_socket(
    path: &std::path::Path,
    stop: &AtomicBool,
) -> Result<UnixStream, WaylandProviderError> {
    if stop.load(Ordering::SeqCst) {
        return Err(WaylandProviderError::Cancelled);
    }

    // Validate the raw pathname bytes before filling a sockaddr_un: a NUL byte
    // must end the path, not appear inside it, and the name needs room for the
    // terminator. Reject rather than truncate or fall into the abstract
    // namespace, and never echo the path into diagnostics.
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= SUN_PATH_CAPACITY {
        return Err(WaylandProviderError::InvalidSocketPath);
    }

    // SAFETY: socket() with a valid address family and flags either returns a
    // fresh, unconnected descriptor owned by this process or -1; nothing else
    // can use the returned fd before we take ownership below.
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw_fd < 0 {
        return Err(WaylandProviderError::Connection(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    // SAFETY: socket() just handed us ownership of raw_fd, and from_raw_fd
    // consumes exactly that one descriptor.
    let socket = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };

    let path_len = bytes.len();
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // SAFETY: bytes are nonempty, NUL-free, and strictly shorter than
    // sun_path, so the cast copy plus the terminator stays inside the field;
    // every byte written is a valid c_char on Linux (c_char is i8).
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            address.sun_path.as_mut_ptr().cast(),
            path_len,
        );
    }
    address.sun_path[path_len] = 0;

    if stop.load(Ordering::SeqCst) {
        // Cancellation after allocation: release the dedicated descriptor and
        // do not attempt the connection.
        return Err(WaylandProviderError::Cancelled);
    }

    // SAFETY: address is a fully initialized sockaddr_un and socket is a
    // valid, owned, unconnected Unix socket descriptor.
    let connected = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            &address as *const libc::sockaddr_un as *const libc::sockaddr,
            (offset_of!(libc::sockaddr_un, sun_path) + path_len + 1) as libc::socklen_t,
        )
    };
    if stop.load(Ordering::SeqCst) {
        // The connect attempt happened; releasing the descriptor here does not
        // retry or wait for a pending connection.
        return Err(WaylandProviderError::Cancelled);
    }
    if connected != 0 {
        // Every negative result (EAGAIN, EINPROGRESS, EINTR, ENOENT, ECONNREFUSED,
        // ...) closes the owned descriptor and fails promptly; this check is a
        // foreground probe and deliberately does not treat EAGAIN as pending
        // success.
        return Err(WaylandProviderError::Connection(
            std::io::Error::last_os_error().to_string(),
        ));
    }

    Ok(UnixStream::from(socket))
}

/// One bounded foreground Wayland readiness check on a caller-supplied
/// context. The context is the captured environment: an inherited
/// `WAYLAND_SOCKET`, a `WAYLAND_DISPLAY`, and an `XDG_RUNTIME_DIR`. This
/// entrypoint never reads or writes the process environment, never duplicates
/// or adopts an inherited FD, and never falls back to another display. An
/// inherited socket is rejected: a separate foreground client must not share
/// it. It resolves the exact path with the accepted resolver, opens its own
/// dedicated nonblocking stream, and runs the full accepted readiness check:
/// a connected socket alone is not readiness, and success requires the whole
/// notification setup with both sync barriers.
pub(crate) fn check_foreground_wayland_readiness(
    inherited_socket: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
    runtime_dir: Option<&std::ffi::OsStr>,
    stop: &AtomicBool,
) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
    if stop.load(Ordering::SeqCst) {
        return Err(WaylandProviderError::Cancelled);
    }
    let path = resolve_foreground_wayland_path(inherited_socket, display, runtime_dir)?;
    let socket = connect_foreground_wayland_socket(&path, stop)?;
    let connection = Connection::from_socket(socket)
        .map_err(|err| WaylandProviderError::Connection(err.to_string()))?;
    check_wayland_readiness_on(connection, stop)
}

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
    if stop.load(Ordering::SeqCst) {
        return Err(WaylandProviderError::Cancelled);
    }
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

/// Bounded readiness check on a dedicated observation connection supplied by
/// the caller. The caller owns this exact connection for the check and
/// retains no clones of it: the operation takes ownership here, drops its
/// temporary queue, state, and connection before returning, and never
/// constructs a second client from a duplicated inherited FD. It does not
/// choose a display, consume WAYLAND_SOCKET, read or mutate the environment,
/// or authorize runtime; safe connection selection and foreground wiring are
/// later work. It performs observation-only protocol setup: no monitor loop,
/// no published observations, no status changes, no action/runtime/config
/// code. Observations emitted during initialization are discarded by the
/// internal no-op callback. Cancellation is honored before any protocol
/// requests are sent, during each bounded roundtrip, and immediately before
/// a successful return; a pre-cancelled check closes its owned connection
/// without writing.
pub(crate) fn check_wayland_readiness_on(
    connection: Connection,
    stop: &AtomicBool,
) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
    let (_, state, capabilities) = initialize_provider(connection, |_observation| true, stop)?;
    if stop.load(Ordering::SeqCst) {
        return Err(WaylandProviderError::Cancelled);
    }
    drop(state);
    Ok(capabilities)
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
        if stop.load(Ordering::SeqCst) {
            return Err(WaylandProviderError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(WaylandProviderError::InitializationTimeout);
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
            let result = guard.read();
            // Queued events retain their own queue through the proxy data. Drain
            // each read before setup can reject a capability or monitoring stops,
            // including events queued before a later message reports an error.
            queue
                .dispatch_pending(state)
                .map_err(|err| WaylandProviderError::Dispatch(err.to_string()))?;
            match result {
                Ok(_) => (),
                // Readable bytes may contain only part of an event. The backend
                // retains them until a later read can complete the message.
                Err(wayland_client::backend::WaylandError::Io(err))
                    if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(WaylandProviderError::Dispatch(err.to_string())),
            }
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
        check_foreground_wayland_readiness, check_wayland_readiness_on,
        connect_foreground_wayland_socket, ext_idle_notification_v1, global_removal_error,
        notification_is_activity, RegistryFacts, WaylandProviderCapabilities, WaylandProviderError,
        SUN_PATH_CAPACITY,
    };
    use std::ffi::OsStr;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;

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

    fn assert_read_releases_event_data(protocol_error: bool) {
        use std::io::Write;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use wayland_client::Proxy;

        let (socket, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let mut queue = connection.new_event_queue();
        let mut state = super::WaylandCapabilityProbeState::default();
        let done = Arc::new(AtomicBool::new(false));
        let retained = Arc::downgrade(&done);
        let callback = connection
            .display()
            .sync(&queue.handle(), Arc::clone(&done));
        let mut words = vec![callback.id().protocol_id(), 12 << 16, 0];
        if protocol_error {
            // wl_display.error after the callback: the read queues an event
            // before failing, and still needs to release that event's data.
            words.extend([1, 24 << 16, 1, 0, 2, u32::from_ne_bytes([b'x', 0, 0, 0])]);
        }
        let bytes: Vec<u8> = words.into_iter().flat_map(u32::to_ne_bytes).collect();
        peer.write_all(&bytes).unwrap();

        let result = super::dispatch_once(&mut queue, &mut state);
        let dispatched = done.load(Ordering::SeqCst);
        drop(done);
        drop(callback);
        drop(queue);
        drop(connection);

        // Dropping the socket/queue alone does not break the cycle between a
        // pending event's proxy data and its queue. Check actual reclamation.
        assert!(retained.upgrade().is_none(), "queued event data leaked");
        assert!(
            dispatched,
            "the read must deliver its callback before returning"
        );
        assert_eq!(result.is_err(), protocol_error);
    }

    #[test]
    fn repeated_connection_teardown_releases_events_from_the_final_read() {
        for _ in 0..32 {
            assert_read_releases_event_data(false);
        }
    }

    #[test]
    fn protocol_failure_releases_events_queued_before_the_error() {
        assert_read_releases_event_data(true);
    }

    #[test]
    fn partial_event_waits_for_remaining_bytes_and_disconnect_is_still_an_error() {
        use std::io::Write;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use wayland_client::Proxy;

        let (socket, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = wayland_client::Connection::from_socket(socket).unwrap();
        let mut queue = connection.new_event_queue();
        let mut state = super::WaylandCapabilityProbeState::default();
        let done = Arc::new(AtomicBool::new(false));
        let callback = connection
            .display()
            .sync(&queue.handle(), Arc::clone(&done));
        let message: Vec<u8> = [callback.id().protocol_id(), 12 << 16, 0]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();

        // poll() sees bytes, but the backend cannot decode a complete event yet.
        peer.write_all(&message[..4]).unwrap();
        super::dispatch_once(&mut queue, &mut state).unwrap();
        assert!(!done.load(Ordering::SeqCst));

        peer.write_all(&message[4..]).unwrap();
        super::dispatch_once(&mut queue, &mut state).unwrap();
        assert!(done.load(Ordering::SeqCst));

        drop(peer);
        assert!(super::dispatch_once(&mut queue, &mut state).is_err());
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
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
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
        let published_source = Arc::clone(&source);
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            adapter.run(
                Arc::new(move |observation| {
                    // Capture availability before allowing setup to finish.
                    // The test thread need not run within the adapter's setup deadline.
                    input
                        .send((observation, published_source.status()))
                        .unwrap();
                    finish_setup.send(()).unwrap();
                }),
                &worker_stop,
            );
            finished.send(()).unwrap();
        });
        let observation = observed.recv_timeout(Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !source.status().is_available() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let status_after_setup = source.status();
        stop.store(true, Ordering::SeqCst);
        completion
            .recv_timeout(Duration::from_secs(5))
            .expect("quiet adapter stops");
        worker.join().unwrap();
        server.join().unwrap();
        let (observation, status_during_setup) = observation.unwrap();
        assert!(matches!(
            observation,
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
        assert!(matches!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            Err(WaylandProviderError::Cancelled)
        ));
        worker.join().unwrap();
    }

    fn readiness_pair() -> (
        wayland_client::Connection,
        std::os::unix::net::UnixStream,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let (client, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        peer.set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let connection = wayland_client::Connection::from_socket(client).unwrap();
        (
            connection,
            peer,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    }

    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
    struct ReadinessTrace {
        sync_requests: usize,
        sync_replies: usize,
        notification_requests: usize,
        notification_events: usize,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ReadinessFixtureMode {
        Complete,
        RejectNotification,
        StallSecondSync,
    }

    fn readiness_number(bytes: &[u8]) -> u32 {
        u32::from_ne_bytes(bytes[..4].try_into().unwrap())
    }

    fn readiness_event(id: u32, opcode: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = id.to_ne_bytes().to_vec();
        bytes.extend_from_slice(&(((body.len() as u32 + 8) << 16) | opcode).to_ne_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn readiness_string(value: &str) -> Vec<u8> {
        let mut body = (value.len() as u32 + 1).to_ne_bytes().to_vec();
        body.extend_from_slice(value.as_bytes());
        body.push(0);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        body
    }

    fn readiness_global(registry: u32, name: u32, interface: &str, version: u32) -> Vec<u8> {
        let mut body = name.to_ne_bytes().to_vec();
        body.extend(readiness_string(interface));
        body.extend_from_slice(&version.to_ne_bytes());
        readiness_event(registry, 0, &body)
    }

    // Read actual requests, inject observations/rejection only after notification
    // creation, and return counters only once the check closes its connection.
    fn serve_readiness_fixture(
        mut peer: std::os::unix::net::UnixStream,
        notifier_version: Option<u32>,
        seat: bool,
        mode: ReadinessFixtureMode,
        mut before_second_reply: impl FnMut(ReadinessTrace),
    ) -> ReadinessTrace {
        use std::io::{Read, Write};
        let mut trace = ReadinessTrace::default();
        let mut registry = None;
        let mut notifier = None;
        let mut rejected = false;
        loop {
            let mut header = [0; 8];
            match peer.read_exact(&mut header) {
                Ok(()) => (),
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return trace,
                Err(err) => panic!("mock Wayland read: {err}"),
            }
            let id = readiness_number(&header);
            let size_opcode = readiness_number(&header[4..]);
            let opcode = size_opcode & 0xffff;
            let mut body = vec![0; (size_opcode >> 16) as usize - 8];
            peer.read_exact(&mut body).unwrap();
            if id == 1 && opcode == 1 {
                let registry_id = readiness_number(&body);
                registry = Some(registry_id);
                let mut globals = Vec::new();
                if let Some(version) = notifier_version {
                    globals.extend(readiness_global(registry_id, 10, NOTIFIER, version));
                }
                if seat {
                    globals.extend(readiness_global(registry_id, 11, SEAT, 1));
                }
                peer.write_all(&globals).unwrap();
            } else if Some(id) == registry && opcode == 0 {
                if readiness_number(&body) == 10 {
                    notifier = Some(readiness_number(&body[body.len() - 4..]));
                }
            } else if Some(id) == notifier && opcode == 2 {
                trace.notification_requests += 1;
                assert_eq!(
                    trace.sync_replies, 1,
                    "notification setup follows registry discovery"
                );
                if mode == ReadinessFixtureMode::RejectNotification {
                    let mut error = id.to_ne_bytes().to_vec();
                    error.extend_from_slice(&1u32.to_ne_bytes());
                    error.extend(readiness_string("readiness notification rejected"));
                    peer.write_all(&readiness_event(1, 0, &error)).unwrap();
                    rejected = true;
                } else {
                    let notification = readiness_number(&body);
                    let mut events = readiness_event(notification, 0, &[]);
                    events.extend(readiness_event(notification, 1, &[]));
                    peer.write_all(&events).unwrap();
                    trace.notification_events += 2;
                }
            } else if id == 1 && opcode == 0 {
                trace.sync_requests += 1;
                if rejected {
                    // Drain already-sent requests after the fatal error; never
                    // acknowledge a barrier after rejecting notification setup.
                    continue;
                }
                if trace.sync_requests == 2 {
                    assert_eq!(trace.notification_requests, 1);
                    assert_eq!(trace.notification_events, 2);
                    before_second_reply(trace);
                    if mode == ReadinessFixtureMode::StallSecondSync {
                        continue;
                    }
                }
                let callback = readiness_number(&body);
                let mut reply = readiness_event(callback, 0, &0u32.to_ne_bytes());
                reply.extend(readiness_event(1, 1, &callback.to_ne_bytes()));
                peer.write_all(&reply).unwrap();
                trace.sync_replies += 1;
            }
        }
    }

    #[test]
    fn readiness_check_completes_after_both_sync_barriers_and_releases() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let (connection, peer, stop) = readiness_pair();
        let worker_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(check_wayland_readiness_on(connection, &worker_stop))
                .unwrap();
        });
        let (at_barrier_tx, at_barrier_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            serve_readiness_fixture(
                peer,
                Some(2),
                true,
                ReadinessFixtureMode::Complete,
                |trace| {
                    at_barrier_tx.send(trace).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                },
            )
        });
        let pending = at_barrier_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("notification creation and second sync must precede readiness");
        assert_eq!(pending.sync_requests, 2);
        assert_eq!(pending.sync_replies, 1);
        assert_eq!(pending.notification_requests, 1);
        assert_eq!(pending.notification_events, 2);
        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(50)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "readiness must wait for the held second barrier"
        );
        release_tx.send(()).unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Ok(WaylandProviderCapabilities {
                idle_notifier_version: 2,
                seat_count: 1
            })
        );
        worker.join().unwrap();
        let finished = server.join().expect("owned connection must close with EOF");
        assert_eq!(
            finished,
            ReadinessTrace {
                sync_replies: 2,
                ..pending
            }
        );
    }

    #[test]
    fn registry_insufficiency_keeps_its_existing_errors_and_releases() {
        use std::sync::Arc;
        use std::time::Duration;
        for (notifier, seat, expected) in [
            (None, true, WaylandProviderError::MissingIdleNotifier),
            (
                Some(1),
                true,
                WaylandProviderError::UnsupportedIdleNotifierVersion(1),
            ),
            (Some(2), false, WaylandProviderError::NoSeats),
        ] {
            let (connection, peer, stop) = readiness_pair();
            let worker_stop = Arc::clone(&stop);
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                done_tx
                    .send(check_wayland_readiness_on(connection, &worker_stop))
                    .unwrap();
            });
            let trace = serve_readiness_fixture(
                peer,
                notifier,
                seat,
                ReadinessFixtureMode::Complete,
                |_| panic!("insufficient capabilities must not reach the second barrier"),
            );
            assert_eq!(
                done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                Err(expected)
            );
            assert_eq!(trace.sync_replies, 1);
            assert_eq!(trace.notification_requests, 0);
            worker.join().unwrap();
        }
    }

    #[test]
    fn protocol_error_after_capabilities_fails_the_readiness_check() {
        use std::sync::Arc;
        use std::time::Duration;
        let (connection, peer, stop) = readiness_pair();
        let worker_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(check_wayland_readiness_on(connection, &worker_stop))
                .unwrap();
        });
        let trace = serve_readiness_fixture(
            peer,
            Some(2),
            true,
            ReadinessFixtureMode::RejectNotification,
            |_| panic!("fatal notification rejection must not acknowledge a second barrier"),
        );
        let error = done_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, WaylandProviderError::Dispatch(ref message)
            if message.contains("readiness notification rejected")),
            "wrong failure: {error:?}"
        );
        assert_eq!(trace.sync_replies, 1);
        assert_eq!(trace.notification_requests, 1);
        assert_eq!(trace.notification_events, 0);
        worker.join().unwrap();
    }

    #[test]
    fn pre_cancelled_readiness_check_sends_nothing_and_closes() {
        use std::io::Read;
        let (connection, mut peer, stop) = readiness_pair();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            check_wayland_readiness_on(connection, &stop),
            Err(WaylandProviderError::Cancelled)
        );
        assert_eq!(
            peer.read(&mut [0u8; 4]).unwrap(),
            0,
            "a pre-cancelled check must close without sending requests"
        );
    }

    fn check_stalled_readiness(cancel: bool) {
        use std::sync::{atomic::Ordering, mpsc, Arc};
        use std::time::Duration;
        let (connection, peer, stop) = readiness_pair();
        let worker_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(check_wayland_readiness_on(connection, &worker_stop))
                .unwrap();
        });
        let (at_barrier_tx, at_barrier_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            serve_readiness_fixture(
                peer,
                Some(2),
                true,
                ReadinessFixtureMode::StallSecondSync,
                |trace| at_barrier_tx.send(trace).unwrap(),
            )
        });
        let pending = at_barrier_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(pending.sync_requests, 2);
        assert_eq!(pending.sync_replies, 1);
        assert_eq!(pending.notification_requests, 1);
        if cancel {
            stop.store(true, Ordering::SeqCst);
        }
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(if cancel {
                WaylandProviderError::Cancelled
            } else {
                WaylandProviderError::InitializationTimeout
            })
        );
        worker.join().unwrap();
        assert_eq!(
            server
                .join()
                .expect("stalled check must close its connection"),
            pending
        );
    }

    #[test]
    fn stalled_setup_readiness_check_is_cancelled_after_requests_are_received() {
        check_stalled_readiness(true);
    }

    #[test]
    fn stalled_setup_readiness_check_without_cancellation_times_out() {
        check_stalled_readiness(false);
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

    fn os(value: &[u8]) -> &OsStr {
        OsStrExt::from_bytes(value)
    }

    #[test]
    fn foreground_path_resolution_rejects_inherited_sockets_without_fallback() {
        let cases = [os(b"3"), os(b""), os(b"not-a-fd"), os(b"-1")];
        for inherited in cases {
            let result = super::resolve_foreground_wayland_path(
                Some(inherited),
                Some(os(b"/run/user/1000/wayland-0")),
                Some(os(b"/run/user/1000")),
            );
            assert_eq!(
                result,
                Err(WaylandProviderError::InheritedSocketRequiresOwner)
            );
        }
    }

    #[test]
    fn foreground_path_resolution_uses_the_absolute_display_unchanged() {
        let absolute = os(b"/run/user/1000/wayland-0");
        assert_eq!(
            super::resolve_foreground_wayland_path(
                None,
                Some(absolute),
                Some(os(b"/run/user/1000"))
            ),
            Ok(std::path::PathBuf::from(absolute))
        );
        // A missing or invalid runtime_dir is irrelevant for an absolute display.
        assert_eq!(
            super::resolve_foreground_wayland_path(None, Some(absolute), None),
            Ok(std::path::PathBuf::from(absolute))
        );
        assert_eq!(
            super::resolve_foreground_wayland_path(None, Some(absolute), Some(os(b"relative"))),
            Ok(std::path::PathBuf::from(absolute))
        );
    }

    #[test]
    fn foreground_path_resolution_requires_an_absolute_runtime_directory() {
        let display = os(b"wayland-1");
        let relative_runtime = os(b"runtime");
        for runtime in [None, Some(os(b"")), Some(relative_runtime)] {
            assert_eq!(
                super::resolve_foreground_wayland_path(None, Some(display), runtime),
                Err(WaylandProviderError::InvalidRuntimeDirectory)
            );
        }
        assert_eq!(
            super::resolve_foreground_wayland_path(
                None,
                Some(display),
                Some(os(b"/run/user/1000"))
            ),
            Ok(std::path::PathBuf::from("/run/user/1000/wayland-1"))
        );
    }

    #[test]
    fn foreground_path_resolution_defaults_a_missing_display_to_wayland_zero() {
        assert_eq!(
            super::resolve_foreground_wayland_path(None, None, Some(os(b"/run/user/1000"))),
            Ok(std::path::PathBuf::from("/run/user/1000/wayland-0"))
        );
        assert_eq!(
            super::resolve_foreground_wayland_path(None, None, None),
            Err(WaylandProviderError::InvalidRuntimeDirectory)
        );
    }

    #[test]
    fn foreground_path_resolution_rejects_an_explicit_empty_display() {
        assert_eq!(
            super::resolve_foreground_wayland_path(
                None,
                Some(os(b"")),
                Some(os(b"/run/user/1000"))
            ),
            Err(WaylandProviderError::InvalidWaylandDisplay)
        );
    }

    #[test]
    fn foreground_path_resolution_preserves_non_utf8_os_bytes() {
        let runtime = os(b"/run/user/\xff\xfe1000");
        let display = os(b"wayland-\xff\xfe0");
        assert_eq!(
            super::resolve_foreground_wayland_path(None, Some(display), Some(runtime)),
            Ok(std::path::PathBuf::from(runtime).join(display))
        );
    }

    #[test]
    fn foreground_path_error_messages_are_fixed_and_do_not_copy_inputs() {
        for error in [
            WaylandProviderError::InheritedSocketRequiresOwner,
            WaylandProviderError::InvalidSocketPath,
            WaylandProviderError::InvalidWaylandDisplay,
            WaylandProviderError::InvalidRuntimeDirectory,
        ] {
            let message = error.to_string();
            let debug = format!("{error:?}");
            for haystack in [&message, &debug] {
                assert!(
                    !haystack.contains("sample"),
                    "unexpected sample input in {haystack}"
                );
                assert!(
                    !haystack.contains("/run/user/1000"),
                    "unexpected path in {haystack}"
                );
            }
        }
        assert_eq!(
            WaylandProviderError::InheritedSocketRequiresOwner.to_string(),
            "an inherited Wayland socket requires the correctly owned adapter connection or context; a separate foreground client must not share it"
        );
        assert_eq!(
            WaylandProviderError::InvalidWaylandDisplay.to_string(),
            "the supplied WAYLAND_DISPLAY value is empty"
        );
        assert_eq!(
            WaylandProviderError::InvalidRuntimeDirectory.to_string(),
            "a relative or default WAYLAND_DISPLAY requires a nonempty absolute XDG_RUNTIME_DIR"
        );
    }

    /// A unique temporary directory. A process-wide monotonic sequence keeps
    /// each test's socket paths short enough for `sun_path` and unique enough
    /// that parallel tests in one process never bind the same path.
    static TEST_DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn unique() -> Self {
            let seq = TEST_DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = format!("lg-buddy-wl-{}-{}", std::process::id(), seq);
            let path = std::env::temp_dir().join(name);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn socket_path(&self, name: &std::ffi::OsStr) -> std::path::PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn local_listener() -> (TempDir, UnixListener, std::path::PathBuf) {
        let dir = TempDir::unique();
        let path = dir.socket_path(std::ffi::OsStr::new("wayland.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        (dir, listener, path)
    }

    fn accept_local(listener: &UnixListener) -> std::os::unix::net::UnixStream {
        let mut fd = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd referring to this live listener.
        assert_eq!(
            unsafe { libc::poll(&mut fd, 1, 5000) },
            1,
            "no client connected within 5 seconds"
        );
        listener.accept().unwrap().0
    }

    /// Binds a Unix listener with a backlog of 0 so a single unaccepted client
    /// saturates the accept queue.
    fn zero_backlog_listener() -> (TempDir, UnixListener, std::path::PathBuf) {
        let (dir, listener, path) = local_listener();
        // SAFETY: the listener owns a valid AF_UNIX stream socket; listen updates
        // its backlog without rebuilding a raw sockaddr or transferring ownership.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        (dir, listener, path)
    }

    fn fd_status_flags(fd: i32) -> libc::c_int {
        // SAFETY: fcntl with F_GETFL on a valid fd returns status flags or -1.
        let got = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(got >= 0, "F_GETFL failed");
        got
    }

    fn fd_close_on_exec(fd: i32) -> bool {
        // SAFETY: fcntl with F_GETFD on a valid fd returns fd flags or -1.
        let got = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(got >= 0, "F_GETFD failed");
        got & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn foreground_helper_connects_a_dedicated_nonblocking_stream() {
        let (_dir, listener, path) = local_listener();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut stream = connect_foreground_wayland_socket(&path, &stop).unwrap();

        // The dedicated descriptor must be nonblocking (F_GETFL status flags)
        // and close-on-exec (F_GETFD fd flags; not visible via F_GETFL).
        let fd = stream.as_raw_fd();
        assert!(
            fd_status_flags(fd) & libc::O_NONBLOCK != 0,
            "foreground socket must be nonblocking"
        );
        assert!(
            fd_close_on_exec(fd),
            "foreground socket must be close-on-exec"
        );

        // Normal byte exchange works over the connected stream.
        let mut accepted = accept_local(&listener);
        use std::io::{Read, Write};
        accepted
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        stream.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(accepted.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, b"ping");
        accepted.write_all(b"pong").unwrap();
        let mut reply = [0u8; 4];
        assert_eq!(stream.read(&mut reply).unwrap(), 4);
        assert_eq!(&reply, b"pong");

        // Dropping our stream closes the peer's side.
        drop(stream);
        assert_eq!(
            accepted.read(&mut [0u8; 4]).unwrap(),
            0,
            "dropping must EOF the peer"
        );
        drop(accepted);
        drop(listener);
    }

    #[test]
    fn foreground_helper_rejects_missing_and_invalid_paths() {
        let dir = TempDir::unique();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // A path with no listener fails promptly with Connection.
        let missing = dir.socket_path(std::ffi::OsStr::new("absent.sock"));
        match connect_foreground_wayland_socket(&missing, &stop) {
            Err(WaylandProviderError::Connection(message)) => assert_eq!(
                message,
                "No such file or directory (os error 2)".to_string()
            ),
            other => panic!("expected a Connection error, got {other:?}"),
        }

        // A NUL inside the path is a validation error, not a truncation/panic.
        let nulled = dir.socket_path(std::ffi::OsStr::new("a\0b"));
        assert!(matches!(
            connect_foreground_wayland_socket(&nulled, &stop),
            Err(WaylandProviderError::InvalidSocketPath)
        ));

        // A path at or over the sun_path capacity is rejected, not truncated.
        let too_long_name = "x".repeat(SUN_PATH_CAPACITY);
        let too_long = std::ffi::OsStr::new(&too_long_name);
        let long_path = dir.socket_path(too_long);
        assert!(matches!(
            connect_foreground_wayland_socket(&long_path, &stop),
            Err(WaylandProviderError::InvalidSocketPath)
        ));
    }

    #[test]
    fn foreground_helper_connects_a_non_utf8_pathname() {
        let dir = TempDir::unique();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // A non-UTF8 byte in the last path component; the raw bytes must survive.
        let name = std::ffi::OsStr::from_bytes(b"wayland-\xff0");
        let path = dir.socket_path(name);
        let listener = UnixListener::bind(&path).unwrap();
        let stream = connect_foreground_wayland_socket(&path, &stop).unwrap();
        let _accepted = accept_local(&listener);
        drop(stream);
        drop(listener);
    }

    #[test]
    fn foreground_helper_fails_promptly_on_a_saturated_backlog() {
        let (_dir, listener, path) = zero_backlog_listener();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first = connect_foreground_wayland_socket(&path, &stop).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = connect_foreground_wayland_socket(&path, &stop);
            let _ = done_tx.send(result);
        });
        let result = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        // Release the listener before asserting or joining. Even a regression
        // to blocking connect must be woken rather than hanging the test suite.
        drop(listener);
        drop(first);
        worker.join().unwrap();
        assert!(
            matches!(result, Ok(Err(WaylandProviderError::Connection(_)))),
            "a saturated backlog must fail within one second, got {result:?}"
        );
    }

    #[test]
    fn pre_cancelled_foreground_readiness_creates_no_connection() {
        let (_dir, listener, path) = local_listener();
        // A pre-cancelled check must not connect; the listener's nonblocking
        // accept would block (WouldBlock) if no connection had been made.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert_eq!(
            check_foreground_wayland_readiness(None, Some(&path.as_os_str()), None, &stop),
            Err(WaylandProviderError::Cancelled)
        );
        listener.set_nonblocking(true).unwrap();
        let accept_err = match listener.accept() {
            Ok(_) => panic!("a pre-cancelled foreground check must open no connection"),
            Err(err) => err,
        };
        assert_eq!(
            accept_err.kind(),
            std::io::ErrorKind::WouldBlock,
            "a pre-cancelled foreground check must open no connection"
        );
        drop(listener);
    }

    #[test]
    fn inherited_socket_context_rejects_without_an_alternate_connection() {
        let (_dir, alt_listener, alt_path) = local_listener();
        // A test-owned inherited socket, passed as captured context (an FD
        // string). We never touch the process environment.
        let (mut inherited, mut _inherited_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        _inherited_peer
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        inherited
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let inherited_fd_string = inherited.as_raw_fd().to_string();
        let inherited_fd = std::ffi::OsStr::new(&inherited_fd_string);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(
            check_foreground_wayland_readiness(
                Some(inherited_fd),
                Some(&alt_path.as_os_str()),
                None,
                &stop
            ),
            Err(WaylandProviderError::InheritedSocketRequiresOwner)
        );
        // The alternate listener must have seen no connection, and the
        // test-owned inherited socket stays usable.
        alt_listener.set_nonblocking(true).unwrap();
        let accept_err = match alt_listener.accept() {
            Ok(_) => panic!("an inherited socket context must not connect the alternate path"),
            Err(err) => err,
        };
        assert_eq!(
            accept_err.kind(),
            std::io::ErrorKind::WouldBlock,
            "an inherited socket context must not connect the alternate path"
        );
        use std::io::{Read, Write};
        let mut marker = [0u8; 1];
        inherited.write_all(b"k").unwrap();
        assert_eq!(_inherited_peer.read(&mut marker).unwrap(), 1);
        drop(alt_listener);
    }

    #[test]
    fn foreground_readiness_succeeds_only_after_full_notification_setup() {
        let (_dir, listener, path) = local_listener();

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = std::sync::Arc::clone(&stop);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let display = std::ffi::OsString::from(path.clone());
        // Spawn the client first; it connects to the listener, which we then
        // accept. Accepting before spawning would block forever on a connection
        // that has not happened yet.
        let worker = std::thread::spawn(move || {
            done_tx
                .send(check_foreground_wayland_readiness(
                    None,
                    Some(&display),
                    None,
                    &worker_stop,
                ))
                .unwrap();
        });

        // Accept the worker's dedicated connection and serve the fixture on it.
        let peer = accept_local(&listener);
        peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        peer.set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        // Serve the accepted readiness fixture; it returns on peer EOF, i.e.
        // after the client's owned connection has closed.
        let trace =
            serve_readiness_fixture(peer, Some(2), true, ReadinessFixtureMode::Complete, |_| {});
        let capabilities = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(
            capabilities,
            WaylandProviderCapabilities {
                idle_notifier_version: 2,
                seat_count: 1
            }
        );
        assert_eq!(trace.sync_requests, 2);
        assert_eq!(trace.sync_replies, 2);
        assert_eq!(trace.notification_requests, 1);
        assert_eq!(trace.notification_events, 2);
        worker.join().unwrap();
        drop(listener);
    }
}
