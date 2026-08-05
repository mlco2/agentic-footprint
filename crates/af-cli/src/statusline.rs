//! `af statusline`: the read-only presentation surface consumed by
//! `statusline/ecologits-bar.sh`.
//!
//! Claude Code refreshes a status line constantly, for every session, on a
//! machine the user is actively typing on. That makes this the one `af`
//! entry point with a hard latency budget and a hard "touch nothing" rule:
//!
//! * **No ingest, no estimation, no migration, no writes.** It opens the
//!   store through [`Store::open_read_only`] — a `SQLITE_OPEN_READ_ONLY`
//!   handle with no read/write fallback, so the process cannot create the
//!   database, recover a hot WAL or checkpoint one on close — and issues one
//!   `SELECT`. A statusline that ingests would race `af watch` and pay the
//!   spool-scan cost on every keystroke-driven refresh; a statusline that
//!   migrates would upgrade a user's database as a side effect of drawing a
//!   bar.
//! * **It never waits.** The read-only handle carries a 250ms busy timeout
//!   rather than the writer's 5s: when `af watch` holds the database, the
//!   honest degradation is zeros now, not a correct number after a stall.
//!   Both the open and the `SELECT` fail into [`ZEROS`].
//! * **It always exits 0 and always prints exactly one line.** Garbage on
//!   stdin, no state dir, no join for this session, a corrupt record: every
//!   one of those prints [`ZEROS`]. Breaking a user's status line is a
//!   worse failure than showing zeros, and zeros are what the bar's own
//!   formatters already render as "0" rather than as a plausible number.
//!
//! Output contract — one line, five space-separated decimal numbers:
//!
//! ```text
//! <gwp_kg> <water_L> <energy_kWh> <adpe_kg> <pe_MJ>
//! ```
//!
//! Each value is the **mean of the criterion's `total` range**
//! (`(min + max) / 2`) taken from the session-level `impact_join` record —
//! the same range mean the original EcoLogits bar computed from the API's
//! response, so the rendering half of the script is unchanged.
//!
//! Numbers are formatted with Rust's `f64` `Display`, which is the shortest
//! round-tripping decimal and — unlike `{:e}` or C's `%g` — **never uses
//! scientific notation**. That matters: the bar's unit formatters are `awk`
//! programs doing `v+0`, and `1.875e-9` is not portably parsed by every
//! `awk`. `0.000000001875` is. Non-finite values print as `0`.
//!
//! The criteria sourcing rules are encoded in [`render`].

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use af_store::Store;

/// What every failure path prints: five zeros, one line, exit 0.
pub const ZEROS: &str = "0 0 0 0 0";

/// Reads the Claude Code status JSON from stdin and prints one impact line.
/// Returns the process exit code, which is always `0`.
pub fn run() -> i32 {
    let mut input = String::new();
    // A read error (closed stdin, invalid UTF-8) leaves `input` empty or
    // partial, which `line_for` already treats as "no session" — there is
    // no failure mode here worth a different exit code.
    let _ = std::io::stdin().read_to_string(&mut input);
    println!("{}", line_for(crate::paths::state_dir_checked(), &input));
    0
}

/// The whole command as a pure-ish function: status JSON in, one line out.
fn line_for(state_dir: Option<PathBuf>, input: &str) -> String {
    match session_join(state_dir.as_deref(), input) {
        Some(join) => render(&join),
        None => ZEROS.to_string(),
    }
}

/// Looks up the stored session-level `impact_join` for the session named on
/// stdin. Every step is allowed to fail into `None` (→ zeros).
fn session_join(state_dir: Option<&Path>, input: &str) -> Option<Value> {
    let status: Value = serde_json::from_str(input.trim()).ok()?;
    let session_id = status.get("session_id")?.as_str()?;
    if session_id.is_empty() {
        return None;
    }

    // Both of these fail into zeros, deliberately including the busy case:
    // the open cannot wait out a lock (250ms timeout) and neither can the
    // `SELECT`, so `SQLITE_BUSY` from either degrades the bar instead of
    // holding up the redraw.
    let db_path = state_dir?.join("state.db");
    let store = Store::open_read_only(&db_path).ok()?;
    let joins = store.joins_for_session(session_id).ok()?;

    // The session unit is identified by its own `unit.level`, not by the
    // `session:` key prefix — `af-store` deliberately keeps the key scheme
    // an implementation detail of the join assembler.
    joins
        .into_iter()
        .map(|(_key, record)| record)
        .find(|record| record["unit"]["level"] == "session")
}

/// Applies the criteria sourcing rules to one session join.
///
/// * **gwp** — `combined_total`, else the remote estimate. A combined total
///   is only emitted when both halves are genuinely complete, so its
///   absence means the local gwp (which needs the zone's emission factor)
///   or some estimate is missing; the remote figure is then the honest
///   best-known number rather than a local-only one dressed up as a total.
/// * **energy** — `combined_total`, else the local measurement, else the
///   remote estimate. Unlike gwp, local energy needs no methodology beyond
///   the sampler, so it is a real number on its own.
/// * **water / adpe / pe** — remote only. Nothing local measures them.
///
/// Anything unavailable is `0`, which the bar renders as `0` rather than as
/// a small-looking quantity.
fn render(join: &Value) -> String {
    let combined = &join["combined_total"];
    let local = &join["local_measured"];
    let remote = &join["remote_estimated"]["impacts"];

    let gwp = mean(&combined["gwp"])
        .or_else(|| mean(&remote["gwp"]))
        .unwrap_or(0.0);
    let energy = mean(&combined["energy"])
        .or_else(|| mean(&local["energy"]))
        .or_else(|| mean(&remote["energy"]))
        .unwrap_or(0.0);
    let water = mean(&remote["water"]).unwrap_or(0.0);
    let adpe = mean(&remote["adpe"]).unwrap_or(0.0);
    let pe = mean(&remote["pe"]).unwrap_or(0.0);

    format!(
        "{} {} {} {} {}",
        num(gwp),
        num(water),
        num(energy),
        num(adpe),
        num(pe),
    )
}

/// Mean of a criterion's `total` range. Both ends must be present and
/// finite: half a range is not a value, and a criterion carrying only a
/// `min` would otherwise be reported as if it were exact.
fn mean(criterion: &Value) -> Option<f64> {
    let total = criterion.get("total")?;
    let min = total.get("min")?.as_f64()?;
    let max = total.get("max")?.as_f64()?;
    if !min.is_finite() || !max.is_finite() {
        return None;
    }
    Some((min + max) / 2.0)
}

/// Formats one value for the bar's `awk` formatters: plain decimal, never
/// scientific notation, non-finite collapsed to `0`.
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range(min: f64, max: f64) -> Value {
        json!({"unit": "x", "total": {"min": min, "max": max}})
    }

    #[test]
    fn renders_range_means_in_gwp_water_energy_adpe_pe_order() {
        let join = json!({
            "combined_total": {"gwp": range(1.0, 3.0), "energy": range(0.5, 1.5)},
            "local_measured": {"energy": range(9.0, 9.0)},
            "remote_estimated": {"impacts": {
                "water": range(2.0, 4.0),
                "adpe": range(10.0, 20.0),
                "pe": range(100.0, 300.0),
            }},
        });
        assert_eq!(render(&join), "2 3 1 15 200");
    }

    #[test]
    fn falls_back_to_remote_gwp_and_local_energy_when_no_combined_total() {
        let join = json!({
            "combined_total": {},
            "local_measured": {"energy": range(7.0, 7.0)},
            "remote_estimated": {"impacts": {"gwp": range(1.0, 2.0)}},
        });
        // gwp from remote, energy from the local measurement, the
        // remote-only criteria absent → 0.
        assert_eq!(render(&join), "1.5 0 7 0 0");
    }

    #[test]
    fn falls_back_to_remote_energy_when_nothing_local_was_measured() {
        let join = json!({
            "combined_total": {},
            "local_measured": {},
            "remote_estimated": {"impacts": {"energy": range(1.0, 2.0)}},
        });
        assert_eq!(render(&join), "0 0 1.5 0 0");
    }

    #[test]
    fn a_join_with_no_impacts_at_all_renders_zeros() {
        assert_eq!(render(&json!({"unit": {"level": "session"}})), ZEROS);
    }

    #[test]
    fn half_a_range_is_not_a_value() {
        let join = json!({
            "combined_total": {"gwp": {"total": {"min": 1.0}}},
            "remote_estimated": {"impacts": {}},
        });
        assert_eq!(render(&join), ZEROS);
    }

    #[test]
    fn small_numbers_are_plain_decimals_not_scientific_notation() {
        // The bar's awk formatters do `v+0`; `1.875e-09` is not portably
        // parsed, `0.000000001875` is.
        assert_eq!(num(1.875e-9), "0.000000001875");
        assert_eq!(num(3.75e-5), "0.0000375");
        assert_eq!(num(0.0), "0");
    }

    #[test]
    fn non_finite_values_never_reach_the_bar() {
        assert_eq!(num(f64::NAN), "0");
        assert_eq!(num(f64::INFINITY), "0");
        assert_eq!(num(f64::NEG_INFINITY), "0");
    }

    #[test]
    fn garbage_and_sessionless_stdin_yield_zeros_without_touching_a_store() {
        for input in ["", "   ", "not json at all", "{}", "[1,2,3]", "null"] {
            assert_eq!(line_for(None, input), ZEROS, "input {input:?}");
        }
    }
}
