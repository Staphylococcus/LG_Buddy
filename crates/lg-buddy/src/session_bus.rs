use dbus::arg::messageitem::MessageItem as DbusMessageItem;
use dbus::blocking::{BlockingSender as DbusBlockingSender, Connection as DbusConnection};
use dbus::message::{MatchRule as DbusMatchRule, MessageType as DbusMessageType};
use dbus::Message as DbusMessage;
use std::fmt;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

const SESSION_BUS_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DBUS_METHOD_CALL_TIMEOUT: Duration = Duration::from_secs(1);
pub const DBUS_SERVICE_NAME: &str = "org.freedesktop.DBus";
pub const DBUS_OBJECT_PATH: &str = "/org/freedesktop/DBus";
pub const DBUS_INTERFACE: &str = "org.freedesktop.DBus";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBusError {
    Transport(String),
    Timeout {
        name: String,
        timeout: Duration,
    },
    UnexpectedReplyShape {
        expected: &'static str,
        actual: &'static str,
    },
    UnsupportedMessageBody {
        context: &'static str,
        kind: &'static str,
    },
}

impl fmt::Display for SessionBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "{message}"),
            Self::Timeout { name, timeout } => {
                write!(
                    f,
                    "timed out waiting for bus name `{name}` after {timeout:?}"
                )
            }
            Self::UnexpectedReplyShape { expected, actual } => {
                write!(
                    f,
                    "unexpected bus reply shape: expected {expected}, got {actual}"
                )
            }
            Self::UnsupportedMessageBody { context, kind } => {
                write!(f, "unsupported D-Bus {context}: {kind}")
            }
        }
    }
}

impl std::error::Error for SessionBusError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusValue {
    Bool(bool),
    UnixFd(RawFd),
    U32(u32),
    U64(u64),
    String(String),
    ObjectPath(String),
    Array(Vec<BusValue>),
    Struct(Vec<BusValue>),
    Dict(Vec<(BusValue, BusValue)>),
    Variant(Box<BusValue>),
}

impl BusValue {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::UnixFd(_) => "fd",
            Self::U32(_) => "u32",
            Self::U64(_) => "u64",
            Self::String(_) => "string",
            Self::ObjectPath(_) => "object path",
            Self::Array(_) => "array",
            Self::Struct(_) => "struct",
            Self::Dict(_) => "dict",
            Self::Variant(_) => "variant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusReply {
    pub body: Vec<BusValue>,
}

impl BusReply {
    pub fn new(body: Vec<BusValue>) -> Self {
        Self { body }
    }

    pub fn single_bool(&self) -> Result<bool, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::Bool(value)] => Ok(*value),
            [BusValue::Variant(value)] => match value.as_ref() {
                BusValue::Bool(value) => Ok(*value),
                value => Err(SessionBusError::UnexpectedReplyShape {
                    expected: "single bool",
                    actual: value.kind(),
                }),
            },
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_u64(&self) -> Result<u64, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::U64(value)] => Ok(*value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u64",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u64",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_u32(&self) -> Result<u32, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::U32(value)] => Ok(*value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u32",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single u32",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_unix_fd(self) -> Result<OwnedFd, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::UnixFd(fd)] => {
                let fd = *fd;
                // SAFETY: D-Bus transferred ownership of this descriptor into the
                // reply. `BusValue` does not close raw descriptors on drop, so
                // this creates the single owned handle responsible for closing it.
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            }
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single fd",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single fd",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_string(&self) -> Result<&str, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::String(value)] => Ok(value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single string",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single string",
                actual: "multiple values",
            }),
        }
    }

    pub fn single_object_path(&self) -> Result<&str, SessionBusError> {
        match self.body.as_slice() {
            [BusValue::ObjectPath(value)] => Ok(value),
            [value] => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single object path",
                actual: value.kind(),
            }),
            _ => Err(SessionBusError::UnexpectedReplyShape {
                expected: "single object path",
                actual: "multiple values",
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusMethodCall<'a> {
    pub destination: &'a str,
    pub path: &'a str,
    pub interface: &'a str,
    pub member: &'a str,
    pub body: Vec<BusValue>,
}

impl<'a> BusMethodCall<'a> {
    pub fn new(destination: &'a str, path: &'a str, interface: &'a str, member: &'a str) -> Self {
        Self {
            destination,
            path,
            interface,
            member,
            body: Vec::new(),
        }
    }

    pub fn with_body(mut self, body: Vec<BusValue>) -> Self {
        self.body = body;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusSignalMatch<'a> {
    pub sender: Option<&'a str>,
    pub path: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub member: Option<&'a str>,
}

impl<'a> BusSignalMatch<'a> {
    pub fn matches(&self, signal: &BusSignal) -> bool {
        if let Some(sender) = self.sender {
            if signal.sender.as_deref() != Some(sender) {
                return false;
            }
        }

        if let Some(path) = self.path {
            if signal.path != path {
                return false;
            }
        }

        if let Some(interface) = self.interface {
            if signal.interface != interface {
                return false;
            }
        }

        if let Some(member) = self.member {
            if signal.member != member {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedBusSignalMatch {
    sender: Option<String>,
    path: Option<String>,
    interface: Option<String>,
    member: Option<String>,
}

impl<'a> From<BusSignalMatch<'a>> for OwnedBusSignalMatch {
    fn from(value: BusSignalMatch<'a>) -> Self {
        Self {
            sender: value.sender.map(ToOwned::to_owned),
            path: value.path.map(ToOwned::to_owned),
            interface: value.interface.map(ToOwned::to_owned),
            member: value.member.map(ToOwned::to_owned),
        }
    }
}

impl OwnedBusSignalMatch {
    fn as_match_rule(&self) -> DbusMatchRule<'static> {
        let mut rule = DbusMatchRule::new().with_type(DbusMessageType::Signal);
        if let Some(sender) = &self.sender {
            rule = rule.with_sender(sender.clone());
        }
        if let Some(path) = &self.path {
            rule = rule.with_path(path.clone());
        }
        if let Some(interface) = &self.interface {
            rule = rule.with_interface(interface.clone());
        }
        if let Some(member) = &self.member {
            rule = rule.with_member(member.clone());
        }
        rule
    }

    fn matches(&self, signal: &BusSignal) -> bool {
        if let Some(sender) = &self.sender {
            if signal.sender.as_ref() != Some(sender) {
                return false;
            }
        }

        if let Some(path) = &self.path {
            if &signal.path != path {
                return false;
            }
        }

        if let Some(interface) = &self.interface {
            if &signal.interface != interface {
                return false;
            }
        }

        if let Some(member) = &self.member {
            if &signal.member != member {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusSignal {
    pub sender: Option<String>,
    pub path: String,
    pub interface: String,
    pub member: String,
    pub body: Vec<BusValue>,
}

impl BusSignal {
    pub fn new(
        path: impl Into<String>,
        interface: impl Into<String>,
        member: impl Into<String>,
    ) -> Self {
        Self {
            sender: None,
            path: path.into(),
            interface: interface.into(),
            member: member.into(),
            body: Vec::new(),
        }
    }

    pub fn with_sender(mut self, sender: impl Into<String>) -> Self {
        self.sender = Some(sender.into());
        self
    }

    pub fn with_body(mut self, body: Vec<BusValue>) -> Self {
        self.body = body;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameOwnerChanged {
    pub name: String,
    pub old_owner: Option<String>,
    pub new_owner: Option<String>,
}

pub fn get_name_owner(
    bus: &mut impl SessionBusClient,
    name: &str,
) -> Result<String, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            DBUS_SERVICE_NAME,
            DBUS_OBJECT_PATH,
            DBUS_INTERFACE,
            "GetNameOwner",
        )
        .with_body(vec![BusValue::String(name.to_string())]),
    )?
    .single_string()
    .map(str::to_owned)
}

pub fn parse_name_owner_changed_signal(signal: &BusSignal) -> Option<NameOwnerChanged> {
    if signal.path != DBUS_OBJECT_PATH
        || signal.interface != DBUS_INTERFACE
        || signal.member != "NameOwnerChanged"
    {
        return None;
    }

    let [BusValue::String(name), BusValue::String(old_owner), BusValue::String(new_owner)] =
        signal.body.as_slice()
    else {
        return None;
    };

    Some(NameOwnerChanged {
        name: name.clone(),
        old_owner: normalize_dbus_owner(old_owner),
        new_owner: normalize_dbus_owner(new_owner),
    })
}

fn normalize_dbus_owner(owner: &str) -> Option<String> {
    if owner.is_empty() {
        None
    } else {
        Some(owner.to_string())
    }
}

pub trait SessionBusClient {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError>;
    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError>;
    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError>;
    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError>;

    fn wait_for_name(&mut self, name: &str, timeout: Duration) -> Result<(), SessionBusError> {
        let started = Instant::now();
        loop {
            if self.name_has_owner(name)? {
                return Ok(());
            }

            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Err(SessionBusError::Timeout {
                    name: name.to_string(),
                    timeout,
                });
            }

            let remaining = timeout.saturating_sub(elapsed);
            let poll_timeout = remaining.min(SESSION_BUS_WAIT_POLL_INTERVAL);
            let _ = self.process(poll_timeout)?;
        }
    }
}

impl<T: SessionBusClient + ?Sized> SessionBusClient for Box<T> {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
        (**self).name_has_owner(name)
    }

    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
        (**self).call_method(call)
    }

    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
        (**self).add_signal_match(rule)
    }

    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
        (**self).process(timeout)
    }

    fn wait_for_name(&mut self, name: &str, timeout: Duration) -> Result<(), SessionBusError> {
        (**self).wait_for_name(name, timeout)
    }
}

pub fn new_session_bus_client() -> Result<Box<dyn SessionBusClient + Send>, SessionBusError> {
    Ok(Box::new(DbusSessionBusClient::new_session()?))
}

pub fn new_system_bus_client() -> Result<Box<dyn SessionBusClient + Send>, SessionBusError> {
    Ok(Box::new(DbusSessionBusClient::new_system()?))
}

pub struct DbusSessionBusClient {
    connection: DbusConnection,
    method_call_timeout: Duration,
    signal_rules: Vec<OwnedBusSignalMatch>,
}

impl DbusSessionBusClient {
    pub fn new_session() -> Result<Self, SessionBusError> {
        Ok(Self {
            connection: DbusConnection::new_session()
                .map_err(|err| SessionBusError::Transport(err.to_string()))?,
            method_call_timeout: DBUS_METHOD_CALL_TIMEOUT,
            signal_rules: Vec::new(),
        })
    }

    pub fn new_system() -> Result<Self, SessionBusError> {
        Ok(Self {
            connection: DbusConnection::new_system()
                .map_err(|err| SessionBusError::Transport(err.to_string()))?,
            method_call_timeout: DBUS_METHOD_CALL_TIMEOUT,
            signal_rules: Vec::new(),
        })
    }
}

impl SessionBusClient for DbusSessionBusClient {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
        self.call_method(
            BusMethodCall::new(
                DBUS_SERVICE_NAME,
                DBUS_OBJECT_PATH,
                DBUS_INTERFACE,
                "NameHasOwner",
            )
            .with_body(vec![BusValue::String(name.to_string())]),
        )?
        .single_bool()
    }

    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
        let mut message =
            DbusMessage::new_method_call(call.destination, call.path, call.interface, call.member)
                .map_err(SessionBusError::Transport)?;
        for value in call.body {
            message = append_dbus_message_value(message, value)?;
        }

        let reply = DbusBlockingSender::send_with_reply_and_block(
            &self.connection,
            message,
            self.method_call_timeout,
        )
        .map_err(|err| SessionBusError::Transport(err.to_string()))?;

        Ok(BusReply::new(
            reply
                .get_items()
                .into_iter()
                .map(bus_value_from_dbus_message_item)
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
        let rule = OwnedBusSignalMatch::from(rule);
        let match_rule = rule.as_match_rule().match_str();
        self.call_method(
            BusMethodCall::new(
                DBUS_SERVICE_NAME,
                DBUS_OBJECT_PATH,
                DBUS_INTERFACE,
                "AddMatch",
            )
            .with_body(vec![BusValue::String(match_rule)]),
        )?;
        self.signal_rules.push(rule);
        Ok(())
    }

    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
        let started = Instant::now();
        loop {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return Ok(None);
            }

            let remaining = timeout.saturating_sub(elapsed);
            let Some(message) = self
                .connection
                .channel()
                .blocking_pop_message(remaining)
                .map_err(|err| SessionBusError::Transport(err.to_string()))?
            else {
                return Ok(None);
            };

            if message.msg_type() != DbusMessageType::Signal {
                continue;
            }

            let signal = bus_signal_from_dbus_message(message)?;
            if self.signal_rules.is_empty()
                || self.signal_rules.iter().any(|rule| rule.matches(&signal))
            {
                return Ok(Some(signal));
            }
        }
    }
}

fn append_dbus_message_value(
    message: DbusMessage,
    value: BusValue,
) -> Result<DbusMessage, SessionBusError> {
    match value {
        BusValue::Bool(value) => Ok(message.append1(value)),
        BusValue::UnixFd(value) => {
            // SAFETY: the descriptor is owned by the caller-provided BusValue for
            // this message construction path.
            let fd = unsafe { dbus::arg::OwnedFd::from_raw_fd(value) };
            Ok(message.append1(fd))
        }
        BusValue::U32(value) => Ok(message.append1(value)),
        BusValue::U64(value) => Ok(message.append1(value)),
        BusValue::String(value) => Ok(message.append1(value)),
        unsupported @ (BusValue::ObjectPath(_)
        | BusValue::Array(_)
        | BusValue::Struct(_)
        | BusValue::Dict(_)
        | BusValue::Variant(_)) => Err(SessionBusError::UnsupportedMessageBody {
            context: "method-call body",
            kind: unsupported.kind(),
        }),
    }
}

fn bus_value_from_dbus_message_item(item: DbusMessageItem) -> Result<BusValue, SessionBusError> {
    match item {
        DbusMessageItem::Bool(value) => Ok(BusValue::Bool(value)),
        DbusMessageItem::UnixFd(value) => Ok(BusValue::UnixFd(value.into_raw_fd())),
        DbusMessageItem::UInt32(value) => Ok(BusValue::U32(value)),
        DbusMessageItem::UInt64(value) => Ok(BusValue::U64(value)),
        DbusMessageItem::Str(value) => Ok(BusValue::String(value)),
        DbusMessageItem::ObjectPath(value) => Ok(BusValue::ObjectPath(value.to_string())),
        DbusMessageItem::Array(values) => values
            .into_vec()
            .into_iter()
            .map(bus_value_from_dbus_message_item)
            .collect::<Result<Vec<_>, _>>()
            .map(BusValue::Array),
        DbusMessageItem::Struct(values) => values
            .into_iter()
            .map(bus_value_from_dbus_message_item)
            .collect::<Result<Vec<_>, _>>()
            .map(BusValue::Struct),
        DbusMessageItem::Dict(values) => values
            .into_vec()
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    bus_value_from_dbus_message_item(key)?,
                    bus_value_from_dbus_message_item(value)?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(BusValue::Dict),
        DbusMessageItem::Variant(value) => {
            bus_value_from_dbus_message_item(*value).map(|value| BusValue::Variant(Box::new(value)))
        }
        other => Err(SessionBusError::UnexpectedReplyShape {
            expected: "bool/u32/u64/string/object-path/array/struct/dict/fd/variant",
            actual: dbus_message_item_kind(&other),
        }),
    }
}

pub(crate) fn bus_signal_from_dbus_message(
    message: DbusMessage,
) -> Result<BusSignal, SessionBusError> {
    let path = message
        .path()
        .ok_or_else(|| SessionBusError::Transport("signal missing object path".to_string()))?
        .to_string();
    let interface = message
        .interface()
        .ok_or_else(|| SessionBusError::Transport("signal missing interface".to_string()))?
        .to_string();
    let member = message
        .member()
        .ok_or_else(|| SessionBusError::Transport("signal missing member".to_string()))?
        .to_string();
    let sender = message.sender().map(|sender| sender.to_string());
    let body = message
        .get_items()
        .into_iter()
        .map(bus_value_from_dbus_message_item)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(BusSignal {
        sender,
        path,
        interface,
        member,
        body,
    })
}

fn dbus_message_item_kind(item: &DbusMessageItem) -> &'static str {
    match item {
        DbusMessageItem::Bool(_) => "bool",
        DbusMessageItem::UInt64(_) => "u64",
        DbusMessageItem::Str(_) => "string",
        DbusMessageItem::Array(_) => "array",
        DbusMessageItem::Struct(_) => "struct",
        DbusMessageItem::Variant(_) => "variant",
        DbusMessageItem::Dict(_) => "dict",
        DbusMessageItem::ObjectPath(_) => "object path",
        DbusMessageItem::Signature(_) => "signature",
        DbusMessageItem::Byte(_) => "byte",
        DbusMessageItem::Int16(_) => "i16",
        DbusMessageItem::Int32(_) => "i32",
        DbusMessageItem::Int64(_) => "i64",
        DbusMessageItem::UInt16(_) => "u16",
        DbusMessageItem::UInt32(_) => "u32",
        DbusMessageItem::Double(_) => "f64",
        DbusMessageItem::UnixFd(_) => "fd",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_dbus_message_value, bus_value_from_dbus_message_item, get_name_owner,
        parse_name_owner_changed_signal, BusMethodCall, BusReply, BusSignal, BusSignalMatch,
        BusValue, DbusConnection, DbusSessionBusClient, NameOwnerChanged, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_METHOD_CALL_TIMEOUT, DBUS_OBJECT_PATH,
        DBUS_SERVICE_NAME,
    };
    use dbus::arg::messageitem::{MessageItem, MessageItemArray, MessageItemDict};
    use dbus::strings::{Path as DbusPath, Signature as DbusSignature};
    use dbus::Message as DbusMessage;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::io::{BufRead, BufReader};
    use std::os::fd::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct OwnedBusMethodCall {
        destination: String,
        path: String,
        interface: String,
        member: String,
        body: Vec<BusValue>,
    }

    impl<'a> From<BusMethodCall<'a>> for OwnedBusMethodCall {
        fn from(value: BusMethodCall<'a>) -> Self {
            Self {
                destination: value.destination.to_string(),
                path: value.path.to_string(),
                interface: value.interface.to_string(),
                member: value.member.to_string(),
                body: value.body,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct OwnedBusSignalMatch {
        sender: Option<String>,
        path: Option<String>,
        interface: Option<String>,
        member: Option<String>,
    }

    impl<'a> From<BusSignalMatch<'a>> for OwnedBusSignalMatch {
        fn from(value: BusSignalMatch<'a>) -> Self {
            Self {
                sender: value.sender.map(ToOwned::to_owned),
                path: value.path.map(ToOwned::to_owned),
                interface: value.interface.map(ToOwned::to_owned),
                member: value.member.map(ToOwned::to_owned),
            }
        }
    }

    impl OwnedBusSignalMatch {
        fn matches(&self, signal: &BusSignal) -> bool {
            if let Some(sender) = &self.sender {
                if signal.sender.as_ref() != Some(sender) {
                    return false;
                }
            }

            if let Some(path) = &self.path {
                if &signal.path != path {
                    return false;
                }
            }

            if let Some(interface) = &self.interface {
                if &signal.interface != interface {
                    return false;
                }
            }

            if let Some(member) = &self.member {
                if &signal.member != member {
                    return false;
                }
            }

            true
        }
    }

    #[derive(Debug, Default)]
    struct FakeSessionBusClient {
        owners: HashSet<String>,
        replies: HashMap<OwnedBusMethodCall, VecDeque<BusReply>>,
        signal_matches: Vec<OwnedBusSignalMatch>,
        queued_signals: VecDeque<BusSignal>,
        owners_to_activate_on_process: VecDeque<String>,
        process_timeouts: Vec<Duration>,
    }

    impl FakeSessionBusClient {
        fn set_name_owner(&mut self, name: &str, present: bool) {
            if present {
                self.owners.insert(name.to_string());
            } else {
                self.owners.remove(name);
            }
        }

        fn queue_name_owner_on_process(&mut self, name: &str) {
            self.owners_to_activate_on_process
                .push_back(name.to_string());
        }

        fn queue_reply(&mut self, call: BusMethodCall<'_>, reply: BusReply) {
            self.replies
                .entry(call.into())
                .or_default()
                .push_back(reply);
        }

        fn queue_signal(&mut self, signal: BusSignal) {
            self.queued_signals.push_back(signal);
        }
    }

    impl SessionBusClient for FakeSessionBusClient {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            Ok(self.owners.contains(name))
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            let key = OwnedBusMethodCall::from(call);
            self.replies
                .get_mut(&key)
                .and_then(VecDeque::pop_front)
                .ok_or_else(|| {
                    SessionBusError::Transport("no queued reply for method call".to_string())
                })
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.signal_matches.push(rule.into());
            Ok(())
        }

        fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            self.process_timeouts.push(timeout);

            if let Some(name) = self.owners_to_activate_on_process.pop_front() {
                self.owners.insert(name);
            }

            while let Some(signal) = self.queued_signals.pop_front() {
                if self.signal_matches.is_empty()
                    || self.signal_matches.iter().any(|rule| rule.matches(&signal))
                {
                    return Ok(Some(signal));
                }
            }

            Ok(None)
        }
    }

    #[test]
    fn reply_helpers_decode_expected_shapes() {
        let mut pipe_fds = [0; 2];
        let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(pipe_result, 0, "test pipe should be created");

        assert_eq!(
            BusReply::new(vec![BusValue::Bool(true)]).single_bool(),
            Ok(true)
        );
        assert_eq!(
            BusReply::new(vec![BusValue::Variant(Box::new(BusValue::Bool(false)))]).single_bool(),
            Ok(false)
        );
        assert_eq!(BusReply::new(vec![BusValue::U64(42)]).single_u64(), Ok(42));
        assert_eq!(
            BusReply::new(vec![BusValue::String("hello".to_string())]).single_string(),
            Ok("hello")
        );
        let fd = BusReply::new(vec![BusValue::UnixFd(pipe_fds[0])])
            .single_unix_fd()
            .expect("decode fd");
        assert_eq!(fd.as_raw_fd(), pipe_fds[0]);
        assert_eq!(
            BusReply::new(vec![BusValue::U64(7)]).single_bool(),
            Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "u64",
            })
        );

        drop(fd);
        unsafe {
            libc::close(pipe_fds[1]);
        }
    }

    #[test]
    fn outgoing_method_body_rejects_decode_only_variant_values() {
        let message = DbusMessage::new_method_call(
            "org.example.Service",
            "/org/example/Object",
            "org.example.Interface",
            "Method",
        )
        .expect("valid method call");

        assert_eq!(
            append_dbus_message_value(message, BusValue::Variant(Box::new(BusValue::Bool(true))))
                .expect_err("outgoing variants should be rejected"),
            SessionBusError::UnsupportedMessageBody {
                context: "method-call body",
                kind: "variant",
            }
        );
    }

    #[test]
    fn nested_logind_reply_shapes_decode_into_transport_values() {
        let user = MessageItem::Struct(vec![
            MessageItem::UInt32(1000),
            MessageItem::ObjectPath(
                DbusPath::new("/org/freedesktop/login1/user/_1000").expect("object path"),
            ),
        ]);
        let properties = MessageItem::Dict(
            MessageItemDict::new(
                vec![(
                    MessageItem::Str("User".to_string()),
                    MessageItem::Variant(Box::new(user)),
                )],
                DbusSignature::new("s").expect("key signature"),
                DbusSignature::new("v").expect("value signature"),
            )
            .expect("property dictionary"),
        );
        let invalidated = MessageItem::Array(
            MessageItemArray::new(
                vec![MessageItem::Str("LockedHint".to_string())],
                DbusSignature::new("as").expect("array signature"),
            )
            .expect("invalidated property array"),
        );

        assert_eq!(
            bus_value_from_dbus_message_item(properties).expect("decode properties"),
            BusValue::Dict(vec![(
                BusValue::String("User".to_string()),
                BusValue::Variant(Box::new(BusValue::Struct(vec![
                    BusValue::U32(1000),
                    BusValue::ObjectPath("/org/freedesktop/login1/user/_1000".to_string()),
                ]))),
            )])
        );
        assert_eq!(
            bus_value_from_dbus_message_item(invalidated).expect("decode invalidated array"),
            BusValue::Array(vec![BusValue::String("LockedHint".to_string())])
        );
    }

    #[test]
    fn wait_for_name_returns_when_owner_appears() {
        let mut bus = FakeSessionBusClient::default();
        bus.queue_name_owner_on_process("org.example.Service");

        bus.wait_for_name("org.example.Service", Duration::from_millis(200))
            .expect("name should appear before timeout");

        assert_eq!(bus.process_timeouts, vec![Duration::from_millis(50)]);
    }

    #[test]
    fn wait_for_name_times_out_when_owner_never_appears() {
        let mut bus = FakeSessionBusClient::default();

        let err = bus
            .wait_for_name("org.example.Missing", Duration::from_millis(120))
            .expect_err("missing name should time out");

        assert_eq!(
            err,
            SessionBusError::Timeout {
                name: "org.example.Missing".to_string(),
                timeout: Duration::from_millis(120),
            }
        );
        assert!(!bus.process_timeouts.is_empty());
        assert_eq!(bus.process_timeouts[0], Duration::from_millis(50));
        assert!(bus
            .process_timeouts
            .iter()
            .all(|timeout| *timeout <= Duration::from_millis(50)));
    }

    #[test]
    fn method_calls_use_generic_transport_shapes() {
        let mut bus = FakeSessionBusClient::default();
        let call = BusMethodCall::new(
            "org.example.Service",
            "/org/example/Object",
            "org.example.Interface",
            "Ping",
        )
        .with_body(vec![BusValue::String("hello".to_string())]);
        bus.queue_reply(call.clone(), BusReply::new(vec![BusValue::Bool(true)]));

        let reply = bus.call_method(call).expect("queued reply");

        assert_eq!(reply.single_bool(), Ok(true));
    }

    #[test]
    fn get_name_owner_uses_generic_dbus_endpoint() {
        let mut bus = FakeSessionBusClient::default();
        let call = BusMethodCall::new(
            DBUS_SERVICE_NAME,
            DBUS_OBJECT_PATH,
            DBUS_INTERFACE,
            "GetNameOwner",
        )
        .with_body(vec![BusValue::String("org.gnome.ScreenSaver".to_string())]);
        bus.queue_reply(
            call.clone(),
            BusReply::new(vec![BusValue::String(":1.42".to_string())]),
        );

        assert_eq!(
            get_name_owner(&mut bus, "org.gnome.ScreenSaver"),
            Ok(":1.42".to_string())
        );
    }

    #[test]
    fn process_returns_only_signals_matching_registered_rules() {
        let mut bus = FakeSessionBusClient::default();
        bus.add_signal_match(BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: Some("/org/gnome/ScreenSaver"),
            interface: Some("org.gnome.ScreenSaver"),
            member: Some("ActiveChanged"),
        })
        .expect("register match");

        bus.queue_signal(
            BusSignal::new("/org/example/Other", "org.example.Other", "Changed")
                .with_sender("org.example.Other"),
        );
        bus.queue_signal(
            BusSignal::new(
                "/org/gnome/ScreenSaver",
                "org.gnome.ScreenSaver",
                "ActiveChanged",
            )
            .with_sender("org.gnome.ScreenSaver")
            .with_body(vec![BusValue::Bool(true)]),
        );

        let signal = bus
            .process(Duration::from_millis(10))
            .expect("process signal")
            .expect("matching signal");

        assert_eq!(signal.member, "ActiveChanged");
        assert_eq!(signal.body, vec![BusValue::Bool(true)]);
        assert_eq!(bus.process(Duration::from_millis(10)), Ok(None));
    }

    #[test]
    fn bus_signal_match_handles_partial_rules() {
        let signal = BusSignal::new(
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
            "WakeUpScreen",
        )
        .with_sender("org.gnome.ScreenSaver");

        let broad_match = BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: None,
            interface: Some("org.gnome.ScreenSaver"),
            member: None,
        };
        let narrow_mismatch = BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: None,
            interface: Some("org.gnome.ScreenSaver"),
            member: Some("ActiveChanged"),
        };

        assert!(broad_match.matches(&signal));
        assert!(!narrow_mismatch.matches(&signal));
    }

    #[test]
    fn parse_name_owner_changed_signal_decodes_unique_owner_updates() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String("org.gnome.ScreenSaver".to_string()),
                BusValue::String(":1.10".to_string()),
                BusValue::String(":1.11".to_string()),
            ]);

        assert_eq!(
            parse_name_owner_changed_signal(&signal),
            Some(NameOwnerChanged {
                name: "org.gnome.ScreenSaver".to_string(),
                old_owner: Some(":1.10".to_string()),
                new_owner: Some(":1.11".to_string()),
            })
        );
    }

    #[test]
    fn parse_name_owner_changed_signal_treats_empty_owners_as_missing() {
        let signal = BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_body(vec![
                BusValue::String("org.gnome.ScreenSaver".to_string()),
                BusValue::String(":1.10".to_string()),
                BusValue::String(String::new()),
            ]);

        assert_eq!(
            parse_name_owner_changed_signal(&signal),
            Some(NameOwnerChanged {
                name: "org.gnome.ScreenSaver".to_string(),
                old_owner: Some(":1.10".to_string()),
                new_owner: None,
            })
        );
    }

    #[test]
    fn name_has_owner_uses_generic_transport_without_methods() {
        let mut bus = FakeSessionBusClient::default();
        bus.set_name_owner("org.example.Service", true);

        assert_eq!(bus.name_has_owner("org.example.Service"), Ok(true));
        assert_eq!(bus.name_has_owner("org.example.Missing"), Ok(false));
    }

    /// Owns a private test bus and reaps it before joining its address reader.
    struct PrivateDbusDaemon {
        address: String,
        child: Child,
        stdout_reader: Option<JoinHandle<()>>,
    }

    impl PrivateDbusDaemon {
        fn start() -> Result<Self, String> {
            let child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|err| {
                    format!(
                        "start private dbus-daemon: {err}; run tests in the Nix shell with dbus"
                    )
                })?;
            let mut daemon = Self {
                address: String::new(),
                child,
                stdout_reader: None,
            };
            let stdout = daemon.child.stdout.take().expect("piped stdout");
            let (tx, rx) = mpsc::channel();
            daemon.stdout_reader = Some(thread::spawn(move || {
                let address = BufReader::new(stdout).lines().next();
                let _ = tx.send(address);
            }));
            daemon.address = rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|err| format!("private bus address did not arrive within 5s: {err}"))?
                .ok_or("private bus closed without an address")?
                .map_err(|err| format!("read private bus address: {err}"))?;
            if !daemon.address.starts_with("unix:") {
                return Err(format!(
                    "unexpected private bus address: {:?}",
                    daemon.address
                ));
            }
            Ok(daemon)
        }

        fn address(&self) -> &str {
            &self.address
        }

        fn child_pid(&self) -> libc::pid_t {
            self.child.id() as libc::pid_t
        }

        fn stop_daemon(&mut self) {
            let _ = self.child.kill();
            self.child.wait().expect("reap private dbus-daemon");
        }
    }

    impl Drop for PrivateDbusDaemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(reader) = self.stdout_reader.take() {
                let _ = reader.join();
            }
        }
    }

    #[test]
    fn private_dbus_daemon_fixture_starts_returns_address_and_reaps_cleanly() {
        // The fixture's own runnable check: it must come up, hand back a usable explicit
        // address, and (on drop) reap its child so no daemon leaks.
        let daemon = PrivateDbusDaemon::start().expect("start private dbus-daemon");
        assert!(
            daemon.address().starts_with("unix:"),
            "address should be a unix transport endpoint, got {}",
            daemon.address()
        );

        // A real client can connect to the *explicit* address — proving the daemon is actually
        // listening, not just that a line was printed.
        let connection = DbusConnection::new_address(daemon.address())
            .expect("connect client to the explicit private bus address");
        drop(connection);

        let pid = daemon.child_pid();
        drop(daemon);
        let mut status = 0;
        // SAFETY: the PID came from our now-reaped child; WNOHANG cannot block.
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn add_signal_match_addmatch_rpc_is_bounded_by_method_call_timeout() {
        // Stall the owned daemon with SIGSTOP *before* the call: the in-flight AddMatch
        // can never be answered, so the transport must surface an Err within
        // method_call_timeout (100ms here) instead of the old fixed-5000ms hang.
        //
        // The daemon is stopped, not killed: the watchdog SIGCONTs it a second later so
        // the whole test stays bounded even under the OLD implementation (whose fixed
        // 5000ms RPC would resume and return late). The SIGCONT is only meaningful while
        // the child is still ours, so the watchdog thread is joined on scope exit —
        // before the fixture reaps its child — to avoid signalling a recycled PID.
        let daemon = PrivateDbusDaemon::start().expect("start private dbus-daemon");
        let address = daemon.address().to_string();
        let pid = daemon.child_pid();

        let mut client = DbusSessionBusClient {
            connection: DbusConnection::new_address(&address)
                .expect("connect client to the explicit private bus address"),
            method_call_timeout: Duration::from_millis(100),
            signal_rules: Vec::new(),
        };

        // SAFETY: this PID belongs to the live child owned by the fixture.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let mut status = 0;
            // WUNTRACED reports a stop without reaping the stopped child.
            // SAFETY: status is writable, pid is our child, and WNOHANG bounds the call.
            let observed =
                unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) };
            if observed == pid {
                assert!(
                    libc::WIFSTOPPED(status),
                    "private daemon exited instead of stopping"
                );
                break;
            }
            assert_eq!(observed, 0, "observe owned daemon stop");
            assert!(
                Instant::now() < deadline,
                "private daemon did not stop within one second"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let rule = BusSignalMatch {
            sender: Some("org.gnome.ScreenSaver"),
            path: None,
            interface: None,
            member: Some("ActiveChanged"),
        };

        // Run the call inside a scope that owns a watchdog: after one second it SIGCONTs
        // the same daemon so the old fixed-5000ms path cannot hang the test. Scoping the
        // watchdog means scope exit joins it (also on panic) before the fixture reaps.
        let (result, elapsed) = thread::scope(|scope| {
            let watchdog = scope.spawn(|| {
                thread::sleep(Duration::from_secs(1));
                // Resume the same child so a stale fixed-timeout RPC completes late.
                unsafe {
                    libc::kill(pid, libc::SIGCONT);
                }
            });

            let started = Instant::now();
            let result = client.add_signal_match(rule);
            // Capture the elapsed time *immediately*, before leaving the scope: the
            // scoped watchdog must not be able to inflate the measured RPC duration.
            let elapsed = started.elapsed();

            // Join the watchdog before the fixture's Drop reaps the child, so the late
            // SIGCONT can never land on a recycled PID.
            watchdog.join().expect("watchdog thread panicked");

            (result, elapsed)
        });

        // The scope has joined the watchdog, so it is safe to let the fixture reap the
        // (now resumed) child; assert on the outcome once we are fully out of the scope.
        assert!(
            result.is_err(),
            "AddMatch against a stopped bus must fail: {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(700),
            "AddMatch took {elapsed:?}, not bounded by the 100ms method_call_timeout (old path hung ~5000ms)"
        );
        assert!(
            client.signal_rules.is_empty(),
            "a failed AddMatch must not push a local rule, got {:?}",
            client.signal_rules
        );
    }

    #[test]
    fn add_signal_match_transport_failure_preserves_prior_rules() {
        // A previously successful subscription must survive a later transport failure:
        // if AddMatch cannot reach the daemon, the client must return Err without adding a
        // new local rule *and* without erasing the rules it already accepted.
        let mut daemon = PrivateDbusDaemon::start().expect("start private dbus-daemon");
        let address = daemon.address();

        let mut client = DbusSessionBusClient {
            connection: DbusConnection::new_address(address)
                .expect("connect client to the explicit private bus address"),
            method_call_timeout: DBUS_METHOD_CALL_TIMEOUT,
            signal_rules: Vec::new(),
        };

        // Establish one real, retained subscription while the daemon is alive.
        let good = BusSignalMatch {
            sender: Some("org.example.Kept"),
            path: None,
            interface: None,
            member: Some("Changed"),
        };
        client
            .add_signal_match(good)
            .expect("the first AddMatch should succeed against the live daemon");
        let kept = client.signal_rules.clone();
        assert_eq!(
            client.signal_rules,
            vec![super::OwnedBusSignalMatch::from(good)],
            "the first rule should be retained after the successful AddMatch"
        );

        // Stop only this owned daemon; a subsequent AddMatch now has no bus to answer.
        daemon.stop_daemon();

        let failed = BusSignalMatch {
            sender: Some("org.example.Dead"),
            path: None,
            interface: None,
            member: Some("Changed"),
        };
        let result = client.add_signal_match(failed);

        // The failed RPC must not add a new local rule, and must not erase the rule the
        // client already accepted — the retained set is exactly the pre-failure set.
        assert!(
            result.is_err(),
            "AddMatch against a dead daemon must fail: {result:?}"
        );
        assert_eq!(
            client.signal_rules, kept,
            "a failed AddMatch must neither add a rule nor erase the previously successful one"
        );
    }

    #[test]
    fn add_signal_match_subscribes_through_daemon_and_processes_matching_signal() {
        let daemon = PrivateDbusDaemon::start().expect("start private dbus-daemon");
        let address = daemon.address();

        // The producer connects first so the rule can filter on the bus-assigned sender: the
        // daemon fills the signal's `sender` field with the producer's *unique* connection
        // name, and the daemon's own match evaluation then enforces that filter. A local-
        // shortcut implementation (no daemon contact) would have no bus to answer AddMatch
        // and no sender to deliver.
        let producer_connection = DbusConnection::new_address(address)
            .expect("publish on the explicit private bus address");
        let producer_sender = producer_connection.unique_name().to_string();

        // The subscriber is built from the private fields against the *explicit* fixture
        // address (never `new_session`).
        let mut subscriber = DbusSessionBusClient {
            connection: DbusConnection::new_address(address)
                .expect("subscribe on the explicit private bus address"),
            method_call_timeout: DBUS_METHOD_CALL_TIMEOUT,
            signal_rules: Vec::new(),
        };
        let rule = BusSignalMatch {
            sender: Some(producer_sender.as_str()),
            path: Some("/org/example/Thing"),
            interface: Some("org.example.Interface"),
            member: Some("Changed"),
        };

        // `add_signal_match` only retains the local rule after the AddMatch round-trip
        // succeeds, so an `Ok` reply plus the retained rule proves the daemon accepted the
        // subscription.
        subscriber
            .add_signal_match(rule)
            .expect("AddMatch should round-trip through the private dbus-daemon");
        assert_eq!(
            subscriber.signal_rules,
            vec![super::OwnedBusSignalMatch::from(rule)],
            "the local rule should be retained after the successful AddMatch"
        );

        // process already filters bookkeeping signals; no matching signal exists yet.
        assert_eq!(subscriber.process(Duration::from_millis(20)).unwrap(), None);

        let message =
            DbusMessage::new_signal("/org/example/Thing", "org.example.Interface", "Changed")
                .expect("valid signal");
        let message = message
            .append1("hello from the producer")
            .append1(7u32)
            .append1(true);
        producer_connection
            .channel()
            .send(message)
            .expect("emit the matching signal");

        let received = subscriber
            .process(Duration::from_secs(2))
            .expect("bounded process should surface the matching signal")
            .expect("the matching signal should be delivered through the daemon subscription");

        // The sender is the bus-assigned unique name of the producer — the daemon set it,
        // proof the signal crossed the bus, not a local shortcut.
        assert_eq!(received.sender, Some(producer_sender.clone()));
        assert_eq!(received.path, "/org/example/Thing");
        assert_eq!(received.interface, "org.example.Interface");
        assert_eq!(received.member, "Changed");
        assert_eq!(
            received.body,
            vec![
                BusValue::String("hello from the producer".to_string()),
                BusValue::U32(7),
                BusValue::Bool(true),
            ]
        );
        // The retained rule's sender filter is the bus-assigned name; it matches exactly the
        // signal the daemon delivered.
        assert_eq!(
            subscriber.signal_rules,
            vec![super::OwnedBusSignalMatch {
                sender: Some(producer_sender.clone()),
                path: Some("/org/example/Thing".to_string()),
                interface: Some("org.example.Interface".to_string()),
                member: Some("Changed".to_string()),
            }],
            "the retained rule should match exactly the signal the daemon delivered"
        );
    }
}
