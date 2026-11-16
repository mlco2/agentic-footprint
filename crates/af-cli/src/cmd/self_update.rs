//! `af self update`: replace this binary with a newer verified GitHub
//! release, then hand the rest (Python runtime, receiver restart) to the
//! new binary. Modelled on `uv self update`: only binaries the installer
//! recorded in an install receipt are updated.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::http;

pub(crate) const RELEASE_SOURCE: &str = "github:mlco2/agentic-footprint";
const DEFAULT_API: &str = "https://api.github.com";
const REPO: &str = "mlco2/agentic-footprint";

/// The release-workflow target this binary was built for, or `None` on a
/// platform no release is built for.
fn current_target() -> Option<&'static str> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        ("x86_64", "windows") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn asset_name(target: &str) -> String {
    if target.contains("windows") {
        format!("af-{target}.exe")
    } else {
        format!("af-{target}")
    }
}

/// `X.Y.Z` (optionally `v`-prefixed) as numbers; anything else — a
/// prerelease, a two-part version — is not a release this updates to.
fn parse_version(value: &str) -> Option<Vec<u64>> {
    af_sidecar::version_key(value.strip_prefix('v').unwrap_or(value)).filter(|key| key.len() == 3)
}

fn update_needed(current: &str, target: &str, explicit: bool) -> bool {
    match (parse_version(current), parse_version(target)) {
        (Some(current), Some(target)) if explicit => current != target,
        (Some(current), Some(target)) => target > current,
        _ => false,
    }
}

/// The hex digest `sha256sum` printed for `asset` (`<hex>  <name>` or
/// `<hex> *<name>`).
fn checksum_for<'a>(sums: &'a str, asset: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == asset).then_some(digest)
    })
}

/// Written by the installer for release-binary installs; its absence is
/// how self update knows this binary is not one it manages. `binary` is
/// what is checked; `source` and `target` record what kind of install it
/// is. The installed version is the binary's own, never recorded here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub source: String,
    pub target: String,
    pub binary: PathBuf,
}

fn receipt_path(state_dir: &Path) -> PathBuf {
    state_dir.join("install-receipt.json")
}

fn read_receipt(state_dir: &Path) -> Result<Option<Receipt>> {
    let path = receipt_path(state_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("parse {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Whether the running binary is one `af self update` manages: a release
/// install with its receipt.
pub(crate) fn manages_this_binary(state_dir: &Path) -> bool {
    let (Ok(Some(receipt)), Ok(exe)) = (read_receipt(state_dir), std::env::current_exe()) else {
        return false;
    };
    receipt_is_for(&receipt, &exe)
}

fn receipt_is_for(receipt: &Receipt, exe: &Path) -> bool {
    match (receipt.binary.canonicalize(), exe.canonicalize()) {
        (Ok(recorded), Ok(running)) => recorded == running,
        _ => false,
    }
}

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum Action {
    /// Update af to the latest release, then its Python runtime
    Update(UpdateArgs),
    /// Record that the installer placed this binary (installer use only)
    #[command(hide = true)]
    RecordInstall {
        #[arg(long)]
        binary: PathBuf,
    },
}

#[derive(Debug, Clone, clap::Args)]
pub struct UpdateArgs {
    /// Exact release to install (X.Y.Z); default: the latest release
    pub version: Option<String>,
    /// Print what would change without changing anything
    #[arg(long)]
    pub dry_run: bool,
    /// Skip refreshing the managed Python runtime
    #[arg(long)]
    pub no_python: bool,
    /// GitHub token for API rate limits (default: $GITHUB_TOKEN)
    #[arg(long)]
    pub token: Option<String>,
    /// Install without verifying the release signature (the checksum is still
    /// checked, but it comes from the same release as the binary)
    #[arg(long)]
    pub allow_unsigned: bool,
}

pub fn run(state_dir: &Path, args: Args) -> Result<i32> {
    match args.action {
        Action::RecordInstall { binary } => {
            let target = current_target().context("no release target for this platform")?;
            let receipt = Receipt {
                source: RELEASE_SOURCE.to_string(),
                target: target.to_string(),
                binary,
            };
            super::write_atomic(
                &receipt_path(state_dir),
                &serde_json::to_vec_pretty(&receipt)?,
            )?;
            Ok(0)
        }
        Action::Update(update) => update_command(state_dir, update),
    }
}

struct Release {
    tag: String,
    assets: Vec<(String, String)>,
}

impl Release {
    fn asset_url(&self, name: &str) -> Result<&str> {
        self.assets
            .iter()
            .find(|(asset, _)| asset == name)
            .map(|(_, url)| url.as_str())
            .with_context(|| {
                format!(
                    "release {} has no {name} asset for this platform; nothing was changed",
                    self.tag
                )
            })
    }
}

fn fetch_release(
    agent: &ureq::Agent,
    base: &str,
    version: Option<&str>,
    token: Option<&str>,
) -> Result<Release> {
    let url = match version {
        Some(version) => format!("{base}/repos/{REPO}/releases/tags/v{version}"),
        None => format!("{base}/repos/{REPO}/releases/latest"),
    };
    let json = http::get_json(agent, &url, token)?;
    let tag = json["tag_name"]
        .as_str()
        .context("release has no tag_name")?
        .to_string();
    let assets = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|asset| {
            Some((
                asset["name"].as_str()?.to_string(),
                asset["browser_download_url"].as_str()?.to_string(),
            ))
        })
        .collect();
    Ok(Release { tag, assets })
}

/// Downloads this platform's asset from `release` beside `exe` (as
/// `<exe>.new`), verified against `SHA256SUMS` and, unless
/// `allow_unsigned`, its sigstore bundle. Returns the staged path and what
/// was verified; nothing is written unless every check passed, and `exe` is
/// untouched.
fn stage_verified(
    agent: &ureq::Agent,
    release: &Release,
    target: &str,
    exe: &Path,
    token: Option<&str>,
    allow_unsigned: bool,
) -> Result<(PathBuf, &'static str)> {
    use sha2::{Digest, Sha256};
    let asset = asset_name(target);
    // Everything that can be refused cheaply first: the release must list
    // this platform's asset, and SHA256SUMS (small) must cover it, before
    // the big download.
    let asset_url = release.asset_url(&asset)?;
    let sums = http::get(agent, release.asset_url("SHA256SUMS")?, token)?;
    let sums = String::from_utf8(sums).context("SHA256SUMS is not UTF-8")?;
    let expected =
        checksum_for(&sums, &asset).with_context(|| format!("SHA256SUMS does not list {asset}"))?;
    let bytes = http::get(agent, asset_url, token)?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(expected),
        "checksum mismatch for {asset}: expected {expected}, got {actual}; nothing was changed"
    );

    // The checksum comes from the same release as the binary: it proves the
    // download intact, not who built it. The signature does.
    let verified = if allow_unsigned {
        "checksum verified; signature not verified (--allow-unsigned)"
    } else {
        let bundle = http::get(
            agent,
            release.asset_url(&format!("{asset}.sigstore.json"))?,
            token,
        )?;
        let bundle = String::from_utf8(bundle).context("the sigstore bundle is not UTF-8")?;
        verify_bundle(
            &bytes,
            &bundle,
            &release_identity(&release.tag),
            GITHUB_ACTIONS_ISSUER,
        )
        .with_context(|| {
            format!(
                "the signature of {asset} in {} does not verify; nothing was changed",
                release.tag
            )
        })?;
        "checksum and sigstore signature verified"
    };

    let staged = exe.with_extension("new");
    std::fs::write(&staged, &bytes).with_context(|| format!("write {}", staged.display()))?;
    Ok((staged, verified))
}

/// The OIDC issuer of GitHub Actions' signing certificates.
const GITHUB_ACTIONS_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// The certificate a release asset's signature must carry: issued to this
/// repository's release workflow, running for this tag. A pattern over the
/// repository would also accept a signature from any other workflow or
/// branch of it.
fn release_identity(tag: &str) -> String {
    format!("https://github.com/{REPO}/.github/workflows/release.yml@refs/tags/{tag}")
}

/// Verifies `artifact` against a sigstore bundle, in-process: the
/// certificate must chain to the Sigstore trust root embedded in this
/// binary, name exactly `identity` from `issuer`, be logged in the Rekor
/// transparency log, and have signed these very bytes.
fn verify_bundle(artifact: &[u8], bundle: &str, identity: &str, issuer: &str) -> Result<()> {
    use sigstore_verify::trust_root::{TrustedRoot, SIGSTORE_PRODUCTION_TRUSTED_ROOT};
    use sigstore_verify::types::Bundle;
    use sigstore_verify::{verify, VerificationPolicy};

    let bundle = Bundle::from_json(bundle).context("not a sigstore bundle")?;
    let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
        .context("the embedded Sigstore trust root")?;
    let policy = VerificationPolicy::any_identity()
        .require_identity(identity)
        .require_issuer(issuer);
    verify(artifact, &bundle, &policy, &root)?;
    Ok(())
}

/// Moves `staged` over `current`. Unix: a rename (a running process keeps
/// its inode). Windows: a running exe cannot be overwritten but can be
/// renamed, so it is moved aside to `.old` (deleted by the next
/// `af self update`).
fn replace_binary(current: &Path, staged: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        let old = current.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(current, &old)
            .with_context(|| format!("move aside {}", current.display()))?;
    }
    std::fs::rename(staged, current).with_context(|| format!("replace {}", current.display()))
}

/// Runs `exe python upgrade` (the new binary's, after a swap) and relays
/// only its `Runtime: ` summary — all of its output when it fails; the
/// previous runtime is kept then. Returns whether the venv now holds
/// different versions, judged from the venv itself.
fn refresh_runtime(exe: &Path, venv: &Path, dry_run: bool) -> bool {
    let before = af_sidecar::installed_versions(venv);
    let mut command = std::process::Command::new(exe);
    command.args(["python", "upgrade"]);
    if dry_run {
        command.arg("--dry-run");
    }
    match command.output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = stdout.lines().rev().find(|l| l.starts_with("Runtime: ")) {
                println!("{line}");
            }
        }
        Ok(output) => {
            eprint!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            eprintln!(
                "af self update: the Python runtime was not upgraded; the previous one is kept \
                 and the next `af self update` retries"
            );
        }
        Err(error) => eprintln!("af self update: could not run {} ({error})", exe.display()),
    }
    af_sidecar::installed_versions(venv) != before
}

/// Restarts the installed receiver with `exe`, or says to restart a
/// manually run one.
fn restart_receiver(exe: &Path) -> Result<()> {
    if !super::service::installed() {
        println!("Restart your `af watch` so it runs the new version.");
        return Ok(());
    }
    let status = std::process::Command::new(exe)
        .args(["service", "start"])
        .status()
        .context("restart the receiver with the new af")?;
    anyhow::ensure!(
        status.success(),
        "the receiver did not restart; run `af service start`"
    );
    Ok(())
}

fn update_command(state_dir: &Path, args: UpdateArgs) -> Result<i32> {
    // The resolved file, not a symlink to it: that is what gets replaced
    // (a link stays a link) and what the new binary is run from.
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .context("resolve the running executable")?;
    let Some(receipt) = read_receipt(state_dir)? else {
        eprintln!(
            "af self update: this af was not installed by the release installer (no {}).\n\
             Update it the way it was installed (e.g. rerun ./install.sh from your checkout).",
            receipt_path(state_dir).display()
        );
        return Ok(2);
    };
    if !receipt_is_for(&receipt, &exe) {
        eprintln!(
            "af self update: the release installer installed af at {}, but this is {}. \
             Are multiple copies of af installed?",
            receipt.binary.display(),
            exe.display()
        );
        return Ok(2);
    }
    #[cfg(windows)]
    let _ = std::fs::remove_file(exe.with_extension("exe.old"));

    if let Some(version) = &args.version {
        anyhow::ensure!(
            parse_version(version).is_some() && !version.starts_with('v'),
            "explicit versions must be exact X.Y.Z, got {version:?}"
        );
    }
    let target = current_target().context("no release is built for this platform")?;
    let base = std::env::var("AF_UPDATE_BASE_URL").unwrap_or_else(|_| DEFAULT_API.to_string());
    let token = args
        .token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()));
    let token = token.as_deref();
    let agent = http::agent();
    let current = env!("CARGO_PKG_VERSION");
    let venv = state_dir.join("venv");

    println!("Checking for updates...");
    let release = fetch_release(&agent, &base, args.version.as_deref(), token)?;
    let swap = update_needed(current, &release.tag, args.version.is_some());
    if args.dry_run {
        if swap {
            println!("Would update af from v{current} to {}", release.tag);
        } else {
            println!("af is already on v{current}");
            if !args.no_python {
                refresh_runtime(&exe, &venv, true);
            }
        }
        return Ok(0);
    }

    if swap {
        let (staged, verified) =
            stage_verified(&agent, &release, target, &exe, token, args.allow_unsigned)?;
        replace_binary(&exe, &staged)?;
        println!("Updated af from v{current} to {} ({verified})", release.tag);
    } else {
        println!(
            "af is already on v{current}{}",
            if args.version.is_none() {
                " (the latest release)"
            } else {
                ""
            }
        );
    }
    // Everything from here runs as the (possibly new) binary: its Python
    // floors, its smoke test, its receiver. New agent models must not wait
    // for an af release, so the runtime is refreshed either way.
    let runtime_changed = !args.no_python && refresh_runtime(&exe, &venv, false);
    if swap || runtime_changed {
        restart_receiver(&exe)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real artifact and its keyless bundle (sigstore/fulcio's release
    /// checksums; see tests/fixtures/sigstore/README.md): a bundle cannot be
    /// minted offline for a test binary.
    const SIGNED: &[u8] =
        include_bytes!("../../../../tests/fixtures/sigstore/fulcio_checksums.txt");
    const BUNDLE: &str =
        include_str!("../../../../tests/fixtures/sigstore/fulcio_checksums.txt.sigstore.json");
    const SIGNER: &str = "keyless@projectsigstore.iam.gserviceaccount.com";
    const SIGNER_ISSUER: &str = "https://accounts.google.com";

    #[test]
    fn a_genuine_signature_from_the_expected_signer_verifies() {
        verify_bundle(SIGNED, BUNDLE, SIGNER, SIGNER_ISSUER).expect("genuine bundle");
    }

    #[test]
    fn a_signature_from_any_other_signer_is_refused() {
        let error = verify_bundle(
            SIGNED,
            BUNDLE,
            &release_identity("v1.9.0"),
            GITHUB_ACTIONS_ISSUER,
        )
        .expect_err("someone else's signature");
        assert!(format!("{error:#}").contains("identity"), "{error:#}");
        verify_bundle(SIGNED, BUNDLE, SIGNER, GITHUB_ACTIONS_ISSUER)
            .expect_err("the right name from another issuer");
    }

    #[test]
    fn a_tampered_artifact_or_a_broken_bundle_is_refused() {
        let mut tampered = SIGNED.to_vec();
        tampered.push(b'x');
        verify_bundle(&tampered, BUNDLE, SIGNER, SIGNER_ISSUER).expect_err("one byte more");
        verify_bundle(SIGNED, "{}", SIGNER, SIGNER_ISSUER).expect_err("not a bundle");
    }

    #[test]
    fn releases_must_be_signed_by_the_release_workflow_for_their_tag() {
        assert_eq!(
            release_identity("v0.2.0"),
            "https://github.com/mlco2/agentic-footprint/.github/workflows/release.yml@refs/tags/v0.2.0"
        );
        assert_eq!(
            GITHUB_ACTIONS_ISSUER,
            "https://token.actions.githubusercontent.com"
        );
    }

    #[test]
    fn versions_parse_with_or_without_v_and_reject_prereleases() {
        assert_eq!(parse_version("0.2.10"), Some(vec![0, 2, 10]));
        assert_eq!(parse_version("v1.0.0"), Some(vec![1, 0, 0]));
        assert_eq!(parse_version("1.0.0-rc1"), None);
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn update_needed_only_for_newer_unless_explicit() {
        assert!(update_needed("0.1.0", "v0.2.0", false));
        assert!(!update_needed("0.2.0", "v0.2.0", false));
        assert!(!update_needed("0.3.0", "v0.2.0", false));
        // An explicit version is a request: downgrades allowed, same is a no-op.
        assert!(update_needed("0.3.0", "v0.2.0", true));
        assert!(!update_needed("0.2.0", "0.2.0", true));
        assert!(!update_needed("0.1.0", "not-a-version", false));
    }

    #[test]
    fn checksum_lookup_matches_the_exact_asset_name() {
        let sums = "aaaa  af-x86_64-apple-darwin\n\
                    bbbb *af-aarch64-apple-darwin\n\
                    cccc  agentic-footprint-v0.2.0-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(checksum_for(sums, "af-aarch64-apple-darwin"), Some("bbbb"));
        assert_eq!(checksum_for(sums, "af-x86_64-apple-darwin"), Some("aaaa"));
        assert_eq!(checksum_for(sums, "af-x86_64-unknown-linux-gnu"), None);
    }

    #[test]
    fn asset_names_follow_the_release_workflow() {
        assert_eq!(
            asset_name("aarch64-apple-darwin"),
            "af-aarch64-apple-darwin"
        );
        assert_eq!(
            asset_name("x86_64-pc-windows-msvc"),
            "af-x86_64-pc-windows-msvc.exe"
        );
    }

    #[test]
    fn this_build_has_a_release_target() {
        // Every platform CI builds on is a release target.
        assert!(current_target().is_some());
    }

    #[test]
    fn receipt_round_trips_and_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_receipt(dir.path()).unwrap(), None);
        let receipt = Receipt {
            source: RELEASE_SOURCE.into(),
            target: "aarch64-apple-darwin".into(),
            binary: dir.path().join("af"),
        };
        std::fs::write(
            receipt_path(dir.path()),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(read_receipt(dir.path()).unwrap(), Some(receipt));
    }

    #[cfg(unix)]
    #[test]
    fn receipt_matches_the_executable_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-af");
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("af");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let receipt = Receipt {
            source: RELEASE_SOURCE.into(),
            target: "t".into(),
            binary: link.clone(),
        };
        assert!(receipt_is_for(&receipt, &real));
        assert!(receipt_is_for(&receipt, &link));
        assert!(!receipt_is_for(&receipt, &dir.path().join("other")));
    }
}
