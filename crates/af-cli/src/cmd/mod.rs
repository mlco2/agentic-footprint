pub mod debug_frames;
pub mod debug_server;
pub mod estimator;
pub mod estimator_worker;
pub mod ingest;
#[cfg(feature = "experimental-opencode")]
pub mod opencode;
pub mod python;
pub mod report;
pub mod service;
pub mod setup;
pub mod validate_line;
pub mod watch;

use std::path::{Path, PathBuf};

use af_store::Store;
use anyhow::Result;

/// Compile-time location of this repository's Python sidecar sources. The
/// PoC ships no installable sidecar package, so a `cargo`-built `af` finds
/// its Python next to its own source tree.
const REPO_PYTHON: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../python");

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

/// Locates one Python sidecar script (`af_sampler/__main__.py`,
/// `af_estimator/__main__.py`) under `state_dir`.
///
/// An installed copy under the state dir wins over the in-repo one, so a
/// packaged deployment never depends on a build-time source path. `None`
/// when neither exists — every caller here treats a missing sidecar as a
/// degraded mode with a stated reason, never as an error.
pub fn sidecar_script(state_dir: &Path, subpath: &str) -> Option<PathBuf> {
    let installed = state_dir.join("python").join(subpath);
    if installed.is_file() {
        return Some(installed);
    }
    let repo = PathBuf::from(REPO_PYTHON).join(subpath);
    repo.is_file().then_some(repo)
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
    #[test]
    fn timestamps_are_rfc3339_with_milliseconds() {
        assert_eq!(
            af_core::rfc3339_ms(1_784_000_000_007).as_deref(),
            Some("2026-07-14T03:33:20.007Z"),
        );
    }
}
