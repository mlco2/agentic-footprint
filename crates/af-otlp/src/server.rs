//! The `tiny_http`-backed HTTP server: `POST /v1/logs` and `POST
//! /v1/metrics`, everything else 404s. Kept separate from [`crate::normalize`]
//! so the mapping logic has no HTTP-layer dependency and can be tested
//! against fixtures directly.
//!
//! # Trust boundary
//!
//! This receiver is an unauthenticated HTTP endpoint on loopback. Every
//! byte it reads is chosen by whatever process connected to it, and the
//! agent exporter it exists for is only the *intended* such process. Three
//! guards, all of which must stay compatible with a real Claude Code
//! exporter (which sends `Host: 127.0.0.1:4318` and batches of a few KiB):
//!
//! - bodies are read through a [`MAX_BODY_BYTES`] cap and refused with
//!   `413` past it, so no client can make the receiver buffer without
//!   bound;
//! - a `Host` that is present and not loopback is refused with `403`,
//!   which is the DNS-rebinding guard;
//! - `session.id` is reduced to one safe path component
//!   ([`crate::sanitize_id`]) before it reaches a filename.
//!
//! tiny_http 0.12 does not expose the body socket for a read timeout. To keep
//! one slow client from serializing ingestion, requests run on a bounded
//! worker pool. Excess work receives a retryable `503`, and shutdown waits
//! only a bounded time for a worker stuck in a body read.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tiny_http::{Method, Response, Server};

use crate::normalize::normalize_logs;
use crate::sanitize::sanitize_id;

/// Largest request body the receiver will read, in bytes.
///
/// Claude Code's OTLP batches are a few KiB; 4 MiB is three orders of
/// magnitude of headroom for a future exporter that batches aggressively,
/// and still small enough that a client cannot exhaust memory by
/// announcing a huge `Content-Length` and streaming forever. Enforced by
/// reading through [`Read::take`] rather than trusting the header — the
/// header is as attacker-controlled as the body.
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;
const WORKER_COUNT: usize = 4;
const REQUEST_QUEUE_CAPACITY: usize = 16;
const SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

/// Handle to a running [`serve`] instance. Dropping it (or calling
/// [`ServerHandle::shutdown`] explicitly) unblocks the server's
/// `incoming_requests()` loop via `tiny_http`'s `Server::unblock`, releases
/// the bound port, and gives workers a bounded grace period to finish. A
/// worker stuck in an uninterruptible tiny_http body read is detached.
pub struct ServerHandle {
    addr: SocketAddr,
    server: Arc<Server>,
    thread: Option<JoinHandle<()>>,
    workers: Vec<WorkerHandle>,
    counters: Arc<Counters>,
}

struct WorkerHandle {
    thread: Option<JoinHandle<()>>,
    finished: Receiver<()>,
}

/// What the receiver has seen since it started.
///
/// Exposed because "the exporter is talking to us" is a different fact from
/// "events reached the spool", and a debugging surface that cannot tell
/// them apart cannot answer the first question a user of this receiver
/// asks. Counted at the HTTP boundary, so a batch that normalized to zero
/// events still registers as an accepted request.
#[derive(Debug, Default)]
pub struct Counters {
    logs_requests: AtomicU64,
    logs_accepted: AtomicU64,
    logs_dropped: AtomicU64,
    logs_unclaimed: AtomicU64,
    logs_persistence_failures: AtomicU64,
    logs_persistence_failed_events: AtomicU64,
    bodies_quarantined: AtomicU64,
    metrics_discarded: AtomicU64,
    requests_overloaded: AtomicU64,
}

impl Counters {
    /// `POST /v1/logs` requests received, whatever came of them.
    pub fn logs_requests(&self) -> u64 {
        self.logs_requests.load(Ordering::Relaxed)
    }

    /// Contract #1 events normalized out of those requests and spooled.
    pub fn logs_accepted(&self) -> u64 {
        self.logs_accepted.load(Ordering::Relaxed)
    }

    /// Records claimed by an installed normalizer and then not mappable to
    /// Contract #1 (missing model, unusable timeUnixNano, ...).
    ///
    /// These are *lost events*, not lost requests: the batch they arrived
    /// in was accepted and 200'd. Exposed so the control plane's health
    /// totals can count them — an upstream Claude Code change that breaks
    /// this normalizer's assumptions otherwise shows up only as telemetry
    /// quietly going missing.
    pub fn logs_dropped(&self) -> u64 {
        self.logs_dropped.load(Ordering::Relaxed)
    }

    /// Valid OTLP log records no installed normalizer claimed. These are not
    /// malformed and are not quarantined; the count exposes coverage gaps.
    pub fn logs_unclaimed(&self) -> u64 {
        self.logs_unclaimed.load(Ordering::Relaxed)
    }

    /// `/v1/logs` requests whose normalized events could not all be appended.
    pub fn logs_persistence_failures(&self) -> u64 {
        self.logs_persistence_failures.load(Ordering::Relaxed)
    }

    /// Normalized events in requests that received a retryable persistence
    /// failure. They are not included in [`Self::logs_accepted`].
    pub fn logs_persistence_failed_events(&self) -> u64 {
        self.logs_persistence_failed_events.load(Ordering::Relaxed)
    }

    /// `/v1/logs` bodies written to `rejected/`: unparseable JSON, or a
    /// batch containing at least one dropped record. Counted per body, not
    /// per record.
    pub fn bodies_quarantined(&self) -> u64 {
        self.bodies_quarantined.load(Ordering::Relaxed)
    }

    /// `POST /v1/metrics` bodies read and thrown away — the PoC has no
    /// Contract #1 shape for raw OTel metrics.
    pub fn metrics_discarded(&self) -> u64 {
        self.metrics_discarded.load(Ordering::Relaxed)
    }

    /// Requests rejected because all workers and queue slots were occupied.
    pub fn requests_overloaded(&self) -> u64 {
        self.requests_overloaded.load(Ordering::Relaxed)
    }
}

impl ServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Live request/event counters, cloneable and readable while the
    /// server runs.
    pub fn counters(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.server.unblock();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        for worker in &mut self.workers {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if worker.finished.recv_timeout(remaining).is_ok() {
                if let Some(thread) = worker.thread.take() {
                    let _ = thread.join();
                }
            } else {
                worker.thread.take();
            }
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the OTLP http/json receiver on `addr` (port 0 picks an ephemeral
/// free port — read it back via [`ServerHandle::addr`]) in a background
/// thread, spooling normalized events under `spool_dir` (created if
/// missing) and quarantining unparseable `/v1/logs` bodies, plus batches
/// containing `claude_code.api_request` records that were identified but
/// couldn't be mapped, under `spool_dir`'s sibling `rejected/` directory.
pub fn serve(addr: SocketAddr, spool_dir: PathBuf) -> anyhow::Result<ServerHandle> {
    let server = Server::http(addr)
        .map_err(|err| anyhow::anyhow!("af-otlp: failed to bind {addr}: {err}"))?;
    let bound_addr = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| anyhow::anyhow!("af-otlp: server is not bound to an IP address"))?;
    let server = Arc::new(server);

    let counters = Arc::new(Counters::default());
    let (request_tx, request_rx) = mpsc::sync_channel(REQUEST_QUEUE_CAPACITY);
    let request_rx = Arc::new(Mutex::new(request_rx));
    let mut workers = Vec::with_capacity(WORKER_COUNT);
    for _ in 0..WORKER_COUNT {
        workers.push(spawn_worker(
            Arc::clone(&request_rx),
            spool_dir.clone(),
            Arc::clone(&counters),
        ));
    }

    let thread_server = Arc::clone(&server);
    let thread_counters = Arc::clone(&counters);
    let thread = std::thread::spawn(move || {
        for request in thread_server.incoming_requests() {
            match request_tx.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request)) => {
                    thread_counters
                        .requests_overloaded
                        .fetch_add(1, Ordering::Relaxed);
                    respond_retryable(
                        request,
                        503,
                        "receiver_overloaded",
                        "all ingestion workers and queue slots are occupied",
                    );
                }
                Err(TrySendError::Disconnected(request)) => {
                    respond_retryable(
                        request,
                        503,
                        "receiver_shutting_down",
                        "ingestion workers are unavailable",
                    );
                    break;
                }
            }
        }
    });

    Ok(ServerHandle {
        addr: bound_addr,
        server,
        thread: Some(thread),
        workers,
        counters,
    })
}

fn spawn_worker(
    request_rx: Arc<Mutex<Receiver<tiny_http::Request>>>,
    spool_dir: PathBuf,
    counters: Arc<Counters>,
) -> WorkerHandle {
    let (finished_tx, finished) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        loop {
            let request = {
                let receiver = request_rx.lock().unwrap_or_else(|err| err.into_inner());
                receiver.recv()
            };
            let Ok(request) = request else {
                break;
            };
            handle_request(request, &spool_dir, &counters);
        }
        let _ = finished_tx.send(());
    });
    WorkerHandle {
        thread: Some(thread),
        finished,
    }
}

fn handle_request(request: tiny_http::Request, spool_dir: &Path, counters: &Counters) {
    // DNS-rebinding guard: a page on the open web can resolve its own
    // hostname to 127.0.0.1 and POST here, but the `Host` it sends is
    // still its own. An absent `Host` is allowed — a legitimate exporter
    // always sends one, and a client that omits it cannot be a rebinding
    // victim either.
    if let Some(host) = header_value(&request, "Host") {
        if !is_loopback_authority(&host) {
            respond(request, 403, "forbidden host");
            return;
        }
    }

    if *request.method() != Method::Post {
        respond(request, 404, "not found");
        return;
    }

    let url = request.url().to_string();
    match url.as_str() {
        "/v1/logs" => handle_logs(request, spool_dir, counters),
        "/v1/metrics" => handle_metrics(request, counters),
        _ => respond(request, 404, "not found"),
    }
}

/// Reads at most [`MAX_BODY_BYTES`] from the request.
///
/// `Ok(None)` means the client sent more than the cap; the caller answers
/// `413`. Reading `cap + 1` is what distinguishes "exactly at the limit"
/// from "over it" without reading the whole oversized body.
fn read_capped_body(request: &mut tiny_http::Request) -> io::Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 > MAX_BODY_BYTES {
        return Ok(None);
    }
    Ok(Some(body))
}

/// `POST /v1/metrics` — read the body (draining the connection cleanly)
/// and discard it. No Contract #1 shape for raw OTel metrics in this PoC;
/// Claude Code's exporter only needs a 2xx to consider the batch
/// delivered.
fn handle_metrics(mut request: tiny_http::Request, counters: &Counters) {
    // A read error still counts and still 200s: the body is discarded
    // either way, and the exporter must not back off over it.
    if let Ok(None) = read_capped_body(&mut request) {
        respond(request, 413, "payload too large");
        return;
    }
    counters.metrics_discarded.fetch_add(1, Ordering::Relaxed);
    respond(request, 200, "{}");
}

fn handle_logs(mut request: tiny_http::Request, spool_dir: &Path, counters: &Counters) {
    counters.logs_requests.fetch_add(1, Ordering::Relaxed);
    let raw = match read_capped_body(&mut request) {
        Ok(Some(raw)) => raw,
        // Over the cap is the one case that does *not* get the receiver's
        // usual "always 200 so the exporter never backs off": there is no
        // partial success to report, and an exporter that sends 5 MiB
        // batches needs to be told, not silently truncated.
        Ok(None) => {
            respond(request, 413, "payload too large");
            return;
        }
        Err(_) => {
            respond_retryable(
                request,
                503,
                "body_read_failed",
                "request body could not be read; retry the export",
            );
            return;
        }
    };
    let body = String::from_utf8_lossy(&raw).into_owned();

    let value: serde_json::Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => {
            // Never break the agent's exporter over a body we couldn't
            // parse — quarantine it for later inspection and still 200.
            counters.bodies_quarantined.fetch_add(1, Ordering::Relaxed);
            quarantine_raw(spool_dir, body.as_bytes(), UNPARSED_PREFIX);
            respond(request, 200, r#"{"partialSuccess":{}}"#);
            return;
        }
    };

    let outcome = normalize_logs(&value);
    counters
        .logs_unclaimed
        .fetch_add(outcome.unclaimed as u64, Ordering::Relaxed);
    if let Err(err) = append_envelopes(spool_dir, &outcome.events) {
        counters
            .logs_persistence_failures
            .fetch_add(1, Ordering::Relaxed);
        counters
            .logs_persistence_failed_events
            .fetch_add(outcome.events.len() as u64, Ordering::Relaxed);
        eprintln!(
            "af[otlp] error: persistence_failed events={} spool={} error={err}",
            outcome.events.len(),
            spool_dir.display(),
        );
        respond_retryable(
            request,
            503,
            "persistence_failed",
            &format!(
                "failed to append {} normalized event(s); retry the export",
                outcome.events.len()
            ),
        );
        return;
    }
    counters
        .logs_accepted
        .fetch_add(outcome.events.len() as u64, Ordering::Relaxed);

    // A logRecord claimed by an installed normalizer that we then couldn't
    // map (missing/malformed required fields)
    // must not vanish silently — quarantine the whole batch it came in and
    // log the count, so an upstream Claude Code change that breaks this
    // normalizer's assumptions is observable rather than a quiet event loss.
    if outcome.dropped > 0 {
        counters
            .logs_dropped
            .fetch_add(outcome.dropped as u64, Ordering::Relaxed);
        counters.bodies_quarantined.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "af[otlp] warn: dropped {} claimed OTLP record(s) that failed to normalize",
            outcome.dropped
        );
        quarantine_raw(spool_dir, body.as_bytes(), DROPPED_PREFIX);
    }

    respond(request, 200, r#"{"partialSuccess":{}}"#);
}

fn respond(request: tiny_http::Request, status: u16, body: &str) {
    let response = Response::from_string(body.to_string()).with_status_code(status);
    let _ = request.respond(response);
}

fn respond_retryable(request: tiny_http::Request, status: u16, code: &str, message: &str) {
    let body = serde_json::json!({
        "error": {
            "code": code,
            "message": message,
            "retryable": true,
        }
    });
    respond(request, status, &body.to_string());
}

fn header_value(request: &tiny_http::Request, field: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(field))
        .map(|h| h.value.as_str().to_string())
}

/// `localhost`, `127.0.0.0/8` or `::1`, with an optional numeric port.
///
/// Deliberately strict, and deliberately *not* resolving names: resolution
/// is precisely the step DNS rebinding subverts. Must keep accepting what
/// a real agent exporter sends — Claude Code's is `127.0.0.1:4318`.
fn is_loopback_authority(authority: &str) -> bool {
    let (host, port) = split_authority(authority);
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    if host == "localhost" || host == "::1" {
        return true;
    }
    let mut octets = host.split('.');
    let Some("127") = octets.next() else {
        return false;
    };
    let rest: Vec<&str> = octets.collect();
    rest.len() == 3
        && rest.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

/// Splits `host[:port]`, keeping IPv6 literals — bracketed or not — intact.
fn split_authority(authority: &str) -> (&str, Option<&str>) {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, "")) => (host, None),
            Some((host, tail)) => (host, tail.strip_prefix(':').or(Some(tail))),
            None => (authority, None),
        };
    }
    if authority.matches(':').count() > 1 {
        return (authority, None);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    }
}

/// Appends normalized envelopes to
/// `spool_dir/<collector>.<session_id>.jsonl`, one file per collector and
/// session, one `write_all` call per line (a single line is
/// small enough that this is effectively the atomic single `write()`
/// collectors are required to use.
fn append_envelopes(spool_dir: &Path, envelopes: &[af_events::Envelope]) -> io::Result<()> {
    if envelopes.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(spool_dir)?;

    let mut by_collector_session: BTreeMap<(&str, &str), Vec<String>> = BTreeMap::new();
    for envelope in envelopes {
        let mut line = serde_json::to_string(envelope).expect("Envelope always serializes to JSON");
        line.push('\n');
        by_collector_session
            .entry((
                envelope.collector.name.as_str(),
                envelope.session_id.as_str(),
            ))
            .or_default()
            .push(line);
    }

    for ((collector, session_id), lines) in by_collector_session {
        // Sanitized here, at the call site, even though `normalize` already
        // did it: this is the line that turns a string into a path, and the
        // guarantee belongs next to the risk. `af_spool::spool_file_name`
        // deliberately does *not* sanitize — it owns the filename grammar,
        // not the trust boundary, and a caller whose ids are already safe
        // must not pay for a reduction it doesn't need. `sanitize_id` is
        // idempotent, so an already-clean id is unchanged.
        let path = spool_dir.join(af_spool::spool_file_name(
            collector,
            &sanitize_id(session_id),
        ));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        for line in lines {
            file.write_all(line.as_bytes())?;
        }
    }

    Ok(())
}

/// Filename prefix for quarantined bodies that couldn't be parsed as JSON
/// at all.
const UNPARSED_PREFIX: &str = "otlp-cc.unparsed";
/// Filename prefix for quarantined batches that parsed fine but contained
/// at least one `claude_code.api_request` record `normalize_logs` couldn't
/// map.
const DROPPED_PREFIX: &str = "otlp-cc.dropped";

/// Writes a `/v1/logs` body to
/// `<spool_dir sibling>/rejected/<prefix>.<unix_millis>[-N].txt` via
/// [`af_spool::quarantine_bytes`], so the collision-suffix convention has
/// one implementation shared with the spool's own line quarantine and two
/// rejects in the same millisecond can't clobber each other. Best-effort:
/// failures are logged to stderr, never propagated (a full disk can't be
/// allowed to break the receiver's 200-always contract).
fn quarantine_raw(spool_dir: &Path, body: &[u8], prefix: &str) {
    let rejected_dir = spool_dir
        .parent()
        .map(|parent| parent.join("rejected"))
        .unwrap_or_else(|| spool_dir.join("rejected"));

    if let Err(err) = af_spool::quarantine_bytes(&rejected_dir, prefix, body) {
        eprintln!(
            "af[otlp] error: failed to quarantine OTLP body ({prefix}) under {}: {err}",
            rejected_dir.display()
        );
    }
}
