//! Translate terminal interrupts into the flow's live cancellation contract.
use super::flow::FlowCancellation;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

static PENDING: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupt(_: libc::c_int) {
    PENDING.store(true, Ordering::Relaxed);
}
pub(super) fn interrupted() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

pub(super) struct CancellationSignals {
    previous: libc::sigaction,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl CancellationSignals {
    pub(super) fn install(cancellation: FlowCancellation) -> io::Result<Self> {
        PENDING.store(false, Ordering::Relaxed);
        CANCELLED.store(false, Ordering::Relaxed);
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut previous = unsafe { std::mem::zeroed() };
        action.sa_sigaction = interrupt as *const () as usize;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            // Deliver terminal interrupts to the main thread so input wakes up.
            unsafe {
                let mut mask = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigaddset(&mut mask, libc::SIGINT);
                libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            }
            while !worker_stop.load(Ordering::Relaxed) {
                if PENDING.swap(false, Ordering::Relaxed) {
                    if cancellation.cancel() {
                        CANCELLED.store(true, Ordering::Relaxed);
                    } else {
                        eprintln!("The current setup operation cannot be cancelled. Waiting for it to finish.");
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(Self {
            previous,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for CancellationSignals {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        unsafe {
            libc::sigaction(libc::SIGINT, &self.previous, std::ptr::null_mut());
        }
        CANCELLED.store(false, Ordering::Relaxed);
    }
}

/// Poll before reading so Ctrl+C can cancel a prompt even when BufRead retries
/// interrupted reads. This reader is only used by the dedicated setup command.
pub(super) struct TerminalInput;
impl io::Read for TerminalInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if interrupted() {
                return Err(io::Error::other("setup cancelled"));
            }
            let mut fd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fd, 1, 50) };
            if ready == 0 {
                continue;
            }
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let read =
                unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read >= 0 {
                return Ok(read as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{
        cli::{render, SetupOptions},
        flow::{OnboardingFlow, SetupStep, SetupSteps, StepAnswer},
        lock::{command_with_lock, FlowLock},
        StepCancellation, StepResponse,
    };
    use std::{fs, os::unix::process::CommandExt, process::Command, sync::atomic::AtomicU64};

    struct InterruptingStep {
        done: AtomicBool,
        cancelable: bool,
    }
    impl SetupSteps for InterruptingStep {
        fn inspect(&self, step: SetupStep) -> StepResponse {
            if step != SetupStep::Services || self.done.load(Ordering::Relaxed) {
                StepResponse::Complete
            } else {
                StepResponse::ActionRequired {
                    explanation: "Fixture operation.",
                    requires_authorization: false,
                }
            }
        }
        fn execute(
            &self,
            _: SetupStep,
            _: StepAnswer,
            cancellation: &StepCancellation,
            lease: &FlowLock,
            _: &mut dyn FnMut(StepResponse),
        ) -> StepResponse {
            if self.cancelable {
                unsafe {
                    libc::raise(libc::SIGINT);
                }
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !cancellation.is_cancelled() {
                    assert!(std::time::Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(5));
                }
                StepResponse::Cancelled
            } else {
                assert!(cancellation.begin());
                // Deliver the terminal signal to the entire foreground group:
                // owner, supervisor and helper. Mutation must still finish.
                let group = unsafe { libc::getpgrp() }.to_string();
                let output = command_with_lock("/bin/sh", Some(&lease.file()))
                    .args([
                        "-c",
                        "kill -s INT -- \"-$1\" && sleep 0.1",
                        "helper",
                        &group,
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                assert!(!cancellation.is_cancelled());
                self.done.store(true, Ordering::Relaxed);
                cancellation.finish();
                StepResponse::Complete
            }
        }
    }
    #[test]
    fn signal_child() {
        let Ok(mode) = std::env::var("LG_BUDDY_SIGNAL_TEST") else {
            return;
        };
        let root = std::path::PathBuf::from(std::env::var_os("LG_BUDDY_SIGNAL_ROOT").unwrap());
        let mut flow = OnboardingFlow::with_backend(
            Box::new(InterruptingStep {
                done: AtomicBool::new(false),
                cancelable: mode == "pairing",
            }),
            &root.join("lock"),
        )
        .unwrap();
        let _signals = CancellationSignals::install(flow.cancellation()).unwrap();
        let options = SetupOptions {
            yes: true,
            ..SetupOptions::default()
        };
        let result = render(
            &mut flow,
            &options,
            false,
            &mut io::empty(),
            &mut Vec::new(),
        );
        if mode == "pairing" {
            assert_eq!(result.unwrap_err().exit_code(), 130);
        } else {
            result.unwrap();
        }
    }
    #[test]
    fn terminal_interrupts_obey_the_live_step_gate() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-signal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        for mode in ["pairing", "services"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "setup::terminal_signals::tests::signal_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("LG_BUDDY_SIGNAL_TEST", mode)
                .env("LG_BUDDY_SIGNAL_ROOT", &root)
                .process_group(0)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            if mode == "services" {
                assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be cancelled"));
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
