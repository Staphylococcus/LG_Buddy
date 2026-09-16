//! Explicit KWin provisioning. Status uses the provisioner's read-only protocol;
//! login loading and the runtime inhibition source do not run this executor.
use super::{StepCancellation, StepFailure, StepInput, StepResponse};
use crate::presentation::brightness::UserFacingError;
use std::path::Path;
use std::process::Output;
use std::{fs::File, sync::Arc};

const BUILD_DEPENDENCIES: StepResponse = StepResponse::InputRequired(StepInput::BuildDependencies {
    explanation: "A compatible Plasma plugin could not be built with the installed tools. Install the compiler and development packages needed to build it? This requires administrator permission.",
});

pub(crate) struct KWinSetup<'a> {
    pub helper: &'a Path,
    pub interactive_authorization: bool,
    pub command_lock: Option<Arc<File>>,
}
impl KWinSetup<'_> {
    pub(crate) fn inspect(&self) -> StepResponse {
        match self.invoke(&["--status"]) {
            Ok(output) => match output.status.code() {
                Some(0) => StepResponse::Complete,
                Some(2) => StepResponse::NotApplicable,
                Some(3) => StepResponse::ActionRequired {
                    explanation: "Set up Plasma integration so applications can keep the TV on. A compatible plugin is used when available; otherwise LG Buddy attempts a local build.",
                    requires_authorization: true,
                },
                Some(4) => StepResponse::Blocked(failure("This installation does not support automatic Plasma integration setup.", &output, false)),
                _ => StepResponse::Failed(failure("Plasma integration could not be checked.", &output, true)),
            },
            Err(error) => io_failure(error),
        }
    }
    pub(crate) fn execute(
        &self,
        allow_dependencies: bool,
        cancellation: &StepCancellation,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        if !cancellation.begin() {
            return if cancellation.is_cancelled() {
                StepResponse::Cancelled
            } else {
                StepResponse::Blocked(StepFailure {
                    presentation: UserFacingError::new(
                        "Plasma setup incomplete",
                        "This attempt has already started.",
                    ),
                    diagnostic: "duplicate KWin setup attempt".into(),
                    retryable: false,
                })
            };
        }
        progress(StepResponse::Running {
            message: "Setting up Plasma integration…",
            cancelable: false,
        });
        let before = self.inspect();
        let response = if matches!(before, StepResponse::ActionRequired { .. }) {
            let mut args = vec!["--foreground"];
            if allow_dependencies {
                args.push("--allow-dependencies");
            }
            if !self.interactive_authorization {
                args.push("--noninteractive");
            }
            match self.invoke(&args) {
                Ok(output) => match output.status.code() {
                    Some(0) => match self.inspect() {
                        StepResponse::Complete => StepResponse::Complete,
                        _ => StepResponse::Failed(failure("Plasma integration did not become available. LG Buddy will continue using its available sources.", &output, true)),
                    },
                    Some(77) if !allow_dependencies => BUILD_DEPENDENCIES,
                    Some(2) => self.inspect(),
                    Some(126) => StepResponse::Cancelled,
                    _ => StepResponse::Failed(failure("Plasma integration could not be set up. LG Buddy will continue using its available sources.", &output, true)),
                },
                Err(error) => io_failure(error),
            }
        } else {
            before
        };
        cancellation.finish();
        response
    }
    fn invoke(&self, args: &[&str]) -> std::io::Result<Output> {
        // Never inherit interactive terminal input into background workers.
        let lock = if args == ["--status"] {
            None
        } else {
            self.command_lock.as_ref()
        };
        super::lock::command_with_lock("bash", lock)
            .arg(self.helper)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
    }
}
fn failure(message: &str, output: &Output, retryable: bool) -> StepFailure {
    StepFailure {
        presentation: UserFacingError::new("Plasma setup incomplete", message),
        diagnostic: format!(
            "KWin setup exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
        retryable,
    }
}
fn io_failure(error: std::io::Error) -> StepResponse {
    StepResponse::Failed(StepFailure {
        presentation: UserFacingError::new(
            "Plasma setup incomplete",
            "The Plasma setup helper could not be run.",
        ),
        diagnostic: error.to_string(),
        retryable: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "lg-buddy-kwin-step-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("status"), "3").unwrap();
            fs::write(root.join("result"), "0").unwrap();
            fs::write(
                root.join("helper.sh"),
                r#"#!/bin/bash
cd -- "$(dirname -- "$0")"
[ "$1" != --status ] || exit "$(cat status)"
printf '%s\n' "$*" >> actions
result="$(cat result)"
if [ "$result" = 0 ] && [ ! -f fail-verification ]; then echo 0 > status; fi
exit "$result"
"#,
            )
            .unwrap();
            Self(root)
        }
        fn helper(&self) -> std::path::PathBuf {
            self.0.join("helper.sh")
        }
        fn run(&self, allow: bool) -> StepResponse {
            KWinSetup {
                helper: &self.helper(),
                interactive_authorization: false,
                command_lock: None,
            }
            .execute(allow, &StepCancellation::default(), &mut |_| {})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn status_and_inapplicable_desktop_do_not_execute_provisioning() {
        let f = Fixture::new();
        let helper = f.helper();
        let step = KWinSetup {
            helper: &helper,
            interactive_authorization: true,
            command_lock: None,
        };
        assert!(matches!(
            step.inspect(),
            StepResponse::ActionRequired { .. }
        ));
        assert!(!f.0.join("actions").exists());
        fs::write(f.0.join("status"), "2").unwrap();
        assert_eq!(f.run(false), StepResponse::NotApplicable);
        assert!(!f.0.join("actions").exists());
    }
    #[test]
    fn successful_provisioning_is_verified_and_repeat_execution_does_nothing() {
        let f = Fixture::new();
        assert_eq!(f.run(false), StepResponse::Complete);
        let actions = fs::read(f.0.join("actions")).unwrap();
        assert_eq!(f.run(false), StepResponse::Complete);
        assert_eq!(fs::read(f.0.join("actions")).unwrap(), actions);
        assert!(String::from_utf8(actions)
            .unwrap()
            .contains("--noninteractive"));
    }
    #[test]
    fn dependencies_are_a_separate_input_and_denial_does_not_retry() {
        let f = Fixture::new();
        fs::write(f.0.join("result"), "77").unwrap();
        assert_eq!(f.run(false), BUILD_DEPENDENCIES);
        fs::write(f.0.join("result"), "126").unwrap();
        assert_eq!(f.run(true), StepResponse::Cancelled);
        assert_eq!(
            fs::read_to_string(f.0.join("actions"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(!fs::read_to_string(f.0.join("actions"))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .contains("--allow-dependencies"));
        fs::write(f.0.join("result"), "0").unwrap();
        assert_eq!(f.run(true), StepResponse::Complete);
    }
    #[test]
    fn exhausted_provisioning_and_failed_verification_remain_incomplete() {
        let f = Fixture::new();
        fs::write(f.0.join("result"), "1").unwrap();
        assert!(matches!(f.run(true), StepResponse::Failed(_)));
        fs::write(f.0.join("result"), "0").unwrap();
        fs::write(f.0.join("fail-verification"), "").unwrap();
        assert!(matches!(f.run(true), StepResponse::Failed(_)));
    }
    #[test]
    fn cancellation_is_accepted_before_execution_and_rejected_during_provisioning() {
        let f = Fixture::new();
        let helper = f.helper();
        let step = KWinSetup {
            helper: &helper,
            interactive_authorization: true,
            command_lock: None,
        };
        let cancellation = StepCancellation::default();
        assert!(cancellation.cancel());
        assert_eq!(
            step.execute(false, &cancellation, &mut |_| panic!("cancelled")),
            StepResponse::Cancelled
        );
        assert!(!f.0.join("actions").exists());
        let cancellation = StepCancellation::default();
        assert_eq!(
            step.execute(false, &cancellation, &mut |s| {
                assert!(matches!(
                    s,
                    StepResponse::Running {
                        cancelable: false,
                        ..
                    }
                ));
                assert!(!cancellation.cancel());
            }),
            StepResponse::Complete
        );
    }
}
