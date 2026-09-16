//! A stable per-user inode serializes CLI and GUI even for different configs.
//! Never unlink it: a competing open must always lock the same inode.
use super::StepFailure;
use crate::presentation::brightness::UserFacingError;
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
        unix::process::CommandExt,
    },
    path::Path,
    process::Command,
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct FlowLock(Arc<File>);

impl FlowLock {
    pub(super) fn acquire(path: &Path) -> Result<Self, StepFailure> {
        let open = || -> io::Result<File> {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("missing runtime directory"))?;
            let metadata = std::fs::symlink_metadata(parent)?;
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o022 != 0
            {
                return Err(io::Error::other(
                    "runtime directory is not owned by the current user or is writable by others",
                ));
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o077 != 0
            {
                return Err(io::Error::other("unsafe onboarding lock file"));
            }
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(file)
        };
        open().map(|file| Self(Arc::new(file))).map_err(|error| StepFailure {
            presentation: UserFacingError::new("Setup unavailable", if error.kind() == io::ErrorKind::WouldBlock {
                "Another LG Buddy setup is already open. Finish or close it before starting another."
            } else { "The setup lock could not be acquired. Run LG Buddy in your user session." }),
            diagnostic: error.to_string(), retryable: true,
        })
    }
    pub(super) fn file(&self) -> Arc<File> {
        self.0.clone()
    }
}

impl Drop for FlowLock {
    fn drop(&mut self) {
        // Normal completion can unlock immediately once the synchronous command
        // waits and their retained Arcs have ended. This avoids brief false-busy
        // results from unrelated threads' fork-before-exec descriptor copies.
        // Process death does not run Drop: a surviving supervisor keeps the lock.
        if Arc::strong_count(&self.0) == 1 {
            unsafe {
                libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

/// A waiting shell retains the lease even when sudo/pkexec closes inherited
/// descriptors in the actual helper. Arguments are forwarded literally, never
/// interpolated as shell code. The command has the helper's exit status/output.
/// Call synchronously with output/status, retaining the command until it exits.
pub(crate) fn command_with_lock(program: impl AsRef<OsStr>, lock: Option<&Arc<File>>) -> Command {
    let Some(lock) = lock else {
        return Command::new(program);
    };
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "\"$@\" &\nwait \"$!\"", "lg-buddy-setup"])
        .arg(program);
    let lock = lock.clone();
    // Duplicate only in the child, after stdio setup. The supervisor inherits a
    // descriptor >= 3; unrelated processes spawned by other threads do not.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(lock.as_raw_fd(), libc::F_DUPFD, 3) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}
