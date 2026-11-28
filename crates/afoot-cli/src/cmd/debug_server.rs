//! The `/debug` HTTP + SSE surface consumed by the debug console
//! (`docs/contracts/debug-console/DATA-CONTRACT.md` §2).
//!
//! Runs inside `afoot watch --debug` only — it is a development-phase server,
//! bound to loopback, no auth, no writes. Seven routes:
//!
//! | Route | Contract |
//! |---|---|
//! | `GET /debug/session` | §2.1 bootstrap |
//! | `GET /debug/snapshot?window=Ns` | §2.2 backfill |
//! | `GET /debug/stream[?from=N]` | §2.3 SSE |
//! | `GET /debug/alloc/{sample_event_id}` | §2.4 allocation trace |
//! | `GET /debug/report[?level=…]` | §2.6 Contract #2 |
//! | `GET /debug/health` | §2.7 collectors, rejects, python |
//! | `GET /debug/status` | watcher boot id, receiver, one row per agent |
//!
//! **Frames are the source of truth for the stream, the snapshot and the
//! `Last-Event-ID` replay alike.** [`DebugState`] keeps one monotonically
//! numbered ring of frames; the snapshot is a projection of that ring over
//! a time window, and a reconnecting client is replayed the same frames
//! from the same buffer. One log, three views: a snapshot that came from a
//! different code path than the stream is how a console ends up showing a
//! chart that disagrees with its own event table.
//! Full reports are the deliberate exception: only the latest payload per
//! session is retained, while the frame log carries a compact versioned
//! invalidation that tells clients to refresh `/debug/report`.
//!
//! **The server is loopback-only, and enforces that rather than assuming
//! it.** Binding to `127.0.0.1` stops remote sockets, but it does not stop
//! a page on the open web from pointing a `fetch` or a DNS rebind at the
//! port and reading a developer's telemetry. Three checks close that:
//!
//! - `Access-Control-Allow-Origin` **reflects** the request's `Origin` when
//!   it is a loopback origin, and is omitted entirely otherwise. The
//!   console is served by a Vite dev server on another port during
//!   development, so some CORS header is required; `*` handed the same
//!   permission to `https://evil.example`.
//! - A request whose `Host` is present and not loopback is refused with
//!   `403`. That is the DNS-rebinding guard: the attacker's name resolves
//!   to `127.0.0.1`, but the `Host` it sends is still their domain.
//! - Concurrent request-handling threads are capped
//!   ([`MAX_HANDLER_THREADS`]); over the cap the server answers `503`
//!   rather than spawning without bound.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tiny_http::{Header, Request, Response, Server};

/// How many frames the ring keeps. At the sampler's default 5s cadence a
/// busy session emits a few frames a second, so this is many minutes of
/// replay — and the cap is what stops a week-long resident `afoot watch` from
/// growing without bound. A client whose `Last-Event-ID` has fallen out of
/// the ring is told to re-snapshot (`reset`) rather than silently handed a
/// hole.
const RING_CAPACITY: usize = 8192;

/// Bound on distinct sessions whose `session`/`report` payloads are kept.
/// A working day of agent sessions is tens, not hundreds; past the cap the
/// oldest-published session's payloads age out exactly like ring frames.
const SESSION_CAP: usize = 256;

/// Frames queued per SSE connection before it is considered wedged. A slow
/// reader is disconnected rather than allowed to stall the writer that
/// feeds every other connection.
///
/// This bounds only the **live** fan-out. Replay is never queued here (see
/// [`DebugState::subscribe`]) — a replay that overflowed a queue would be a
/// silent hole in the client's history, which is exactly what the `reset`
/// frame exists to avoid.
const CONNECTION_QUEUE: usize = 1024;

/// How long a stream stays silent before the server writes a `ping` event.
///
/// Two jobs. The client cannot tell a quiet watcher from a dead connection
/// without it — a flush is a minute apart by default and an idle watcher
/// publishes nothing — so it treats three missed pings as a lost stream.
/// And the write is how the server notices a client that vanished during a
/// quiet period: until something is written, a closed socket and an idle
/// one look the same, and the handler thread waits on its channel forever.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// How long [`DebugServer::stop`] waits for the accept thread before
/// detaching it. Shutdown continues either way: a thread still blocked on a
/// client that will not read must not become the reason `afoot watch` refuses
/// to exit.
const JOIN_DEADLINE: Duration = Duration::from_secs(2);

/// Ceiling on request-handling threads alive at once.
///
/// Every request gets a thread, because `Request::respond` blocks until the
/// client drains the socket (see [`dispatch`]). That is fine per request
/// and unbounded in aggregate: a client that opens connections and never
/// reads them holds a thread each, and a few thousand of those exhaust the
/// process. 32 is far above what one console needs — it holds one SSE
/// stream and issues a handful of one-shot fetches — and far below anything
/// that threatens the host. Over the cap the server answers `503` on the
/// accept thread, which is a stated refusal rather than a hang.
const MAX_HANDLER_THREADS: usize = 32;

/// Threads currently handling a request. Paired with [`HandlerSlot`], which
/// decrements on drop so a panicking handler still returns its slot.
static ACTIVE_HANDLERS: AtomicUsize = AtomicUsize::new(0);

/// A reservation against [`MAX_HANDLER_THREADS`], released on drop.
struct HandlerSlot;

impl Drop for HandlerSlot {
    fn drop(&mut self) {
        ACTIVE_HANDLERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Reserves one handler slot, or `None` when the server is already at
/// [`MAX_HANDLER_THREADS`].
///
/// Compare-and-swap rather than `fetch_add` + check: the latter briefly
/// exceeds the cap under concurrent accepts, and "briefly" is exactly the
/// window an attacker opening connections in a loop lives in.
fn acquire_handler_slot() -> Option<HandlerSlot> {
    let mut current = ACTIVE_HANDLERS.load(Ordering::Acquire);
    loop {
        if current >= MAX_HANDLER_THREADS {
            return None;
        }
        match ACTIVE_HANDLERS.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Some(HandlerSlot),
            Err(actual) => current = actual,
        }
    }
}

/// The cursor a client sends when it has seen no frames at all.
///
/// `as_of_seq` is `-1` on an empty ring rather than `0`, because `0` is a
/// real frame number: a console that snapshots an empty session and then
/// subscribes from `0` would never receive frame `0`. `null` was the other
/// candidate and was rejected after reading the console —
/// `console/src/lib/client/afootClient.svelte.ts` types `as_of_seq` as `number`
/// and interpolates it straight into `?from=${fromSeq}`, so a `null` would
/// arrive here as the literal string `"null"` and be silently downgraded to
/// "no cursor". `-1` survives that round trip unchanged.
const EMPTY_CURSOR: i64 = -1;

/// One numbered frame in the log.
#[derive(Debug, Clone)]
pub struct Frame {
    pub seq: u64,
    pub event: &'static str,
    pub data: Value,
    /// Epoch milliseconds the frame was recorded, for the snapshot's time
    /// window. Not on the wire: the data's own `ts` is authoritative for
    /// anything the console displays.
    pub at_ms: i64,
}

/// Everything the `/debug` routes serve, and the fan-out to live SSE
/// connections.
///
/// Snapshot-shaped state (`session`, `report`, `health`, `watchdog`) is
/// replace-on-arrival; the frame ring is append-only.
pub struct DebugState {
    next_seq: u64,
    frames: VecDeque<Frame>,
    /// Session payloads by `session_id`, replace-on-arrival per session —
    /// several agents' sessions coexist, and the last-processed one must
    /// not erase the others (the single-`Option` this replaced did exactly
    /// that). Bounded by [`SESSION_CAP`] in first-publication order.
    sessions: std::collections::BTreeMap<String, Value>,
    /// One row per session known to the store, for the picker: what there
    /// is to say about a session nobody has rebuilt since a console
    /// attached. A `session` frame for the same id supersedes its row.
    summaries: std::collections::BTreeMap<String, Value>,
    /// How many streams are open right now — see [`DebugState::attached`].
    attached: usize,
    /// Sessions a console asked for that nothing here describes yet, for
    /// the watcher to rebuild. Bounded by [`WANTED_CAP`].
    wanted: std::collections::BTreeSet<String>,
    session_order: VecDeque<String>,
    /// Session-level reports by `session_id`, same shape and bound. Full
    /// report payloads live only here; the frame ring and subscriber queues
    /// receive compact version notifications instead.
    reports: std::collections::BTreeMap<String, Value>,
    pending_report_evictions: VecDeque<String>,
    health: Option<Value>,
    /// The watcher-level status (`GET /debug/status`): boot id, receiver,
    /// one row per agent. Replace-on-arrival like `health`, but unlike it
    /// independent of any session, so it exists before the first pass.
    status: Option<Value>,
    watchdog: Vec<Value>,
    /// Allocation traces by `sample_event_id`, for `GET /debug/alloc/{id}`.
    ///
    /// Bounded by [`DebugState::ring_capacity`], the same horizon the frame
    /// ring keeps, with `alloc_order` recording first-publication order so
    /// the oldest goes first. Unbounded, this map was the one structure in
    /// a resident `afoot watch` that grew for the life of the process: the
    /// frame ring evicts, the connections come and go, and this kept one
    /// full allocation trace per energy sample ever apportioned — a
    /// day-long session at a 5 s sampler cadence is tens of thousands of
    /// them. An id that has fallen out is a `404`, which is the same answer
    /// the ring gives for a frame that has aged out of it.
    allocs: std::collections::BTreeMap<String, Value>,
    alloc_order: VecDeque<String>,
    connections: Vec<SyncSender<Vec<u8>>>,
    /// Ring size and per-connection queue size. Fields rather than the bare
    /// consts so a test can drive the eviction and back-pressure paths with
    /// tens of frames instead of tens of thousands.
    ring_capacity: usize,
    queue_capacity: usize,
    /// How long a stream may stay silent before it is sent a `ping`.
    heartbeat: Duration,
}

impl Default for DebugState {
    fn default() -> Self {
        DebugState::with_limits(RING_CAPACITY, CONNECTION_QUEUE)
    }
}

/// What a new subscriber is written before it joins the live fan-out.
pub(crate) enum Replay {
    /// Every frame after the client's cursor, oldest first, already on the
    /// wire. Written straight to the socket by the connection's own thread,
    /// so its size is bounded by the ring and not by any queue.
    Frames(Vec<Vec<u8>>),
    /// The cursor is older than the ring: the client must re-snapshot.
    /// Anything else would hand it a history with an invisible hole.
    Reset,
}

impl DebugState {
    pub fn with_limits(ring_capacity: usize, queue_capacity: usize) -> Self {
        Self::with_limits_and_heartbeat(ring_capacity, queue_capacity, HEARTBEAT)
    }

    /// [`DebugState::with_limits`] with the heartbeat interval too, so a
    /// test can watch pings without waiting [`HEARTBEAT`] for each.
    pub fn with_limits_and_heartbeat(
        ring_capacity: usize,
        queue_capacity: usize,
        heartbeat: Duration,
    ) -> Self {
        DebugState {
            next_seq: 0,
            frames: VecDeque::new(),
            sessions: std::collections::BTreeMap::new(),
            summaries: std::collections::BTreeMap::new(),
            attached: 0,
            wanted: std::collections::BTreeSet::new(),
            session_order: VecDeque::new(),
            reports: std::collections::BTreeMap::new(),
            pending_report_evictions: VecDeque::new(),
            health: None,
            status: None,
            watchdog: Vec::new(),
            allocs: std::collections::BTreeMap::new(),
            alloc_order: VecDeque::new(),
            connections: Vec::new(),
            ring_capacity: ring_capacity.max(1),
            queue_capacity: queue_capacity.max(1),
            heartbeat,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_attached(&mut self, streams: usize) {
        self.attached = streams;
    }

    #[cfg(test)]
    pub(crate) fn has_health(&self) -> bool {
        self.health.is_some()
    }

    #[cfg(test)]
    pub(crate) fn want(&mut self, session_id: &str) {
        self.wanted.insert(session_id.to_string());
    }

    /// How many frames of `event` the ring holds.
    #[cfg(test)]
    pub(crate) fn frames_of(&self, event: &str) -> usize {
        self.frames
            .iter()
            .filter(|frame| frame.event == event)
            .count()
    }

    /// Appends a frame to the log and pushes it to every live connection.
    pub fn publish(&mut self, event: &'static str, data: Value, at_ms: i64) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let mut wire_data = data.clone();

        if event == "alloc" {
            if let Some(id) = data.get("sample_event_id").and_then(Value::as_str) {
                // A re-published trace replaces its value and keeps its
                // place in the queue: eviction order is first publication,
                // so a sample that is republished every pass cannot pin
                // itself in the map ahead of newer ones forever.
                if self.allocs.insert(id.to_string(), data.clone()).is_none() {
                    self.alloc_order.push_back(id.to_string());
                }
                while self.alloc_order.len() > self.ring_capacity {
                    if let Some(oldest) = self.alloc_order.pop_front() {
                        self.allocs.remove(&oldest);
                    }
                }
            }
        }
        match event {
            "report" => {
                let mut stored = data.clone();
                if let Some(object) = stored.as_object_mut() {
                    object.insert("report_version".to_string(), json!(seq));
                }
                let evicted = self.store_session_payload("report", stored);
                if let Some(evicted) = evicted {
                    self.queue_report_eviction(evicted);
                }
                wire_data = Self::report_notification(
                    &data,
                    seq,
                    self.pending_report_evictions.drain(..).collect(),
                );
            }
            "session" => {
                if let Some(evicted) = self.store_session_payload("session", data.clone()) {
                    self.queue_report_eviction(evicted.clone());
                    if let Some(object) = wire_data.as_object_mut() {
                        object.insert("evicted_session_id".to_string(), json!(evicted));
                    }
                }
            }
            "health" => self.health = Some(data.clone()),
            "watchdog" => {
                self.watchdog = data
                    .get("pids")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            }
            _ => {}
        }

        self.fan_out(&sse_bytes(event, &wire_data, Some(seq)));

        self.frames.push_back(Frame {
            seq,
            event,
            data: wire_data,
            at_ms,
        });
        while self.frames.len() > self.ring_capacity {
            self.frames.pop_front();
        }
    }

    /// Replaces the watcher status and tells every live connection.
    ///
    /// Deliberately **not** a ring frame: it takes no sequence number, is
    /// not replayed and is not in the snapshot. A status is a current fact,
    /// not history — every connection is handed the latest one first (see
    /// [`DebugState::status_frame`]), which is all a replay could add.
    ///
    /// Returns whether anything was sent: a status equal to the stored one
    /// is not news, and is not repeated.
    pub fn set_status(&mut self, status: Value) -> bool {
        if self.status.as_ref() == Some(&status) {
            return false;
        }
        self.fan_out(&sse_bytes("status", &status, None));
        self.status = Some(status);
        true
    }

    /// Hands one wire frame to every live connection, dropping those that
    /// are gone or whose queue is full (a slow reader replays later).
    fn fan_out(&mut self, wire: &[u8]) {
        self.connections.retain(|tx| {
            !matches!(
                tx.try_send(wire.to_vec()),
                Err(mpsc::TrySendError::Disconnected(_)) | Err(mpsc::TrySendError::Full(_))
            )
        });
    }

    /// The stored status as the frame a new connection is written first,
    /// whatever its cursor.
    fn status_frame(&self) -> Option<Vec<u8>> {
        self.status
            .as_ref()
            .map(|status| sse_bytes("status", status, None))
    }

    /// Registers one SSE connection and returns what it owes the client
    /// before the live channel takes over.
    ///
    /// **This is the whole replay-integrity argument.** The replay is
    /// computed and the sender is pushed under the *same* lock, so `publish`
    /// either ran before (and its frame is in the replay) or after (and its
    /// frame goes down the channel) — never both, never neither. The replay
    /// itself is returned as a `Vec` rather than pushed through the channel:
    /// a `try_send` into a bounded queue drops frames once it is full, and a
    /// dropped *replay* frame is precisely the silent hole DATA-CONTRACT
    /// §2.3 forbids. Live frames may still be dropped when a reader stalls —
    /// but that drops the whole connection with them, and the client's
    /// reconnect replays from its `Last-Event-ID`.
    pub(crate) fn subscribe(&mut self, cursor: Option<i64>) -> (Replay, Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(self.queue_capacity);
        let replay = match cursor {
            Some(last) => {
                let oldest = self.frames.front().map(|frame| frame.seq as i64);
                // Two ways a cursor cannot be served. Older than the ring:
                // the frames after it are gone. Ahead of the head: this
                // process never issued it, so it belongs to an earlier
                // watcher — sequence numbers restart at zero with the
                // process, and answering "you are up to date" would leave
                // that client holding another process's history for good.
                let issued = last < self.next_seq as i64;
                let in_ring = oldest
                    .map(|oldest| last.saturating_add(1) >= oldest)
                    // An empty ring holds nothing to replay: only a
                    // client already at the head is up to date. Frames
                    // dropped while nobody was attached are gone.
                    .unwrap_or(last.saturating_add(1) >= self.next_seq as i64);
                let replayable = issued && in_ring;
                if replayable {
                    Replay::Frames(
                        self.frames
                            .iter()
                            .filter(|frame| (frame.seq as i64) > last)
                            .map(|frame| sse_bytes(frame.event, &frame.data, Some(frame.seq)))
                            .collect(),
                    )
                } else {
                    Replay::Reset
                }
            }
            None => {
                // A fresh client with no cursor still needs the current
                // sessions/reports/health immediately — they are
                // replace-on-arrival and the next periodic ones may be a
                // minute away. Every session's pair is sent, not just the
                // latest: the client's picker needs the full set.
                let mut frames = Vec::new();
                for session in self.sessions.values() {
                    frames.push(sse_bytes("session", session, None));
                }
                for report in self.reports.values() {
                    frames.push(sse_bytes(
                        "report",
                        &Self::report_notification(
                            report,
                            report
                                .get("report_version")
                                .and_then(Value::as_u64)
                                .unwrap_or(0),
                            Vec::new(),
                        ),
                        None,
                    ));
                }
                if let Some(health) = &self.health {
                    frames.push(sse_bytes("health", health, None));
                }
                Replay::Frames(frames)
            }
        };
        self.connections.push(tx);
        (replay, rx)
    }

    /// Replace-on-arrival keyed by the payload's own `session_id`, bounded
    /// across both session and report maps in first-publication order. A
    /// payload without a `session_id` is dropped — storing it under a
    /// made-up key would invent a session the spool never named.
    fn store_session_payload(&mut self, event: &'static str, data: Value) -> Option<String> {
        let session_id = data.get("session_id").and_then(Value::as_str)?;
        let session_id = session_id.to_string();
        let is_new =
            !self.sessions.contains_key(&session_id) && !self.reports.contains_key(&session_id);
        match event {
            "session" => {
                self.sessions.insert(session_id.clone(), data);
            }
            "report" => {
                self.reports.insert(session_id.clone(), data);
            }
            _ => unreachable!("only session-scoped payloads are stored here"),
        }
        if is_new {
            self.session_order.push_back(session_id);
        }
        if self.session_order.len() > SESSION_CAP {
            if let Some(evicted) = self.session_order.pop_front() {
                self.sessions.remove(&evicted);
                self.reports.remove(&evicted);
                return Some(evicted);
            }
        }
        None
    }

    fn report_notification(report: &Value, version: u64, evicted: Vec<String>) -> Value {
        let mut notification = json!({
            "level": report.get("level").and_then(Value::as_str).unwrap_or("session"),
            "session_id": report.get("session_id"),
            "report_version": version,
        });
        if !evicted.is_empty() {
            notification["evicted_session_ids"] = json!(evicted);
        }
        notification
    }

    fn queue_report_eviction(&mut self, session_id: String) {
        if !self.pending_report_evictions.contains(&session_id) {
            self.pending_report_evictions.push_back(session_id);
        }
        while self.pending_report_evictions.len() > SESSION_CAP {
            self.pending_report_evictions.pop_front();
        }
    }

    /// The stored payload for `session_id`, or the latest-active one
    /// (greatest `t_last`, RFC 3339 strings order lexicographically) when
    /// no id is asked for — which preserves the single-session behavior
    /// every existing client already relies on.
    fn by_session(
        map: &std::collections::BTreeMap<String, Value>,
        session_id: Option<&str>,
    ) -> Option<Value> {
        match session_id {
            Some(id) => map.get(id).cloned(),
            None => map
                .values()
                .max_by_key(|value| {
                    value
                        .get("t_last")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
                .cloned(),
        }
    }

    /// The picker's list: one summary row per known session, latest first.
    /// How many streams are open: whether anyone is watching. The watcher
    /// does its diagnostic work only while this is not zero.
    pub fn attached(&self) -> usize {
        self.attached
    }

    /// Replaces the picker's rows for sessions that have no `session`
    /// frame yet. Each row has the shape of a `/debug/sessions` element.
    pub fn set_session_summaries(&mut self, rows: Vec<Value>) {
        self.summaries = rows
            .into_iter()
            .filter_map(|row| {
                let id = row.get("session_id")?.as_str()?.to_string();
                Some((id, row))
            })
            .collect();
    }

    /// Forgets everything but the status, once nobody is attached: what
    /// is kept would only go stale. Sequence numbers keep counting, so a
    /// cursor issued before this is answered `reset`.
    pub fn forget_picture(&mut self) {
        self.frames.clear();
        self.sessions.clear();
        self.session_order.clear();
        self.summaries.clear();
        self.reports.clear();
        self.pending_report_evictions.clear();
        self.health = None;
        self.watchdog.clear();
        self.allocs.clear();
        self.alloc_order.clear();
        self.wanted.clear();
    }

    /// The sessions asked for since the last call that nothing described.
    pub fn take_wanted(&mut self) -> std::collections::BTreeSet<String> {
        std::mem::take(&mut self.wanted)
    }

    /// `map`'s entry for `session_id`; a miss for a named session is
    /// recorded for the watcher, which publishes it shortly after.
    fn by_session_or_want(
        &mut self,
        reports: bool,
        session_id: Option<&str>,
        may_want: bool,
    ) -> Option<Value> {
        let map = if reports {
            &self.reports
        } else {
            &self.sessions
        };
        let found = DebugState::by_session(map, session_id);
        if let (None, Some(id)) = (&found, session_id) {
            if may_want && self.wanted.len() < WANTED_CAP {
                self.wanted.insert(id.to_string());
            }
        }
        found
    }

    pub(crate) fn session_list(&self) -> Value {
        let summarised = self
            .summaries
            .iter()
            .filter(|(id, _)| !self.sessions.contains_key(*id))
            .map(|(_, row)| row.clone());
        let mut listed: Vec<Value> = summarised.collect();
        let mut rows: Vec<&Value> = self.sessions.values().collect();
        rows.sort_by_key(|value| {
            std::cmp::Reverse(value.get("t_last").and_then(Value::as_str).unwrap_or(""))
        });
        listed.extend(rows.into_iter().map(|session| {
            json!({
                "session_id": session.get("session_id"),
                "agent_app": session.pointer("/session_meta/agent_app"),
                "t_start": session.get("t_start"),
                "t_last": session.get("t_last"),
                "events": session.get("events"),
            })
        }));
        listed.sort_by(|a, b| {
            let last = |row: &Value| {
                row.get("t_last")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            last(b).cmp(&last(a))
        });
        Value::Array(listed)
    }

    /// The seq the next published frame will carry.
    fn head_seq(&self) -> u64 {
        self.next_seq
    }

    /// Closes every SSE connection by dropping its sender: the reader
    /// thread sees EOF and finishes its response.
    fn disconnect_all(&mut self) {
        self.connections.clear();
    }
}

/// A running debug server. Dropping it stops accepting, closes every live
/// SSE connection and joins the accept thread.
///
/// **Drop is the only way to stop it.** There used to be an explicit
/// `shutdown(self)` as well, which did exactly what `Drop` then did again a
/// line later — a second, optional spelling of the mandatory path, whose
/// only possible contribution was for the two to fall out of step.
pub struct DebugServer {
    /// `None` for a server on a Unix socket.
    addr: Option<SocketAddr>,
    /// The socket file to remove on the way out, for a server on one, and
    /// the file's identity when it was bound: a path that has since become
    /// another collector's socket is not this server's to remove.
    socket: Option<(std::path::PathBuf, Option<(u64, u64)>)>,
    server: Arc<Server>,
    state: Arc<Mutex<DebugState>>,
    thread: Option<JoinHandle<()>>,
    /// Signalled by the accept thread as its last act, so [`stop`] can wait
    /// with a deadline instead of joining unconditionally.
    ///
    /// [`stop`]: DebugServer::stop
    finished: Receiver<()>,
}

impl DebugServer {
    /// The TCP address served. Panics for a server on a Unix socket,
    /// which has none: callers that bind one do not ask.
    pub fn addr(&self) -> SocketAddr {
        self.addr.expect("this debug server is on a Unix socket")
    }

    /// Unblocks the accept loop and waits [`JOIN_DEADLINE`] for it.
    ///
    /// The wait is bounded and the thread is **detached** if the deadline
    /// passes. `afoot watch`'s shutdown is a user pressing Ctrl-C; a debug
    /// server thread that is somehow still stuck must cost that user a
    /// logged line, not an unbounded hang — the process is exiting, and the
    /// OS reclaims the thread.
    fn stop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.disconnect_all();
        }
        self.server.unblock();
        if let Some((socket, bound_as)) = self.socket.take() {
            if file_identity(&socket) == bound_as {
                let _ = std::fs::remove_file(socket);
            } else {
                // The HTTP library unlinks its socket path when the server
                // is dropped, whoever owns the file by then. Never dropping
                // it is the only way to leave the other collector's socket
                // alone; this process is on its way out.
                std::mem::forget(Arc::clone(&self.server));
            }
        }
        let Some(thread) = self.thread.take() else {
            return;
        };
        match self.finished.recv_timeout(JOIN_DEADLINE) {
            Ok(()) => {
                let _ = thread.join();
            }
            Err(_) => eprintln!(
                "afoot watch: debug server did not stop within {JOIN_DEADLINE:?}; detaching it and continuing shutdown"
            ),
        }
    }
}

impl Drop for DebugServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Binds `addr` and serves the `/debug` routes from `state`, plus the
/// embedded console (`afoot_console::asset`) on every other path.
///
/// Port `0` picks a free port — read it back with [`DebugServer::addr`],
/// which is how the tests avoid fighting over a fixed port.
pub fn serve(addr: SocketAddr, state: Arc<Mutex<DebugState>>) -> Result<DebugServer> {
    if afoot_console::is_placeholder() {
        // A dev who forgot `npm --prefix console run build` gets the
        // placeholder page silently otherwise — indistinguishable from a
        // real UI that just happens to be minimal.
        eprintln!(
            "afoot watch: debug console embedded as placeholder — build console/ and rebuild to serve the UI"
        );
    }

    // Session data with no authentication: this machine only.
    if !addr.ip().is_loopback() {
        anyhow::bail!("refusing to serve /debug on {addr}: loopback addresses only");
    }
    let server = Server::http(addr)
        .map_err(|err| anyhow::anyhow!("failed to bind debug server on {addr}: {err}"))?;
    let bound = server
        .server_addr()
        .to_ip()
        .context("debug server is not bound to an IP address")?;
    Ok(run(server, state, Some(bound), None))
}

/// Serves the same routes on a Unix socket at `path`, readable and
/// writable by its owner only: the way a console attaches to a collector
/// that listens on no TCP port.
///
/// A socket file nobody answers on — what a collector that died leaves
/// behind — is replaced. One that answers belongs to a running collector,
/// and is left alone.
#[cfg(unix)]
pub fn serve_unix(path: &std::path::Path, state: Arc<Mutex<DebugState>>) -> Result<DebugServer> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        if socket_is_served(path) {
            anyhow::bail!(
                "{} is already served by a running collector",
                path.display()
            );
        }
        std::fs::remove_file(path)
            .with_context(|| format!("remove the stale socket {}", path.display()))?;
    }
    let server = Server::http_unix(path)
        .map_err(|err| anyhow::anyhow!("failed to bind {}: {err}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restrict {}", path.display()))?;
    let socket = (path.to_path_buf(), file_identity(path));
    Ok(run(server, state, None, Some(socket)))
}

/// Device and inode of `path`, where there are such things.
fn file_identity(path: &std::path::Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// The console socket's file name inside the state directory.
pub const SOCKET_NAME: &str = "console.sock";

/// [`serve_unix`] where there are Unix sockets, nothing elsewhere.
pub fn serve_socket(
    path: &std::path::Path,
    state: Arc<Mutex<DebugState>>,
) -> Result<Option<DebugServer>> {
    #[cfg(unix)]
    return serve_unix(path, state).map(Some);
    #[cfg(not(unix))]
    {
        let _ = (path, state);
        Ok(None)
    }
}

/// Whether a running collector answers on the socket at `path`.
pub fn socket_is_served(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::net::UnixStream::connect(path).is_ok();
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Starts the accept thread for an already-bound server.
fn run(
    server: Server,
    state: Arc<Mutex<DebugState>>,
    addr: Option<SocketAddr>,
    socket: Option<(std::path::PathBuf, Option<(u64, u64)>)>,
) -> DebugServer {
    let server = Arc::new(server);
    let thread_server = Arc::clone(&server);
    let thread_state = Arc::clone(&state);
    let (finished_tx, finished) = mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        for request in thread_server.incoming_requests() {
            handle(request, &thread_state);
        }
        let _ = finished_tx.send(());
    });
    DebugServer {
        addr,
        socket,
        server,
        state,
        thread: Some(thread),
        finished,
    }
}

/// Accept-thread entry point: bound concurrency, then move the whole
/// request onto its own thread.
///
/// Everything below this — reading state, serialising, writing the socket —
/// happens off the accept loop. `Request::respond` blocks until the client
/// drains the socket, so a client that opens a connection and never reads
/// (a paused debugger, a `curl` under `^Z`, a console tab the OS has
/// frozen) would otherwise hold the single accept loop and take every other
/// route down with it, including the stream the console needs to notice
/// anything is wrong.
fn handle(request: Request, state: &Arc<Mutex<DebugState>>) {
    let Some(slot) = acquire_handler_slot() else {
        // Written inline, on the accept thread: there is by definition no
        // thread budget left to write it from. The body is a few dozen
        // bytes and fits in the socket buffer, so this does not block in
        // any realistic case — and answering `503` is strictly better than
        // dropping the connection unexplained.
        let response = Response::from_string(r#"{"error":"too_many_connections"}"#)
            .with_status_code(503)
            .with_header(header("Content-Type", "application/json"));
        let _ = request.respond(response);
        return;
    };

    let state = Arc::clone(state);
    std::thread::spawn(move || {
        let _slot = slot;
        // A panic in one request handler must not take the server down with
        // it. The thread would die either way; catching it here is what
        // turns a silent dead connection into a stated failure, so a
        // console that stops updating has a reason on stderr.
        let handled = std::panic::catch_unwind(AssertUnwindSafe(|| dispatch(request, &state)));
        if handled.is_err() {
            eprintln!("afoot watch: a /debug request handler panicked; the server continues");
        }
    });
}

fn dispatch(request: Request, state: &Arc<Mutex<DebugState>>) {
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (url, String::new()),
    };

    // DNS-rebinding guard. A page on the open web can resolve its own
    // hostname to 127.0.0.1 and reach this port; what it cannot do is
    // forge the `Host` header, which still carries the attacker's name.
    // An absent `Host` is allowed: HTTP/1.0 clients and hand-rolled probes
    // omit it, and they cannot be rebinding victims.
    if let Some(host) = header_value(&request, "Host") {
        if !is_loopback_authority(&host) {
            respond_json(
                request,
                403,
                json!({"error": "forbidden_host", "host": host}),
            );
            return;
        }
    }

    if *request.method() == tiny_http::Method::Options {
        let origin = allowed_origin(&request);
        let mut response = Response::empty(204)
            .with_header(header("Access-Control-Allow-Headers", "*"))
            .with_header(header("Vary", "Origin"));
        if let Some(origin) = origin {
            response = response.with_header(header("Access-Control-Allow-Origin", &origin));
        }
        let _ = request.respond(response);
        return;
    }
    if *request.method() != tiny_http::Method::Get {
        respond_json(request, 405, json!({"error": "method_not_allowed"}));
        return;
    }

    match path.as_str() {
        "/debug/session" => {
            let session_id = param(&query, "session_id");
            respond_session_field(request, state, false, session_id.as_deref());
        }
        "/debug/sessions" => respond_field(request, state, |s| Some(s.session_list())),
        "/debug/snapshot" => {
            let window_ms = parse_window_ms(&query);
            respond_field(request, state, move |s| Some(snapshot(s, window_ms)));
        }
        "/debug/report" => {
            let session_id = param(&query, "session_id");
            respond_session_field(request, state, true, session_id.as_deref());
        }
        "/debug/health" => respond_field(request, state, |s| s.health.clone()),
        "/debug/status" => respond_field(request, state, |s| s.status.clone()),
        "/debug/stream" => stream(request, state, &query),
        path if path.starts_with("/debug/alloc/") => {
            let id = percent_decode(path.trim_start_matches("/debug/alloc/"));
            let found = state.lock().ok().and_then(|s| s.allocs.get(&id).cloned());
            match found {
                Some(trace) => respond_json(request, 200, trace),
                None => respond_json(
                    request,
                    404,
                    json!({"error": "not_found", "sample_event_id": id}),
                ),
            }
        }
        path if path.starts_with("/debug/") => {
            respond_json(request, 404, json!({"error": "not_found"}))
        }
        path => serve_static(request, path),
    }
}

/// Mount point for the embedded debug console (`afoot-console`'s mount
/// contract, `console/README.md` "Shipping inside `afoot`"): any request whose
/// path is not `/debug/*` is answered from `afoot_console::asset`. `Some` is a
/// 200 with `Content-Type`, `ETag` and `Cache-Control: no-cache`; a matching
/// `If-None-Match` short-circuits to an empty `304`; `None` is the same
/// `404` shape every unmatched `/debug/*` path already gets — there is no
/// SPA fallback to `index.html`, because every real route the console needs
/// resolves directly.
fn serve_static(request: Request, path: &str) {
    let Some(asset) = afoot_console::asset(path) else {
        respond_json(request, 404, json!({"error": "not_found"}));
        return;
    };

    if header_value(&request, "If-None-Match").as_deref() == Some(asset.etag) {
        let response = Response::empty(304)
            .with_header(header("ETag", asset.etag))
            .with_header(header("Cache-Control", "no-cache"));
        let _ = request.respond(response);
        return;
    }

    let response = Response::from_data(asset.bytes)
        .with_header(header("Content-Type", asset.content_type))
        .with_header(header("ETag", asset.etag))
        .with_header(header("Cache-Control", "no-cache"));
    let _ = request.respond(response);
}

/// The four read-a-field-from-state routes, which differ only in which
/// field they read.
///
/// `null` is the answer for "not published yet" **and** for a poisoned
/// lock, and deliberately the same one: a console that asked before the
/// first pass and a console that asked after a handler panicked are both
/// being told "there is nothing here", which is true in both cases and is
/// all either can act on. The four arms used to spell that out separately,
/// and two of them spelled it differently (`.ok().and_then(…)` versus
/// `.map(…)`) for no reason anyone could state.
fn respond_field(
    request: Request,
    state: &Arc<Mutex<DebugState>>,
    pick: impl FnOnce(&DebugState) -> Option<Value>,
) {
    let body = state
        .lock()
        .ok()
        .and_then(|s| pick(&s))
        .unwrap_or(Value::Null);
    respond_json(request, 200, body);
}

/// A session's stored payload (its report when `reports`), or `null`.
fn respond_session_field(
    request: Request,
    state: &Arc<Mutex<DebugState>>,
    reports: bool,
    session_id: Option<&str>,
) {
    // Asking makes the watcher work, so only the console's own pages may:
    // a request another site's page sent carries that site's `Origin`.
    let may_want = header_value(&request, "Origin").is_none() || allowed_origin(&request).is_some();
    let body = state
        .lock()
        .ok()
        .and_then(|mut s| s.by_session_or_want(reports, session_id, may_want))
        .unwrap_or(Value::Null);
    respond_json(request, 200, body);
}

/// DATA-CONTRACT §2.2 — the last `window_ms` of the frame log, projected
/// into the batched shape.
///
/// `open_spans` is always empty and that is a *reported* fact, not an
/// omission: the only span collector in this PoC (the Claude Code hook
/// shim) emits a span when it closes, so the control plane never observes
/// an open one. Synthesising one from the `PreToolUse` scratch files would
/// mean minting a `span_id` and an `event_id` the collector never issued,
/// which DATA-CONTRACT §3.2 forbids outright.
fn snapshot(state: &DebugState, window_ms: i64) -> Value {
    let now = state.frames.back().map(|f| f.at_ms).unwrap_or(0);
    let lower = now - window_ms;

    let mut events = Vec::new();
    let mut allocations = Vec::new();
    let mut coverage_gaps = Vec::new();
    // `-1` on an empty ring, not `0`: see [`EMPTY_CURSOR`]. A snapshot taken
    // before the first frame exists is a cursor *before* frame `0`, and
    // saying `0` would cost the client that frame forever.
    let mut as_of_seq: i64 = match state.head_seq() {
        0 => EMPTY_CURSOR,
        head => head as i64 - 1,
    };

    for frame in state.frames.iter().filter(|f| f.at_ms >= lower) {
        match frame.event {
            "fact" => events.push(frame.data.clone()),
            "alloc" => allocations.push(frame.data.clone()),
            "gap" => coverage_gaps.push(frame.data.clone()),
            _ => {}
        }
        as_of_seq = frame.seq as i64;
    }

    json!({
        "events": events,
        "allocations": allocations,
        "coverage_gaps": coverage_gaps,
        "open_spans": [],
        "watchdog": state.watchdog,
        "as_of_seq": as_of_seq,
    })
}

/// DATA-CONTRACT §2.3 — `text/event-stream`, replaying from
/// `Last-Event-ID` (or `?from=`, which is the only option a browser
/// `EventSource` has on its *first* connect) and then streaming live.
///
/// tiny_http has no async story, so each stream gets its own thread: the
/// response body is a blocking reader fed by an `mpsc` channel that
/// [`DebugState::publish`] writes into. The thread ends when the channel's
/// senders are dropped (shutdown) or the client goes away (the write
/// fails) — and a `ping` every [`HEARTBEAT`] of silence guarantees there is
/// a write to fail.
fn stream(request: Request, state: &Arc<Mutex<DebugState>>, query: &str) {
    let cursor = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Last-Event-ID"))
        .map(|h| h.value.as_str().to_string())
        .or_else(|| param(query, "from"))
        .and_then(|raw| parse_cursor(&raw));

    // The status frame and the subscription are taken under one lock, so a
    // status set concurrently is either this first frame or arrives down
    // the channel — never missed.
    let (hello, replay, rx, heartbeat) = {
        let Ok(mut state) = state.lock() else {
            respond_json(request, 500, json!({"error": "state_poisoned"}));
            return;
        };
        let hello = state.status_frame();
        let (replay, rx) = state.subscribe(cursor);
        state.attached += 1;
        (hello, replay, rx, state.heartbeat)
    };
    // Counted for as long as this function runs, however it returns.
    let _attached = Attached(Arc::clone(state));

    // The response is written by hand rather than through
    // `Request::respond`. tiny_http's chunked encoder buffers 8 KiB before
    // it emits anything, which for a stream of ~200-byte frames means the
    // console sees nothing for minutes — the response is not "slow", it is
    // invisible. Taking the writer lets every frame be flushed the moment
    // it is produced, which is the entire point of an event stream.
    let mut head = String::from(concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/event-stream\r\n",
        "Cache-Control: no-cache\r\n",
        "Connection: keep-alive\r\n",
        "X-Accel-Buffering: no\r\n",
        "Vary: Origin\r\n",
        "Transfer-Encoding: chunked\r\n",
    ));
    if let Some(origin) = allowed_origin(&request) {
        // `allowed_origin` has already established this is
        // `http://localhost[:port]`-shaped, so it carries no CR/LF to split
        // the response with.
        head.push_str(&format!("Access-Control-Allow-Origin: {origin}\r\n"));
    }
    head.push_str("\r\n");

    let mut writer = request.into_writer();
    if writer.write_all(head.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }
    // The watcher's status leads every connection: it is how a client
    // learns which process it is talking to before it trusts its cursor.
    if let Some(hello) = &hello {
        if write_chunk(&mut writer, hello).is_err() {
            return;
        }
    }
    // Replay next, straight off the `Vec` — however long it is, the
    // client gets all of it or the connection dies trying. Only then
    // does the bounded live channel take over, picking up at the frame
    // after the replayed range.
    let replayed = match &replay {
        Replay::Frames(frames) => frames
            .iter()
            .all(|bytes| write_chunk(&mut writer, bytes).is_ok()),
        Replay::Reset => write_chunk(&mut writer, &sse_bytes("reset", &json!({}), None)).is_ok(),
    };
    if replayed {
        loop {
            let bytes = match rx.recv_timeout(heartbeat) {
                Ok(bytes) => bytes,
                // Nothing to say for a whole interval: say so. See
                // [`HEARTBEAT`] for the two things this write is for.
                Err(mpsc::RecvTimeoutError::Timeout) => ping_frame(state),
                // The senders are gone: the server is shutting down, or
                // this connection fell too far behind and was dropped.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if write_chunk(&mut writer, &bytes).is_err() {
                // The client went away. Dropping `rx` here makes the next
                // publish see a disconnected sender and reap it.
                break;
            }
        }
    }
    let _ = writer.write_all(b"0\r\n\r\n");
    let _ = writer.flush();
}

/// How many not-yet-described sessions are remembered between two watcher
/// loops: a person picking sessions, not a queue.
const WANTED_CAP: usize = 32;

/// The snapshot's default window, and how far back a console attaching to
/// a running collector is told about.
pub const SNAPSHOT_WINDOW_MS: i64 = 180_000;

/// One open stream, counted in [`DebugState::attached`] until dropped.
struct Attached(Arc<Mutex<DebugState>>);

impl Drop for Attached {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.attached = state.attached.saturating_sub(1);
        }
    }
}

/// The heartbeat event: no `id`, never in the ring. It carries the boot id
/// so even a client that hears nothing else learns when the process behind
/// its connection is not the one it bootstrapped from.
fn ping_frame(state: &Arc<Mutex<DebugState>>) -> Vec<u8> {
    let boot_id = state
        .lock()
        .ok()
        .and_then(|state| state.status.as_ref()?.get("boot_id").cloned())
        .unwrap_or(Value::Null);
    sse_bytes("ping", &json!({ "boot_id": boot_id }), None)
}

/// `?from=` / `Last-Event-ID` → a cursor. Signed, so [`EMPTY_CURSOR`] (`-1`,
/// "I have seen nothing") round-trips and frame `0` is replayed rather than
/// skipped. An unparseable value is no cursor at all, which is the
/// fresh-client path.
fn parse_cursor(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok()
}

/// One HTTP/1.1 chunk, flushed immediately.
fn write_chunk(writer: &mut Box<dyn Write + Send + 'static>, bytes: &[u8]) -> io::Result<()> {
    write!(writer, "{:x}\r\n", bytes.len())?;
    writer.write_all(bytes)?;
    writer.write_all(b"\r\n")?;
    writer.flush()
}

fn sse_bytes(event: &str, data: &Value, seq: Option<u64>) -> Vec<u8> {
    let mut out = format!("event: {event}\n");
    if let Some(seq) = seq {
        out.push_str(&format!("id: {seq}\n"));
    }
    out.push_str(&format!("data: {data}\n\n"));
    out.into_bytes()
}

/// Writes one JSON response. Called on the per-request thread [`handle`]
/// spawned, so blocking here costs one wedged client its own thread and
/// nothing else.
fn respond_json(request: Request, status: u16, body: Value) {
    let origin = allowed_origin(&request);
    let mut response = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Vary", "Origin"));
    if let Some(origin) = origin {
        response = response.with_header(header("Access-Control-Allow-Origin", &origin));
    }
    let _ = request.respond(response);
}

fn header(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes())
        .expect("static header field/value are always valid")
}

fn header_value(request: &Request, field: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(field))
        .map(|h| h.value.as_str().to_string())
}

/// The value to reflect in `Access-Control-Allow-Origin`, or `None` for no
/// CORS header at all.
///
/// Reflecting rather than echoing `*`: the console runs on a Vite dev
/// server on a port we do not control, so the allowed origin cannot be
/// hard-coded, but `*` granted the same read access to every page on the
/// internet. A request with no `Origin` is not a browser cross-origin
/// request and needs no header.
fn allowed_origin(request: &Request) -> Option<String> {
    let origin = header_value(request, "Origin")?;
    is_loopback_origin(&origin).then_some(origin)
}

/// `http://localhost[:port]` / `http://127.x.x.x[:port]` / `http://[::1][:port]`
/// and nothing else.
///
/// `https` is excluded deliberately: the console is served over plain http
/// on loopback, so accepting `https` only widens the set of pages that can
/// read the port.
fn is_loopback_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("http://") else {
        return false;
    };
    // An `Origin` is scheme + authority and nothing else; anything carrying
    // a path, query, fragment or userinfo is not one, and treating it as
    // one is how `http://localhost@evil.example` gets reflected.
    if authority.contains(['/', '?', '#', '@']) {
        return false;
    }
    is_loopback_authority(authority)
}

/// `localhost`, `127.0.0.0/8` or `::1`, with an optional numeric port.
///
/// Used for both the CORS reflection and the `Host` check. Deliberately
/// strict: no wildcards, no name resolution (resolving would re-open the
/// rebinding hole this closes), no non-numeric ports.
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
    // 127.0.0.0/8 — the whole loopback block, not just 127.0.0.1: a server
    // bound to 127.0.0.2 is just as local.
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

/// Splits `host[:port]`, keeping bracketed IPv6 literals intact
/// (`[::1]:8787` → `("::1", Some("8787"))`).
fn split_authority(authority: &str) -> (&str, Option<&str>) {
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, "")) => (host, None),
            Some((host, tail)) => (host, tail.strip_prefix(':').or(Some(tail))),
            None => (authority, None),
        };
    }
    // More than one colon and no brackets: an unbracketed IPv6 literal.
    // Not legal in a URL authority, but it does turn up in hand-written
    // `Host` headers, and splitting it on the last colon would silently
    // read `::1` as host `::` port `1`.
    if authority.matches(':').count() > 1 {
        return (authority, None);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    }
}

/// `?window=180s` → milliseconds. A malformed or absent value falls back to
/// the contract's own default of 180s.
fn parse_window_ms(query: &str) -> i64 {
    param(query, "window")
        .and_then(|raw| raw.trim_end_matches('s').parse::<i64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(|seconds| seconds * 1000)
        .unwrap_or(180_000)
}

fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| percent_decode(value))
    })
}

/// Minimal percent-decoding for the one place a path segment can carry an
/// arbitrary `event_id`. Not a general URL decoder — `+` is left alone,
/// since it is a literal plus in a path segment.
///
/// Decoding is done **entirely on bytes**. Slicing the `&str` to read the
/// two hex digits panics whenever `%` is followed by a multi-byte character
/// (`/debug/alloc/%€` splits a 3-byte `€` at index 1), and a panic on a
/// malformed URL is a denial of service against the whole server, delivered
/// by anything that can type a URL.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_server_refuses_to_listen_beyond_loopback() {
        for addr in ["0.0.0.0:0", "[::]:0"] {
            let error = serve(
                addr.parse().unwrap(),
                Arc::new(Mutex::new(DebugState::default())),
            )
            .err()
            .unwrap_or_else(|| panic!("{addr} was accepted"));
            assert!(error.to_string().contains("loopback"), "{error:#}");
        }
    }

    fn state_with_frames() -> DebugState {
        let mut state = DebugState::default();
        state.publish("fact", json!({"event_id": "e1"}), 1_000);
        state.publish("alloc", json!({"sample_event_id": "s1"}), 2_000);
        state.publish("gap", json!({"reason": "sampler exited"}), 3_000);
        state.publish("watchdog", json!({"pids": [{"pid": 7}]}), 4_000);
        state
    }

    #[test]
    fn window_parsing_accepts_the_contract_form_and_defaults_otherwise() {
        assert_eq!(parse_window_ms("window=180s"), 180_000);
        assert_eq!(parse_window_ms("window=30s"), 30_000);
        assert_eq!(parse_window_ms("window=30"), 30_000);
        assert_eq!(parse_window_ms(""), 180_000);
        assert_eq!(parse_window_ms("window=nope"), 180_000);
        assert_eq!(parse_window_ms("window=0s"), 180_000);
    }

    #[test]
    fn the_snapshot_is_a_projection_of_the_frame_log() {
        let state = state_with_frames();
        let snap = snapshot(&state, 180_000);

        assert_eq!(snap["events"].as_array().unwrap().len(), 1);
        assert_eq!(snap["allocations"].as_array().unwrap().len(), 1);
        assert_eq!(snap["coverage_gaps"].as_array().unwrap().len(), 1);
        // §2.2's watchdog is the bare array, unwrapped from §2.3's
        // `{pids: [...]}` frame.
        assert_eq!(snap["watchdog"], json!([{"pid": 7}]));
        assert_eq!(snap["open_spans"], json!([]));
        assert_eq!(snap["as_of_seq"], json!(3));
    }

    #[test]
    fn a_narrow_window_excludes_older_frames_but_keeps_the_cursor_at_the_head() {
        let state = state_with_frames();
        let snap = snapshot(&state, 1_500);
        assert_eq!(snap["events"].as_array().unwrap().len(), 0);
        assert_eq!(snap["allocations"].as_array().unwrap().len(), 0);
        // The client must still subscribe from the true head, or it would
        // be replayed frames the snapshot deliberately left out.
        assert_eq!(snap["as_of_seq"], json!(3));
    }

    #[test]
    fn sse_bytes_are_the_named_event_wire_format() {
        let bytes = sse_bytes("decision", &json!({"kind": "attr"}), Some(12));
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "event: decision\nid: 12\ndata: {\"kind\":\"attr\"}\n\n"
        );
    }

    #[test]
    fn allocs_are_indexed_by_sample_event_id_for_the_per_sample_route() {
        let state = state_with_frames();
        assert!(state.allocs.contains_key("s1"));
        assert!(!state.allocs.contains_key("s2"));
    }

    /// The alloc index is the one structure here that used to grow for the
    /// life of the process: one full allocation trace per energy sample
    /// ever apportioned, kept long after its frame had aged out of the
    /// ring. It is bounded by the same horizon the ring is, oldest first.
    #[test]
    fn the_alloc_index_is_bounded_by_the_ring_and_evicts_the_oldest_first() {
        let mut state = DebugState::with_limits(8, 8);
        for i in 0..20 {
            state.publish(
                "alloc",
                json!({"sample_event_id": format!("s{i}")}),
                i as i64,
            );
        }
        assert_eq!(state.allocs.len(), 8);
        assert!(!state.allocs.contains_key("s0"), "the oldest went first");
        assert!(state.allocs.contains_key("s19"), "the newest is kept");
    }

    #[test]
    fn the_ring_is_bounded_and_drops_the_oldest_frames_first() {
        let mut state = DebugState::default();
        for i in 0..(RING_CAPACITY + 10) {
            state.publish("fact", json!({"i": i}), i as i64);
        }
        assert_eq!(state.frames.len(), RING_CAPACITY);
        assert_eq!(state.frames.front().unwrap().seq, 10);
        assert_eq!(state.frames.back().unwrap().seq, (RING_CAPACITY + 9) as u64);
    }

    #[test]
    fn reports_are_retained_once_but_ring_and_live_queues_only_get_invalidations() {
        let mut state = DebugState::with_limits(8, 8);
        let (_replay, rx) = state.subscribe(Some(EMPTY_CURSOR));
        let full = json!({
            "level": "session",
            "session_id": "sess-a",
            "impact_join": {"large": "x".repeat(32_000)},
            "by_model": [],
            "estimation_status_histogram": {},
        });

        state.publish("report", full, 1);

        let stored = state
            .reports
            .get("sess-a")
            .expect("latest full report retained");
        assert!(stored.get("impact_join").is_some());
        assert_eq!(stored["report_version"], json!(0));

        let frame = state.frames.back().expect("report invalidation frame");
        assert_eq!(frame.event, "report");
        assert_eq!(frame.data["session_id"], json!("sess-a"));
        assert_eq!(frame.data["report_version"], json!(0));
        assert!(frame.data.get("impact_join").is_none());

        let live = String::from_utf8(rx.try_recv().expect("live invalidation")).unwrap();
        assert!(live.contains("\"report_version\":0"), "{live}");
        assert!(!live.contains("impact_join"), "{live}");
        assert!(
            live.len() < 256,
            "notification stays small: {} bytes",
            live.len()
        );
    }

    #[test]
    fn report_replay_is_compact_and_direct_state_keeps_only_the_latest_version() {
        let mut state = DebugState::with_limits(8, 8);
        for version in 0..3 {
            state.publish(
                "report",
                json!({
                    "level": "session",
                    "session_id": "sess-a",
                    "impact_join": {"version": version, "large": "x".repeat(8_000)},
                    "by_model": [],
                    "estimation_status_histogram": {},
                }),
                version,
            );
        }

        assert_eq!(state.reports.len(), 1);
        assert_eq!(state.reports["sess-a"]["impact_join"]["version"], json!(2));
        assert_eq!(state.reports["sess-a"]["report_version"], json!(2));

        let (replay, _rx) = state.subscribe(Some(EMPTY_CURSOR));
        let Replay::Frames(frames) = replay else {
            panic!("report invalidations should be replayable");
        };
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|frame| frame.len() < 256));
        assert!(frames
            .iter()
            .all(|frame| !String::from_utf8_lossy(frame).contains("impact_join")));
    }

    #[test]
    fn session_cap_evicts_session_and_report_state_together() {
        let mut state = DebugState::with_limits(8, 8);
        for i in 0..=SESSION_CAP {
            let session_id = format!("sess-{i}");
            state.publish(
                "session",
                json!({"session_id": session_id, "t_last": format!("{i:04}")}),
                i as i64,
            );
            state.publish(
                "report",
                json!({
                    "level": "session",
                    "session_id": session_id,
                    "impact_join": {},
                    "by_model": [],
                    "estimation_status_histogram": {},
                }),
                i as i64,
            );
        }

        assert_eq!(state.sessions.len(), SESSION_CAP);
        assert_eq!(state.reports.len(), SESSION_CAP);
        assert!(!state.sessions.contains_key("sess-0"));
        assert!(!state.reports.contains_key("sess-0"));
        assert!(state.sessions.contains_key(&format!("sess-{SESSION_CAP}")));
        assert!(state.reports.contains_key(&format!("sess-{SESSION_CAP}")));
    }

    #[test]
    fn pending_report_eviction_hints_are_bounded_when_reports_stop() {
        let mut state = DebugState::with_limits(8, 8);
        for i in 0..(SESSION_CAP * 3) {
            state.publish(
                "session",
                json!({"session_id": format!("sess-{i}"), "t_last": format!("{i:04}")}),
                i as i64,
            );
        }
        assert_eq!(state.sessions.len(), SESSION_CAP);
        assert_eq!(state.pending_report_evictions.len(), SESSION_CAP);
    }

    /// The frames a subscriber is owed, in order, as `id: N` numbers.
    fn replayed_ids(replay: &Replay) -> Vec<u64> {
        match replay {
            Replay::Reset => panic!("expected a replay, got reset"),
            Replay::Frames(frames) => frames
                .iter()
                .filter_map(|bytes| {
                    String::from_utf8_lossy(bytes)
                        .lines()
                        .find_map(|line| line.strip_prefix("id: ").map(str::to_string))
                })
                .map(|id| id.parse::<u64>().expect("numeric id"))
                .collect(),
        }
    }

    #[test]
    fn a_replay_far_longer_than_the_connection_queue_is_delivered_whole() {
        // The queue is *deliberately* tiny relative to the ring: this is the
        // shape of the bug it guards. Replay used to be `try_send` into this
        // queue, so everything past the 8th frame was dropped on the floor
        // and the client was left believing it had a complete history.
        let mut state = DebugState::with_limits(4096, 8);
        for i in 0..2000 {
            state.publish("fact", json!({"i": i}), i as i64);
        }

        let (replay, _rx) = state.subscribe(Some(EMPTY_CURSOR));
        let ids = replayed_ids(&replay);
        assert_eq!(ids.len(), 2000, "every frame in the ring must be replayed");
        assert_eq!(ids.first().copied(), Some(0), "frame 0 is never skipped");
        assert!(
            ids.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "a replay is contiguous — a hole is the one outcome the contract forbids"
        );
    }

    #[test]
    fn a_cursor_older_than_the_ring_gets_reset_rather_than_a_hole() {
        let mut state = DebugState::with_limits(16, 8);
        for i in 0..40 {
            state.publish("fact", json!({"i": i}), i as i64);
        }
        // Frames 0..=23 were evicted; a client resuming from 5 cannot be
        // served without a gap.
        assert!(matches!(state.subscribe(Some(5)).0, Replay::Reset));
        // …and one resuming from inside the ring still can.
        let (replay, _rx) = state.subscribe(Some(30));
        assert_eq!(replayed_ids(&replay), (31..40).collect::<Vec<_>>());
    }

    #[test]
    fn subscribing_and_publishing_hand_off_without_a_gap_or_a_duplicate() {
        let mut state = DebugState::with_limits(64, 8);
        state.publish("fact", json!({"i": 0}), 0);
        state.publish("fact", json!({"i": 1}), 1);

        let (replay, rx) = state.subscribe(Some(EMPTY_CURSOR));
        assert_eq!(replayed_ids(&replay), vec![0, 1]);

        state.publish("fact", json!({"i": 2}), 2);
        let live = String::from_utf8(rx.try_recv().expect("frame 2 on the live channel")).unwrap();
        assert!(live.contains("id: 2"), "{live}");
        assert!(
            rx.try_recv().is_err(),
            "the replayed frames must not also arrive live"
        );
    }

    #[test]
    fn an_empty_ring_reports_a_cursor_before_frame_zero() {
        let state = DebugState::default();
        let snap = snapshot(&state, 180_000);
        assert_eq!(
            snap["as_of_seq"],
            json!(EMPTY_CURSOR),
            "`0` would be a real frame number, and the client would never receive frame 0"
        );

        // …and that sentinel round-trips through `?from=` into a replay that
        // starts at frame 0.
        let mut state = state;
        state.publish("fact", json!({"i": 0}), 0);
        let cursor = parse_cursor(&snap["as_of_seq"].to_string()).expect("parses");
        assert_eq!(replayed_ids(&state.subscribe(Some(cursor)).0), vec![0]);
    }

    #[test]
    fn a_cursor_is_parsed_signed_and_an_unparseable_one_is_no_cursor() {
        assert_eq!(parse_cursor("0"), Some(0));
        assert_eq!(parse_cursor("-1"), Some(EMPTY_CURSOR));
        assert_eq!(parse_cursor("42"), Some(42));
        assert_eq!(parse_cursor("null"), None);
        assert_eq!(parse_cursor(""), None);
    }

    #[test]
    fn percent_decoding_handles_ids_with_reserved_characters() {
        assert_eq!(percent_decode("otlp-req%2F1"), "otlp-req/1");
        assert_eq!(percent_decode("plain-id"), "plain-id");
        assert_eq!(percent_decode("trailing%"), "trailing%");
    }

    #[test]
    fn loopback_authorities_are_accepted_with_or_without_a_port() {
        assert!(is_loopback_authority("localhost"));
        assert!(is_loopback_authority("localhost:5173"));
        assert!(is_loopback_authority("127.0.0.1"));
        assert!(is_loopback_authority("127.0.0.1:8787"));
        // The whole 127/8 block, not just .1.
        assert!(is_loopback_authority("127.0.0.2:8787"));
        assert!(is_loopback_authority("[::1]"));
        assert!(is_loopback_authority("[::1]:5173"));
        assert!(is_loopback_authority("::1"));
    }

    #[test]
    fn non_loopback_authorities_are_refused() {
        assert!(!is_loopback_authority("evil.example"));
        assert!(!is_loopback_authority("evil.example:8787"));
        // Resolves to 127.0.0.1 for a rebinding attacker; the name is what
        // we judge, and the name is theirs.
        assert!(!is_loopback_authority("localhost.evil.example"));
        assert!(!is_loopback_authority("127.0.0.1.evil.example"));
        assert!(!is_loopback_authority("10.0.0.5:8787"));
        assert!(!is_loopback_authority("128.0.0.1"));
        assert!(!is_loopback_authority("127.0.0.999"));
        assert!(!is_loopback_authority("localhost:notaport"));
        assert!(!is_loopback_authority(""));
    }

    /// The CORS decision is made on the `Origin` string as sent. These are
    /// the strings a browser can be made to send that *look* loopback.
    #[test]
    fn only_loopback_http_origins_are_reflected() {
        assert!(is_loopback_origin("http://localhost:5173"));
        assert!(is_loopback_origin("http://127.0.0.1:8787"));
        assert!(is_loopback_origin("http://localhost"));
        assert!(is_loopback_origin("http://[::1]:5173"));

        // https is not what the console is served over, and accepting it
        // widens the set of pages that can read the port for no gain.
        assert!(!is_loopback_origin("https://localhost:5173"));
        assert!(!is_loopback_origin("http://evil.example"));
        // Userinfo: the authority's *host* here is evil.example.
        assert!(!is_loopback_origin("http://localhost@evil.example"));
        assert!(!is_loopback_origin("http://localhost.evil.example"));
        assert!(!is_loopback_origin("http://localhost:5173/path"));
        // The opaque origin a sandboxed iframe or a `file://` page sends.
        assert!(!is_loopback_origin("null"));
    }

    /// Held by every test that takes real handler slots: the counter is
    /// process-wide, and the cap test needs it at zero.
    static SLOT_USERS: Mutex<()> = Mutex::new(());

    /// The cap must hold under repeated acquire/release, and slots must come
    /// back when the guard drops — a leak here bricks the server after 32
    /// requests.
    #[test]
    fn the_handler_slot_cap_holds_and_releases() {
        let _alone = SLOT_USERS.lock().unwrap_or_else(|e| e.into_inner());
        let before = ACTIVE_HANDLERS.load(Ordering::Acquire);
        assert_eq!(before, 0, "no other unit test holds a slot");

        let slots: Vec<HandlerSlot> = (0..MAX_HANDLER_THREADS)
            .map(|_| acquire_handler_slot().expect("under the cap"))
            .collect();
        assert!(
            acquire_handler_slot().is_none(),
            "the {MAX_HANDLER_THREADS}th+1 handler must be refused, not queued"
        );

        drop(slots);
        assert_eq!(ACTIVE_HANDLERS.load(Ordering::Acquire), 0);
        assert!(
            acquire_handler_slot().is_some(),
            "slots must be reusable once their guards drop"
        );
    }

    /// `%` followed by a multi-byte character used to slice the `&str`
    /// mid-character and panic — a whole-server denial of service reachable
    /// from any URL bar.
    #[test]
    fn percent_decoding_never_panics_on_a_malformed_escape() {
        assert_eq!(percent_decode("%€"), "%€");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("%%41"), "%A");
        assert_eq!(percent_decode("é%2Fé"), "é/é");
    }

    // --- console mount: serve_static + the routing fallback -------------
    //
    // These bind a real ephemeral-port `tiny_http::Server` and issue raw
    // HTTP/1.1 requests over a `TcpStream` — the same technique
    // `crates/afoot-cli/tests/watch.rs`'s `http_get_raw` uses — but call
    // `dispatch` directly on the accept thread rather than going through
    // `serve`/`handle`'s per-request spawn + [`HandlerSlot`] accounting.
    // Going through the real slot-accounted path would make these tests
    // race `the_handler_slot_cap_holds_and_releases` above, which asserts
    // exclusive ownership of the process-global [`ACTIVE_HANDLERS`]
    // counter; `dispatch` is everything these tests need (routing,
    // `serve_static`, header shape) without touching that counter at all.
    // Whichever bundle `afoot-console` embeds for this build (a real
    // `console/dist` from `npm --prefix console run build`, or `build.rs`'s
    // placeholder when that step was skipped) is what these exercise;
    // `afoot_console::is_placeholder()` says which, and either way
    // `index.html` is present with a stable ETag, which is all these tests
    // depend on.

    use std::io::Read as _;
    use std::net::TcpStream;

    /// One dispatch-backed test server: a background thread that answers
    /// every request on `addr` by calling `dispatch` directly, one at a
    /// time, for the life of the test binary.
    struct TestServer {
        addr: SocketAddr,
        _thread: JoinHandle<()>,
    }

    fn start_test_server() -> TestServer {
        let server = Server::http("127.0.0.1:0").expect("bind ephemeral port");
        let addr = server
            .server_addr()
            .to_ip()
            .expect("bound to an IP address");
        let state = Arc::new(Mutex::new(DebugState::default()));
        let thread = std::thread::spawn(move || {
            for request in server.incoming_requests() {
                dispatch(request, &state);
            }
        });
        TestServer {
            addr,
            _thread: thread,
        }
    }

    /// Minimal HTTP/1.1 GET over a raw socket, mirroring
    /// `tests/watch.rs`'s `http_get_raw` (not reused across the
    /// bin/integration-test boundary). Returns (status, response head,
    /// body bytes) — bytes rather than a `String` because the console's own
    /// assets aren't all UTF-8 (fonts, etc.), and a 304's body is empty.
    fn http_get(addr: SocketAddr, path: &str, extra: &[(&str, &str)]) -> (u16, String, Vec<u8>) {
        let mut stream =
            TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");

        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (field, value) in extra {
            request.push_str(&format!("{field}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).expect("write request");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read response");
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("head/body split");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let body = raw[split + 4..].to_vec();
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .expect("status line");
        (status, head, body)
    }

    fn response_header(head: &str, name: &str) -> Option<String> {
        head.lines().find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field
                .trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }

    #[test]
    fn non_debug_get_serves_the_embedded_console_with_etag_and_no_cache() {
        let server = start_test_server();
        let (status, head, body) = http_get(server.addr, "/", &[]);

        assert_eq!(status, 200);
        assert!(response_header(&head, "Content-Type")
            .expect("content-type present")
            .starts_with("text/html"));
        assert_eq!(
            response_header(&head, "Cache-Control").as_deref(),
            Some("no-cache")
        );
        let etag = response_header(&head, "ETag").expect("etag present");
        assert!(etag.starts_with('"') && etag.ends_with('"'));
        assert!(!body.is_empty(), "index.html body must not be empty");
    }

    #[test]
    fn if_none_match_round_trips_to_a_304_with_an_empty_body() {
        let server = start_test_server();
        let (_, first_head, _) = http_get(server.addr, "/", &[]);
        let etag = response_header(&first_head, "ETag").expect("etag present");

        let (status, head, body) = http_get(server.addr, "/", &[("If-None-Match", etag.as_str())]);

        assert_eq!(status, 304);
        assert!(body.is_empty(), "a 304 must not carry a body");
        assert_eq!(
            response_header(&head, "ETag").as_deref(),
            Some(etag.as_str())
        );
    }

    #[test]
    fn unknown_non_debug_path_still_404s() {
        let server = start_test_server();
        let (status, _, _) = http_get(server.addr, "/does/not/exist.js", &[]);
        assert_eq!(status, 404);
    }

    #[test]
    fn debug_routes_are_unaffected_by_the_console_mount() {
        let server = start_test_server();

        // A known /debug/* route still answers as before (no session
        // published yet, so the field route's `null`).
        let (status, head, body) = http_get(server.addr, "/debug/session", &[]);
        assert_eq!(status, 200);
        assert_eq!(
            response_header(&head, "Content-Type").as_deref(),
            Some("application/json")
        );
        assert_eq!(body, b"null");

        // An unmatched /debug/* path is the pre-existing 404, not a console
        // asset lookup.
        let (status, _, body) = http_get(server.addr, "/debug/nonexistent", &[]);
        assert_eq!(status, 404);
        assert_eq!(
            String::from_utf8(body).unwrap(),
            json!({"error": "not_found"}).to_string()
        );
    }

    // --- /debug/status, the on-connect frame, heartbeat ----------------------

    /// A test server over a caller-owned state, one thread per request so a
    /// test can hold a stream open and still issue other requests. `done`
    /// receives a request's path when its handler returns, which is how a
    /// test observes that a stream's thread ended.
    struct StatefulServer {
        addr: SocketAddr,
        done: Receiver<String>,
    }

    fn start_test_server_with(state: Arc<Mutex<DebugState>>) -> StatefulServer {
        let server = Server::http("127.0.0.1:0").expect("bind ephemeral port");
        let addr = server
            .server_addr()
            .to_ip()
            .expect("bound to an IP address");
        let (done_tx, done) = mpsc::channel();
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                let state = Arc::clone(&state);
                let done_tx = done_tx.clone();
                std::thread::spawn(move || {
                    let path = request.url().to_string();
                    dispatch(request, &state);
                    let _ = done_tx.send(path);
                });
            }
        });
        StatefulServer { addr, done }
    }

    /// One parsed SSE event off the wire.
    #[derive(Debug)]
    struct WireEvent {
        event: String,
        id: Option<String>,
        data: Value,
    }

    fn open_stream(addr: SocketAddr, path: &str) -> TcpStream {
        let mut stream =
            TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect");
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
            .expect("write request");
        stream
    }

    /// Reads whatever the stream sends for `window`, parsed into events.
    fn read_events(stream: &mut TcpStream, window: Duration) -> Vec<WireEvent> {
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("set read timeout");
        let deadline = std::time::Instant::now() + window;
        let mut raw = Vec::new();
        let mut buf = [0u8; 8192];
        while std::time::Instant::now() < deadline {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        }
        let text = String::from_utf8_lossy(&raw);
        let mut events = Vec::new();
        let mut event = None;
        let mut id = None;
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("event: ") {
                event = Some(name.to_string());
                id = None;
            } else if let Some(value) = line.strip_prefix("id: ") {
                id = Some(value.to_string());
            } else if let Some(data) = line.strip_prefix("data: ") {
                if let Some(event) = event.take() {
                    events.push(WireEvent {
                        event,
                        id: id.take(),
                        data: serde_json::from_str(data).expect("frame data is JSON"),
                    });
                }
            }
        }
        events
    }

    fn shared(state: DebugState) -> Arc<Mutex<DebugState>> {
        Arc::new(Mutex::new(state))
    }

    #[cfg(unix)]
    #[test]
    fn the_api_is_served_over_a_private_unix_socket() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream;
        let _alone = SLOT_USERS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("console.sock");
        let state = shared(DebugState::default());
        state.lock().unwrap().set_status(json!({"boot_id": "b1"}));
        let server = serve_unix(&path, Arc::clone(&state)).expect("serve on the socket");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the socket is the owner's alone");

        let mut stream = UnixStream::connect(&path).expect("connect");
        stream
            .write_all(
                b"GET /debug/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains(r#""boot_id":"b1""#), "{response}");

        drop(server);
        assert!(!path.exists(), "the socket file goes with the server");
        // The handler thread is detached: its slot is back before the
        // next test that counts them.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while ACTIVE_HANDLERS.load(Ordering::Acquire) != 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_socket_file_is_replaced_and_a_live_one_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("console.sock");
        // What a collector that died without cleaning up leaves behind.
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        assert!(path.exists());
        let first = serve_unix(&path, shared(DebugState::default())).expect("stale file replaced");
        let second = serve_unix(&path, shared(DebugState::default()));
        let error = second
            .err()
            .expect("a live collector's socket is not taken over");
        assert!(format!("{error:#}").contains("already"), "{error:#}");
        drop(first);
    }

    #[test]
    fn attached_counts_live_streams() {
        let state = shared(DebugState::with_limits_and_heartbeat(
            RING_CAPACITY,
            CONNECTION_QUEUE,
            Duration::from_millis(40),
        ));
        let server = start_test_server_with(Arc::clone(&state));
        assert_eq!(state.lock().unwrap().attached(), 0);

        let mut one = open_stream(server.addr, "/debug/stream");
        let mut two = open_stream(server.addr, "/debug/stream");
        read_events(&mut one, Duration::from_millis(150));
        read_events(&mut two, Duration::from_millis(50));
        assert_eq!(state.lock().unwrap().attached(), 2);

        // One leaves: the heartbeat's failed write is what notices.
        drop(one);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while state.lock().unwrap().attached() != 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            state.lock().unwrap().attached(),
            1,
            "the other is still attached"
        );
        drop(two);
        while state.lock().unwrap().attached() != 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(state.lock().unwrap().attached(), 0);
    }

    #[test]
    fn session_summaries_fill_the_picker_until_a_session_frame_replaces_them() {
        let mut state = DebugState::default();
        state.set_session_summaries(vec![
            json!({"session_id": "s1", "agent_app": null, "t_start": "2026-01-01T00:00:00.000Z", "t_last": "2026-01-01T00:01:00.000Z", "events": 3}),
            json!({"session_id": "s2", "agent_app": null, "t_start": "2026-01-02T00:00:00.000Z", "t_last": "2026-01-02T00:01:00.000Z", "events": 9}),
        ]);
        let list = state.session_list();
        assert_eq!(list[0]["session_id"], "s2", "most recent first");
        assert_eq!(list[1]["events"], 3);

        state.publish(
            "session",
            json!({"session_id": "s1", "t_start": "2026-01-01T00:00:00.000Z", "t_last": "2026-01-03T00:00:00.000Z", "events": 5, "session_meta": {"agent_app": {"name": "codex"}}}),
            0,
        );
        let list = state.session_list();
        assert_eq!(list.as_array().unwrap().len(), 2);
        assert_eq!(list[0]["session_id"], "s1");
        assert_eq!(list[0]["events"], 5);
        assert_eq!(list[0]["agent_app"]["name"], "codex");
    }

    #[test]
    fn asking_for_a_session_nobody_published_records_it_as_wanted() {
        let state = shared(DebugState::default());
        state.lock().unwrap().publish(
            "session",
            json!({"session_id": "known", "t_last": "2026-01-01T00:00:00.000Z"}),
            0,
        );
        let server = start_test_server_with(Arc::clone(&state));

        let (status, _, body) = http_get(server.addr, "/debug/session?session_id=known", &[]);
        assert_eq!(status, 200);
        assert!(String::from_utf8_lossy(&body).contains("known"));
        assert!(state.lock().unwrap().take_wanted().is_empty());

        let (status, _, body) = http_get(server.addr, "/debug/report?session_id=old", &[]);
        assert_eq!((status, body.as_slice()), (200, b"null".as_slice()));
        let (_, _, _) = http_get(server.addr, "/debug/session?session_id=old", &[]);
        let wanted = state.lock().unwrap().take_wanted();
        assert_eq!(
            wanted.into_iter().collect::<Vec<_>>(),
            vec!["old".to_string()]
        );
        assert!(state.lock().unwrap().take_wanted().is_empty(), "taken once");
    }

    #[test]
    fn a_page_from_another_origin_cannot_make_the_watcher_work() {
        let state = shared(DebugState::default());
        let server = start_test_server_with(Arc::clone(&state));
        let _ = http_get(
            server.addr,
            "/debug/session?session_id=old",
            &[("Origin", "https://evil.example")],
        );
        assert!(state.lock().unwrap().take_wanted().is_empty());
        let _ = http_get(
            server.addr,
            "/debug/session?session_id=old",
            &[("Origin", "http://127.0.0.1:9414")],
        );
        assert_eq!(
            state.lock().unwrap().take_wanted().len(),
            1,
            "the console's own page may"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_server_leaves_a_socket_file_that_is_no_longer_its_own() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("console.sock");
        let server = serve_unix(&path, shared(DebugState::default())).expect("serve");
        // Another collector replaced the file while this one was running.
        std::fs::remove_file(&path).expect("unlink");
        let _other = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        drop(server);
        assert!(path.exists(), "the other collector's socket survives");
    }

    #[test]
    fn forgetting_the_picture_keeps_the_status_and_drops_the_rest() {
        let mut state = DebugState::default();
        state.set_status(json!({"boot_id": "b1"}));
        state.publish("session", json!({"session_id": "s1"}), 0);
        state.set_session_summaries(vec![json!({"session_id": "s2"})]);
        state.forget_picture();
        assert_eq!(state.session_list(), json!([]));
        assert_eq!(state.status, Some(json!({"boot_id": "b1"})));
    }

    #[test]
    fn dropping_history_resets_any_earlier_cursor() {
        let mut state = DebugState::default();
        state.publish("decision", json!({"kind": "attr"}), 0);
        state.publish("decision", json!({"kind": "attr"}), 0);
        state.forget_picture();
        assert_eq!(state.frames_of("decision"), 0);
        let (replay, _rx) = state.subscribe(Some(0));
        assert!(
            matches!(replay, Replay::Reset),
            "frames it asks for are gone"
        );
    }

    #[test]
    fn status_route_serves_the_stored_payload() {
        let state = shared(DebugState::default());
        let server = start_test_server_with(Arc::clone(&state));

        let (status, _, body) = http_get(server.addr, "/debug/status", &[]);
        assert_eq!(status, 200);
        assert_eq!(
            body, b"null",
            "before the first status there is none to serve"
        );

        let payload = json!({"boot_id": "b1", "agents": []});
        state.lock().unwrap().set_status(payload.clone());
        let (status, _, body) = http_get(server.addr, "/debug/status", &[]);
        assert_eq!(status, 200);
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), payload);
    }

    #[test]
    fn status_is_the_first_frame_on_every_connection() {
        let mut seeded = DebugState::default();
        for i in 0..3 {
            seeded.publish("fact", json!({"i": i}), i);
        }
        seeded.set_status(json!({"boot_id": "b1"}));
        let server = start_test_server_with(shared(seeded));

        // No cursor, a cursor inside the ring, and one that gets `reset`.
        for path in [
            "/debug/stream",
            "/debug/stream?from=0",
            "/debug/stream?from=999",
        ] {
            let mut stream = open_stream(server.addr, path);
            let events = read_events(&mut stream, Duration::from_millis(300));
            let first = events
                .first()
                .unwrap_or_else(|| panic!("{path}: no events"));
            assert_eq!(first.event, "status", "{path}");
            assert_eq!(first.id, None, "{path}: status is not a ring frame");
            assert_eq!(first.data["boot_id"], "b1", "{path}");
        }
    }

    #[test]
    fn a_connection_gets_no_status_frame_when_none_is_stored() {
        let mut seeded = DebugState::default();
        seeded.publish("fact", json!({"i": 0}), 0);
        let server = start_test_server_with(shared(seeded));
        let mut stream = open_stream(server.addr, "/debug/stream?from=-1");
        let events = read_events(&mut stream, Duration::from_millis(300));
        assert_eq!(
            events.iter().map(|e| e.event.as_str()).collect::<Vec<_>>(),
            ["fact"]
        );
    }

    #[test]
    fn set_status_reaches_live_connections_but_not_the_ring() {
        let mut state = state_with_frames();
        let head = state.head_seq();
        let before = snapshot(&state, 60_000);

        let (_replay, rx) = state.subscribe(Some(head as i64 - 1));
        state.set_status(json!({"boot_id": "b1"}));

        let wire = String::from_utf8(rx.try_recv().expect("live status frame")).unwrap();
        assert!(wire.starts_with("event: status\ndata: "), "{wire}");
        assert!(!wire.contains("\nid: "), "{wire}");
        assert_eq!(state.head_seq(), head, "status takes no sequence number");
        assert_eq!(
            snapshot(&state, 60_000),
            before,
            "status is not in the snapshot"
        );
        // A later replay of the whole ring carries no status frame.
        let (replay, _rx) = state.subscribe(Some(EMPTY_CURSOR));
        let Replay::Frames(frames) = replay else {
            panic!("expected a replay");
        };
        assert!(frames
            .iter()
            .all(|bytes| !bytes.starts_with(b"event: status")));
    }

    #[test]
    fn a_cursor_ahead_of_the_head_is_reset() {
        let mut state = DebugState::with_limits(16, 8);
        for i in 0..3 {
            state.publish("fact", json!({"i": i}), i);
        }
        // Caught up exactly: nothing to replay, and nothing wrong.
        assert!(replayed_ids(&state.subscribe(Some(2)).0).is_empty());
        // A cursor this process never issued comes from another process —
        // a restarted watcher. "Up to date" would be a lie.
        assert!(matches!(state.subscribe(Some(3)).0, Replay::Reset));
        assert!(matches!(state.subscribe(Some(193_933)).0, Replay::Reset));
    }

    #[test]
    fn a_cursor_on_an_empty_ring_is_reset_unless_it_is_the_empty_cursor() {
        let mut state = DebugState::with_limits(16, 8);
        assert!(replayed_ids(&state.subscribe(Some(EMPTY_CURSOR)).0).is_empty());
        assert!(matches!(state.subscribe(Some(0)).0, Replay::Reset));
        assert!(matches!(state.subscribe(Some(97_163)).0, Replay::Reset));
    }

    fn heartbeat_state(heartbeat: Duration) -> DebugState {
        let mut state =
            DebugState::with_limits_and_heartbeat(RING_CAPACITY, CONNECTION_QUEUE, heartbeat);
        state.set_status(json!({"boot_id": "b1"}));
        state
    }

    #[test]
    fn a_quiet_stream_gets_a_ping_with_the_boot_id() {
        let server = start_test_server_with(shared(heartbeat_state(Duration::from_millis(50))));
        let mut stream = open_stream(server.addr, "/debug/stream");
        let events = read_events(&mut stream, Duration::from_millis(400));
        let pings: Vec<&WireEvent> = events.iter().filter(|e| e.event == "ping").collect();
        assert!(
            pings.len() >= 2,
            "expected at least two pings, got {events:?}"
        );
        for ping in pings {
            assert_eq!(ping.data["boot_id"], "b1");
            assert_eq!(ping.id, None, "a ping is not a ring frame");
        }
    }

    #[test]
    fn a_ping_without_a_status_carries_a_null_boot_id() {
        let state = DebugState::with_limits_and_heartbeat(16, 8, Duration::from_millis(50));
        let server = start_test_server_with(shared(state));
        let mut stream = open_stream(server.addr, "/debug/stream");
        let events = read_events(&mut stream, Duration::from_millis(300));
        let ping = events.iter().find(|e| e.event == "ping").expect("a ping");
        assert_eq!(ping.data["boot_id"], Value::Null);
    }

    #[test]
    fn pings_stop_while_frames_flow() {
        // Frames every 50 ms for longer than the stream is read, which is
        // itself longer than the heartbeat: without frames a ping would
        // have been due. Only a pause of a whole second between two frames
        // could let one through, so a slow machine does not fail this; how
        // many frames arrive in the window is not what is being tested.
        let state = shared(heartbeat_state(Duration::from_secs(1)));
        let server = start_test_server_with(Arc::clone(&state));
        let mut stream = open_stream(server.addr, "/debug/stream");
        let publisher = std::thread::spawn(move || {
            for i in 0..32 {
                std::thread::sleep(Duration::from_millis(50));
                state.lock().unwrap().publish("fact", json!({"i": i}), i);
            }
        });
        let events = read_events(&mut stream, Duration::from_millis(1300));
        publisher.join().unwrap();
        assert!(events.iter().any(|e| e.event == "fact"), "frames flowed");
        assert!(
            events.iter().all(|e| e.event != "ping"),
            "a stream that is carrying frames needs no heartbeat: {events:?}"
        );
    }

    #[test]
    fn a_vanished_client_is_reaped_by_the_heartbeat() {
        let server = start_test_server_with(shared(heartbeat_state(Duration::from_millis(50))));
        let mut stream = open_stream(server.addr, "/debug/stream");
        // Wait for the stream to be established, then vanish without a word.
        let events = read_events(&mut stream, Duration::from_millis(150));
        assert_eq!(events[0].event, "status");
        drop(stream);
        // Nothing is ever published: only the heartbeat's failed write can
        // end the handler.
        let finished = server
            .done
            .recv_timeout(Duration::from_secs(3))
            .expect("the stream handler ends once its client is gone");
        assert_eq!(finished, "/debug/stream");
    }
}
