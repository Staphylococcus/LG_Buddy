//! Config-only staging and publication shared by migration transactions.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigWriteError {
    Changed,
    Storage,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Point {
    StageWrite,
    FileSync,
    ConfigRename,
    RenameResult,
    DirectorySync,
}

#[derive(PartialEq, Eq)]
pub(super) struct Identity {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    mtime: (i64, i64),
}
impl Identity {
    pub(super) fn of(m: &fs::Metadata) -> Self {
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
pub(crate) struct ConfigSnapshot {
    pub(crate) bytes: Vec<u8>,
    identity: Identity,
}

pub(crate) fn config_snapshot(
    path: &Path,
    owner: &SystemUser,
) -> Result<ConfigSnapshot, ConfigWriteError> {
    ensure_config_owner(path, owner).map_err(|_| ConfigWriteError::Storage)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| ConfigWriteError::Storage)?;
    let metadata = file.metadata().map_err(|_| ConfigWriteError::Storage)?;
    if !metadata.is_file() || metadata.uid() != owner.uid() || metadata.mode() & 0o200 == 0 {
        return Err(ConfigWriteError::Storage);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| ConfigWriteError::Storage)?;
    let identity = Identity::of(&metadata);
    if Identity::of(&fs::symlink_metadata(path).map_err(|_| ConfigWriteError::Storage)?) != identity
    {
        return Err(ConfigWriteError::Changed);
    }
    Ok(ConfigSnapshot { bytes, identity })
}

impl ConfigSnapshot {
    pub(crate) fn check(&self, path: &Path, owner: &SystemUser) -> Result<(), ConfigWriteError> {
        let current = config_snapshot(path, owner)?;
        if current.identity != self.identity || current.bytes != self.bytes {
            return Err(ConfigWriteError::Changed);
        }
        Ok(())
    }

    pub(crate) fn stage(
        &self,
        path: &Path,
        bytes: &[u8],
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<StagedConfig, ConfigWriteError> {
        StagedConfig::new(path, bytes, &self.identity, hook)
    }
}

pub(crate) fn owner_for_write(path: &Path) -> Result<SystemUser, ConfigWriteError> {
    if current_euid() == 0 {
        return Err(ConfigWriteError::Storage);
    }
    let owner = resolve_owner(path, true, current_euid()).map_err(|_| ConfigWriteError::Storage)?;
    if owner.uid() != current_euid() {
        return Err(ConfigWriteError::Storage);
    }
    ensure_config_owner(path, &owner).map_err(|_| ConfigWriteError::Storage)?;
    Ok(owner)
}

pub(crate) struct StagedConfig(PathBuf);
impl StagedConfig {
    fn new(
        path: &Path,
        bytes: &[u8],
        identity: &Identity,
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<Self, ConfigWriteError> {
        let temp_path = temporary_path(path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp_path)
            .map_err(|_| ConfigWriteError::Storage)?;
        // Own cleanup only after create_new proved that this is our file.
        let stage = Self(temp_path);
        let result = (|| {
            hook(Point::StageWrite)?;
            file.write_all(bytes)?;
            set_owner_ids(&file, identity.uid, identity.gid)?;
            file.set_permissions(fs::Permissions::from_mode(identity.mode & 0o7777))?;
            hook(Point::FileSync)?;
            file.sync_all()
        })();
        result.map_err(|_| ConfigWriteError::Storage)?;
        Ok(stage)
    }

    /// The full-file rename is the only publication point. Callers revalidate
    /// their config/credential snapshots immediately before this operation.
    pub(crate) fn publish(
        &self,
        path: &Path,
        original: &[u8],
        rendered: &[u8],
        hook: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<bool, ConfigWriteError> {
        let renamed = hook(Point::ConfigRename)
            .and_then(|_| fs::rename(&self.0, path))
            .and_then(|_| hook(Point::RenameResult));
        if renamed.is_err() {
            match fs::read(path) {
                Ok(bytes) if bytes == rendered => {}
                Ok(bytes) if bytes == original => return Err(ConfigWriteError::Storage),
                _ => return Err(ConfigWriteError::Indeterminate),
            }
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(hook(Point::DirectorySync)
            .and_then(|_| File::open(parent)?.sync_all())
            .is_err())
    }
}
impl Drop for StagedConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
