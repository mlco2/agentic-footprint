//! Managed-venv provisioning (`setup`) and health checks (`doctor`) via
//! `uv`. Pins come from `python/manifest.toml`, embedded at compile time
//! so the `afoot` binary is self-contained (no runtime file lookup).
//!
//! Layout under `state_dir`: `venv/` (created by `uv venv`), with the
//! interpreter at `venv/bin/python` (unix) or `venv\Scripts\python.exe`
//! (Windows) — see [`venv_interpreter`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// `python/manifest.toml`, embedded at compile time. Path is relative to
/// this file (`crates/afoot-sidecar/src/pyenv.rs`): up to `afoot-sidecar/`, up
/// to `crates/`, up to the repo root, then into `python/`.
const MANIFEST_TOML: &str = include_str!("../../../python/manifest.toml");

#[derive(Deserialize)]
struct Manifest {
    python: PythonSection,
    packages: Packages,
    /// Security floors for packages the three above bring with them:
    /// name to the lowest version without a known vulnerability.
    #[serde(default)]
    minimums: BTreeMap<String, String>,
}

/// The first installed package below its security floor, as
/// `(name, installed, floor)`, read from the `name version` lines the
/// version check prints after the two runtime versions.
fn below_minimum(
    stdout: &str,
    minimums: &BTreeMap<String, String>,
) -> Option<(String, String, String)> {
    stdout.lines().find_map(|line| {
        let (name, installed) = line.trim().split_once(' ')?;
        let floor = minimums.get(name)?;
        let below = match (version_key(installed), version_key(floor)) {
            (Some(have), Some(need)) => have < need,
            // A version that cannot be compared is not vouched for.
            _ => true,
        };
        below.then(|| (name.to_string(), installed.to_string(), floor.clone()))
    })
}

/// Python that prints `name version` for each package with a security
/// floor that is installed (one that is not installed has nothing to fix).
fn minimums_script(minimums: &BTreeMap<String, String>) -> String {
    let names: Vec<String> = minimums.keys().map(|name| format!("{name:?}")).collect();
    format!(
        "\nfor n in [{}]:\n    try:\n        print(n, m.version(n))\n    except m.PackageNotFoundError:\n        pass\n",
        names.join(", ")
    )
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

/// What changes when the managed runtime does: the modification times of
/// the venv, its `pyvenv.cfg`, its interpreter and its `site-packages`
/// directories (an install or removal adds or drops entries there). Equal
/// fingerprints mean [`doctor`] would answer the same, without starting
/// Python to ask.
pub fn environment_fingerprint(state_dir: &Path) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    let venv = venv_dir(state_dir);
    let mut paths = vec![
        venv.clone(),
        venv.join("pyvenv.cfg"),
        venv_interpreter(&venv),
    ];
    #[cfg(windows)]
    paths.push(venv.join("Lib").join("site-packages"));
    #[cfg(unix)]
    if let Ok(entries) = std::fs::read_dir(venv.join("lib")) {
        let mut found: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join("site-packages"))
            .collect();
        found.sort();
        paths.extend(found);
    }
    paths
        .into_iter()
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok();
            (path, modified)
        })
        .collect()
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

/// An interpreter invocation that cannot pick up code or configuration from
/// wherever `afoot` was started: `-I` keeps the working directory and the
/// script's directory off `sys.path` and ignores `PYTHON*` variables and the
/// user site, and the process runs from `workdir`, a directory afoot owns —
/// so neither a project's `ecologits.py` nor its `.codecarbon.config` is
/// ever read.
pub fn isolated_python(python: &Path, workdir: &Path) -> Command {
    let mut command = Command::new(python);
    command.arg("-I").current_dir(workdir);
    #[cfg(windows)]
    if WINDOWLESS.load(std::sync::atomic::Ordering::Relaxed) {
        // CREATE_NO_WINDOW: a process that has no console (the scheduled
        // task's `afoot watch`) would otherwise get a new, visible one for
        // each interpreter it starts.
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(windows)]
static WINDOWLESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// From now on, start interpreters without a console window (Windows; a
/// no-op elsewhere). For an `afoot` that runs with no console of its own.
pub fn spawn_without_windows() {
    #[cfg(windows)]
    WINDOWLESS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// `uv` with no configuration discovery: `uv.toml` and `[tool.uv]` are read
/// from the working directory and its parents, which a project controls
/// (an index URL there would choose where the managed runtime comes from).
/// Environment variables (`UV_INDEX_URL`, a user's mirror) still apply.
fn uv(workdir: &Path) -> Command {
    let mut command = Command::new("uv");
    command.arg("--no-config").current_dir(workdir);
    command
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
/// which nothing is installed; `afoot python upgrade` moves above them. The
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
    let output = isolated_python(&venv_interpreter(venv), venv.parent().unwrap_or(venv))
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
    let manifest = manifest();
    let mut requirements = vec![
        requirement("ecologits", &versions.ecologits),
        requirement("codecarbon", &versions.codecarbon),
        requirement("psutil", &manifest.packages.psutil),
    ];
    // Named, so that an environment that already has an older one is
    // upgraded: an installer leaves a satisfied dependency alone.
    requirements.extend(
        manifest
            .minimums
            .iter()
            .map(|(name, floor)| format!("{name}>={floor}")),
    );
    requirements
}

/// Provisions the managed venv at the versions it already has (when they
/// meet the floors), else at the floors: a re-setup never downgrades what
/// `afoot python upgrade` installed, and never stays below this binary's pins.
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
    // Every tool runs from the venv's parent, a directory afoot owns (see
    // [`isolated_python`]).
    let workdir = venv.parent().unwrap_or(venv);
    std::fs::create_dir_all(workdir).with_context(|| format!("creating {}", workdir.display()))?;
    if uv_on_path() {
        return setup_with_uv(&manifest, venv, workdir, requirements, exclude_newer);
    }
    setup_with_stdlib(&manifest, venv, workdir, requirements)
}

fn setup_with_uv(
    manifest: &Manifest,
    venv: &Path,
    workdir: &Path,
    requirements: &[String],
    exclude_newer: Option<&str>,
) -> Result<()> {
    let venv_status = uv(workdir)
        .args([
            "venv",
            "--allow-existing",
            "--python",
            &manifest.python.version,
        ])
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
    let install_status = uv(workdir)
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

/// Where uv's own install instructions live.
pub const UV_INSTALL_URL: &str = "https://docs.astral.sh/uv/getting-started/installation/";

/// What `afoot python setup` says when it has nothing to build the runtime
/// with. uv is the recommendation rather than "install Python": it brings
/// the right Python itself, so it is the one thing to install.
pub const NO_PROVISIONER: &str = "neither uv nor a usable Python with the venv module was found on PATH; \
     install uv (https://docs.astral.sh/uv/getting-started/installation/), which also provides Python, \
     then run `afoot python setup` again";

/// What `afoot python setup` would build the runtime with on this machine:
/// `"uv"`, else `"python"` (a system interpreter with `venv`), else nothing.
pub fn provisioner() -> Option<&'static str> {
    if uv_on_path() {
        return Some("uv");
    }
    let version = manifest().python.version;
    stdlib_python_candidates(&version)
        .iter()
        .any(|(program, args)| {
            Command::new(program)
                .args(args)
                .args(["-I", "-c", "import venv"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .then_some("python")
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

fn setup_with_stdlib(
    manifest: &Manifest,
    venv: &Path,
    workdir: &Path,
    requirements: &[String],
) -> Result<()> {
    let candidates = stdlib_python_candidates(&manifest.python.version);
    let (python, python_args) = candidates
        .iter()
        .find(|(program, args)| {
            Command::new(program)
                .current_dir(workdir)
                .args(args)
                .args(["-I", "-c", "import venv"])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .context(NO_PROVISIONER)?;

    let venv_status = Command::new(python)
        .current_dir(workdir)
        .args(python_args)
        .args(["-I", "-m", "venv"])
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
    let install_status = isolated_python(&venv_python_path, workdir)
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
/// `afoot python doctor` exit non-zero; `Warn` findings are surfaced but
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

/// Diagnoses the managed venv under `state_dir`: at most one finding (see
/// [`runtime_status`]), and an empty result means everything checked out
/// healthy.
pub fn doctor(state_dir: &Path) -> Vec<DoctorFinding> {
    runtime_status(state_dir).err().into_iter().collect()
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
/// 4. those versions meet this binary's floors;
/// 5. no installed dependency is below its security floor.
///
/// The check runs from `state_dir`, never from where `afoot` was started.
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
            "run `afoot python setup`",
        ));
    }
    let Some(python) = venv_python(state_dir) else {
        return Err(error(
            format!(
                "venv interpreter missing or not executable: {}",
                venv_interpreter(&venv).display()
            ),
            "run `afoot python setup` to rebuild the venv",
        ));
    };
    let output = isolated_python(&python, state_dir)
        .args([
            "-c",
            &format!(
                "import ecologits, codecarbon, psutil; {VERSIONS_SCRIPT}{}",
                minimums_script(&manifest().minimums)
            ),
        ])
        .output()
        .map_err(|err| {
            error(
                format!("failed to execute {}: {err}", python.display()),
                "run `afoot python setup` to rebuild the venv",
            )
        })?;
    if !output.status.success() {
        return Err(error(
            format!(
                "import check failed under {}: {}",
                python.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "run `afoot python setup` to reinstall the pinned packages",
        ));
    }
    let versions = parse_versions(&output.stdout).ok_or_else(|| {
        error(
            "cannot read the installed ecologits/codecarbon versions".to_string(),
            "run `afoot python setup` to reinstall the pinned packages",
        )
    })?;
    if !meets_floors(&versions) {
        let floors = floors();
        return Err(error(
            format!(
                "ecologits {} / codecarbon {} is older than this afoot needs ({} / {})",
                versions.ecologits, versions.codecarbon, floors.ecologits, floors.codecarbon
            ),
            "run `afoot python setup` to install at least the pinned versions",
        ));
    }
    if let Some((name, installed, floor)) = below_minimum(
        &String::from_utf8_lossy(&output.stdout),
        &manifest().minimums,
    ) {
        // The runtime works; it should not be left like this.
        return Err(DoctorFinding {
            severity: Severity::Warn,
            message: format!("{name} {installed} has a known vulnerability, fixed in {floor}"),
            fix_hint: "run `afoot python setup` to upgrade it".to_string(),
        });
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state dir whose venv interpreter is the system `python3`, with a
    /// module planted next to it that leaves a marker when imported.
    #[cfg(unix)]
    fn planted_state_dir(module: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("venv/bin");
        std::fs::create_dir_all(&bin).expect("venv bin");
        let python3 = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v python3"])
                .output()
                .expect("which")
                .stdout,
        )
        .expect("utf8");
        std::os::unix::fs::symlink(python3.trim(), bin.join("python")).expect("symlink python");
        let marker = dir.path().join("imported");
        std::fs::write(
            dir.path().join(format!("{module}.py")),
            format!("open({:?}, 'w').write('x')\n", marker.display().to_string()),
        )
        .expect("plant module");
        (dir, marker)
    }

    #[cfg(unix)]
    #[test]
    fn the_runtime_check_never_imports_from_the_working_directory() {
        let (dir, marker) = planted_state_dir("ecologits");
        // Whatever directory afoot runs from, a project's `ecologits.py` must
        // not be what the health check imports.
        let _ = runtime_status(dir.path());
        assert!(!marker.exists(), "a planted module ran");
    }

    #[cfg(unix)]
    #[test]
    fn the_fingerprint_moves_when_packages_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = environment_fingerprint(dir.path());
        let site = dir.path().join("venv/lib/python3.12/site-packages");
        std::fs::create_dir_all(&site).expect("site-packages");
        let created = environment_fingerprint(dir.path());
        assert_ne!(created, empty);
        assert_eq!(
            environment_fingerprint(dir.path()),
            created,
            "stable while unchanged"
        );

        // Directory mtimes have coarse resolution on some filesystems.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::create_dir(site.join("newpkg")).expect("install");
        assert_ne!(environment_fingerprint(dir.path()), created);
    }

    #[test]
    fn requirements_pin_the_given_versions_and_keep_psutil_open() {
        let versions = RuntimeVersions {
            ecologits: "0.11.3".into(),
            codecarbon: "3.3.1".into(),
        };
        assert_eq!(
            requirements_for(&versions),
            [
                "ecologits==0.11.3",
                "codecarbon==3.3.1",
                "psutil>=6",
                "certifi>=2026.10.18",
                "joserfc>=1.7.6"
            ]
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
        assert_eq!(m.packages.ecologits, "0.12.0");
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
    fn doctor_reports_at_most_one_finding() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(doctor(dir.path()).len(), 1, "the first problem only");
    }
    #[test]
    fn a_dependency_with_a_security_floor_is_required_at_that_floor() {
        let requirements = requirements_for(&floors());
        assert!(
            requirements.iter().any(|r| r == "certifi>=2026.10.18"),
            "{requirements:?}"
        );
    }

    #[test]
    fn an_installed_dependency_below_its_security_floor_is_found() {
        let minimums = BTreeMap::from([("certifi".to_string(), "2026.10.18".to_string())]);
        assert_eq!(
            below_minimum("0.11.2\n3.3.1\ncertifi 2026.7.22\n", &minimums),
            Some((
                "certifi".to_string(),
                "2026.7.22".to_string(),
                "2026.10.18".to_string()
            ))
        );
        assert_eq!(
            below_minimum("0.11.2\n3.3.1\ncertifi 2026.10.23\n", &minimums),
            None
        );
        // Not installed: nothing to be vulnerable.
        assert_eq!(below_minimum("0.11.2\n3.3.1\n", &minimums), None);
    }
}
