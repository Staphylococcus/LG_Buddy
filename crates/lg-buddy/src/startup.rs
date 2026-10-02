//! Startup conversion boundary for the user-owned screen daemon.
//!
//! Resolves the config path and converts a supported stale 1.x / bscpylgtv
//! config before the daemon reads it.
//!
//! The screen daemon (`LG_Buddy_screen.service`) runs as the config's owner.
//! The lifecycle daemon (`LG_Buddy_lifecycle.service`) runs as root, and
//! `migrate_config` only ever writes as the file's non-root owner, so root
//! skips the migration and the read path surfaces `MigrationRequired` (the base
//! behavior); the owning screen daemon converts the file and this daemon's
//! `Restart=on-failure` retry picks up the current config within one cycle.

use std::path::PathBuf;

use crate::migration::automatic::migrate_config;
use crate::RunError;

/// Resolve the config path, run automatic migration when this process can be
/// the config's non-root owner, then hand the path back so the caller can read
/// a current `CurrentConfig`.
///
/// A root process (the system-scope lifecycle daemon) can never be the owner,
/// so it skips the migration entirely and degrades to the read-only base
/// behavior; the owning user-scope daemon self-heals the file.
///
/// # Errors
/// Returns `RunError::ConfigPath` if the config path cannot be resolved, or
/// `RunError::Migration` if the in-place conversion fails.
pub fn backend_start() -> Result<PathBuf, RunError> {
    let config_path =
        crate::config::resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    // euid check so a non-owner user daemon still fails loudly on a real I/O migration
    // error; upgrade to an owner_for_write probe if root becomes the only caller.
    if unsafe { libc::geteuid() } == 0 {
        return Ok(config_path);
    }
    migrate_config(&config_path)?;
    Ok(config_path)
}
