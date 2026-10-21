//! `af python upgrade` against a fake PyPI (hermetic), and against the real
//! one (gated).
mod common;

use assert_cmd::Command;
use common::{fake_server, floors, pypi_releases};
use predicates::prelude::*;

/// A fake PyPI serving these ecologits and codecarbon release lists.
fn fake_pypi(ecologits: &[(&str, &str)], codecarbon: &[(&str, &str)]) -> String {
    fake_server(vec![
        ("/pypi/ecologits/json", 200, pypi_releases(ecologits)),
        ("/pypi/codecarbon/json", 200, pypi_releases(codecarbon)),
    ])
}

fn upgrade(state: &std::path::Path, base: &str, extra: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", state)
        .env("AF_PYPI_BASE_URL", base)
        .args(["python", "upgrade"])
        .args(extra)
        .assert()
}

#[test]
fn dry_run_reports_eligible_and_cooling_releases_without_installing() {
    let state = tempfile::tempdir().unwrap();
    let (ecologits, codecarbon) = floors();
    let base = fake_pypi(
        &[
            (&ecologits, "2026-01-01T00:00:00Z"),
            ("99.0.0", "2999-01-01T00:00:00Z"),
        ],
        &[
            (&codecarbon, "2026-01-01T00:00:00Z"),
            ("99.1.0", "2026-01-02T00:00:00Z"),
        ],
    );
    let output = upgrade(state.path(), &base, &["--dry-run"])
        .success()
        .stdout(predicate::str::contains(format!(
            "ecologits {ecologits} (up to date"
        )))
        .stdout(predicate::str::contains(format!(
            "codecarbon {codecarbon} -> 99.1.0"
        )))
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(
        stdout.lines().last().unwrap().starts_with("Runtime: "),
        "{stdout}"
    );
    assert!(!state.path().join("venv.next").exists());
}

/// An existing install predates this af, or a new release raised the
/// floors: what counts is what the venv really has.
#[cfg(unix)]
#[test]
fn a_venv_below_the_floors_is_upgraded_even_with_nothing_newer_on_pypi() {
    let state = tempfile::tempdir().unwrap();
    common::fake_runtime_with(state.path(), "0.0.1", "0.0.1");
    let (ecologits, codecarbon) = floors();
    let base = fake_pypi(
        &[(&ecologits, "2026-01-01T00:00:00Z")],
        &[(&codecarbon, "2026-01-01T00:00:00Z")],
    );
    upgrade(state.path(), &base, &["--dry-run"])
        .success()
        .stdout(predicate::str::contains(format!(
            "ecologits 0.0.1 -> {ecologits}"
        )))
        .stdout(predicate::str::contains(format!(
            "codecarbon 0.0.1 -> {codecarbon}"
        )));
}

/// Real packages from the real PyPI: resolve, provision venv.next, smoke
/// test with the real sidecars, swap. Minutes and network.
#[test]
#[ignore = "network: installs real ecologits/codecarbon — AF_E2E=1 cargo test -p af-cli --test python_upgrade -- --ignored"]
fn real_upgrade_provisions_smoke_tests_and_swaps() {
    assert_eq!(std::env::var("AF_E2E").as_deref(), Ok("1"), "set AF_E2E=1");
    let state = tempfile::tempdir().unwrap();
    Command::cargo_bin("af")
        .unwrap()
        .env("AF_STATE_DIR", state.path())
        .args(["python", "upgrade"])
        .timeout(std::time::Duration::from_secs(900))
        .assert()
        .success()
        .stdout(predicate::str::contains("smoke test passed"));
    assert!(state.path().join("venv").is_dir());
    assert!(!state.path().join("venv.next").exists());
}
