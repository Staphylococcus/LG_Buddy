use crate::presentation::brightness::UserFacingError;
use crate::settings::{EffectiveSetting, SettingSource, SettingType, SettingValue, SettingsStore};
use crate::settings_view::{BehaviorSetting, SettingsIntent};

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

/// The only settings exposed by the read/write Settings view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsEditStatus {
    Unchanged,
    Validating,
    Persisting,
    Persisted,
    Applying,
    Applied,
    ValidationFailed,
    PersistenceFailed,
    ApplyFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFeedbackSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsFeedback {
    severity: SettingsFeedbackSeverity,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsCommitPolicy {
    OnChange,
    OnFinalize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsChoice {
    value: String,
    label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEditor {
    Toggle {
        value: Option<bool>,
    },
    Choice {
        options: Vec<SettingsChoice>,
        selected: Option<usize>,
    },
    Number {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsGroup {
    title: String,
    description: String,
    rows: Vec<SettingsRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsRow {
    setting: BehaviorSetting,
    title: String,
    description: String,
    value_label: String,
    source_label: String,
    default_label: String,
    accepted_values_label: String,
    problem: Option<String>,
    editor: SettingsEditor,
    commit_policy: SettingsCommitPolicy,
    editor_enabled: bool,
    edit_status: SettingsEditStatus,
    feedback: Option<SettingsFeedback>,
    reset_action: Option<SettingsAction>,
    retry_apply_action: Option<SettingsAction>,
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

    pub(crate) fn failed_with_groups(error: UserFacingError, groups: Vec<SettingsGroup>) -> Self {
        Self {
            status: SettingsStatus::Failed(error),
            groups,
            retry_action: Some(SettingsAction::new("Retry", true, SettingsIntent::Retry)),
        }
    }

    /// Keep the current rows visible while an explicit refresh is in flight.
    /// This lets the renderer retain focus and any editor state until the new
    /// snapshot arrives.
    pub(crate) fn mark_loading(&mut self) {
        self.status = SettingsStatus::Loading {
            message: "Refreshing settings…".to_string(),
        };
        self.retry_action = None;
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

    pub(crate) fn row(&self, setting: BehaviorSetting) -> Option<&SettingsRow> {
        self.groups
            .iter()
            .flat_map(|group| group.rows())
            .find(|row| row.setting() == setting)
    }

    pub(crate) fn replace_row(&mut self, replacement: SettingsRow) -> Option<SettingsRow> {
        for group in &mut self.groups {
            if let Some(row) = group
                .rows
                .iter_mut()
                .find(|row| row.setting() == replacement.setting())
            {
                let mut replacement = replacement;
                replacement.editor_enabled = row.editor_enabled;
                replacement.reset_action = replacement.reset_action.map(|mut action| {
                    action.enabled = row.editor_enabled;
                    action
                });
                replacement.retry_apply_action = None;
                return Some(std::mem::replace(row, replacement));
            }
        }
        None
    }

    pub(crate) fn set_controls_available(&mut self, available: bool) {
        for group in &mut self.groups {
            for row in &mut group.rows {
                row.set_editor_enabled(available);
            }
        }
    }

    pub(crate) fn set_row_state(
        &mut self,
        setting: BehaviorSetting,
        status: SettingsEditStatus,
        feedback: Option<SettingsFeedback>,
        retry_apply: bool,
    ) {
        if let Some(row) = self
            .groups
            .iter_mut()
            .flat_map(|group| group.rows.iter_mut())
            .find(|row| row.setting == setting)
        {
            row.edit_status = status;
            row.feedback = feedback;
            row.retry_apply_action = retry_apply.then(|| {
                SettingsAction::new(
                    "Retry apply",
                    row.editor_enabled,
                    SettingsIntent::RetryApply(setting),
                )
            });
        }
    }

    /// Carry a warning about a saved value through a refresh when the saved
    /// value is still the same. A changed value means the warning has been
    /// reconciled and must not be resurrected.
    pub(crate) fn preserve_apply_failures_from(&mut self, previous: &Self) {
        for group in &mut self.groups {
            for row in &mut group.rows {
                let Some(old) = previous.row(row.setting) else {
                    continue;
                };
                if old.retry_apply_action.is_none()
                    || old.editor != row.editor
                    || old.problem != row.problem
                    || old.source_label != row.source_label
                {
                    continue;
                }
                row.edit_status = old.edit_status;
                row.feedback = old.feedback.clone();
                row.retry_apply_action = Some(SettingsAction::new(
                    "Retry apply",
                    row.editor_enabled,
                    SettingsIntent::RetryApply(row.setting),
                ));
            }
        }
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

impl SettingsFeedback {
    pub(crate) fn new(severity: SettingsFeedbackSeverity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
        }
    }

    pub fn severity(&self) -> SettingsFeedbackSeverity {
        self.severity
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl SettingsChoice {
    pub(crate) fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl SettingsEditor {
    pub fn as_toggle(&self) -> Option<Option<bool>> {
        match self {
            Self::Toggle { value } => Some(*value),
            Self::Choice { .. } | Self::Number { .. } => None,
        }
    }

    pub fn choices(&self) -> Option<&[SettingsChoice]> {
        match self {
            Self::Choice { options, .. } => Some(options),
            Self::Toggle { .. } | Self::Number { .. } => None,
        }
    }

    pub fn selected_choice(&self) -> Option<usize> {
        match self {
            Self::Choice { selected, .. } => *selected,
            Self::Toggle { .. } | Self::Number { .. } => None,
        }
    }

    pub fn number_text(&self) -> Option<&str> {
        match self {
            Self::Number { text } => Some(text),
            Self::Toggle { .. } | Self::Choice { .. } => None,
        }
    }
}

impl SettingsRow {
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

    pub fn setting(&self) -> BehaviorSetting {
        self.setting
    }

    pub fn editor(&self) -> &SettingsEditor {
        &self.editor
    }

    pub fn commit_policy(&self) -> SettingsCommitPolicy {
        self.commit_policy
    }

    pub fn editor_enabled(&self) -> bool {
        self.editor_enabled
    }

    pub fn edit_status(&self) -> SettingsEditStatus {
        self.edit_status
    }

    pub fn feedback(&self) -> Option<&SettingsFeedback> {
        self.feedback.as_ref()
    }

    pub fn reset_action(&self) -> Option<&SettingsAction> {
        self.reset_action.as_ref()
    }

    pub fn retry_apply_action(&self) -> Option<&SettingsAction> {
        self.retry_apply_action.as_ref()
    }

    pub(crate) fn set_editor_enabled(&mut self, enabled: bool) {
        self.editor_enabled = enabled;
        if let Some(action) = &mut self.reset_action {
            action.set_enabled(enabled);
        }
        if let Some(action) = &mut self.retry_apply_action {
            action.set_enabled(enabled);
        }
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
        self.intent.clone()
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
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
                row_from_effective(setting("screen.backend")),
                row_from_effective(setting("screen.idle_blank")),
                row_from_effective(setting("screen.idle_timeout")),
                row_from_effective(setting("screen.restore_policy")),
            ],
        ),
        SettingsGroup::new(
            "Sleep & Wake",
            "Coordinate your TV with the computer’s sleep and wake behavior.",
            vec![row_from_effective(setting("system.sleep_wake_policy"))],
        ),
        SettingsGroup::new(
            "Updates",
            "Choose how LG Buddy checks for new versions.",
            vec![
                row_from_effective(setting("updates.auto_check")),
                row_from_effective(setting("updates.channel")),
            ],
        ),
    ]
}

pub(crate) fn row_from_effective(setting: &EffectiveSetting) -> SettingsRow {
    let definition = setting.definition();
    let behavior_setting = BehaviorSetting::from_key(setting.key_name())
        .expect("settings registry contains every Settings view key");
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

    let editor = editor_for(behavior_setting, setting, definition.value_type());
    let commit_policy = match editor {
        SettingsEditor::Number { .. } => SettingsCommitPolicy::OnFinalize,
        SettingsEditor::Toggle { .. } | SettingsEditor::Choice { .. } => {
            SettingsCommitPolicy::OnChange
        }
    };

    SettingsRow {
        setting: behavior_setting,
        title: setting_title(setting.key_name()).to_string(),
        description: definition.description().to_string(),
        value_label: current_value_label,
        source_label: source_label(setting.source()).to_string(),
        default_label: definition
            .default_value()
            .map(|value| value_label(setting.key_name(), value))
            .unwrap_or_else(|| "Not configured".to_string()),
        accepted_values_label: accepted_values,
        problem,
        editor,
        commit_policy,
        editor_enabled: true,
        edit_status: SettingsEditStatus::Unchanged,
        feedback: None,
        reset_action: Some(SettingsAction::new(
            "Reset",
            true,
            SettingsIntent::Reset(behavior_setting),
        )),
        retry_apply_action: None,
    }
}

fn editor_for(
    setting: BehaviorSetting,
    effective: &EffectiveSetting,
    value_type: SettingType,
) -> SettingsEditor {
    match setting {
        BehaviorSetting::ScreenIdleBlank
        | BehaviorSetting::SystemSleepWakePolicy
        | BehaviorSetting::UpdatesAutoCheck => SettingsEditor::Toggle {
            value: effective.value().and_then(|value| match value {
                SettingValue::Enum("enabled") => Some(true),
                SettingValue::Enum("disabled") => Some(false),
                _ => None,
            }),
        },
        BehaviorSetting::ScreenBackend
        | BehaviorSetting::ScreenRestorePolicy
        | BehaviorSetting::UpdatesChannel => {
            let SettingType::Enum(enum_type) = value_type else {
                unreachable!("choice setting must be an enum")
            };
            let selected = effective.value().and_then(|value| {
                enum_type
                    .values()
                    .iter()
                    .position(|allowed| Some(*allowed) == value.as_enum())
            });
            SettingsEditor::Choice {
                options: enum_type
                    .values()
                    .iter()
                    .map(|value| {
                        SettingsChoice::new(
                            *value,
                            value_label(effective.key_name(), SettingValue::Enum(value)),
                        )
                    })
                    .collect(),
                selected,
            }
        }
        BehaviorSetting::ScreenIdleTimeout => SettingsEditor::Number {
            text: effective
                .value()
                .map(|value| value.to_string())
                .or_else(|| effective.invalid_value().map(str::to_string))
                .unwrap_or_default(),
        },
    }
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
