//! State directory resolution shared by all `af` subcommands.
//!
//! Layout under the resolved directory: `spool/`, `rejected/`, `state.db`,
//! managed sidecars, and integration state.

use std::path::PathBuf;

/// The env var and relative layout the per-OS default is built from:
/// `$HOME/.local/state/agentic-footprint` on unix,
/// `%LOCALAPPDATA%\agentic-footprint` on Windows.
#[cfg(unix)]
const BASE_ENV: &str = "HOME";
#[cfg(windows)]
const BASE_ENV: &str = "LOCALAPPDATA";

#[cfg(unix)]
const PLATFORM_SUFFIX: &str = ".local/state/agentic-footprint";
#[cfg(windows)]
const PLATFORM_SUFFIX: &str = "agentic-footprint";

/// Resolves the base state directory: `$AF_STATE_DIR` if set (tests rely on
/// this override), else the per-OS default (see [`BASE_ENV`]).
///
/// Deliberately avoids the `dirs` crate to keep dependencies light — the
/// base env var is set on every platform this project targets (macOS,
/// Linux, Windows 11).
pub fn state_dir() -> PathBuf {
    state_dir_checked().unwrap_or_else(|| panic!("{BASE_ENV} environment variable must be set"))
}

/// The same resolution, reporting an unset base env var as `None` instead
/// of panicking. `af statusline` runs inside another program's status line
/// and must degrade to zeros rather than abort on a stripped environment.
pub fn state_dir_checked() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("AF_STATE_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let base = std::env::var(BASE_ENV).ok()?;
    if base.is_empty() {
        return None;
    }
    Some(PathBuf::from(base).join(PLATFORM_SUFFIX))
}

/// The user's home directory: `$HOME`, with `%USERPROFILE%` as the Windows
/// fallback. Used for agent config locations (`~/.claude`, `~/.codex`) and
/// service definitions, which live under the profile root on every OS.
pub fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    #[cfg(windows)]
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return Some(PathBuf::from(profile));
        }
    }
    None
}
