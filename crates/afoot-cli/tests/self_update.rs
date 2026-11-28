//! `afoot self update` and its install receipt, against a local fake release
//! server — no network, no real GitHub.
mod common;

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::OnceLock;

use assert_cmd::Command;
#[cfg(unix)]
use common::fake_server;
use predicates::prelude::*;

#[test]
fn record_install_writes_a_receipt_for_this_binary() {
    let state = tempfile::tempdir().unwrap();
    let binary = assert_cmd::cargo::cargo_bin("afoot");
    Command::cargo_bin("afoot")
        .unwrap()
        .env("AFOOT_STATE_DIR", state.path())
        .args(["self", "record-install", "--binary"])
        .arg(&binary)
        .assert()
        .success();
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.path().join("install-receipt.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["source"], "github:mlco2/agentic-footprint");
    assert_eq!(receipt["binary"], binary.to_string_lossy().as_ref());
    assert!(receipt["target"].as_str().unwrap().contains('-'));
}

#[cfg(unix)]
struct Install {
    _dir: tempfile::TempDir,
    state: PathBuf,
    binary: PathBuf,
}

/// A release-style install: a copy of the test `afoot` plus a receipt for it.
#[cfg(unix)]
fn install(with_receipt: bool) -> Install {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let binary = bin.join("afoot");
    // A hard link, not a 40 MB copy: self update renames over the path and
    // never writes into the file, so the shared cargo binary is untouched.
    let built = assert_cmd::cargo::cargo_bin("afoot");
    std::fs::hard_link(&built, &binary)
        .or_else(|_| std::fs::copy(&built, &binary).map(|_| ()))
        .unwrap();
    if with_receipt {
        Command::new(&binary)
            .env("AFOOT_STATE_DIR", &state)
            .args(["self", "record-install", "--binary"])
            .arg(&binary)
            .assert()
            .success();
    }
    Install {
        _dir: dir,
        state,
        binary,
    }
}

/// This build's release target, as the binary itself records it.
#[cfg(unix)]
fn target() -> &'static str {
    static TARGET: OnceLock<String> = OnceLock::new();
    TARGET.get_or_init(|| {
        let state = tempfile::tempdir().unwrap();
        Command::cargo_bin("afoot")
            .unwrap()
            .env("AFOOT_STATE_DIR", state.path())
            .args(["self", "record-install", "--binary", "afoot"])
            .assert()
            .success();
        let receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(state.path().join("install-receipt.json")).unwrap(),
        )
        .unwrap();
        receipt["target"].as_str().unwrap().to_string()
    })
}

#[cfg(unix)]
const NEW_BINARY: &[u8] = b"#!/bin/sh\necho 'afoot 9.9.9'\n";

#[cfg(unix)]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
fn release_routes(tag: &str, asset: &[u8], sums_digest: &str) -> Vec<(&'static str, u16, Vec<u8>)> {
    let name = format!("afoot-{}", target());
    let release = serde_json::json!({
        "tag_name": tag,
        "draft": false,
        "prerelease": false,
        "assets": [
            {"name": name, "browser_download_url": "{base}/download/afoot"},
            {"name": "SHA256SUMS", "browser_download_url": "{base}/download/SHA256SUMS"},
            {"name": format!("{name}.sigstore.json"), "browser_download_url": "{base}/download/bundle"},
        ]
    });
    vec![
        (
            "/repos/mlco2/agentic-footprint/releases/latest",
            200,
            release.to_string().into_bytes(),
        ),
        ("/download/afoot", 200, asset.to_vec()),
        ("/download/bundle", 200, b"{}".to_vec()),
        (
            "/download/SHA256SUMS",
            200,
            format!("{sums_digest}  {name}\n").into_bytes(),
        ),
    ]
}

/// A fake release server cannot produce a sigstore signature for a test
/// binary (that takes a real OIDC identity), so tests about what happens
/// *after* verification pass this; the signature check itself is unit-tested
/// against a real bundle in `cmd::self_update`.
#[cfg(unix)]
const UNSIGNED: &str = "--allow-unsigned";

#[cfg(unix)]
fn self_update(install: &Install, base: &str, extra: &[&str]) -> assert_cmd::assert::Assert {
    Command::new(&install.binary)
        .env("AFOOT_STATE_DIR", &install.state)
        .env("AFOOT_UPDATE_BASE_URL", base)
        .env("PATH", "/usr/bin:/bin")
        .env("AFOOT_SERVICE_MANAGER", "unsupported")
        .env_remove("GITHUB_TOKEN")
        .args(["self", "update", "--no-python"])
        .args(extra)
        .assert()
}

#[cfg(unix)]
#[test]
fn newer_release_replaces_the_binary_and_updates_the_receipt() {
    let install = install(true);
    let base = fake_server(release_routes(
        "v9.9.9",
        NEW_BINARY,
        &sha256_hex(NEW_BINARY),
    ));
    self_update(&install, &base, &[UNSIGNED])
        .success()
        .stdout(predicate::str::contains("v9.9.9"))
        .stdout(predicate::str::contains("checksum verified"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), NEW_BINARY);
}

/// The checksum comes from the same release as the binary, so on its own
/// it proves only that the download was not corrupted: a release whose
/// signature does not verify is refused, unless the user opts out.
#[cfg(unix)]
#[test]
fn an_unverified_signature_is_refused_unless_unsigned_is_allowed() {
    let install = install(true);
    let original = std::fs::read(&install.binary).unwrap();
    let base = fake_server(release_routes(
        "v9.9.9",
        NEW_BINARY,
        &sha256_hex(NEW_BINARY),
    ));
    self_update(&install, &base, &[])
        .failure()
        .stderr(predicate::str::contains("signature"))
        .stderr(predicate::str::contains("nothing was changed"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), original);
    assert!(!install.binary.with_extension("new").exists());

    self_update(&install, &base, &[UNSIGNED])
        .success()
        .stdout(predicate::str::contains("signature not verified"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), NEW_BINARY);
}

#[cfg(unix)]
#[test]
fn up_to_date_is_a_no_op() {
    let install = install(true);
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let base = fake_server(release_routes(&tag, NEW_BINARY, &sha256_hex(NEW_BINARY)));
    let before = std::fs::read(&install.binary).unwrap();
    self_update(&install, &base, &[])
        .success()
        .stdout(predicate::str::contains("already on"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn dry_run_changes_nothing() {
    let install = install(true);
    let base = fake_server(release_routes(
        "v9.9.9",
        NEW_BINARY,
        &sha256_hex(NEW_BINARY),
    ));
    let before = std::fs::read(&install.binary).unwrap();
    self_update(&install, &base, &["--dry-run"])
        .success()
        .stdout(predicate::str::contains("Would update afoot"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn checksum_mismatch_is_refused_and_the_binary_is_untouched() {
    let install = install(true);
    let base = fake_server(release_routes("v9.9.9", NEW_BINARY, &"0".repeat(64)));
    let before = std::fs::read(&install.binary).unwrap();
    self_update(&install, &base, &[])
        .code(1)
        .stderr(predicate::str::contains("checksum mismatch"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn without_a_receipt_self_update_refuses() {
    let install = install(false);
    let base = fake_server(vec![]);
    self_update(&install, &base, &[])
        .code(2)
        .stderr(predicate::str::contains(
            "installed by the release installer",
        ));
}

#[cfg(unix)]
#[test]
fn a_receipt_for_another_executable_refuses() {
    let install = install(true);
    let other = install.binary.with_file_name("afoot-copy");
    std::fs::copy(&install.binary, &other).unwrap();
    let base = fake_server(vec![]);
    Command::new(&other)
        .env("AFOOT_STATE_DIR", &install.state)
        .env("AFOOT_UPDATE_BASE_URL", &base)
        .args(["self", "update", "--no-python"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("multiple copies"));
}

#[cfg(unix)]
#[test]
fn api_errors_name_the_token_and_change_nothing() {
    let install = install(true);
    let base = fake_server(vec![(
        "/repos/mlco2/agentic-footprint/releases/latest",
        403,
        b"{\"message\":\"API rate limit exceeded\"}".to_vec(),
    )]);
    let before = std::fs::read(&install.binary).unwrap();
    self_update(&install, &base, &[])
        .code(1)
        .stderr(predicate::str::contains("--token"));
    assert_eq!(std::fs::read(&install.binary).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn a_release_without_this_target_is_refused() {
    let install = install(true);
    let release = serde_json::json!({
        "tag_name": "v9.9.9", "draft": false, "prerelease": false,
        "assets": [{"name": "SHA256SUMS", "browser_download_url": "{base}/download/SHA256SUMS"}]
    });
    let base = fake_server(vec![(
        "/repos/mlco2/agentic-footprint/releases/latest",
        200,
        release.to_string().into_bytes(),
    )]);
    self_update(&install, &base, &[])
        .code(1)
        .stderr(predicate::str::contains(format!("afoot-{}", target())));
}

/// The new binary does the rest: its own `python upgrade` (its floors, its
/// smoke test). The release asset here is a script logging its arguments;
/// only its `Runtime: ` summary reaches the user.
#[cfg(unix)]
#[test]
fn the_new_binary_upgrades_the_python_runtime_and_only_its_summary_is_shown() {
    let install = install(true);
    let log = install.state.join("new-afoot.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\necho '  resolving detail'\necho 'Runtime: ecologits test'\n",
        log.display()
    );
    let base = fake_server(release_routes(
        "v9.9.9",
        script.as_bytes(),
        &sha256_hex(script.as_bytes()),
    ));
    Command::new(&install.binary)
        .env("AFOOT_STATE_DIR", &install.state)
        .env("AFOOT_UPDATE_BASE_URL", &base)
        .env("PATH", "/usr/bin:/bin")
        .env("AFOOT_SERVICE_MANAGER", "unsupported")
        .args(["self", "update", UNSIGNED])
        .assert()
        .success()
        .stdout(predicate::str::contains("Runtime: ecologits test"))
        .stdout(predicate::str::contains("resolving detail").not())
        .stdout(predicate::str::contains("Restart your `afoot watch`"));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        "python upgrade"
    );
}

#[test]
fn the_python_runtime_commands_are_not_in_the_top_level_help() {
    Command::cargo_bin("afoot")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("self"))
        .stdout(predicate::str::contains("python").not());
}

/// New agent models must not wait for an afoot release: with the binary
/// already current, `afoot self update` still refreshes the runtime.
#[cfg(unix)]
#[test]
fn an_up_to_date_binary_still_refreshes_the_runtime() {
    let install = install(true);
    common::fake_runtime(&install.state);
    let (ecologits, codecarbon) = common::floors();
    let pypi = |version: &str| common::pypi_releases(&[(version, "2026-01-01T00:00:00Z")]);
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let mut routes = release_routes(&tag, NEW_BINARY, &sha256_hex(NEW_BINARY));
    routes.push(("/pypi/ecologits/json", 200, pypi(&ecologits)));
    routes.push(("/pypi/codecarbon/json", 200, pypi(&codecarbon)));
    let base = fake_server(routes);
    Command::new(&install.binary)
        .env("AFOOT_STATE_DIR", &install.state)
        .env("AFOOT_UPDATE_BASE_URL", &base)
        .env("AFOOT_PYPI_BASE_URL", &base)
        .env("PATH", "/usr/bin:/bin")
        .env("AFOOT_SERVICE_MANAGER", "unsupported")
        .args(["self", "update"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already on"))
        .stdout(predicate::str::contains("Runtime: "));
}

/// `~/.local/bin/afoot -> …/afoot`: the file the link points at is replaced, the
/// link stays a link, and the next update still matches the receipt.
#[cfg(unix)]
#[test]
fn an_update_through_a_symlink_replaces_its_target() {
    let install = install(true);
    let link = install.binary.with_file_name("afoot-link");
    std::os::unix::fs::symlink(&install.binary, &link).unwrap();
    let base = fake_server(release_routes(
        "v9.9.9",
        NEW_BINARY,
        &sha256_hex(NEW_BINARY),
    ));
    Command::new(&link)
        .env("AFOOT_STATE_DIR", &install.state)
        .env("AFOOT_UPDATE_BASE_URL", &base)
        .env("PATH", "/usr/bin:/bin")
        .env("AFOOT_SERVICE_MANAGER", "unsupported")
        .args(["self", "update", "--no-python", UNSIGNED])
        .assert()
        .success();
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(&install.binary).unwrap(), NEW_BINARY);
}
