use crate::session_bus::{new_system_bus_client, SessionBusClient};
use crate::sources::linux::logind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimePhaseRead {
    Pending,
    NotPending,
    Unknown { detail: String },
}

pub(crate) trait RuntimePhaseProvider {
    fn machine_sleep_pending(&mut self) -> RuntimePhaseRead;
}

#[cfg(test)]
pub(crate) struct NoopRuntimePhaseProvider;

#[cfg(test)]
impl RuntimePhaseProvider for NoopRuntimePhaseProvider {
    fn machine_sleep_pending(&mut self) -> RuntimePhaseRead {
        RuntimePhaseRead::NotPending
    }
}

pub(crate) enum LogindRuntimePhaseProvider {
    Ready(Box<dyn SessionBusClient + Send>),
    Unavailable(String),
}

impl LogindRuntimePhaseProvider {
    pub(crate) fn from_system_bus() -> Self {
        match new_system_bus_client() {
            Ok(bus) => Self::Ready(bus),
            Err(err) => Self::Unavailable(err.to_string()),
        }
    }
}

impl RuntimePhaseProvider for LogindRuntimePhaseProvider {
    fn machine_sleep_pending(&mut self) -> RuntimePhaseRead {
        match self {
            Self::Ready(bus) => match logind::preparing_for_sleep(bus) {
                Ok(true) => RuntimePhaseRead::Pending,
                Ok(false) => RuntimePhaseRead::NotPending,
                Err(err) => RuntimePhaseRead::Unknown {
                    detail: err.to_string(),
                },
            },
            Self::Unavailable(detail) => RuntimePhaseRead::Unknown {
                detail: detail.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LogindRuntimePhaseProvider, RuntimePhaseProvider, RuntimePhaseRead};
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError,
    };
    use std::collections::VecDeque;
    use std::time::Duration;

    #[derive(Debug)]
    struct FakeBus {
        replies: VecDeque<Result<BusReply, SessionBusError>>,
    }

    impl FakeBus {
        fn with_reply(reply: Result<BusReply, SessionBusError>) -> Self {
            Self {
                replies: VecDeque::from([reply]),
            }
        }
    }

    impl SessionBusClient for FakeBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            unreachable!("name probing is not used by runtime phase tests")
        }

        fn call_method(&mut self, _call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.replies.pop_front().expect("queued reply")
        }

        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            unreachable!("signal matches are not used by runtime phase tests")
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            unreachable!("signal processing is not used by runtime phase tests")
        }
    }

    #[test]
    fn logind_provider_reads_pending_sleep() {
        let bus = FakeBus::with_reply(Ok(BusReply::new(vec![BusValue::Variant(Box::new(
            BusValue::Bool(true),
        ))])));
        let mut provider = LogindRuntimePhaseProvider::Ready(Box::new(bus));

        assert_eq!(provider.machine_sleep_pending(), RuntimePhaseRead::Pending);
    }

    #[test]
    fn logind_provider_reads_not_pending_sleep() {
        let bus = FakeBus::with_reply(Ok(BusReply::new(vec![BusValue::Variant(Box::new(
            BusValue::Bool(false),
        ))])));
        let mut provider = LogindRuntimePhaseProvider::Ready(Box::new(bus));

        assert_eq!(
            provider.machine_sleep_pending(),
            RuntimePhaseRead::NotPending
        );
    }

    #[test]
    fn logind_provider_reports_unknown_on_read_failure() {
        let bus = FakeBus::with_reply(Err(SessionBusError::Transport(
            "system bus unavailable".to_string(),
        )));
        let mut provider = LogindRuntimePhaseProvider::Ready(Box::new(bus));

        assert_eq!(
            provider.machine_sleep_pending(),
            RuntimePhaseRead::Unknown {
                detail: "system bus unavailable".to_string(),
            }
        );
    }
}
