//! Managed-venv provisioning (`setup`) and health checks (`doctor`) via
//! `uv`. Pins come from `python/manifest.toml`, embedded at compile time
//! so the `af` binary is self-contained (no runtime file lookup).
//!
//! Layout under `state_dir`: `venv/` (created by `uv venv`), with the
//! interpreter at `venv/bin/python`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// `python/manifest.toml`, embedded at compile time. Path is relative to
/// this file (`crates/af-sidecar/src/pyenv.rs`): up to `af-sidecar/`, up
/// to `crates/`, up to the repo root, then into `python/`.
const MANIFEST_TOML: &str = include_str!("../../../python/manifest.toml");

#[derive(Deserialize)]
struct Manifest {
    python: PythonSection,
    packages: Packages,
}

#[derive(Deserialize)]
struct PythonSection {
    version: String,
}

/// Explicit fields rather than a generic map: the global constraints pin
/// sidecars to exactly these three packages ("stdlib + pinned
/// ecologits/codecarbon/psutil only"), so an unknown `[packages]` key is
/// a manifest error, not silently-ignored data — `toml`'s deny-unknown
/// default (implicit for struct fields without `#[serde(flatten)]`)
/// gives us that for free.
#[derive(Deserialize)]
struct Packages {
    ecologits: String,
    codecarbon: String,
    psutil: String,
}

fn manifest() -> Manifest {
    // Parse failure means the embedded manifest is malformed, which is a
    // build-time invariant of this crate, not a runtime condition callers
    // can recover from.
    toml::from_str(MANIFEST_TOML).expect("python/manifest.toml must parse")
}

/// Turns a manifest `(name, spec)` pair into a `uv pip install`
/// requirement string. If `spec` already starts with a comparison
/// operator (`>=6` etc.) it's used as-is; otherwise it's treated as an
/// exact pin and joined with `==` (`0.11.1` -> `ecologits==0.11.1`).
fn requirement(name: &str, spec: &str) -> String {
    if spec.starts_with(['=', '<', '>', '!', '~']) {
        format!("{name}{spec}")
    } else {
        format!("{name}=={spec}")
    }
}

fn venv_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("venv")
}

/// `state_dir/venv/bin/python`, if it exists and is executable. Returns
/// `None` for any other condition (missing venv, missing interpreter, not
/// executable) — callers that need to distinguish those should use
/// [`doctor`] instead.
pub fn venv_python(state_dir: &Path) -> Option<PathBuf> {
    let python = venv_dir(state_dir).join("bin").join("python");
    let meta = std::fs::metadata(&python).ok()?;
    if !meta.is_file() {
        return None;
    }
    if !is_executable(&meta) {
        return None;
    }
    Some(python)
}

/// Unix only, deliberately: this PoC targets macOS/Linux (see
/// `crates/af-cli/src/paths.rs`). The `#[cfg(not(unix))]` companion that
/// used to sit here answered `true` unconditionally, which is not a
/// portability shim but a permission check that always passes — on the one
/// platform where nobody would notice it was wrong. A build failure is the
/// honest outcome for a target this project does not support.
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

fn uv_on_path() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Provisions the managed venv with `uv` when available, otherwise with a
/// matching system Python's standard-library `venv` and `pip` modules.
pub fn setup(state_dir: &Path) -> Result<()> {
    let manifest = manifest();
    let venv = venv_dir(state_dir);
    let requirements = [
        requirement("ecologits", &manifest.packages.ecologits),
        requirement("codecarbon", &manifest.packages.codecarbon),
        requirement("psutil", &manifest.packages.psutil),
    ];

    if uv_on_path() {
        return setup_with_uv(&manifest, &venv, &requirements);
    }

    setup_with_stdlib(&manifest, &venv, &requirements)
}

fn setup_with_uv(manifest: &Manifest, venv: &Path, requirements: &[String]) -> Result<()> {
    let venv_status = Command::new("uv")
        .args(["venv", "--python", &manifest.python.version])
        .arg(venv)
        .status()
        .context("failed to run `uv venv`")?;
    if !venv_status.success() {
        bail!(
            "`uv venv --python {} {}` exited with {venv_status}",
            manifest.python.version,
            venv.display()
        );
    }

    let venv_python_path = venv.join("bin").join("python");
    let install_status = Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&venv_python_path)
        .args(requirements)
        .status()
        .context("failed to run `uv pip install`")?;
    if !install_status.success() {
        bail!(
            "`uv pip install --python {} {}` exited with {install_status}",
            venv_python_path.display(),
            requirements.join(" ")
        );
    }

    Ok(())
}

fn setup_with_stdlib(manifest: &Manifest, venv: &Path, requirements: &[String]) -> Result<()> {
    let candidates = [
        format!("python{}", manifest.python.version),
        "python3".to_string(),
    ];
    let python = candidates
        .iter()
        .find(|candidate| {
            Command::new(candidate)
                .args(["-c", "import venv"])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .context("neither uv nor a usable python3 with the venv module was found on PATH")?;

    let venv_status = Command::new(python)
        .args(["-m", "venv"])
        .arg(venv)
        .status()
        .with_context(|| format!("failed to run `{python} -m venv`"))?;
    if !venv_status.success() {
        bail!(
            "`{python} -m venv {}` exited with {venv_status}",
            venv.display()
        );
    }

    let venv_python_path = venv.join("bin").join("python");
    let install_status = Command::new(&venv_python_path)
        .args(["-m", "pip", "install"])
        .args(requirements)
        .status()
        .context("failed to run managed-venv pip")?;
    if !install_status.success() {
        bail!(
            "`{} -m pip install {}` exited with {install_status}",
            venv_python_path.display(),
            requirements.join(" ")
        );
    }
    Ok(())
}

/// Severity of a [`DoctorFinding`]. `Error` findings are what makes
/// `af python doctor` exit non-zero; `Warn` findings are surfaced but
/// don't fail the check. Declared `Error` before `Warn` so the derived
/// `Ord` sorts errors first ("most severe first").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorFinding {
    pub severity: Severity,
    pub message: String,
    pub fix_hint: String,
}

/// Diagnoses the managed venv under `state_dir`, returning findings
/// ordered most-severe first (`Error` before `Warn`). An empty result
/// means everything checked out healthy.
///
/// Checks, in order:
/// 1. `state_dir/venv` exists (Error if missing).
/// 2. `state_dir/venv/bin/python` exists and is executable (Error if not;
///    only checked when the venv dir itself exists).
/// 3. `import ecologits, codecarbon, psutil` succeeds under the venv
///    interpreter (Error if not; only run when step 3 passed).
pub fn doctor(state_dir: &Path) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();

    findings.extend(venv_finding(state_dir));

    findings.sort_by_key(|f| f.severity);
    findings
}

/// Steps 2–4 of [`doctor`]: the venv chain, which reports **at most one**
/// finding because each step is a precondition of the next — an
/// interpreter that isn't there cannot also fail an import check, and
/// listing both would invent a second problem out of one.
///
/// `None` means the venv is healthy.
fn venv_finding(state_dir: &Path) -> Option<DoctorFinding> {
    let venv = venv_dir(state_dir);
    if !venv.exists() {
        return Some(DoctorFinding {
            severity: Severity::Error,
            message: format!("venv directory missing: {}", venv.display()),
            fix_hint: "run `af python setup`".to_string(),
        });
    }

    let Some(python) = venv_python(state_dir) else {
        return Some(DoctorFinding {
            severity: Severity::Error,
            message: format!(
                "venv interpreter missing or not executable: {}",
                venv.join("bin").join("python").display()
            ),
            fix_hint: "run `af python setup` to rebuild the venv".to_string(),
        });
    };

    match Command::new(&python)
        .args(["-c", "import ecologits, codecarbon, psutil"])
        .output()
    {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(DoctorFinding {
            severity: Severity::Error,
            message: format!(
                "import check failed under {}: {}",
                python.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            fix_hint: "run `af python setup` to reinstall the pinned packages".to_string(),
        }),
        Err(err) => Some(DoctorFinding {
            severity: Severity::Error,
            message: format!("failed to execute {}: {err}", python.display()),
            fix_hint: "run `af python setup` to rebuild the venv".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_expected_pins() {
        let m = manifest();
        assert_eq!(m.python.version, "3.12");
        assert_eq!(m.packages.ecologits, "0.11.1");
        assert_eq!(m.packages.codecarbon, "3.2.8");
        assert_eq!(m.packages.psutil, ">=6");
    }

    #[test]
    fn requirement_formats_exact_pin_with_double_equals() {
        assert_eq!(requirement("ecologits", "0.11.1"), "ecologits==0.11.1");
        assert_eq!(requirement("codecarbon", "3.2.8"), "codecarbon==3.2.8");
    }

    #[test]
    fn requirement_passes_through_existing_operator() {
        assert_eq!(requirement("psutil", ">=6"), "psutil>=6");
        assert_eq!(requirement("foo", "~=1.2"), "foo~=1.2");
        assert_eq!(requirement("foo", "!=1.2"), "foo!=1.2");
    }

    #[test]
    fn venv_python_none_when_state_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(venv_python(dir.path()), None);
    }

    #[test]
    fn doctor_reports_error_when_venv_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let findings = doctor(dir.path());
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Error && f.message.contains("venv directory")),
            "expected a venv-missing Error finding, got: {findings:?}"
        );
    }

    #[test]
    fn doctor_reports_error_when_venv_dir_exists_but_python_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("venv").join("bin")).unwrap();
        let findings = doctor(dir.path());
        assert!(
            findings.iter().any(|f| f.severity == Severity::Error
                && f.message.contains("interpreter missing")),
            "expected an interpreter-missing Error finding, got: {findings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_reports_error_when_import_check_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("venv").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake_python = bin.join("python");
        std::fs::write(&fake_python, "#!/bin/sh\nexit 1\n").unwrap();
        let mut perms = std::fs::metadata(&fake_python).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_python, perms).unwrap();

        let findings = doctor(dir.path());
        assert!(
            findings
                .iter()
                .any(|f| f.severity == Severity::Error && f.message.contains("import check failed")),
            "expected an import-check-failed Error finding, got: {findings:?}"
        );
    }

    #[test]
    fn doctor_findings_are_sorted_most_severe_first() {
        let dir = tempfile::tempdir().unwrap();
        let findings = doctor(dir.path());
        for pair in findings.windows(2) {
            assert!(pair[0].severity <= pair[1].severity);
        }
    }
}
