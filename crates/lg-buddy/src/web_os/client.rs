use super::registration::{
    parse_registration_message, WebOsRegistrationError, WebOsRegistrationEvent,
    WebOsRegistrationRequest,
};
use super::tls::webos_tv_self_signed_client_config;
use crate::platform_access_token::{
    PlatformAccessToken, PlatformAccessTokenAcquisitionError, PlatformAccessTokenStore,
};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{client_tls_with_config, Connector, HandshakeError, Message, WebSocket};

const WEBOS_WS_PORT: u16 = 3000;
const WEBOS_WSS_PORT: u16 = 3001;
const PAIRING_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

type WebOsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebOsTransport {
    Ws,
    Wss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOsEndpoint {
    address: SocketAddr,
    transport: WebOsTransport,
}

impl WebOsEndpoint {
    pub fn ws(ip: Ipv4Addr) -> Self {
        Self {
            address: SocketAddr::new(IpAddr::V4(ip), WEBOS_WS_PORT),
            transport: WebOsTransport::Ws,
        }
    }

    /// Uses encrypted WSS while accepting the TV's self-signed certificate.
    /// This protects traffic from passive observation but does not establish
    /// the TV's identity through a trusted certificate authority.
    pub fn wss(ip: Ipv4Addr) -> Self {
        Self::wss_at(SocketAddr::new(IpAddr::V4(ip), WEBOS_WSS_PORT))
    }

    #[doc(hidden)]
    pub fn ws_at(address: SocketAddr) -> Self {
        Self {
            address,
            transport: WebOsTransport::Ws,
        }
    }

    #[doc(hidden)]
    pub fn wss_at(address: SocketAddr) -> Self {
        Self {
            address,
            transport: WebOsTransport::Wss,
        }
    }

    fn url(self) -> String {
        let scheme = match self.transport {
            WebOsTransport::Ws => "ws",
            WebOsTransport::Wss => "wss",
        };
        format!("{scheme}://{}/", self.address)
    }
}

impl fmt::Display for WebOsEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url())
    }
}

pub struct WebOsClient {
    socket: WebOsSocket,
    next_request_sequence: u64,
    response_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsPairingEvent {
    WaitingForConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebOsPairingError {
    Cancelled,
    Rejected,
    Timeout,
    Failed,
}

/// Failure while a caller performs a cancellable typed read after pairing.
/// The protocol layer deliberately does not choose which reads constitute
/// verification; the application owns that policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsPairingReadError {
    Cancelled,
    Timeout,
    Failed,
}

impl fmt::Display for WebOsPairingReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "webOS pairing read was cancelled"),
            Self::Timeout => write!(f, "webOS pairing read timed out"),
            Self::Failed => write!(f, "webOS pairing read failed"),
        }
    }
}

impl Error for WebOsPairingReadError {}

impl fmt::Display for WebOsPairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "webOS pairing was cancelled"),
            Self::Rejected => write!(f, "webOS pairing was rejected"),
            Self::Timeout => write!(f, "webOS pairing timed out"),
            Self::Failed => write!(f, "webOS pairing failed"),
        }
    }
}

impl Error for WebOsPairingError {}

impl WebOsClient {
    /// Pairs a fresh native webOS client without reading or writing a token
    /// store. The returned token is owned by the caller and remains in memory
    /// until it is explicitly persisted by the surrounding profile workflow.
    pub fn pair_in_memory(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
        cancelled: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(WebOsPairingEvent),
    ) -> Result<(WebOsClient, PlatformAccessToken), WebOsPairingError> {
        ensure_pairing_uid_not_root(effective_uid())?;
        if cancelled() {
            return Err(WebOsPairingError::Cancelled);
        }

        let mut client = Self::connect(endpoint, connect_timeout, response_timeout)
            .map_err(|_source| WebOsPairingError::Failed)?;
        let pairing_deadline = Instant::now()
            .checked_add(response_timeout)
            .ok_or(WebOsPairingError::Failed)?;

        let token = client
            .registration()
            .register_for_pairing(pairing_deadline, cancelled, on_event)
            .map_err(pairing_registration_error)?;

        if cancelled() {
            return Err(WebOsPairingError::Cancelled);
        }

        Ok((client, token))
    }

    /// Connects and authenticates using the stored token only.
    ///
    /// This is the authentication path for background operations. It loads
    /// the token before connecting and never attempts pairing when the token
    /// is missing or stale.
    pub fn connect_authenticated_with_stored_token(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
        token_store: &PlatformAccessTokenStore,
    ) -> Result<Self, WebOsAuthenticatedClientError> {
        let token = token_store
            .load_stored()
            .map_err(|source| WebOsAuthenticatedClientError::Authentication { source })?;
        let mut client = Self::connect(endpoint, connect_timeout, response_timeout)
            .map_err(|source| WebOsAuthenticatedClientError::Connect { source })?;
        let mut registration = client.registration();
        registration
            .register(Some(&token), &mut |_| {})
            .map_err(|source| WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration { source },
            })?;

        Ok(client)
    }

    pub fn connect_authenticated<F>(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
        token_store: &PlatformAccessTokenStore,
        mut on_auth_event: F,
    ) -> Result<Self, WebOsAuthenticatedClientError>
    where
        F: FnMut(WebOsAuthenticationEvent),
    {
        let mut client = Self::connect(endpoint, connect_timeout, response_timeout)
            .map_err(|source| WebOsAuthenticatedClientError::Connect { source })?;
        let authentication = {
            let mut registration = client.registration();
            token_store.get_or_acquire(&mut registration, &mut on_auth_event)
        };

        match authentication {
            Ok(_) => Ok(client),
            Err(PlatformAccessTokenAcquisitionError::Registration {
                source: WebOsClientRegistrationError::StoredTokenRequiresPairing,
            }) => {
                // Foreground preflight may repair a stale credential, but it
                // must retire the rejected connection before pairing again.
                drop(client);
                let mut client = Self::connect(endpoint, connect_timeout, response_timeout)
                    .map_err(|source| WebOsAuthenticatedClientError::Connect { source })?;
                let mut registration = client.registration();
                token_store
                    .acquire_and_persist(&mut registration, &mut on_auth_event)
                    .map_err(|source| WebOsAuthenticatedClientError::Authentication { source })?;
                Ok(client)
            }
            Err(source) => Err(WebOsAuthenticatedClientError::Authentication { source }),
        }
    }

    fn connect(
        endpoint: WebOsEndpoint,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<Self, WebOsClientError> {
        if connect_timeout.is_zero() {
            return Err(WebOsClientError::InvalidTimeout { name: "connect" });
        }
        if response_timeout.is_zero() {
            return Err(WebOsClientError::InvalidTimeout { name: "response" });
        }

        let stream = TcpStream::connect_timeout(&endpoint.address, connect_timeout)
            .map_err(|source| WebOsClientError::Connect { endpoint, source })?;
        stream
            .set_nodelay(true)
            .and_then(|_| stream.set_read_timeout(Some(connect_timeout)))
            .and_then(|_| stream.set_write_timeout(Some(connect_timeout)))
            .map_err(|source| WebOsClientError::ConfigureSocket { source })?;

        let connector = match endpoint.transport {
            WebOsTransport::Ws => Connector::Plain,
            WebOsTransport::Wss => Connector::Rustls(webos_tv_self_signed_client_config()),
        };
        let (mut socket, _) =
            match client_tls_with_config(endpoint.url(), stream, None, Some(connector)) {
                Ok(connected) => connected,
                Err(HandshakeError::Failure(source)) => {
                    return Err(WebOsClientError::Handshake { source })
                }
                Err(HandshakeError::Interrupted(_)) => {
                    return Err(WebOsClientError::HandshakeInterrupted)
                }
            };
        set_read_timeout(&mut socket, response_timeout)
            .map_err(|source| WebOsClientError::ConfigureSocket { source })?;

        Ok(Self {
            socket,
            next_request_sequence: 0,
            response_timeout,
        })
    }

    fn registration(&mut self) -> WebOsClientRegistration<'_> {
        WebOsClientRegistration { client: self }
    }

    /// Reads the typed power state for application-owned pairing checks.
    pub fn power_state_with_cancellation(
        &mut self,
        response_timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<super::power::WebOsPowerState, WebOsPairingReadError> {
        self.pairing_read(
            super::power::GET_POWER_STATE_URI,
            json!({}),
            response_timeout,
            cancelled,
            super::power::parse_power_state_response,
        )
    }

    /// Reads the typed audio status for application-owned pairing checks.
    pub fn audio_status_with_cancellation(
        &mut self,
        response_timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<super::audio::WebOsAudioStatus, WebOsPairingReadError> {
        self.pairing_read(
            super::audio::GET_AUDIO_STATUS_URI,
            json!({}),
            response_timeout,
            cancelled,
            super::audio::parse_audio_status_response,
        )
    }

    /// Reads the typed OLED backlight brightness for application-owned
    /// pairing checks.
    pub fn backlight_brightness_with_cancellation(
        &mut self,
        response_timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<super::picture::WebOsBacklightBrightness, WebOsPairingReadError> {
        self.pairing_read(
            super::picture::GET_SYSTEM_SETTINGS_URI,
            json!({
                "category": "picture",
                "keys": ["backlight"],
            }),
            response_timeout,
            cancelled,
            super::picture::parse_backlight_brightness_response,
        )
    }

    fn pairing_read<T, E>(
        &mut self,
        uri: &str,
        payload: Value,
        response_timeout: Duration,
        cancelled: &dyn Fn() -> bool,
        parse: impl FnOnce(&Value) -> Result<T, E>,
    ) -> Result<T, WebOsPairingReadError> {
        if cancelled() {
            return Err(WebOsPairingReadError::Cancelled);
        }
        let response = self
            .send_pairing_request(uri, payload, response_timeout, cancelled)
            .map_err(pairing_read_transport_error)?;
        parse(&response).map_err(|_error| WebOsPairingReadError::Failed)
    }

    fn send_pairing_request(
        &mut self,
        uri: &str,
        payload: Value,
        response_timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, PairingTransportError> {
        if response_timeout.is_zero() {
            return Err(PairingTransportError::Failed);
        }
        set_write_timeout(&mut self.socket, response_timeout)
            .map_err(|_source| PairingTransportError::Failed)?;
        let request_id = self
            .next_request_id()
            .map_err(|_source| PairingTransportError::Failed)?;
        let request = json!({
            "id": request_id,
            "type": "request",
            "uri": uri,
            "payload": payload,
        });
        self.send_message(request)
            .map_err(|_source| PairingTransportError::Failed)?;
        let deadline = Instant::now()
            .checked_add(response_timeout)
            .ok_or(PairingTransportError::Failed)?;
        let response = self
            .receive_correlated_until(&request_id, deadline, cancelled)
            .map_err(pairing_receive_error)?;
        validate_response_message(response).map_err(pairing_transport_error_from_client)
    }

    pub(crate) fn send_request(
        &mut self,
        uri: &str,
        payload: Value,
    ) -> Result<Value, WebOsClientError> {
        let request_id = self.next_request_id()?;
        let request = json!({
            "id": request_id,
            "type": "request",
            "uri": uri,
            "payload": payload,
        });

        self.exchange(&request_id, request)
    }

    fn next_request_id(&mut self) -> Result<String, WebOsClientError> {
        let sequence = self.next_request_sequence;
        self.next_request_sequence = sequence
            .checked_add(1)
            .ok_or(WebOsClientError::RequestIdExhausted)?;
        Ok(format!("request_{sequence}"))
    }

    fn exchange(&mut self, request_id: &str, request: Value) -> Result<Value, WebOsClientError> {
        self.send_message(request)?;
        let deadline = self.response_deadline()?;
        let response = self.receive_correlated(request_id, deadline)?;
        validate_response_message(response)
    }

    fn send_message(&mut self, request: Value) -> Result<(), WebOsClientError> {
        self.socket
            .send(Message::text(request.to_string()))
            .map_err(|source| WebOsClientError::Send { source })
    }

    fn response_deadline(&self) -> Result<Instant, WebOsClientError> {
        Instant::now()
            .checked_add(self.response_timeout)
            .ok_or(WebOsClientError::InvalidTimeout { name: "response" })
    }

    fn receive_correlated(
        &mut self,
        request_id: &str,
        deadline: Instant,
    ) -> Result<Value, WebOsClientError> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| WebOsClientError::Timeout {
                    request_id: request_id.to_string(),
                })?;
            if remaining.is_zero() {
                return Err(WebOsClientError::Timeout {
                    request_id: request_id.to_string(),
                });
            }
            set_read_timeout(&mut self.socket, remaining)
                .map_err(|source| WebOsClientError::ConfigureSocket { source })?;

            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(source))
                    if matches!(
                        source.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(WebOsClientError::Timeout {
                        request_id: request_id.to_string(),
                    })
                }
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(WebOsClientError::ConnectionClosed {
                        request_id: request_id.to_string(),
                    })
                }
                Err(source) => return Err(WebOsClientError::Receive { source }),
            };

            match message {
                Message::Text(text) => {
                    if let Some(response) = parse_correlated_frame(request_id, text.as_str())? {
                        return Ok(response);
                    }
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => {
                    return Err(WebOsClientError::ConnectionClosed {
                        request_id: request_id.to_string(),
                    })
                }
                Message::Binary(_) => return Err(WebOsClientError::UnexpectedBinaryFrame),
                Message::Frame(_) => return Err(WebOsClientError::UnexpectedRawFrame),
            }
        }
    }

    fn receive_correlated_until(
        &mut self,
        request_id: &str,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, PairingReceiveError> {
        loop {
            if cancelled() {
                return Err(PairingReceiveError::Cancelled);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(PairingReceiveError::Timeout)?;
            if remaining.is_zero() {
                return Err(PairingReceiveError::Timeout);
            }
            set_read_timeout(
                &mut self.socket,
                remaining.min(PAIRING_CANCEL_POLL_INTERVAL),
            )
            .map_err(|source| {
                PairingReceiveError::Failed(WebOsClientError::ConfigureSocket { source })
            })?;

            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(source))
                    if matches!(
                        source.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue
                }
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(PairingReceiveError::Failed(
                        WebOsClientError::ConnectionClosed {
                            request_id: request_id.to_string(),
                        },
                    ))
                }
                Err(source) => {
                    return Err(PairingReceiveError::Failed(WebOsClientError::Receive {
                        source,
                    }))
                }
            };

            match message {
                Message::Text(text) => {
                    if let Some(response) = parse_correlated_frame(request_id, text.as_str())
                        .map_err(PairingReceiveError::Failed)?
                    {
                        return Ok(response);
                    }
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => {
                    return Err(PairingReceiveError::Failed(
                        WebOsClientError::ConnectionClosed {
                            request_id: request_id.to_string(),
                        },
                    ))
                }
                Message::Binary(_) => {
                    return Err(PairingReceiveError::Failed(
                        WebOsClientError::UnexpectedBinaryFrame,
                    ))
                }
                Message::Frame(_) => {
                    return Err(PairingReceiveError::Failed(
                        WebOsClientError::UnexpectedRawFrame,
                    ))
                }
            }
        }
    }
}

#[derive(Debug)]
enum PairingTransportError {
    Cancelled,
    Timeout,
    Failed,
}

#[derive(Debug)]
enum PairingReceiveError {
    Cancelled,
    Timeout,
    Failed(WebOsClientError),
}

fn pairing_receive_error(error: PairingReceiveError) -> PairingTransportError {
    match error {
        PairingReceiveError::Cancelled => PairingTransportError::Cancelled,
        PairingReceiveError::Timeout => PairingTransportError::Timeout,
        PairingReceiveError::Failed(_source) => PairingTransportError::Failed,
    }
}

fn pairing_transport_error_from_client(source: WebOsClientError) -> PairingTransportError {
    match source {
        WebOsClientError::Timeout { .. } => PairingTransportError::Timeout,
        _ => PairingTransportError::Failed,
    }
}

fn pairing_read_transport_error(error: PairingTransportError) -> WebOsPairingReadError {
    match error {
        PairingTransportError::Cancelled => WebOsPairingReadError::Cancelled,
        PairingTransportError::Timeout => WebOsPairingReadError::Timeout,
        PairingTransportError::Failed => WebOsPairingReadError::Failed,
    }
}

fn effective_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and does not mutate process state.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        1
    }
}

fn ensure_pairing_uid_not_root(uid: u32) -> Result<(), WebOsPairingError> {
    if uid == 0 {
        Err(WebOsPairingError::Failed)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOsAuthenticationEvent {
    UsingStoredAccessToken,
    PairingPrompt,
    AccessTokenPersisted,
}

pub(crate) struct WebOsClientRegistration<'client> {
    client: &'client mut WebOsClient,
}

impl WebOsClientRegistration<'_> {
    fn register_for_pairing(
        &mut self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(WebOsPairingEvent),
    ) -> Result<PlatformAccessToken, PairingRegistrationError> {
        if cancelled() {
            return Err(PairingRegistrationError::Cancelled);
        }
        let request_id = self
            .client
            .next_request_id()
            .map_err(|_source| PairingRegistrationError::Failed)?;
        let request = WebOsRegistrationRequest::new(&request_id, None)
            .map_err(|_source| PairingRegistrationError::Failed)?;
        self.client
            .send_message(request.to_json_value())
            .map_err(|_source| PairingRegistrationError::Failed)?;

        loop {
            let response = self
                .client
                .receive_correlated_until(&request_id, deadline, cancelled)
                .map_err(|error| match error {
                    PairingReceiveError::Cancelled => PairingRegistrationError::Cancelled,
                    PairingReceiveError::Timeout => PairingRegistrationError::Timeout,
                    PairingReceiveError::Failed(_source) => PairingRegistrationError::Failed,
                })?;
            let event = parse_registration_message(&request_id, &response.to_string()).map_err(
                |source| match source {
                    WebOsRegistrationError::PairingRejected { .. } => {
                        PairingRegistrationError::Rejected
                    }
                    _ => PairingRegistrationError::Failed,
                },
            )?;

            match event {
                WebOsRegistrationEvent::PairingPrompt => {
                    on_event(WebOsPairingEvent::WaitingForConfirmation);
                }
                WebOsRegistrationEvent::Registered { access_token } => return Ok(access_token),
            }
        }
    }

    pub(crate) fn register<F>(
        &mut self,
        access_token: Option<&PlatformAccessToken>,
        on_auth_event: &mut F,
    ) -> Result<PlatformAccessToken, WebOsClientRegistrationError>
    where
        F: FnMut(WebOsAuthenticationEvent),
    {
        let request_id = self
            .client
            .next_request_id()
            .map_err(|source| WebOsClientRegistrationError::Transport { source })?;
        let request = WebOsRegistrationRequest::new(&request_id, access_token)
            .map_err(|source| WebOsClientRegistrationError::Protocol { source })?;
        self.client
            .send_message(request.to_json_value())
            .map_err(|source| WebOsClientRegistrationError::Transport { source })?;
        let deadline = self
            .client
            .response_deadline()
            .map_err(|source| WebOsClientRegistrationError::Transport { source })?;

        loop {
            let response = self
                .client
                .receive_correlated(&request_id, deadline)
                .map_err(|source| WebOsClientRegistrationError::Transport { source })?;
            let event = parse_registration_message(&request_id, &response.to_string())
                .map_err(|source| WebOsClientRegistrationError::Protocol { source })?;

            match event {
                WebOsRegistrationEvent::PairingPrompt if access_token.is_some() => {
                    return Err(WebOsClientRegistrationError::StoredTokenRequiresPairing)
                }
                WebOsRegistrationEvent::PairingPrompt => {
                    on_auth_event(WebOsAuthenticationEvent::PairingPrompt)
                }
                WebOsRegistrationEvent::Registered { access_token } => return Ok(access_token),
            }
        }
    }
}

#[derive(Debug)]
enum PairingRegistrationError {
    Cancelled,
    Rejected,
    Timeout,
    Failed,
}

fn pairing_registration_error(error: PairingRegistrationError) -> WebOsPairingError {
    match error {
        PairingRegistrationError::Cancelled => WebOsPairingError::Cancelled,
        PairingRegistrationError::Rejected => WebOsPairingError::Rejected,
        PairingRegistrationError::Timeout => WebOsPairingError::Timeout,
        PairingRegistrationError::Failed => WebOsPairingError::Failed,
    }
}

#[derive(Debug)]
pub enum WebOsAuthenticatedClientError {
    Connect {
        source: WebOsClientError,
    },
    Authentication {
        source: PlatformAccessTokenAcquisitionError,
    },
}

impl fmt::Display for WebOsAuthenticatedClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { source } => write!(f, "could not connect webOS client: {source}"),
            Self::Authentication { source } => {
                write!(f, "could not authenticate webOS client: {source}")
            }
        }
    }
}

impl Error for WebOsAuthenticatedClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Connect { source } => Some(source),
            Self::Authentication { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub enum WebOsClientRegistrationError {
    Transport { source: WebOsClientError },
    Protocol { source: WebOsRegistrationError },
    StoredTokenRequiresPairing,
}

impl fmt::Display for WebOsClientRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { source } => {
                write!(f, "webOS registration transport failed: {source}")
            }
            Self::Protocol { source } => write!(f, "webOS registration failed: {source}"),
            Self::StoredTokenRequiresPairing => {
                write!(
                    f,
                    "webOS rejected the stored access token and requires pairing"
                )
            }
        }
    }
}

impl Error for WebOsClientRegistrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport { source } => Some(source),
            Self::Protocol { source } => Some(source),
            Self::StoredTokenRequiresPairing => None,
        }
    }
}

#[derive(Debug)]
pub enum WebOsClientError {
    InvalidTimeout {
        name: &'static str,
    },
    Connect {
        endpoint: WebOsEndpoint,
        source: io::Error,
    },
    ConfigureSocket {
        source: io::Error,
    },
    Handshake {
        source: tungstenite::Error,
    },
    HandshakeInterrupted,
    RequestIdExhausted,
    Send {
        source: tungstenite::Error,
    },
    Timeout {
        request_id: String,
    },
    ConnectionClosed {
        request_id: String,
    },
    Receive {
        source: tungstenite::Error,
    },
    MalformedJson {
        source: serde_json::Error,
    },
    InvalidFrameRoot,
    MissingResponseId,
    InvalidResponseId,
    MissingMessageType,
    InvalidMessageType,
    MissingWebOsErrorMessage,
    InvalidWebOsErrorMessage,
    WebOs {
        code: Option<i32>,
        message: String,
        payload: Option<Value>,
    },
    UnexpectedBinaryFrame,
    UnexpectedRawFrame,
}

impl fmt::Display for WebOsClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimeout { name } => {
                write!(f, "webOS {name} timeout must be greater than zero")
            }
            Self::Connect { endpoint, source } => {
                write!(
                    f,
                    "could not connect to webOS endpoint `{endpoint}`: {source}"
                )
            }
            Self::ConfigureSocket { source } => {
                write!(f, "could not configure webOS socket: {source}")
            }
            Self::Handshake { source } => write!(f, "webOS websocket handshake failed: {source}"),
            Self::HandshakeInterrupted => {
                write!(f, "webOS websocket handshake was interrupted")
            }
            Self::RequestIdExhausted => write!(f, "webOS request ID sequence is exhausted"),
            Self::Send { source } => write!(f, "could not send webOS request: {source}"),
            Self::Timeout { request_id } => {
                write!(f, "timed out waiting for webOS response `{request_id}`")
            }
            Self::ConnectionClosed { request_id } => write!(
                f,
                "webOS connection closed before response `{request_id}` arrived"
            ),
            Self::Receive { source } => write!(f, "could not receive webOS response: {source}"),
            Self::MalformedJson { source } => {
                write!(f, "webOS response is malformed JSON: {source}")
            }
            Self::InvalidFrameRoot => write!(f, "webOS response root is not an object"),
            Self::MissingResponseId => write!(f, "webOS response has no request ID"),
            Self::InvalidResponseId => write!(f, "webOS response request ID is not a string"),
            Self::MissingMessageType => write!(f, "webOS response has no message type"),
            Self::InvalidMessageType => write!(f, "webOS response message type is not a string"),
            Self::MissingWebOsErrorMessage => {
                write!(f, "webOS error response has no error message")
            }
            Self::InvalidWebOsErrorMessage => {
                write!(f, "webOS error response message is not a string")
            }
            Self::WebOs {
                code: Some(code),
                message,
                ..
            } => write!(f, "webOS error {code}: {message}"),
            Self::WebOs {
                code: None,
                message,
                ..
            } => write!(f, "webOS error: {message}"),
            Self::UnexpectedBinaryFrame => write!(f, "webOS sent an unexpected binary frame"),
            Self::UnexpectedRawFrame => write!(f, "webOS sent an unexpected raw frame"),
        }
    }
}

impl Error for WebOsClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Connect { source, .. } | Self::ConfigureSocket { source } => Some(source),
            Self::Handshake { source } | Self::Send { source } | Self::Receive { source } => {
                Some(source)
            }
            Self::MalformedJson { source } => Some(source),
            Self::InvalidTimeout { .. }
            | Self::HandshakeInterrupted
            | Self::RequestIdExhausted
            | Self::Timeout { .. }
            | Self::ConnectionClosed { .. }
            | Self::InvalidFrameRoot
            | Self::MissingResponseId
            | Self::InvalidResponseId
            | Self::MissingMessageType
            | Self::InvalidMessageType
            | Self::MissingWebOsErrorMessage
            | Self::InvalidWebOsErrorMessage
            | Self::WebOs { .. }
            | Self::UnexpectedBinaryFrame
            | Self::UnexpectedRawFrame => None,
        }
    }
}

fn set_read_timeout(socket: &mut WebOsSocket, timeout: Duration) -> io::Result<()> {
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported webOS TLS stream",
            ))
        }
    };
    stream.set_read_timeout(Some(timeout))
}

fn set_write_timeout(socket: &mut WebOsSocket, timeout: Duration) -> io::Result<()> {
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported webOS TLS stream",
            ))
        }
    };
    stream.set_write_timeout(Some(timeout))
}

fn parse_correlated_frame(
    expected_request_id: &str,
    raw_message: &str,
) -> Result<Option<Value>, WebOsClientError> {
    let message: Value = serde_json::from_str(raw_message)
        .map_err(|source| WebOsClientError::MalformedJson { source })?;
    let object = message
        .as_object()
        .ok_or(WebOsClientError::InvalidFrameRoot)?;
    let actual_request_id = match object.get("id") {
        Some(Value::String(request_id)) => request_id,
        Some(_) => return Err(WebOsClientError::InvalidResponseId),
        None => return Err(WebOsClientError::MissingResponseId),
    };
    if actual_request_id != expected_request_id {
        return Ok(None);
    }

    Ok(Some(message))
}

fn validate_response_message(message: Value) -> Result<Value, WebOsClientError> {
    let object = message
        .as_object()
        .expect("correlated websocket message root was already validated");
    let message_type = match object.get("type") {
        Some(Value::String(message_type)) => message_type,
        Some(_) => return Err(WebOsClientError::InvalidMessageType),
        None => return Err(WebOsClientError::MissingMessageType),
    };
    if message_type == "error" {
        return Err(parse_webos_error(object));
    }

    Ok(message)
}

fn parse_webos_error(message: &serde_json::Map<String, Value>) -> WebOsClientError {
    let payload = message.get("payload").cloned();
    let error = match message.get("error") {
        Some(Value::String(error)) => error,
        Some(_) => return WebOsClientError::InvalidWebOsErrorMessage,
        None => return WebOsClientError::MissingWebOsErrorMessage,
    };
    let mut parts = error.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or_default();
    let (code, message) = match first.parse::<i32>() {
        Ok(code) => (
            Some(code),
            parts.next().unwrap_or_default().trim().to_string(),
        ),
        Err(_) => (None, error.to_string()),
    };
    WebOsClientError::WebOs {
        code,
        message,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WebOsAuthenticatedClientError, WebOsAuthenticationEvent, WebOsClient, WebOsClientError,
        WebOsClientRegistrationError, WebOsEndpoint, WebOsPairingError, WebOsPairingEvent,
        WebOsPairingReadError,
    };
    use crate::auth::SystemUser;
    use crate::platform_access_token::{
        PlatformAccessToken, PlatformAccessTokenAcquisitionError, PlatformAccessTokenStore,
        PlatformAccessTokenStoreError, PlatformAccessTokenStoreOperation,
    };
    use crate::web_os::test_support::{
        WebOsTestInput, WebOsTestScenario, WebOsTestServer, WebOsTestVersion,
    };
    use crate::web_os::{WebOsAudioVolume, WebOsPowerStateError, WebOsRegistrationError};
    use serde_json::json;
    use std::fs;
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
    const RESPONSE_TIMEOUT: Duration = Duration::from_millis(200);
    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-web-os-client-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn token(value: &str) -> PlatformAccessToken {
        PlatformAccessToken::new(value).expect("valid platform access token")
    }

    fn token_store(dir: &TestDir) -> PlatformAccessTokenStore {
        #[cfg(unix)]
        let owner = SystemUser::new(
            "test-user",
            unsafe { libc::geteuid() },
            unsafe { libc::getegid() },
            dir.path(),
        );
        #[cfg(not(unix))]
        let owner = SystemUser::new("test-user", 0, 0, dir.path());

        PlatformAccessTokenStore::for_primary_profile(&dir.path().join("config.env"), owner)
            .expect("derive test token store")
    }

    fn pairing_error(
        result: Result<(WebOsClient, PlatformAccessToken), WebOsPairingError>,
    ) -> WebOsPairingError {
        match result {
            Ok(_) => panic!("pairing unexpectedly succeeded"),
            Err(error) => error,
        }
    }

    #[test]
    fn pair_in_memory_accepts_without_choosing_verification_checks() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StatefulTv,
        );
        let mut events = Vec::new();
        let (mut client, access_token) = WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &|| false,
            &mut |event| events.push(event),
        )
        .expect("pair native webOS client");

        assert_eq!(access_token.as_secret_str(), "webos-test-access-token");
        assert_eq!(events, vec![WebOsPairingEvent::WaitingForConfirmation,]);
        assert_eq!(
            server.snapshot().registration_tokens,
            vec![None],
            "pairing must register without a stored client key"
        );
        assert_eq!(
            client
                .power_state_with_cancellation(RESPONSE_TIMEOUT, &|| false)
                .expect("returned client remains authenticated"),
            super::super::WebOsPowerState::Active
        );
        assert_eq!(
            client
                .audio_status_with_cancellation(RESPONSE_TIMEOUT, &|| false)
                .expect("typed audio read")
                .volume(),
            WebOsAudioVolume::Known(20)
        );
        assert_eq!(
            client
                .backlight_brightness_with_cancellation(RESPONSE_TIMEOUT, &|| false)
                .expect("typed backlight read")
                .as_percent(),
            100
        );
        drop(client);
        server.finish();
    }

    #[test]
    fn pair_in_memory_does_not_verify_capabilities_before_returning() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::PowerStatePermissionDenied,
        );
        let result = WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &|| false,
            &mut |_| {},
        );

        // This scenario rejects the first power read. A successful pair proves
        // that capability verification starts only when the caller asks.
        assert!(result.is_ok());
        server.finish();
    }

    #[test]
    fn pair_in_memory_reports_rejection_without_exposing_credentials() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::PairingRejected,
        );
        let error = pairing_error(WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &|| false,
            &mut |_| {},
        ));

        assert_eq!(error, WebOsPairingError::Rejected);
        assert!(!format!("{error:?}").contains("webos-test-access-token"));
        server.finish();
    }

    #[test]
    fn pair_in_memory_times_out_waiting_for_confirmation() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::RegistrationTimeout,
        );
        let error = pairing_error(WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            Duration::from_millis(80),
            &|| false,
            &mut |_| {},
        ));

        assert_eq!(error, WebOsPairingError::Timeout);
        server.finish();
    }

    #[test]
    fn pair_in_memory_cancellation_interrupts_prompt_wait() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::RegistrationTimeout,
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_after = Arc::clone(&cancelled);
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            cancel_after.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let error = pairing_error(WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            Duration::from_secs(2),
            &|| cancelled.load(Ordering::Acquire),
            &mut |_| {},
        ));
        let elapsed = started.elapsed();
        setter.join().expect("cancellation setter");

        assert_eq!(error, WebOsPairingError::Cancelled);
        assert!(
            elapsed < Duration::from_millis(500),
            "cancel took {elapsed:?}"
        );
        server.finish();
    }

    #[test]
    fn cancelling_during_connection_does_not_send_a_pairing_request() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StatefulTv,
        );
        let checks = std::cell::Cell::new(0);
        let error = pairing_error(WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &|| {
                let previous = checks.get();
                checks.set(previous + 1);
                previous > 0
            },
            &mut |_| panic!("a cancelled connection must not begin pairing"),
        ));
        assert_eq!(error, WebOsPairingError::Cancelled);
        assert!(server.snapshot().registration_tokens.is_empty());
        server.finish();
    }

    #[test]
    fn pair_in_memory_cancellation_from_prompt_event_skips_verification() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StatefulTv,
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_on_prompt = Arc::clone(&cancelled);
        let mut events = Vec::new();
        let error = pairing_error(WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &|| cancelled.load(Ordering::Acquire),
            &mut |event| {
                events.push(event);
                if event == WebOsPairingEvent::WaitingForConfirmation {
                    cancel_on_prompt.store(true, Ordering::Release);
                }
            },
        ));

        assert_eq!(error, WebOsPairingError::Cancelled);
        assert_eq!(events, vec![WebOsPairingEvent::WaitingForConfirmation]);
        server.finish();
    }

    #[test]
    fn cancellable_typed_pairing_read_reports_protocol_failure() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::PowerStatePermissionDenied,
        );
        let (mut client, _) = WebOsClient::pair_in_memory(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &|| false,
            &mut |_| {},
        )
        .expect("pairing should finish before capability checks");

        assert_eq!(
            client.power_state_with_cancellation(RESPONSE_TIMEOUT, &|| false),
            Err(WebOsPairingReadError::Failed)
        );
        server.finish();
    }

    #[test]
    fn pairing_root_guard_rejects_before_network() {
        assert_eq!(
            super::ensure_pairing_uid_not_root(0),
            Err(WebOsPairingError::Failed)
        );
        assert!(super::ensure_pairing_uid_not_root(1000).is_ok());
    }

    #[test]
    fn requests_use_stable_ids_and_return_the_matching_response() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::ProtocolEcho,
        );
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        for sequence in 0..2 {
            let response = client
                .send_request(&format!("ssap://test/{sequence}"), json!({}))
                .expect("matching response");
            assert_eq!(response["payload"]["sequence"], sequence);
        }
        server.finish();
    }

    #[test]
    fn unrelated_frame_is_ignored_before_matching_response() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::UnrelatedFrameBeforeResponse,
        );
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        let response = client
            .send_request("ssap://test/correlated", json!({}))
            .expect("matching response");
        assert_eq!(response["payload"]["ok"], true);
        server.finish();
    }

    #[test]
    fn wrong_response_id_is_not_accepted_and_deadline_is_absolute() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::WrongResponseId,
        );
        let mut client = WebOsClient::connect(
            server.endpoint(),
            CONNECT_TIMEOUT,
            Duration::from_millis(40),
        )
        .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/wrong-id", json!({})),
            Err(WebOsClientError::Timeout { request_id }) if request_id == "request_0"
        ));
        server.finish();
    }

    #[test]
    fn close_before_matching_response_is_typed() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::CloseBeforeResponse,
        );
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/close", json!({})),
            Err(WebOsClientError::ConnectionClosed { request_id })
                if request_id == "request_0"
        ));
        server.finish();
    }

    #[test]
    fn malformed_text_frame_is_typed() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::MalformedTextFrame,
        );
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/malformed", json!({})),
            Err(WebOsClientError::MalformedJson { .. })
        ));
        server.finish();
    }

    #[test]
    fn webos_error_preserves_code_and_message() {
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::WebOsError,
        );
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect client");

        assert!(matches!(
            client.send_request("ssap://test/error", json!({})),
            Err(WebOsClientError::WebOs {
                code: Some(-401),
                message,
                payload: None,
            }) if message == "not permitted"
        ));
        server.finish();
    }

    #[test]
    fn connection_failure_is_typed() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve address");
        let endpoint = WebOsEndpoint::ws_at(listener.local_addr().expect("reserved address"));
        drop(listener);

        assert!(matches!(
            WebOsClient::connect(endpoint, CONNECT_TIMEOUT, RESPONSE_TIMEOUT),
            Err(WebOsClientError::Connect {
                endpoint: failed_endpoint,
                ..
            }) if failed_endpoint == endpoint
        ));
    }

    #[test]
    fn wss_accepts_self_signed_tv_certificate() {
        let server = WebOsTestServer::for_tls_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::ProtocolEcho,
        );
        let mut client = WebOsClient::connect(server.endpoint(), CONNECT_TIMEOUT, RESPONSE_TIMEOUT)
            .expect("connect secure client");

        let response = client
            .send_request("ssap://test/wss", json!({}))
            .expect("secure response");
        assert_eq!(response["payload"]["encrypted"], true);
        server.finish();
    }

    // Synthetic fault injection: the observed TV returned the stored token unchanged.
    #[test]
    fn stored_token_registration_does_not_persist_a_replacement_from_the_response() {
        let dir = TestDir::new("stored-token");
        let store = token_store(&dir);
        let original = token("stored-client-key");
        store.persist(&original).expect("persist stored token");
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StoredTokenReplacement,
        );
        let mut events = Vec::new();

        let _client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| events.push(event),
        )
        .expect("authenticate with stored token");

        assert_eq!(
            events,
            vec![WebOsAuthenticationEvent::UsingStoredAccessToken]
        );
        assert_eq!(store.load().expect("reload stored token"), Some(original));
        server.finish();
    }

    #[test]
    fn stored_token_authentication_requires_a_credential_without_pairing() {
        let dir = TestDir::new("missing-runtime-token");
        let store = token_store(&dir);
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);

        let result = WebOsClient::connect_authenticated_with_stored_token(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::MissingStoredToken,
            })
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    // Real-TV first-pairing acceptance target:
    // https://github.com/Staphylococcus/LG_Buddy/issues/47
    #[test]
    fn authenticated_client_pairs_and_persists_new_token() {
        let dir = TestDir::new("new-token");
        let store = token_store(&dir);
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);
        let mut events = Vec::new();

        let client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| events.push(event),
        )
        .expect("pair and authenticate client");

        assert_eq!(
            events,
            vec![
                WebOsAuthenticationEvent::PairingPrompt,
                WebOsAuthenticationEvent::AccessTokenPersisted,
            ]
        );
        assert_eq!(
            store.load().expect("load acquired token"),
            Some(server.access_token())
        );
        drop(client);
        server.finish();
    }

    #[test]
    fn foreground_authentication_repairs_rejected_stored_token() {
        let dir = TestDir::new("rejected-stored-token");
        let store = token_store(&dir);
        let original = token("rejected-client-key");
        store.persist(&original).expect("persist stored token");
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StoredTokenPairingPrompt,
        );
        let mut events = Vec::new();

        let _client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| events.push(event),
        )
        .expect("foreground authentication should repair a stale token");

        assert_eq!(
            events,
            vec![
                WebOsAuthenticationEvent::UsingStoredAccessToken,
                WebOsAuthenticationEvent::PairingPrompt,
                WebOsAuthenticationEvent::AccessTokenPersisted,
            ]
        );
        assert_eq!(
            store.load().expect("reload stored token"),
            Some(server.access_token())
        );
        assert_ne!(store.load().expect("reload stored token"), Some(original));
        assert_eq!(server.snapshot().connection_count, 2);
        server.finish();
    }

    #[test]
    fn stored_token_runtime_authentication_rejects_pairing_prompt() {
        let dir = TestDir::new("rejected-runtime-token");
        let store = token_store(&dir);
        let original = token("rejected-runtime-client-key");
        store.persist(&original).expect("persist stored token");
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::StoredTokenPairingPrompt,
        );

        let result = WebOsClient::connect_authenticated_with_stored_token(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::StoredTokenRequiresPairing,
                },
            })
        ));
        assert_eq!(store.load().expect("reload stored token"), Some(original));
        server.finish();
    }

    #[test]
    fn pairing_rejection_does_not_create_token() {
        let dir = TestDir::new("pairing-rejected");
        let store = token_store(&dir);
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::PairingRejected,
        );

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::Protocol {
                        source: WebOsRegistrationError::PairingRejected {
                            message: Some(message),
                        },
                    },
                },
            }) if message == "pairing denied"
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    #[test]
    fn registration_timeout_does_not_create_token() {
        let dir = TestDir::new("registration-timeout");
        let store = token_store(&dir);
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::RegistrationTimeout,
        );

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            Duration::from_millis(40),
            &store,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::Transport {
                        source: WebOsClientError::Timeout { .. },
                    },
                },
            })
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    #[test]
    fn malformed_registration_does_not_create_token() {
        let dir = TestDir::new("malformed-registration");
        let store = token_store(&dir);
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::RegistrationMissingClientKey,
        );

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |_| {},
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Registration {
                    source: WebOsClientRegistrationError::Protocol {
                        source: WebOsRegistrationError::MissingClientKey,
                    },
                },
            })
        ));
        assert!(!store.token_path().exists());
        server.finish();
    }

    #[test]
    fn persistence_failure_does_not_return_authenticated_client() {
        let dir = TestDir::new("persistence-failure");
        let store = token_store(&dir);
        let token_path = store.token_path().to_path_buf();
        let server =
            WebOsTestServer::active(WebOsTestVersion::WebOs24Version92261, WebOsTestInput::Hdmi3);

        let result = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| {
                if event == WebOsAuthenticationEvent::PairingPrompt {
                    fs::create_dir_all(&token_path).expect("block token file replacement");
                }
            },
        );

        assert!(matches!(
            result,
            Err(WebOsAuthenticatedClientError::Authentication {
                source: PlatformAccessTokenAcquisitionError::Store {
                    source: PlatformAccessTokenStoreError::Io {
                        operation: PlatformAccessTokenStoreOperation::ReplaceToken,
                        ..
                    },
                },
            })
        ));
        assert!(store.token_path().is_dir());
        server.finish();
    }

    #[test]
    fn power_state_preserves_webos_error() {
        let dir = TestDir::new("power-state-error");
        let store = token_store(&dir);
        store
            .persist(&token("power-state-error-key"))
            .expect("persist stored token");
        let server = WebOsTestServer::for_scenario(
            WebOsTestVersion::WebOs24Version92261,
            WebOsTestScenario::PowerStatePermissionDenied,
        );
        let mut client = WebOsClient::connect_authenticated(
            server.endpoint(),
            CONNECT_TIMEOUT,
            RESPONSE_TIMEOUT,
            &store,
            |event| assert_eq!(event, WebOsAuthenticationEvent::UsingStoredAccessToken),
        )
        .expect("authenticate power-state client");

        assert!(matches!(
            client.power_state(),
            Err(WebOsPowerStateError::Request {
                source: WebOsClientError::WebOs {
                    code: Some(-401),
                    message,
                    payload: None,
                },
            }) if message == "not permitted"
        ));
        server.finish();
    }
}
