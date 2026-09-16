use super::*;
use crate::{
    settings::{ServiceController, SystemdUserServiceController},
    setup::{kwin::KWinSetup, lock::command_with_lock},
};
use std::{
    io::Read,
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    time::{Duration, Instant},
};

// These adapters invoke the actual service and KWin command paths, substituting
// only the executable for an isolated helper which emulates sudo's FD closing.
struct HelperSteps {
    helper: PathBuf,
    plasma: bool,
}
impl SetupSteps for HelperSteps {
    fn inspect(&self, step: SetupStep) -> StepResponse {
        if step
            == if self.plasma {
                SetupStep::Plasma
            } else {
                SetupStep::Services
            }
        {
            action()
        } else {
            StepResponse::Complete
        }
    }
    fn execute(
        &self,
        _: SetupStep,
        _: StepAnswer,
        cancellation: &StepCancellation,
        lease: &FlowLock,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        if self.plasma {
            KWinSetup {
                helper: &self.helper,
                authorization: crate::setup::flow::AuthorizationMode::Noninteractive,
                command_lock: Some(lease.file()),
            }
            .execute(false, cancellation, progress)
        } else {
            assert!(cancellation.begin());
            SystemdUserServiceController::from_env().with_command_lock(
                lease.file(),
                |controller| {
                    assert!(controller.stop_user_service("fixture.service").is_err());
                },
            );
            StepResponse::Failed(failure(true))
        }
    }
}

#[test]
fn owner_child() {
    let Some(root) = std::env::var_os("LG_BUDDY_HELPER_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    let steps = HelperSteps {
        helper: root.join("helper.sh"),
        plasma: std::env::var("LG_BUDDY_HELPER_TEST_MODE").unwrap() == "plasma",
    };
    let mut flow = OnboardingFlow::with_backend(Box::new(steps), &root.join("setup.lock")).unwrap();
    advance(&mut flow);
}

#[test]
fn helper_child() {
    let Some(root) = std::env::var_os("LG_BUDDY_HELPER_TEST_ROOT").map(PathBuf::from) else {
        return;
    };
    // Reproduce privilege wrappers closing inherited descriptors. Only the
    // waiting supervisor can preserve the flow lease from this point onward.
    for fd in 3..1024 {
        unsafe {
            libc::close(fd);
        }
    }
    let mut connection = UnixStream::connect(root.join("ready.sock")).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    connection.write_all(b"ready").unwrap();
    let mut release = [0];
    connection.read_exact(&mut release).unwrap();
    fs::write(root.join("completed"), "done").unwrap();
    std::process::exit(126);
}

struct Owner(std::process::Child);
impl Drop for Owner {
    fn drop(&mut self) {
        // This process group contains only this test's owner and helper tree.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

#[test]
fn orphaned_service_and_plasma_helpers_keep_competing_flows_excluded() {
    for mode in ["service", "plasma"] {
        let fixture = Fixture::new([action(), action(), action()]);
        let helper = fixture.root.join("helper.sh");
        fs::write(&helper, r#"#!/bin/sh
[ "$1" != --status ] || exit 3
exec "$LG_BUDDY_HELPER_TEST_EXE" --exact setup::flow::tests::helper_process::helper_child --nocapture --test-threads=1
"#).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        let listener = UnixListener::bind(fixture.root.join("ready.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut owner = Owner(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "setup::flow::tests::helper_process::owner_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("LG_BUDDY_HELPER_TEST_ROOT", &fixture.root)
                .env("LG_BUDDY_HELPER_TEST_MODE", mode)
                .env("LG_BUDDY_HELPER_TEST_EXE", std::env::current_exe().unwrap())
                .env("LG_BUDDY_SYSTEMCTL", &helper)
                .process_group(0)
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut connection = loop {
            match listener.accept() {
                Ok((connection, _)) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "helper did not start: {mode}");
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
        assert!(fixture.open().is_err());
        // Kill just the GUI/CLI-equivalent owner while the helper is blocked.
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        assert!(fixture.open().is_err(), "{mode} helper lost its flow lease");
        connection.write_all(b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fixture.open().is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{mode} helper leaked its flow lease"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fs::read_to_string(fixture.root.join("completed")).unwrap(),
            "done"
        );
    }
}

#[test]
fn supervised_commands_preserve_literal_arguments_output_and_exit_status() {
    let fixture = Fixture::new([action(), action(), action()]);
    let lease = FlowLock::acquire(&fixture.lock()).unwrap();
    let literal = "a quoted ' argument with $(literal) and $HOME";
    let output = command_with_lock("/bin/sh", Some(&lease.file()))
        .args([
            "-c",
            "printf '%s' \"$1\"; printf 'failure' >&2; exit 126",
            "helper",
            literal,
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(126));
    assert_eq!(output.stdout, literal.as_bytes());
    assert_eq!(output.stderr, b"failure");
    drop(lease);
    assert!(fixture.open().is_ok());
}
