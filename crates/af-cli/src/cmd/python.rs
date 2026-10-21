//! `af python setup|doctor`: provisions and diagnoses the managed Python
//! venv used by sidecar processes.

use std::path::Path;

use anyhow::Result;

use af_sidecar::{doctor, setup, Severity};

/// `af python setup`: provisions `state_dir/venv` via `uv` (pins from
/// `python/manifest.toml`). Prints one confirmation line on success.
pub fn run_setup(state_dir: &Path) -> Result<()> {
    setup(state_dir)?;
    super::install_sidecar_scripts(state_dir)?;
    println!("af python setup: runtime ready at {}", state_dir.display());
    Ok(())
}

/// `af python doctor`: prints one line per finding
/// (`[error|warn] <message> — fix: <hint>`) and returns the process exit
/// code — `0` if there are no `Error`-severity findings, `1` otherwise.
pub fn run_doctor(state_dir: &Path) -> i32 {
    let findings = doctor(state_dir);
    if findings.is_empty() {
        println!("af python doctor: OK — venv healthy");
        return 0;
    }

    let mut exit_code = 0;
    for finding in &findings {
        let label = match finding.severity {
            Severity::Error => {
                exit_code = 1;
                "error"
            }
            Severity::Warn => "warn",
        };
        println!("[{label}] {} — fix: {}", finding.message, finding.fix_hint);
    }
    exit_code
}
