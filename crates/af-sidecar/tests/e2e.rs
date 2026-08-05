//! Gated end-to-end test: actually provisions a venv via `uv` (network +
//! real package downloads). `#[ignore]` is the **single** gate, so plain
//! `cargo test --workspace` never touches the network per the project's
//! "no network in tests" constraint.
//!
//! It used to carry a second `AF_E2E=1` env check on top, which made
//! `-- --ignored` — the one command whose entire purpose is "run the
//! ignored tests" — silently pass without running anything. A gate that
//! reports success for a test it skipped is worse than no gate: `#[ignore]`
//! already refuses by default, and the harness *tells* you it did.
//!
//! Run explicitly with:
//!
//! ```sh
//! cargo test -p af-sidecar --test e2e -- --ignored
//! ```

use af_sidecar::{doctor, setup, venv_python, Severity};

#[test]
#[ignore = "provisions a real venv via `uv`: network + package downloads"]
fn e2e_setup_provisions_a_healthy_venv() {
    let dir = tempfile::tempdir().expect("tempdir");

    setup(dir.path()).expect("setup should provision the venv");

    assert!(
        venv_python(dir.path()).is_some(),
        "venv interpreter should exist and be executable after setup"
    );

    let findings = doctor(dir.path());
    let errors: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "doctor should report no errors after a successful setup, got: {errors:?}"
    );
}
