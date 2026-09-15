//! Effective typed settings, without exposing raw configuration values.

use super::DiagnosticSection;
use crate::settings::{ConfigPathResolver, SettingValue, SettingsStore};
use std::path::Path;

pub(super) fn collect() -> DiagnosticSection {
    let path = ConfigPathResolver::resolve_from_env().ok();
    collect_from_path(path.as_deref())
}

fn collect_from_path(path: Option<&Path>) -> DiagnosticSection {
    let path = match path {
        Some(path) => path,
        None => {
            return DiagnosticSection::new(
                "Effective settings",
                "Settings are unavailable: no configuration path could be resolved.\nAction: configure a TV, then retry diagnostics.",
            )
        }
    };

    let store = match SettingsStore::load(path) {
        Ok(store) => store,
        Err(_) => {
            return DiagnosticSection::new(
                "Effective settings",
                "Settings are unavailable: the configuration could not be read.\nAction: check the configuration access and retry diagnostics.",
            )
        }
    };

    collect_from_store(&store)
}

fn collect_from_store(store: &SettingsStore) -> DiagnosticSection {
    let mut body = String::new();
    body.push_str("configuration: resolved and readable\n");
    let mut invalid = false;
    for setting in store.all_effective() {
        body.push_str(setting.key_name());
        body.push_str(" = ");
        match (setting.value(), setting.invalid_value()) {
            (_, Some(_)) => {
                invalid = true;
                body.push_str("invalid (raw value omitted; expected ");
                body.push_str(&setting.definition().value_type().expected());
                body.push(')');
            }
            (Some(value), None) => body.push_str(&safe_setting_value(value)),
            (None, None) => body.push_str("missing"),
        }
        body.push_str(" [source: ");
        body.push_str(setting.source().as_str());
        body.push_str("]\n");
    }
    if invalid {
        body.push_str("Invalid settings: correct the values marked above.\n");
    }
    DiagnosticSection::new("Effective settings", body)
}

fn safe_setting_value(value: SettingValue) -> String {
    // SettingValue can only contain typed, registry-validated values.  Keep
    // this helper explicit so future secret-bearing setting types cannot be
    // accidentally rendered by a broad Debug or raw-config formatter.
    match value {
        SettingValue::Enum(value) => value.to_string(),
        SettingValue::Integer(value) => value.to_string(),
        SettingValue::Ipv4(value) => value.to_string(),
        SettingValue::MacAddress(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ConfigEnvReader;

    #[test]
    fn getter_keeps_effective_values_and_origins_but_omits_invalid_raw_values() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_idle_blank=enabled\nscreen_idle_timeout=42\n",
        ));
        let section = collect_from_store(&store);
        assert!(section
            .body()
            .contains("screen.idle_blank = enabled [source: config.env]"));
        assert!(section
            .body()
            .contains("screen.idle_timeout = 42 [source: config.env]"));
        assert!(section
            .body()
            .contains("screen.backend = auto [source: default]"));
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_idle_blank=private-invalid-value\n",
        ));
        let section = collect_from_store(&store);
        assert!(section.body().contains("screen.idle_blank = invalid"));
        assert!(!section.body().contains("private-invalid-value"));
    }

    #[test]
    fn unresolved_and_unreadable_configuration_remain_distinct() {
        assert!(collect_from_path(None)
            .body()
            .contains("no configuration path could be resolved"));
        assert!(collect_from_path(Some(Path::new("/dev/null/config.env")))
            .body()
            .contains("configuration could not be read"));
    }
}
