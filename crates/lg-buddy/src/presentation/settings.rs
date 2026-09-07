use crate::presentation::brightness::UserFacingError;
use crate::settings::{EffectiveSetting, SettingSource, SettingType, SettingValue, SettingsStore};
use crate::settings_view::SettingsIntent;

/// The toolkit-neutral model rendered by the Settings view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPresentation {
    status: SettingsStatus,
    groups: Vec<SettingsGroup>,
    retry_action: Option<SettingsAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsStatus {
    Loading { message: String },
    Ready,
    Failed(UserFacingError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsGroup {
    title: String,
    description: String,
    rows: Vec<SettingsRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsRow {
    title: String,
    description: String,
    value_label: String,
    source_label: String,
    default_label: String,
    accepted_values_label: String,
    problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsAction {
    label: String,
    enabled: bool,
    intent: SettingsIntent,
}

impl SettingsPresentation {
    pub(crate) fn loading() -> Self {
        Self {
            status: SettingsStatus::Loading {
                message: "Loading settings…".to_string(),
            },
            groups: Vec::new(),
            retry_action: None,
        }
    }

    pub(crate) fn ready(groups: Vec<SettingsGroup>) -> Self {
        Self {
            status: SettingsStatus::Ready,
            groups,
            retry_action: None,
        }
    }

    pub(crate) fn failed(error: UserFacingError) -> Self {
        Self {
            status: SettingsStatus::Failed(error),
            groups: Vec::new(),
            retry_action: Some(SettingsAction::new("Retry", true, SettingsIntent::Retry)),
        }
    }

    /// Build the Settings view from the same registry-backed store used by the
    /// CLI. This is also useful to fixture presentation tests without GTK or a
    /// live environment.
    pub fn from_store(store: &SettingsStore) -> Self {
        Self::ready(groups_from_store(store))
    }

    pub fn status(&self) -> &SettingsStatus {
        &self.status
    }

    pub fn groups(&self) -> &[SettingsGroup] {
        &self.groups
    }

    pub fn retry_action(&self) -> Option<&SettingsAction> {
        self.retry_action.as_ref()
    }
}

impl SettingsGroup {
    pub(crate) fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        rows: Vec<SettingsRow>,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            rows,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn rows(&self) -> &[SettingsRow] {
        &self.rows
    }
}

impl SettingsRow {
    pub(crate) fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        value_label: impl Into<String>,
        source_label: impl Into<String>,
        default_label: impl Into<String>,
        accepted_values_label: impl Into<String>,
        problem: Option<impl Into<String>>,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            value_label: value_label.into(),
            source_label: source_label.into(),
            default_label: default_label.into(),
            accepted_values_label: accepted_values_label.into(),
            problem: problem.map(Into::into),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn value_label(&self) -> &str {
        &self.value_label
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn default_label(&self) -> &str {
        &self.default_label
    }

    pub fn accepted_values_label(&self) -> &str {
        &self.accepted_values_label
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }
}

impl SettingsAction {
    pub(crate) fn new(label: impl Into<String>, enabled: bool, intent: SettingsIntent) -> Self {
        Self {
            label: label.into(),
            enabled,
            intent,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn intent(&self) -> SettingsIntent {
        self.intent
    }
}

pub(crate) fn groups_from_store(store: &SettingsStore) -> Vec<SettingsGroup> {
    let effective = store.all_effective();
    let setting = |key: &str| {
        effective
            .iter()
            .find(|item| item.key_name() == key)
            .expect("settings registry contains every Settings view key")
    };

    vec![
        SettingsGroup::new(
            "Screen",
            "Choose when LG Buddy blanks and restores your TV screen.",
            vec![
                row(setting("screen.backend")),
                row(setting("screen.idle_blank")),
                row(setting("screen.idle_timeout")),
                row(setting("screen.restore_policy")),
            ],
        ),
        SettingsGroup::new(
            "Sleep & Wake",
            "Coordinate your TV with the computer’s sleep and wake behavior.",
            vec![row(setting("system.sleep_wake_policy"))],
        ),
        SettingsGroup::new(
            "Updates",
            "Choose how LG Buddy checks for new versions.",
            vec![
                row(setting("updates.auto_check")),
                row(setting("updates.channel")),
            ],
        ),
    ]
}

fn row(setting: &EffectiveSetting) -> SettingsRow {
    let definition = setting.definition();
    let accepted_values = accepted_values_label(setting.key_name(), definition.value_type());
    let problem = setting.invalid_value().map(|invalid| {
        format!("Invalid configured value \"{invalid}\". Accepted values: {accepted_values}.")
    });

    let current_value_label = setting
        .value()
        .map(|value| value_label(setting.key_name(), value))
        .unwrap_or_else(|| {
            setting
                .invalid_value()
                .map(|_| "Invalid value".to_string())
                .unwrap_or_else(|| "Not configured".to_string())
        });

    SettingsRow::new(
        setting_title(setting.key_name()),
        definition.description(),
        current_value_label,
        source_label(setting.source()),
        definition
            .default_value()
            .map(|value| value_label(setting.key_name(), value))
            .unwrap_or_else(|| "Not configured".to_string()),
        accepted_values,
        problem,
    )
}

fn setting_title(key: &str) -> &'static str {
    match key {
        "screen.backend" => "Desktop integration",
        "screen.idle_blank" => "Idle blanking",
        "screen.idle_timeout" => "Idle timeout",
        "screen.restore_policy" => "Restore policy",
        "system.sleep_wake_policy" => "TV sleep & wake",
        "updates.auto_check" => "Automatic update checks",
        "updates.channel" => "Update channel",
        _ => "Setting",
    }
}

fn value_label(key: &str, value: SettingValue) -> String {
    match (key, value) {
        ("screen.backend", SettingValue::Enum("auto")) => "Automatic".to_string(),
        ("screen.backend", SettingValue::Enum("gnome")) => "GNOME".to_string(),
        ("screen.backend", SettingValue::Enum("wayland")) => "Wayland".to_string(),
        ("screen.backend", SettingValue::Enum("swayidle")) => "swayidle (deprecated)".to_string(),
        (_, SettingValue::Enum("enabled")) => "Enabled".to_string(),
        (_, SettingValue::Enum("disabled")) => "Disabled".to_string(),
        ("screen.restore_policy", SettingValue::Enum("conservative")) => "Conservative".to_string(),
        ("screen.restore_policy", SettingValue::Enum("aggressive")) => "Aggressive".to_string(),
        ("updates.channel", SettingValue::Enum("stable")) => "Stable".to_string(),
        ("updates.channel", SettingValue::Enum("prerelease")) => "Prerelease".to_string(),
        ("screen.idle_timeout", SettingValue::Integer(seconds)) => {
            format!("{seconds} seconds")
        }
        (_, value) => value.to_string(),
    }
}

fn source_label(source: SettingSource) -> &'static str {
    match source {
        SettingSource::Default => "Default",
        SettingSource::ConfigEnv => "Saved configuration",
        SettingSource::LegacyConfigEnv => "Legacy configuration",
        SettingSource::InvalidConfigEnv => "Invalid configuration",
        SettingSource::InvalidLegacyConfigEnv => "Invalid legacy configuration",
        SettingSource::Missing => "Not configured",
    }
}

fn accepted_values_label(key: &str, value_type: SettingType) -> String {
    match value_type {
        SettingType::Enum(enum_type) => {
            let values = enum_type
                .values()
                .iter()
                .map(|value| value_label(key, SettingValue::Enum(value)))
                .collect::<Vec<_>>()
                .join(", ");
            values
        }
        SettingType::Integer(integer_type) => {
            format!("{}–{} seconds", integer_type.min(), integer_type.max())
        }
        SettingType::Ipv4 => "An IPv4 address".to_string(),
        SettingType::MacAddress => "A MAC address like aa:bb:cc:dd:ee:ff".to_string(),
    }
}
