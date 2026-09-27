//! The config rename is the migration commit point. The advisory lock excludes
//! cooperating writers, not arbitrary external editors; compare again before
//! publication and never roll a credential back under a possibly committed config.
use super::*;
use crate::config::{stale_config_reasons, StaleConfigReason};
use crate::migration::MigrationCandidate;
use crate::setup::StepCancellation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationStoreError {
    Cancelled,
    ConfigurationChanged,
    CredentialChanged,
    CredentialInvalid,
    Storage,
    RollbackFailed,
    CommitIndeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MigrationCommit {
    pub durability_warning: bool,
}

#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    mtime: (i64, i64),
}
impl Identity {
    fn of(m: &fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            uid: m.uid(),
            gid: m.gid(),
            mode: m.mode(),
            mtime: (m.mtime(), m.mtime_nsec()),
        }
    }
}
struct ConfigSnapshot {
    bytes: Vec<u8>,
    identity: Identity,
}
struct CredentialSnapshot {
    file: Option<TokenBefore>,
    identity: Option<Identity>,
}

/// Contains private original bytes, never Debug/serialized. No lock is held
/// between capture and commit, including during pairing or capability probes.
pub(crate) struct MigrationSnapshot {
    path: PathBuf,
    config: ConfigSnapshot,
    owner: SystemUser,
    credential: Option<CredentialSnapshot>,
    profile_existed: bool,
    tvs_existed: bool,
}

fn config_snapshot(path: &Path, owner: &SystemUser) -> Result<ConfigSnapshot, MigrationStoreError> {
    ensure_config_owner(path, owner).map_err(|_| MigrationStoreError::Storage)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| MigrationStoreError::Storage)?;
    let metadata = file.metadata().map_err(|_| MigrationStoreError::Storage)?;
    if !metadata.is_file() || metadata.uid() != owner.uid() || metadata.mode() & 0o200 == 0 {
        return Err(MigrationStoreError::Storage);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| MigrationStoreError::Storage)?;
    let identity = Identity::of(&metadata);
    if Identity::of(&fs::symlink_metadata(path).map_err(|_| MigrationStoreError::Storage)?)
        != identity
    {
        return Err(MigrationStoreError::ConfigurationChanged);
    }
    Ok(ConfigSnapshot { bytes, identity })
}

fn credential_snapshot(
    store: &PlatformAccessTokenStore,
    owner: &SystemUser,
) -> Result<CredentialSnapshot, MigrationStoreError> {
    ensure_token_path_safe(store.token_path(), owner).map_err(|_| MigrationStoreError::Storage)?;
    let file = read_existing_token(store.token_path()).map_err(|_| MigrationStoreError::Storage)?;
    let identity = match fs::symlink_metadata(store.token_path()) {
        Ok(m) => Some(Identity::of(&m)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err(MigrationStoreError::Storage),
    };
    Ok(CredentialSnapshot { file, identity })
}
fn same_credential(a: &CredentialSnapshot, b: &CredentialSnapshot) -> bool {
    a.identity == b.identity && same_token_snapshot(a.file.as_ref(), b.file.as_ref())
}

impl MigrationSnapshot {
    pub(crate) fn capture(path: &Path) -> Result<Self, MigrationStoreError> {
        if current_euid() == 0 {
            return Err(MigrationStoreError::Storage);
        }
        let _guard = PairingLock::for_config(path).map_err(|_| MigrationStoreError::Storage)?;
        let owner =
            resolve_owner(path, true, current_euid()).map_err(|_| MigrationStoreError::Storage)?;
        if owner.uid() != current_euid() {
            return Err(MigrationStoreError::Storage);
        }
        let config = config_snapshot(path, &owner)?;
        let raw = std::str::from_utf8(&config.bytes).map_err(|_| MigrationStoreError::Storage)?;
        let needs_token = stale_config_reasons(raw).iter().any(|r| {
            matches!(
                r,
                StaleConfigReason::MissingTvPlatform | StaleConfigReason::BscpylgtvPlatform
            )
        });
        let store = PlatformAccessTokenStore::for_primary_profile(path, owner.clone())
            .map_err(|_| MigrationStoreError::Storage)?;
        let credential = if needs_token {
            Some(credential_snapshot(&store, &owner)?)
        } else {
            None
        };
        Ok(Self {
            path: path.into(),
            config,
            owner,
            credential,
            profile_existed: store.token_path().parent().unwrap().exists(),
            tvs_existed: store
                .token_path()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .exists(),
        })
    }

    pub(crate) fn contents(&self) -> &str {
        std::str::from_utf8(&self.config.bytes).expect("capture validated UTF-8")
    }

    pub(crate) fn existing_token(
        &self,
    ) -> Result<Option<PlatformAccessToken>, MigrationStoreError> {
        let Some(snapshot) = &self.credential else {
            return Ok(None);
        };
        let Some(file) = &snapshot.file else {
            return Ok(None);
        };
        // Parse the captured bytes, never reread a credential after the plan.
        let value: serde_json::Value = serde_json::from_slice(&file.contents)
            .map_err(|_| MigrationStoreError::CredentialInvalid)?;
        value
            .get("access_token")
            .and_then(|v| v.as_str())
            .and_then(|s| PlatformAccessToken::new(s).ok())
            .map(Some)
            .ok_or(MigrationStoreError::CredentialInvalid)
    }

    fn check_config(&self) -> Result<(), MigrationStoreError> {
        let current = config_snapshot(&self.path, &self.owner)?;
        if current.identity != self.config.identity || current.bytes != self.config.bytes {
            return Err(MigrationStoreError::ConfigurationChanged);
        }
        Ok(())
    }

    pub(crate) fn commit(
        &self,
        candidate: &MigrationCandidate,
        token: Option<&PlatformAccessToken>,
        gate: &StepCancellation,
    ) -> Result<MigrationCommit, MigrationStoreError> {
        self.commit_with(candidate, token, gate, &mut |_| Ok(()))
    }

    fn commit_with(
        &self,
        candidate: &MigrationCandidate,
        token: Option<&PlatformAccessToken>,
        gate: &StepCancellation,
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<MigrationCommit, MigrationStoreError> {
        if gate.is_cancelled() {
            return Err(MigrationStoreError::Cancelled);
        }
        if candidate.path() != self.path
            || candidate.validate_current().is_err()
            || candidate.requires_tv_pairing() != token.is_some()
        {
            return Err(MigrationStoreError::Storage);
        }
        let guard =
            PairingLock::for_config(&self.path).map_err(|_| MigrationStoreError::Storage)?;
        self.check_config()?;
        let store = PlatformAccessTokenStore::for_primary_profile(&self.path, self.owner.clone())
            .map_err(|_| MigrationStoreError::Storage)?;
        if let Some(before) = &self.credential {
            if !same_credential(before, &credential_snapshot(&store, &self.owner)?) {
                return Err(MigrationStoreError::CredentialChanged);
            }
        }
        // Fully stage and sync config while cancellation can still win.
        let stage = StagedConfig::new(
            &self.path,
            candidate.rendered().as_bytes(),
            &self.owner,
            self.config.identity.mode,
            hook,
        )?;
        self.check_config()?;
        if !gate.begin() {
            return Err(MigrationStoreError::Cancelled);
        }
        let result = self.publish(candidate, token, &store, &guard, &stage, hook);
        gate.finish();
        result
    }

    fn publish(
        &self,
        candidate: &MigrationCandidate,
        token: Option<&PlatformAccessToken>,
        store: &PlatformAccessTokenStore,
        guard: &PairingLock,
        stage: &StagedConfig,
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<MigrationCommit, MigrationStoreError> {
        let mut written = None;
        let result = (|| {
            if let Some(token) = token {
                if self.existing_token()?.as_ref() != Some(token) {
                    hook(Point::TokenPublish).map_err(|_| MigrationStoreError::Storage)?;
                    let write = store.persist_locked(token, guard);
                    // Reconcile even an error: rename errors can be ambiguous.
                    if store.load().ok().flatten().as_ref() == Some(token) {
                        written = Some(credential_snapshot(store, &self.owner)?);
                    }
                    write.map_err(|_| MigrationStoreError::Storage)?;
                }
                // A surviving token from an interrupted attempt also needs a
                // durability barrier before publishing a config that uses it.
                hook(Point::TokenSync)
                    .and_then(|_| File::open(store.token_path())?.sync_all())
                    .and_then(|_| sync_token_dirs(store))
                    .map_err(|_| MigrationStoreError::Storage)?;
            }
            hook(Point::BeforeConfig).map_err(|_| MigrationStoreError::Storage)?;
            self.check_config()?;
            if let Some(expected) = written.as_ref().or(self.credential.as_ref()) {
                if !same_credential(expected, &credential_snapshot(store, &self.owner)?) {
                    return Err(MigrationStoreError::CredentialChanged);
                }
            }
            // The only publication of config is this full-file rename.
            let renamed = hook(Point::ConfigRename)
                .and_then(|_| fs::rename(&stage.0, &self.path))
                .and_then(|_| hook(Point::RenameResult));
            if renamed.is_err() {
                match fs::read(&self.path) {
                    Ok(bytes) if bytes == candidate.rendered().as_bytes() => {}
                    Ok(bytes) if bytes == self.config.bytes => {
                        return Err(MigrationStoreError::Storage)
                    }
                    _ => return Err(MigrationStoreError::CommitIndeterminate),
                }
            }
            let durability_warning = hook(Point::DirectorySync)
                .and_then(|_| File::open(self.path.parent().unwrap())?.sync_all())
                .is_err();
            Ok(MigrationCommit { durability_warning })
        })();
        if let Err(error) = result {
            if error != MigrationStoreError::CommitIndeterminate {
                if let Some(ours) = written {
                    // An external editor need not honor our lock. If it has
                    // published a different config after token publication,
                    // never restore an old credential underneath that config.
                    if fs::read(&self.path).ok().as_deref() != Some(self.config.bytes.as_slice()) {
                        return Err(MigrationStoreError::CommitIndeterminate);
                    }
                    if self.rollback_token(store, &ours, hook).is_err() {
                        return Err(MigrationStoreError::RollbackFailed);
                    }
                } else if self.credential.as_ref().is_some_and(|c| c.file.is_none()) {
                    // A failed temp write may have created empty directories.
                    self.cleanup_dirs(store)
                        .map_err(|_| MigrationStoreError::RollbackFailed)?;
                }
            }
        }
        result
    }

    fn cleanup_dirs(&self, store: &PlatformAccessTokenStore) -> Result<(), PairingStoreError> {
        if !self.profile_existed {
            remove_empty_dir(store.token_path().parent().unwrap())?;
        }
        if !self.tvs_existed {
            remove_empty_dir(store.token_path().parent().unwrap().parent().unwrap())?;
        }
        Ok(())
    }
    fn rollback_token(
        &self,
        store: &PlatformAccessTokenStore,
        ours: &CredentialSnapshot,
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<(), MigrationStoreError> {
        hook(Point::Rollback).map_err(|_| MigrationStoreError::RollbackFailed)?;
        if !same_credential(ours, &credential_snapshot(store, &self.owner)?) {
            return Err(MigrationStoreError::RollbackFailed);
        }
        match self.credential.as_ref().and_then(|c| c.file.as_ref()) {
            Some(before) => atomic_write_bytes(store.token_path(), before)
                .map_err(|_| MigrationStoreError::RollbackFailed)?,
            None => fs::remove_file(store.token_path())
                .map_err(|_| MigrationStoreError::RollbackFailed)?,
        }
        sync_token_dirs(store).map_err(|_| MigrationStoreError::RollbackFailed)?;
        self.cleanup_dirs(store)
            .map_err(|_| MigrationStoreError::RollbackFailed)?;
        File::open(self.path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|_| MigrationStoreError::RollbackFailed)
    }
}

fn sync_token_dirs(store: &PlatformAccessTokenStore) -> io::Result<()> {
    let profile = store.token_path().parent().unwrap();
    for path in [
        profile,
        profile.parent().unwrap(),
        profile.parent().unwrap().parent().unwrap(),
    ] {
        File::open(path)?.sync_all()?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Point {
    StageWrite,
    FileSync,
    TokenPublish,
    TokenSync,
    BeforeConfig,
    ConfigRename,
    RenameResult,
    DirectorySync,
    Rollback,
}
struct StagedConfig(PathBuf);
impl StagedConfig {
    fn new(
        path: &Path,
        bytes: &[u8],
        owner: &SystemUser,
        mode: u32,
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<Self, MigrationStoreError> {
        let temp_path = temporary_path(path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp_path)
            .map_err(|_| MigrationStoreError::Storage)?;
        // Own cleanup only after create_new proved that this is our file.
        let stage = Self(temp_path);
        let result = (|| {
            hook(Point::StageWrite)?;
            file.write_all(bytes)?;
            set_owner(&file, owner)?;
            file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
            hook(Point::FileSync)?;
            file.sync_all()
        })();
        result.map_err(|_| MigrationStoreError::Storage)?;
        Ok(stage)
    }
}
impl Drop for StagedConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests;
