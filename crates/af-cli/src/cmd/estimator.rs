//! Locating and spawning the `af_estimator` Python sidecar.
//!
//! Every path here is allowed to come up empty: the estimator is optional.
//! A machine with no managed venv still gets a full local-measurement
//! report, with the remote half honestly marked `pending` — so this module
//! returns a *reason* rather than an error, for the caller to print.

use std::path::Path;

use af_sidecar::{venv_python, Sidecar};

use super::sidecar_script;

/// The estimator script's path relative to the Python source root.
pub const ESTIMATOR_SCRIPT: &str = "af_estimator/__main__.py";

/// Either a live sidecar or the reason there isn't one.
pub struct Estimator {
    pub sidecar: Option<Sidecar>,
    pub note: Option<String>,
}

impl Estimator {
    fn missing(reason: impl Into<String>) -> Self {
        Estimator {
            sidecar: None,
            note: Some(format!(
                "no estimator ({}): remote llm_call impacts stay pending and no local gwp is computed",
                reason.into()
            )),
        }
    }
}

/// Spawns the estimator sidecar for `state_dir`, degrading to
/// [`Estimator::missing`] rather than failing.
///
/// `AF_ESTIMATOR_SCRIPT` overrides the script path (with
/// `AF_ESTIMATOR_PYTHON`, default `python3`, and newline-separated
/// `AF_ESTIMATOR_ARGS`). It exists so the golden-transcript tests can drive
/// `tests/fixtures/fake_sidecar.py --replay` through the real CLI without
/// ecologits, a venv, or a network — the same seam `af-core`'s own
/// estimator tests use, moved up to the process boundary.
pub fn spawn(state_dir: &Path) -> Estimator {
    if let Ok(script) = std::env::var("AF_ESTIMATOR_SCRIPT") {
        let python = std::env::var("AF_ESTIMATOR_PYTHON").unwrap_or_else(|_| "python3".into());
        let raw_args = std::env::var("AF_ESTIMATOR_ARGS").unwrap_or_default();
        let args: Vec<&str> = raw_args.split('\n').filter(|a| !a.is_empty()).collect();
        return match Sidecar::spawn(Path::new(&python), &script, &args) {
            Ok(sidecar) => Estimator {
                sidecar: Some(sidecar),
                note: None,
            },
            Err(err) => Estimator::missing(format!("AF_ESTIMATOR_SCRIPT failed to spawn: {err:#}")),
        };
    }

    let Some(python) = venv_python(state_dir) else {
        return Estimator::missing("managed venv not provisioned — run `af python setup`");
    };
    let Some(script) = sidecar_script(state_dir, ESTIMATOR_SCRIPT) else {
        return Estimator::missing("af_estimator script not found");
    };
    let Some(script_str) = script.to_str() else {
        return Estimator::missing("af_estimator script path is not UTF-8");
    };

    match Sidecar::spawn(&python, script_str, &[]) {
        Ok(sidecar) => Estimator {
            sidecar: Some(sidecar),
            note: None,
        },
        Err(err) => Estimator::missing(format!("af_estimator failed to spawn: {err:#}")),
    }
}
