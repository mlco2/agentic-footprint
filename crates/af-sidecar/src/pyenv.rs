//! Managed-venv provisioning (`setup`) and health checks (`doctor`) via
//! `uv`. Pins come from `python/manifest.toml`, embedded at compile time
//! so the `af` binary is self-contained (no runtime file lookup).
//!
//! Layout under `state_dir`: `venv/` (created by `uv venv`), with the
//! interpreter at `venv/bin/python` (unix) or `venv\Scripts\python.exe`
//! (Windows) — see [`venv_interpreter`].

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

/// The venv's interpreter path: `bin/python` on unix, `Scripts\python.exe`
/// on Windows. The single source of truth for the layout — provisioning,
/// resolution, and doctor messages all go through here.
pub fn venv_interpreter(venv: &Path) -> PathBuf {
    #[cfg(unix)]
    return venv.join("bin").join("python");
    #[cfg(windows)]
    return venv.join("Scripts").join("python.exe");
}

/// The venv interpreter, if it exists and is executable. Returns `None`
/// for any other condition (missing venv, missing interpreter, not
/// executable) — callers that need to distinguish those should use
/// [`doctor`] instead.
pub fn venv_python(state_dir: &Path) -> Option<PathBuf> {
    let python = venv_interpreter(&venv_dir(state_dir));
    let meta = std::fs::metadata(&python).ok()?;
    if !meta.is_file() {
        return None;
    }
    if !is_executable(&python, &meta) {
        return None;
    }
    Some(python)
}

/// Whether the interpreter can actually be executed. On unix that is a
/// mode-bit check; on Windows there are no execute bits, so the honest
/// equivalent is the `.exe` extension — and [`venv_interpreter`] only ever
/// hands this a `python.exe` path, so no other launcher extension needs
/// recognizing.
#[cfg(unix)]
fn is_executable(_path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable(path: &Path, _meta: &std::fs::Metadata) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

fn uv_on_path() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The ecologits/codecarbon versions a managed venv runs. The manifest's
/// pins are **floors**: the versions this binary was tested with, below
/// which nothing is installed; `af python upgrade` moves above them. The
/// venv itself is the record of what is installed (see
/// [`installed_versions`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeVersions {
    pub ecologits: String,
    pub codecarbon: String,
}

pub fn floors() -> RuntimeVersions {
    let manifest = manifest();
    RuntimeVersions {
        ecologits: manifest.packages.ecologits,
        codecarbon: manifest.packages.codecarbon,
    }
}

/// Numeric `X.Y.Z…` comparison key; `None` for anything else (a
/// prerelease like `0.11.4rc1` does not parse, which is how prereleases
/// are excluded).
pub fn version_key(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// Prints the installed ecologits and codecarbon versions, one per line.
const VERSIONS_SCRIPT: &str =
    "import importlib.metadata as m; print(m.version('ecologits')); print(m.version('codecarbon'))";

fn parse_versions(stdout: &[u8]) -> Option<RuntimeVersions> {
    let stdout = std::str::from_utf8(stdout).ok()?;
    let mut lines = stdout.lines().map(str::trim);
    let mut next = || {
        lines
            .next()
            .filter(|v| version_key(v).is_some())
            .map(str::to_string)
    };
    Some(RuntimeVersions {
        ecologits: next()?,
        codecarbon: next()?,
    })
}

/// What the venv at `venv` really has installed, asked of its own
/// interpreter; `None` when there is no venv or it cannot answer.
pub fn installed_versions(venv: &Path) -> Option<RuntimeVersions> {
    let output = Command::new(venv_interpreter(venv))
        .args(["-c", VERSIONS_SCRIPT])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_versions(&output.stdout))
        .flatten()
}

/// Whether every installed version is at least this binary's floor.
pub fn meets_floors(versions: &RuntimeVersions) -> bool {
    let floors = floors();
    [
        (&versions.ecologits, &floors.ecologits),
        (&versions.codecarbon, &floors.codecarbon),
    ]
    .iter()
    .all(|(version, floor)| {
        matches!((version_key(version), version_key(floor)), (Some(v), Some(f)) if v >= f)
    })
}

pub fn requirements_for(versions: &RuntimeVersions) -> Vec<String> {
    vec![
        requirement("ecologits", &versions.ecologits),
        requirement("codecarbon", &versions.codecarbon),
        requirement("psutil", &manifest().packages.psutil),
    ]
}

/// Provisions the managed venv at the versions it already has (when they
/// meet the floors), else at the floors: a re-setup never downgrades what
/// `af python upgrade` installed, and never stays below this binary's pins.
pub fn setup(state_dir: &Path) -> Result<()> {
    let venv = venv_dir(state_dir);
    let versions = installed_versions(&venv)
        .filter(meets_floors)
        .unwrap_or_else(floors);
    provision(&venv, &requirements_for(&versions), None)
}

/// Creates (or refreshes) the venv at `venv` with exactly `requirements`,
/// with `uv` when available, otherwise with a matching system Python's
/// standard-library `venv` and `pip` modules. `exclude_newer` (RFC 3339)
/// cools transitive dependencies too, and is only honored with uv; pip has
/// no equivalent.
pub fn provision(venv: &Path, requirements: &[String], exclude_newer: Option<&str>) -> Result<()> {
    let manifest = manifest();
    if uv_on_path() {
        return setup_with_uv(&manifest, venv, requirements, exclude_newer);
    }
    setup_with_stdlib(&manifest, venv, requirements)
}

fn setup_with_uv(
    manifest: &Manifest,
    venv: &Path,
    requirements: &[String],
    exclude_newer: Option<&str>,
) -> Result<()> {
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

    let venv_python_path = venv_interpreter(venv);
    let install_status = Command::new("uv")
        .args(["pip", "install", "--python"])
        .arg(&venv_python_path)
        .args(
            exclude_newer
                .map(|cutoff| ["--exclude-newer", cutoff])
                .into_iter()
                .flatten(),
        )
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

/// System-interpreter candidates for the stdlib fallback, as
/// `(program, leading args)` pairs — the Windows `py` launcher selects the
/// version through an argument (`py -3.12`), not the program name.
fn stdlib_python_candidates(version: &str) -> Vec<(String, Vec<String>)> {
    #[cfg(unix)]
    return vec![
        (format!("python{version}"), vec![]),
        ("python3".to_string(), vec![]),
    ];
    #[cfg(windows)]
    return vec![
        ("py".to_string(), vec![format!("-{version}")]),
        ("python".to_string(), vec![]),
    ];
}

fn setup_with_stdlib(manifest: &Manifest, venv: &Path, requirements: &[String]) -> Result<()> {
    let candidates = stdlib_python_candidates(&manifest.python.version);
    let (python, python_args) = candidates
        .iter()
        .find(|(program, args)| {
            Command::new(program)
                .args(args)
                .args(["-c", "import venv"])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .context("neither uv nor a usable python3 with the venv module was found on PATH")?;

    let venv_status = Command::new(python)
        .args(python_args)
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

    let venv_python_path = venv_interpreter(venv);
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
/// means everything checked out healthy. See [`runtime_status`].
pub fn doctor(state_dir: &Path) -> Vec<DoctorFinding> {
    let mut findings: Vec<DoctorFinding> = runtime_status(state_dir).err().into_iter().collect();
    findings.sort_by_key(|f| f.severity);
    findings
}

/// The managed venv's installed versions, or the first thing wrong with
/// it. Each step is a precondition of the next, so there is **at most one**
/// finding — an interpreter that isn't there cannot also fail an import
/// check, and listing both would invent a second problem out of one:
///
/// 1. `state_dir/venv` exists;
/// 2. its interpreter exists and is executable;
/// 3. `import ecologits, codecarbon, psutil` succeeds and reports both
///    versions (one interpreter run);
/// 4. those versions meet this binary's floors.
pub fn runtime_status(state_dir: &Path) -> std::result::Result<RuntimeVersions, DoctorFinding> {
    let error = |message: String, fix_hint: &str| DoctorFinding {
        severity: Severity::Error,
        message,
        fix_hint: fix_hint.to_string(),
    };
    let venv = venv_dir(state_dir);
    if !venv.exists() {
        return Err(error(
            format!("venv directory missing: {}", venv.display()),
            "run `af python setup`",
        ));
    }
    let Some(python) = venv_python(state_dir) else {
        return Err(error(
            format!(
                "venv interpreter missing or not executable: {}",
                venv_interpreter(&venv).display()
            ),
            "run `af python setup` to rebuild the venv",
        ));
    };
    let output = Command::new(&python)
        .args([
            "-c",
            &format!("import ecologits, codecarbon, psutil; {VERSIONS_SCRIPT}"),
        ])
        .output()
        .map_err(|err| {
            error(
                format!("failed to execute {}: {err}", python.display()),
                "run `af python setup` to rebuild the venv",
            )
        })?;
    if !output.status.success() {
        return Err(error(
            format!(
                "import check failed under {}: {}",
                python.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "run `af python setup` to reinstall the pinned packages",
        ));
    }
    let versions = parse_versions(&output.stdout).ok_or_else(|| {
        error(
            "cannot read the installed ecologits/codecarbon versions".to_string(),
            "run `af python setup` to reinstall the pinned packages",
        )
    })?;
    if !meets_floors(&versions) {
        let floors = floors();
        return Err(error(
            format!(
                "ecologits {} / codecarbon {} is older than this af needs ({} / {})",
                versions.ecologits, versions.codecarbon, floors.ecologits, floors.codecarbon
            ),
            "run `af python setup` to install at least the pinned versions",
        ));
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_pin_the_given_versions_and_keep_psutil_open() {
        let versions = RuntimeVersions {
            ecologits: "0.11.3".into(),
            codecarbon: "3.3.1".into(),
        };
        assert_eq!(
            requirements_for(&versions),
            ["ecologits==0.11.3", "codecarbon==3.3.1", "psutil>=6"]
        );
    }

    #[test]
    fn floors_are_the_manifest_pins() {
        let floors = floors();
        assert_eq!(floors.ecologits, manifest().packages.ecologits);
        assert_eq!(floors.codecarbon, manifest().packages.codecarbon);
    }

    /// A venv whose interpreter answers the version query with `output`.
    #[cfg(unix)]
    fn fake_venv(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let venv = dir.join("venv");
        let python = venv_interpreter(&venv);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, script).unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        venv
    }

    #[cfg(unix)]
    #[test]
    fn installed_versions_are_read_from_the_venv_itself() {
        let dir = tempfile::tempdir().unwrap();
        let venv = fake_venv(dir.path(), "#!/bin/sh\nprintf '0.11.1\\n3.2.8\\n'\n");
        assert_eq!(
            installed_versions(&venv),
            Some(RuntimeVersions {
                ecologits: "0.11.1".into(),
                codecarbon: "3.2.8".into(),
            })
        );
        let broken = fake_venv(&dir.path().join("b"), "#!/bin/sh\nexit 1\n");
        assert_eq!(installed_versions(&broken), None);
        assert_eq!(installed_versions(&dir.path().join("missing")), None);
    }

    #[test]
    fn version_keys_are_numeric_and_reject_prereleases() {
        assert_eq!(version_key("0.11.10"), Some(vec![0, 11, 10]));
        assert!(version_key("0.11.10") > version_key("0.11.9"));
        assert_eq!(version_key("0.11.4rc1"), None);
    }

    #[test]
    fn manifest_parses_expected_pins() {
        let m = manifest();
        assert_eq!(m.python.version, "3.12");
        assert_eq!(m.packages.ecologits, "0.11.2");
        assert_eq!(m.packages.codecarbon, "3.3.1");
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

    #[cfg(unix)]
    #[test]
    fn venv_interpreter_uses_bin_python() {
        assert_eq!(
            venv_interpreter(Path::new("/s/venv")),
            PathBuf::from("/s/venv/bin/python")
        );
    }

    #[cfg(windows)]
    #[test]
    fn venv_interpreter_uses_scripts_python_exe() {
        assert_eq!(
            venv_interpreter(Path::new(r"C:\s\venv")),
            PathBuf::from(r"C:\s\venv")
                .join("Scripts")
                .join("python.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn venv_python_found_by_extension_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = dir.path().join("venv").join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("python.exe"), b"").unwrap();
        assert_eq!(venv_python(dir.path()), Some(scripts.join("python.exe")));
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
