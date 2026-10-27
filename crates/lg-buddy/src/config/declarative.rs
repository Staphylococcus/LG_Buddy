//! Read-only generated declarations, separate from mutable user preferences.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::settings::{SettingValue, SettingsError, SETTINGS_REGISTRY};

const MAX_BYTES: u64 = 65_536;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeclarativeSource {
    #[default]
    Generic,
    Nix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declarations {
    pub path: PathBuf,
    pub source: DeclarativeSource,
    values: BTreeMap<&'static str, SettingValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    #[serde(default)]
    source: DeclarativeSource,
    #[serde(deserialize_with = "unique_settings")]
    settings: BTreeMap<String, Value>,
}

fn unique_settings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Value>, D::Error> {
    struct Unique;
    impl<'de> Visitor<'de> for Unique {
        type Value = BTreeMap<String, Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object with unique setting keys")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate declarative setting"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Unique)
}

fn invalid(path: &Path, reason: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "invalid declarative configuration at {}: {reason}",
            path.display()
        ),
    )
}

/// Resolve from the selected mutable config, never from a cached store target.
pub fn binding_path(config: &Path) -> io::Result<PathBuf> {
    let config = match fs::canonicalize(config) {
        Ok(path) => path,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(config).is_ok() {
                return Err(invalid(config, "selected config has a dangling link"));
            }
            std::path::absolute(config)?
        }
        Err(e) => return Err(e),
    };
    let mut name = config
        .file_name()
        .ok_or_else(|| invalid(&config, "missing filename"))?
        .to_os_string();
    name.push(".declarative.json");
    Ok(config.with_file_name(name))
}

impl Declarations {
    pub fn load(config: &Path) -> io::Result<Option<Self>> {
        let path = binding_path(config)?;
        let binding = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(metadata) => metadata,
        };
        let target = fs::canonicalize(&path)
            .map_err(|e| invalid(&path, format!("declaration target is unavailable: {e}")))?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&target)?;
        let metadata = file.metadata()?;
        let owner = fs::metadata(config)
            .or_else(|_| fs::metadata(config.parent().unwrap_or(Path::new("."))))
            .map(|m| m.uid())
            .unwrap_or_else(|_| unsafe { libc::geteuid() });
        let parent = fs::metadata(path.parent().unwrap_or(Path::new(".")))?;
        if ![0, owner].contains(&binding.uid())
            || ![0, owner].contains(&parent.uid())
            || (parent.mode() & 0o022 != 0 && !(parent.uid() == 0 && parent.mode() & 0o1000 != 0))
        {
            return Err(invalid(&path, "declaration binding and directory must belong to root or the config owner without unsafe redirection permissions"));
        }
        if !metadata.is_file()
            || ![0, owner].contains(&metadata.uid())
            || metadata.mode() & 0o022 != 0
            || metadata.len() > MAX_BYTES
        {
            return Err(invalid(&path, "expected bounded regular data owned by root or the config owner, without group/other writes"));
        }
        let mut bytes = Vec::new();
        file.by_ref().take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        let current = fs::metadata(&path)?;
        if bytes.len() as u64 > MAX_BYTES
            || fs::canonicalize(&path)? != target
            || (
                current.dev(),
                current.ino(),
                current.len(),
                current.mtime(),
                current.mtime_nsec(),
            ) != (
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            )
        {
            return Err(invalid(
                &path,
                "declaration changed while it was read; retry",
            ));
        }
        Self::parse(path, &bytes).map(Some)
    }

    fn parse(path: PathBuf, bytes: &[u8]) -> io::Result<Self> {
        let document: Document = serde_json::from_slice(bytes).map_err(|e| {
            invalid(
                &path,
                format!(
                    "invalid schema or JSON at line {}, column {}",
                    e.line(),
                    e.column()
                ),
            )
        })?;
        if document.version != 1 {
            return Err(invalid(&path, "unsupported declaration version"));
        }
        let mut values = BTreeMap::new();
        for (key, value) in document.settings {
            let definition = SETTINGS_REGISTRY
                .get_by_name(&key)
                .map_err(|_| invalid(&path, "unknown setting key"))?;
            if !key.starts_with("screen.")
                && !key.starts_with("system.")
                && !key.starts_with("updates.")
            {
                return Err(invalid(&path, "only screen, system and update behavior settings may be declared; TV profiles and credentials remain user-owned"));
            }
            let raw = match value {
                Value::String(value) => value,
                Value::Number(value) if value.is_i64() || value.is_u64() => value.to_string(),
                Value::Bool(value) => if value { "enabled" } else { "disabled" }.into(),
                _ => return Err(invalid(&path, "expected a scalar setting value")),
            };
            let value = definition.parse_value(&raw).map_err(|_| {
                invalid(
                    &path,
                    format!("{} requires {}", key, definition.value_type().expected()),
                )
            })?;
            values.insert(definition.key_name(), value);
        }
        Ok(Self {
            path,
            source: document.source,
            values,
        })
    }

    pub fn value(&self, key: &str) -> Option<SettingValue> {
        self.values.get(key).copied()
    }

    /// Render only an in-memory effective snapshot. Never persist this output.
    pub fn effective_contents(&self, saved: &str) -> String {
        let mut effective = saved.to_owned();
        effective.push('\n');
        for (key, value) in &self.values {
            let definition = SETTINGS_REGISTRY
                .get_by_name(key)
                .expect("validated declaration");
            effective.push_str(&format!("{}={value}\n", definition.storage_key()));
        }
        effective
    }

    fn reject(&self, key: &str) -> SettingsError {
        SettingsError::ManagedSetting {
            key: key.into(),
            path: self.path.clone(),
            source: self.source,
        }
    }
}

pub(crate) fn ensure_mutable(config: &Path, key: &str) -> Result<(), SettingsError> {
    if let Some(declarations) = Declarations::load(config).map_err(|e| settings_error(config, e))? {
        if declarations.value(key).is_some() {
            return Err(declarations.reject(key));
        }
    }
    Ok(())
}

pub(crate) fn settings_error(config: &Path, error: io::Error) -> SettingsError {
    SettingsError::ReadConfig {
        path: binding_path(config).unwrap_or_else(|_| config.into()),
        kind: error.kind(),
        message: error.to_string(),
    }
}

/// Final publication guard for editors, pairing and migration writers alike.
pub(crate) fn validate_changes(config: &Path, before: &[u8], after: &[u8]) -> io::Result<()> {
    let Some(declarations) = Declarations::load(config)? else {
        return Ok(());
    };
    let parse = |bytes| {
        std::str::from_utf8(bytes)
            .map(super::parse_config_entries)
            .map_err(|_| invalid(&declarations.path, "config must be UTF-8"))
    };
    let before = parse(before)?;
    let after = parse(after)?;
    for key in declarations.values.keys() {
        let definition = SETTINGS_REGISTRY
            .get_by_name(key)
            .expect("validated declaration");
        for storage_key in std::iter::once(definition.storage_key())
            .chain(definition.fallback_storage_keys().iter().copied())
        {
            if before.get(storage_key) != after.get(storage_key) {
                return Err(io::Error::other(declarations.reject(key)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
