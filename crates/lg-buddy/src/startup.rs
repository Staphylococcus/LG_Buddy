//! Shared backend startup boundary, invoked at the start of the persistent
//! daemon entry points (`session::runner::run_monitor` / `run_lifecycle_monitor`).
//!
//! Runs before any config-dependent work: resolves the config path and converts
//! a supported stale 1.x / bscpylgtv config in place so the daemon can proceed
//! on an already-current config. The persistent daemons (`LG_Buddy_lifecycle`
//! and `LG_Buddy_screen`) start at boot, so by the time a user reaches the GUI
//! the config has already been migrated — one migration point covers every path.

use std::path::PathBuf;

use crate::migration::automatic::migrate_config;
use crate::RunError;

/// Resolve the config path and run automatic migration, then hand the path back
/// so the caller can read a current `CurrentConfig`.
///
/// # Errors
/// Returns `RunError::ConfigPath` if the config path cannot be resolved, or
/// `RunError::Migration` if the config cannot be loaded or converted in place.
pub fn backend_start() -> Result<PathBuf, RunError> {
    let config_path = crate::config::resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    migrate_config(&config_path)?;
    Ok(config_path)
}
