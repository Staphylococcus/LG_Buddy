// Host-observation layer: the raw facts the preflight judgment consumes.
// `FilesystemFacts` and its OS implementation (mountinfo decoding, statvfs
// read-only probes, systemd-manager observation), `InstalledLayout`, and
// the `HostPreflightFacts` snapshot. Moved verbatim from
// upgrade_preflight.rs; `user_systemd_path` is promoted to `pub(super)`
// for the parent coordinator.
use std::env;
use std::ffi::{CString, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFacts {
    pub kind: PathKind,
    pub owner_uid: u32,
    pub mode: u32,
    pub link_count: u64,
    pub read_only_filesystem: bool,
    pub mount_point: bool,
}

pub trait FilesystemFacts {
    fn path_facts(&self, path: &Path) -> io::Result<PathFacts>;
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn mount_points(&self) -> io::Result<Vec<PathBuf>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OsFilesystemFacts;

impl FilesystemFacts for OsFilesystemFacts {
    fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
        let metadata = fs::symlink_metadata(path)?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            PathKind::Symlink
        } else if file_type.is_file() {
            PathKind::File
        } else if file_type.is_dir() {
            PathKind::Directory
        } else {
            PathKind::Other
        };

        Ok(PathFacts {
            kind,
            owner_uid: metadata.uid(),
            mode: metadata.permissions().mode(),
            link_count: metadata.nlink(),
            read_only_filesystem: if matches!(kind, PathKind::File | PathKind::Directory) {
                filesystem_is_read_only(path)?
            } else {
                false
            },
            mount_point: if matches!(kind, PathKind::File | PathKind::Directory) {
                mounted_paths()?.iter().any(|mounted| mounted == path)
            } else {
                false
            },
        })
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries: Vec<_> = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        entries.sort();
        Ok(entries)
    }

    fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
        mounted_paths()
    }
}

fn filesystem_is_read_only(path: &Path) -> io::Result<bool> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(stat.f_flag & libc::ST_RDONLY as libc::c_ulong != 0)
}

fn mounted_paths() -> io::Result<Vec<PathBuf>> {
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    Ok(mountinfo
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.split(|byte| *byte == b' ').nth(4))
        .map(|field| PathBuf::from(OsString::from_vec(decode_mountinfo_field(field))))
        .collect())
}

fn decode_mountinfo_field(field: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\'
            && index + 3 < field.len()
            && field[index + 1..=index + 3]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = (field[index + 1] - b'0') * 64
                + (field[index + 2] - b'0') * 8
                + (field[index + 3] - b'0');
            decoded.push(value);
            index += 4;
        } else {
            decoded.push(field[index]);
            index += 1;
        }
    }
    decoded
}

/// What `systemctl is-system-running` observed. Pure observation: whether a
/// state counts as usable is decided by the preflight judgment, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemdManagerObservation {
    /// The probe ran and reported a manager state (possibly empty).
    Reported { state: String, stderr: String },
    /// systemctl could not be run at all.
    ProbeFailed(String),
    /// The probe was deliberately not run (sandboxed execution).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceManagerFacts {
    pub system: SystemdManagerObservation,
    pub user: SystemdManagerObservation,
}

impl ServiceManagerFacts {
    /// Record that no probe was run. Sandboxed execution cannot reach a
    /// system manager, so the judgment treats a skipped probe as passing.
    pub fn skipped() -> Self {
        Self {
            system: SystemdManagerObservation::Skipped,
            user: SystemdManagerObservation::Skipped,
        }
    }

    pub fn observe() -> Self {
        Self {
            system: observe_systemd(false),
            user: observe_systemd(true),
        }
    }
}

fn observe_systemd(user: bool) -> SystemdManagerObservation {
    let mut command = Command::new("systemctl");
    if user {
        command.arg("--user");
    }
    let output = match command.arg("is-system-running").output() {
        Ok(output) => output,
        Err(err) => {
            return SystemdManagerObservation::ProbeFailed(format!(
                "could not run systemctl: {err}"
            ));
        }
    };
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    SystemdManagerObservation::Reported { state, stderr }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledLayout {
    pub system_root: PathBuf,
    pub user_home: PathBuf,
    pub user_config_home: PathBuf,
}

impl InstalledLayout {
    pub fn new(
        system_root: impl Into<PathBuf>,
        user_home: impl Into<PathBuf>,
        xdg_config_home: Option<OsString>,
    ) -> Self {
        let user_home = user_home.into();
        let user_config_home = xdg_config_home
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| user_home.join(".config"));
        Self {
            system_root: system_root.into(),
            user_home,
            user_config_home,
        }
    }

    pub fn system_path(&self, path: &str) -> PathBuf {
        debug_assert!(path.starts_with('/'));
        if self.system_root == Path::new("/") {
            PathBuf::from(path)
        } else {
            self.system_root.join(path.trim_start_matches('/'))
        }
    }

    pub fn installed_executable(&self) -> PathBuf {
        self.system_path("/usr/bin/lg-buddy")
    }

    pub fn config_pointer(&self) -> PathBuf {
        self.system_path("/usr/lib/lg-buddy/config-path")
    }

    pub(super) fn user_systemd_path(&self, path: &str) -> PathBuf {
        self.user_config_home.join("systemd/user").join(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPreflightFacts {
    pub layout: InstalledLayout,
    pub running_executable: PathBuf,
    pub effective_uid: u32,
    pub system_owner_uid: u32,
    pub user_owner_uid: u32,
    pub service_managers: ServiceManagerFacts,
}

/// What the process observation could not see. The observation reports the
/// missing fact; deciding how to refuse (check name, message, remedy) is the
/// judgment layer's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationFailure {
    RunningExecutable(String),
    UserHome,
}

pub(super) fn observe_process() -> Result<HostPreflightFacts, ObservationFailure> {
    let running_executable = match env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return Err(ObservationFailure::RunningExecutable(format!(
                "could not resolve the running executable: {err}"
            )));
        }
    };
    let user_home = match env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => return Err(ObservationFailure::UserHome),
    };
    let effective_uid = unsafe { libc::geteuid() };
    let install_root = env::var_os("LG_BUDDY_INSTALL_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from);
    let sandboxed_install = install_root.is_some();
    let system_root = install_root.unwrap_or_else(|| PathBuf::from("/"));
    let service_managers =
        if sandboxed_install && env::var("LG_BUDDY_SKIP_SYSTEMD_ACTIONS").as_deref() == Ok("1") {
            ServiceManagerFacts::skipped()
        } else {
            ServiceManagerFacts::observe()
        };
    Ok(HostPreflightFacts {
        layout: InstalledLayout::new(system_root, user_home, env::var_os("XDG_CONFIG_HOME")),
        running_executable,
        effective_uid,
        system_owner_uid: if sandboxed_install { effective_uid } else { 0 },
        user_owner_uid: effective_uid,
        service_managers,
    })
}

mod tests {
    use super::{decode_mountinfo_field, FilesystemFacts, InstalledLayout, OsFilesystemFacts};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn os_filesystem_identifies_mount_points() {
        let root = OsFilesystemFacts.path_facts(Path::new("/")).unwrap();

        assert!(root.mount_point);
        assert_eq!(
            decode_mountinfo_field(br"/tmp/lg\040buddy\134config"),
            br"/tmp/lg buddy\config"
        );
    }

    #[test]
    fn decode_mountinfo_field_preserves_literal_backslashes() {
        // No escape sequences, and an empty field, pass through unchanged.
        assert_eq!(decode_mountinfo_field(b"/etc/hostname"), b"/etc/hostname");
        assert_eq!(decode_mountinfo_field(b""), b"");
        // A backslash not followed by three octal digits is kept literally.
        assert_eq!(decode_mountinfo_field(br"a\bx"), br"a\bx");
        // A complete escape at the very end still decodes.
        assert_eq!(decode_mountinfo_field(br"a\040"), b"a ");
    }

    #[test]
    fn installed_layout_xdg_config_home_falls_back_to_user_config() {
        // No XDG config home -> <home>/.config.
        let layout = InstalledLayout::new("/", "/home/user", None);
        assert_eq!(layout.user_config_home, PathBuf::from("/home/user/.config"));

        // An absolute XDG config home is used verbatim.
        let layout = InstalledLayout::new("/", "/home/user", Some(OsString::from("/cfg")));
        assert_eq!(layout.user_config_home, PathBuf::from("/cfg"));

        // A relative XDG config home is rejected and falls back.
        let layout = InstalledLayout::new("/", "/home/user", Some(OsString::from("rel")));
        assert_eq!(layout.user_config_home, PathBuf::from("/home/user/.config"));
    }

    #[test]
    fn installed_layout_system_path_resolves_against_root() {
        // Root install (system_root == "/") -> path used as-is.
        let layout = InstalledLayout::new("/", "/home/user", None);
        assert_eq!(
            layout.system_path("/usr/bin/x"),
            PathBuf::from("/usr/bin/x")
        );
        assert_eq!(
            layout.installed_executable(),
            PathBuf::from("/usr/bin/lg-buddy")
        );

        // Custom system root -> leading slash stripped and joined under it.
        let layout = InstalledLayout::new("/opt", "/home/user", None);
        assert_eq!(
            layout.system_path("/usr/bin/x"),
            PathBuf::from("/opt/usr/bin/x")
        );
        assert_eq!(
            layout.installed_executable(),
            PathBuf::from("/opt/usr/bin/lg-buddy")
        );
        assert_eq!(
            layout.config_pointer(),
            PathBuf::from("/opt/usr/lib/lg-buddy/config-path")
        );
    }

    #[test]
    fn installed_layout_user_systemd_path_joins_under_config() {
        let layout = InstalledLayout::new("/", "/home/user", None);
        assert_eq!(
            layout.user_systemd_path("LG_Buddy_screen.service"),
            PathBuf::from("/home/user/.config/systemd/user/LG_Buddy_screen.service")
        );
    }
}
