//! The `tiny_http`-backed HTTP server: `POST /v1/logs` and `POST
//! /v1/metrics`, a plain-text `GET /` saying what the port is, everything
//! else 404s. Kept separate from [`crate::normalize`]
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
//! - requests labelled by a browser (`Origin`, a cross-site
//!   `Sec-Fetch-Site`) get `403`, and anything but `application/json` gets
//!   `415`, so a web page cannot write here;
//! - nothing is written to disk: normalized events and metadata-only
//!   rejects go to the owner's bounded channel, and a full channel is a
//!   retryable `503`.
//!
//! tiny_http 0.12 does not expose the body socket for a read timeout. To keep
//! one slow client from serializing ingestion, requests run on a bounded
//! worker pool, and to keep a few stalled clients from holding every worker,
//! a body is read on a thread of its own that the worker waits for only
//! [`BODY_READ_DEADLINE`]: past it the worker moves on and the read is left
//! to end when its client closes (at most [`MAX_BODY_READS`] at a time).
//! Excess work receives a retryable `503`, and shutdown waits only a
//! bounded time for a worker.

use std::io::{self, Read};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tiny_http::{Method, Response, Server};

use crate::inbound::{Inbound, Reject};
use crate::normalize::{normalize_logs, NormalizeOutcome, MAX_LOG_RECORDS};

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
/// How long a worker waits for a request's body. An exporter on loopback
/// sends its few KiB at once; ten seconds is a client that has stopped.
const BODY_READ_DEADLINE: Duration = Duration::from_secs(10);
/// Body reads in progress at once, abandoned ones included: each is a
/// parked thread and a buffer of at most [`MAX_BODY_BYTES`]. Past this the
/// receiver answers a retryable `503` without reading.
const MAX_BODY_READS: u64 = 64;

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
/// "events reached the store", and a debugging surface that cannot tell
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
    bodies_timed_out: AtomicU64,
    /// Body reads in progress right now (a gauge, not a count).
    body_reads: AtomicU64,
}

impl Counters {
    /// `POST /v1/logs` requests received, whatever came of them.
    pub fn logs_requests(&self) -> u64 {
        self.logs_requests.load(Ordering::Relaxed)
    }

    /// Contract #1 events normalized out of those requests and handed to the
    /// sink.
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
    /// malformed and are not rejected; the count exposes coverage gaps.
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

    /// `/v1/logs` bodies reported to the sink as a metadata-only reject
    /// (the body itself is not kept): unparseable JSON, too many records, or
    /// a batch containing at least one dropped record. Counted per body, not
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

    /// Requests given up on because their body did not arrive in time.
    pub fn bodies_timed_out(&self) -> u64 {
        self.bodies_timed_out.load(Ordering::Relaxed)
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
/// thread. Normalized events and metadata-only rejects are sent to `sink`;
/// the receiver writes nothing to disk.
pub fn serve(addr: SocketAddr, sink: SyncSender<Inbound>) -> anyhow::Result<ServerHandle> {
    serve_with_body_deadline(addr, sink, BODY_READ_DEADLINE)
}

/// [`serve`] with the time a request's body is waited for given (tests use
/// a short one).
pub fn serve_with_body_deadline(
    addr: SocketAddr,
    sink: SyncSender<Inbound>,
    body_deadline: Duration,
) -> anyhow::Result<ServerHandle> {
    // Unauthenticated by design, so reachable only from this machine: the
    // Host check alone cannot stop a LAN client that sends `Host: 127.0.0.1`.
    if !addr.ip().is_loopback() {
        anyhow::bail!(
            "afoot-otlp: refusing to listen on {addr}: the receiver serves loopback addresses only"
        );
    }
    let server = Server::http(addr)
        .map_err(|err| anyhow::anyhow!("afoot-otlp: failed to bind {addr}: {err}"))?;
    let bound_addr = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| anyhow::anyhow!("afoot-otlp: server is not bound to an IP address"))?;
    let server = Arc::new(server);

    let counters = Arc::new(Counters::default());
    let (request_tx, request_rx) = mpsc::sync_channel(REQUEST_QUEUE_CAPACITY);
    let request_rx = Arc::new(Mutex::new(request_rx));
    let mut workers = Vec::with_capacity(WORKER_COUNT);
    for _ in 0..WORKER_COUNT {
        workers.push(spawn_worker(
            Arc::clone(&request_rx),
            sink.clone(),
            Arc::clone(&counters),
            body_deadline,
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
    sink: SyncSender<Inbound>,
    counters: Arc<Counters>,
    body_deadline: Duration,
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
            handle_request(request, &sink, &counters, body_deadline);
        }
        let _ = finished_tx.send(());
    });
    WorkerHandle {
        thread: Some(thread),
        finished,
    }
}

fn handle_request(
    request: tiny_http::Request,
    sink: &SyncSender<Inbound>,
    counters: &Arc<Counters>,
    body_deadline: Duration,
) {
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

    // A web page can POST to loopback with a CORS "simple request" (no
    // preflight) and the Host check passes, since the page addresses
    // 127.0.0.1 itself. Browsers always label such requests: they send
    // `Origin` on cross-origin POSTs and `Sec-Fetch-Site` on every fetch.
    // Agent exporters send neither (or `Sec-Fetch-Site: none`).
    let from_a_page = header_value(&request, "Origin").is_some()
        || header_value(&request, "Sec-Fetch-Site")
            .is_some_and(|site| !site.trim().eq_ignore_ascii_case("none"));
    if from_a_page {
        respond(request, 403, "forbidden origin");
        return;
    }

    if *request.method() != Method::Post {
        // Someone who opens the port in a browser learns what owns it. Only
        // static text: this port is unauthenticated, so it serves no data.
        let is_root = request.url() == "/";
        let is_read = matches!(request.method(), Method::Get | Method::Head);
        if is_root && is_read {
            respond(request, 200, &landing_text());
        } else {
            respond(
                request,
                404,
                "not found: this is the Agentic Footprint receiver (GET / says more)",
            );
        }
        return;
    }

    let url = request.url().to_string();
    if matches!(url.as_str(), "/v1/logs" | "/v1/metrics")
        && !is_json_content_type(header_value(&request, "Content-Type").as_deref())
    {
        // http/json only. Requiring the JSON media type also means a page
        // cannot reach this endpoint without a CORS preflight, which this
        // server never grants.
        respond(
            request,
            415,
            "unsupported media type: send application/json",
        );
        return;
    }
    match url.as_str() {
        "/v1/logs" => handle_logs(request, sink, counters, body_deadline),
        "/v1/metrics" => handle_metrics(request, counters, body_deadline),
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

/// Reads the request's body on a thread of its own and waits for it at most
/// `deadline`; gives the request back with what was read.
///
/// `None`: there is nothing left to answer. Either the body did not arrive
/// in time (the worker is free again; the read ends, and the connection is
/// closed, when the client gives up), or too many reads are already in
/// progress and the request was answered with a retryable `503`.
fn read_body_within(
    mut request: tiny_http::Request,
    deadline: Duration,
    counters: &Arc<Counters>,
) -> Option<(tiny_http::Request, io::Result<Option<Vec<u8>>>)> {
    if counters.body_reads.load(Ordering::Relaxed) >= MAX_BODY_READS {
        counters.requests_overloaded.fetch_add(1, Ordering::Relaxed);
        respond_retryable(
            request,
            503,
            "receiver_overloaded",
            "too many requests are waiting for their body",
        );
        return None;
    }
    let (done_tx, done) = mpsc::sync_channel(1);
    let reader_counters = Arc::clone(counters);
    counters.body_reads.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("afoot-otlp-body".to_string())
        .spawn(move || {
            let body = read_capped_body(&mut request);
            reader_counters.body_reads.fetch_sub(1, Ordering::Relaxed);
            // Nobody waiting any more: the request is dropped here, which
            // closes its connection.
            let _ = done_tx.send((request, body));
        });
    if spawned.is_err() {
        // The request went with the closure: its connection is closed.
        counters.body_reads.fetch_sub(1, Ordering::Relaxed);
        return None;
    }
    match done.recv_timeout(deadline) {
        Ok(read) => Some(read),
        Err(_) => {
            counters.bodies_timed_out.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// `POST /v1/metrics` — read the body (draining the connection cleanly)
/// and discard it. No Contract #1 shape for raw OTel metrics in this PoC;
/// Claude Code's exporter only needs a 2xx to consider the batch
/// delivered.
fn handle_metrics(request: tiny_http::Request, counters: &Arc<Counters>, body_deadline: Duration) {
    let Some((request, body)) = read_body_within(request, body_deadline, counters) else {
        return;
    };
    // A read error still counts and still 200s: the body is discarded
    // either way, and the exporter must not back off over it.
    if let Ok(None) = body {
        respond(request, 413, "payload too large");
        return;
    }
    counters.metrics_discarded.fetch_add(1, Ordering::Relaxed);
    respond(request, 200, "{}");
}

fn handle_logs(
    request: tiny_http::Request,
    sink: &SyncSender<Inbound>,
    counters: &Arc<Counters>,
    body_deadline: Duration,
) {
    counters.logs_requests.fetch_add(1, Ordering::Relaxed);
    let Some((request, body)) = read_body_within(request, body_deadline, counters) else {
        return;
    };
    let raw = match body {
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

    let value: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => {
            // Never break the agent's exporter over a body we couldn't
            // parse — record why (not what) and still 200.
            counters.bodies_quarantined.fetch_add(1, Ordering::Relaxed);
            send_reject(
                sink,
                "body is not valid JSON".to_string(),
                Vec::new(),
                raw.len(),
            );
            respond(request, 200, r#"{"partialSuccess":{}}"#);
            return;
        }
    };

    let NormalizeOutcome {
        events,
        dropped,
        unclaimed,
        dropped_keys,
        over_capacity,
    } = normalize_logs(&value);
    if over_capacity {
        counters.bodies_quarantined.fetch_add(1, Ordering::Relaxed);
        send_reject(
            sink,
            format!("more than {MAX_LOG_RECORDS} log records in one request"),
            Vec::new(),
            raw.len(),
        );
        respond(request, 413, "too many log records");
        return;
    }
    counters
        .logs_unclaimed
        .fetch_add(unclaimed as u64, Ordering::Relaxed);

    let accepted = events.len();
    if accepted > 0 {
        let refused = match sink.try_send(Inbound::Facts(events)) {
            Ok(()) => None,
            Err(TrySendError::Full(_)) => {
                Some(("receiver_overloaded", "the store is busy; retry the export"))
            }
            Err(TrySendError::Disconnected(_)) => {
                Some(("receiver_shutting_down", "the receiver is stopping"))
            }
        };
        if let Some((code, message)) = refused {
            counters
                .logs_persistence_failures
                .fetch_add(1, Ordering::Relaxed);
            counters
                .logs_persistence_failed_events
                .fetch_add(accepted as u64, Ordering::Relaxed);
            respond_retryable(request, 503, code, message);
            return;
        }
    }
    counters
        .logs_accepted
        .fetch_add(accepted as u64, Ordering::Relaxed);

    // A logRecord claimed by an installed normalizer that we then couldn't
    // map (missing/malformed required fields) must not vanish silently:
    // reject it with its attribute names, so an upstream schema change that
    // breaks a normalizer's assumptions is observable rather than a quiet
    // event loss.
    if dropped > 0 {
        counters
            .logs_dropped
            .fetch_add(dropped as u64, Ordering::Relaxed);
        counters.bodies_quarantined.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "afoot[otlp] warn: dropped {dropped} claimed OTLP record(s) that failed to normalize"
        );
        send_reject(
            sink,
            format!("{dropped} claimed record(s) failed to normalize"),
            dropped_keys,
            raw.len(),
        );
    }

    respond(request, 200, r#"{"partialSuccess":{}}"#);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Best effort: a reject that cannot be queued is counted, not retried.
fn send_reject(
    sink: &SyncSender<Inbound>,
    reason: String,
    attribute_keys: Vec<String>,
    bytes: usize,
) {
    let _ = sink.try_send(Inbound::Reject(Reject {
        ts_ms: now_ms(),
        source: "otlp".to_string(),
        reason,
        attribute_keys,
        bytes: bytes as u64,
    }));
}

/// What `GET /` answers: what this port is, and where the results are.
fn landing_text() -> String {
    format!(
        "Agentic Footprint receiver (afoot {version})\n\
         \n\
         This port receives telemetry from coding agents so afoot can estimate\n\
         the environmental impact of their work. There is nothing to browse\n\
         here: run `afoot --help` to see the results on the command line.\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

fn respond(request: tiny_http::Request, status: u16, body: &str) {
    let mut response = Response::from_string(body.to_string()).with_status_code(status);
    if let Ok(header) = tiny_http::Header::from_bytes(
        crate::RECEIVER_HEADER.as_bytes(),
        env!("CARGO_PKG_VERSION").as_bytes(),
    ) {
        response.add_header(header);
    }
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

/// `application/json`, ignoring case and any `; charset=...` parameters.
fn is_json_content_type(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
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
