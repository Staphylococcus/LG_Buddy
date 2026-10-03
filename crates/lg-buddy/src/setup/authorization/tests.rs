use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-authorization-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self(root);
        fixture.script(
            "pkcheck",
            r#"
[ "$1" = --action-id ] && [ "$2" = io.github.staphylococcus.LGBuddy.setup ] || exit 127
[ "$3" = --process ] && [ "$5" = --allow-user-interaction ] || exit 127
subject="${4%%,*}"
printf '%s\n' "$4" >> "$root/checks"
if [ -f "$root/denied" ]; then
    [ ! -f "$root/sudo-after-denial" ] || printf grant > "$root/sudo-grant"
    exit "$(cat "$root/denied")"
fi
if [ ! -f "$root/grant" ]; then
    printf '%s\n' "$subject" >> "$root/prompts"
    printf '%s' "$subject" > "$root/grant"
fi
[ "$(cat "$root/grant")" = "$subject" ] || exit 127
"#,
        );
        fixture.script(
            "sudo",
            r#"
[ "$1" = -n ] || exit 127
shift
if [ "${1:-}" = /usr/bin/true ]; then
    printf 'probe\n' >> "$root/sudo-probes"
    [ -f "$root/sudo-grant" ]
    exit $?
fi
[ -f "$root/sudo-grant" ] || exit 127
printf '%s\n' "$PPID" >> "$root/sudo-executors"
printf '%s\0' "$@" >> "$root/sudo-arguments"
printf 'sudo helper diagnostic' >&2
[ ! -f "$root/sudo-helper-result" ] || exit "$(cat "$root/sudo-helper-result")"
"#,
        );
        // Exercise the real privilege-selection function with isolated tools,
        // even when the test itself is run by root.
        let helper = include_str!("../../../../../data/kwin/setup.sh")
            .replace("/usr/bin/sudo", fixture.0.join("sudo").to_str().unwrap())
            .replace(
                "/usr/bin/pkexec",
                fixture.0.join("pkexec").to_str().unwrap(),
            )
            .replace("if [ \"$EUID\" -eq 0 ]; then", "if false; then")
            .replace("if [ \"$(id -u)\" -eq 0 ]; then", "if false; then");
        fs::write(fixture.0.join("setup.sh"), helper).unwrap();
        fixture.script(
            "pkexec",
            r#"
[ "$1" = --disable-internal-agent ] || exit 127
shift
[ "$(cat "$root/grant")" = "$PPID" ] || exit 127
printf '%s\n' "$PPID" >> "$root/executors"
printf '%s\0' "$@" >> "$root/arguments"
printf 'output\0with\nnewlines'
printf 'helper diagnostic' >&2
[ "${2:-}" != fail ] || exit 1
"#,
        );
        fs::write(
            fixture.0.join("plasma.sh"),
            format!(
                r#"
source '{root}/setup.sh'
payload_dir='{root}'
foreground=0
allow_dependencies=0
main() {{
    exec 9>'{root}/plasma.lock'
    flock -n 9 || return 1
    [ "$1" = --foreground ] || return 1
    if [ "${{2:-}}" = --allow-dependencies ]; then
        privileged --system-dependencies 6.1.0 || return $?
    fi
    privileged --system-remove 1000 root id || return $?
    privileged --system-install 1000 root id source
}}
"#,
                root = fixture.0.display()
            ),
        )
        .unwrap();
        fixture
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

    fn runner(&self) -> String {
        RUNNER
            .replace("/usr/bin/pkcheck", self.0.join("pkcheck").to_str().unwrap())
            .replace("/usr/bin/pkexec", self.0.join("pkexec").to_str().unwrap())
    }

    fn session(&self, lock: Option<&Arc<File>>) -> AuthorizationSession {
        AuthorizationSession(Mutex::new(SessionState {
            process: Some(SessionProcess::start(&self.runner(), lock).unwrap()),
            closed: false,
        }))
    }

    fn lines(&self, name: &str) -> Vec<String> {
        fs::read_to_string(self.0.join(name))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn services_and_multiple_plasma_operations_share_the_same_authorized_subject() {
    let fixture = Fixture::new();
    let session = fixture.session(None);
    let literal = Path::new("a path with ' quotes, $(literal) and\na newline");
    let output = session.services(literal, None).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"output\0with\nnewlines");
    assert_eq!(output.stderr, b"helper diagnostic");
    assert!(session
        .plasma(&fixture.0.join("plasma.sh"), false, None)
        .unwrap()
        .status
        .success());
    let prompts = fixture.lines("prompts");
    assert_eq!(prompts.len(), 1);
    assert_eq!(fixture.lines("executors"), vec![prompts[0].clone(); 3]);
    let checks = fixture.lines("checks");
    assert_eq!(checks.len(), 3);
    assert!(checks.iter().all(|subject| subject == &checks[0]));
    assert_eq!(checks[0].split(',').count(), 3);
    let arguments = fs::read(fixture.0.join("arguments")).unwrap();
    assert!(arguments
        .split(|byte| *byte == 0)
        .any(|arg| arg == literal.as_os_str().as_bytes()));
    assert!(!arguments
        .split(|byte| *byte == 0)
        .any(|arg| arg == b"--system-dependencies"));
    // A new request must neither retain KWin's local lock nor forget consent.
    assert!(session
        .plasma(&fixture.0.join("plasma.sh"), true, None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("prompts").len(), 1);
    assert!(fs::read(fixture.0.join("arguments"))
        .unwrap()
        .split(|byte| *byte == 0)
        .any(|arg| arg == b"--system-dependencies"));
}

#[test]
fn helper_failure_keeps_permission_but_expiry_requires_a_fresh_grant() {
    let fixture = Fixture::new();
    let session = fixture.session(None);
    assert_eq!(
        session
            .services(Path::new("fail"), None)
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert!(session
        .services(Path::new("retry"), None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("prompts").len(), 1);
    fs::remove_file(fixture.0.join("grant")).unwrap();
    assert!(session
        .services(Path::new("expired"), None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("prompts").len(), 2);
}

#[test]
fn denial_and_dismissal_stop_without_helper_execution_or_automatic_retry() {
    for (check_status, helper_status) in [(1, 127), (2, 127), (3, 126), (127, 127)] {
        let fixture = Fixture::new();
        let session = fixture.session(None);
        fs::write(fixture.0.join("denied"), check_status.to_string()).unwrap();
        fs::write(fixture.0.join("sudo-after-denial"), "").unwrap();
        let output = session
            .plasma(&fixture.0.join("plasma.sh"), true, None)
            .unwrap();
        assert_eq!(output.status.code(), Some(helper_status));
        assert_eq!(fixture.lines("checks").len(), 1);
        assert!(!fixture.0.join("executors").exists());
        assert!(!fixture.0.join("sudo-executors").exists());
        fs::remove_file(fixture.0.join("denied")).unwrap();
        assert!(session
            .services(Path::new("explicit retry"), None)
            .unwrap()
            .status
            .success());
        assert_eq!(fixture.lines("prompts").len(), 1);
    }
}

#[test]
fn plasma_reuses_existing_sudo_permission_without_a_polkit_agent() {
    let fixture = Fixture::new();
    let session = fixture.session(None);
    fs::write(fixture.0.join("sudo-grant"), "grant").unwrap();
    fs::write(fixture.0.join("denied"), "2").unwrap();
    assert!(session
        .plasma(&fixture.0.join("plasma.sh"), false, None)
        .unwrap()
        .status
        .success());
    assert!(!fixture.0.join("checks").exists());
    assert!(!fixture.0.join("executors").exists());
    assert_eq!(fixture.lines("sudo-executors").len(), 2);
    let arguments = fs::read(fixture.0.join("sudo-arguments")).unwrap();
    let arguments: Vec<_> = arguments.split(|byte| *byte == 0).collect();
    assert_eq!(arguments[0], b"/bin/bash");
    assert_eq!(
        arguments[1],
        fixture.0.join("setup.sh").as_os_str().as_bytes()
    );
    assert!(!arguments.contains(&b"--system-dependencies".as_slice()));
    assert!(session
        .plasma(&fixture.0.join("plasma.sh"), true, None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("sudo-executors").len(), 5);
    assert!(fs::read(fixture.0.join("sudo-arguments"))
        .unwrap()
        .split(|byte| *byte == 0)
        .any(|arg| arg == b"--system-dependencies"));
    assert!(!fixture.0.join("checks").exists());
}

#[test]
fn a_sudo_helper_failure_does_not_retry_through_polkit() {
    let fixture = Fixture::new();
    let session = fixture.session(None);
    fs::write(fixture.0.join("sudo-grant"), "grant").unwrap();
    fs::write(fixture.0.join("sudo-helper-result"), "1").unwrap();
    let output = session
        .plasma(&fixture.0.join("plasma.sh"), true, None)
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"sudo helper diagnostic");
    assert_eq!(fixture.lines("sudo-executors").len(), 1);
    assert!(!fixture.0.join("checks").exists());
    assert!(!fixture.0.join("executors").exists());
}

#[test]
fn closing_the_session_reaps_its_owner_and_releases_the_inherited_lease() {
    let fixture = Fixture::new();
    let path = fixture.0.join("flow.lock");
    let lease = super::super::lock::FlowLock::try_acquire(&path).unwrap();
    let session = fixture.session(Some(&lease.file()));
    assert!(session
        .services(Path::new("config"), None)
        .unwrap()
        .status
        .success());
    assert!(super::super::lock::FlowLock::try_acquire(&path).is_err());
    let pid = session
        .0
        .lock()
        .unwrap()
        .process
        .as_ref()
        .unwrap()
        .child
        .id();
    session.close();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    drop(lease);
    assert!(super::super::lock::FlowLock::try_acquire(&path).is_ok());
    assert!(session.services(Path::new("after close"), None).is_err());
}

#[test]
fn authorization_owner_child() {
    let Some(root) = std::env::var_os("LG_BUDDY_AUTHORIZATION_TEST_ROOT") else {
        return;
    };
    let fixture = Fixture(root.into());
    let lease = super::super::lock::FlowLock::try_acquire(&fixture.0.join("flow.lock")).unwrap();
    let session = fixture.session(Some(&lease.file()));
    let _ = session.plasma(&fixture.0.join("plasma.sh"), false, None);
    // The parent kills this owner during the call; only it owns fixture cleanup.
    std::mem::forget(fixture);
}

#[test]
fn gui_death_does_not_release_the_lease_while_a_privileged_helper_is_running() {
    use std::{
        os::unix::{net::UnixListener, process::CommandExt},
        time::{Duration, Instant},
    };
    struct Owner(Child);
    impl Drop for Owner {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    fixture.script("pkexec", r#"
exec "$LG_BUDDY_HELPER_TEST_EXE" --exact setup::flow::tests::helper_process::helper_child --nocapture --test-threads=1
"#);
    let listener = UnixListener::bind(fixture.0.join("ready.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut owner = Owner(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "setup::authorization::tests::authorization_owner_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LG_BUDDY_AUTHORIZATION_TEST_ROOT", &fixture.0)
            .env("LG_BUDDY_HELPER_TEST_ROOT", &fixture.0)
            .env("LG_BUDDY_HELPER_TEST_EXE", std::env::current_exe().unwrap())
            .process_group(0)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut connection = loop {
        match listener.accept() {
            Ok((connection, _)) => break connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "privileged helper did not start");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{error}"),
        }
    };
    connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut ready = [0; 5];
    connection.read_exact(&mut ready).unwrap();
    assert_eq!(&ready, b"ready");
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    let lock = fixture.0.join("flow.lock");
    assert!(super::super::lock::FlowLock::try_acquire(&lock).is_err());
    connection.write_all(b"x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while super::super::lock::FlowLock::try_acquire(&lock).is_err() {
        assert!(
            Instant::now() < deadline,
            "authorization owner leaked the lease"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fs::read_to_string(fixture.0.join("completed")).unwrap(),
        "done"
    );
}
