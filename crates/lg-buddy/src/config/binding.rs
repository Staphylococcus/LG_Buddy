//! Config discovery records contain a path only. Settings and credentials stay put.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const MACHINE_BINDING: &str = "/etc/lg-buddy/config-path";
pub(crate) const LEGACY_BINDING: &str = "/usr/lib/lg-buddy/config-path";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn absolute_path(path: &Path) -> io::Result<()> {
    let normalized: PathBuf = path.components().collect();
    if !path.is_absolute()
        || normalized.as_os_str() != path.as_os_str()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path
            .to_str()
            .is_none_or(|value| value.contains(['\n', '\r', '\0']))
        || path
            .as_os_str()
            .as_encoded_bytes()
            .windows(2)
            .any(|part| part == b"//")
    {
        return Err(invalid(format!(
            "expected a canonical absolute config path: {}",
            path.display()
        )));
    }
    Ok(())
}

// Inspect links themselves, including dangling links and redirected parents.
pub(crate) fn safe_destination(path: &Path) -> io::Result<()> {
    absolute_path(path)?;
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid(format!(
                    "refusing a symlink in config binding path: {}",
                    ancestor.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) fn read(path: &Path, owner: u32) -> io::Result<Option<PathBuf>> {
    // An absent user selector must allow normal default config discovery,
    // including when the user's config directory itself is a symlink.
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    safe_destination(path)?;
    if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(invalid(format!("config binding must be a regular file owned by uid {owner}, without group/other writes: {}", path.display())));
    }
    parse(&fs::read_to_string(path)?).map(Some)
}

pub(crate) fn parse(contents: &str) -> io::Result<PathBuf> {
    if contents.contains(['\r', '\0']) {
        return Err(invalid("config binding contains a carriage return or NUL"));
    }
    let mut lines = contents.lines().filter(|line| !line.is_empty());
    let target = PathBuf::from(
        lines
            .next()
            .ok_or_else(|| invalid("config binding is empty"))?,
    );
    if lines.next().is_some() {
        return Err(invalid("config binding must contain exactly one path"));
    }
    absolute_path(&target)?;
    Ok(target)
}

fn validate_target(config: &Path, owner: Option<u32>) -> io::Result<()> {
    absolute_path(config)?;
    let metadata = fs::symlink_metadata(config)?;
    if !metadata.is_file() || fs::canonicalize(config)? != config {
        return Err(invalid("bound config must be a canonical regular file"));
    }
    if owner.is_some_and(|owner| metadata.uid() != owner) {
        return Err(invalid(
            "bound config belongs to another user; run installation as its owner",
        ));
    }
    Ok(())
}

fn system_path(root: &Path, path: &str) -> PathBuf {
    root.join(path.trim_start_matches('/'))
}

fn system_owner(root: &Path) -> u32 {
    if root == Path::new("/") {
        0
    } else {
        unsafe { libc::geteuid() }
    }
}

/// Only installers use the old location, and only while the new one is absent.
pub(crate) fn installed(root: &Path) -> io::Result<Option<PathBuf>> {
    let owner = system_owner(root);
    validate_destination(&system_path(root, MACHINE_BINDING), owner, root)?;
    match read(&system_path(root, MACHINE_BINDING), owner)? {
        Some(config) => Ok(Some(config)),
        None => {
            validate_destination(&system_path(root, LEGACY_BINDING), owner, root)?;
            read(&system_path(root, LEGACY_BINDING), owner)
        }
    }
}

pub(super) fn machine_path() -> io::Result<PathBuf> {
    validate_destination(Path::new(MACHINE_BINDING), 0, Path::new("/"))?;
    let config = read(Path::new(MACHINE_BINDING), 0)?.ok_or_else(|| {
        invalid(format!(
            "machine config binding is missing: {MACHINE_BINDING}; complete LG Buddy service setup"
        ))
    })?;
    validate_target(&config, None)?;
    Ok(config)
}

pub(crate) fn validate_destination(path: &Path, owner: u32, boundary: &Path) -> io::Result<()> {
    safe_destination(path)?;
    // Every existing directory inside the selected trust boundary must be owned
    // by the writer and must not allow another user to redirect the rename.
    for ancestor in path
        .ancestors()
        .skip(1)
        .take_while(|p| p.starts_with(boundary))
    {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata)
                if !metadata.is_dir()
                    || metadata.uid() != owner
                    || metadata.mode() & 0o022 != 0 =>
            {
                return Err(invalid(format!(
                    "unsafe config binding directory: {}",
                    ancestor.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
            return Err(invalid(format!(
                "unsafe config binding file: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn write(path: &Path, config: &Path, owner: u32, boundary: &Path) -> io::Result<()> {
    validate_destination(path, owner, boundary)?;
    if let Some(existing) = read(path, owner)? {
        if existing == config && fs::metadata(path)?.mode() & 0o7777 == 0o644 {
            return Ok(());
        }
    }
    ensure_parent(path)?;
    let parent = path.parent().unwrap();
    validate_destination(path, owner, boundary)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temporary, mut output) = loop {
        let temporary = parent.join(format!(
            ".config-path-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)
        {
            Ok(output) => break (temporary, output),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        writeln!(output, "{}", config.display())?;
        output.set_permissions(fs::Permissions::from_mode(0o644))?;
        output.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn ensure_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("config binding has no parent"))?;
    let missing: Vec<_> = parent
        .ancestors()
        .take_while(|path| !path.exists())
        .collect();
    for directory in missing.into_iter().rev() {
        fs::DirBuilder::new().mode(0o755).create(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
        fs::File::open(directory)?.sync_all()?;
        fs::File::open(directory.parent().unwrap())?.sync_all()?;
    }
    Ok(())
}

/// Preserve a custom config for later launches by this user, without consulting
/// or changing the machine binding. Called only by explicit setup/installation.
pub(crate) fn remember(config: &Path, record: &Path, replace: bool) -> io::Result<()> {
    let owner = unsafe { libc::geteuid() };
    validate_target(config, Some(owner))?;
    if !replace && read(record, owner)?.is_some_and(|existing| existing != config) {
        return Err(invalid("user config selection conflicts with the installed binding; resolve it before reinstalling"));
    }
    write(record, config, owner, record.parent().unwrap())
}

fn migrate(root: &Path, config: &Path) -> io::Result<()> {
    let owner = system_owner(root);
    if unsafe { libc::geteuid() } != owner {
        return Err(invalid(
            "machine binding installation requires root authorization",
        ));
    }
    validate_target(config, None)?;
    if installed(root)?.is_some_and(|existing| existing != config) {
        return Err(invalid(
            "selected config conflicts with the installed machine binding",
        ));
    }
    let path = system_path(root, MACHINE_BINDING);
    // Missing package ownership information is never permission to overwrite.
    if root == Path::new("/")
        && crate::upgrade_preflight::PackageDatabase::system()
            .owner_of(&path)
            .map_err(|error| invalid(error.to_string()))?
            .blocks_mutation()
    {
        return Err(invalid(
            "refusing to replace a package-owned machine binding",
        ));
    }
    write(&path, config, owner, root)
}

/// Private installer/activation protocol. Only legacy installer operations accept
/// an isolated root; declarative machine publication always uses the real /etc.
pub fn run_installer_command(arguments: &[String]) -> io::Result<()> {
    match arguments {
        [command, ..]
            if matches!(
                command.as_str(),
                "publish-user" | "publish-machine" | "refresh-user"
            ) =>
        {
            super::activation::run(arguments)
        }
        [command, root] if command == "inspect" => {
            if let Some(config) = installed(Path::new(root))? {
                validate_target(
                    &config,
                    (unsafe { libc::geteuid() } != 0).then(|| unsafe { libc::geteuid() }),
                )?;
                if std::env::var_os("LG_BUDDY_CONFIG")
                    .is_some_and(|selected| Path::new(&selected) != config)
                {
                    return Err(invalid(
                        "LG_BUDDY_CONFIG conflicts with the installed machine binding",
                    ));
                }
                println!("{}", config.display());
            }
            Ok(())
        }
        [command, root, config] if command == "migrate" => {
            migrate(Path::new(root), Path::new(config))
        }
        [command, config] if command == "remember" => {
            let record = super::user_binding_path_from_env()
                .ok_or_else(|| invalid("user config home is unavailable"))?;
            remember(Path::new(config), &record, false)
        }
        _ => Err(invalid("invalid config-binding installer command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "lg-buddy-binding-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn config(&self) -> PathBuf {
            let path = self.0.join("custom/config.env");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                "tvs_primary_platform=lg_webos\nupdates_channel=prerelease\n",
            )
            .unwrap();
            path
        }
        fn seed(&self, relative: &str, contents: &str) -> PathBuf {
            let path = system_path(&self.0, relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn binding_parser_accepts_literal_paths_and_rejects_ambiguous_records() {
        assert_eq!(
            parse("/a b/%quoted.env\n").unwrap(),
            Path::new("/a b/%quoted.env")
        );
        for contents in [
            "",
            "relative.env",
            "/a/../b",
            "/a/./b",
            "/a//b",
            "/a\n/b",
            "/a\r\n",
            "/a\0",
        ] {
            assert!(parse(contents).is_err(), "accepted {contents:?}");
        }
    }

    #[test]
    fn legacy_custom_binding_migrates_once_and_leaves_config_and_recovery_input_intact() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let contents = fs::read(&config).unwrap();
        let old = fixture.seed(LEGACY_BINDING, &format!("{}\n", config.display()));
        let original = fs::read(&old).unwrap();
        assert_eq!(installed(&fixture.0).unwrap(), Some(config.clone()));
        migrate(&fixture.0, &config).unwrap();
        let new = system_path(&fixture.0, MACHINE_BINDING);
        assert_eq!(fs::read(&new).unwrap(), original);
        assert_eq!(fs::read(&old).unwrap(), original);
        assert_eq!(fs::read(&config).unwrap(), contents);
        let inode = fs::metadata(&new).unwrap().ino();
        // Retry after interruption: valid new state overrides stale legacy input.
        fs::write(&old, "/stale/config.env\n").unwrap();
        migrate(&fixture.0, &config).unwrap();
        assert_eq!(fs::metadata(&new).unwrap().ino(), inode);
        assert_eq!(installed(&fixture.0).unwrap(), Some(config));
    }

    #[test]
    fn invalid_new_binding_never_falls_back_to_valid_legacy_input() {
        let fixture = Fixture::new();
        let config = fixture.config();
        fixture.seed(LEGACY_BINDING, &format!("{}\n", config.display()));
        let new = fixture.seed(MACHINE_BINDING, "relative/config.env\n");
        assert!(installed(&fixture.0).is_err());
        assert!(migrate(&fixture.0, &config).is_err());
        assert_eq!(fs::read_to_string(new).unwrap(), "relative/config.env\n");
    }

    #[test]
    fn conflicting_config_does_not_rebind_or_change_any_input() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let pointer = fixture.seed(MACHINE_BINDING, "/another/config.env\n");
        assert!(migrate(&fixture.0, &config).is_err());
        assert_eq!(
            fs::read_to_string(pointer).unwrap(),
            "/another/config.env\n"
        );
    }

    #[test]
    fn binding_migration_refuses_file_and_parent_symlinks_before_writing() {
        for parent_link in [false, true] {
            let fixture = Fixture::new();
            let config = fixture.config();
            fixture.seed(LEGACY_BINDING, &format!("{}\n", config.display()));
            let external = fixture.0.join("external");
            fs::create_dir(&external).unwrap();
            let sentinel = external.join("config-path");
            fs::write(&sentinel, "unchanged\n").unwrap();
            let pointer = system_path(&fixture.0, MACHINE_BINDING);
            if parent_link {
                fs::create_dir_all(pointer.parent().unwrap().parent().unwrap()).unwrap();
                symlink(&external, pointer.parent().unwrap()).unwrap();
            } else {
                fs::create_dir_all(pointer.parent().unwrap()).unwrap();
                symlink(&sentinel, &pointer).unwrap();
            }
            assert!(migrate(&fixture.0, &config).is_err());
            assert_eq!(fs::read_to_string(&sentinel).unwrap(), "unchanged\n");
        }
    }

    #[test]
    fn missing_machine_binding_below_a_symlink_never_falls_back_or_writes() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let legacy = fixture.seed(LEGACY_BINDING, &format!("{}\n", config.display()));
        let original = fs::read(&legacy).unwrap();
        let external = fixture.0.join("external");
        fs::create_dir(&external).unwrap();
        let pointer = system_path(&fixture.0, MACHINE_BINDING);
        fs::create_dir_all(pointer.parent().unwrap().parent().unwrap()).unwrap();
        symlink(&external, pointer.parent().unwrap()).unwrap();
        assert!(installed(&fixture.0).is_err());
        assert!(migrate(&fixture.0, &config).is_err());
        assert_eq!(fs::read(&legacy).unwrap(), original);
        assert!(!external.join("config-path").exists());
    }

    #[test]
    fn migration_refuses_unsafe_directory_without_creating_the_binding() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let directory = fixture.0.join("etc");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(migrate(&fixture.0, &config).is_err());
        assert!(!directory.join("lg-buddy").exists());
    }

    #[test]
    fn user_custom_selection_is_explicit_and_conflicting_selection_is_preserved() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let record = fixture.0.join("xdg/lg-buddy/config-path");
        remember(&config, &record, false).unwrap();
        assert_eq!(
            read(&record, unsafe { libc::geteuid() }).unwrap(),
            Some(config.clone())
        );
        let other = fixture.seed("other/config.env", "screen_idle_blank=disabled\n");
        assert!(remember(&other, &record, false).is_err());
        assert_eq!(
            fs::read_to_string(&record).unwrap(),
            format!("{}\n", config.display())
        );
        remember(&other, &record, true).unwrap();
        assert_eq!(
            fs::read_to_string(&record).unwrap(),
            format!("{}\n", other.display())
        );
    }
}
