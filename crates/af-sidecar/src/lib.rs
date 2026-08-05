//! `af-sidecar`: supervision of Python sidecar subprocesses over a
//! newline-delimited-JSON stdio protocol, plus `uv`-based provisioning
//! and health checks for the managed venv.
//!
//! The externally visible protocol is newline-delimited JSON over stdio.

mod pyenv;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

pub use pyenv::{doctor, setup, venv_python, DoctorFinding, Severity};

/// Default timeout for [`Sidecar::request`]. Overridable per-instance via
/// [`Sidecar::set_timeout`] — used by tests to keep the timeout-path test
/// fast rather than waiting the full 30s.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A supervised Python sidecar subprocess speaking newline-delimited JSON
/// over stdin/stdout.
///
/// Framing: every line written to the child's stdin and read from its
/// stdout is exactly one JSON object. [`Sidecar::request`] injects an
/// `"id"` field into outgoing objects and matches responses by that same
/// field; responses whose `"id"` doesn't match (or is missing, e.g. a
/// stray reply to a prior [`Sidecar::send`]) are discarded with a stderr
/// warning rather than being returned to the wrong caller.
///
/// A background reader thread continuously drains the child's stdout into
/// an internal channel so that `request()`'s timeout can be enforced with
/// `recv_timeout` regardless of how slow (or silent) the child is.
pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
    timeout: Duration,
}

impl Sidecar {
    /// Spawns `python <module> <args...>` with piped stdin/stdout
    /// (stderr is inherited so sidecar diagnostics surface directly).
    ///
    /// `module` is passed as a plain positional argument to the
    /// interpreter (i.e. a script path), not via `-m` — this PoC has no
    /// real installable sidecar packages yet, so the simpler form was chosen.
    ///
    /// A `Sidecar` supervises exactly one child for its whole life and
    /// deliberately keeps no copy of how it was launched: restart policy
    /// lives with the supervisor, which recovers by dropping this handle
    /// (killing and reaping the child, see the `Drop` impl) and calling
    /// `spawn` again with whatever configuration is current *then* — see
    /// `af watch`. A `respawn` method that replayed the *original*
    /// arguments would be a second, staler source of truth for that.
    pub fn spawn(python: &Path, module: &str, args: &[&str]) -> Result<Sidecar> {
        let mut command = Command::new(python);
        command
            .arg(module)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn sidecar: {} {} {}",
                python.display(),
                module,
                args.join(" ")
            )
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let rx = Self::spawn_reader(stdout);

        Ok(Sidecar {
            child,
            stdin,
            rx,
            next_id: 0,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Spawns the background line-reader thread. The channel disconnects
    /// (all senders dropped) once the child closes stdout or the thread
    /// hits a read error — `request()` treats that as "sidecar exited".
    fn spawn_reader(stdout: ChildStdout) -> Receiver<String> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Dropping `tx` here signals EOF/disconnect to any pending
            // `recv_timeout` in `request()`.
        });
        rx
    }

    /// Overrides the per-request timeout (default 30s). Exposed as a
    /// plain setter rather than a `#[cfg(test)]`-only method so it stays
    /// usable outside tests too; tests use it to keep the timeout-path
    /// test fast.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The child process's OS pid, mainly useful for tests/diagnostics
    /// that need to signal the process directly.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Non-blocking liveness check: `Ok(None)` while the child is running,
    /// `Ok(Some(status))` once it has exited (reaping it).
    ///
    /// Supervisors need this because the write side lies: a sidecar killed
    /// by `SIGKILL` leaves a pipe whose first `write` still succeeds, so
    /// [`send`] reports health for a process that is already gone. A
    /// supervisor that only learns of a death when a write finally fails
    /// keeps attributing measured-nothing to a live collector — which for
    /// this project is a fabricated zero, not a missing number.
    ///
    /// [`send`]: Sidecar::send
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    /// Sends `req` (which must be a JSON object) with a freshly assigned
    /// monotonic `"id"` injected, then blocks for a matching response
    /// (by `"id"`) for up to the configured timeout (default 30s).
    ///
    /// Lines that fail to parse as JSON, or whose `"id"` doesn't match
    /// this request's id, are discarded (with a stderr warning) and the
    /// wait continues against the remaining time budget.
    pub fn request(&mut self, req: &Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;

        let mut outgoing = req.clone();
        match outgoing.as_object_mut() {
            Some(map) => {
                map.insert("id".to_string(), Value::from(id));
            }
            None => bail!("sidecar request must be a JSON object, got: {req}"),
        }
        self.write_line(&outgoing)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!(
                    "sidecar request id={id} timed out after {:?} waiting for a response",
                    self.timeout
                );
            }
            match self.rx.recv_timeout(remaining) {
                Ok(line) => {
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(err) => {
                            eprintln!(
                                "af-sidecar: warning: discarding unparseable line from sidecar: {err} (line: {line:?})"
                            );
                            continue;
                        }
                    };
                    let resp_id = value.get("id").and_then(Value::as_u64);
                    if resp_id == Some(id) {
                        return Ok(value);
                    }
                    eprintln!(
                        "af-sidecar: warning: discarding response with unmatched id (want {id}, got {resp_id:?}): {value}"
                    );
                }
                Err(RecvTimeoutError::Timeout) => {
                    bail!(
                        "sidecar request id={id} timed out after {:?} waiting for a response",
                        self.timeout
                    );
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!(
                        "sidecar process exited before responding to request id={id}"
                    ));
                }
            }
        }
    }

    /// Fire-and-forget: writes `msg` as a single JSON line and returns
    /// without waiting for (or expecting) a response. No `"id"` is
    /// injected. Intended for the watch-list use case (pushing updates
    /// the sidecar consumes but never acks).
    pub fn send(&mut self, msg: &Value) -> Result<()> {
        self.write_line(msg)
    }

    fn write_line(&mut self, value: &Value) -> Result<()> {
        let line = serde_json::to_string(value).context("serializing sidecar message")?;
        writeln!(self.stdin, "{line}").context("writing to sidecar stdin")?;
        self.stdin.flush().context("flushing sidecar stdin")?;
        Ok(())
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
