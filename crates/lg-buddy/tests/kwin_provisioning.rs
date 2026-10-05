//! Installed native worker and authorization-owner interoperability, with only
//! the privileged executables mocked. No compositor or system files are changed.
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-kwin-worker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("tools")).unwrap();
        fs::create_dir(root.join("payload")).unwrap();
        fs::create_dir_all(root.join("state/lg-buddy/kwin/plugins")).unwrap();
        // The worker must work with neither flock nor shell filesystem tools.
        // Only the authorization owner's existing utilities are supplied.
        for name in ["dirname", "mktemp", "cat", "rm", "sleep"] {
            std::os::unix::fs::symlink(executable(name), root.join("tools").join(name)).unwrap();
        }
        fs::write(
            root.join("payload/setup.sh"),
            include_str!("../../../data/kwin/setup.sh").replace(
                "runtime=/usr/bin/lg-buddy",
                &format!("runtime='{}'", env!("CARGO_BIN_EXE_lg-buddy")),
            ),
        )
        .unwrap();
        let fixture = Self(root);
        fixture.script("sudo", "exit 1\n");
        fixture.script(
            "pkcheck",
            r#"
[ "$1" = --action-id ] && [ "$2" = io.github.staphylococcus.LGBuddy.setup ] || exit 1
printf '%s\n' "$4" >> "$root/subjects"
printf '%s' "${4%%,*}" > "$root/grant"
"#,
        );
        fixture.script(
            "pkexec",
            r#"
[ "$1" = --disable-internal-agent ] || exit 127
[ "$(cat "$root/grant")" = "$PPID" ] || exit 127
shift
printf '%s\0' "$@" >> "$root/arguments"
printf ready > "$root/ready"
while [ -f "$root/stall" ] && [ ! -f "$root/release" ]; do sleep 0.01; done
"#,
        );
        fixture.receipt();
        fixture
    }
    fn receipt(&self) {
        let id = format!(
            "lg_buddy_inhibition_{}_{}",
            unsafe { libc::getuid() },
            "a".repeat(64)
        );
        fs::write(
            self.0
                .join("state/lg-buddy/kwin/plugins")
                .join(format!("{id}.tsv")),
            format!("/usr/lib64/qt6/plugins\t{id}\n"),
        )
        .unwrap();
    }
    fn script(&self, name: &str, body: &str) {
        let path = self.0.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\nroot='{}'\n{body}", self.0.display()),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn owner(&self) -> Command {
        self.owner_mode("--remove")
    }
    fn owner_mode(&self, mode: &str) -> Command {
        let runner = include_str!("../src/setup/authorization.sh");
        let functions = runner.split("\nwhile IFS=").next().unwrap();
        let script =
            format!("{functions}\n_lg_buddy_cancellable=0\n_lg_buddy_kwin \"$2\" \"$3\" \"$4\"\n")
                .replace("if [ \"$EUID\" -eq 0 ]; then", "if false; then")
                .replace("/usr/bin/pkcheck", self.0.join("pkcheck").to_str().unwrap())
                .replace("/usr/bin/pkexec", self.0.join("pkexec").to_str().unwrap())
                .replace("/usr/bin/sudo", self.0.join("sudo").to_str().unwrap());
        let mut command = Command::new(executable("bash"));
        command
            .args([
                "-c",
                &script,
                "authorization-owner",
                "interactive",
                env!("CARGO_BIN_EXE_lg-buddy"),
            ])
            .arg(self.0.join("payload"))
            .arg(mode);
        self.environment(&mut command);
        command
    }
    fn environment(&self, command: &mut Command) {
        command
            .env("PATH", self.0.join("tools"))
            .env("HOME", &self.0)
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/lg-buddy-test-bus",
            );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
        .unwrap()
}
fn wait(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "worker did not reach {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
struct Owner(Child);
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

#[test]
fn fast_read_only_workers_return_their_status_without_losing_coprocess_descriptors() {
    let fixture = Fixture::new();
    let expected = if unsafe { libc::getuid() } == 0 { 2 } else { 1 };
    for _ in 0..30 {
        let output = fixture.owner_mode("--status").output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!fixture.0.join("subjects").exists());
    assert!(!fixture.0.join("state/lg-buddy/kwin/setup.lock").exists());
}

#[test]
fn native_removal_brokers_permission_through_the_original_subject_without_flock() {
    let fixture = Fixture::new();
    let output = fixture.owner().output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_dir(fixture.0.join("state/lg-buddy/kwin/plugins"))
            .unwrap()
            .count(),
        0
    );
    assert!(!fixture.0.join("tools/flock").exists());
    let arguments = fs::read(fixture.0.join("arguments")).unwrap();
    assert!(arguments
        .split(|b| *b == 0)
        .any(|arg| arg == b"--system-remove"));
    assert_eq!(
        fs::read_to_string(fixture.0.join("subjects"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn native_worker_keeps_removal_excluded_after_the_frontend_disconnects() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("stall"), "").unwrap();
    let mut owner = Owner(
        fixture
            .owner()
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(&fixture.0.join("ready"));
    drop(owner.0.stdin.take());
    drop(owner.0.stdout.take());
    let mut competitor = Command::new(env!("CARGO_BIN_EXE_lg-buddy"));
    competitor.args(["kwin-setup", "--remove"]);
    fixture.environment(&mut competitor);
    let output = competitor.output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Resource temporarily unavailable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(fixture.0.join("release"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = competitor.output().unwrap();
        if !String::from_utf8_lossy(&output.stderr).contains("Resource temporarily unavailable") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native worker retained the lock after its privileged action finished"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = owner.0.wait();
}
