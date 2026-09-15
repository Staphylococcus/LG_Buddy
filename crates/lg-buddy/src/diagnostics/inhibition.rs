//! Fresh inhibition readings, independent of the monitor's policy evaluation.

use super::{report::safe_text, DiagnosticSection};
use crate::session_bus::{
    get_name_owner, new_session_bus_client, BusMethodCall, BusValue, SessionBusClient,
    SessionBusError,
};

pub(super) fn collect() -> DiagnosticSection {
    match new_session_bus_client() {
        Ok(mut bus) => collect_from_bus(&mut bus),
        Err(_) => DiagnosticSection::new("Inhibition sources", "Session bus: unavailable"),
    }
}

fn collect_from_bus(bus: &mut impl SessionBusClient) -> DiagnosticSection {
    use crate::sources::desktop::kwin;
    let mut body = String::new();
    for (label, service, path, interface, method, flag) in [
        (
            "GNOME",
            "org.gnome.SessionManager",
            "/org/gnome/SessionManager",
            "org.gnome.SessionManager",
            "IsInhibited",
            Some(8),
        ),
        (
            "PowerDevil",
            "org.kde.Solid.PowerManagement",
            "/org/kde/Solid/PowerManagement/PolicyAgent",
            "org.kde.Solid.PowerManagement.PolicyAgent",
            "HasInhibition",
            Some(4),
        ),
        (
            "KWin",
            kwin::SERVICE,
            kwin::PATH,
            kwin::INTERFACE,
            "IsInhibited",
            None,
        ),
    ] {
        let result = (|| -> Result<Option<bool>, SessionBusError> {
            if !bus.name_has_owner(service)? {
                return Ok(None);
            }
            let owner = get_name_owner(bus, service)?;
            if service == kwin::SERVICE && get_name_owner(bus, "org.kde.KWin")? != owner {
                return Err(SessionBusError::Transport(
                    "bridge owner does not match KWin".into(),
                ));
            }
            let mut call = BusMethodCall::new(&owner, path, interface, method);
            if let Some(flag) = flag {
                call = call.with_body(vec![BusValue::U32(flag)]);
            }
            let inhibited = bus.call_method(call)?.single_bool()?;
            if get_name_owner(bus, service)? != owner
                || (service == kwin::SERVICE && get_name_owner(bus, "org.kde.KWin")? != owner)
            {
                return Err(SessionBusError::Transport(
                    "source changed during query".into(),
                ));
            }
            Ok(Some(inhibited))
        })();
        body.push_str(label);
        body.push_str(": ");
        match result {
            Ok(Some(true)) => body.push_str("available; inhibiting"),
            Ok(Some(false)) => body.push_str("available; not inhibiting"),
            Ok(None) => body.push_str("absent"),
            Err(error) => {
                body.push_str("read failed: ");
                body.push_str(safe_text(&error.to_string(), 512).trim_end());
            }
        }
        body.push('\n');
    }
    DiagnosticSection::new("Inhibition sources", body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[derive(Default)]
    struct InhibitionBus {
        present: bool,
        service: Option<&'static str>,
        replies: std::collections::VecDeque<BusValue>,
        getters: usize,
        changed_owner: bool,
        bridge_owner_mismatch: bool,
    }

    impl SessionBusClient for InhibitionBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            Ok(self.present
                && name
                    == self
                        .service
                        .unwrap_or(crate::sources::desktop::kwin::SERVICE))
        }
        fn call_method(
            &mut self,
            call: BusMethodCall<'_>,
        ) -> Result<crate::session_bus::BusReply, SessionBusError> {
            let value = match call.member {
                "GetNameOwner" => BusValue::String(
                    if (self.changed_owner && self.getters > 0)
                        || (self.bridge_owner_mismatch
                            && call.body == [BusValue::String("org.kde.KWin".into())])
                    {
                        ":1.24"
                    } else {
                        ":1.23"
                    }
                    .into(),
                ),
                "IsInhibited" | "HasInhibition" => {
                    assert_eq!(call.destination, ":1.23");
                    match self.service {
                        Some("org.gnome.SessionManager") => {
                            assert_eq!(call.member, "IsInhibited");
                            assert_eq!(call.path, "/org/gnome/SessionManager");
                            assert_eq!(call.interface, "org.gnome.SessionManager");
                            assert_eq!(call.body, [BusValue::U32(8)]);
                        }
                        Some("org.kde.Solid.PowerManagement") => {
                            assert_eq!(call.member, "HasInhibition");
                            assert_eq!(call.path, "/org/kde/Solid/PowerManagement/PolicyAgent");
                            assert_eq!(call.interface, "org.kde.Solid.PowerManagement.PolicyAgent");
                            assert_eq!(call.body, [BusValue::U32(4)]);
                        }
                        _ => {
                            assert_eq!(call.path, crate::sources::desktop::kwin::PATH);
                            assert_eq!(call.interface, crate::sources::desktop::kwin::INTERFACE);
                            assert!(call.body.is_empty());
                        }
                    }
                    self.getters += 1;
                    self.replies.pop_front().expect("unexpected query")
                }
                _ => panic!("unexpected method: {}", call.member),
            };
            Ok(crate::session_bus::BusReply { body: vec![value] })
        }
        fn add_signal_match(
            &mut self,
            _: crate::session_bus::BusSignalMatch<'_>,
        ) -> Result<(), SessionBusError> {
            panic!("snapshot must not subscribe")
        }
        fn process(
            &mut self,
            _: Duration,
        ) -> Result<Option<crate::session_bus::BusSignal>, SessionBusError> {
            panic!("snapshot must not consume activity")
        }
    }

    #[test]
    fn inhibition_snapshot_queries_current_state_without_an_idle_evaluation() {
        let mut bus = InhibitionBus {
            present: true,
            replies: [
                BusValue::Bool(true),
                BusValue::Bool(false),
                BusValue::U32(1),
            ]
            .into(),
            ..InhibitionBus::default()
        };
        let first = collect_from_bus(&mut bus);
        assert!(first.body().contains("KWin: available; inhibiting"));
        assert!(first.body().contains("PowerDevil: absent"));
        let second = collect_from_bus(&mut bus);
        assert!(second.body().contains("KWin: available; not inhibiting"));
        let malformed = collect_from_bus(&mut bus);
        assert!(malformed.body().contains("KWin: read failed:"));
        assert!(!malformed.body().contains("KWin: available"));
        assert_eq!(bus.getters, 3);
    }

    #[test]
    fn absent_inhibition_sources_are_not_activated_and_owner_changes_are_rejected() {
        let mut bus = InhibitionBus::default();
        assert!(collect_from_bus(&mut bus).body().contains("KWin: absent"));
        assert_eq!(bus.getters, 0);
        bus.present = true;
        bus.changed_owner = true;
        bus.replies.push_back(BusValue::Bool(false));
        let section = collect_from_bus(&mut bus);
        assert!(section.body().contains("source changed during query"));
        assert!(!section.body().contains("KWin: available"));
    }

    #[test]
    fn desktop_policy_getters_use_their_effective_idle_flags() {
        for (service, label) in [
            ("org.gnome.SessionManager", "GNOME"),
            ("org.kde.Solid.PowerManagement", "PowerDevil"),
        ] {
            let mut bus = InhibitionBus {
                present: true,
                service: Some(service),
                replies: [BusValue::Bool(true), BusValue::Bool(false)].into(),
                ..InhibitionBus::default()
            };
            assert!(collect_from_bus(&mut bus)
                .body()
                .contains(&format!("{label}: available; inhibiting")));
            assert!(collect_from_bus(&mut bus)
                .body()
                .contains(&format!("{label}: available; not inhibiting")));
            assert_eq!(bus.getters, 2);
        }
    }

    #[test]
    fn bridge_owned_by_another_process_is_not_queried() {
        let mut bus = InhibitionBus {
            present: true,
            bridge_owner_mismatch: true,
            ..InhibitionBus::default()
        };
        assert!(collect_from_bus(&mut bus)
            .body()
            .contains("bridge owner does not match KWin"));
        assert_eq!(bus.getters, 0);
    }
}
