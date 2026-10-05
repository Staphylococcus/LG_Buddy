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
noninteractive=0
if [ "$1" = -n ]; then noninteractive=1; shift; fi
printf '%s\n' "$PPID" >> "$root/sudo-callers"
printf '%s\n' "$noninteractive" >> "$root/sudo-modes"
has_grant() {
    [ -f "$root/sudo-grant" ] || return 1
    grant="$(cat "$root/sudo-grant")"
    [ "$grant" = grant ] || [ "$grant" = "$PPID" ]
}
if [ "${1:-}" = /usr/bin/true ]; then
    printf 'probe\n' >> "$root/sudo-probes"
    has_grant
    exit $?
fi
if ! has_grant; then
    [ "$noninteractive" = 0 ] || exit 1
    [ ! -f "$root/sudo-denied" ] || exit 1
    if [ -f "$root/terminal-input-required" ]; then
        printf 'Fixture authorization: ' > /dev/tty
        read -r password < /dev/tty || exit 1
        [ "$password" = fixture-password ] || exit 1
    fi
    printf '%s\n' "$PPID" >> "$root/sudo-prompts"
    printf '%s' "$PPID" > "$root/sudo-grant"
fi
[ "${1:-}" != -v ] || exit 0
printf '%s\n' "$PPID" >> "$root/sudo-executors"
printf '%s\0' "$@" >> "$root/sudo-arguments"
[ ! -f "$root/stall-mutation" ] || {
    touch "$root/ready"
    while [ ! -f "$root/release" ]; do sleep 0.01; done
}
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
[ ! -f "$root/stall-mutation" ] || {
    touch "$root/ready"
    while [ ! -f "$root/release" ]; do sleep 0.01; done
}
printf 'output\0with\nnewlines'
printf 'helper diagnostic' >&2
[ "${2:-}" != fail ] || exit 1
"#,
        );
        // Lock Bash's fd 9 through the test binary; the shell retains the
        // locked open-file description after the child exits.
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
    LG_BUDDY_PLASMA_LOCK_CHILD=1 '{test_exe}' --exact setup::authorization::tests::plasma_lock_child --quiet >/dev/null || return 1
    [ "$1" = --foreground ] || return 1
    printf '%s\n' "$*" >> '{root}/plasma-options'
    for option in "${{@:2}}"; do
        case "$option" in
            --terminal) terminal=1 ;;
            --noninteractive) noninteractive=1 ;;
            --allow-dependencies) allow_dependencies=1 ;;
            *) return 1 ;;
        esac
    done
    if [ "$allow_dependencies" = 1 ]; then
        privileged --system-dependencies 6.1.0 || return $?
    fi
    privileged --system-remove 1000 root id || return $?
    privileged --system-install 1000 root id source
}}
"#,
                root = fixture.0.display(),
                test_exe = std::env::current_exe().unwrap().display()
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
            .replace("/usr/bin/sudo", self.0.join("sudo").to_str().unwrap())
    }

    fn session(&self, lock: Option<&Arc<File>>) -> AuthorizationSession {
        self.session_for_mode(AuthorizationMode::Interactive, lock)
    }

    fn session_for_mode(
        &self,
        mode: AuthorizationMode,
        lock: Option<&Arc<File>>,
    ) -> AuthorizationSession {
        AuthorizationSession(Mutex::new(SessionState {
            process: Some(SessionProcess::start(&self.runner(), mode, lock).unwrap()),
            closed: false,
            mode,
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

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "worker did not reach {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn every_plasma_privilege_route_protects_mutation_before_execution() {
    for (mode, cached) in [
        (AuthorizationMode::Terminal, false),
        (AuthorizationMode::Terminal, true),
        (AuthorizationMode::Interactive, false),
        (AuthorizationMode::Interactive, true),
        (AuthorizationMode::Noninteractive, true),
    ] {
        let fixture = Fixture::new();
        if cached {
            fs::write(fixture.0.join("sudo-grant"), "grant").unwrap();
        }
        fs::write(fixture.0.join("stall-mutation"), "").unwrap();
        let path = fixture.0.join("flow.lock");
        let lease = super::super::lock::FlowLock::try_acquire(&path).unwrap();
        let session = Arc::new(fixture.session_for_mode(mode, Some(&lease.file())));
        let cancellation = super::super::StepCancellation::default();
        let protected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = std::thread::spawn({
            let session = session.clone();
            let cancellation = cancellation.clone();
            let protected = protected.clone();
            let root = fixture.0.clone();
            let file = lease.file();
            move || {
                session.plasma_cancellable(
                    &root.join("plasma.sh"),
                    true,
                    Some(&file),
                    &cancellation,
                    &mut || {
                        assert!(!root.join("sudo-executors").exists());
                        assert!(!root.join("executors").exists());
                        assert!(!cancellation.can_cancel());
                        protected.store(true, Ordering::Release);
                    },
                )
            }
        });
        wait_for_file(&fixture.0.join("ready"));
        let acknowledged = protected.load(Ordering::Acquire);
        let cancelled = cancellation.cancel();
        fs::write(fixture.0.join("release"), "").unwrap();
        let result = worker.join().unwrap();
        session.close();
        assert!(acknowledged, "unprotected route: {mode:?}, cached={cached}");
        assert!(!cancelled, "running mutation accepted cancellation");
        assert!(result.unwrap().status.success());
        let executions = if mode == AuthorizationMode::Interactive && !cached {
            "executors"
        } else {
            "sudo-executors"
        };
        assert_eq!(fixture.lines(executions).len(), 3);
        assert!(super::super::lock::FlowLock::try_acquire(&path).is_err());
    }
}

#[test]
fn releasing_an_idle_authorizer_allows_reacquisition_without_closing_the_session() {
    let fixture = Fixture::new();
    let path = fixture.0.join("flow.lock");
    let lease = super::super::lock::FlowLock::try_acquire(&path).unwrap();
    let session = fixture.session(Some(&lease.file()));
    assert!(session
        .services(Path::new("config"), None)
        .unwrap()
        .status
        .success());
    session.release_process();
    assert!(!session.0.lock().unwrap().closed);
    drop(lease);
    let deadline = Instant::now() + Duration::from_secs(2);
    let lease = loop {
        if let Ok(lease) = super::super::lock::FlowLock::try_acquire(&path) {
            break lease;
        }
        assert!(
            Instant::now() < deadline,
            "idle authorizer retained the old lease"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    // Polkit grants belong to the old process subject, not the new owner.
    fs::remove_file(fixture.0.join("grant")).unwrap();
    session.0.lock().unwrap().process = Some(
        SessionProcess::start(
            &fixture.runner(),
            AuthorizationMode::Interactive,
            Some(&lease.file()),
        )
        .unwrap(),
    );
    assert!(session
        .services(Path::new("retry"), None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("prompts").len(), 2);
    session.close();
}

#[test]
fn preparation_stalls_are_cancelable_without_mutating_or_leaking_the_owner() {
    for phase in ["authorization", "build", "ipc"] {
        let fixture = Fixture::new();
        let stall = format!(
            "touch '{}/ready'; while true; do sleep 0.01; done",
            fixture.0.display()
        );
        let (script, operation, argument) = match phase {
            "authorization" => { fixture.script("pkcheck", &stall); (fixture.runner(), "services", fixture.0.join("config")) }
            "build" => { fs::write(fixture.0.join("plasma.sh"), format!("main() {{ {stall}; }}")).unwrap(); (fixture.runner(), "plasma", fixture.0.join("plasma.sh")) }
            _ => (format!("IFS= read -r -d '' op; IFS= read -r -d '' path; IFS= read -r -d '' option; {stall}"), "services", fixture.0.join("config")),
        };
        let lock_path = fixture.0.join("flow.lock");
        let lease = super::super::lock::FlowLock::try_acquire(&lock_path).unwrap();
        let mut process =
            SessionProcess::start(&script, AuthorizationMode::Interactive, Some(&lease.file()))
                .unwrap();
        let cancellation = super::super::StepCancellation::default();
        let worker_cancel = cancellation.clone();
        let worker = std::thread::spawn(move || {
            process.run(
                operation,
                &argument,
                false,
                Some(&worker_cancel),
                &mut || panic!("no mutation consent"),
                Duration::from_secs(5),
            )
        });
        wait_for_file(&fixture.0.join("ready"));
        assert!(cancellation.cancel());
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        drop(lease);
        let deadline = Instant::now() + Duration::from_secs(2);
        while super::super::lock::FlowLock::try_acquire(&lock_path).is_err() {
            assert!(
                Instant::now() < deadline,
                "cancelled preparation retained its lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn observation_timeout_does_not_kill_mutation_or_release_its_lease() {
    let fixture = Fixture::new();
    fixture.script("pkexec", "touch \"$root/ready\"; while [ ! -f \"$root/release\" ]; do sleep 0.01; done; touch \"$root/finished\"");
    let script = fixture.runner();
    let path = fixture.0.join("flow.lock");
    let lease = super::super::lock::FlowLock::try_acquire(&path).unwrap();
    let mut process =
        SessionProcess::start(&script, AuthorizationMode::Interactive, Some(&lease.file()))
            .unwrap();
    let cancellation = super::super::StepCancellation::default();
    let worker_cancel = cancellation.clone();
    let worker = std::thread::spawn(move || {
        process.run(
            "services",
            Path::new("config"),
            false,
            Some(&worker_cancel),
            &mut || assert!(!worker_cancel.cancel()),
            Duration::from_millis(200),
        )
    });
    wait_for_file(&fixture.0.join("ready"));
    assert!(!cancellation.cancel());
    assert_eq!(
        worker.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    drop(lease);
    assert!(
        super::super::lock::FlowLock::try_acquire(&path).is_err(),
        "active mutation must exclude another flow after caller closes"
    );
    fs::write(fixture.0.join("release"), "").unwrap();
    wait_for_file(&fixture.0.join("finished"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while super::super::lock::FlowLock::try_acquire(&path).is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn terminal_services_and_plasma_share_parent_scoped_sudo_permission() {
    let fixture = Fixture::new();
    let session = fixture.session_for_mode(AuthorizationMode::Terminal, None);
    assert!(!fixture.0.join("sudo-prompts").exists());
    let config = Path::new("a path with ' quotes, $(literal) and\na newline");
    assert!(session.services(config, None).unwrap().status.success());
    let plasma = fixture.0.join("plasma.sh");
    assert!(session
        .plasma(&plasma, false, None)
        .unwrap()
        .status
        .success());
    assert!(!fs::read(fixture.0.join("sudo-arguments"))
        .unwrap()
        .split(|byte| *byte == 0)
        .any(|arg| arg == b"--system-dependencies"));
    assert!(session
        .plasma(&plasma, true, None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("sudo-prompts").len(), 1);
    let callers = fixture.lines("sudo-callers");
    assert!(callers.iter().all(|pid| pid == &callers[0]));
    assert_eq!(fixture.lines("sudo-executors").len(), 6);
    assert!(fs::read(fixture.0.join("sudo-arguments"))
        .unwrap()
        .split(|byte| *byte == 0)
        .any(|arg| arg == config.as_os_str().as_bytes()));
    assert_eq!(
        fixture.lines("plasma-options"),
        [
            "--foreground --terminal",
            "--foreground --terminal --allow-dependencies"
        ]
    );
    assert!(!fixture.0.join("checks").exists());
    assert!(!fixture.0.join("executors").exists());
    session.close();
    // Closing our owner must not invalidate the user's native sudo cache.
    assert!(fixture.0.join("sudo-grant").exists());
}

#[test]
fn plasma_lock_child() {
    if std::env::var_os("LG_BUDDY_PLASMA_LOCK_CHILD").is_none() {
        return;
    }
    assert_eq!(
        unsafe { libc::flock(9, libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
}

#[test]
fn terminal_prompt_child() {
    let Some(root) = std::env::var_os("LG_BUDDY_TERMINAL_AUTHORIZATION_ROOT") else {
        return;
    };
    // The parent owns this directory, including when an assertion fails here.
    let fixture = std::mem::ManuallyDrop::new(Fixture(root.into()));
    let session = fixture.session_for_mode(AuthorizationMode::Terminal, None);
    assert!(session
        .services(Path::new("config"), None)
        .unwrap()
        .status
        .success());
    assert!(session
        .plasma(&fixture.0.join("plasma.sh"), false, None)
        .unwrap()
        .status
        .success());
    session.close();
}

#[test]
fn terminal_authentication_can_read_the_controlling_tty_without_consuming_protocol_input() {
    use std::{
        os::{fd::FromRawFd, unix::process::CommandExt},
        time::{Duration, Instant},
    };
    let fixture = Fixture::new();
    fs::write(fixture.0.join("terminal-input-required"), "").unwrap();
    let (mut master_fd, mut slave_fd) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "setup::authorization::tests::terminal_prompt_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("LG_BUDDY_TERMINAL_AUTHORIZATION_ROOT", &fixture.0)
        .stdin(slave);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    master.write_all(b"fixture-password\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            panic!("terminal authorization did not finish");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(status.success());
    assert_eq!(fixture.lines("sudo-prompts").len(), 1);
    assert_eq!(fixture.lines("sudo-executors").len(), 3);
}

#[test]
fn terminal_sudo_preserves_grants_after_helper_failure_and_observes_expiry() {
    let fixture = Fixture::new();
    let session = fixture.session_for_mode(AuthorizationMode::Terminal, None);
    fs::write(fixture.0.join("sudo-helper-result"), "1").unwrap();
    let output = session.services(Path::new("config"), None).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"sudo helper diagnostic");
    assert_eq!(fixture.lines("sudo-executors").len(), 1);
    fs::remove_file(fixture.0.join("sudo-helper-result")).unwrap();
    assert!(session
        .services(Path::new("retry"), None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("sudo-prompts").len(), 1);
    fs::remove_file(fixture.0.join("sudo-grant")).unwrap();
    assert!(session
        .plasma(&fixture.0.join("plasma.sh"), false, None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("sudo-prompts").len(), 2);
    let prompts = fixture.lines("sudo-prompts");
    assert_eq!(prompts[0], prompts[1]);
    assert!(!fixture.0.join("checks").exists());
}

#[test]
fn terminal_sudo_denial_does_not_execute_or_fall_back_to_polkit() {
    let fixture = Fixture::new();
    let session = fixture.session_for_mode(AuthorizationMode::Terminal, None);
    fs::write(fixture.0.join("sudo-denied"), "").unwrap();
    assert_eq!(
        session
            .services(Path::new("config"), None)
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(fixture.lines("sudo-callers").len(), 1);
    assert!(!fixture.0.join("sudo-executors").exists());
    assert!(!fixture.0.join("checks").exists());
    fs::remove_file(fixture.0.join("sudo-denied")).unwrap();
    assert!(session
        .services(Path::new("explicit retry"), None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("sudo-prompts").len(), 1);
}

#[test]
fn noninteractive_sudo_never_prompts_and_reuses_only_existing_permission() {
    let fixture = Fixture::new();
    let session = fixture.session_for_mode(AuthorizationMode::Noninteractive, None);
    let plasma = fixture.0.join("plasma.sh");
    assert_eq!(
        session
            .services(Path::new("config"), None)
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        session.plasma(&plasma, true, None).unwrap().status.code(),
        Some(127)
    );
    assert!(!fixture.0.join("sudo-prompts").exists());
    assert!(!fixture.0.join("sudo-executors").exists());
    let callers = fixture.lines("sudo-callers");
    fs::write(fixture.0.join("sudo-grant"), &callers[0]).unwrap();
    assert!(session
        .services(Path::new("config"), None)
        .unwrap()
        .status
        .success());
    assert!(session
        .plasma(&plasma, true, None)
        .unwrap()
        .status
        .success());
    assert_eq!(fixture.lines("sudo-executors").len(), 4);
    assert!(fixture.lines("sudo-modes").iter().all(|mode| mode == "1"));
    assert!(fixture
        .lines("sudo-callers")
        .iter()
        .all(|pid| pid == &callers[0]));
    assert!(!fixture.0.join("checks").exists());
    assert!(!fixture.0.join("sudo-prompts").exists());
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
        .as_ref()
        .unwrap()
        .id();
    session.close();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "idle owner did not finish closing"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(lease);
    assert!(super::super::lock::FlowLock::try_acquire(&path).is_ok());
    assert!(session.services(Path::new("after close"), None).is_err());
}

#[test]
fn authorization_owner_child() {
    let Some(root) = std::env::var_os("LG_BUDDY_AUTHORIZATION_TEST_ROOT") else {
        return;
    };
    // The parent owns this directory, including if this child exits early.
    let fixture = std::mem::ManuallyDrop::new(Fixture(root.into()));
    let lease = super::super::lock::FlowLock::try_acquire(&fixture.0.join("flow.lock")).unwrap();
    let session = fixture.session(Some(&lease.file()));
    let _ = session.plasma(&fixture.0.join("plasma.sh"), false, None);
    // The parent kills this owner during the call and cleans up the fixture.
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
