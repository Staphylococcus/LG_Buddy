use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use crate::config::{
    parse_config_entries, resolve_config_path, resolve_config_path_from_env, ConfigPathError,
    ConfigPathSources, DEFAULT_IDLE_TIMEOUT,
};

const SETTINGS_SUBCOMMANDS: &[&str] = &["list", "describe", "get", "set", "unset"];
const SCREEN_SERVICE_NAME: &str = "LG_Buddy_screen.service";

const READ_WRITE_OPERATIONS: &[SettingOperation] = &[
    SettingOperation::Get,
    SettingOperation::Describe,
    SettingOperation::Set,
    SettingOperation::Unset,
];
const READ_ONLY_OPERATIONS: &[SettingOperation] =
    &[SettingOperation::Get, SettingOperation::Describe];

const EMPTY_ALIASES: &[SettingAlias] = &[];
const SCREEN_RESTORE_POLICY_ALIASES: &[SettingAlias] = &[SettingAlias {
    from: "marker_only",
    to: "conservative",
}];

const SCREEN_BACKEND_VALUES: &[&str] = &["auto", "gnome", "swayidle"];
const SCREEN_RESTORE_POLICY_VALUES: &[&str] = &["conservative", "aggressive"];
const SYSTEM_SLEEP_WAKE_POLICY_VALUES: &[&str] = &["enabled", "disabled"];

const SETTING_DEFINITIONS: &[SettingDefinition] = &[
    SettingDefinition {
        key: "screen.backend",
        storage_key: "screen_backend",
        value_type: SettingType::Enum(EnumSettingType {
            values: SCREEN_BACKEND_VALUES,
            aliases: EMPTY_ALIASES,
        }),
        default_value: SettingValue::Enum("auto"),
        mutability: SettingMutability::ReadWrite,
        operations: READ_WRITE_OPERATIONS,
        apply_strategy: ApplyStrategy::RestartUserScreenService,
        description: "Screen backend selection for user-session blanking and restore behavior.",
    },
    SettingDefinition {
        key: "screen.idle_timeout",
        storage_key: "screen_idle_timeout",
        value_type: SettingType::Integer(IntegerSettingType {
            min: 1,
            max: 86_400,
        }),
        default_value: SettingValue::Integer(DEFAULT_IDLE_TIMEOUT as i64),
        mutability: SettingMutability::ReadWrite,
        operations: READ_WRITE_OPERATIONS,
        apply_strategy: ApplyStrategy::RestartUserScreenService,
        description: "Idle timeout in seconds before LG Buddy blanks the configured screen.",
    },
    SettingDefinition {
        key: "screen.restore_policy",
        storage_key: "screen_restore_policy",
        value_type: SettingType::Enum(EnumSettingType {
            values: SCREEN_RESTORE_POLICY_VALUES,
            aliases: SCREEN_RESTORE_POLICY_ALIASES,
        }),
        default_value: SettingValue::Enum("conservative"),
        mutability: SettingMutability::ReadWrite,
        operations: READ_WRITE_OPERATIONS,
        apply_strategy: ApplyStrategy::RestartUserScreenService,
        description: "Screen restore policy after LG Buddy blanks the configured screen.",
    },
    SettingDefinition {
        key: "system.sleep_wake_policy",
        storage_key: "system_sleep_wake_policy",
        value_type: SettingType::Enum(EnumSettingType {
            values: SYSTEM_SLEEP_WAKE_POLICY_VALUES,
            aliases: EMPTY_ALIASES,
        }),
        default_value: SettingValue::Enum("enabled"),
        mutability: SettingMutability::ReadOnly,
        operations: READ_ONLY_OPERATIONS,
        apply_strategy: ApplyStrategy::PendingLifecycleService,
        description: "System sleep and wake policy for lifecycle hooks.",
    },
];

pub static SETTINGS_REGISTRY: SettingsRegistry = SettingsRegistry {
    definitions: SETTING_DEFINITIONS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsCommand {
    List,
    Describe(Option<String>),
    Get(String),
    Set { key: String, value: String },
    Unset(String),
}

impl SettingsCommand {
    pub fn parse<I, S>(args: I) -> Result<Self, SettingsParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let Some(subcommand) = args.next() else {
            return Err(SettingsParseError::MissingSubcommand);
        };

        match subcommand.as_ref() {
            "list" => {
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::List)
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "list",
                        arguments: extra_args,
                    })
                }
            }
            "describe" => {
                let key = args.next().map(|arg| arg.as_ref().to_string());
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Describe(key))
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "describe",
                        arguments: extra_args,
                    })
                }
            }
            "get" => {
                let key = args
                    .next()
                    .ok_or(SettingsParseError::MissingKey { subcommand: "get" })?;
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Get(key.as_ref().to_string()))
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "get",
                        arguments: extra_args,
                    })
                }
            }
            "set" => {
                let key = args
                    .next()
                    .ok_or(SettingsParseError::MissingKey { subcommand: "set" })?;
                let value = args
                    .next()
                    .ok_or(SettingsParseError::MissingValue { subcommand: "set" })?;
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Set {
                        key: key.as_ref().to_string(),
                        value: value.as_ref().to_string(),
                    })
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "set",
                        arguments: extra_args,
                    })
                }
            }
            "unset" => {
                let key = args.next().ok_or(SettingsParseError::MissingKey {
                    subcommand: "unset",
                })?;
                let extra_args = collect_args(args);
                if extra_args.is_empty() {
                    Ok(Self::Unset(key.as_ref().to_string()))
                } else {
                    Err(SettingsParseError::UnexpectedArguments {
                        subcommand: "unset",
                        arguments: extra_args,
                    })
                }
            }
            other => Err(SettingsParseError::UnknownSubcommand(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Describe(_) => "describe",
            Self::Get(_) => "get",
            Self::Set { .. } => "set",
            Self::Unset(_) => "unset",
        }
    }

    pub fn is_mutation(&self) -> bool {
        matches!(self, Self::Set { .. } | Self::Unset(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    MissingKey {
        subcommand: &'static str,
    },
    MissingValue {
        subcommand: &'static str,
    },
    UnexpectedArguments {
        subcommand: &'static str,
        arguments: Vec<String>,
    },
}

impl fmt::Display for SettingsParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => {
                write!(
                    f,
                    "missing settings command; expected one of {}",
                    SETTINGS_SUBCOMMANDS.join(", ")
                )
            }
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown settings command `{subcommand}`")
            }
            Self::MissingKey { subcommand } => {
                write!(f, "missing setting key for `settings {subcommand}`")
            }
            Self::MissingValue { subcommand } => {
                write!(f, "missing setting value for `settings {subcommand}`")
            }
            Self::UnexpectedArguments {
                subcommand,
                arguments,
            } => {
                write!(
                    f,
                    "unexpected arguments for `settings {subcommand}`: {}",
                    arguments.join(" ")
                )
            }
        }
    }
}

impl std::error::Error for SettingsParseError {}

#[derive(Debug, Clone, Copy, Default)]
pub struct SettingsFormatter;

impl SettingsFormatter {
    pub fn write_get<W: io::Write>(
        &self,
        writer: &mut W,
        setting: EffectiveSetting,
    ) -> Result<(), SettingsError> {
        writeln!(writer, "{}", setting.value()).map_err(output_error)
    }

    pub fn write_list<W: io::Write>(
        &self,
        writer: &mut W,
        settings: &[EffectiveSetting],
    ) -> Result<(), SettingsError> {
        for setting in settings {
            writeln!(
                writer,
                "{}={} ({}, {}, ops: {})",
                setting.key_name(),
                setting.value(),
                setting.source().as_str(),
                setting.definition().mutability().as_str(),
                format_operations(setting.definition().supported_operations(), ",")
            )
            .map_err(output_error)?;
        }

        Ok(())
    }

    pub fn write_describe<W: io::Write>(
        &self,
        writer: &mut W,
        settings: &[EffectiveSetting],
    ) -> Result<(), SettingsError> {
        for (index, setting) in settings.iter().enumerate() {
            if index > 0 {
                writeln!(writer).map_err(output_error)?;
            }

            self.write_single_description(writer, *setting)?;
        }

        Ok(())
    }

    pub fn write_change<W: io::Write>(
        &self,
        writer: &mut W,
        change: &SettingsChange,
        apply: &SettingsApplyOutcome,
    ) -> Result<(), SettingsError> {
        let mutation = change.mutation();

        match (mutation.action(), change.file_changed()) {
            (SettingsMutationAction::Set, true) => {
                writeln!(
                    writer,
                    "{}={} (saved to {})",
                    mutation.key_name(),
                    mutation.new_value(),
                    change.path().display()
                )
                .map_err(output_error)?;
            }
            (SettingsMutationAction::Set, false) => {
                writeln!(
                    writer,
                    "{} already set to {} ({})",
                    mutation.key_name(),
                    mutation.new_value(),
                    change.path().display()
                )
                .map_err(output_error)?;
            }
            (SettingsMutationAction::Unset, true) => {
                writeln!(
                    writer,
                    "{} unset (saved to {})",
                    mutation.key_name(),
                    change.path().display()
                )
                .map_err(output_error)?;
            }
            (SettingsMutationAction::Unset, false) => {
                writeln!(
                    writer,
                    "{} already unset ({})",
                    mutation.key_name(),
                    change.path().display()
                )
                .map_err(output_error)?;
            }
        }

        if !change.file_changed() {
            writeln!(writer, "config: unchanged").map_err(output_error)?;
        }

        writeln!(writer, "apply: {apply}").map_err(output_error)?;
        Ok(())
    }

    fn write_single_description<W: io::Write>(
        &self,
        writer: &mut W,
        setting: EffectiveSetting,
    ) -> Result<(), SettingsError> {
        let definition = setting.definition();

        writeln!(writer, "{}", setting.key_name()).map_err(output_error)?;
        writeln!(writer, "  storage key: {}", setting.storage_key()).map_err(output_error)?;
        writeln!(writer, "  type: {}", definition.value_type().as_str()).map_err(output_error)?;
        writeln!(writer, "  current: {}", setting.value()).map_err(output_error)?;
        writeln!(writer, "  source: {}", setting.source().as_str()).map_err(output_error)?;
        writeln!(writer, "  default: {}", definition.default_value()).map_err(output_error)?;
        writeln!(writer, "  mutability: {}", definition.mutability().as_str())
            .map_err(output_error)?;
        writeln!(
            writer,
            "  supported operations: {}",
            format_operations(definition.supported_operations(), ", ")
        )
        .map_err(output_error)?;

        match definition.value_type() {
            SettingType::Enum(enum_type) => {
                writeln!(
                    writer,
                    "  allowed values: {}",
                    enum_type.values().join(", ")
                )
                .map_err(output_error)?;
                if !enum_type.aliases().is_empty() {
                    writeln!(writer, "  aliases: {}", format_aliases(enum_type.aliases()))
                        .map_err(output_error)?;
                }
            }
            SettingType::Integer(integer_type) => {
                writeln!(
                    writer,
                    "  range: {}..={}",
                    integer_type.min(),
                    integer_type.max()
                )
                .map_err(output_error)?;
            }
        }

        writeln!(writer, "  apply: {}", definition.apply_strategy().as_str())
            .map_err(output_error)?;
        writeln!(writer, "  description: {}", definition.description()).map_err(output_error)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ConfigEnvEditor {
    path: PathBuf,
    lines: Vec<String>,
}

impl ConfigEnvEditor {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let path = path.as_ref().to_path_buf();
        match fs::read_to_string(&path) {
            Ok(contents) => Ok(Self::parse(path, &contents)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::empty(path)),
            Err(err) => Err(SettingsError::ReadConfig {
                path,
                kind: err.kind(),
                message: err.to_string(),
            }),
        }
    }

    pub fn parse(path: impl Into<PathBuf>, contents: &str) -> Self {
        Self {
            path: path.into(),
            lines: contents.lines().map(str::to_string).collect(),
        }
    }

    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lines: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set(&mut self, storage_key: &str, value: SettingValue) -> bool {
        let value = value.to_string();

        if let Some(index) = self.last_key_index(storage_key) {
            let replacement = replace_config_line_value(&self.lines[index], storage_key, &value);
            let changed = self.lines[index] != replacement;
            self.lines[index] = replacement;
            changed
        } else {
            self.lines.push(format!("{storage_key}={value}"));
            true
        }
    }

    pub fn unset(&mut self, storage_key: &str) -> bool {
        let original_len = self.lines.len();
        self.lines
            .retain(|line| config_line_key(line) != Some(storage_key));
        self.lines.len() != original_len
    }

    pub fn save(&self) -> Result<(), SettingsError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|err| SettingsError::WriteConfig {
                    path: parent.to_path_buf(),
                    message: err.to_string(),
                })?;
            }
        }

        fs::write(&self.path, self.render()).map_err(|err| SettingsError::WriteConfig {
            path: self.path.clone(),
            message: err.to_string(),
        })
    }

    pub fn render(&self) -> String {
        if self.lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", self.lines.join("\n"))
        }
    }

    fn last_key_index(&self, storage_key: &str) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| config_line_key(line) == Some(storage_key))
            .map(|(index, _)| index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsMutationAction {
    Set,
    Unset,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingsMutation {
    definition: &'static SettingDefinition,
    old_value: SettingValue,
    old_source: SettingSource,
    new_value: SettingValue,
    action: SettingsMutationAction,
}

impl SettingsMutation {
    pub fn set(store: &SettingsStore, key: &str, value: &str) -> Result<Self, SettingsError> {
        let definition = SETTINGS_REGISTRY.get_by_name(key)?;
        definition.ensure_operation_supported(SettingOperation::Set)?;
        let old = store.effective_definition(definition);
        let new_value = definition.parse_value(value)?;

        Ok(Self {
            definition,
            old_value: old.value(),
            old_source: old.source(),
            new_value,
            action: SettingsMutationAction::Set,
        })
    }

    pub fn unset(store: &SettingsStore, key: &str) -> Result<Self, SettingsError> {
        let definition = SETTINGS_REGISTRY.get_by_name(key)?;
        definition.ensure_operation_supported(SettingOperation::Unset)?;
        let old = store.effective_definition(definition);

        Ok(Self {
            definition,
            old_value: old.value(),
            old_source: old.source(),
            new_value: definition.default_value(),
            action: SettingsMutationAction::Unset,
        })
    }

    pub fn definition(self) -> &'static SettingDefinition {
        self.definition
    }

    pub fn key_name(self) -> &'static str {
        self.definition.key_name()
    }

    pub fn storage_key(self) -> &'static str {
        self.definition.storage_key()
    }

    pub fn old_value(self) -> SettingValue {
        self.old_value
    }

    pub fn old_source(self) -> SettingSource {
        self.old_source
    }

    pub fn new_value(self) -> SettingValue {
        self.new_value
    }

    pub fn action(self) -> SettingsMutationAction {
        self.action
    }
}

#[derive(Debug, Clone)]
pub struct SettingsChange {
    mutation: SettingsMutation,
    path: PathBuf,
    file_changed: bool,
}

impl SettingsChange {
    pub fn mutation(&self) -> SettingsMutation {
        self.mutation
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_changed(&self) -> bool {
        self.file_changed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsApplyOutcome {
    Restarted { service: &'static str },
    NotInstalled { service: &'static str },
    InactiveDisabled { service: &'static str },
    Skipped { reason: String },
    NoActionRequired,
}

impl fmt::Display for SettingsApplyOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Restarted { service } => write!(f, "restarted {service}"),
            Self::NotInstalled { service } => {
                write!(
                    f,
                    "{service} is not installed; change applies when it is installed"
                )
            }
            Self::InactiveDisabled { service } => write!(
                f,
                "{service} is inactive and disabled; change applies when it is started"
            ),
            Self::Skipped { reason } => write!(f, "{reason}"),
            Self::NoActionRequired => write!(f, "no runtime apply action required"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserServiceState {
    Missing,
    InactiveDisabled,
    ActiveOrEnabled,
}

pub trait ServiceController {
    fn systemd_actions_disabled(&self) -> bool {
        false
    }

    fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError>;

    fn restart_user_service(&self, service: &str) -> Result<(), SettingsError>;
}

#[derive(Debug, Clone)]
pub struct SystemdUserServiceController {
    command_path: PathBuf,
    skip_systemd_actions: bool,
}

impl Default for SystemdUserServiceController {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SystemdUserServiceController {
    pub fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_SYSTEMCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
            skip_systemd_actions: env_truthy("LG_BUDDY_SKIP_SYSTEMD_ACTIONS"),
        }
    }

    fn user_systemctl_status(&self, args: &[&str]) -> io::Result<bool> {
        ProcessCommand::new(&self.command_path)
            .arg("--user")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
    }
}

impl ServiceController for SystemdUserServiceController {
    fn systemd_actions_disabled(&self) -> bool {
        self.skip_systemd_actions
    }

    fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError> {
        if !self
            .user_systemctl_status(&["cat", service])
            .unwrap_or(false)
        {
            return Ok(UserServiceState::Missing);
        }

        let active = self
            .user_systemctl_status(&["is-active", "--quiet", service])
            .unwrap_or(false);
        let enabled = self
            .user_systemctl_status(&["is-enabled", "--quiet", service])
            .unwrap_or(false);

        if active || enabled {
            Ok(UserServiceState::ActiveOrEnabled)
        } else {
            Ok(UserServiceState::InactiveDisabled)
        }
    }

    fn restart_user_service(&self, service: &str) -> Result<(), SettingsError> {
        let output = ProcessCommand::new(&self.command_path)
            .arg("--user")
            .arg("restart")
            .arg(service)
            .output()
            .map_err(|err| SettingsError::Apply {
                message: format!("could not run systemctl: {err}"),
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(SettingsError::Apply {
                message: format_command_failure(
                    output.status.code(),
                    &output.stdout,
                    &output.stderr,
                ),
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettingsApplier<C = SystemdUserServiceController> {
    service_controller: C,
}

impl SettingsApplier<SystemdUserServiceController> {
    pub fn from_env() -> Self {
        Self {
            service_controller: SystemdUserServiceController::from_env(),
        }
    }
}

impl<C: ServiceController> SettingsApplier<C> {
    pub fn new(service_controller: C) -> Self {
        Self { service_controller }
    }

    pub fn apply(&self, change: &SettingsChange) -> Result<SettingsApplyOutcome, SettingsError> {
        match change.mutation().definition().apply_strategy() {
            ApplyStrategy::RestartUserScreenService => self.apply_screen_service_restart(),
            ApplyStrategy::PendingLifecycleService => Ok(SettingsApplyOutcome::NoActionRequired),
        }
    }

    fn apply_screen_service_restart(&self) -> Result<SettingsApplyOutcome, SettingsError> {
        if self.service_controller.systemd_actions_disabled() {
            return Ok(SettingsApplyOutcome::Skipped {
                reason: "skipped systemd apply because LG_BUDDY_SKIP_SYSTEMD_ACTIONS=1".to_string(),
            });
        }

        match self
            .service_controller
            .user_service_state(SCREEN_SERVICE_NAME)?
        {
            UserServiceState::Missing => Ok(SettingsApplyOutcome::NotInstalled {
                service: SCREEN_SERVICE_NAME,
            }),
            UserServiceState::InactiveDisabled => Ok(SettingsApplyOutcome::InactiveDisabled {
                service: SCREEN_SERVICE_NAME,
            }),
            UserServiceState::ActiveOrEnabled => {
                self.service_controller
                    .restart_user_service(SCREEN_SERVICE_NAME)?;
                Ok(SettingsApplyOutcome::Restarted {
                    service: SCREEN_SERVICE_NAME,
                })
            }
        }
    }
}

#[derive(Debug)]
pub struct SettingsCommandRunner<C = SystemdUserServiceController> {
    store: SettingsStore,
    formatter: SettingsFormatter,
    applier: SettingsApplier<C>,
}

impl SettingsCommandRunner<SystemdUserServiceController> {
    pub fn new(store: SettingsStore) -> Self {
        Self::with_applier(store, SettingsApplier::from_env())
    }
}

impl<C: ServiceController> SettingsCommandRunner<C> {
    pub fn with_applier(store: SettingsStore, applier: SettingsApplier<C>) -> Self {
        Self {
            store,
            formatter: SettingsFormatter,
            applier,
        }
    }

    pub fn run<W: io::Write>(
        &self,
        command: SettingsCommand,
        writer: &mut W,
    ) -> Result<(), SettingsError> {
        match command {
            SettingsCommand::List => {
                let settings = self.store.all_effective();
                self.formatter.write_list(writer, &settings)
            }
            SettingsCommand::Describe(key) => match key {
                Some(key) => {
                    let setting = self.store.effective_by_name(&key)?;
                    self.formatter.write_describe(writer, &[setting])
                }
                None => {
                    let settings = self.store.all_effective();
                    self.formatter.write_describe(writer, &settings)
                }
            },
            SettingsCommand::Get(key) => {
                let setting = self.store.effective_by_name(&key)?;
                self.formatter.write_get(writer, setting)
            }
            SettingsCommand::Set { key, value } => {
                let mutation = SettingsMutation::set(&self.store, &key, &value)?;
                let change = persist_settings_mutation(self.store.path(), mutation)?;
                let apply = self.apply_after_persist(&change)?;
                self.formatter.write_change(writer, &change, &apply)
            }
            SettingsCommand::Unset(key) => {
                let mutation = SettingsMutation::unset(&self.store, &key)?;
                let change = persist_settings_mutation(self.store.path(), mutation)?;
                let apply = self.apply_after_persist(&change)?;
                self.formatter.write_change(writer, &change, &apply)
            }
        }
    }

    fn apply_after_persist(
        &self,
        change: &SettingsChange,
    ) -> Result<SettingsApplyOutcome, SettingsError> {
        self.applier
            .apply(change)
            .map_err(|err| SettingsError::ApplyAfterPersist {
                key: change.mutation().key_name().to_string(),
                path: change.path().to_path_buf(),
                message: err.to_string(),
            })
    }
}

pub fn run_settings_command<W: io::Write>(
    command: SettingsCommand,
    writer: &mut W,
) -> Result<(), SettingsError> {
    let store = SettingsStore::load_from_env()?;
    SettingsCommandRunner::new(store).run(command, writer)
}

fn persist_settings_mutation(
    path: &Path,
    mutation: SettingsMutation,
) -> Result<SettingsChange, SettingsError> {
    let mut editor = ConfigEnvEditor::load(path)?;
    let file_changed = match mutation.action() {
        SettingsMutationAction::Set => editor.set(mutation.storage_key(), mutation.new_value()),
        SettingsMutationAction::Unset => editor.unset(mutation.storage_key()),
    };

    if file_changed {
        editor.save()?;
    }

    Ok(SettingsChange {
        mutation,
        path: editor.path().to_path_buf(),
        file_changed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsStore {
    reader: ConfigEnvReader,
}

impl SettingsStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        ConfigEnvReader::load(path).map(Self::from_reader)
    }

    pub fn load_from_env() -> Result<Self, SettingsError> {
        let path = ConfigPathResolver::resolve_from_env()?;
        Self::load(path)
    }

    pub fn from_reader(reader: ConfigEnvReader) -> Self {
        Self { reader }
    }

    pub fn path(&self) -> &Path {
        self.reader.path()
    }

    pub fn raw_storage_value(&self, storage_key: &str) -> Option<&str> {
        self.reader.raw_value(storage_key)
    }

    pub fn effective_by_name(&self, key: &str) -> Result<EffectiveSetting, SettingsError> {
        self.effective(SettingKey::parse(key)?)
    }

    pub fn effective(&self, key: SettingKey<'_>) -> Result<EffectiveSetting, SettingsError> {
        let definition = SETTINGS_REGISTRY.get(key)?;
        Ok(self.effective_definition(definition))
    }

    pub fn effective_definition(&self, definition: &'static SettingDefinition) -> EffectiveSetting {
        let raw_value = self.reader.raw_value(definition.storage_key());
        let parsed_value = raw_value.and_then(|value| definition.parse_value(value).ok());

        match parsed_value {
            Some(value) => EffectiveSetting {
                definition,
                value,
                source: SettingSource::ConfigEnv,
            },
            None => EffectiveSetting {
                definition,
                value: definition.default_value(),
                source: SettingSource::Default,
            },
        }
    }

    pub fn all_effective(&self) -> Vec<EffectiveSetting> {
        SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| self.effective_definition(definition))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEnvReader {
    path: PathBuf,
    entries: HashMap<String, String>,
}

impl ConfigEnvReader {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let path = path.as_ref().to_path_buf();
        match fs::read_to_string(&path) {
            Ok(contents) => Ok(Self::parse(path, &contents)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::empty(path)),
            Err(err) => Err(SettingsError::ReadConfig {
                path,
                kind: err.kind(),
                message: err.to_string(),
            }),
        }
    }

    pub fn parse(path: impl Into<PathBuf>, contents: &str) -> Self {
        Self {
            path: path.into(),
            entries: parse_config_entries(contents),
        }
    }

    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            entries: HashMap::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw_value(&self, storage_key: &str) -> Option<&str> {
        self.entries.get(storage_key).map(String::as_str)
    }

    pub fn into_store(self) -> SettingsStore {
        SettingsStore::from_reader(self)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConfigPathResolver;

impl ConfigPathResolver {
    pub fn resolve(sources: ConfigPathSources<'_>) -> Result<PathBuf, SettingsError> {
        resolve_config_path(sources).map_err(SettingsError::ConfigPath)
    }

    pub fn resolve_from_env() -> Result<PathBuf, SettingsError> {
        resolve_config_path_from_env().map_err(SettingsError::ConfigPath)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EffectiveSetting {
    definition: &'static SettingDefinition,
    value: SettingValue,
    source: SettingSource,
}

impl EffectiveSetting {
    pub fn definition(self) -> &'static SettingDefinition {
        self.definition
    }

    pub fn key(self) -> SettingKey<'static> {
        self.definition.key()
    }

    pub fn key_name(self) -> &'static str {
        self.definition.key_name()
    }

    pub fn storage_key(self) -> &'static str {
        self.definition.storage_key()
    }

    pub fn value(self) -> SettingValue {
        self.value
    }

    pub fn source(self) -> SettingSource {
        self.source
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    Default,
    ConfigEnv,
}

impl SettingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::ConfigEnv => "config.env",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SettingsRegistry {
    definitions: &'static [SettingDefinition],
}

impl SettingsRegistry {
    pub fn all(&self) -> &'static [SettingDefinition] {
        self.definitions
    }

    pub fn get(&self, key: SettingKey<'_>) -> Result<&'static SettingDefinition, SettingsError> {
        self.definitions
            .iter()
            .find(|definition| definition.key == key.as_str())
            .ok_or_else(|| SettingsError::UnknownKey(key.as_str().to_string()))
    }

    pub fn get_by_name(&self, key: &str) -> Result<&'static SettingDefinition, SettingsError> {
        self.get(SettingKey::parse(key)?)
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        let mut public_keys = HashSet::new();
        let mut storage_keys = HashSet::new();

        for definition in self.definitions {
            SettingKey::parse(definition.key)?;
            validate_storage_key(definition.storage_key).map_err(|reason| {
                SettingsError::RegistryInvariant(format!(
                    "invalid storage key `{}` for `{}`: {reason}",
                    definition.storage_key, definition.key
                ))
            })?;

            if !public_keys.insert(definition.key) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "duplicate setting key `{}`",
                    definition.key
                )));
            }

            if !storage_keys.insert(definition.storage_key) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "duplicate storage key `{}`",
                    definition.storage_key
                )));
            }

            definition.validate_type_metadata()?;
            definition.validate_default()?;
            definition.validate_operation_metadata()?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SettingKey<'a>(&'a str);

impl<'a> SettingKey<'a> {
    pub fn parse(value: &'a str) -> Result<Self, SettingsError> {
        validate_setting_key(value)
            .map_err(|reason| SettingsError::InvalidKey {
                key: value.to_string(),
                reason,
            })
            .map(|()| Self(value))
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl fmt::Display for SettingKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SettingDefinition {
    key: &'static str,
    storage_key: &'static str,
    value_type: SettingType,
    default_value: SettingValue,
    mutability: SettingMutability,
    operations: &'static [SettingOperation],
    apply_strategy: ApplyStrategy,
    description: &'static str,
}

impl SettingDefinition {
    pub fn key(&self) -> SettingKey<'static> {
        SettingKey(self.key)
    }

    pub fn key_name(&self) -> &'static str {
        self.key
    }

    pub fn storage_key(&self) -> &'static str {
        self.storage_key
    }

    pub fn value_type(&self) -> SettingType {
        self.value_type
    }

    pub fn default_value(&self) -> SettingValue {
        self.default_value
    }

    pub fn mutability(&self) -> SettingMutability {
        self.mutability
    }

    pub fn supported_operations(&self) -> &'static [SettingOperation] {
        self.operations
    }

    pub fn apply_strategy(&self) -> ApplyStrategy {
        self.apply_strategy
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn supports_operation(&self, operation: SettingOperation) -> bool {
        self.operations.contains(&operation)
    }

    pub fn ensure_operation_supported(
        &self,
        operation: SettingOperation,
    ) -> Result<(), SettingsError> {
        if self.supports_operation(operation) {
            Ok(())
        } else {
            Err(SettingsError::UnsupportedOperation {
                key: self.key.to_string(),
                operation,
            })
        }
    }

    pub fn parse_value(&self, value: &str) -> Result<SettingValue, SettingsError> {
        self.value_type.parse_value(self.key, value)
    }

    fn validate_type_metadata(&self) -> Result<(), SettingsError> {
        match self.value_type {
            SettingType::Enum(enum_type) => enum_type.validate(self.key),
            SettingType::Integer(integer_type) => integer_type.validate(self.key),
        }
    }

    fn validate_default(&self) -> Result<(), SettingsError> {
        self.value_type
            .validate_value(self.key, self.default_value)
            .map_err(|err| match err {
                SettingsError::InvalidValue {
                    key,
                    value,
                    expected,
                } => SettingsError::RegistryInvariant(format!(
                    "invalid default value `{value}` for `{key}`: expected {expected}"
                )),
                other => other,
            })
    }

    fn validate_operation_metadata(&self) -> Result<(), SettingsError> {
        if self.operations.is_empty() {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{}` must support at least one operation",
                self.key
            )));
        }

        match self.mutability {
            SettingMutability::ReadWrite => {
                for operation in READ_WRITE_OPERATIONS {
                    if !self.operations.contains(operation) {
                        return Err(SettingsError::RegistryInvariant(format!(
                            "`{}` is read-write but does not support `{}`",
                            self.key,
                            operation.as_str()
                        )));
                    }
                }
            }
            SettingMutability::ReadOnly => {
                for operation in [SettingOperation::Set, SettingOperation::Unset] {
                    if self.operations.contains(&operation) {
                        return Err(SettingsError::RegistryInvariant(format!(
                            "`{}` is read-only but supports `{}`",
                            self.key,
                            operation.as_str()
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingType {
    Enum(EnumSettingType),
    Integer(IntegerSettingType),
}

impl SettingType {
    pub fn parse_value(self, key: &str, value: &str) -> Result<SettingValue, SettingsError> {
        match self {
            Self::Enum(enum_type) => enum_type
                .canonicalize(value)
                .map(SettingValue::Enum)
                .ok_or_else(|| SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: enum_type.expected(),
                }),
            Self::Integer(integer_type) => match value.parse::<i64>() {
                Ok(parsed) if integer_type.contains(parsed) => Ok(SettingValue::Integer(parsed)),
                _ => Err(SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: integer_type.expected(),
                }),
            },
        }
    }

    pub fn validate_value(self, key: &str, value: SettingValue) -> Result<(), SettingsError> {
        match (self, value) {
            (Self::Enum(enum_type), SettingValue::Enum(value)) => {
                match enum_type.canonicalize(value) {
                    Some(canonical) if canonical == value => Ok(()),
                    _ => Err(SettingsError::InvalidValue {
                        key: key.to_string(),
                        value: value.to_string(),
                        expected: enum_type.expected(),
                    }),
                }
            }
            (Self::Integer(integer_type), SettingValue::Integer(value))
                if integer_type.contains(value) =>
            {
                Ok(())
            }
            (Self::Integer(integer_type), SettingValue::Integer(value)) => {
                Err(SettingsError::InvalidValue {
                    key: key.to_string(),
                    value: value.to_string(),
                    expected: integer_type.expected(),
                })
            }
            (expected_type, actual_value) => Err(SettingsError::InvalidValue {
                key: key.to_string(),
                value: actual_value.to_string(),
                expected: expected_type.expected(),
            }),
        }
    }

    pub fn expected(self) -> String {
        match self {
            Self::Enum(enum_type) => enum_type.expected(),
            Self::Integer(integer_type) => integer_type.expected(),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enum(_) => "enum",
            Self::Integer(_) => "integer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumSettingType {
    values: &'static [&'static str],
    aliases: &'static [SettingAlias],
}

impl EnumSettingType {
    pub fn values(self) -> &'static [&'static str] {
        self.values
    }

    pub fn aliases(self) -> &'static [SettingAlias] {
        self.aliases
    }

    pub fn canonicalize(self, value: &str) -> Option<&'static str> {
        if let Some(canonical) = self
            .values
            .iter()
            .copied()
            .find(|allowed| *allowed == value)
        {
            return Some(canonical);
        }

        self.aliases
            .iter()
            .find(|alias| alias.from == value)
            .map(|alias| alias.to)
    }

    fn expected(self) -> String {
        format!("one of {}", self.values.join(", "))
    }

    fn validate(self, key: &str) -> Result<(), SettingsError> {
        if self.values.is_empty() {
            return Err(SettingsError::RegistryInvariant(format!(
                "`{key}` enum settings must define at least one value"
            )));
        }

        let mut values = HashSet::new();
        for value in self.values {
            if value.is_empty() {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum settings must not define empty values"
                )));
            }

            if !values.insert(*value) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum setting has duplicate value `{value}`"
                )));
            }
        }

        let mut aliases = HashSet::new();
        for alias in self.aliases {
            if alias.from.is_empty() || alias.to.is_empty() {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum settings must not define empty aliases"
                )));
            }

            if values.contains(alias.from) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum alias `{}` duplicates a canonical value",
                    alias.from
                )));
            }

            if !values.contains(alias.to) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum alias `{}` points to unknown value `{}`",
                    alias.from, alias.to
                )));
            }

            if !aliases.insert(alias.from) {
                return Err(SettingsError::RegistryInvariant(format!(
                    "`{key}` enum setting has duplicate alias `{}`",
                    alias.from
                )));
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingAlias {
    from: &'static str,
    to: &'static str,
}

impl SettingAlias {
    pub fn from(self) -> &'static str {
        self.from
    }

    pub fn to(self) -> &'static str {
        self.to
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegerSettingType {
    min: i64,
    max: i64,
}

impl IntegerSettingType {
    pub fn min(self) -> i64 {
        self.min
    }

    pub fn max(self) -> i64 {
        self.max
    }

    pub fn contains(self, value: i64) -> bool {
        value >= self.min && value <= self.max
    }

    fn expected(self) -> String {
        format!("an integer from {} to {}", self.min, self.max)
    }

    fn validate(self, key: &str) -> Result<(), SettingsError> {
        if self.min <= self.max {
            Ok(())
        } else {
            Err(SettingsError::RegistryInvariant(format!(
                "`{key}` integer setting has an invalid range {}..{}",
                self.min, self.max
            )))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingValue {
    Enum(&'static str),
    Integer(i64),
}

impl SettingValue {
    pub fn as_enum(self) -> Option<&'static str> {
        match self {
            Self::Enum(value) => Some(value),
            Self::Integer(_) => None,
        }
    }

    pub fn as_integer(self) -> Option<i64> {
        match self {
            Self::Enum(_) => None,
            Self::Integer(value) => Some(value),
        }
    }
}

impl fmt::Display for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enum(value) => write!(f, "{value}"),
            Self::Integer(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingMutability {
    ReadOnly,
    ReadWrite,
}

impl SettingMutability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingOperation {
    Get,
    Describe,
    Set,
    Unset,
}

impl SettingOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Describe => "describe",
            Self::Set => "set",
            Self::Unset => "unset",
        }
    }
}

impl fmt::Display for SettingOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    RestartUserScreenService,
    PendingLifecycleService,
}

impl ApplyStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RestartUserScreenService => "restart-user-screen-service",
            Self::PendingLifecycleService => "pending-lifecycle-service",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    ConfigPath(ConfigPathError),
    ReadConfig {
        path: PathBuf,
        kind: io::ErrorKind,
        message: String,
    },
    WriteConfig {
        path: PathBuf,
        message: String,
    },
    Apply {
        message: String,
    },
    ApplyAfterPersist {
        key: String,
        path: PathBuf,
        message: String,
    },
    WriteOutput(String),
    InvalidKey {
        key: String,
        reason: &'static str,
    },
    UnknownKey(String),
    InvalidValue {
        key: String,
        value: String,
        expected: String,
    },
    UnsupportedOperation {
        key: String,
        operation: SettingOperation,
    },
    RegistryInvariant(String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigPath(err) => write!(f, "{err}"),
            Self::ReadConfig { path, message, .. } => {
                write!(
                    f,
                    "could not read settings config `{}`: {message}",
                    path.display()
                )
            }
            Self::WriteConfig { path, message } => {
                write!(
                    f,
                    "could not write settings config `{}`: {message}",
                    path.display()
                )
            }
            Self::Apply { message } => write!(f, "{message}"),
            Self::ApplyAfterPersist { key, path, message } => write!(
                f,
                "setting `{key}` was saved to `{}` but could not be applied: {message}. Restart LG Buddy or rerun the command after fixing the apply error.",
                path.display()
            ),
            Self::WriteOutput(message) => write!(f, "{message}"),
            Self::InvalidKey { key, reason } => {
                write!(f, "invalid setting key `{key}`: {reason}")
            }
            Self::UnknownKey(key) => write!(f, "unknown setting `{key}`"),
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(
                f,
                "invalid value for setting `{key}`: `{value}`; expected {expected}"
            ),
            Self::UnsupportedOperation { key, operation } => {
                write!(
                    f,
                    "setting `{key}` does not support `{}`",
                    operation.as_str()
                )
            }
            Self::RegistryInvariant(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigPath(err) => Some(err),
            Self::ReadConfig { .. }
            | Self::WriteConfig { .. }
            | Self::Apply { .. }
            | Self::ApplyAfterPersist { .. }
            | Self::WriteOutput(_)
            | Self::InvalidKey { .. }
            | Self::UnknownKey(_)
            | Self::InvalidValue { .. }
            | Self::UnsupportedOperation { .. }
            | Self::RegistryInvariant(_) => None,
        }
    }
}

fn collect_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect()
}

fn format_operations(operations: &[SettingOperation], separator: &str) -> String {
    operations
        .iter()
        .map(|operation| operation.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}

fn format_aliases(aliases: &[SettingAlias]) -> String {
    aliases
        .iter()
        .map(|alias| format!("{} -> {}", alias.from(), alias.to()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn output_error(err: io::Error) -> SettingsError {
    SettingsError::WriteOutput(err.to_string())
}

fn config_line_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let (key, _) = trimmed.split_once('=')?;
    Some(key.trim())
}

fn replace_config_line_value(line: &str, storage_key: &str, value: &str) -> String {
    let indentation: String = line
        .chars()
        .take_while(|character| character.is_whitespace())
        .collect();
    let suffix = line
        .split_once('=')
        .map(|(_, existing_value)| config_value_suffix(existing_value))
        .unwrap_or_default();

    format!("{indentation}{storage_key}={value}{suffix}")
}

fn config_value_suffix(value: &str) -> &str {
    let Some(comment_start) = value.find('#') else {
        return "";
    };

    let before_comment = &value[..comment_start];
    let suffix_start = before_comment
        .char_indices()
        .rev()
        .find(|(_, character)| !character.is_whitespace())
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or(0);

    &value[suffix_start..]
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "True" | "yes" | "YES" | "Yes"
            )
        })
        .unwrap_or(false)
}

fn format_command_failure(status_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> String {
    let status = status_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();

    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => format!("systemctl exited with status {status}"),
        (false, true) => format!("systemctl exited with status {status}: {stdout}"),
        (true, false) => format!("systemctl exited with status {status}: {stderr}"),
        (false, false) => format!("systemctl exited with status {status}: {stderr}; {stdout}"),
    }
}

fn validate_setting_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }

    let mut last_was_dot = true;
    let mut has_dot = false;

    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'_' => {
                last_was_dot = false;
            }
            b'.' if !last_was_dot => {
                last_was_dot = true;
                has_dot = true;
            }
            b'.' => return Err("must not contain empty segments"),
            _ => {
                return Err(
                    "must contain only ASCII lowercase letters, digits, underscores, and dots",
                )
            }
        }
    }

    if last_was_dot {
        return Err("must not end with a dot");
    }

    if !has_dot {
        return Err("must contain at least one dot");
    }

    Ok(())
}

fn validate_storage_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }

    if value
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        Ok(())
    } else {
        Err("must contain only ASCII lowercase letters, digits, and underscores")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApplyStrategy, ConfigEnvReader, ConfigPathResolver, ServiceController, SettingKey,
        SettingMutability, SettingOperation, SettingSource, SettingType, SettingValue,
        SettingsApplier, SettingsCommand, SettingsCommandRunner, SettingsError, SettingsParseError,
        SettingsStore, UserServiceState, SETTINGS_REGISTRY,
    };
    use crate::config::ConfigPathSources;
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn registry_metadata_is_internally_valid() {
        SETTINGS_REGISTRY.validate().unwrap();
    }

    #[test]
    fn registry_contains_initial_settings() {
        let keys: Vec<&str> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| definition.key_name())
            .collect();

        assert_eq!(
            keys,
            vec![
                "screen.backend",
                "screen.idle_timeout",
                "screen.restore_policy",
                "system.sleep_wake_policy",
            ]
        );
    }

    #[test]
    fn registry_maps_public_keys_to_storage_keys() {
        let mappings: Vec<(&str, &str)> = SETTINGS_REGISTRY
            .all()
            .iter()
            .map(|definition| (definition.key_name(), definition.storage_key()))
            .collect();

        assert_eq!(
            mappings,
            vec![
                ("screen.backend", "screen_backend"),
                ("screen.idle_timeout", "screen_idle_timeout"),
                ("screen.restore_policy", "screen_restore_policy"),
                ("system.sleep_wake_policy", "system_sleep_wake_policy"),
            ]
        );
    }

    #[test]
    fn settings_command_parser_accepts_supported_commands() {
        assert_eq!(SettingsCommand::parse(["list"]), Ok(SettingsCommand::List));
        assert_eq!(
            SettingsCommand::parse(["describe"]),
            Ok(SettingsCommand::Describe(None))
        );
        assert_eq!(
            SettingsCommand::parse(["describe", "screen.backend"]),
            Ok(SettingsCommand::Describe(Some(
                "screen.backend".to_string()
            )))
        );
        assert_eq!(
            SettingsCommand::parse(["get", "screen.backend"]),
            Ok(SettingsCommand::Get("screen.backend".to_string()))
        );
        assert_eq!(
            SettingsCommand::parse(["set", "screen.backend", "gnome"]),
            Ok(SettingsCommand::Set {
                key: "screen.backend".to_string(),
                value: "gnome".to_string(),
            })
        );
        assert_eq!(
            SettingsCommand::parse(["unset", "screen.backend"]),
            Ok(SettingsCommand::Unset("screen.backend".to_string()))
        );
    }

    #[test]
    fn settings_command_parser_rejects_invalid_shapes() {
        assert_eq!(
            SettingsCommand::parse(Vec::<String>::new()),
            Err(SettingsParseError::MissingSubcommand)
        );
        assert_eq!(
            SettingsCommand::parse(["show"]),
            Err(SettingsParseError::UnknownSubcommand("show".to_string()))
        );
        assert_eq!(
            SettingsCommand::parse(["get"]),
            Err(SettingsParseError::MissingKey { subcommand: "get" })
        );
        assert_eq!(
            SettingsCommand::parse(["set", "screen.backend"]),
            Err(SettingsParseError::MissingValue { subcommand: "set" })
        );
        assert_eq!(
            SettingsCommand::parse(["describe", "screen.backend", "extra"]),
            Err(SettingsParseError::UnexpectedArguments {
                subcommand: "describe",
                arguments: vec!["extra".to_string()],
            })
        );
    }

    #[test]
    fn settings_runner_lists_values_sources_mutability_and_operations() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=gnome
            system_sleep_wake_policy=disabled
            ",
        )
        .into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner.run(SettingsCommand::List, &mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output,
            "\
screen.backend=gnome (config.env, read-write, ops: get,describe,set,unset)
screen.idle_timeout=300 (default, read-write, ops: get,describe,set,unset)
screen.restore_policy=conservative (default, read-write, ops: get,describe,set,unset)
system.sleep_wake_policy=disabled (config.env, read-only, ops: get,describe)
"
        );
    }

    #[test]
    fn settings_runner_get_prints_value_only() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_idle_timeout=450\n").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Get("screen.idle_timeout".to_string()),
                &mut output,
            )
            .unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "450\n");
    }

    #[test]
    fn settings_runner_describe_includes_metadata_and_operations() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_restore_policy=marker_only\n")
                .into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Describe(Some("screen.restore_policy".to_string())),
                &mut output,
            )
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.restore_policy\n"));
        assert!(output.contains("  storage key: screen_restore_policy\n"));
        assert!(output.contains("  type: enum\n"));
        assert!(output.contains("  current: conservative\n"));
        assert!(output.contains("  source: config.env\n"));
        assert!(output.contains("  default: conservative\n"));
        assert!(output.contains("  mutability: read-write\n"));
        assert!(output.contains("  supported operations: get, describe, set, unset\n"));
        assert!(output.contains("  allowed values: conservative, aggressive\n"));
        assert!(output.contains("  aliases: marker_only -> conservative\n"));
        assert!(output.contains("  apply: restart-user-screen-service\n"));
    }

    #[test]
    fn settings_runner_describe_without_key_describes_all_settings() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        runner
            .run(SettingsCommand::Describe(None), &mut output)
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.backend\n"));
        assert!(output.contains("screen.idle_timeout\n"));
        assert!(output.contains("  range: 1..=86400\n"));
        assert!(output.contains("system.sleep_wake_policy\n"));
        assert!(output.contains("  supported operations: get, describe\n"));
    }

    #[test]
    fn settings_runner_rejects_unknown_keys() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();
        let runner = SettingsCommandRunner::new(store);
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Get("screen.unknown".to_string()),
                &mut output,
            )
            .unwrap_err();

        assert_eq!(err, SettingsError::UnknownKey("screen.unknown".to_string()));
        assert!(output.is_empty());
    }

    #[test]
    fn settings_runner_sets_value_and_restarts_active_screen_service() {
        let path = unique_test_path("set");
        fs::write(
            &path,
            "\
tv_ip=192.168.1.42
screen_backend=swayidle # keep backend comment
screen_idle_timeout=300
",
        )
        .unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let fake_service = FakeServiceController::active_or_enabled();
        let restarts = fake_service.restarts.clone();
        let runner = SettingsCommandRunner::with_applier(store, SettingsApplier::new(fake_service));
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "gnome".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(restarts.get(), 1);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
tv_ip=192.168.1.42
screen_backend=gnome # keep backend comment
screen_idle_timeout=300
"
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.backend=gnome (saved to "));
        assert!(output.contains("apply: restarted LG_Buddy_screen.service\n"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_unsets_value_and_removes_all_duplicate_keys() {
        let path = unique_test_path("unset");
        fs::write(
            &path,
            "\
screen_backend=swayidle
screen_idle_timeout=120
screen_idle_timeout=450
screen_restore_policy=aggressive
",
        )
        .unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Unset("screen.idle_timeout".to_string()),
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "\
screen_backend=swayidle
screen_restore_policy=aggressive
"
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: LG_Buddy_screen.service is not installed"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_unsets_absent_key_without_creating_config() {
        let path = unique_test_path("unset-absent");
        let _ = fs::remove_file(&path);
        let store = ConfigEnvReader::empty(&path).into_store();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::missing()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Unset("screen.backend".to_string()),
                &mut output,
            )
            .unwrap();

        assert!(!path.exists());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("screen.backend already unset"));
        assert!(output.contains("config: unchanged\n"));
        assert!(output.contains("apply: LG_Buddy_screen.service is not installed"));
    }

    #[test]
    fn settings_runner_rejects_invalid_write_without_touching_config() {
        let path = unique_test_path("invalid");
        fs::write(&path, "screen_backend=swayidle\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::active_or_enabled()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "kde".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert!(matches!(err, SettingsError::InvalidValue { .. }));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_backend=swayidle\n"
        );
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_rejects_unknown_write_without_touching_config() {
        let path = unique_test_path("unknown");
        fs::write(&path, "screen_backend=swayidle\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::active_or_enabled()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "screen.unknown".to_string(),
                    value: "gnome".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert_eq!(err, SettingsError::UnknownKey("screen.unknown".to_string()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_backend=swayidle\n"
        );
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_rejects_read_only_write_without_touching_config() {
        let path = unique_test_path("readonly");
        fs::write(&path, "system_sleep_wake_policy=disabled\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::active_or_enabled()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "system.sleep_wake_policy".to_string(),
                    value: "enabled".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert_eq!(
            err,
            SettingsError::UnsupportedOperation {
                key: "system.sleep_wake_policy".to_string(),
                operation: SettingOperation::Set,
            }
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "system_sleep_wake_policy=disabled\n"
        );
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_runner_creates_parent_directory_for_valid_write() {
        let root = unique_test_path("parent").with_extension("");
        let path = root.join("nested").join("config.env");
        let _ = fs::remove_dir_all(&root);
        let store = ConfigEnvReader::empty(&path).into_store();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::inactive_disabled()),
        );
        let mut output = Vec::new();

        runner
            .run(
                SettingsCommand::Set {
                    key: "screen.idle_timeout".to_string(),
                    value: "600".to_string(),
                },
                &mut output,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_idle_timeout=600\n"
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: LG_Buddy_screen.service is inactive and disabled"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_runner_reports_apply_failure_after_persisting_value() {
        let path = unique_test_path("apply-fail");
        fs::write(&path, "screen_backend=swayidle\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let runner = SettingsCommandRunner::with_applier(
            store,
            SettingsApplier::new(FakeServiceController::failing_restart()),
        );
        let mut output = Vec::new();

        let err = runner
            .run(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "gnome".to_string(),
                },
                &mut output,
            )
            .unwrap_err();

        assert_eq!(fs::read_to_string(&path).unwrap(), "screen_backend=gnome\n");
        assert!(matches!(err, SettingsError::ApplyAfterPersist { .. }));
        assert!(err.to_string().contains("was saved"));
        assert!(output.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn config_path_resolver_reuses_config_path_resolution() {
        let resolved = ConfigPathResolver::resolve(ConfigPathSources {
            explicit_config: Some(Path::new("/tmp/custom.env")),
            install_pointer_config: Some(Path::new("/tmp/pointer.env")),
            sudo_user_home: Some(Path::new("/tmp/sudo-home")),
            xdg_config_home: Some(Path::new("/tmp/xdg")),
            home: Some(Path::new("/tmp/home")),
        });

        assert_eq!(resolved, Ok(PathBuf::from("/tmp/custom.env")));
    }

    #[test]
    fn config_env_reader_sanitizes_comments_and_uses_last_duplicate_value() {
        let reader = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=swayidle
            screen_backend=gnome # use GNOME when available
            unused=value
            ",
        );

        assert_eq!(reader.raw_value("screen_backend"), Some("gnome"));
        assert_eq!(reader.raw_value("unused"), Some("value"));
        assert_eq!(reader.raw_value("missing"), None);
    }

    #[test]
    fn settings_store_reads_existing_values_without_required_tv_config() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=gnome
            screen_idle_timeout=450
            screen_restore_policy=aggressive
            system_sleep_wake_policy=disabled
            ",
        )
        .into_store();

        let backend = store.effective_by_name("screen.backend").unwrap();
        assert_eq!(backend.value(), SettingValue::Enum("gnome"));
        assert_eq!(backend.source(), SettingSource::ConfigEnv);

        let idle_timeout = store.effective_by_name("screen.idle_timeout").unwrap();
        assert_eq!(idle_timeout.value(), SettingValue::Integer(450));
        assert_eq!(idle_timeout.source(), SettingSource::ConfigEnv);

        let restore_policy = store.effective_by_name("screen.restore_policy").unwrap();
        assert_eq!(restore_policy.value(), SettingValue::Enum("aggressive"));
        assert_eq!(restore_policy.source(), SettingSource::ConfigEnv);

        let sleep_policy = store.effective_by_name("system.sleep_wake_policy").unwrap();
        assert_eq!(sleep_policy.value(), SettingValue::Enum("disabled"));
        assert_eq!(sleep_policy.source(), SettingSource::ConfigEnv);
    }

    #[test]
    fn settings_store_uses_defaults_for_missing_values() {
        let store = ConfigEnvReader::parse("/tmp/config.env", "").into_store();

        let effective = store.effective_by_name("screen.idle_timeout").unwrap();

        assert_eq!(effective.value(), SettingValue::Integer(300));
        assert_eq!(effective.source(), SettingSource::Default);
    }

    #[test]
    fn settings_store_uses_defaults_for_invalid_optional_values() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=not-a-backend
            screen_idle_timeout=not-a-number
            screen_restore_policy=not-a-policy
            ",
        )
        .into_store();

        let backend = store.effective_by_name("screen.backend").unwrap();
        assert_eq!(backend.value(), SettingValue::Enum("auto"));
        assert_eq!(backend.source(), SettingSource::Default);

        let idle_timeout = store.effective_by_name("screen.idle_timeout").unwrap();
        assert_eq!(idle_timeout.value(), SettingValue::Integer(300));
        assert_eq!(idle_timeout.source(), SettingSource::Default);

        let restore_policy = store.effective_by_name("screen.restore_policy").unwrap();
        assert_eq!(restore_policy.value(), SettingValue::Enum("conservative"));
        assert_eq!(restore_policy.source(), SettingSource::Default);
    }

    #[test]
    fn settings_store_canonicalizes_valid_alias_values_from_config_env() {
        let store =
            ConfigEnvReader::parse("/tmp/config.env", "screen_restore_policy=marker_only\n")
                .into_store();

        let restore_policy = store.effective_by_name("screen.restore_policy").unwrap();

        assert_eq!(restore_policy.value(), SettingValue::Enum("conservative"));
        assert_eq!(restore_policy.source(), SettingSource::ConfigEnv);
    }

    #[test]
    fn settings_store_loads_existing_config_file() {
        let path = unique_test_path("existing");
        fs::write(&path, "screen_idle_timeout=123\n").unwrap();

        let store = SettingsStore::load(&path).unwrap();

        assert_eq!(store.path(), path.as_path());
        assert_eq!(store.raw_storage_value("screen_idle_timeout"), Some("123"));
        assert_eq!(
            store
                .effective_by_name("screen.idle_timeout")
                .unwrap()
                .value(),
            SettingValue::Integer(123)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn settings_store_loads_missing_config_file_as_empty_defaults() {
        let path = unique_test_path("missing");
        let _ = fs::remove_file(&path);

        let store = SettingsStore::load(&path).unwrap();

        assert_eq!(store.path(), path.as_path());
        assert_eq!(
            store.effective_by_name("screen.backend").unwrap().value(),
            SettingValue::Enum("auto")
        );
        assert_eq!(
            store.effective_by_name("screen.backend").unwrap().source(),
            SettingSource::Default
        );
    }

    #[test]
    fn all_effective_returns_registry_order() {
        let store = ConfigEnvReader::parse(
            "/tmp/config.env",
            "\
            screen_backend=gnome
            system_sleep_wake_policy=disabled
            ",
        )
        .into_store();

        let settings = store.all_effective();
        let keys: Vec<&str> = settings.iter().map(|setting| setting.key_name()).collect();
        let values: Vec<String> = settings
            .iter()
            .map(|setting| setting.value().to_string())
            .collect();
        let sources: Vec<SettingSource> = settings.iter().map(|setting| setting.source()).collect();

        assert_eq!(
            keys,
            vec![
                "screen.backend",
                "screen.idle_timeout",
                "screen.restore_policy",
                "system.sleep_wake_policy",
            ]
        );
        assert_eq!(values, vec!["gnome", "300", "conservative", "disabled"]);
        assert_eq!(
            sources,
            vec![
                SettingSource::ConfigEnv,
                SettingSource::Default,
                SettingSource::Default,
                SettingSource::ConfigEnv,
            ]
        );
    }

    #[test]
    fn key_parser_accepts_supported_dotted_names() {
        for key in [
            "screen.backend",
            "screen.idle_timeout",
            "system.sleep_wake_policy",
        ] {
            assert_eq!(SettingKey::parse(key).unwrap().as_str(), key);
        }
    }

    #[test]
    fn key_parser_rejects_invalid_names() {
        for key in [
            "",
            "screen",
            ".screen.backend",
            "screen.",
            "screen..backend",
            "Screen.backend",
            "screen-backend",
            "screen backend",
        ] {
            assert!(
                matches!(
                    SettingKey::parse(key),
                    Err(SettingsError::InvalidKey { .. })
                ),
                "expected invalid key for `{key}`"
            );
        }
    }

    #[test]
    fn lookup_returns_definitions_by_key() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();

        assert_eq!(definition.key_name(), "screen.idle_timeout");
        assert_eq!(definition.storage_key(), "screen_idle_timeout");
        assert_eq!(definition.default_value(), SettingValue::Integer(300));
        assert_eq!(definition.mutability(), SettingMutability::ReadWrite);
    }

    #[test]
    fn lookup_rejects_unknown_keys() {
        assert!(matches!(
            SETTINGS_REGISTRY.get_by_name("screen.unknown"),
            Err(SettingsError::UnknownKey(key)) if key == "screen.unknown"
        ));
    }

    #[test]
    fn screen_backend_values_are_validated() {
        let definition = SETTINGS_REGISTRY.get_by_name("screen.backend").unwrap();

        for value in ["auto", "gnome", "swayidle"] {
            assert_eq!(definition.parse_value(value), Ok(SettingValue::Enum(value)));
        }

        assert!(matches!(
            definition.parse_value("kde"),
            Err(SettingsError::InvalidValue { .. })
        ));
    }

    #[test]
    fn screen_restore_policy_accepts_legacy_alias() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.restore_policy")
            .unwrap();

        assert_eq!(
            definition.parse_value("conservative"),
            Ok(SettingValue::Enum("conservative"))
        );
        assert_eq!(
            definition.parse_value("marker_only"),
            Ok(SettingValue::Enum("conservative"))
        );
        assert_eq!(
            definition.parse_value("aggressive"),
            Ok(SettingValue::Enum("aggressive"))
        );
    }

    #[test]
    fn integer_values_are_range_checked() {
        let definition = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();

        assert_eq!(definition.parse_value("1"), Ok(SettingValue::Integer(1)));
        assert_eq!(
            definition.parse_value("86400"),
            Ok(SettingValue::Integer(86_400))
        );

        for value in ["0", "86401", "-1", "abc"] {
            assert!(
                matches!(
                    definition.parse_value(value),
                    Err(SettingsError::InvalidValue { .. })
                ),
                "expected invalid idle timeout for `{value}`"
            );
        }
    }

    #[test]
    fn mutability_controls_supported_operations() {
        let screen_definition = SETTINGS_REGISTRY.get_by_name("screen.backend").unwrap();
        assert_eq!(
            screen_definition.supported_operations(),
            &[
                SettingOperation::Get,
                SettingOperation::Describe,
                SettingOperation::Set,
                SettingOperation::Unset,
            ]
        );
        screen_definition
            .ensure_operation_supported(SettingOperation::Set)
            .unwrap();

        let sleep_definition = SETTINGS_REGISTRY
            .get_by_name("system.sleep_wake_policy")
            .unwrap();
        assert_eq!(sleep_definition.mutability(), SettingMutability::ReadOnly);
        assert_eq!(
            sleep_definition.supported_operations(),
            &[SettingOperation::Get, SettingOperation::Describe]
        );
        assert_eq!(
            sleep_definition.ensure_operation_supported(SettingOperation::Set),
            Err(SettingsError::UnsupportedOperation {
                key: "system.sleep_wake_policy".to_string(),
                operation: SettingOperation::Set,
            })
        );
    }

    #[test]
    fn definitions_expose_type_and_apply_metadata() {
        let idle_timeout = SETTINGS_REGISTRY
            .get_by_name("screen.idle_timeout")
            .unwrap();
        assert!(matches!(idle_timeout.value_type(), SettingType::Integer(_)));
        assert_eq!(
            idle_timeout.apply_strategy(),
            ApplyStrategy::RestartUserScreenService
        );

        let sleep_policy = SETTINGS_REGISTRY
            .get_by_name("system.sleep_wake_policy")
            .unwrap();
        assert!(matches!(sleep_policy.value_type(), SettingType::Enum(_)));
        assert_eq!(
            sleep_policy.apply_strategy(),
            ApplyStrategy::PendingLifecycleService
        );
    }

    fn unique_test_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!(
            "lg-buddy-settings-{name}-{}-{nanos}.env",
            std::process::id()
        ))
    }

    #[derive(Debug, Clone)]
    struct FakeServiceController {
        state: UserServiceState,
        restarts: Rc<Cell<usize>>,
        restart_error: Option<&'static str>,
        skip_actions: bool,
    }

    impl FakeServiceController {
        fn active_or_enabled() -> Self {
            Self {
                state: UserServiceState::ActiveOrEnabled,
                restarts: Rc::new(Cell::new(0)),
                restart_error: None,
                skip_actions: false,
            }
        }

        fn inactive_disabled() -> Self {
            Self {
                state: UserServiceState::InactiveDisabled,
                restarts: Rc::new(Cell::new(0)),
                restart_error: None,
                skip_actions: false,
            }
        }

        fn missing() -> Self {
            Self {
                state: UserServiceState::Missing,
                restarts: Rc::new(Cell::new(0)),
                restart_error: None,
                skip_actions: false,
            }
        }

        fn failing_restart() -> Self {
            Self {
                state: UserServiceState::ActiveOrEnabled,
                restarts: Rc::new(Cell::new(0)),
                restart_error: Some("restart failed"),
                skip_actions: false,
            }
        }
    }

    impl ServiceController for FakeServiceController {
        fn systemd_actions_disabled(&self) -> bool {
            self.skip_actions
        }

        fn user_service_state(&self, _service: &str) -> Result<UserServiceState, SettingsError> {
            Ok(self.state)
        }

        fn restart_user_service(&self, _service: &str) -> Result<(), SettingsError> {
            self.restarts.set(self.restarts.get() + 1);

            if let Some(message) = self.restart_error {
                Err(SettingsError::Apply {
                    message: message.to_string(),
                })
            } else {
                Ok(())
            }
        }
    }
}
