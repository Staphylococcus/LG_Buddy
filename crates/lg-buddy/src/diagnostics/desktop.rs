//! Session type and configured desktop integration.

use super::DiagnosticSection;
use crate::config::ScreenBackend;
use crate::settings::{ConfigPathResolver, SettingsStore};
use std::env;

pub(super) fn collect() -> DiagnosticSection {
    let store = ConfigPathResolver::resolve_from_env()
        .ok()
        .and_then(|path| SettingsStore::load(path).ok());
    let override_value = env::var("LG_BUDDY_SCREEN_BACKEND").ok();
    collect_from(
        env::var("XDG_SESSION_TYPE").ok().as_deref(),
        override_value.as_deref(),
        store.as_ref(),
    )
}

fn collect_from(
    session: Option<&str>,
    override_value: Option<&str>,
    store: Option<&SettingsStore>,
) -> DiagnosticSection {
    let session = match session {
        Some("wayland") => "wayland",
        Some("x11") => "x11",
        Some(_) => "unknown session type",
        None => "session type unavailable",
    };
    let mut body = format!("session type: {session}\n");
    match diagnostic_configured_backend(override_value, store) {
        Ok(configured) => {
            body.push_str("configured backend: ");
            body.push_str(configured.as_str());
            body.push('\n');
        }
        Err(error) => {
            body.push_str("configured backend: invalid or unavailable\n");
            body.push_str("configuration finding: ");
            body.push_str(error);
            body.push('\n');
        }
    }
    DiagnosticSection::new("Desktop", body)
}

fn diagnostic_configured_backend(
    override_value: Option<&str>,
    store: Option<&SettingsStore>,
) -> Result<ScreenBackend, &'static str> {
    if let Some(value) = override_value {
        return value
            .parse()
            .map_err(|_| "invalid backend override (raw value omitted)");
    }
    let store = store.ok_or("screen.backend could not be read")?;
    setting_value(store, "screen.backend")
        .and_then(|value| value.parse().ok())
        .ok_or("screen.backend is invalid (raw value omitted)")
}

fn setting_value(store: &SettingsStore, key: &str) -> Option<&'static str> {
    store.effective_by_name(key).ok()?.value()?.as_enum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ConfigEnvReader;
    #[test]
    fn backend_setting_remains_visible_when_an_unrelated_setting_is_invalid() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_backend=gnome\ntvs_primary_ip=invalid\n",
        ));
        assert!(store
            .effective_by_name("tv.ip")
            .unwrap()
            .invalid_value()
            .is_some());
        assert_eq!(
            diagnostic_configured_backend(None, Some(&store)),
            Ok(ScreenBackend::Gnome)
        );
        assert_eq!(
            diagnostic_configured_backend(Some("wayland"), Some(&store)),
            Ok(ScreenBackend::Wayland)
        );
        let invalid = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_backend=private-invalid-value\n",
        ));
        assert!(diagnostic_configured_backend(None, Some(&invalid)).is_err());
        assert!(
            diagnostic_configured_backend(Some("private-invalid-value"), Some(&store)).is_err()
        );
    }

    #[test]
    fn getter_uses_explicit_inputs_and_omits_invalid_raw_values() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_backend=gnome\n",
        ));
        let section = collect_from(Some("wayland"), Some("auto"), Some(&store));
        assert_eq!(
            section.body(),
            "session type: wayland\nconfigured backend: auto\n"
        );
        let section = collect_from(None, None, None);
        assert!(section.body().contains("session type unavailable"));
        assert!(section.body().contains("screen.backend could not be read"));
        let section = collect_from(
            Some("unexpected-private-value"),
            Some("private-invalid-value"),
            Some(&store),
        );
        assert!(section.body().contains("unknown session type"));
        assert!(section.body().contains("invalid backend override"));
        assert!(!section.body().contains("private-value"));
        assert!(!section.body().contains("private-invalid-value"));
    }
}
