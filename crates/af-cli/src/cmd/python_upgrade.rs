//! `af python upgrade`: move the managed venv to the newest cooled
//! ecologits/codecarbon, smoke-tested before it replaces anything.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use af_sidecar::{RuntimeVersions, Sidecar};
use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::estimator::ESTIMATOR_SCRIPT;
use super::http;
use super::watch::SAMPLER_SCRIPT;

const DEFAULT_PYPI: &str = "https://pypi.org";
const AGENT_MODELS: &str = include_str!("../../../../tests/fixtures/models/agent-models.json");
const COOLDOWN_DAYS: i64 = 7;

/// Releases uploaded after this instant are still cooling down.
fn cooldown_cutoff(now: OffsetDateTime) -> OffsetDateTime {
    now - time::Duration::days(COOLDOWN_DAYS)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    version: String,
    uploaded: OffsetDateTime,
}

/// The highest non-prerelease, non-yanked release of a PyPI project whose
/// newest file was uploaded no later than `cutoff` and that is `>= floor`.
fn newest_cooled(
    pypi: &serde_json::Value,
    floor: &str,
    cutoff: OffsetDateTime,
) -> Option<Candidate> {
    let floor = af_sidecar::version_key(floor)?;
    pypi["releases"]
        .as_object()?
        .iter()
        .filter_map(|(version, files)| {
            let key = af_sidecar::version_key(version)?;
            let files = files.as_array()?;
            if key < floor || files.is_empty() || files.iter().any(|f| f["yanked"] == true) {
                return None;
            }
            let uploaded = files
                .iter()
                .filter_map(|f| f["upload_time_iso_8601"].as_str())
                .filter_map(|ts| OffsetDateTime::parse(ts, &Rfc3339).ok())
                .max()?;
            (uploaded <= cutoff).then(|| {
                (
                    key,
                    Candidate {
                        version: version.clone(),
                        uploaded,
                    },
                )
            })
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, candidate)| candidate)
}

/// One package's next version — the highest of what is installed, this
/// af's floor, and the newest cooled release — with its detail line.
fn resolve(
    name: &str,
    current: &str,
    floor: &str,
    pypi: &serde_json::Value,
    cutoff: OffsetDateTime,
) -> String {
    let key = af_sidecar::version_key;
    let mut next = (current.to_string(), None);
    if key(floor) > key(current) {
        next = (
            floor.to_string(),
            Some("the minimum for this af".to_string()),
        );
    }
    if let Some(pick) = newest_cooled(pypi, floor, cutoff) {
        if key(&pick.version) > key(&next.0) {
            let uploaded = pick.uploaded.format(&Rfc3339).unwrap_or_default();
            next = (pick.version, Some(format!("released {uploaded}")));
        }
    }
    match &next.1 {
        Some(reason) => println!("  {name} {current} -> {} ({reason})", next.0),
        None => println!(
            "  {name} {current} (up to date: no newer release outside the {COOLDOWN_DAYS}-day cooldown)"
        ),
    }
    next.0
}

/// Resolves, provisions `venv.next`, smoke-tests it and only then swaps it
/// in. Everything printed is detail except the last line, which starts
/// with `Runtime: ` — the one line `af self update` relays.
pub fn run(state_dir: &Path, dry_run: bool) -> Result<i32> {
    let base = std::env::var("AF_PYPI_BASE_URL").unwrap_or_else(|_| DEFAULT_PYPI.to_string());
    let agent = http::agent();
    let cutoff = cooldown_cutoff(OffsetDateTime::now_utc());
    let floors = af_sidecar::floors();
    // What the venv really has: an install from an older af, or a release
    // that raised the floors, must not look up to date below the floors.
    let installed = af_sidecar::installed_versions(&state_dir.join("venv"));
    let current = installed.clone().unwrap_or_else(|| floors.clone());

    let fetch = |name: &str| http::get_json(&agent, &format!("{base}/pypi/{name}/json"), None);
    let (ecologits, codecarbon) = std::thread::scope(|scope| {
        let ecologits = scope.spawn(|| fetch("ecologits"));
        let codecarbon = fetch("codecarbon");
        (
            ecologits.join().expect("PyPI fetch thread panicked"),
            codecarbon,
        )
    });
    let next = RuntimeVersions {
        ecologits: resolve(
            "ecologits",
            &current.ecologits,
            &floors.ecologits,
            &ecologits?,
            cutoff,
        ),
        codecarbon: resolve(
            "codecarbon",
            &current.codecarbon,
            &floors.codecarbon,
            &codecarbon?,
            cutoff,
        ),
    };
    if dry_run {
        println!("{}", summary("Runtime: would install ", &current, &next));
        return Ok(0);
    }
    if installed.as_ref() == Some(&next) {
        println!("{}", summary("Runtime: ", &current, &next));
        return Ok(0);
    }

    let staging = clear_next(state_dir)?;
    af_sidecar::provision(
        &staging,
        &af_sidecar::requirements_for(&next),
        Some(&cutoff.format(&Rfc3339)?),
    )
    .context("provision the new runtime (the current one is untouched)")?;
    if let Err(error) = smoke_test(state_dir, &staging, &next.ecologits) {
        let _ = clear_next(state_dir);
        return Err(
            error.context("the new runtime failed its smoke test; the current one is untouched")
        );
    }
    println!("  smoke test passed");
    swap(state_dir)?;
    println!("{}", summary("Runtime: ", &current, &next));
    Ok(0)
}

/// `<prefix>ecologits 0.11.2 -> 0.11.3, codecarbon up to date`.
fn summary(prefix: &str, from: &RuntimeVersions, to: &RuntimeVersions) -> String {
    if from == to {
        return format!(
            "{prefix}ecologits {}, codecarbon {} (up to date)",
            to.ecologits, to.codecarbon
        );
    }
    let part = |name: &str, from: &str, to: &str| {
        if from == to {
            format!("{name} up to date")
        } else {
            format!("{name} {from} -> {to}")
        }
    };
    format!(
        "{prefix}{}, {}",
        part("ecologits", &from.ecologits, &to.ecologits),
        part("codecarbon", &from.codecarbon, &to.codecarbon)
    )
}

/// Removes a `venv.next` left by an interrupted upgrade; returns its path.
fn clear_next(state_dir: &Path) -> Result<PathBuf> {
    let next = state_dir.join("venv.next");
    if next.exists() {
        std::fs::remove_dir_all(&next).with_context(|| format!("remove {}", next.display()))?;
    }
    Ok(next)
}

/// `venv` -> `venv.prev`, `venv.next` -> `venv`; restores `venv` if the
/// second rename fails.
fn swap(state_dir: &Path) -> Result<()> {
    let current = state_dir.join("venv");
    let previous = state_dir.join("venv.prev");
    let next = state_dir.join("venv.next");
    if previous.exists() {
        std::fs::remove_dir_all(&previous)
            .with_context(|| format!("remove {}", previous.display()))?;
    }
    let had_current = current.exists();
    if had_current {
        std::fs::rename(&current, &previous).with_context(|| {
            format!(
                "move {} aside (on Windows, stop `af watch` first)",
                current.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(&next, &current) {
        if had_current {
            let _ = std::fs::rename(&previous, &current);
        }
        return Err(error).with_context(|| format!("install {}", next.display()));
    }
    Ok(())
}

/// Runs both real sidecars from `venv` before it replaces anything.
fn smoke_test(state_dir: &Path, venv: &Path, expected_ecologits: &str) -> Result<()> {
    let python = af_sidecar::venv_interpreter(venv);
    let script = |subpath| {
        super::sidecar_script(state_dir, subpath)
            .with_context(|| format!("sidecar script {subpath} is unavailable"))
    };
    smoke_estimator(&python, &script(ESTIMATOR_SCRIPT)?, expected_ecologits)?;
    smoke_sampler(
        &python,
        &script(SAMPLER_SCRIPT)?,
        &state_dir.join("tmp/smoke"),
    )
}

/// The estimator reports `expected_ecologits` and estimates every model
/// the agents emit.
fn smoke_estimator(python: &Path, script: &Path, expected_ecologits: &str) -> Result<()> {
    let mut sidecar = Sidecar::spawn(python, &script.to_string_lossy(), &[])
        .context("spawn the estimator from the new runtime")?;
    let version = sidecar.request(&serde_json::json!({"op": "version"}))?;
    anyhow::ensure!(
        version["methodology"]["ecologits_version"] == expected_ecologits,
        "estimator reports {version}, expected ecologits {expected_ecologits}"
    );
    let models: serde_json::Value = serde_json::from_str(AGENT_MODELS)?;
    for entry in models["models"]
        .as_array()
        .context("agent-models.json models")?
    {
        let response = sidecar.request(&serde_json::json!({
            "op": "estimate",
            "provider": entry["provider"],
            "model_name": entry["model"],
            "output_token_count": 100,
            "electricity_mix_zone": "WOR",
        }))?;
        anyhow::ensure!(
            response["status"] == "ok",
            "ecologits {expected_ecologits} cannot estimate {}/{}: {response}",
            entry["provider"],
            entry["model"]
        );
    }
    Ok(())
}

/// The codecarbon sampler writes an energy sample within 60 s (its import
/// and hardware probe alone can take most of that).
fn smoke_sampler(python: &Path, script: &Path, scratch: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(scratch);
    std::fs::create_dir_all(scratch)?;
    let scratch_arg = scratch.to_string_lossy();
    let mut sampler = Sidecar::spawn(
        python,
        &script.to_string_lossy(),
        &[
            "--state-dir",
            &scratch_arg,
            "--session",
            "smoke",
            "--interval",
            "1",
        ],
    )
    .context("spawn the sampler from the new runtime")?;
    let spool = scratch
        .join("spool")
        .join(af_spool::spool_file_name("codecarbon", "smoke"));
    let has_sample = || {
        std::fs::read_to_string(&spool).is_ok_and(|text| {
            text.lines().any(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .is_ok_and(|event| event["type"] == "energy_sample")
            })
        })
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let sampled = loop {
        if has_sample() {
            break true;
        }
        if Instant::now() > deadline || matches!(sampler.try_wait(), Ok(Some(_))) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let _ = sampler.send(&serde_json::json!({"op": "shutdown"}));
    drop(sampler);
    let _ = std::fs::remove_dir_all(scratch);
    anyhow::ensure!(
        sampled,
        "the codecarbon sampler wrote no energy sample within 60 s"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/pypi-ecologits.json")).unwrap()
    }

    fn at(ts: &str) -> OffsetDateTime {
        OffsetDateTime::parse(ts, &Rfc3339).unwrap()
    }

    #[test]
    fn swap_keeps_the_previous_venv_and_installs_the_next() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("venv")).unwrap();
        std::fs::write(dir.path().join("venv/marker"), "old").unwrap();
        std::fs::create_dir_all(dir.path().join("venv.next")).unwrap();
        std::fs::write(dir.path().join("venv.next/marker"), "new").unwrap();
        std::fs::create_dir_all(dir.path().join("venv.prev")).unwrap();
        swap(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("venv/marker")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("venv.prev/marker")).unwrap(),
            "old"
        );
        assert!(!dir.path().join("venv.next").exists());
    }

    #[test]
    fn swap_without_a_current_venv_just_installs_the_next() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("venv.next")).unwrap();
        swap(dir.path()).unwrap();
        assert!(dir.path().join("venv").is_dir());
        assert!(!dir.path().join("venv.prev").exists());
    }

    #[test]
    fn a_leftover_next_venv_is_cleared_before_provisioning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("venv.next/stale")).unwrap();
        clear_next(dir.path()).unwrap();
        assert!(!dir.path().join("venv.next").exists());
    }

    #[test]
    fn summary_is_one_runtime_line() {
        let from = af_sidecar::RuntimeVersions {
            ecologits: "0.11.2".into(),
            codecarbon: "3.3.1".into(),
        };
        let mut to = from.clone();
        to.ecologits = "0.11.3".into();
        assert_eq!(
            summary("Runtime: ", &from, &to),
            "Runtime: ecologits 0.11.2 -> 0.11.3, codecarbon up to date"
        );
        assert_eq!(
            summary("Runtime: ", &from, &from),
            "Runtime: ecologits 0.11.2, codecarbon 3.3.1 (up to date)"
        );
    }

    #[test]
    fn picks_the_highest_release_older_than_the_cooldown() {
        // 0.11.3 is 2.5 days old: skipped. Prereleases, yanked releases and
        // releases without files never count.
        let pick = newest_cooled(
            &fixture(),
            "0.11.1",
            cooldown_cutoff(at("2026-10-17T06:00:00Z")),
        )
        .unwrap();
        assert_eq!(pick.version, "0.11.2");
    }

    #[test]
    fn a_release_becomes_eligible_exactly_at_the_cooldown() {
        let pick = newest_cooled(
            &fixture(),
            "0.11.1",
            cooldown_cutoff(at("2026-10-21T18:38:48Z")),
        )
        .unwrap();
        assert_eq!(pick.version, "0.11.3");
    }

    #[test]
    fn nothing_eligible_above_the_floor_is_none() {
        assert_eq!(
            newest_cooled(
                &fixture(),
                "0.11.3",
                cooldown_cutoff(at("2026-10-17T06:00:00Z"))
            ),
            None
        );
    }

    #[test]
    fn the_floor_itself_is_eligible() {
        let pick = newest_cooled(
            &fixture(),
            "0.11.2",
            cooldown_cutoff(at("2026-10-17T06:00:00Z")),
        )
        .unwrap();
        assert_eq!(pick.version, "0.11.2");
    }
}
