//! The config rename is the migration commit point. The advisory lock excludes
//! cooperating writers, not arbitrary external editors; compare again before
//! publication and never roll a credential back under a possibly committed config.
use super::config::{
    config_snapshot, ConfigSnapshot, ConfigWriteError, Identity, Point as ConfigPoint,
};
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
        self.config
            .check(&self.path, &self.owner)
            .map_err(Into::into)
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
        let stage =
            self.config
                .stage(&self.path, candidate.rendered().as_bytes(), &mut |point| {
                    hook(point.into())
                })?;
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
        stage: &super::config::StagedConfig,
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
            let durability_warning = stage.publish(
                &self.path,
                &self.config.bytes,
                candidate.rendered().as_bytes(),
                &mut |point| hook(point.into()),
            )?;
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

impl From<ConfigWriteError> for MigrationStoreError {
    fn from(error: ConfigWriteError) -> Self {
        match error {
            ConfigWriteError::Changed => Self::ConfigurationChanged,
            ConfigWriteError::Storage => Self::Storage,
            ConfigWriteError::Indeterminate => Self::CommitIndeterminate,
        }
    }
}

impl From<ConfigPoint> for Point {
    fn from(point: ConfigPoint) -> Self {
        match point {
            ConfigPoint::StageWrite => Self::StageWrite,
            ConfigPoint::FileSync => Self::FileSync,
            ConfigPoint::ConfigRename => Self::ConfigRename,
            ConfigPoint::RenameResult => Self::RenameResult,
            ConfigPoint::DirectorySync => Self::DirectorySync,
        }
    }
}

#[cfg(test)]
mod tests;
