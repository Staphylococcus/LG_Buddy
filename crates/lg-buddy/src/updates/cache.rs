// Update cache: path resolution and the file-backed cache store.
// Moved verbatim from updates.rs; the items the parent orchestrator and the
// colocated tests touch are promoted to `pub(super)`.
// `UpdateCachePathError` stays `pub` in the parent (external API) and is
// re-exported by it.
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process;

use super::{UpdateCheckCache, UpdatesError};

const CACHE_DIR_NAME: &str = "lg-buddy";
const UPDATE_CHECK_CACHE_FILE_NAME: &str = "update-check.json";

#[derive(Debug, Clone, Default)]
pub(super) struct UpdateCachePathSources<'a> {
    pub(super) xdg_cache_home: Option<&'a Path>,
    pub(super) home: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCachePathError {
    NotConfigured,
}

impl fmt::Display for UpdateCachePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => write!(
                f,
                "could not resolve an update cache path from XDG_CACHE_HOME or HOME"
            ),
        }
    }
}

impl Error for UpdateCachePathError {}

pub(super) fn resolve_update_cache_path(
    sources: UpdateCachePathSources<'_>,
) -> Result<PathBuf, UpdateCachePathError> {
    if let Some(path) = sources.xdg_cache_home {
        return Ok(path.join(CACHE_DIR_NAME).join(UPDATE_CHECK_CACHE_FILE_NAME));
    }

    if let Some(path) = sources.home {
        return Ok(path
            .join(".cache")
            .join(CACHE_DIR_NAME)
            .join(UPDATE_CHECK_CACHE_FILE_NAME));
    }

    Err(UpdateCachePathError::NotConfigured)
}

pub(super) fn resolve_update_cache_path_from_env() -> Result<PathBuf, UpdateCachePathError> {
    let xdg_cache_home = non_empty_env_path("XDG_CACHE_HOME");
    let home = non_empty_env_path("HOME");

    resolve_update_cache_path(UpdateCachePathSources {
        xdg_cache_home: xdg_cache_home.as_deref(),
        home: home.as_deref(),
    })
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

pub(super) trait UpdateCacheStore {
    fn load(&self) -> Result<UpdateCheckCache, UpdatesError>;
    fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError>;
}

pub(super) struct FileUpdateCacheStore {
    path: PathBuf,
}

impl FileUpdateCacheStore {
    #[cfg(test)]
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

pub(super) enum DefaultUpdateCacheStore {
    File(FileUpdateCacheStore),
    Unavailable(UpdateCachePathError),
}

impl DefaultUpdateCacheStore {
    pub(super) fn from_env() -> Self {
        match resolve_update_cache_path_from_env() {
            Ok(path) => Self::File(FileUpdateCacheStore { path }),
            Err(err) => Self::Unavailable(err),
        }
    }
}

impl UpdateCacheStore for DefaultUpdateCacheStore {
    fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
        match self {
            Self::File(store) => store.load(),
            Self::Unavailable(_) => Ok(UpdateCheckCache::default()),
        }
    }

    fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
        match self {
            Self::File(store) => store.save(cache),
            Self::Unavailable(err) => Err(UpdatesError::CachePath(err.clone())),
        }
    }
}

impl UpdateCacheStore for FileUpdateCacheStore {
    fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                serde_json::from_str(&contents).map_err(|source| UpdatesError::CacheDecode {
                    path: self.path.clone(),
                    source,
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(UpdateCheckCache::default()),
            Err(err) => Err(UpdatesError::Io(err)),
        }
    }

    fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
        let contents = serde_json::to_vec_pretty(cache).map_err(UpdatesError::CacheEncode)?;
        atomic_write_file(&self.path, &contents).map_err(UpdatesError::Io)
    }
}

pub(super) fn atomic_write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            ensure_cache_parent(parent)?;
        }
    }

    let mut last_error = None;
    for attempt in 0..100 {
        let temp_path = atomic_temp_path(path, attempt);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);

        let mut file = match options.open(&temp_path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(err);
                continue;
            }
            Err(err) => return Err(err),
        };

        let result = (|| {
            file.write_all(contents)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp_path, path)
        })();

        if let Err(err) = result {
            let _ = fs::remove_file(&temp_path);
            return Err(err);
        }

        return Ok(());
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create unique update cache temporary file",
        )
    }))
}

#[cfg(unix)]
fn ensure_cache_parent(parent: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!(
                        "cache path component `{}` is not a directory",
                        current.display()
                    ),
                ))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o700).create(&current) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(err),
                }
                if !fs::symlink_metadata(&current)?.file_type().is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!(
                            "cache path component `{}` is not a directory",
                            current.display()
                        ),
                    ));
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_cache_parent(parent: &Path) -> io::Result<()> {
    fs::create_dir_all(parent)
}

fn atomic_temp_path(path: &Path, attempt: u8) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(UPDATE_CHECK_CACHE_FILE_NAME);
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", process::id(), attempt))
}
