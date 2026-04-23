use std::fmt;
use std::process::Command;
use std::time::{Duration, Instant};

const SESSION_BUS_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DBUS_SERVICE_NAME: &str = "org.freedesktop.DBus";
const DBUS_OBJECT_PATH: &str = "/org/freedesktop/DBus";

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
        }
    }
}

impl std::error::Error for SessionBusError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusValue {
    Bool(bool),
    U64(u64),
    String(String),
}

impl BusValue {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::U64(_) => "u64",
            Self::String(_) => "string",
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

#[derive(Debug, Default, Clone, Copy)]
pub struct GdbusSessionBusClient;

impl SessionBusClient for GdbusSessionBusClient {
    fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
        let output = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                DBUS_SERVICE_NAME,
                "--object-path",
                DBUS_OBJECT_PATH,
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                name,
            ])
            .output()
            .map_err(|err| SessionBusError::Transport(err.to_string()))?;

        if !output.status.success() {
            return Err(SessionBusError::Transport(format!(
                "gdbus NameHasOwner failed with status {}",
                output.status
            )));
        }

        parse_gdbus_name_has_owner_output(&String::from_utf8_lossy(&output.stdout)).ok_or(
            SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "unparsed gdbus reply",
            },
        )
    }

    fn wait_for_name(&mut self, name: &str, timeout: Duration) -> Result<(), SessionBusError> {
        let timeout_secs = timeout.as_secs().max(1).to_string();
        let status = Command::new("gdbus")
            .args(["wait", "--session", "--timeout", &timeout_secs, name])
            .status()
            .map_err(|err| SessionBusError::Transport(err.to_string()))?;

        if status.success() {
            Ok(())
        } else {
            Err(SessionBusError::Timeout {
                name: name.to_string(),
                timeout,
            })
        }
    }

    fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
        let _ = call;
        Err(SessionBusError::Transport(
            "GdbusSessionBusClient does not implement generic method calls".to_string(),
        ))
    }

    fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
        let _ = rule;
        Err(SessionBusError::Transport(
            "GdbusSessionBusClient does not implement signal matches".to_string(),
        ))
    }

    fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
        let _ = timeout;
        Err(SessionBusError::Transport(
            "GdbusSessionBusClient does not implement in-process message pumping".to_string(),
        ))
    }
}

fn parse_gdbus_name_has_owner_output(output: &str) -> Option<bool> {
    if output.contains("(true,)") {
        Some(true)
    } else if output.contains("(false,)") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_gdbus_name_has_owner_output, BusMethodCall, BusReply, BusSignal, BusSignalMatch,
        BusValue, SessionBusClient, SessionBusError,
    };
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::time::Duration;

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
        assert_eq!(
            BusReply::new(vec![BusValue::Bool(true)]).single_bool(),
            Ok(true)
        );
        assert_eq!(BusReply::new(vec![BusValue::U64(42)]).single_u64(), Ok(42));
        assert_eq!(
            BusReply::new(vec![BusValue::String("hello".to_string())]).single_string(),
            Ok("hello")
        );
        assert_eq!(
            BusReply::new(vec![BusValue::U64(7)]).single_bool(),
            Err(SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "u64",
            })
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
    fn name_has_owner_uses_generic_transport_without_methods() {
        let mut bus = FakeSessionBusClient::default();
        bus.set_name_owner("org.example.Service", true);

        assert_eq!(bus.name_has_owner("org.example.Service"), Ok(true));
        assert_eq!(bus.name_has_owner("org.example.Missing"), Ok(false));
    }

    #[test]
    fn parses_gdbus_name_has_owner_output() {
        assert_eq!(parse_gdbus_name_has_owner_output("(true,)\n"), Some(true));
        assert_eq!(parse_gdbus_name_has_owner_output("(false,)\n"), Some(false));
        assert_eq!(parse_gdbus_name_has_owner_output("unexpected"), None);
    }
}
