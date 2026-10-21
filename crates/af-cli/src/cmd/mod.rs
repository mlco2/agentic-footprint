pub mod debug_frames;
pub mod debug_server;
pub mod doctor;
pub mod estimator;
pub mod estimator_worker;
pub mod hook;
pub mod http;
pub mod ingest;
#[cfg(feature = "experimental-opencode")]
pub mod opencode;
pub mod python;
pub mod python_upgrade;
pub mod report;
pub mod self_update;
pub mod service;
pub mod setup;
#[cfg(test)]
pub(crate) mod test_support;
pub mod validate_line;
pub mod watch;

use std::path::{Path, PathBuf};

use af_store::Store;
use anyhow::{Context, Result};

/// Compile-time location of this repository's Python sidecar sources. The
/// PoC ships no installable sidecar package, so a `cargo`-built `af` finds
/// its Python next to its own source tree.
const REPO_PYTHON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../python");

/// The OTLP HTTP/JSON logs endpoint `af setup` configures agents with and
/// `af watch` serves by default — one value, so setup, the service and
/// doctor cannot each assume a different one.
pub const DEFAULT_LOGS_ENDPOINT: &str = "http://127.0.0.1:4318/v1/logs";

/// Fallback electricity-mix zone: ecologits' world-average mix. Used only
/// when nothing else declares one, and recorded as `source: "default"` on
/// every join so a defaulted figure is never mistaken for a declared one.
const DEFAULT_ZONE: &str = "WOR";

/// Epoch milliseconds now. The one clock reading the control plane makes,
/// so a frame's `at_ms` and a control-plane-minted timestamp cannot come
/// from two different notions of "now".
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The current UTC instant as an RFC 3339 string with millisecond
/// precision — the same shape (`…T…:…:….123Z`) every collector emits, so a
/// control-plane-generated timestamp sorts and parses exactly like a
/// collected one.
///
/// The formatting itself is [`af_core::rfc3339_ms`], which is also what
/// derived records are stamped with: two hand-rolled formatters that agreed
/// today are two that can disagree tomorrow.
pub fn now_rfc3339() -> String {
    af_core::rfc3339_ms(now_ms()).unwrap_or_default()
}

/// Replaces `path` with `bytes` atomically and durably: a fresh temp file
/// beside it (keeping the existing file's permissions, `0600` for a new
/// one), fsynced, renamed over `path`, and on unix the directory fsynced so
/// the rename itself survives a crash. Creates the parent directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create directory {}", parent.display()))?;
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create temporary file {}", temporary.display()))?;
        keep_permissions(&temporary, path)?;
        file.write_all(bytes)
            .with_context(|| format!("write temporary file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", temporary.display()))?;
        std::fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
        // Unix durability idiom only: fsyncing the directory makes the rename
        // itself durable. Windows can't open a directory as a `File` (the
        // call fails with PermissionDenied) and offers no equivalent through
        // safe std, so the rename is as far as durability goes there.
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("sync directory {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn keep_permissions(temporary: &Path, existing: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(existing)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0o600);
    std::fs::set_permissions(temporary, std::fs::Permissions::from_mode(mode & 0o777))
        .with_context(|| format!("set permissions {}", temporary.display()))
}

#[cfg(not(unix))]
fn keep_permissions(_temporary: &Path, _existing: &Path) -> Result<()> {
    Ok(())
}

/// The Python sidecar scripts this binary ships, by path under
/// `state_dir/python`.
const SIDECAR_SOURCES: &[(&str, &str)] = &[
    (
        "af_sampler/__main__.py",
        include_str!("../../../../python/af_sampler/__main__.py"),
    ),
    (
        "af_estimator/__main__.py",
        include_str!("../../../../python/af_estimator/__main__.py"),
    ),
];

/// Locates one Python sidecar script (`af_sampler/__main__.py`,
/// `af_estimator/__main__.py`) under `state_dir`, first making the
/// installed copy this binary's own: whoever spawns a sidecar gets the
/// scripts that match the running `af`, whatever installed or updated it.
///
/// When the state dir cannot be written, the in-repo copy is the fallback
/// (a `cargo`-built `af`). `None` when neither exists — every caller here
/// treats a missing sidecar as a degraded mode with a stated reason, never
/// as an error.
pub fn sidecar_script(state_dir: &Path, subpath: &str) -> Option<PathBuf> {
    let installed = state_dir.join("python").join(subpath);
    if let Some((_, source)) = SIDECAR_SOURCES.iter().find(|(path, _)| *path == subpath) {
        let current = std::fs::read_to_string(&installed).ok();
        if current.as_deref() == Some(*source)
            || write_atomic(&installed, source.as_bytes()).is_ok()
        {
            return Some(installed);
        }
    } else if installed.is_file() {
        return Some(installed);
    }
    let repo = PathBuf::from(REPO_PYTHON).join(subpath);
    repo.is_file().then_some(repo)
}

/// Installs every sidecar script this binary ships under `state_dir`.
pub fn install_sidecar_scripts(state_dir: &Path) -> Result<()> {
    for (subpath, _) in SIDECAR_SOURCES {
        sidecar_script(state_dir, subpath)
            .with_context(|| format!("install sidecar script {subpath}"))?;
    }
    Ok(())
}

/// Resolves the local machine grid zone for one pass, most explicit first:
/// the `--local-grid-zone`/`--zone` flag, `$AF_LOCAL_GRID_ZONE`, `$AF_ZONE`,
/// a `session_meta.geo_zone` the collectors
/// actually recorded, and finally [`DEFAULT_ZONE`].
///
/// Contract #1 keeps `geo_zone` user-configured and never auto-detected, so
/// a declared zone is a real signal. Sessions may disagree; the
/// lexicographically first is used (deterministically) and the conflict is
/// reported, rather than one session's zone being silently applied to
/// another's numbers without a word.
///
/// The resolved zone governs the *local* half of every join immediately.
/// Stored remote estimates keep the zone they were computed under until
/// `af replay` re-runs them; the mismatch is warned about rather than
/// hidden (see `af_core::rebuild_derived`).
///
/// Shared by `af report`, `af replay` and `af watch`. `af watch` used to
/// carry its own copy that defaulted to a hard-coded `"WOR"` and said
/// nothing about a conflict, so a resident watch and a report over the same
/// store could pick different zones and only one of them would admit it.
pub fn resolve_zone(store: &Store, flag: Option<&str>) -> Result<(String, String)> {
    if let Some(zone) = flag {
        return Ok((zone.to_string(), "flag".to_string()));
    }
    if let Ok(zone) = std::env::var("AF_LOCAL_GRID_ZONE") {
        if !zone.is_empty() {
            return Ok((zone, "env".to_string()));
        }
    }
    if let Ok(zone) = std::env::var("AF_ZONE") {
        if !zone.is_empty() {
            return Ok((zone, "env".to_string()));
        }
    }
    let declared = store.declared_geo_zones()?;
    if declared.len() > 1 {
        eprintln!(
            "af: sessions declare {} different geo_zones ({}); using {} for the whole pass — \
             pass --zone to choose",
            declared.len(),
            declared.join(", "),
            declared[0],
        );
    }
    match declared.into_iter().next() {
        Some(zone) => Ok((zone, "session_meta".to_string())),
        None => Ok((DEFAULT_ZONE.to_string(), "default".to_string())),
    }
}

/// Resolves the optional remote inference region override. With no explicit
/// value the estimator owns region detection; the local machine zone is never
/// forwarded as a remote-region guess.
pub fn resolve_remote_region(flag: Option<&str>) -> af_core::EstimationRegion {
    if let Some(region) = flag.filter(|value| !value.is_empty()) {
        return af_core::EstimationRegion::explicit(region, "flag");
    }
    if let Ok(region) = std::env::var("AF_REMOTE_REGION") {
        if !region.is_empty() {
            return af_core::EstimationRegion::explicit(region, "env");
        }
    }
    af_core::EstimationRegion::automatic()
}

#[cfg(test)]
mod tests {
    /// The scripts belong to the binary: whoever spawns a sidecar gets the
    /// running af's own copy, whatever an older af left in the state dir.
    #[test]
    fn sidecar_script_replaces_a_stale_installed_copy_with_this_binarys() {
        let state = tempfile::tempdir().unwrap();
        let stale = state.path().join("python/af_estimator/__main__.py");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "# from an older af\n").unwrap();
        let script = super::sidecar_script(state.path(), "af_estimator/__main__.py").unwrap();
        assert_eq!(script, stale);
        assert!(std::fs::read_to_string(&script)
            .unwrap()
            .contains("def handle_version"));
    }

    #[test]
    fn timestamps_are_rfc3339_with_milliseconds() {
        assert_eq!(
            af_core::rfc3339_ms(1_784_000_000_007).as_deref(),
            Some("2026-07-14T03:33:20.007Z"),
        );
    }
}
