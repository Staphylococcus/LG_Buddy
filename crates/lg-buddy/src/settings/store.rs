use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use crate::config::{
    parse_config_entries, resolve_config_path, resolve_config_path_from_env, ConfigPathSources,
};

use super::{
    SettingDefinition, SettingKey, SettingOperation, SettingValue, SettingsError, SETTINGS_REGISTRY,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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

        atomic_write_config(&self.path, self.render().as_bytes()).map_err(|err| {
            SettingsError::WriteConfig {
                path: self.path.clone(),
                message: err.to_string(),
            }
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

fn atomic_write_config(path: &Path, contents: &[u8]) -> io::Result<()> {
    // Continue updating an existing symlink's target instead of replacing the
    // link itself. A dangling link cannot be safely resolved for publication.
    let path = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path)?,
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    // Check the original file's write access without truncating it. Rename
    // permission alone must not let a save replace a read-only configuration.
    let metadata = match OpenOptions::new().write(true).open(&path) {
        Ok(file) => Some(file.metadata()?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "config path has no file name")
    })?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".settings.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = path.with_file_name(temp_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(if metadata.is_some() { 0o600 } else { 0o666 });
    let mut file = options.open(&temp_path)?;
    let result = (|| {
        file.write_all(contents)?;
        if let Some(metadata) = metadata {
            #[cfg(unix)]
            {
                let created = file.metadata()?;
                if (created.uid(), created.gid()) != (metadata.uid(), metadata.gid()) {
                    let result =
                        unsafe { libc::fchown(file.as_raw_fd(), metadata.uid(), metadata.gid()) };
                    if result != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            file.set_permissions(metadata.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        // Publication is the final fallible step: errors leave the old bytes
        // intact, and success means readers can see the complete new contents.
        fs::rename(&temp_path, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsMutationAction {
    Set,
    Unset,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingsMutation {
    definition: &'static SettingDefinition,
    old_value: Option<SettingValue>,
    old_source: SettingSource,
    new_value: Option<SettingValue>,
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
            new_value: Some(new_value),
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

    pub fn old_value(self) -> Option<SettingValue> {
        self.old_value
    }

    pub fn old_source(self) -> SettingSource {
        self.old_source
    }

    pub fn new_value(self) -> Result<SettingValue, SettingsError> {
        self.new_value
            .ok_or_else(|| SettingsError::MissingRequiredSetting {
                key: self.key_name().to_string(),
            })
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
    effective: EffectiveSetting,
}

impl SettingsChange {
    pub fn effective_setting(&self) -> &EffectiveSetting {
        &self.effective
    }

    pub(crate) fn for_apply(store: &SettingsStore, key: &str) -> Result<Self, SettingsError> {
        let effective = store.effective_by_name(key)?;
        let value = effective.required_value()?;
        let mutation = SettingsMutation::set(store, key, &value.to_string())?;
        Ok(Self {
            mutation,
            path: store.path().to_path_buf(),
            file_changed: false,
            effective,
        })
    }

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

pub(crate) fn persist_settings_mutation(
    path: &Path,
    mutation: SettingsMutation,
) -> Result<SettingsChange, SettingsError> {
    let mut editor = ConfigEnvEditor::load(path)?;
    let file_changed = match mutation.action() {
        SettingsMutationAction::Set => {
            let mut changed = editor.set(mutation.storage_key(), mutation.new_value()?);
            for fallback_key in mutation.definition().fallback_storage_keys() {
                changed |= editor.unset(fallback_key);
            }
            changed
        }
        SettingsMutationAction::Unset => {
            let mut changed = editor.unset(mutation.storage_key());
            for fallback_key in mutation.definition().fallback_storage_keys() {
                changed |= editor.unset(fallback_key);
            }
            changed
        }
    };

    if file_changed {
        editor.save()?;
    }

    Ok(SettingsChange {
        mutation,
        path: editor.path().to_path_buf(),
        file_changed,
        effective: EffectiveSetting {
            definition: mutation.definition(),
            value: mutation.new_value,
            source: match mutation.action() {
                SettingsMutationAction::Set => SettingSource::ConfigEnv,
                SettingsMutationAction::Unset if mutation.new_value.is_some() => {
                    SettingSource::Default
                }
                SettingsMutationAction::Unset => SettingSource::Missing,
            },
            invalid_value: None,
        },
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
        let raw_value = self.reader.raw_setting_value(definition);

        if let Some((raw_value, source)) = raw_value {
            return match definition.parse_value(raw_value) {
                Ok(value) => EffectiveSetting {
                    definition,
                    value: Some(value),
                    source,
                    invalid_value: None,
                },
                Err(_) => EffectiveSetting {
                    definition,
                    value: None,
                    source: source.invalid(),
                    invalid_value: Some(raw_value.to_string()),
                },
            };
        }

        match definition.default_value() {
            Some(value) => EffectiveSetting {
                definition,
                value: Some(value),
                source: SettingSource::Default,
                invalid_value: None,
            },
            None => EffectiveSetting {
                definition,
                value: None,
                source: SettingSource::Missing,
                invalid_value: None,
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

    pub fn raw_setting_value(
        &self,
        definition: &'static SettingDefinition,
    ) -> Option<(&str, SettingSource)> {
        self.raw_value(definition.storage_key())
            .map(|value| (value, SettingSource::ConfigEnv))
            .or_else(|| {
                definition
                    .fallback_storage_keys()
                    .iter()
                    .find_map(|storage_key| {
                        self.raw_value(storage_key)
                            .map(|value| (value, SettingSource::LegacyConfigEnv))
                    })
            })
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

#[derive(Debug, Clone)]
pub struct EffectiveSetting {
    definition: &'static SettingDefinition,
    value: Option<SettingValue>,
    source: SettingSource,
    invalid_value: Option<String>,
}

impl EffectiveSetting {
    pub fn definition(&self) -> &'static SettingDefinition {
        self.definition
    }

    pub fn key(&self) -> SettingKey<'static> {
        self.definition.key()
    }

    pub fn key_name(&self) -> &'static str {
        self.definition.key_name()
    }

    pub fn storage_key(&self) -> &'static str {
        self.definition.storage_key()
    }

    pub fn value(&self) -> Option<SettingValue> {
        self.value
    }

    pub fn required_value(&self) -> Result<SettingValue, SettingsError> {
        if let Some(value) = self.invalid_value() {
            return Err(SettingsError::InvalidValue {
                key: self.key_name().to_string(),
                value: value.to_string(),
                expected: self.definition.value_type().expected(),
            });
        }

        self.value
            .ok_or_else(|| SettingsError::MissingRequiredSetting {
                key: self.key_name().to_string(),
            })
    }

    pub fn source(&self) -> SettingSource {
        self.source
    }

    pub fn invalid_value(&self) -> Option<&str> {
        self.invalid_value.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    Default,
    ConfigEnv,
    LegacyConfigEnv,
    InvalidConfigEnv,
    InvalidLegacyConfigEnv,
    Missing,
}

impl SettingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::ConfigEnv => "config.env",
            Self::LegacyConfigEnv => "legacy config.env",
            Self::InvalidConfigEnv => "invalid config.env",
            Self::InvalidLegacyConfigEnv => "invalid legacy config.env",
            Self::Missing => "missing",
        }
    }

    fn invalid(self) -> Self {
        match self {
            Self::ConfigEnv => Self::InvalidConfigEnv,
            Self::LegacyConfigEnv => Self::InvalidLegacyConfigEnv,
            other => other,
        }
    }
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
