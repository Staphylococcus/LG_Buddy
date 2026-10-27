//! Declarative activation uses the same discovery, readers and writer lock as the app.
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-activation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> PathBuf {
        self.0.join("custom state/config.env")
    }
    fn selector(&self) -> PathBuf {
        self.0.join("xdg/lg-buddy/config-path")
    }
    fn overlay(&self) -> PathBuf {
        self.config().with_file_name("config.env.declarative.json")
    }
    fn generation(&self, timeout: u64) -> PathBuf {
        let path = self.0.join(format!("generation-{timeout}.json"));
        fs::write(
            &path,
            format!(
                r#"{{"version":1,"source":"nix","settings":{{"screen.idle_timeout":{timeout}}}}}"#
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        path
    }
    fn run(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lg-buddy"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("LG_BUDDY_")
                || key == "SUDO_USER"
                || key == "SUDO_UID"
            {
                command.env_remove(key);
            }
        }
        command
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1")
            .args(arguments)
            .output()
            .unwrap()
    }
    fn publish(&self, generation: &str) -> Output {
        self.run(&[
            "config-binding",
            "publish-user",
            self.config().to_str().unwrap(),
            generation,
        ])
    }
    fn timeout(&self) -> u64 {
        let result = self.run(&["settings", "get", "screen.idle_timeout"]);
        assert!(result.status.success(), "{result:?}");
        String::from_utf8(result.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn activation_before_pairing_then_replacement_rollback_and_removal_preserve_user_state() {
    let f = Fixture::new();
    let first = f.generation(300);
    let result = f.publish(first.to_str().unwrap());
    assert!(result.status.success(), "{result:?}");
    assert!(!f.config().exists());
    assert!(!f.config().parent().unwrap().join("tvs").exists());
    assert!(fs::symlink_metadata(f.selector()).unwrap().is_file());
    assert_eq!(
        fs::read_to_string(f.selector()).unwrap().trim(),
        f.config().to_str().unwrap()
    );
    assert_eq!(f.timeout(), 300);
    let selector_inode = fs::metadata(f.selector()).unwrap().ino();
    let lock = f.config().with_file_name(".config.env.pairing.lock");
    let lock_inode = fs::metadata(&lock).unwrap().ino();

    let saved = b"screen_idle_timeout=600\nupdates_channel=stable\n";
    fs::write(f.config(), saved).unwrap();
    let token = f
        .config()
        .parent()
        .unwrap()
        .join("tvs/primary/access-token.json");
    fs::create_dir_all(token.parent().unwrap()).unwrap();
    fs::write(&token, b"private credential sentinel").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let token_before = fs::metadata(&token).unwrap();
    let second = f.generation(450);
    for (generation, expected) in [(&second, 450), (&first, 300), (&first, 300)] {
        assert!(f.publish(generation.to_str().unwrap()).status.success());
        assert_eq!(f.timeout(), expected);
        assert!(!f
            .run(&["settings", "set", "screen.idle_timeout", "900"])
            .status
            .success());
        assert_eq!(fs::read(f.config()).unwrap(), saved);
        assert_eq!(fs::read(&token).unwrap(), b"private credential sentinel");
        assert_eq!(
            (
                fs::metadata(&token).unwrap().mtime(),
                fs::metadata(&token).unwrap().mtime_nsec()
            ),
            (token_before.mtime(), token_before.mtime_nsec())
        );
        assert_eq!(fs::metadata(&lock).unwrap().ino(), lock_inode);
        assert_eq!(fs::metadata(f.selector()).unwrap().ino(), selector_inode);
    }
    assert!(f.publish("-").status.success());
    assert!(!f.overlay().exists());
    assert_eq!(f.timeout(), 600);
    assert!(f
        .run(&["settings", "set", "screen.idle_timeout", "900"])
        .status
        .success());
    assert_eq!(f.timeout(), 900);
    assert!(f.publish("-").status.success());
    assert_eq!(fs::metadata(lock).unwrap().ino(), lock_inode);
}

#[test]
fn publication_refuses_a_busy_writer_and_invalid_generation_without_replacing_policy() {
    let f = Fixture::new();
    let first = f.generation(300);
    assert!(f.publish(first.to_str().unwrap()).status.success());
    let lock_path = f.config().with_file_name(".config.env.pairing.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let second = f.generation(450);
    assert!(!f.publish(second.to_str().unwrap()).status.success());
    assert_eq!(f.timeout(), 300);
    drop(lock);
    fs::set_permissions(&second, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&second, b"invalid generation").unwrap();
    fs::set_permissions(&second, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(!f.publish(second.to_str().unwrap()).status.success());
    assert_eq!(fs::read_link(f.overlay()).unwrap(), first);
    assert_eq!(f.timeout(), 300);
    assert!(!f.config().exists());
}

#[test]
fn publication_refuses_symlinked_selectors_and_conflicting_user_selection() {
    for redirected in [true, false] {
        let f = Fixture::new();
        fs::create_dir_all(f.selector().parent().unwrap()).unwrap();
        let other = f.0.join("other-selection");
        fs::write(&other, "/other/config.env\n").unwrap();
        if redirected {
            symlink(&other, f.selector()).unwrap();
        } else {
            fs::write(f.selector(), "/other/config.env\n").unwrap();
        }
        let generation = f.generation(300);
        assert!(!f.publish(generation.to_str().unwrap()).status.success());
        assert_eq!(fs::read(&other).unwrap(), b"/other/config.env\n");
        assert!(!f.overlay().exists());
        assert!(!f.config().exists());
    }
}
