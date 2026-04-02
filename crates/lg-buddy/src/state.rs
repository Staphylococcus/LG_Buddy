use std::env;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const STATE_DIR_NAME: &str = "lg_buddy";
pub const SCREEN_OFF_BY_US_MARKER: &str = "screen_off_by_us";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateScope {
    System,
    Session,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeDirSources<'a> {
    pub system_override: Option<&'a Path>,
    pub session_override: Option<&'a Path>,
    pub xdg_runtime_dir: Option<&'a Path>,
    pub uid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateDirError {
    SessionRuntimeUnavailable,
}

impl fmt::Display for StateDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionRuntimeUnavailable => {
                write!(
                    f,
                    "could not resolve a session runtime directory from override, XDG_RUNTIME_DIR, or uid"
                )
            }
        }
    }
}

impl Error for StateDirError {}

pub fn resolve_state_dir(
    scope: StateScope,
    sources: RuntimeDirSources<'_>,
) -> Result<PathBuf, StateDirError> {
    match scope {
        StateScope::System => Ok(sources
            .system_override
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/run").join(STATE_DIR_NAME))),
        StateScope::Session => {
            if let Some(path) = sources.session_override {
                return Ok(path.to_path_buf());
            }

            if let Some(path) = sources.xdg_runtime_dir {
                return Ok(path.join(STATE_DIR_NAME));
            }

            if let Some(uid) = sources.uid {
                return Ok(PathBuf::from("/run/user")
                    .join(uid.to_string())
                    .join(STATE_DIR_NAME));
            }

            Err(StateDirError::SessionRuntimeUnavailable)
        }
    }
}

pub fn resolve_state_dir_from_env(scope: StateScope) -> Result<PathBuf, StateDirError> {
    let system_override = env::var_os("LG_BUDDY_SYSTEM_RUNTIME_DIR").map(PathBuf::from);
    let session_override = env::var_os("LG_BUDDY_SESSION_RUNTIME_DIR").map(PathBuf::from);
    let xdg_runtime_dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);

    resolve_state_dir(
        scope,
        RuntimeDirSources {
            system_override: system_override.as_deref(),
            session_override: session_override.as_deref(),
            xdg_runtime_dir: xdg_runtime_dir.as_deref(),
            uid: current_uid(),
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenOwnershipMarker {
    path: PathBuf,
}

impl ScreenOwnershipMarker {
    pub fn for_scope(
        scope: StateScope,
        sources: RuntimeDirSources<'_>,
    ) -> Result<Self, StateDirError> {
        let state_dir = resolve_state_dir(scope, sources)?;
        Ok(Self::new(state_dir))
    }

    pub fn from_env(scope: StateScope) -> Result<Self, StateDirError> {
        let state_dir = resolve_state_dir_from_env(scope)?;
        Ok(Self::new(state_dir))
    }

    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            path: state_dir.join(SCREEN_OFF_BY_US_MARKER),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state_dir(&self) -> &Path {
        self.path
            .parent()
            .expect("screen ownership marker should always have a parent directory")
    }

    pub fn create(&self) -> io::Result<()> {
        fs::create_dir_all(self.state_dir())?;
        create_marker_file(&self.path)
    }

    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    pub fn is_stale(&self, max_age: Duration, now: SystemTime) -> io::Result<bool> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        };

        let modified = metadata.modified()?;
        let Ok(age) = now.duration_since(modified) else {
            return Ok(false);
        };

        Ok(age > max_age)
    }
}

#[cfg(unix)]
fn create_marker_file(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map(|_| ())
}

#[cfg(not(unix))]
fn create_marker_file(path: &Path) -> io::Result<()> {
    fs::write(path, [])
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }

    Some(unsafe { geteuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_state_dir, RuntimeDirSources, ScreenOwnershipMarker, StateDirError, StateScope,
        SCREEN_OFF_BY_US_MARKER,
    };
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn system_scope_uses_default_path() {
        let path = resolve_state_dir(StateScope::System, RuntimeDirSources::default())
            .expect("resolve system runtime directory");

        assert_eq!(path, PathBuf::from("/run/lg_buddy"));
    }

    #[test]
    fn system_scope_respects_override() {
        let path = resolve_state_dir(
            StateScope::System,
            RuntimeDirSources {
                system_override: Some(Path::new("/tmp/lg-buddy-system")),
                ..RuntimeDirSources::default()
            },
        )
        .expect("resolve overridden system runtime directory");

        assert_eq!(path, PathBuf::from("/tmp/lg-buddy-system"));
    }

    #[test]
    fn session_scope_prefers_explicit_override() {
        let path = resolve_state_dir(
            StateScope::Session,
            RuntimeDirSources {
                session_override: Some(Path::new("/tmp/lg-buddy-session")),
                xdg_runtime_dir: Some(Path::new("/tmp/xdg-runtime")),
                uid: Some(1000),
                ..RuntimeDirSources::default()
            },
        )
        .expect("resolve overridden session runtime directory");

        assert_eq!(path, PathBuf::from("/tmp/lg-buddy-session"));
    }

    #[test]
    fn session_scope_uses_xdg_runtime_dir() {
        let path = resolve_state_dir(
            StateScope::Session,
            RuntimeDirSources {
                xdg_runtime_dir: Some(Path::new("/tmp/xdg-runtime")),
                uid: Some(1000),
                ..RuntimeDirSources::default()
            },
        )
        .expect("resolve session runtime directory from xdg");

        assert_eq!(path, PathBuf::from("/tmp/xdg-runtime/lg_buddy"));
    }

    #[test]
    fn session_scope_falls_back_to_uid() {
        let path = resolve_state_dir(
            StateScope::Session,
            RuntimeDirSources {
                uid: Some(1000),
                ..RuntimeDirSources::default()
            },
        )
        .expect("resolve session runtime directory from uid");

        assert_eq!(path, PathBuf::from("/run/user/1000/lg_buddy"));
    }

    #[test]
    fn session_scope_requires_override_xdg_or_uid() {
        let err = resolve_state_dir(StateScope::Session, RuntimeDirSources::default())
            .expect_err("session path without inputs should fail");

        assert_eq!(err, StateDirError::SessionRuntimeUnavailable);
    }

    #[test]
    fn marker_create_and_clear_manage_file() {
        let temp_dir = TestDir::new("marker-create-clear");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());

        assert!(!marker.exists());

        marker.create().expect("create marker");
        assert!(marker.exists());
        assert_eq!(
            marker.path(),
            temp_dir.path().join(SCREEN_OFF_BY_US_MARKER).as_path()
        );

        marker.clear().expect("clear marker");
        assert!(!marker.exists());
    }

    #[test]
    fn marker_clear_ignores_missing_file() {
        let temp_dir = TestDir::new("marker-clear-missing");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());

        marker.clear().expect("clear missing marker");
        assert!(!marker.exists());
    }

    #[test]
    fn marker_is_not_stale_when_missing() {
        let temp_dir = TestDir::new("marker-missing-not-stale");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());

        let stale = marker
            .is_stale(Duration::from_secs(60), SystemTime::now())
            .expect("check missing marker staleness");

        assert!(!stale);
    }

    #[test]
    fn marker_staleness_is_deterministic_from_supplied_time() {
        let temp_dir = TestDir::new("marker-stale");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");

        let modified = fs::metadata(marker.path())
            .expect("marker metadata")
            .modified()
            .expect("marker modified time");

        let stale = marker
            .is_stale(Duration::from_secs(60), modified + Duration::from_secs(61))
            .expect("check stale marker");
        let fresh = marker
            .is_stale(Duration::from_secs(60), modified + Duration::from_secs(60))
            .expect("check fresh marker");

        assert!(stale);
        assert!(!fresh);
    }

    #[cfg(unix)]
    #[test]
    fn marker_create_rejects_symlink_targets() {
        let temp_dir = TestDir::new("marker-symlink");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let target = temp_dir.path().join("target");
        fs::write(&target, b"sentinel").expect("write target");
        symlink(&target, marker.path()).expect("create symlink marker");

        let err = marker
            .create()
            .expect_err("symlink marker should be rejected");

        assert!(matches!(
            err.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::EEXIST)
        ));
        assert_eq!(fs::read(&target).expect("read target"), b"sentinel");
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);

            let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-{label}-{}-{timestamp}-{unique}",
                process::id()
            ));

            fs::create_dir_all(&path).expect("create test temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
