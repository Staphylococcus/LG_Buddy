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

#[cfg(test)]
mod tests {
    use super::super::tests::{
        cached_entry, cached_entry_with_notification, env_lock, unique_temp_dir, UmaskGuard,
        TEST_NOW,
    };
    use super::super::{ReleaseAsset, UpdateChannel, UpdateCheckCache, UpdatesError};
    use super::{
        atomic_write_file, resolve_update_cache_path, resolve_update_cache_path_from_env,
        FileUpdateCacheStore, UpdateCachePathSources, UpdateCacheStore,
    };
    use std::fs;
    use std::io;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    #[test]
    fn cache_path_resolver_prefers_xdg_cache_home() {
        let xdg_cache_home = PathBuf::from("/tmp/xdg-cache");
        let home = PathBuf::from("/home/test-user");

        let path = resolve_update_cache_path(UpdateCachePathSources {
            xdg_cache_home: Some(&xdg_cache_home),
            home: Some(&home),
        })
        .expect("resolve cache path");

        assert_eq!(
            path,
            PathBuf::from("/tmp/xdg-cache/lg-buddy/update-check.json")
        );
    }

    #[test]
    fn cache_path_resolver_falls_back_to_home_cache() {
        let home = PathBuf::from("/home/test-user");

        let path = resolve_update_cache_path(UpdateCachePathSources {
            xdg_cache_home: None,
            home: Some(&home),
        })
        .expect("resolve cache path");

        assert_eq!(
            path,
            PathBuf::from("/home/test-user/.cache/lg-buddy/update-check.json")
        );
    }

    #[test]
    fn empty_env_paths_are_treated_as_unset_for_cache_resolution() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_xdg_cache_home = std::env::var_os("XDG_CACHE_HOME");
        let original_home = std::env::var_os("HOME");

        std::env::set_var("XDG_CACHE_HOME", "");
        std::env::set_var("HOME", "/home/test-user");

        let path = resolve_update_cache_path_from_env().expect("resolve cache path");

        assert_eq!(
            path,
            PathBuf::from("/home/test-user/.cache/lg-buddy/update-check.json")
        );

        match original_xdg_cache_home {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        match original_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn missing_cache_loads_as_empty_and_malformed_cache_reports_decode_error() {
        let dir = unique_temp_dir("malformed-cache");
        let path = dir.join("lg-buddy").join("update-check.json");
        let store = FileUpdateCacheStore::new(path.clone());

        assert_eq!(
            store.load().expect("missing cache should load"),
            UpdateCheckCache::default()
        );

        fs::create_dir_all(path.parent().expect("cache path parent")).expect("create cache dir");
        fs::write(&path, "{").expect("write malformed cache");

        let err = store
            .load()
            .expect_err("malformed cache should report decode error");

        assert!(
            matches!(err, UpdatesError::CacheDecode { path: error_path, .. } if error_path == path)
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn file_cache_round_trips_entries_and_preserves_other_channel() {
        let dir = unique_temp_dir("cache-roundtrip");
        let path = dir.join("lg-buddy").join("update-check.json");
        let store = FileUpdateCacheStore::new(path);

        let mut cache = UpdateCheckCache::default();
        let mut stable_entry = cached_entry_with_notification(
            Some("\"stable-etag\""),
            "1.1.0",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.0",
            TEST_NOW,
            TEST_NOW + 1,
        );
        stable_entry.latest.tag_name = Some("v1.1.0".to_string());
        stable_entry.latest.assets = vec![ReleaseAsset::from_github(
            42,
            "lg-buddy-1.1.0-x86_64-unknown-linux-musl.tar.gz".to_string(),
            "uploaded".to_string(),
            1234,
            Some(format!("sha256:{}", "a".repeat(64))),
            "https://api.github.test/releases/assets/42".to_string(),
            "https://github.test/releases/download/v1.1.0/bundle.tar.gz".to_string(),
        )];
        cache.set_entry(UpdateChannel::Stable, stable_entry);
        cache.set_entry(
            UpdateChannel::Prerelease,
            cached_entry(
                Some("\"prerelease-etag\""),
                "1.2.0-beta.1",
                UpdateChannel::Prerelease,
                "https://github.test/releases/tag/v1.2.0-beta.1",
                TEST_NOW + 1,
            ),
        );

        store.save(&cache).expect("save cache");
        assert_eq!(store.load().expect("load cache"), cache);

        let mut updated = store.load().expect("load cache for update");
        updated.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag-2\""),
                "1.1.1",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.1",
                TEST_NOW + 2,
            ),
        );
        store.save(&updated).expect("save updated cache");

        let loaded = store.load().expect("load updated cache");
        assert_eq!(
            loaded.entry(UpdateChannel::Stable),
            updated.entry(UpdateChannel::Stable)
        );
        assert_eq!(
            loaded.entry(UpdateChannel::Prerelease),
            cache.entry(UpdateChannel::Prerelease)
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn file_cache_creates_private_path_and_file_under_group_writable_umask() {
        const CHILD_ENV: &str = "LG_BUDDY_TEST_CACHE_PERMISSIONS_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let status = std::process::Command::new(
                std::env::current_exe().expect("resolve current test executable"),
            )
            .arg("file_cache_creates_private_path_and_file_under_group_writable_umask")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .status()
            .expect("run isolated cache-permissions regression");
            assert!(status.success(), "isolated cache-permissions test failed");
            return;
        }

        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = unique_temp_dir("cache-permissions");
        let home = dir.join("home");
        fs::create_dir(&home).expect("create test home");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o750))
            .expect("set test home permissions");
        let path = home
            .join(".cache")
            .join("lg-buddy")
            .join("update-check.json");

        let _umask = UmaskGuard::set(0o002);
        FileUpdateCacheStore::new(path.clone())
            .save(&UpdateCheckCache::default())
            .expect("save cache");

        for directory in [home.join(".cache"), home.join(".cache").join("lg-buddy")] {
            assert_eq!(
                fs::symlink_metadata(directory)
                    .expect("cache directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::symlink_metadata(path)
                .expect("cache file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn cache_without_notification_state_loads_with_absent_notification() {
        let cache: UpdateCheckCache = serde_json::from_str(
            r#"{
              "stable": {
                "etag": "\"stable-etag\"",
                "last_checked_at_unix_seconds": 1778234400,
                "latest": {
                  "version": "1.1.0",
                  "channel": "stable",
                  "url": "https://github.test/releases/tag/v1.1.0"
                }
              }
            }"#,
        )
        .expect("legacy cache should decode");

        assert_eq!(
            cache
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification,
            None
        );
    }

    #[test]
    fn failed_atomic_write_does_not_replace_existing_target() {
        let dir = unique_temp_dir("atomic-write-failure");
        let path = dir.join("update-check.json");
        fs::create_dir_all(&path).expect("create directory at target path");

        let err = atomic_write_file(&path, b"{}").expect_err("rename over directory should fail");

        assert!(err.kind() != io::ErrorKind::NotFound);
        assert!(path.is_dir());

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }
}
