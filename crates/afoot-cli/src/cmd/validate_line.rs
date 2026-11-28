//! `afoot validate-line`: hidden helper for collector test suites (currently
//! `collectors/claude-code/test_hooks.sh`) — reads ONE line from stdin and
//! validates it against Contract #1 via [`afoot_events::parse_line`], without
//! the caller (a shell script) needing to reimplement schema validation.
//!
//! Exit 0 if the line parses as a valid event; exit 1 with the reject
//! reason on stderr otherwise. Hidden from `afoot --help` (`#[command(hide =
//! true)]` in `crate::Commands`) since it's a test/debugging seam, not a
//! user-facing surface.

use std::io::{self, BufRead};

/// Reads one line from stdin and validates it. Returns the process exit
/// code the caller should use (0 valid, 1 invalid or no input).
pub fn run() -> i32 {
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) => {
            eprintln!("afoot validate-line: no input on stdin");
            1
        }
        Ok(_) => validate(line.trim_end_matches(['\n', '\r'])),
        Err(err) => {
            eprintln!("afoot validate-line: failed to read stdin: {err}");
            1
        }
    }
}

fn validate(line: &str) -> i32 {
    match afoot_events::parse_line(line) {
        Ok(_) => 0,
        Err(reason) => {
            eprintln!("afoot validate-line: {reason}");
            1
        }
    }
}
