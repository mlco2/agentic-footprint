//! State directory resolution shared by all `af` subcommands.
//!
//! Layout under the resolved directory: `spool/`, `rejected/`, `state.db`,
//! managed sidecars, and integration state.

use std::path::PathBuf;

/// Resolves the base state directory: `$AF_STATE_DIR` if set (tests rely on
/// this override), else `~/.local/state/agentic-footprint` derived from
/// `$HOME`.
///
/// Deliberately avoids the `dirs` crate to keep dependencies light — `HOME`
/// is set on every platform this PoC targets (macOS, Linux).
pub fn state_dir() -> PathBuf {
    state_dir_checked().expect("HOME environment variable must be set")
}

/// The same resolution, reporting an unset `HOME` as `None` instead of
/// panicking. `af statusline` runs inside another program's status line and
/// must degrade to zeros rather than abort on a stripped environment.
pub fn state_dir_checked() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("AF_STATE_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = std::env::var("HOME").ok()?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(".local/state/agentic-footprint"))
}
