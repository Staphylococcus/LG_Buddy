//! First-TV pairing persistence.
//!
//! Pairing has two durable outputs: the native webOS access token and the
//! primary-TV settings in `config.env`.  This module keeps the workflow
//! reversible until the config file is published.  There is still an
//! unavoidable crash window between those two filesystem operations; callers
//! should therefore treat a successfully returned commit as the completion
//! boundary and repair an interrupted pairing on the next attempt.

use crate::auth::{resolve_config_owner, AuthContextError, SystemUser};
use crate::config::{parse_config_entries, HdmiInput, MacAddress};
use crate::platform_access_token::{
    PlatformAccessToken, PlatformAccessTokenStore, PlatformAccessTokenStoreError,
};
use crate::settings::{ConfigEnvEditor, ConfigEnvReader, SettingValue};
use crate::settings_view::BehaviorSetting;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const LOCK_FILE_SUFFIX: &str = ".pairing.lock";
const TV_KEYS: &[&str] = &[
    "tvs_primary_ip",
    "tv_ip",
    "tvs_primary_mac",
    "tv_mac",
    "tvs_primary_input",
    "input",
    "tvs_primary_platform",
];

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn unpair_primary(
    config_path: &Path,
    expected: &crate::tvs::TvProfile,
) -> Result<(), PairingStoreError> {
    PairingStore::unpair_primary(config_path, expected)
}

/// A prepared, exclusive first-TV pairing transaction.
///
/// Preparing one takes a narrow lock which remains held until this value is
/// committed or dropped. Dropping it releases the kernel lock; the inert
/// marker remains so every process locks the same inode, including after a crash.
pub(crate) struct PairingStore {
    config_path: PathBuf,
    snapshot: Option<Vec<u8>>,
    owner: SystemUser,
    token_store: PlatformAccessTokenStore,
    token_before: Option<TokenBefore>,
    profile_dir_existed: bool,
    tvs_dir_existed: bool,
    _lock: PairingLock,
}

impl PairingStore {
    /// Prepare a first-primary-TV transaction before starting network pairing.
    pub(crate) fn prepare(config_path: &Path) -> Result<Self, PairingStoreError> {
        prepare_with_euid(config_path, current_euid())
    }

    /// Remove the configured primary-TV profile and its native credential.
    ///
    /// The same lock and snapshots used by pairing are held for the complete
    /// operation.  The profile is checked again immediately before removing
    /// the credential, so a stale caller cannot erase a newly selected TV.
    pub(crate) fn unpair_primary(
        config_path: &Path,
        expected: &crate::tvs::TvProfile,
    ) -> Result<(), PairingStoreError> {
        let store = prepare_unpair_with_euid(config_path, current_euid())?;
        store.unpair(expected)
    }

    fn unpair(self, expected: &crate::tvs::TvProfile) -> Result<(), PairingStoreError> {
        self.check_unpair_preconditions(expected)?;
        self.remove_token()?;

        let result = (|| {
            self.check_unpair_config_preconditions(expected)?;
            match fs::symlink_metadata(self.token_store.token_path()) {
                Ok(_) => {
                    return Err(PairingStoreError::TokenChanged {
                        path: self.token_store.token_path().to_path_buf(),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(PairingStoreError::TokenRemove {
                        path: self.token_store.token_path().to_path_buf(),
                        source,
                    });
                }
            }
            let original = self.snapshot.as_deref().unwrap_or_default();
            let contents = remove_primary_config_keys(original);
            atomic_write_config(
                &self.config_path,
                &contents,
                &self.owner,
                self.snapshot.is_some(),
            )
        })();

        match result {
            Ok(()) => Ok(()),
            Err(error) => match self.restore_unpaired_token() {
                Ok(()) => Err(error),
                Err(rollback) => Err(PairingStoreError::Rollback {
                    operation: Box::new(error),
                    rollback: Box::new(rollback),
                }),
            },
        }
    }

    /// Persist the verified native webOS pairing result.
    pub(crate) fn commit(
        self,
        address: Ipv4Addr,
        mac: MacAddress,
        input: HdmiInput,
        token: &PlatformAccessToken,
    ) -> Result<Vec<BehaviorSetting>, PairingStoreError> {
        if current_euid() == 0 {
            return Err(PairingStoreError::RunningAsRoot);
        }

        let current = read_config_snapshot(&self.config_path)?;
        if current != self.snapshot {
            return Err(PairingStoreError::ConfigChanged {
                path: self.config_path.clone(),
            });
        }

        let token_path = self.token_store.token_path().to_path_buf();
        if let Err(source) = self.token_store.persist(token) {
            let cleanup = self.cleanup_created_dirs();
            let error = PairingStoreError::TokenWrite {
                path: token_path,
                source,
            };
            return match cleanup {
                Ok(()) => Err(error),
                Err(rollback) => Err(PairingStoreError::Rollback {
                    operation: Box::new(error),
                    rollback: Box::new(rollback),
                }),
            };
        }

        let result = (|| {
            let current = read_config_snapshot(&self.config_path)?;
            if current != self.snapshot {
                return Err(PairingStoreError::ConfigChanged {
                    path: self.config_path.clone(),
                });
            }

            let original = self.snapshot.as_deref().unwrap_or_default();
            let original =
                std::str::from_utf8(original).map_err(|source| PairingStoreError::ConfigRead {
                    path: self.config_path.clone(),
                    source: io::Error::new(io::ErrorKind::InvalidData, source),
                })?;
            let (contents, defaults) = render_first_primary_config(original, address, mac, input);
            atomic_write_config(
                &self.config_path,
                &contents,
                &self.owner,
                self.snapshot.is_some(),
            )?;
            Ok(defaults)
        })();

        match result {
            Ok(defaults) => Ok(defaults),
            Err(error) => match self.restore_token() {
                Ok(()) => Err(error),
                Err(rollback) => Err(PairingStoreError::Rollback {
                    operation: Box::new(error),
                    rollback: Box::new(rollback),
                }),
            },
        }
    }

    fn restore_token(&self) -> Result<(), PairingStoreError> {
        let token_path = self.token_store.token_path();
        match &self.token_before {
            Some(before) => {
                atomic_write_bytes(token_path, &before.contents, before.mode).map_err(|source| {
                    PairingStoreError::TokenRollback {
                        path: token_path.to_path_buf(),
                        source,
                    }
                })
            }
            None => {
                match fs::symlink_metadata(token_path) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        fs::remove_file(token_path).map_err(|source| {
                            PairingStoreError::TokenRollback {
                                path: token_path.to_path_buf(),
                                source,
                            }
                        })?;
                    }
                    Ok(_) => {
                        return Err(PairingStoreError::TokenRollback {
                            path: token_path.to_path_buf(),
                            source: io::Error::new(
                                io::ErrorKind::InvalidData,
                                "credential path changed into a non-file",
                            ),
                        });
                    }
                    Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(PairingStoreError::TokenRollback {
                            path: token_path.to_path_buf(),
                            source,
                        });
                    }
                }

                self.cleanup_created_dirs()
            }
        }
    }

    fn restore_unpaired_token(&self) -> Result<(), PairingStoreError> {
        let token_path = self.token_store.token_path();
        match &self.token_before {
            Some(before) => {
                ensure_token_path_safe(token_path, &self.owner)?;
                match read_existing_token(token_path)? {
                    Some(current) if !same_token_snapshot(Some(before), Some(&current)) => {
                        Err(PairingStoreError::TokenChanged {
                            path: token_path.to_path_buf(),
                        })
                    }
                    Some(_) => Ok(()),
                    None => atomic_write_bytes(token_path, &before.contents, before.mode).map_err(
                        |source| PairingStoreError::TokenRollback {
                            path: token_path.to_path_buf(),
                            source,
                        },
                    ),
                }
            }
            None => match fs::symlink_metadata(token_path) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    // A credential which appeared while the config was being
                    // published belongs to whoever created it.  Leave it in
                    // place instead of deleting a stale concurrent change.
                    Ok(())
                }
                Ok(_) => Err(PairingStoreError::TokenRollback {
                    path: token_path.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "credential path changed into a non-file",
                    ),
                }),
                Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(PairingStoreError::TokenRollback {
                    path: token_path.to_path_buf(),
                    source,
                }),
            },
        }
    }

    fn check_unpair_preconditions(
        &self,
        expected: &crate::tvs::TvProfile,
    ) -> Result<(), PairingStoreError> {
        self.check_unpair_config_preconditions(expected)?;
        ensure_token_path_safe(self.token_store.token_path(), &self.owner)?;
        let current_token = read_existing_token(self.token_store.token_path())?;
        if !same_token_snapshot(self.token_before.as_ref(), current_token.as_ref()) {
            return Err(PairingStoreError::TokenChanged {
                path: self.token_store.token_path().to_path_buf(),
            });
        }
        Ok(())
    }

    fn check_unpair_config_preconditions(
        &self,
        expected: &crate::tvs::TvProfile,
    ) -> Result<(), PairingStoreError> {
        let current = read_config_snapshot(&self.config_path)?;
        if current != self.snapshot {
            return Err(PairingStoreError::ConfigChanged {
                path: self.config_path.clone(),
            });
        }
        ensure_config_owner(&self.config_path, &self.owner)?;
        validate_primary_identity(&self.config_path, current.as_deref(), expected)?;
        Ok(())
    }

    fn remove_token(&self) -> Result<(), PairingStoreError> {
        let path = self.token_store.token_path();
        ensure_token_path_safe(path, &self.owner)?;
        let current = read_existing_token(path)?;
        if !same_token_snapshot(self.token_before.as_ref(), current.as_ref()) {
            return Err(PairingStoreError::TokenChanged {
                path: path.to_path_buf(),
            });
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                fs::remove_file(path).map_err(|source| PairingStoreError::TokenRemove {
                    path: path.to_path_buf(),
                    source,
                })
            }
            Ok(_) => Err(PairingStoreError::TokenRead {
                path: path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "credential path is not a regular file",
                ),
            }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PairingStoreError::TokenRemove {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn cleanup_created_dirs(&self) -> Result<(), PairingStoreError> {
        let token_path = self.token_store.token_path();
        if !self.profile_dir_existed {
            let profile = token_path
                .parent()
                .expect("derived token path always has a profile directory");
            remove_empty_dir(profile)?;
        }
        if !self.tvs_dir_existed {
            let tvs = token_path
                .parent()
                .and_then(Path::parent)
                .expect("derived token path always has a TVs directory");
            remove_empty_dir(tvs)?;
        }
        Ok(())
    }
}

fn prepare_with_euid(config_path: &Path, euid: u32) -> Result<PairingStore, PairingStoreError> {
    prepare_transaction(config_path, euid, true)
}

fn prepare_unpair_with_euid(
    config_path: &Path,
    euid: u32,
) -> Result<PairingStore, PairingStoreError> {
    prepare_transaction(config_path, euid, false)
}

fn prepare_transaction(
    config_path: &Path,
    euid: u32,
    require_unconfigured: bool,
) -> Result<PairingStore, PairingStoreError> {
    if euid == 0 {
        return Err(PairingStoreError::RunningAsRoot);
    }

    let parent = config_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| PairingStoreError::ConfigPathHasNoParent {
            path: config_path.to_path_buf(),
        })?;

    let lock_path = config_path.with_file_name(format!(
        ".{}{}",
        config_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.env"),
        LOCK_FILE_SUFFIX
    ));
    let lock = PairingLock::acquire(lock_path, parent.to_path_buf())?;

    let snapshot = read_config_snapshot(config_path)?;
    let config_contents =
        std::str::from_utf8(snapshot.as_deref().unwrap_or_default()).map_err(|source| {
            PairingStoreError::ConfigRead {
                path: config_path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::InvalidData, source),
            }
        })?;
    let entries = parse_config_entries(config_contents);
    if require_unconfigured {
        if let Some(key) = TV_KEYS.iter().find(|key| entries.contains_key(**key)) {
            return Err(PairingStoreError::PrimaryAlreadyConfigured {
                key: (*key).to_string(),
            });
        }
    }

    let owner = resolve_owner(config_path, snapshot.is_some(), euid)?;
    if owner.uid() == 0 {
        return Err(PairingStoreError::ConfigOwnedByRoot {
            path: config_path.to_path_buf(),
        });
    }
    if owner.uid() != euid {
        return Err(PairingStoreError::ConfigOwnerMismatch {
            path: config_path.to_path_buf(),
            owner_uid: owner.uid(),
            euid,
        });
    }
    let token_store = PlatformAccessTokenStore::for_primary_profile(config_path, owner.clone())
        .map_err(PairingStoreError::TokenPath)?;
    let token_path = token_store.token_path().to_path_buf();
    let token_before = read_existing_token(&token_path)?;
    let profile_dir = token_path
        .parent()
        .expect("derived token path always has a profile directory");
    let tvs_dir = profile_dir
        .parent()
        .expect("derived profile path always has a TVs directory");

    if !require_unconfigured {
        ensure_token_path_safe(&token_path, &owner)?;
    }

    Ok(PairingStore {
        config_path: config_path.to_path_buf(),
        snapshot,
        owner,
        token_store,
        token_before,
        profile_dir_existed: fs::symlink_metadata(profile_dir).is_ok(),
        tvs_dir_existed: fs::symlink_metadata(tvs_dir).is_ok(),
        _lock: lock,
    })
}

fn resolve_owner(
    config_path: &Path,
    config_exists: bool,
    euid: u32,
) -> Result<SystemUser, PairingStoreError> {
    if config_exists {
        return resolve_config_owner(config_path).map_err(PairingStoreError::Owner);
    }

    #[cfg(unix)]
    {
        let gid = unsafe { libc::getegid() };
        let username = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        Ok(SystemUser::new(username, euid, gid, PathBuf::new()))
    }
    #[cfg(not(unix))]
    {
        let _ = (config_path, euid);
        Err(PairingStoreError::Lock {
            path: config_path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                format!("native pairing is unsupported on this platform (uid {euid})"),
            ),
        })
    }
}

fn current_euid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        u32::MAX
    }
}

fn read_config_snapshot(path: &Path) -> Result<Option<Vec<u8>>, PairingStoreError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(PairingStoreError::ConfigSymlink {
                path: path.to_path_buf(),
            });
        }
    }
    match fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PairingStoreError::ConfigRead {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_existing_token(path: &Path) -> Result<Option<TokenBefore>, PairingStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PairingStoreError::TokenRead {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if !metadata.file_type().is_file() {
        return Err(PairingStoreError::TokenRead {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "credential path is not a regular file",
            ),
        });
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(path)
        .map_err(|source| PairingStoreError::TokenRead {
            path: path.to_path_buf(),
            source,
        })?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|source| PairingStoreError::TokenRead {
            path: path.to_path_buf(),
            source,
        })?;

    #[cfg(unix)]
    let mode = metadata.mode();
    #[cfg(not(unix))]
    let mode = 0;
    Ok(Some(TokenBefore { contents, mode }))
}

fn same_token_snapshot(before: Option<&TokenBefore>, current: Option<&TokenBefore>) -> bool {
    match (before, current) {
        (None, None) => true,
        (Some(before), Some(current)) => {
            before.contents == current.contents && before.mode == current.mode
        }
        _ => false,
    }
}

fn ensure_config_owner(path: &Path, owner: &SystemUser) -> Result<(), PairingStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PairingStoreError::ConfigRead {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(PairingStoreError::ConfigSymlink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.file_type().is_file() {
        return Err(PairingStoreError::ConfigRead {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                "config path is not a regular file",
            ),
        });
    }
    #[cfg(unix)]
    {
        if metadata.uid() == 0 {
            return Err(PairingStoreError::ConfigOwnedByRoot {
                path: path.to_path_buf(),
            });
        }
        if metadata.uid() != owner.uid() {
            return Err(PairingStoreError::ConfigOwnerMismatch {
                path: path.to_path_buf(),
                owner_uid: metadata.uid(),
                euid: current_euid(),
            });
        }
    }
    Ok(())
}

fn ensure_token_path_safe(token_path: &Path, owner: &SystemUser) -> Result<(), PairingStoreError> {
    let profile_dir = token_path
        .parent()
        .expect("derived token path always has a profile directory");
    let tvs_dir = profile_dir
        .parent()
        .expect("derived profile path always has a TVs directory");

    for directory in [tvs_dir, profile_dir] {
        let metadata = match fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(PairingStoreError::TokenRead {
                    path: directory.to_path_buf(),
                    source,
                })
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(PairingStoreError::TokenRead {
                path: directory.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "credential directory is a symlink",
                ),
            });
        }
        if !metadata.file_type().is_dir() {
            return Err(PairingStoreError::TokenRead {
                path: directory.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "credential path is not a directory",
                ),
            });
        }
        #[cfg(unix)]
        {
            if metadata.uid() == 0 {
                return Err(PairingStoreError::TokenRead {
                    path: directory.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "credential directory is owned by root",
                    ),
                });
            }
            if metadata.uid() != owner.uid() {
                return Err(PairingStoreError::TokenRead {
                    path: directory.to_path_buf(),
                    source: io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "credential directory has a different owner",
                    ),
                });
            }
        }
    }

    match fs::symlink_metadata(token_path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            #[cfg(unix)]
            {
                if metadata.uid() == 0 {
                    return Err(PairingStoreError::TokenRead {
                        path: token_path.to_path_buf(),
                        source: io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "credential file is owned by root",
                        ),
                    });
                }
                if metadata.uid() != owner.uid() {
                    return Err(PairingStoreError::TokenRead {
                        path: token_path.to_path_buf(),
                        source: io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "credential file has a different owner",
                        ),
                    });
                }
            }
        }
        Ok(_) => {
            return Err(PairingStoreError::TokenRead {
                path: token_path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "credential path is not a regular file",
                ),
            });
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(PairingStoreError::TokenRead {
                path: token_path.to_path_buf(),
                source,
            });
        }
    }

    Ok(())
}

fn validate_primary_identity(
    config_path: &Path,
    contents: Option<&[u8]>,
    expected: &crate::tvs::TvProfile,
) -> Result<(), PairingStoreError> {
    let contents = contents.ok_or_else(|| PairingStoreError::PrimaryIdentityMismatch {
        path: config_path.to_path_buf(),
        field: "profile",
    })?;
    let text = std::str::from_utf8(contents).map_err(|source| PairingStoreError::ConfigRead {
        path: config_path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidData, source),
    })?;
    let entries = parse_config_entries(text);

    let value = |primary: &str, fallback: &str| {
        entries
            .get(primary)
            .or_else(|| entries.get(fallback))
            .map(String::as_str)
    };
    let address = value("tvs_primary_ip", "tv_ip")
        .and_then(|value| value.parse::<Ipv4Addr>().ok())
        .ok_or_else(|| identity_mismatch(config_path, "ip"))?;
    let mac = value("tvs_primary_mac", "tv_mac")
        .and_then(|value| value.parse::<MacAddress>().ok())
        .ok_or_else(|| identity_mismatch(config_path, "mac"))?;
    let input = value("tvs_primary_input", "input")
        .and_then(|value| value.parse::<HdmiInput>().ok())
        .ok_or_else(|| identity_mismatch(config_path, "input"))?;
    let platform = entries
        .get("tvs_primary_platform")
        .map(String::as_str)
        .unwrap_or(crate::config::TvPlatform::DEFAULT.as_str())
        .parse::<crate::config::TvPlatform>()
        .map_err(|_| identity_mismatch(config_path, "platform"))?;

    for (field, matches) in [
        ("ip", address == expected.address()),
        ("mac", mac == expected.mac()),
        ("input", input == expected.input()),
        ("platform", platform == expected.platform()),
    ] {
        if !matches {
            return Err(identity_mismatch(config_path, field));
        }
    }
    Ok(())
}

fn identity_mismatch(path: &Path, field: &'static str) -> PairingStoreError {
    PairingStoreError::PrimaryIdentityMismatch {
        path: path.to_path_buf(),
        field,
    }
}

fn remove_primary_config_keys(original: &[u8]) -> Vec<u8> {
    let mut contents = Vec::with_capacity(original.len());
    let mut start = 0;
    for (index, byte) in original.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line = &original[start..=index];
        if !is_primary_config_line(line) {
            contents.extend_from_slice(line);
        }
        start = index + 1;
    }
    if start < original.len() {
        let line = &original[start..];
        if !is_primary_config_line(line) {
            contents.extend_from_slice(line);
        }
    }
    contents
}

fn is_primary_config_line(line: &[u8]) -> bool {
    let line = trim_ascii_whitespace(line.strip_suffix(b"\n").unwrap_or(line));
    let line = trim_ascii_whitespace(line.strip_suffix(b"\r").unwrap_or(line));
    if line.is_empty() || line[0] == b'#' {
        return false;
    }
    let Some(equal) = line.iter().position(|byte| *byte == b'=') else {
        return false;
    };
    let key = trim_ascii_whitespace(&line[..equal]);
    TV_KEYS.iter().any(|candidate| key == candidate.as_bytes())
}

fn trim_ascii_whitespace(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &value[start..end]
}

fn remove_empty_dir(path: &Path) -> Result<(), PairingStoreError> {
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(source) => Err(PairingStoreError::TokenRollback {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn render_first_primary_config(
    original: &str,
    address: Ipv4Addr,
    mac: MacAddress,
    input: HdmiInput,
) -> (Vec<u8>, Vec<BehaviorSetting>) {
    let store = ConfigEnvReader::parse("config.env", original).into_store();
    let mut editor = ConfigEnvEditor::parse("config.env", original);
    let mut defaults = Vec::new();
    for setting in [
        BehaviorSetting::ScreenIdleBlank,
        BehaviorSetting::SystemSleepWakePolicy,
    ] {
        let effective = store
            .effective_by_name(setting.key_name())
            .expect("known behavior");
        if effective.value() == Some(SettingValue::Enum("enabled")) {
            defaults.push(setting);
            editor.set(effective.storage_key(), SettingValue::Enum("disabled"));
        }
    }
    // Publish these policies off with the TV. Onboarding enables each only
    // after its service is available, so interruption cannot claim activation.
    let mut contents = editor.render().into_bytes();
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        contents.push(b'\n');
    }
    contents.extend_from_slice(format!("tvs_primary_ip={address}\n").as_bytes());
    contents.extend_from_slice(format!("tvs_primary_mac={mac}\n").as_bytes());
    contents.extend_from_slice(format!("tvs_primary_input={}\n", input.as_str()).as_bytes());
    contents.extend_from_slice(b"tvs_primary_platform=lg_webos\n");
    (contents, defaults)
}

fn atomic_write_config(
    path: &Path,
    contents: &[u8],
    owner: &SystemUser,
    existed: bool,
) -> Result<(), PairingStoreError> {
    let mode = if existed {
        #[cfg(unix)]
        {
            fs::metadata(path)
                .map_err(|source| PairingStoreError::ConfigWrite {
                    path: path.to_path_buf(),
                    source,
                })?
                .permissions()
                .mode()
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            0o600
        }
    } else {
        0o600
    };
    let temp = temporary_path(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode & 0o7777).custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(&temp)
        .map_err(|source| PairingStoreError::ConfigWrite {
            path: temp.clone(),
            source,
        })?;

    let result = (|| {
        file.write_all(contents)
            .map_err(|source| PairingStoreError::ConfigWrite {
                path: temp.clone(),
                source,
            })?;
        file.flush()
            .map_err(|source| PairingStoreError::ConfigWrite {
                path: temp.clone(),
                source,
            })?;
        #[cfg(unix)]
        {
            file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|source| PairingStoreError::ConfigWrite {
                    path: temp.clone(),
                    source,
                })?;
            set_owner(&file, owner).map_err(|source| PairingStoreError::ConfigWrite {
                path: temp.clone(),
                source,
            })?;
        }
        file.sync_all()
            .map_err(|source| PairingStoreError::ConfigWrite {
                path: temp.clone(),
                source,
            })?;
        drop(file);
        fs::rename(&temp, path).map_err(|source| PairingStoreError::ConfigWrite {
            path: path.to_path_buf(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn atomic_write_bytes(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let temp = temporary_path(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode & 0o7777).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(contents)?;
        file.flush()?;
        #[cfg(unix)]
        {
            file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!(".{name}.{}.{}.tmp", process::id(), counter))
}

#[cfg(unix)]
fn set_owner(file: &File, owner: &SystemUser) -> io::Result<()> {
    set_owner_ids(file, owner.uid(), owner.gid())
}

#[cfg(unix)]
fn set_owner_ids(file: &File, uid: u32, gid: u32) -> io::Result<()> {
    let result = unsafe { libc::fchown(file.as_raw_fd(), uid, gid) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

struct TokenBefore {
    contents: Vec<u8>,
    mode: u32,
}

#[derive(Debug)]
struct PairingLock {
    file: File,
}

impl PairingLock {
    fn acquire(path: PathBuf, parent: PathBuf) -> Result<Self, PairingStoreError> {
        let created_parents = ensure_lock_parent(&parent)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(source) => {
                remove_created_parents(&created_parents);
                return Err(PairingStoreError::Lock { path, source });
            }
        };
        #[cfg(unix)]
        {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let source = io::Error::last_os_error();
                remove_created_parents(&created_parents);
                if source.kind() == io::ErrorKind::WouldBlock {
                    return Err(PairingStoreError::PairingInProgress { path });
                }
                return Err(PairingStoreError::Lock { path, source });
            }
        }
        let _ = writeln!(file, "pid={}", process::id());
        Ok(Self { file })
    }
}

impl Drop for PairingLock {
    fn drop(&mut self) {
        // Closing only our descriptor can leave the lock held by a child
        // between fork and exec. Release the shared open-file-description lock.
        #[cfg(unix)]
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn ensure_lock_parent(parent: &Path) -> Result<Vec<PathBuf>, PairingStoreError> {
    if parent.is_dir() {
        return Ok(Vec::new());
    }
    if parent.exists() {
        return Err(PairingStoreError::Lock {
            path: parent.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::NotADirectory,
                "config parent is not a directory",
            ),
        });
    }

    let mut created = Vec::new();
    let mut current = parent;
    while !current.exists() {
        created.push(current.to_path_buf());
        current = current.parent().ok_or_else(|| PairingStoreError::Lock {
            path: parent.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::NotFound,
                "config parent has no existing ancestor",
            ),
        })?;
    }
    fs::create_dir_all(parent).map_err(|source| PairingStoreError::Lock {
        path: parent.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    for path in &created {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(created)
}

fn remove_created_parents(created: &[PathBuf]) {
    for path in created {
        let _ = fs::remove_dir(path);
    }
}

#[derive(Debug)]
pub(crate) enum PairingStoreError {
    RunningAsRoot,
    ConfigPathHasNoParent {
        path: PathBuf,
    },
    PairingInProgress {
        path: PathBuf,
    },
    Lock {
        path: PathBuf,
        source: io::Error,
    },
    ConfigRead {
        path: PathBuf,
        source: io::Error,
    },
    ConfigChanged {
        path: PathBuf,
    },
    TokenChanged {
        path: PathBuf,
    },
    PrimaryIdentityMismatch {
        path: PathBuf,
        field: &'static str,
    },
    PrimaryAlreadyConfigured {
        key: String,
    },
    ConfigOwnedByRoot {
        path: PathBuf,
    },
    ConfigOwnerMismatch {
        path: PathBuf,
        owner_uid: u32,
        euid: u32,
    },
    ConfigSymlink {
        path: PathBuf,
    },
    Owner(AuthContextError),
    TokenPath(PlatformAccessTokenStoreError),
    TokenRead {
        path: PathBuf,
        source: io::Error,
    },
    TokenWrite {
        path: PathBuf,
        source: PlatformAccessTokenStoreError,
    },
    TokenRemove {
        path: PathBuf,
        source: io::Error,
    },
    ConfigWrite {
        path: PathBuf,
        source: io::Error,
    },
    TokenRollback {
        path: PathBuf,
        source: io::Error,
    },
    Rollback {
        operation: Box<Self>,
        rollback: Box<Self>,
    },
}

impl fmt::Display for PairingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunningAsRoot => write!(f, "native TV pairing cannot run as root"),
            Self::ConfigPathHasNoParent { path } => {
                write!(
                    f,
                    "config path `{}` has no parent directory",
                    path.display()
                )
            }
            Self::PairingInProgress { path } => {
                write!(
                    f,
                    "another TV pairing is already in progress ({})",
                    path.display()
                )
            }
            Self::Lock { path, source } => {
                write!(f, "could not lock `{}`: {source}", path.display())
            }
            Self::ConfigRead { path, source } => {
                write!(f, "could not read config `{}`: {source}", path.display())
            }
            Self::ConfigChanged { path } => {
                write!(f, "config `{}` changed during pairing", path.display())
            }
            Self::TokenChanged { path } => {
                write!(
                    f,
                    "native credential `{}` changed during unpairing",
                    path.display()
                )
            }
            Self::PrimaryIdentityMismatch { path, field } => write!(
                f,
                "configured primary TV {field} does not match the selected profile in `{}`",
                path.display()
            ),
            Self::PrimaryAlreadyConfigured { key } => {
                write!(f, "primary TV is already configured ({key})")
            }
            Self::ConfigOwnedByRoot { path } => {
                write!(f, "config `{}` is owned by root", path.display())
            }
            Self::ConfigOwnerMismatch {
                path,
                owner_uid,
                euid,
            } => write!(
                f,
                "config `{}` is owned by uid {owner_uid}, current uid is {euid}",
                path.display()
            ),
            Self::ConfigSymlink { path } => {
                write!(f, "config `{}` is a symlink", path.display())
            }
            Self::Owner(source) => write!(f, "could not resolve config owner: {source}"),
            Self::TokenPath(source) => {
                write!(f, "could not derive native credential path: {source}")
            }
            Self::TokenRead { path, source } => write!(
                f,
                "could not read native credential `{}`: {source}",
                path.display()
            ),
            Self::TokenWrite { path, source } => write!(
                f,
                "could not write native credential `{}`: {source}",
                path.display()
            ),
            Self::TokenRemove { path, source } => write!(
                f,
                "could not remove native credential `{}`: {source}",
                path.display()
            ),
            Self::ConfigWrite { path, source } => {
                write!(f, "could not publish config `{}`: {source}", path.display())
            }
            Self::TokenRollback { path, source } => write!(
                f,
                "could not roll back native credential `{}`: {source}",
                path.display()
            ),
            Self::Rollback {
                operation,
                rollback,
            } => write!(f, "{operation}; rollback also failed: {rollback}"),
        }
    }
}

impl Error for PairingStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lock { source, .. }
            | Self::ConfigRead { source, .. }
            | Self::TokenRead { source, .. }
            | Self::TokenRemove { source, .. }
            | Self::ConfigWrite { source, .. }
            | Self::TokenRollback { source, .. } => Some(source),
            Self::Owner(source) => Some(source),
            Self::TokenPath(source) => Some(source),
            Self::TokenWrite { source, .. } => Some(source),
            Self::Rollback { operation, .. } => Some(operation),
            Self::RunningAsRoot
            | Self::ConfigPathHasNoParent { .. }
            | Self::PairingInProgress { .. }
            | Self::ConfigChanged { .. }
            | Self::TokenChanged { .. }
            | Self::PrimaryIdentityMismatch { .. }
            | Self::PrimaryAlreadyConfigured { .. }
            | Self::ConfigOwnedByRoot { .. }
            | Self::ConfigOwnerMismatch { .. }
            | Self::ConfigSymlink { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TvPlatform;
    use crate::tvs::{TvCredentialState, TvId, TvProfile};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("lg-buddy-pairing-{name}-{}-{id}", process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config(&self) -> PathBuf {
            self.0.join("config.env")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn token(value: &str) -> PlatformAccessToken {
        PlatformAccessToken::new(value).unwrap()
    }

    fn mac() -> MacAddress {
        "aa:bb:cc:dd:ee:ff".parse().unwrap()
    }

    fn profile(platform: TvPlatform) -> TvProfile {
        TvProfile::new(
            TvId::primary(),
            "Primary TV",
            "192.0.2.42".parse().unwrap(),
            mac(),
            HdmiInput::Hdmi2,
            platform,
            TvCredentialState::Stored,
        )
    }

    fn native_config() -> &'static str {
        "# keep this comment\n\
         screen_backend=gnome\n\
         tvs_primary_ip=192.0.2.42\n\
         tvs_primary_mac=aa:bb:cc:dd:ee:ff\n\
         tvs_primary_input=HDMI_2\n\
         tvs_primary_platform=lg_webos\n"
    }

    #[test]
    fn prepare_refuses_root_before_creating_lock() {
        let dir = TestDir::new("root");
        let error = prepare_with_euid(&dir.config(), 0)
            .err()
            .expect("root guard should reject preparation");
        assert!(matches!(error, PairingStoreError::RunningAsRoot));
        assert!(!dir
            .config()
            .with_file_name(".config.env.pairing.lock")
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_rejects_symlinked_config_without_following_or_replacing_it() {
        let dir = TestDir::new("symlink");
        let target = dir.0.join("real-config.env");
        fs::write(&target, "screen_backend=gnome\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.config()).unwrap();

        let error = PairingStore::prepare(&dir.config())
            .err()
            .expect("symlink config should be rejected");
        assert!(matches!(error, PairingStoreError::ConfigSymlink { .. }));
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            "screen_backend=gnome\n"
        );
        assert!(fs::symlink_metadata(dir.config())
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn successful_commit_preserves_unrelated_config_and_writes_native_profile() {
        let dir = TestDir::new("success");
        fs::write(dir.config(), "# keep\nscreen_backend=gnome\n").unwrap();
        let store = PairingStore::prepare(&dir.config()).unwrap();
        let defaults = store
            .commit(
                "192.0.2.42".parse().unwrap(),
                mac(),
                HdmiInput::Hdmi2,
                &token("secret"),
            )
            .unwrap();

        let config = fs::read_to_string(dir.config()).unwrap();
        assert_eq!(
            defaults,
            [
                BehaviorSetting::ScreenIdleBlank,
                BehaviorSetting::SystemSleepWakePolicy
            ]
        );
        assert!(config.contains("screen_idle_blank=disabled\n"));
        assert!(config.contains("system_sleep_wake_policy=disabled\n"));
        assert!(config.starts_with("# keep\nscreen_backend=gnome\n"));
        assert!(config.contains("tvs_primary_ip=192.0.2.42\n"));
        assert!(config.contains("tvs_primary_mac=aa:bb:cc:dd:ee:ff\n"));
        assert!(config.contains("tvs_primary_input=HDMI_2\n"));
        assert!(config.contains("tvs_primary_platform=lg_webos\n"));
        let token_path = dir.0.join("tvs/primary/access-token.json");
        let token: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(token_path).unwrap()).unwrap();
        assert_eq!(token["access_token"], "secret");
        assert!(dir
            .config()
            .with_file_name(".config.env.pairing.lock")
            .exists());
    }

    #[test]
    fn pairing_preserves_opt_outs_and_reactivates_only_requested_behaviors() {
        for (idle, sleep, expected) in [
            ("disabled", "disabled", vec![]),
            (
                "enabled",
                "disabled",
                vec![BehaviorSetting::ScreenIdleBlank],
            ),
            (
                "disabled",
                "enabled",
                vec![BehaviorSetting::SystemSleepWakePolicy],
            ),
        ] {
            let dir = TestDir::new("behavior-preferences");
            fs::write(dir.config(), format!("# retained after unpairing\nscreen_idle_blank={idle}\nsystem_sleep_wake_policy={sleep}\nscreen_idle_timeout=42\n")).unwrap();
            let defaults = PairingStore::prepare(&dir.config())
                .unwrap()
                .commit(
                    "192.0.2.42".parse().unwrap(),
                    mac(),
                    HdmiInput::Hdmi1,
                    &token("secret"),
                )
                .unwrap();
            assert_eq!(defaults, expected);
            let saved = ConfigEnvReader::load(dir.config()).unwrap().into_store();
            assert_eq!(
                saved.raw_storage_value("screen_idle_blank"),
                Some("disabled")
            );
            assert_eq!(
                saved.raw_storage_value("system_sleep_wake_policy"),
                Some("disabled")
            );
            assert_eq!(saved.raw_storage_value("screen_idle_timeout"), Some("42"));
        }
    }

    #[test]
    fn dropping_prepared_store_cancels_without_durable_changes() {
        let dir = TestDir::new("cancel");
        fs::write(dir.config(), "screen_backend=gnome\n").unwrap();
        {
            let _store = PairingStore::prepare(&dir.config()).unwrap();
        }
        assert_eq!(
            fs::read_to_string(dir.config()).unwrap(),
            "screen_backend=gnome\n"
        );
        assert!(dir
            .config()
            .with_file_name(".config.env.pairing.lock")
            .exists());
        assert!(!dir.0.join("tvs").exists());
    }

    #[test]
    fn second_primary_is_refused_after_success() {
        let dir = TestDir::new("second");
        fs::write(dir.config(), "screen_backend=gnome\n").unwrap();
        let store = PairingStore::prepare(&dir.config()).unwrap();
        store
            .commit(
                "192.0.2.42".parse().unwrap(),
                mac(),
                HdmiInput::Hdmi1,
                &token("secret"),
            )
            .unwrap();
        assert!(matches!(
            PairingStore::prepare(&dir.config()),
            Err(PairingStoreError::PrimaryAlreadyConfigured { .. })
        ));
    }

    #[test]
    fn concurrent_config_mutation_is_rejected_without_writing_token() {
        let dir = TestDir::new("concurrent");
        fs::write(dir.config(), "screen_backend=gnome\n").unwrap();
        let store = PairingStore::prepare(&dir.config()).unwrap();
        fs::write(dir.config(), "screen_backend=wayland\n").unwrap();
        let error = store
            .commit(
                "192.0.2.42".parse().unwrap(),
                mac(),
                HdmiInput::Hdmi1,
                &token("secret"),
            )
            .unwrap_err();
        assert!(matches!(error, PairingStoreError::ConfigChanged { .. }));
        assert!(!dir.0.join("tvs").exists());
    }

    #[test]
    fn preexisting_orphan_token_is_unchanged_when_prepare_is_cancelled() {
        let dir = TestDir::new("orphan");
        fs::write(dir.config(), "screen_backend=gnome\n").unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        let original = b"{\n  \"access_token\": \"orphan\"\n}\n";
        fs::write(&token_path, original).unwrap();
        {
            let _store = PairingStore::prepare(&dir.config()).unwrap();
        }
        assert_eq!(fs::read(token_path).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn dropping_lock_releases_flock_without_replacing_marker_inode() {
        let dir = TestDir::new("lock-lifecycle");
        let lock_path = dir.config().with_file_name(".config.env.pairing.lock");
        let first = PairingLock::acquire(lock_path.clone(), dir.0.clone()).unwrap();
        // A forked child can retain this open file description until exec.
        let inherited = first.file.try_clone().unwrap();
        let before = fs::metadata(&lock_path).unwrap().ino();
        assert!(matches!(
            PairingLock::acquire(lock_path.clone(), dir.0.clone()),
            Err(PairingStoreError::PairingInProgress { .. })
        ));
        drop(first);
        let after = fs::metadata(&lock_path).unwrap().ino();
        assert_eq!(before, after);

        let second = PairingLock::acquire(lock_path.clone(), dir.0.clone()).unwrap();
        drop(inherited);
        assert!(matches!(
            PairingLock::acquire(lock_path.clone(), dir.0.clone()),
            Err(PairingStoreError::PairingInProgress { .. })
        ));
        drop(second);
        assert_eq!(fs::metadata(lock_path).unwrap().ino(), before);
    }

    #[cfg(unix)]
    #[test]
    fn config_failure_rolls_back_new_token() {
        let dir = TestDir::new("rollback");
        fs::write(dir.config(), "screen_backend=gnome\n").unwrap();
        fs::create_dir_all(dir.0.join("tvs/primary")).unwrap();
        let store = PairingStore::prepare(&dir.config()).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o500)).unwrap();
        let result = store.commit(
            "192.0.2.42".parse().unwrap(),
            mac(),
            HdmiInput::Hdmi1,
            &token("secret"),
        );
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert!(!dir.0.join("tvs/primary/access-token.json").exists());
        assert_eq!(
            fs::read_to_string(dir.config()).unwrap(),
            "screen_backend=gnome\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_failure_restores_preexisting_orphan_token_bytes() {
        let dir = TestDir::new("rollback-orphan");
        fs::write(dir.config(), "screen_backend=gnome\n").unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        let original = b"orphan bytes that must survive exactly\n";
        fs::write(&token_path, original).unwrap();
        let store = PairingStore::prepare(&dir.config()).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o500)).unwrap();
        let result = store.commit(
            "192.0.2.42".parse().unwrap(),
            mac(),
            HdmiInput::Hdmi1,
            &token("secret"),
        );
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(fs::read(token_path).unwrap(), original);
    }

    #[test]
    fn unpair_native_removes_all_primary_keys_and_native_token() {
        let dir = TestDir::new("unpair-native");
        fs::write(dir.config(), native_config()).unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        fs::write(&token_path, "{\"access_token\":\"native\"}\n").unwrap();
        let legacy = dir.0.join(".aiopylgtv.sqlite");
        fs::write(&legacy, b"legacy database").unwrap();

        PairingStore::unpair_primary(&dir.config(), &profile(TvPlatform::LgWebOs)).unwrap();

        assert_eq!(
            fs::read_to_string(dir.config()).unwrap(),
            "# keep this comment\nscreen_backend=gnome\n"
        );
        assert!(!token_path.exists());
        assert_eq!(fs::read(legacy).unwrap(), b"legacy database");
    }

    #[test]
    fn unpair_compatibility_removes_config_but_preserves_missing_native_token() {
        let dir = TestDir::new("unpair-compatibility");
        fs::write(
            dir.config(),
            "screen_backend=gnome\n\
             tv_ip=192.0.2.42\n\
             tv_mac=aa:bb:cc:dd:ee:ff\n\
             input=HDMI_2\n",
        )
        .unwrap();
        let legacy = dir.0.join(".aiopylgtv.sqlite");
        fs::write(&legacy, b"legacy database").unwrap();

        PairingStore::unpair_primary(&dir.config(), &profile(TvPlatform::Bscpylgtv)).unwrap();

        assert_eq!(
            fs::read_to_string(dir.config()).unwrap(),
            "screen_backend=gnome\n"
        );
        assert!(!dir.0.join("tvs/primary/access-token.json").exists());
        assert_eq!(fs::read(legacy).unwrap(), b"legacy database");
    }

    #[test]
    fn dropping_unpair_transaction_is_cancellation_equivalent() {
        let dir = TestDir::new("unpair-cancel");
        fs::write(dir.config(), native_config()).unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        let original_token = b"malformed token bytes\n";
        fs::write(&token_path, original_token).unwrap();

        {
            let _store = prepare_unpair_with_euid(&dir.config(), current_euid()).unwrap();
        }

        assert_eq!(fs::read_to_string(dir.config()).unwrap(), native_config());
        assert_eq!(fs::read(token_path).unwrap(), original_token);
    }

    #[cfg(unix)]
    #[test]
    fn unpair_config_failure_restores_original_token_and_config() {
        let dir = TestDir::new("unpair-rollback");
        let original_config = native_config();
        fs::write(dir.config(), original_config).unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        let original_token = b"malformed token bytes\n";
        fs::write(&token_path, original_token).unwrap();
        let store = prepare_unpair_with_euid(&dir.config(), current_euid()).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o500)).unwrap();

        let result = store.unpair(&profile(TvPlatform::LgWebOs));

        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(PairingStoreError::ConfigWrite { .. })));
        assert_eq!(fs::read_to_string(dir.config()).unwrap(), original_config);
        assert_eq!(fs::read(token_path).unwrap(), original_token);
    }

    #[test]
    fn unpair_refuses_stale_profile_before_removing_token() {
        let dir = TestDir::new("unpair-stale");
        fs::write(dir.config(), native_config()).unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        fs::write(&token_path, b"token").unwrap();

        let stale = TvProfile::new(
            TvId::primary(),
            "Primary TV",
            "192.0.2.99".parse().unwrap(),
            mac(),
            HdmiInput::Hdmi2,
            TvPlatform::LgWebOs,
            TvCredentialState::Stored,
        );
        let result = PairingStore::unpair_primary(&dir.config(), &stale);

        assert!(matches!(
            result,
            Err(PairingStoreError::PrimaryIdentityMismatch { .. })
        ));
        assert_eq!(fs::read_to_string(dir.config()).unwrap(), native_config());
        assert_eq!(fs::read(token_path).unwrap(), b"token");
    }

    #[test]
    fn unpair_removes_malformed_regular_token() {
        let dir = TestDir::new("unpair-malformed");
        fs::write(dir.config(), native_config()).unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        fs::write(&token_path, b"not json").unwrap();

        PairingStore::unpair_primary(&dir.config(), &profile(TvPlatform::LgWebOs)).unwrap();

        assert!(!token_path.exists());
        assert_eq!(
            fs::read_to_string(dir.config()).unwrap(),
            "# keep this comment\nscreen_backend=gnome\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unpair_refuses_unsafe_token_symlink() {
        let dir = TestDir::new("unpair-token-symlink");
        fs::write(dir.config(), native_config()).unwrap();
        let token_path = dir.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        let target = dir.0.join("outside-token");
        fs::write(&target, b"must survive").unwrap();
        std::os::unix::fs::symlink(&target, &token_path).unwrap();

        assert!(
            PairingStore::unpair_primary(&dir.config(), &profile(TvPlatform::LgWebOs)).is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"must survive");
        assert!(fs::symlink_metadata(token_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(dir.config()).unwrap(), native_config());
    }
}
