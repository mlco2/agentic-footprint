//! `af watch`: resident mode.
//!
//! One process that keeps the whole control plane running while an agent
//! session is live:
//!
//! * the **OTLP receiver** (`af-otlp`) on `127.0.0.1:4318`, so Claude Code's
//!   telemetry exporter has somewhere to post;
//! * an **fs-watch** on the spool directory (the `notify` crate) with a
//!   periodic tick as a fallback, driving one ingest → estimate → join pass
//!   per wake;
//! * the **codecarbon sampler sidecar**, one per session, told which pid
//!   tree to follow;
//! * with `--debug`, the human-readable decision stream on stderr **and**
//!   the `/debug` HTTP+SSE server the debug console consumes.
//!
//! ## Loop semantics
//!
//! An fs event does not run a pass immediately — collectors append line by
//! line, and a burst of appends would otherwise mean a burst of passes over
//! the same file. A wake starts a short debounce; the pass runs once the
//! spool has been quiet for [`DEBOUNCE`]. Independently, a lightweight
//! supervision pass runs every [`TICK`] without touching the spool, while a
//! full-directory [`RECONCILE`] pass recovers from missed notifications. A
//! watcher error or failed ingest forces that reconciliation on the next loop.
//!
//! ## Shutdown
//!
//! `SIGINT`/`SIGTERM` set a flag the loop checks at least every
//! [`IDLE_POLL`]. Shutdown
//! is graceful and ordered: `shutdown` op to every sampler (so its last
//! window reaches the spool), then a final ingest pass to pick that window
//! up, then the servers, then exit 0. A killed sampler would lose the
//! window in flight — that is the difference between a clean stop and a
//! coverage gap.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use af_core::{
    apportion, apportion_traced, correlate, rebuild_prepared_stored, Apportionment,
    EstimationRegion, PreparedSession, SampleTrace, SessionTree, Zone,
};
use af_events::{EnergySample, Envelope, Payload, ProcessSample};
use af_sidecar::{doctor, venv_python, Severity, Sidecar};
use af_store::Store;

use super::agents::{self, AgentFacts, AgentId};
use super::debug_frames::{
    alloc_trace, attr_decision, ingest_decision, orphan_decision, otlp_reject_frame,
    policy_schema_name, reject_frame, span_open_decision, watchdog_entries, Decision,
};
use super::debug_server::{self, DebugState};
use super::estimator;
use super::estimator_worker::Worker as EstimatorWorker;
use super::ingest::{ingest, ingest_paths, IngestFileState, IngestMetrics, RejectRecord};
use super::{now_ms, sidecar_script};

/// The longest the loop sleeps: how often an idle loop wakes to check the
/// shutdown flag, and so the ceiling on how long a `SIGTERM` waits before
/// anything happens. Received telemetry wakes it at once; every other
/// deadline (debounce, flush, supervision, reconciliation) is slept until,
/// not polled for — see [`loop_wait`].
const IDLE_POLL: Duration = Duration::from_secs(1);
/// How often the resident watch writes. Each flush is one commit, and a
/// commit rewrites whole pages twice (WAL, then checkpoint), so this, not
/// the volume of telemetry, is what the SSD wear follows: at 10 s a day of
/// work measured about 600 MiB physical. Once a minute trades that for
/// numbers up to a minute old in the status line and console, and up to a
/// minute of received telemetry held in memory (written on a clean stop).
pub const DEFAULT_BATCH_INTERVAL_MS: u64 = 60_000;

/// Quiet period after the last fs event before a pass runs.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Maximum time between passes with no fs events at all.
const TICK: Duration = Duration::from_secs(2);
/// Full-directory safety scan for missed filesystem notifications. Normal
/// appends take the dirty-path route; this deliberately slower pass keeps
/// correctness independent of a platform watcher backend.
const RECONCILE: Duration = Duration::from_secs(30);
/// Default OTLP receiver address — http/json, never 4317 (that is the gRPC
/// port, which this PoC does not speak).
pub const DEFAULT_OTLP_ADDR: &str = "127.0.0.1:4318";
/// Default `/debug` address. Port 9414 is DATA-CONTRACT §2's suggestion.
pub const DEFAULT_DEBUG_ADDR: &str = "127.0.0.1:9414";

/// How many reject records the health payload carries. Older ones are
/// counted (`rejected_dropped`) rather than kept: the health frame is
/// republished, ring-buffered and fanned out every pass, and a spool of
/// malformed lines would otherwise grow it without bound.
const REJECT_HISTORY: usize = 100;

/// Receiver batches queued for the loop before the receiver answers 503.
const INBOX_CAPACITY: usize = 1024;

/// Received events kept for a retry while the store refuses writes.
const MAX_PENDING_FACTS: usize = 50_000;

/// The sampler script's path relative to the Python source root
/// (see [`super::sidecar_script`]).
pub(crate) const SAMPLER_SCRIPT: &str = "af_sampler/__main__.py";

/// The methodology artifact this build carries.
///
/// The one there is is the estimator sidecar's ecologits pin, and it is
/// only knowable once a call has been estimated. Until then, say so.
const METHODOLOGY_VERSION: &str = "unknown until the first estimate";

/// The longest a `af python doctor` result is reused while the managed
/// runtime looks unchanged.
///
/// `doctor` forks the managed venv's interpreter to import ecologits,
/// codecarbon and psutil (~90 ms of CPU, ~40 MiB peak) to answer a question
/// whose answer changes only when someone runs `af python setup`. So the
/// result is kept until the runtime's fingerprint
/// ([`af_sidecar::environment_fingerprint`]) moves, and recomputed at most
/// hourly otherwise, for changes no modification time records.
const PYTHON_HEALTH_TTL: Duration = Duration::from_secs(3600);
const SAMPLER_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How many event ids the "already said this" memories keep.
///
/// [`Watch::reported_events`] is a dedup set, and an unbounded dedup set in
/// a process designed to stay resident for a working day is a leak with a
/// nice name: one entry per event ever
/// ingested, held until the process exits. The horizon is twice the debug
/// ring's capacity, so anything a console could still be looking at is
/// still deduplicated; an id evicted past it could in principle be
/// re-announced, which costs a duplicate frame for a record no live view
/// holds any more.
const DEDUP_HORIZON: usize = 16_384;

/// Flags for one `af watch` run.
pub struct WatchArgs {
    pub debug: bool,
    pub no_sidecars: bool,
    pub no_otlp: bool,
    /// Fail instead of continuing without a receiver when the OTLP
    /// address cannot be bound.
    pub require_receiver: bool,
    pub otlp_addr: String,
    pub debug_addr: String,
    pub interval: f64,
    pub local_grid_zone: Option<String>,
    pub remote_region: Option<String>,
    /// How often received telemetry and spool changes are written, and the
    /// minimum time between derived-view rebuilds of a session with new
    /// facts (sample-only staleness waits [`SAMPLE_REBUILD_FACTOR`] times
    /// longer). New sessions skip the wait. Tests pass zero.
    pub batch_interval: Duration,
    /// How far back a backfill reaches, in milliseconds; 0 is everything.
    pub console_window_ms: i64,
}

/// Termination-request plumbing: one flag, set by the platform's stop
/// signal, polled by the resident loop.
///
/// Two reactions per signal, and the order is the feature: the first
/// Ctrl-C (or SIGTERM / console-close) sets the flag and runs the graceful
/// path (shutdown ops, final ingest pass), while a **second** signal exits
/// immediately with 130. Without the escape hatch a user watching a
/// sampler take its time to flush has nothing to press — graceful shutdown
/// becomes a hang they can only answer with `kill -9`, which is the
/// outcome the graceful path existed to avoid.
mod shutdown {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use anyhow::Result;

    #[cfg(unix)]
    pub fn install(term: &Arc<AtomicBool>) -> Result<()> {
        use anyhow::Context;
        // The *conditional* handler exits only if the flag is already set,
        // so the first signal falls through to the flag registration below.
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            signal_hook::flag::register_conditional_shutdown(signal, 130, Arc::clone(term))
                .with_context(|| format!("registering the second-signal escape for {signal}"))?;
            signal_hook::flag::register(signal, Arc::clone(term))
                .with_context(|| format!("registering signal handler for {signal}"))?;
        }
        Ok(())
    }

    #[cfg(windows)]
    pub fn install(term: &Arc<AtomicBool>) -> Result<()> {
        use std::sync::atomic::Ordering;
        use std::sync::OnceLock;

        use anyhow::bail;
        use windows_sys::core::BOOL;
        use windows_sys::Win32::Foundation::{FALSE, TRUE};
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

        // The handler runs on a system-spawned thread with no user data
        // pointer, so the flag rides in a process-wide static.
        static TERM: OnceLock<Arc<AtomicBool>> = OnceLock::new();

        // Ctrl+C, Ctrl+Break, console close, logoff, and shutdown all
        // request the same graceful stop; a second event is the immediate
        // escape, mirroring the unix double-signal contract (exit 130).
        unsafe extern "system" fn handler(_ctrl_type: u32) -> BOOL {
            if let Some(term) = TERM.get() {
                if term.swap(true, Ordering::SeqCst) {
                    std::process::exit(130);
                }
                return TRUE;
            }
            FALSE
        }

        TERM.set(Arc::clone(term)).ok();
        // SAFETY: `handler` is a valid console-control callback for the
        // life of the process, and TERM is initialized before registration.
        if unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) } == 0 {
            bail!("registering the console control handler failed");
        }
        Ok(())
    }
}

/// Parses a listen address and refuses anything but loopback: the receiver
/// and the `/debug` API have no authentication, and a wildcard or LAN bind
/// would hand both to the network (a `Host: 127.0.0.1` header passes the
/// host check from anywhere).
pub(crate) fn loopback_addr(flag: &str, value: &str) -> Result<SocketAddr> {
    let addr: SocketAddr = value
        .parse()
        .with_context(|| format!("invalid {flag} {value}"))?;
    if !addr.ip().is_loopback() {
        bail!("{flag} {value}: af listens on loopback addresses only (127.0.0.1, ::1)");
    }
    Ok(addr)
}

/// Runs the resident loop until the platform stop signal
/// (`SIGINT`/`SIGTERM` on unix, Ctrl+C / console close on Windows).
pub fn run(state_dir: &Path, args: WatchArgs) -> Result<()> {
    // Checked before anything starts: a bad address is a configuration
    // error, not a receiver that is merely unavailable.
    let otlp_addr = (!args.no_otlp)
        .then(|| loopback_addr("--otlp-addr", &args.otlp_addr))
        .transpose()?;
    let debug_addr = args
        .debug
        .then(|| loopback_addr("--debug-addr", &args.debug_addr))
        .transpose()?;
    crate::paths::ensure_private_dir(state_dir)
        .with_context(|| format!("securing {}", state_dir.display()))?;
    let spool_dir = state_dir.join("spool");
    std::fs::create_dir_all(&spool_dir)
        .with_context(|| format!("creating spool dir {}", spool_dir.display()))?;

    let term = Arc::new(AtomicBool::new(false));
    shutdown::install(&term)?;

    // The OTLP receiver is best-effort: a port already in use (a second
    // `af watch`, or Claude Code pointed at someone else's collector) must
    // not cost the user their spool ingestion, which needs no network at
    // all. The failure is reported, loudly, and the loop continues.
    // The receiver's output: normalized events and metadata-only rejects,
    // stored by this loop (the one owner of the database). Bounded, so a
    // loop busy rebuilding makes the receiver answer 503 instead of
    // buffering without limit.
    let (inbox_tx, inbox_rx) = mpsc::sync_channel::<af_otlp::Inbound>(INBOX_CAPACITY);
    let mut otlp = if let Some(addr) = otlp_addr {
        match af_otlp::serve(addr, inbox_tx.clone()) {
            Ok(handle) => {
                eprintln!(
                    "af watch: OTLP http/json receiver on http://{} (POST /v1/logs, /v1/metrics)",
                    handle.addr()
                );
                Some(handle)
            }
            // A service manager only retries a process that exits: the
            // resident service runs with --require-receiver, so a port held
            // by someone else (a foreground watch, a stale daemon) is
            // retried until it frees up instead of never being served.
            Err(err) if args.require_receiver => {
                eprintln!("af watch: OTLP receiver unavailable ({err:#}); exiting");
                bail!("OTLP receiver unavailable on {addr}");
            }
            Err(err) => {
                eprintln!("af watch: OTLP receiver unavailable ({err:#}); continuing without it");
                None
            }
        }
    } else {
        eprintln!(
            "af watch: OTLP receiver disabled (--no-otlp); llm_call telemetry will not be received"
        );
        None
    };

    // Only the receiver's workers hold senders now, so the inbox reports
    // `Disconnected` when there is no receiver at all.
    drop(inbox_tx);

    // One debug state, always: the console socket serves it, and `--debug`
    // serves it on a TCP port as well.
    let debug_state = Arc::new(Mutex::new(DebugState::default()));
    let socket = state_dir.join(debug_server::SOCKET_NAME);
    if args.debug && otlp.is_none() && !args.no_otlp && debug_server::socket_is_served(&socket) {
        bail!(
            "a collector is already running here, and `af watch --debug` would receive nothing beside it; open its console with `af console`"
        );
    }
    // Not fatal: without it there is no `af console`, and everything else
    // works. A second watcher beside a running collector ends up here.
    let _console_socket = match debug_server::serve_socket(&socket, Arc::clone(&debug_state)) {
        Ok(server) => server,
        Err(err) => {
            eprintln!(
                "af watch: no console socket ({err:#}); `af console` cannot attach to this process"
            );
            None
        }
    };
    let debug_server = if let Some(addr) = debug_addr {
        let server = debug_server::serve(addr, Arc::clone(&debug_state))
            .context("starting the /debug server")?;
        eprintln!(
            "af watch: debug console on http://{}/ (API endpoints under /debug/*)",
            server.addr()
        );
        Some(server)
    } else {
        None
    };

    let (fs_tx, fs_rx) = mpsc::channel::<FsNotice>();
    // `notify`'s handle must outlive the loop: dropping the watcher stops
    // the events.
    let _watcher = spawn_watcher(&spool_dir, fs_tx)?;

    // **One connection for the life of the process.** Every pass used to
    // open two — one inside `ingest`, one for the rebuild — so a watch on a
    // two-second tick re-ran sqlite's `PRAGMA journal_mode`, its busy
    // timeout and its migration check thousands of times a day, and had two
    // handles on the same file able to contend with each other.
    let mut store = Store::open(&state_dir.join("state.db"))?;

    let suppress_duplicate_sidecars = !args.no_otlp && otlp.is_none();
    if suppress_duplicate_sidecars && !args.no_sidecars {
        eprintln!(
            "af watch: another receiver likely owns {}; sidecars disabled in this process to avoid duplicate machine measurement",
            args.otlp_addr
        );
    }
    let effective_args = WatchArgs {
        no_sidecars: args.no_sidecars || suppress_duplicate_sidecars,
        ..args
    };
    let mut watch = Watch::new(
        state_dir,
        &effective_args,
        Some(Arc::clone(&debug_state)),
        otlp.as_ref()
            .map(|handle| (handle.addr(), handle.counters())),
    );
    watch.opaque_events = store.count_opaque_events().unwrap_or(0);
    // Behind `af console`, diagnostics follow whoever is attached; with
    // `--debug` they are on for the life of the process.
    watch.on_demand = debug_server.is_none();
    watch.watched = debug_server.is_some();
    // The store may hold days of history: a console is told the present.
    watch.backfill = true;

    // Before the first pass, so `/debug/status` answers from the first
    // second: the startup pass can take a while on a large store, and an
    // empty one never publishes a session at all.
    watch.refresh_agents();
    watch.publish_status(Instant::now(), true);
    let mut last_detect = Instant::now();

    // One pass before entering the loop: a spool that already has content
    // when `af watch` starts must not wait for the first event or tick.
    // Failure is logged and the loop entered anyway — exactly the tolerance
    // every later pass gets. A half-written spool file or a database still
    // locked by another `af` is a condition the *next* pass clears, and
    // exiting here would take the OTLP receiver and the debug server down
    // over it, which is the one failure mode a resident process must not
    // have.
    if let Err(err) = watch.pass(&mut store, PassKind::FullReconcile) {
        eprintln!("af watch: startup pass failed: {err:#}");
    }
    watch.request_estimates();
    if let Err(err) = watch.rebuild_due(&mut store) {
        eprintln!("af watch: startup rebuild failed: {err:#}");
    }
    watch.backfill = false;

    let mut last_pass = Instant::now();
    let mut last_reconcile = Instant::now();
    let mut pending_since: Option<Instant> = None;
    let mut dirty_paths = BTreeSet::new();
    let mut force_reconcile = false;
    // Received telemetry not yet written: waiting for the next flush, or
    // refused by the store and retried.
    let mut pending_inbox = Inbox::default();
    let mut flush = FlushClock::default();
    while !term.load(Ordering::Relaxed) {
        watch.take_estimator_completion(&store);
        let mut inbox = std::mem::take(&mut pending_inbox);
        // Writes wait for the flush clock, so a write's deadline is never
        // earlier than the next flush.
        let now = Instant::now();
        let flush_at = flush.next(watch.batch_interval).unwrap_or(now);
        let writable = |at: Instant| Some(at.max(flush_at));
        let wait = loop_wait(
            now,
            [
                pending_since.and_then(|at| writable(at + DEBOUNCE)),
                (!inbox.is_empty() || watch.has_estimates_to_store()).then_some(flush_at),
                watch
                    .dirty
                    .next_due(now, watch.batch_interval)
                    .and_then(|(at, waits)| if waits { writable(at) } else { Some(at) }),
                watch.has_supervision().then_some(last_pass + TICK),
                writable(last_reconcile + RECONCILE),
            ],
        );
        match inbox_rx.recv_timeout(wait) {
            Ok(item) => {
                watch.note_inbound(&item, now_ms());
                inbox.push(item)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // No receiver (`--no-otlp`, or the bind failed): keep the
            // loop's cadence instead of spinning.
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(wait),
        }
        while let Ok(item) = inbox_rx.try_recv() {
            watch.note_inbound(&item, now_ms());
            inbox.push(item);
        }
        while let Ok(notice) = fs_rx.try_recv() {
            if let FsNotice::Paths(paths) = &notice {
                watch.note_spool_paths(paths, now_ms());
            }
            absorb_notice(notice, &mut dirty_paths, &mut force_reconcile);
            pending_since = Some(Instant::now());
        }
        let now = Instant::now();
        watch.sync_attachment(&mut store);
        if now.saturating_duration_since(last_detect) >= DETECT_INTERVAL {
            watch.refresh_agents();
            last_detect = now;
        }
        watch.publish_status(now, false);
        let something_new = watch.has_new_session(&inbox.facts)
            || watch.has_unknown_spool_file(&dirty_paths)
            || inbox.facts.len() >= MAX_BATCHED_FACTS;
        let flush_due = flush.due(now, watch.batch_interval, something_new);
        if term.load(Ordering::Relaxed) {
            pending_inbox = inbox;
            break;
        }

        let debounced = pending_since
            .map(|at| at.elapsed() >= DEBOUNCE)
            .unwrap_or(false);
        // Everything that writes waits for the flush, reconciliation too: a
        // write pass is one commit, and the commit count is the wear.
        let reconcile_due = flush_due && (force_reconcile || last_reconcile.elapsed() >= RECONCILE);
        let pass_kind = if reconcile_due {
            dirty_paths.clear();
            force_reconcile = false;
            pending_since = None;
            last_reconcile = Instant::now();
            Some(PassKind::FullReconcile)
        } else if debounced && flush_due {
            pending_since = None;
            Some(PassKind::Dirty(std::mem::take(&mut dirty_paths)))
        } else if last_pass.elapsed() >= TICK {
            Some(PassKind::Supervise)
        } else {
            None
        };
        let absorb = flush_due && !inbox.is_empty();
        let ingest_pass = matches!(
            pass_kind,
            Some(PassKind::FullReconcile | PassKind::Dirty(_))
        );
        let rebuild = watch.dirty.due(now, watch.batch_interval, flush_due);
        if pass_kind.is_some() {
            last_pass = Instant::now();
        }
        if !(absorb || ingest_pass || rebuild || (flush_due && watch.has_estimates_to_store())) {
            pending_inbox = inbox;
            if let Some(PassKind::Supervise) = pass_kind {
                watch.supervise(&BTreeSet::new());
            }
            continue;
        }
        if flush_due {
            flush.flushed(now);
        }

        let outcome = watch.write_flush(&mut store, inbox, flush_due, pass_kind);
        pending_inbox = outcome.unwritten;
        force_reconcile |= outcome.reconcile;
    }

    eprintln!("af watch: shutting down");
    watch.shutdown();
    // Stop the receiver first, so no worker is still sending, then store
    // whatever it had already queued.
    if let Some(handle) = otlp.take() {
        handle.shutdown();
    }
    let mut inbox = pending_inbox;
    while let Ok(item) = inbox_rx.try_recv() {
        inbox.push(item);
    }
    if !inbox.is_empty() {
        if let Err(err) = watch.absorb_inbox(&mut store, &inbox) {
            eprintln!(
                "af watch: storing the last received telemetry failed: {err:#}; {} event(s) lost",
                inbox.facts.len()
            );
        }
    }
    if let Err(err) = watch.pass(&mut store, PassKind::FullReconcile) {
        eprintln!("af watch: final ingest pass failed: {err:#}");
    }
    watch.take_estimator_completion(&store);
    match watch.store_estimates(&mut store) {
        Ok(()) => watch.estimates_stored(),
        Err(err) => eprintln!("af watch: storing the last estimates failed: {err:#}"),
    }
    if let Err(err) = watch.rebuild_all_dirty(&mut store) {
        eprintln!("af watch: final rebuild failed: {err:#}");
    }
    // Dropping the server is what stops it — see [`debug_server::DebugServer`].
    drop(debug_server);
    Ok(())
}

/// How long the loop may sleep: until the earliest of `deadlines`, never
/// longer than [`IDLE_POLL`] and never shorter than [`MIN_WAIT`] — a
/// deadline the loop cannot act on yet must not turn into a spin.
fn loop_wait(now: Instant, deadlines: impl IntoIterator<Item = Option<Instant>>) -> Duration {
    deadlines
        .into_iter()
        .flatten()
        .map(|at| at.saturating_duration_since(now))
        .min()
        .unwrap_or(IDLE_POLL)
        .clamp(MIN_WAIT, IDLE_POLL)
}

/// How often agent detection (two config files, `PATH`, one `ps`) is
/// refreshed. It writes nothing, so it runs on its own clock rather than
/// waiting for a flush the way reconciliation does.
const DETECT_INTERVAL: Duration = Duration::from_secs(30);

/// The least time between two `status` frames. Activity changes the status
/// on every event; a console needs to learn "this agent is sending", not
/// to be told so once per event.
const STATUS_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// An identifier for this watcher process: fixed for its life, different
/// for any other. The console compares it across reconnects to learn that
/// the process it is talking to is not the one whose history it holds.
///
/// Start time and pid, mixed (splitmix64) so the result does not read as a
/// timestamp. For a fixed pid the mix is a bijection of the start time, so
/// a reused pid still gets a new id.
fn boot_id(started: SystemTime, pid: u32) -> String {
    let nanos = started
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = nanos.wrapping_add(u64::from(pid).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    format!("{:016x}", z ^ (z >> 31))
}

/// The shortest sleep [`loop_wait`] returns.
const MIN_WAIT: Duration = Duration::from_millis(20);

enum FsNotice {
    Paths(Vec<PathBuf>),
    Error(String),
}

fn absorb_notice(
    notice: FsNotice,
    dirty_paths: &mut BTreeSet<PathBuf>,
    force_reconcile: &mut bool,
) {
    match notice {
        FsNotice::Paths(paths) => dirty_paths.extend(paths),
        FsNotice::Error(error) => {
            eprintln!("af watch: filesystem watcher error: {error}; forcing reconciliation");
            *force_reconcile = true;
        }
    }
}

/// Watches `spool_dir` non-recursively and retains the changed paths. The
/// debounce loop deduplicates them before targeted ingestion.
fn spawn_watcher(
    spool_dir: &Path,
    tx: mpsc::Sender<FsNotice>,
) -> Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};

    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let notice = match event {
            Ok(event) => FsNotice::Paths(event.paths),
            Err(error) => FsNotice::Error(error.to_string()),
        };
        let _ = tx.send(notice);
    })
    .context("creating the filesystem watcher")?;
    watcher
        .watch(spool_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {}", spool_dir.display()))?;
    Ok(watcher)
}

enum PassKind {
    FullReconcile,
    Dirty(BTreeSet<PathBuf>),
    Supervise,
}

/// Backoff floor and ceiling for respawning a sampler that died.
const RESPAWN_BASE: Duration = Duration::from_secs(2);
const RESPAWN_CAP: Duration = Duration::from_secs(60);
/// A sampler that ran at least this long before dying is treated as a fresh
/// failure rather than a continuation of an earlier one — otherwise an hour
/// of healthy sampling followed by one crash would be punished with the
/// backoff that a crash loop earned an hour ago.
const RESPAWN_HEALTHY_AFTER: Duration = RESPAWN_CAP;

/// How long a session may produce no events before its sampler is retired.
///
/// Nothing in Contract #1 marks a session as over: there is no
/// `session_end` event, and the hook shim's `Stop`/`SessionEnd` produce
/// ordinary `action_span`s. So `known_sessions` only ever grew, and with it
/// the set of samplers a resident `af watch` supervised — one Python
/// process per session ever seen, each polling forever, for a session whose
/// Claude Code window closed hours ago.
///
/// Inactivity is the honest available signal. Ten minutes is well past any
/// pause inside a live session (the sampler itself emits energy samples
/// every few seconds while it runs, so a live session is never quiet) and
/// short enough that a day's work doesn't accumulate dozens of samplers.
///
/// Retirement is not a decision that the session ended: if an event for it
/// arrives later, the ordinary path adds it back to [`Watch::sessions`] and
/// spawns a fresh sampler.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// One supervised sampler sidecar.
struct SamplerProcess {
    sidecar: Sidecar,
    /// When this process was spawned, for the "it was healthy" rule above.
    spawned_at: Instant,
}

/// Respawn bookkeeping for one session, kept whether or not a sampler is
/// currently running for it.
#[derive(Default)]
struct SamplerRetry {
    /// Consecutive deaths (or spawn failures), driving the backoff.
    failures: u32,
    /// Earliest instant a respawn may be attempted. `None` = now.
    next_attempt: Option<Instant>,
    /// Whether the "no managed venv" note has been printed for this session.
    /// Printed once, not once per pass: a note repeated every two seconds is
    /// noise a user learns to scroll past, and this one matters.
    venv_noted: bool,
    last_error: Option<String>,
}

/// Everything the watch knows about one session it has seen an event for.
///
/// One record rather than four parallel maps (`samplers`,
/// `known_sessions`, `last_event_at`, `sampler_retry`). Those had to be
/// kept in step by hand — retirement cleared three of them one line at a
/// time, and a session present in one and absent from another was a
/// silently wrong state no type could rule out. Membership in this map *is*
/// "known session": there is nowhere else for the answer to live.
struct SessionState {
    watch_sent: bool,
    /// When this session last produced an event, driving
    /// [`SESSION_IDLE_TIMEOUT`] retirement.
    last_event_at: Instant,
}

impl SessionState {
    fn seen(now: Instant) -> Self {
        SessionState {
            watch_sent: false,
            last_event_at: now,
        }
    }
}

/// Sessions whose last event is at least `timeout` old.
///
/// Pure, so the retirement rule can be tested against explicit instants
/// rather than by waiting on a real clock.
fn idle_sessions(
    sessions: &BTreeMap<String, SessionState>,
    now: Instant,
    timeout: Duration,
) -> Vec<String> {
    sessions
        .iter()
        .filter(|(_, state)| now.saturating_duration_since(state.last_event_at) >= timeout)
        .map(|(session, _)| session.clone())
        .collect()
}

/// A dedup set with a hard ceiling on how much it can remember.
///
/// Answers one question — "have I already announced this id?" — over a
/// bounded window of the most recently announced ids. See
/// [`DEDUP_HORIZON`] for why the window exists and what falling out of it
/// costs.
struct BoundedSet {
    seen: BTreeSet<String>,
    /// Insertion order, so the oldest id is the one evicted.
    order: VecDeque<String>,
    capacity: usize,
}

impl BoundedSet {
    fn new(capacity: usize) -> Self {
        BoundedSet {
            seen: BTreeSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// `true` when `key` had not been seen within the current window, i.e.
    /// when the caller should go ahead and announce it.
    fn insert(&mut self, key: &str) -> bool {
        if !self.seen.insert(key.to_string()) {
            return false;
        }
        self.order.push_back(key.to_string());
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

/// `2s, 4s, 8s, 16s, 32s, 60s, 60s…` — bounded exponential backoff.
///
/// A sampler that dies the instant it starts (no codecarbon, a venv from a
/// different Python, a machine with no readable RAPL) would otherwise be
/// respawned once per pass, forever: two spawns a second, each writing its
/// own failure to stderr. The measurement is lost either way; the backoff is
/// about not turning a lost measurement into a fork bomb.
fn respawn_delay(failures: u32) -> Duration {
    let steps = failures.saturating_sub(1).min(16);
    let secs = RESPAWN_BASE
        .as_secs()
        .saturating_mul(1u64 << steps)
        .min(RESPAWN_CAP.as_secs());
    Duration::from_secs(secs)
}

/// When received telemetry and spool changes are written.
///
/// A commit writes whole pages twice (WAL, then checkpoint), so a resident
/// writer's disk wear follows its number of commits. Sampler windows land
/// every few seconds and Claude Code exports every two; writing each one
/// on arrival is what this batches into one flush per interval. Something
/// new — a session or spool file this watch has not seen — is written at
/// once, so its sampler watch and first rebuild are not delayed.
#[derive(Debug, Default)]
struct FlushClock {
    last: Option<Instant>,
}

impl FlushClock {
    fn due(&self, now: Instant, interval: Duration, something_new: bool) -> bool {
        something_new
            || self
                .last
                .is_none_or(|at| now.saturating_duration_since(at) >= interval)
    }

    fn flushed(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// When the next flush is due; `None` before the first, which is due
    /// at once.
    fn next(&self, interval: Duration) -> Option<Instant> {
        self.last.map(|at| at + interval)
    }
}

/// What a [`Watch::write_flush`] left for the loop.
struct FlushOutcome {
    /// Received telemetry still to write: refused by the store, or rolled
    /// back with a failed commit.
    unwritten: Inbox,
    /// A pass or the commit failed; reconcile the spool on the next flush.
    reconcile: bool,
}

/// Received events held for one flush before it is forced early, so a burst
/// cannot grow the batch without bound.
const MAX_BATCHED_FACTS: usize = 5_000;

/// What the receiver sent since the last loop iteration.
#[derive(Debug, Default)]
struct Inbox {
    facts: Vec<Envelope>,
    rejects: Vec<af_otlp::Reject>,
}

impl Inbox {
    fn push(&mut self, item: af_otlp::Inbound) {
        match item {
            af_otlp::Inbound::Facts(events) => self.facts.extend(events),
            af_otlp::Inbound::Reject(reject) => self.rejects.push(reject),
        }
    }

    fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.rejects.is_empty()
    }

    /// Keeps at most the newest `max` facts for a retry, saying what fell
    /// off: a store that stays broken must not grow the retry without bound.
    fn capped(mut self, max: usize) -> Self {
        if self.facts.len() > max {
            let dropped = self.facts.len() - max;
            self.facts.drain(..dropped);
            eprintln!("af watch: store still failing; dropped {dropped} received event(s)");
        }
        self
    }
}

/// Sample-only staleness is rebuilt this many fact intervals apart: a
/// sampler window lands every few seconds, and rebuilding a session's
/// joins for each one is what kept the database rewriting itself.
const SAMPLE_REBUILD_FACTOR: u32 = 6;

/// Sessions whose derived views are stale, split by what made them stale.
///
/// A session with a new fact (llm call, span, metadata) is rebuilt at most
/// once per interval; a session that only gained energy samples waits
/// [`SAMPLE_REBUILD_FACTOR`] intervals. Neither is ever dropped: the next
/// due rebuild takes both sets, and shutdown takes everything. A session
/// that has never been rebuilt is due at once: its first rebuild is what
/// sends its sampler the pids to watch.
#[derive(Debug, Default)]
struct DirtySessions {
    facts: BTreeSet<String>,
    samples: BTreeSet<String>,
    last_rebuild: Option<Instant>,
    /// Sessions rebuilt at least once by this process. Grows by one entry
    /// per session, the same order as [`Watch::sessions`].
    rebuilt: BTreeSet<String>,
}

impl DirtySessions {
    /// `touched`: every session that gained events; `facts`: the subset
    /// that gained something other than sampler output.
    fn mark(&mut self, touched: &BTreeSet<String>, facts: &BTreeSet<String>) {
        for session in touched {
            if facts.contains(session) {
                self.samples.remove(session);
                self.facts.insert(session.clone());
            } else if !self.facts.contains(session) {
                self.samples.insert(session.clone());
            }
        }
    }

    /// Whether a rebuild is due now. Interval-based staleness waits for a
    /// flush (`flushing`), so the rebuild shares that flush's commit; a
    /// session never rebuilt does not wait.
    fn due(&self, now: Instant, interval: Duration, flushing: bool) -> bool {
        let waited = |needed: Duration| {
            self.last_rebuild
                .is_none_or(|at| now.saturating_duration_since(at) >= needed)
        };
        self.has_first_time()
            || (flushing
                && ((!self.facts.is_empty() && waited(interval))
                    || (!self.samples.is_empty() && waited(interval * SAMPLE_REBUILD_FACTOR))))
    }

    /// When the earliest rebuild becomes due by its interval, if anything
    /// is dirty, and whether it waits for a flush: a session never rebuilt
    /// is due at once and does not.
    fn next_due(&self, now: Instant, interval: Duration) -> Option<(Instant, bool)> {
        if self.has_first_time() {
            return Some((now, false));
        }
        let after = |wait: Duration| self.last_rebuild.map_or(now, |at| at + wait);
        let facts = (!self.facts.is_empty()).then(|| after(interval));
        let samples = (!self.samples.is_empty()).then(|| after(interval * SAMPLE_REBUILD_FACTOR));
        facts.into_iter().chain(samples).min().map(|at| (at, true))
    }

    fn has_first_time(&self) -> bool {
        self.facts
            .iter()
            .chain(&self.samples)
            .any(|session| !self.rebuilt.contains(session))
    }

    /// Takes every dirty session once a rebuild is [`due`](Self::due) at a
    /// flush.
    fn take_due(&mut self, now: Instant, interval: Duration) -> Option<BTreeSet<String>> {
        if !self.due(now, interval, true) {
            return None;
        }
        self.last_rebuild = Some(now);
        Some(self.take_all())
    }

    fn take_all(&mut self) -> BTreeSet<String> {
        let mut all = std::mem::take(&mut self.facts);
        all.append(&mut self.samples);
        self.rebuilt.extend(all.iter().cloned());
        all
    }
}

/// The resident pipeline's mutable state between passes.
struct Watch {
    state_dir: PathBuf,
    debug: bool,
    no_sidecars: bool,
    interval: f64,
    zone_flag: Option<String>,
    remote_region: EstimationRegion,
    /// How long a session may be quiet before its sampler is retired.
    /// A field rather than the bare [`SESSION_IDLE_TIMEOUT`] so a test can
    /// drive retirement without waiting ten minutes on a real clock.
    idle_timeout: Duration,
    debug_state: Option<Arc<Mutex<DebugState>>>,
    estimator: Option<EstimatorWorker>,
    sampler: Option<SamplerProcess>,
    sampler_retry: SamplerRetry,
    /// Every session this watch has seen an event for and not yet retired,
    /// with its sampler and respawn bookkeeping. A sampler is (re)spawned
    /// for each of them, not only for the sessions touched by the current
    /// pass: a dead sampler's own events are exactly what stops arriving
    /// when it dies, so "retry only sessions that produced events this
    /// pass" would never retry the one session that needs it.
    sessions: BTreeMap<String, SessionState>,
    /// The zone this watch resolved, cached across passes. Resolution
    /// scans every stored `session_meta` for a declared `geo_zone`, and the
    /// answer can only change when a *new* `session_meta` is ingested — so
    /// that, and nothing else, invalidates it.
    zone: Option<Zone>,
    /// The last `af python doctor` result, when it was taken, and the
    /// runtime fingerprint it was taken against. See [`PYTHON_HEALTH_TTL`].
    python_health: Option<PythonHealth>,
    /// Allocation traces already published, per session, so a re-run of
    /// the pipeline over the same session does not re-emit them. Per
    /// session and released when the session retires: a refresh walks the
    /// whole session, and a shared bounded set evicted, once a session
    /// outgrew it, exactly the ids the walk was about to look up — so every
    /// refresh re-published the entire history.
    published_allocs: BTreeMap<String, HashSet<String>>,
    /// Health rows of retired sessions, folded into per-collector totals so
    /// old activity stays visible without one row per session ever seen.
    retired_collectors: BTreeMap<String, RetiredCollector>,
    /// Decisions already emitted, keyed by the `event_id` they refer to.
    reported_events: BoundedSet,
    /// Whether the console's presence is what switches diagnostics on: the
    /// collector behind `af console`. Otherwise (`--debug`, tests) a debug
    /// state is watched for as long as it exists.
    on_demand: bool,
    /// How far back a backfill reaches; 0 is everything.
    console_window_ms: i64,
    /// Whether someone is looking at the debug state right now.
    watched: bool,
    /// Set while telling a console what it missed — at startup, on attach,
    /// for a session it asked for. Facts and traces older than
    /// [`debug_server::SNAPSHOT_WINDOW_MS`] are then passed over for good:
    /// a console is told the present, and reads the past from reports.
    backfill: bool,
    /// The most recent rejects, for the health payload — see
    /// [`REJECT_HISTORY`]. The health frame is republished every pass and
    /// carries this vector inline, so an unbounded one turns a spool full of
    /// malformed lines into a frame that grows without limit and is then
    /// serialized, ring-buffered and fanned out to every connection, once
    /// per pass, forever.
    rejects: VecDeque<Value>,
    /// How many rejects fell off the front of that window. Reported, so
    /// "100 rejects" is never mistaken for "only 100 rejects happened".
    rejects_dropped: u64,
    /// Every spool reject this process has seen.
    rejects_total: u64,
    /// Unknown event types preserved in `opaque_events`, cached so health
    /// publication does not query SQLite every pass.
    opaque_events: u64,
    /// Spool rejects per `collector:session_id`, parsed out of the
    /// rejecting file's name. The health payload used to report the
    /// process-wide total against *every* collector row, so one collector
    /// writing malformed lines made every other collector look equally
    /// broken.
    rejects_by_collector: BTreeMap<String, u64>,
    /// Last known spool path and consumed offset by collector/session. A full
    /// reconciliation replaces this map; dirty passes update only the paths
    /// they touched.
    spool_state: BTreeMap<(String, String), (PathBuf, u64)>,
    /// Collector health rows accumulated across passes, keyed
    /// `collector:session_id`. A pass only loads the sessions it touched,
    /// and a health payload rebuilt from that alone erased every other
    /// session's collectors — with several agents running, whichever
    /// session's pass ran last wiped the rest from the Health tab.
    health_rows: BTreeMap<String, Value>,
    /// Address + live counters of the OTLP receiver, when one is running.
    otlp: Option<(SocketAddr, Arc<af_otlp::Counters>)>,
    /// Sessions waiting for a derived-view rebuild — see [`DirtySessions`].
    dirty: DirtySessions,
    batch_interval: Duration,
    /// Estimates the worker finished, waiting for the next flush.
    estimates: Vec<af_store::ImpactEstimate>,
    /// Their sessions, rebuilt once the rows are stored.
    estimated_sessions: BTreeSet<String>,
    /// `llm_call`s stored by the flush in progress, for the estimator.
    calls_to_estimate: Vec<String>,
    /// See [`boot_id`].
    boot_id: String,
    started_at: SystemTime,
    /// Where the agents' user-scope configuration lives. `None` when no
    /// home directory can be found: agents are then reported from events
    /// alone.
    home: Option<PathBuf>,
    /// The last detection result — see [`Watch::refresh_agents`].
    agent_facts: Vec<AgentFacts>,
    /// What inspecting running agent processes found, by pid — kept so a
    /// settled process is not inspected again.
    inspections: BTreeMap<u32, agents::Inspection>,
    /// The Claude Code session each process runs, from the hook.
    claude_sessions: BTreeMap<u32, String>,
    /// What each agent has sent since this process started, recorded at
    /// receipt rather than at flush so the status is seconds behind the
    /// agent and not a flush interval behind it.
    activity: BTreeMap<AgentId, agents::Activity>,
    /// Whether anything the status is built from changed since it was last
    /// built, and when one was last sent — see [`Watch::publish_status`].
    status_dirty: bool,
    last_status_at: Option<Instant>,
}

struct LoadedSession {
    events: Vec<Envelope>,
    tree: SessionTree,
    apportionment: Apportionment,
    /// The rest is what only diagnostics render, and is left empty
    /// headless.
    traces: Vec<SampleTrace>,
    samples: Vec<EnergySample>,
    sample_ids: Vec<String>,
    procs: Vec<ProcessSample>,
}

impl LoadedSession {
    fn load(events: Vec<Envelope>, diagnostics: bool) -> Self {
        let tree = correlate(&events);
        let mut sample_ids = Vec::new();
        let mut samples = Vec::new();
        let mut procs = Vec::new();
        for event in &events {
            match &event.payload {
                Payload::EnergySample(sample) => {
                    if diagnostics {
                        sample_ids.push(event.event_id.clone());
                    }
                    samples.push(sample.clone());
                }
                Payload::ProcessSample(sample) => procs.push(sample.clone()),
                _ => {}
            }
        }
        if !diagnostics {
            return Self {
                apportionment: apportion(&samples, &procs, &tree),
                events,
                tree,
                traces: Vec::new(),
                samples: Vec::new(),
                sample_ids,
                procs: Vec::new(),
            };
        }
        let (apportionment, traces) = apportion_traced(&samples, &procs, &tree);
        Self {
            events,
            tree,
            apportionment,
            traces,
            samples,
            sample_ids,
            procs,
        }
    }
}

impl Watch {
    fn new(
        state_dir: &Path,
        args: &WatchArgs,
        debug_state: Option<Arc<Mutex<DebugState>>>,
        otlp: Option<(SocketAddr, Arc<af_otlp::Counters>)>,
    ) -> Self {
        let started_at = SystemTime::now();
        let watched = debug_state.is_some();
        let estimator = if args.no_sidecars {
            None
        } else {
            let spawned = estimator::spawn(state_dir);
            if let Some(note) = &spawned.note {
                eprintln!("af watch: {note}");
            }
            match spawned.sidecar {
                Some(sidecar) => match EstimatorWorker::spawn(state_dir, sidecar) {
                    Ok(worker) => Some(worker),
                    Err(error) => {
                        eprintln!("af watch: estimator worker unavailable ({error:#})");
                        None
                    }
                },
                None => None,
            }
        };

        Watch {
            state_dir: state_dir.to_path_buf(),
            debug: args.debug,
            no_sidecars: args.no_sidecars,
            interval: args.interval,
            zone_flag: args.local_grid_zone.clone(),
            remote_region: super::resolve_remote_region(args.remote_region.as_deref()),
            idle_timeout: SESSION_IDLE_TIMEOUT,
            debug_state,
            estimator,
            sampler: None,
            sampler_retry: SamplerRetry::default(),
            sessions: BTreeMap::new(),
            zone: None,
            python_health: None,
            published_allocs: BTreeMap::new(),
            retired_collectors: BTreeMap::new(),
            reported_events: BoundedSet::new(DEDUP_HORIZON),
            on_demand: false,
            console_window_ms: args.console_window_ms,
            watched,
            backfill: false,
            rejects: VecDeque::new(),
            rejects_dropped: 0,
            rejects_total: 0,
            opaque_events: 0,
            rejects_by_collector: BTreeMap::new(),
            spool_state: BTreeMap::new(),
            health_rows: BTreeMap::new(),
            otlp,
            dirty: DirtySessions::default(),
            batch_interval: args.batch_interval,
            estimates: Vec::new(),
            estimated_sessions: BTreeSet::new(),
            calls_to_estimate: Vec::new(),
            boot_id: boot_id(started_at, std::process::id()),
            started_at,
            home: crate::paths::home_dir(),
            agent_facts: Vec::new(),
            inspections: BTreeMap::new(),
            claude_sessions: BTreeMap::new(),
            activity: BTreeMap::new(),
            status_dirty: true,
            last_status_at: None,
        }
    }

    // --- watcher status (`GET /debug/status`) -------------------------------

    /// The logs endpoint agents must export to for this watcher to hear
    /// them: its own receiver, or the default when it has none.
    fn logs_endpoint(&self) -> String {
        match &self.otlp {
            Some((addr, _)) => format!("http://{addr}/v1/logs"),
            None => super::DEFAULT_LOGS_ENDPOINT.to_string(),
        }
    }

    /// Re-reads what the machine says about each agent. Only when someone
    /// can see it: without the debug server this is a `ps` every thirty
    /// seconds for nobody.
    fn refresh_agents(&mut self) {
        if self.debug_state.is_none() || !self.diagnostics() {
            return;
        }
        if let Some(home) = &self.home {
            let mut facts = agents::detect_live(
                home,
                &self.logs_endpoint(),
                &mut self.inspections,
                &self.claude_sessions,
            );
            agents::keep_start_times(&mut facts, &self.agent_facts);
            // A link outlives its process only until the next listing: a
            // reused pid must not inherit a dead session.
            self.claude_sessions.retain(|pid, _| {
                facts
                    .iter()
                    .flat_map(|facts| facts.processes.iter().flatten())
                    .any(|process| process.pid == *pid)
            });
            self.agent_facts = facts;
        }
        self.status_dirty = true;
    }

    /// Records one received event for agent status. A collector that
    /// belongs to no agent (the sampler, a generic OTLP source) is ignored.
    fn note_activity(&mut self, collector: &str, session_id: &str, at_ms: i64) {
        let Some(agent) = agents::agent_for_collector(collector) else {
            return;
        };
        let activity = self.activity.entry(agent).or_default();
        let heard = activity
            .sessions
            .entry(session_id.to_string())
            .or_insert(at_ms);
        *heard = (*heard).max(at_ms);
        activity.last_event_at_ms =
            Some(activity.last_event_at_ms.map_or(at_ms, |at| at.max(at_ms)));
        self.status_dirty = true;
    }

    /// The hook's bootstrap span names the process a Claude Code session
    /// runs in: remembered so that process's row can speak for itself.
    fn note_session_roots(&mut self, session_id: &str, root_pids: &[i64]) {
        for pid in root_pids.iter().filter_map(|pid| u32::try_from(*pid).ok()) {
            if self.claude_sessions.get(&pid).map(String::as_str) != Some(session_id) {
                self.claude_sessions.insert(pid, session_id.to_string());
                self.status_dirty = true;
            }
        }
    }

    /// Activity from telemetry the receiver just accepted.
    fn note_inbound(&mut self, item: &af_otlp::Inbound, at_ms: i64) {
        if let af_otlp::Inbound::Facts(events) = item {
            for event in events {
                self.note_activity(&event.collector.name, &event.session_id, at_ms);
            }
        }
    }

    /// Activity from spool files that just changed: the file name carries
    /// the collector and the session, so nothing has to be read.
    fn note_spool_paths(&mut self, paths: &[PathBuf], at_ms: i64) {
        for file in paths.iter().filter_map(|path| spool_file_of(path)) {
            self.note_activity(&file.collector, &file.session_id, at_ms);
        }
    }

    /// The `/debug/status` payload: this process, its receiver, and one
    /// row per supported agent.
    fn status_payload(&self) -> Value {
        let receiver = self.otlp_health();
        let facts_for = |agent: AgentId| {
            let mut facts = self
                .agent_facts
                .iter()
                .find(|facts| facts.agent == agent)
                .cloned()
                // No detection (no home directory): only events can speak.
                .unwrap_or_else(|| {
                    agents::facts_from(agent, false, agents::UserConfig::Absent, None, None)
                });
            // A link learned since the last detection counts at once.
            if agent == AgentId::ClaudeCode {
                for process in facts.processes.iter_mut().flatten() {
                    if let Some(session) = self.claude_sessions.get(&process.pid) {
                        process.session_ids = vec![session.clone()];
                    }
                }
            }
            facts
        };
        let home = self.home.as_deref().unwrap_or(Path::new(""));
        json!({
            "boot_id": self.boot_id,
            "started_at": agents::system_time_rfc3339(self.started_at),
            "af_version": env!("CARGO_PKG_VERSION"),
            "flush_interval_ms": self.batch_interval.as_millis() as u64,
            "receiver": {
                "endpoint": receiver["endpoint"],
                "note": receiver["note"],
            },
            "agents": AgentId::all()
                .into_iter()
                .map(|agent| {
                    agents::agent_row_in(&facts_for(agent), self.activity.get(&agent), home)
                })
                .collect::<Vec<_>>(),
        })
    }

    /// Publishes the status when something that feeds it changed, it
    /// differs from the one last published, and [`STATUS_MIN_INTERVAL`] has
    /// passed since (or `force`).
    fn publish_status(&mut self, now: Instant, force: bool) {
        let Some(state) = &self.debug_state else {
            return;
        };
        if !self.status_dirty && !force {
            return;
        }
        let too_soon = self
            .last_status_at
            .is_some_and(|at| now.saturating_duration_since(at) < STATUS_MIN_INTERVAL);
        if too_soon && !force {
            return;
        }
        let payload = self.status_payload();
        self.status_dirty = false;
        if state
            .lock()
            .is_ok_and(|mut state| state.set_status(payload))
        {
            self.last_status_at = Some(now);
        }
    }

    /// One wake's work: ingest, supervise, rebuild, publish.
    fn pass(&mut self, store: &mut Store, kind: PassKind) -> Result<()> {
        if matches!(kind, PassKind::Supervise) {
            self.supervise(&BTreeSet::new());
            return Ok(());
        }
        let mut summary = match kind {
            PassKind::FullReconcile => ingest(store, &self.state_dir)?,
            PassKind::Dirty(paths) => ingest_paths(store, &self.state_dir, &paths)?,
            PassKind::Supervise => unreachable!(),
        };
        self.update_spool_state(
            summary.metrics.full_scan,
            std::mem::take(&mut summary.file_states),
        );
        self.opaque_events = self.opaque_events.saturating_add(summary.opaque as u64);
        self.publish_ingest_metrics(&summary.metrics);
        self.after_ingest(store, &summary.events, &summary.rejects)
    }

    /// One flush, one commit: received telemetry and the estimator's
    /// finished rows (when `flushing`), a spool pass, and the rebuilds those
    /// make due. Each step's failure is its own and is logged; only a failed
    /// commit loses the lot, and then the telemetry and estimates are kept
    /// for a retry and the rebuilt sessions marked dirty again.
    fn write_flush(
        &mut self,
        store: &mut Store,
        inbox: Inbox,
        flushing: bool,
        pass_kind: Option<PassKind>,
    ) -> FlushOutcome {
        let absorb = flushing && !inbox.is_empty();
        let mut rebuilt = BTreeSet::new();
        let mut absorbed = false;
        let mut estimates_written = false;
        let mut reconcile = false;
        let committed = store.in_transaction(|store| -> Result<()> {
            if absorb {
                match self.absorb_inbox(store, &inbox) {
                    Ok(()) => absorbed = true,
                    Err(err) => eprintln!(
                        "af watch: storing received telemetry failed: {err:#}; will retry"
                    ),
                }
            }
            if flushing {
                match self.store_estimates(store) {
                    Ok(()) => estimates_written = true,
                    Err(err) => {
                        eprintln!("af watch: storing estimates failed: {err:#}; will retry")
                    }
                }
            }
            if let Some(pass_kind) = pass_kind {
                if let Err(err) = self.pass(store, pass_kind) {
                    // A failed pass is not a failed watch: the next one may
                    // well succeed (a half-written file, a locked db), and
                    // exiting would take the OTLP receiver down with it.
                    eprintln!("af watch: pass failed: {err:#}");
                    reconcile = true;
                }
            }
            match self.rebuild_due(store) {
                Ok(sessions) => rebuilt = sessions,
                Err(err) => eprintln!("af watch: rebuild failed: {err:#}"),
            }
            Ok(())
        });
        match committed {
            Ok(()) => {
                if estimates_written {
                    self.estimates_stored();
                }
                self.request_estimates();
                FlushOutcome {
                    unwritten: if absorbed {
                        Inbox::default()
                    } else {
                        inbox.capped(MAX_PENDING_FACTS)
                    },
                    reconcile,
                }
            }
            Err(err) => {
                eprintln!("af watch: writing this flush failed: {err:#}; will retry");
                self.dirty.mark(&rebuilt, &rebuilt);
                // Rolled back: they are asked for again once re-ingested.
                self.calls_to_estimate.clear();
                FlushOutcome {
                    unwritten: inbox.capped(MAX_PENDING_FACTS),
                    reconcile: true,
                }
            }
        }
    }

    /// Stores what the receiver sent and runs the same post-ingest steps a
    /// spool pass runs.
    ///
    /// Facts first, in one transaction. The receiver already told the
    /// exporter they were accepted, so when the store refuses them (locked
    /// past its busy timeout, disk full) the error is returned and the
    /// caller, which still holds the inbox, retries it rather than dropping
    /// it. Rejects are best effort: a broken reject log never costs the
    /// facts.
    fn absorb_inbox(&mut self, store: &mut Store, inbox: &Inbox) -> Result<()> {
        store.insert_events(&inbox.facts)?;
        let rows: Vec<af_store::RejectRow> = inbox
            .rejects
            .iter()
            .map(|reject| af_store::RejectRow {
                ts: af_core::rfc3339_ms(reject.ts_ms).unwrap_or_default(),
                source: reject.source.clone(),
                reason: reject.reason.clone(),
                attribute_keys: reject.attribute_keys.clone(),
                bytes: reject.bytes,
            })
            .collect();
        if let Err(err) = store.insert_rejects(&rows) {
            eprintln!("af watch: recording {} reject(s) failed: {err}", rows.len());
        }
        for row in &rows {
            let frame = otlp_reject_frame(row);
            self.rejects.push_back(frame.clone());
            while self.rejects.len() > REJECT_HISTORY {
                self.rejects.pop_front();
                self.rejects_dropped = self.rejects_dropped.saturating_add(1);
            }
            self.publish("reject", frame);
        }
        if inbox.facts.is_empty() {
            return Ok(());
        }
        // The facts are stored; a failure past this point is the pass's
        // own and must not resend them.
        if let Err(err) = self.after_ingest(store, &inbox.facts, &[]) {
            eprintln!("af watch: processing received telemetry failed: {err:#}");
        }
        Ok(())
    }

    /// Everything a pass does once events are in the store: publish them
    /// and the spool's `rejects`, supervise samplers, mark sessions for
    /// rebuild.
    fn after_ingest(
        &mut self,
        store: &mut Store,
        events: &[Envelope],
        rejects: &[RejectRecord],
    ) -> Result<()> {
        let mut touched: BTreeSet<String> = BTreeSet::new();
        let mut active: BTreeSet<String> = BTreeSet::new();
        let mut zone_may_have_changed = false;
        for event in events {
            touched.insert(event.session_id.clone());
            if event.collector.name != "codecarbon" {
                active.insert(event.session_id.clone());
            }
            // The only input to zone resolution that a pass can change.
            zone_may_have_changed |= matches!(event.payload, Payload::SessionMeta(_));
            // Decisions and fact frames are for someone watching; headless,
            // they would be built only to be dropped.
            if self.diagnostics() {
                self.publish_fact(event);
            }
        }

        for record in rejects {
            let frame = reject_frame(record);
            if self.debug {
                eprintln!(
                    "af watch: rejected {}:{} ({}) — {}",
                    record.origin, record.line, record.byte_offset, record.reason
                );
            }
            self.rejects_total = self.rejects_total.saturating_add(1);
            if let Some(key) = collector_key(&record.origin) {
                let count = self.rejects_by_collector.entry(key).or_insert(0);
                *count = count.saturating_add(1);
            }
            self.rejects.push_back(frame.clone());
            while self.rejects.len() > REJECT_HISTORY {
                self.rejects.pop_front();
                self.rejects_dropped = self.rejects_dropped.saturating_add(1);
            }
            self.publish("reject", frame);
        }

        self.supervise(&active);

        if events.is_empty() {
            if !rejects.is_empty() {
                let sessions = rejects
                    .iter()
                    .filter_map(|record| {
                        af_spool::parse_spool_filename(&record.origin, PathBuf::new())
                            .map(|file| file.session_id)
                    })
                    .collect::<BTreeSet<_>>();
                let mut loaded = BTreeMap::new();
                for session in sessions {
                    loaded.insert(session.clone(), store.events_for_session(&session)?);
                }
                self.publish_health(
                    loaded
                        .iter()
                        .map(|(session_id, events)| (session_id.as_str(), events.as_slice())),
                );
            }
            return Ok(());
        }

        let previous_zone = self.zone.clone();
        if zone_may_have_changed {
            self.zone = None;
        }
        let zone = match &self.zone {
            Some(zone) => zone.clone(),
            None => {
                let (id, source) = super::resolve_zone(store, self.zone_flag.as_deref())?;
                let resolved = Zone::unresolved(id, source);
                self.zone = Some(resolved.clone());
                resolved
            }
        };
        // Told once the write commits ([`Watch::request_estimates`]): the
        // worker reads through its own connection, which cannot see these
        // events until then.
        self.calls_to_estimate.extend(
            events
                .iter()
                .filter(|event| matches!(event.payload, Payload::LlmCall(_)))
                .map(|event| event.event_id.clone()),
        );

        // Ordinarily: only the sessions that just produced events. Their
        // inputs are the only ones that changed, and rebuilding the rest
        // writes back byte-identical rows — for every session accumulated
        // over a working day, once every couple of seconds.
        //
        // Two passes are not like that and take the whole store. The first
        // one to resolve a zone, because the database may hold rows an
        // earlier run left under a different one; and any pass where the
        // zone *moved*, because the zone governs the local half of every
        // stored join and not only the touched sessions'.
        let scope = match &previous_zone {
            Some(previous) if previous.id == zone.id => Some(&touched),
            _ => None,
        };

        let rebuild_sessions = scope.cloned().unwrap_or_else(|| {
            store
                .session_summaries()
                .map(|summaries| {
                    summaries
                        .into_iter()
                        .map(|summary| summary.session_id)
                        .collect()
                })
                .unwrap_or_default()
        });
        // Not rebuilt here: marked, and rebuilt by `rebuild_due` at most once
        // per interval. A full-store scope (the zone moved) counts as facts.
        let facts = if scope.is_some() {
            active.clone()
        } else {
            rebuild_sessions.clone()
        };
        self.dirty.mark(&rebuild_sessions, &facts);
        Ok(())
    }

    /// Whether periodic supervision has anything to look after: a session
    /// to retire, a sampler to reap or respawn.
    fn has_supervision(&self) -> bool {
        !self.sessions.is_empty() || self.sampler.is_some()
    }

    /// Whether `facts` name a session this watch has not seen yet.
    fn has_new_session(&self, facts: &[Envelope]) -> bool {
        facts
            .iter()
            .any(|event| !self.sessions.contains_key(&event.session_id))
    }

    /// Whether `paths` include a spool file no pass has read yet.
    ///
    /// Looked up by the `(collector, session)` its name parses to, the key
    /// [`Watch::spool_state`] is filed under, rather than by scanning every
    /// known path; a name that does not parse was never read.
    fn has_unknown_spool_file(&self, paths: &BTreeSet<PathBuf>) -> bool {
        paths.iter().any(|path| {
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return false;
            }
            let Some(file) = spool_file_of(path) else {
                return true;
            };
            self.spool_state
                .get(&(file.collector, file.session_id))
                .is_none_or(|(known, _)| known != path)
        })
    }

    /// Rebuilds whatever [`DirtySessions`] says is due. Called once per loop.
    /// Returns the sessions it rebuilt, so a flush whose commit fails can
    /// mark them dirty again.
    fn rebuild_due(&mut self, store: &mut Store) -> Result<BTreeSet<String>> {
        match self.dirty.take_due(Instant::now(), self.batch_interval) {
            Some(sessions) => {
                if let Err(err) = self.rebuild_sessions(store, &sessions) {
                    self.dirty.mark(&sessions, &sessions);
                    return Err(err);
                }
                Ok(sessions)
            }
            None => Ok(BTreeSet::new()),
        }
    }

    /// Rebuilds every still-dirty session, for shutdown.
    fn rebuild_all_dirty(&mut self, store: &mut Store) -> Result<()> {
        let sessions = self.dirty.take_all();
        self.rebuild_sessions(store, &sessions)
    }

    fn rebuild_sessions(&mut self, store: &mut Store, touched: &BTreeSet<String>) -> Result<()> {
        if touched.is_empty() {
            return Ok(());
        }
        let zone = self
            .zone
            .clone()
            .unwrap_or_else(|| Zone::unresolved("WOR", "default"));
        let mut loaded: BTreeMap<String, LoadedSession> = BTreeMap::new();
        for session in touched {
            loaded.insert(
                session.clone(),
                LoadedSession::load(store.events_for_session(session)?, self.diagnostics()),
            );
        }
        let prepared = loaded
            .iter()
            .map(|(session_id, loaded)| {
                (
                    session_id.clone(),
                    PreparedSession {
                        events: &loaded.events,
                        tree: &loaded.tree,
                        apportionment: &loaded.apportionment,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let outcome = rebuild_prepared_stored(store, zone, &self.remote_region, &prepared)?;
        for (session_id, loaded) in &loaded {
            self.refresh_session(store, session_id, loaded, &outcome.zone)?;
        }

        self.publish_health(
            loaded
                .iter()
                .map(|(session_id, loaded)| (session_id.as_str(), loaded.events.as_slice())),
        );
        Ok(())
    }

    /// Collects what the estimator finished: its rows wait for the next
    /// flush ([`Watch::store_estimates`]); a zone it resolved makes every
    /// session's local half stale.
    fn take_estimator_completion(&mut self, store: &Store) {
        let Some(estimator) = &self.estimator else {
            return;
        };
        let Some(completion) = estimator.take_completion() else {
            return;
        };
        if let Some(error) = completion.error {
            eprintln!("af watch: estimator worker degraded ({error})");
        }
        if self.debug && !completion.rows.is_empty() {
            eprintln!(
                "af watch: estimator completed {} call(s), {} still pending",
                completion.rows.len(),
                completion.pending
            );
        }
        self.estimates.extend(completion.rows);
        self.estimated_sessions.extend(completion.sessions);
        if let Some(zone) = completion.zone {
            self.zone = Some(zone);
            let all: BTreeSet<String> = store
                .session_summaries()
                .unwrap_or_default()
                .into_iter()
                .map(|summary| summary.session_id)
                .collect();
            self.dirty.mark(&all, &all);
        }
    }

    /// Hands the estimator the calls stored since the last commit, under
    /// the zone this watch resolved. Called after a commit.
    fn request_estimates(&mut self) {
        let calls = std::mem::take(&mut self.calls_to_estimate);
        let (Some(estimator), Some(zone)) = (&self.estimator, &self.zone) else {
            return;
        };
        estimator.request(&zone.id, &zone.source, &self.remote_region, calls);
    }

    fn has_estimates_to_store(&self) -> bool {
        !self.estimates.is_empty()
    }

    /// Writes the estimator's finished rows and marks their sessions for a
    /// rebuild. The rows are kept until [`Watch::estimates_stored`]: the
    /// enclosing commit may still fail.
    fn store_estimates(&mut self, store: &mut Store) -> Result<()> {
        if self.estimates.is_empty() {
            return Ok(());
        }
        store.upsert_estimates(&self.estimates)?;
        let sessions = self.estimated_sessions.clone();
        self.dirty.mark(&sessions, &sessions);
        Ok(())
    }

    /// The flush that wrote [`Watch::store_estimates`]'s rows committed.
    fn estimates_stored(&mut self) {
        if let Some(estimator) = &self.estimator {
            estimator.stored(self.estimates.iter().map(|row| row.event_id.clone()));
        }
        self.estimates.clear();
        self.estimated_sessions.clear();
    }

    /// Records where a pass left each spool file; a full scan's states
    /// replace everything known.
    fn update_spool_state(&mut self, full_scan: bool, file_states: Vec<IngestFileState>) {
        if full_scan {
            self.spool_state.clear();
        }
        for IngestFileState {
            collector,
            session_id,
            path,
            offset,
        } in file_states
        {
            self.spool_state
                .insert((collector, session_id), (path, offset));
        }
    }

    fn supervise(&mut self, touched: &BTreeSet<String>) {
        let now = Instant::now();
        for session in touched {
            self.sessions
                .entry(session.clone())
                .and_modify(|state| state.last_event_at = now)
                .or_insert_with(|| SessionState::seen(now));
        }
        self.retire_idle_sessions(now);
        if !self.no_sidecars {
            self.reap_dead_sampler();
            self.ensure_sampler();
        }
    }

    fn publish_ingest_metrics(&mut self, metrics: &IngestMetrics) {
        if !self.debug {
            return;
        }
        eprintln!(
            "af watch: ingest mode={} dirty_paths={} files={} observed_spool_bytes={} opened={} bytes_read={} lines={} partial={} parsed={} inserted={} opaque_parsed={} opaque_inserted={} dedup={} offset_reads={} offset_writes={} skipped_offsets={} empty_batches={} discover_ms={:.3} tail_ms={:.3} validate_ms={:.3} insert_ms={:.3} offset_ms={:.3} total_ms={:.3}",
            if metrics.full_scan { "reconcile" } else { "dirty" },
            metrics.dirty_paths,
            metrics.spool_files_total,
            metrics.spool_bytes_total,
            metrics.files_opened,
            metrics.bytes_read,
            metrics.complete_lines,
            metrics.partial_lines,
            metrics.events_parsed,
            metrics.events_inserted,
            metrics.opaque_events_parsed,
            metrics.opaque_events_inserted,
            metrics.events_deduplicated,
            metrics.offset_reads,
            metrics.offset_writes,
            metrics.unchanged_offset_writes_skipped,
            metrics.empty_insert_batches,
            metrics.discovery_duration.as_secs_f64() * 1_000.0,
            metrics.tail_duration.as_secs_f64() * 1_000.0,
            metrics.validation_duration.as_secs_f64() * 1_000.0,
            metrics.insert_duration.as_secs_f64() * 1_000.0,
            metrics.offset_duration.as_secs_f64() * 1_000.0,
            metrics.total_duration.as_secs_f64() * 1_000.0,
        );
    }

    /// Recomputes the attribution view for one session and publishes
    /// everything derived from it.
    ///
    /// The whole session is recomputed each pass rather than incrementally
    /// updated: an energy sample's allocation depends on process samples
    /// and spans that may only arrive later, so an incremental view would
    /// be a different (and wrong) number than `af report`'s. Sessions are
    /// bounded by a working day; correctness beats the arithmetic saved.
    fn refresh_session(
        &mut self,
        store: &Store,
        session_id: &str,
        loaded: &LoadedSession,
        zone: &af_core::Zone,
    ) -> Result<()> {
        self.send_watch_op(session_id, &loaded.tree);
        self.note_session_roots(session_id, &loaded.tree.root_pids);
        if !self.diagnostics() {
            return Ok(());
        }
        if self.backfill {
            // What a console attaching now did not see go by.
            for event in &loaded.events {
                self.publish_fact(event);
            }
        }
        let root_pid = loaded.tree.root_pids.first().copied();
        for trace in &loaded.traces {
            let Some(sample_event_id) = loaded.sample_ids.get(trace.sample_index) else {
                continue;
            };
            if !self
                .published_allocs
                .entry(session_id.to_string())
                .or_default()
                .insert(sample_event_id.clone())
            {
                continue;
            }
            // Remembered above either way: an old trace is passed over, not
            // postponed to the session's next refresh.
            if self.backfill && !self.recent(&loaded.samples[trace.sample_index].t_end) {
                continue;
            }
            self.decide(&attr_decision(trace));
            if trace.orphaned_j > 0.0 {
                self.decide(&orphan_decision(trace));
            }
            self.publish(
                "alloc",
                alloc_trace(
                    sample_event_id,
                    session_id,
                    &loaded.samples[trace.sample_index],
                    trace,
                    root_pid,
                ),
            );
        }

        if let Some(latest) = loaded.procs.last() {
            self.publish(
                "watchdog",
                json!({ "pids": watchdog_entries(latest, &loaded.tree) }),
            );
        }

        self.publish_session(&loaded.events, &loaded.apportionment, zone, session_id);
        self.publish_report(store, &loaded.events, session_id);
        Ok(())
    }

    /// Sends the session's root pid tree to its sampler, once.
    ///
    /// **v1 watch-list = the root tree only.** The Claude Code hook
    /// collector emits a span when it *closes*, so by the time the control
    /// plane learns a span existed, watching its pids would measure
    /// nothing. It also names no pids for tool-call spans at all — only the
    /// agent process, on the bootstrap span. Since attribution already
    /// inherits the root tree for pid-less spans, watching that one tree covers every span the
    /// collector can produce. Per-span `watch`/`unwatch` ops stay
    /// implemented in the sampler, unused here, waiting for a collector
    /// that can report a tool's pids while the tool is still running.
    fn send_watch_op(&mut self, session_id: &str, tree: &SessionTree) {
        if tree.root_pids.is_empty() {
            return;
        }
        let Some(state) = self.sessions.get(session_id) else {
            return;
        };
        if state.watch_sent {
            return;
        }
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };
        let span_id = tree
            .spans
            .iter()
            .find(|span| span.tool_name == af_core::BOOTSTRAP_TOOL_NAME)
            .map(|span| span.span_id.clone())
            .unwrap_or_else(|| format!("session-boot-{session_id}"));

        let op = json!({"op": "watch", "session_id": session_id, "span_id": span_id, "pids": tree.root_pids});
        match sampler.sidecar.send(&op) {
            Ok(()) => {
                if let Some(state) = self.sessions.get_mut(session_id) {
                    state.watch_sent = true;
                }
                if self.debug {
                    eprintln!(
                        "af watch: sampler[{session_id}] watch {:?} (root tree)",
                        tree.root_pids
                    );
                }
            }
            Err(err) => {
                // The sidecar's stdin is gone: it died. Everything it would
                // have measured from here is unmeasured, which is a
                // reported coverage gap, never an implied zero.
                eprintln!("af watch: sampler[{session_id}] watch op failed: {err:#}");
                self.sampler = None;
                self.note_spawn_failure();
                self.publish(
                    "gap",
                    json!({
                        "t_start": super::now_rfc3339(),
                        "t_end": super::now_rfc3339(),
                        "reason": "codecarbon sampler stopped accepting control ops",
                        "collector": "codecarbon-sampler",
                    }),
                );
            }
        }
    }

    /// Retires sessions that have produced nothing for
    /// [`SESSION_IDLE_TIMEOUT`], stopping their samplers.
    ///
    /// A session with no sampler is not "ended" — it is simply not being
    /// supervised. The next event for it goes through the ordinary
    /// [`Watch::sessions`] → `ensure_sampler` path and spawns a fresh one,
    /// which is why the retry bookkeeping is dropped too (it goes with the
    /// record): a session coming back deserves an immediate attempt, not
    /// the backoff its last crash earned.
    fn retire_idle_sessions(&mut self, now: Instant) {
        let idle = idle_sessions(&self.sessions, now, self.idle_timeout);
        for session in idle {
            if let Some(sampler) = self.sampler.as_mut() {
                if let Err(err) = sampler
                    .sidecar
                    .send(&json!({"op": "remove_session", "session_id": session}))
                {
                    eprintln!("af watch: sampler session removal failed: {err:#}");
                }
            }
            if self.debug {
                eprintln!(
                    "af watch: session {session} idle for {}s; removed from sampler (a later event re-adds it)",
                    self.idle_timeout.as_secs()
                );
            }
            self.sessions.remove(&session);
            self.forget_session(&session);
        }
        if self.sessions.is_empty() {
            if let Some(sampler) = self.sampler.as_mut() {
                let _ = sampler.sidecar.send(&json!({"op": "shutdown"}));
            }
            self.sampler = None;
        }
    }

    /// Releases what this watch kept about a retired session: its
    /// published-trace memory, its spool position, its reject counts, and
    /// its health rows, which are folded into per-collector totals. A later
    /// event for it starts over as a new session would.
    fn forget_session(&mut self, session: &str) {
        self.published_allocs.remove(session);
        self.dirty.rebuilt.remove(session);
        self.spool_state.retain(|(_, id), _| id != session);
        let suffix = format!(":{session}");
        self.rejects_by_collector
            .retain(|key, _| !key.ends_with(&suffix));
        let retired: Vec<String> = self
            .health_rows
            .keys()
            .filter(|key| key.ends_with(&suffix))
            .cloned()
            .collect();
        for key in retired {
            let Some(row) = self.health_rows.remove(&key) else {
                continue;
            };
            let name = row["name"].as_str().unwrap_or_default().to_string();
            let totals = self.retired_collectors.entry(name).or_default();
            totals.sessions += 1;
            totals.events += row["events"].as_u64().unwrap_or(0);
            totals.rejected += row["rejected"].as_u64().unwrap_or(0);
        }
    }

    /// Checks every sampler for liveness and publishes one coverage gap per
    /// death.
    ///
    /// **`try_wait`, not "did a write fail".** A sampler killed by `SIGKILL`
    /// leaves a stdin pipe whose next write still succeeds, so the send path
    /// reports health for a process that is already gone — and the `watch`
    /// op, the one write this loop performs, is sent *once per session* and
    /// then never again, so a sampler that dies after it can never be
    /// noticed by writing at all. Every pass asks the OS instead. That
    /// closes the remaining half of gap #8: a `SIGKILL`ed sampler is now a
    /// reported gap rather than an unreported silence.
    ///
    /// The gap is published once, at the moment of death, because the
    /// sampler is removed from the map in the same step — a gap frame per
    /// pass would turn one lost window into a stream of duplicate claims
    /// about the same lost window.
    fn reap_dead_sampler(&mut self) {
        let Some(sampler) = self.sampler.as_mut() else {
            return;
        };
        let exit = match sampler.sidecar.try_wait() {
            Ok(None) => return,
            Ok(Some(status)) => status.to_string(),
            // The check itself failed (the child was reaped elsewhere,
            // say). An unanswerable liveness question is not evidence of
            // life, and treating it as one is how a dead collector keeps
            // being credited with silence.
            Err(err) => format!("liveness check failed: {err}"),
        };
        let lifetime = sampler.spawned_at.elapsed();
        self.sampler = None;
        for state in self.sessions.values_mut() {
            state.watch_sent = false;
        }

        let retry = &mut self.sampler_retry;
        retry.failures = if lifetime >= RESPAWN_HEALTHY_AFTER {
            1
        } else {
            retry.failures.saturating_add(1)
        };
        let delay = respawn_delay(retry.failures);
        retry.next_attempt = Some(Instant::now() + delay);

        eprintln!(
                "af watch: shared sampler exited ({exit}) after {:?}; coverage gap published, respawn in {:?}",
                lifetime, delay
            );
        self.publish(
            "gap",
            json!({
                "t_start": super::now_rfc3339(),
                "t_end": super::now_rfc3339(),
                "reason": format!("codecarbon sampler exited ({exit})"),
                "collector": "codecarbon-sampler",
            }),
        );
    }

    /// Spawns the codecarbon sampler for a session that has none, subject to
    /// the respawn backoff. Failure is reported, counted, and retried on a
    /// widening schedule — never in a tight loop, and never silently.
    fn ensure_sampler(&mut self) {
        let Some(first_session) = self.sessions.keys().next().cloned() else {
            return;
        };
        if self.sampler.is_some() {
            return;
        }
        if self
            .sampler_retry
            .next_attempt
            .is_some_and(|at| Instant::now() < at)
        {
            return;
        }
        let Some(python) = venv_python(&self.state_dir) else {
            // Silence here used to be the whole report: a user with no venv
            // got a watch that measured no energy and never said why.
            let state_dir = self.state_dir.display().to_string();
            if !self.sampler_retry.venv_noted {
                self.sampler_retry.venv_noted = true;
                eprintln!(
                        "af watch: no managed venv under {state_dir} (run `af python setup`); active sessions will have no locally measured energy"
                    );
            }
            return;
        };
        let Some(script) = sidecar_script(&self.state_dir, SAMPLER_SCRIPT) else {
            eprintln!("af watch: af_sampler script not found; no local energy will be measured");
            self.note_spawn_failure();
            return;
        };
        let Some(script) = script.to_str() else {
            self.note_spawn_failure();
            return;
        };

        let interval = format!("{}", self.interval);
        let state_dir = self.state_dir.display().to_string();
        // One argument, so an id starting with `-` (session ids reach us
        // from the network) cannot be read as a flag.
        let session_arg = format!("--session={first_session}");
        let args = [
            "--state-dir",
            state_dir.as_str(),
            session_arg.as_str(),
            "--interval",
            interval.as_str(),
        ];
        match Sidecar::spawn(&python, script, &args) {
            Ok(mut sidecar) => {
                sidecar.set_timeout(SAMPLER_READY_TIMEOUT);
                let readiness = sidecar.request(&json!({"op": "ping"}));
                if let Err(err) = readiness {
                    let message = format!("sampler readiness check failed: {err:#}");
                    eprintln!("af watch: shared sampler is not ready: {err:#}");
                    self.sampler_retry.last_error = Some(message);
                    self.note_spawn_failure();
                    return;
                }
                eprintln!(
                    "af watch: shared codecarbon sampler ready (pid {}, {}s windows)",
                    sidecar.pid(),
                    self.interval
                );
                self.sampler = Some(SamplerProcess {
                    sidecar,
                    spawned_at: Instant::now(),
                });
                // The attempt succeeded; only the schedule is cleared.
                // The failure count stays, so a process that spawns and
                // dies over and over still backs off — "it started" is
                // not evidence that it ran.
                self.sampler_retry.next_attempt = None;
                self.sampler_retry.last_error = None;
                for state in self.sessions.values_mut() {
                    state.watch_sent = false;
                }
            }
            Err(err) => {
                eprintln!("af watch: shared sampler failed to spawn: {err:#}");
                self.sampler_retry.last_error = Some(format!("spawn failed: {err:#}"));
                self.note_spawn_failure();
            }
        }
    }

    /// Counts one failed spawn attempt and pushes the next one out.
    fn note_spawn_failure(&mut self) {
        self.sampler_retry.failures = self.sampler_retry.failures.saturating_add(1);
        self.sampler_retry.next_attempt =
            Some(Instant::now() + respawn_delay(self.sampler_retry.failures));
    }

    /// DATA-CONTRACT §2.1.
    fn publish_session(
        &mut self,
        events: &[Envelope],
        apportionment: &af_core::Apportionment,
        zone: &af_core::Zone,
        session_id: &str,
    ) {
        if self.debug_state.is_none() || !self.diagnostics() {
            return;
        }
        let session_meta = events
            .iter()
            .find_map(|event| match &event.payload {
                Payload::SessionMeta(meta) => serde_json::to_value(meta).ok(),
                _ => None,
            })
            .unwrap_or(Value::Null);
        let t_start = events
            .first()
            .map(|event| event.ts.clone())
            .unwrap_or_default();
        // Latest event ts, not "now": ordering sessions by wall-clock
        // activity must reflect what the spool recorded, and a session
        // whose pass merely re-ran must not leapfrog one that actually
        // produced events.
        let t_last = events
            .iter()
            .map(|event| event.ts.as_str())
            .max()
            .unwrap_or_default()
            .to_string();
        // The policy the join *actually applied*, not a guess from the
        // presence of spans. Spans existing says nothing about whether any
        // energy sample was ever divided: a session with spans but no
        // process data is L1, and one with spans and no energy samples at
        // all was apportioned by nothing. `none` is the honest answer for
        // the last case — claiming L2 there described a computation that
        // never ran.
        let applied = apportionment.applied_policy();

        // `null` rather than a plausible number: without the estimator
        // sidecar there is no electricity-mix factor, and a defaulted grid
        // intensity is exactly the kind of invented figure the project
        // forbids. The `source` field says which case this is.
        let (g_co2e, grid_source) = match &zone.factors {
            Some(factors) => (
                json!((factors.gwp_min + factors.gwp_max) / 2.0 * 1000.0),
                format!(
                    "ecologits electricity mix (zone resolved from {})",
                    zone.source
                ),
            ),
            None => (
                Value::Null,
                "unavailable — no estimator sidecar or unknown zone".to_string(),
            ),
        };

        let value = json!({
            "session_id": session_id,
            "session_meta": session_meta,
            "t_start": t_start,
            "t_last": t_last,
            "events": events.len(),
            "attribution_policy": policy_schema_name(applied),
            "attribution_policy_id": af_core::policy_id(applied),
            "methodology": {
                "version": METHODOLOGY_VERSION,
                "source": "bundled",
            },
            "grid": {
                "zone": zone.id,
                "g_co2e_per_kwh": g_co2e,
                "source": grid_source,
            },
            "state_dir": self.state_dir.display().to_string(),
            "schema_version": "0.1.0",
            "mode": "watch --debug",
        });
        // A frame, not just stored state: the console's session picker
        // follows sessions live, and a session that only existed in
        // `GET /debug/session` would never appear without a refetch.
        self.publish("session", value);
    }

    /// DATA-CONTRACT §2.6 — the session-level `impact_join` verbatim, the
    /// per-model estimate groups, and the status histogram.
    fn publish_report(&mut self, store: &Store, events: &[Envelope], session_id: &str) {
        if self.debug_state.is_none() || !self.diagnostics() {
            return;
        }
        let Ok(joins) = store.joins_for_session(session_id) else {
            return;
        };
        let Some((_, record)) = joins
            .into_iter()
            .find(|(unit_key, _)| unit_key.starts_with("session:"))
        else {
            return;
        };

        let llm_calls: Vec<&Envelope> = events
            .iter()
            .filter(|event| matches!(event.payload, Payload::LlmCall(_)))
            .collect();
        let ids: Vec<String> = llm_calls
            .iter()
            .map(|event| event.event_id.clone())
            .collect();
        let stored = store.estimates_for_events(&ids).unwrap_or_default();

        // All five contract statuses are zero-filled so an empty category
        // reads as "zero occurrences", not "not reported". `missing_usage`
        // is this pipeline's sixth status (a call with no token count never
        // reaches the estimator) and is added only when it occurred.
        let mut histogram: Map<String, Value> =
            ["ok", "unknown_model", "missing_zone", "pending", "error"]
                .iter()
                .map(|status| ((*status).to_string(), json!(0)))
                .collect();

        let mut by_model: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for event in &llm_calls {
            let Payload::LlmCall(call) = &event.payload else {
                continue;
            };
            let blob = stored.get(&event.event_id);
            // Absent row vs. present-but-unreadable row are different
            // facts, and `af_core::join` already distinguishes them. No row
            // means the estimator has not reached this call yet
            // (`pending`); a row we cannot read a status out of means
            // something wrote it and we cannot say what it decided, which
            // is `error`. Reporting that as `pending` promises a result
            // still to come, and the console shows a spinner forever.
            let status = match blob {
                None => "pending".to_string(),
                Some(blob) => blob
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string(),
            };
            let seen = histogram.get(&status).and_then(Value::as_u64).unwrap_or(0);
            histogram.insert(status.clone(), json!(seen + 1));

            let mut estimate = Map::new();
            estimate.insert("event_id".into(), json!(event.event_id));
            estimate.insert("estimation_status".into(), json!(status));
            if let Some(impacts) = blob.and_then(|b| b.get("impacts")) {
                estimate.insert("impacts".into(), impacts.clone());
            }
            estimate.insert(
                "methodology".into(),
                json!({
                    "version": blob
                        .and_then(|b| b.pointer("/methodology/ecologits_version"))
                        .and_then(Value::as_str)
                        .map(|v| format!("ecologits-{v}"))
                        .unwrap_or_else(|| "unknown".to_string()),
                    "source": "bundled",
                }),
            );
            by_model
                .entry(call.model_id_requested.clone())
                .or_default()
                .push(Value::Object(estimate));
        }

        let by_model: Vec<Value> = by_model
            .into_iter()
            .map(|(model_id, estimates)| {
                json!({
                    "model_id": model_id,
                    "impacts": sum_impacts(&estimates),
                    "estimates": estimates,
                })
            })
            .collect();

        self.publish(
            "report",
            json!({
                "level": "session",
                "session_id": session_id,
                "impact_join": record,
                "by_model": by_model,
                "estimation_status_histogram": Value::Object(histogram),
            }),
        );
    }

    /// DATA-CONTRACT §2.7.
    ///
    /// `conformance` is **deliberately absent**: gap #9's per-field presence
    /// counters were a design proposal, not an agreed feature, and the
    /// contract's own type makes the key optional precisely so its absence
    /// can mean "not counted" instead of a table of zeroes claiming
    /// everything was checked and found missing.
    fn publish_health<'a, I>(&mut self, loaded: I)
    where
        I: IntoIterator<Item = (&'a str, &'a [Envelope])>,
    {
        if self.debug_state.is_none() || !self.diagnostics() {
            return;
        }

        // Rows for the sessions this pass loaded replace their previous
        // versions; rows for sessions the pass did not touch are kept in
        // `self.health_rows`. `loaded` only ever holds the touched
        // sessions, so rebuilding from it alone erased everyone else.
        for (session_id, events) in loaded {
            let mut per_collector: BTreeMap<String, CollectorStats> = BTreeMap::new();
            for event in events {
                let stats = per_collector
                    .entry(event.collector.name.clone())
                    .or_insert_with(|| CollectorStats::new(&event.collector.version));
                stats.events += 1;
                if event.ts > stats.last_seen {
                    stats.last_seen = event.ts.clone();
                }
                stats.emits.insert(event.type_tag().to_string());
            }
            for (name, stats) in per_collector {
                let file = self
                    .spool_state
                    .get(&(name.clone(), session_id.to_string()));
                let byte_offset = file.map(|(_, offset)| *offset).unwrap_or(0);
                self.health_rows.insert(
                    format!("{name}:{session_id}"),
                    json!({
                        "name": name,
                        "session_id": session_id,
                        "version": stats.version,
                        "transport": if name.starts_with("otlp-") { "POST /v1/logs" } else { "jsonl spool" },
                        "spool_file": file.and_then(|(path, _)| path.file_name()).map(|n| n.to_string_lossy().into_owned()),
                        "byte_offset": byte_offset,
                        "events": stats.events,
                        "events_per_s": Value::Null,
                        "rejected": self
                            .rejects_by_collector
                            .get(&format!("{name}:{session_id}"))
                            .copied()
                            .unwrap_or(0),
                        "last_seen": stats.last_seen,
                        "emits": stats.emits.into_iter().collect::<Vec<_>>(),
                    }),
                );
            }
        }
        let collectors: Vec<Value> = self.health_rows.values().cloned().collect();

        let python = self.python_health();

        // OTLP losses come from the receiver's own counters: the reject
        // records it sends are shown in the list below but counted only
        // here, since `rejects_total` is the spool tail's count.
        let otlp_losses = self
            .otlp
            .as_ref()
            .map(|(_, counters)| counters.logs_dropped() + counters.bodies_quarantined())
            .unwrap_or(0);

        let retired: Vec<Value> = self
            .retired_collectors
            .iter()
            .map(|(name, totals)| {
                json!({
                    "name": name,
                    "sessions": totals.sessions,
                    "events": totals.events,
                    "rejected": totals.rejected,
                })
            })
            .collect();
        let payload = json!({
            "collectors": collectors,
            "retired_collectors": retired,
            "otlp_receiver": self.otlp_health(),
            // The window, plus what it does not show. A truncated list that
            // did not say it was truncated would under-report a spool that
            // is failing wholesale as if it had failed a hundred times.
            "rejected": self.rejects.iter().cloned().collect::<Vec<_>>(),
            // The total is every record this process is known to have lost,
            // not only the spool's share of them. An OTLP body the receiver
            // rejected and a record it could not map never touch the
            // spool, so `rejected_total` used to read 0 while the
            // receiver was losing records. The components stay separately visible
            // below and under `otlp_receiver`, because they are diagnosed
            // in different places.
            "rejected_total": self.rejects_total.saturating_add(otlp_losses),
            "rejected_spool": self.rejects_total,
            "rejected_otlp": otlp_losses,
            "rejected_dropped": self.rejects_dropped,
            "opaque_events": self.opaque_events,
            "python": python,
            "samplers": self.sessions.iter().map(|(session_id, state)| {
                json!({
                    "session_id": session_id,
                    "state": if self.sampler.is_some() && state.watch_sent { "ready" } else if self.sampler_retry.next_attempt.is_some() { "retrying" } else { "waiting" },
                    "pid": self.sampler.as_ref().map(|sampler| sampler.sidecar.pid()),
                    "failures": self.sampler_retry.failures,
                    "last_error": self.sampler_retry.last_error.as_deref(),
                })
            }).collect::<Vec<_>>(),
            "estimator_worker": self.estimator.as_ref().map(|worker| {
                let health = worker.health();
                json!({
                    "state": health.state,
                    "pending": health.pending,
                    "processed": health.processed,
                    "failures": health.failures,
                    "last_error": health.last_error,
                })
            }),
        });
        self.publish("health", payload);
    }

    /// The `python` rows of the health payload, recomputed at most once per
    /// [`PYTHON_HEALTH_TTL`].
    ///
    /// The findings themselves are unchanged — this is the same
    /// `af_sidecar::doctor` output, rendered the same way. Only how often
    /// the fork behind it happens has changed.
    fn python_health(&mut self) -> Vec<Value> {
        let fingerprint = af_sidecar::environment_fingerprint(&self.state_dir);
        if let Some(cached) = &self.python_health {
            if cached.taken_at.elapsed() < PYTHON_HEALTH_TTL && cached.fingerprint == fingerprint {
                return cached.findings.clone();
            }
        }
        let findings = doctor(&self.state_dir);
        let rendered: Vec<Value> = if findings.is_empty() {
            vec![json!({"key": "venv", "value": "healthy", "status": "ok"})]
        } else {
            findings
                .iter()
                .map(|finding| {
                    json!({
                        "key": "venv",
                        "value": finding.message,
                        "status": match finding.severity {
                            Severity::Error => "error",
                            Severity::Warn => "warn",
                        },
                    })
                })
                .collect()
        };
        self.python_health = Some(PythonHealth {
            taken_at: Instant::now(),
            fingerprint,
            findings: rendered.clone(),
        });
        rendered
    }

    /// The OTLP half of the health payload. `logs_accepted` counts Contract
    /// #1 events normalized out of `/v1/logs` bodies, not requests — the
    /// question a user asks of a receiver is whether their telemetry became
    /// events, and a 200 that normalized nothing is precisely the failure
    /// that question is about.
    ///
    /// The no-receiver case is the same shape with its counters left at
    /// their zero defaults plus a `note` saying why, rather than a second
    /// literal restating all six of them: two literals are two places for
    /// the payload's key set to drift, and the console reads them as one
    /// type.
    fn otlp_health(&self) -> Value {
        let mut health = json!({
            "endpoint": Value::Null,
            "protocol": "http/json",
            "logs_accepted": 0,
            "logs_requests": 0,
            // Records the receiver identified and then could not map, and
            // bodies it rejected (metadata only, into the store's reject
            // ring). Both are losses, and neither ever passed through the
            // spool, so neither was visible in any spool reject count.
            "logs_dropped": 0,
            "logs_unclaimed": 0,
            "bodies_quarantined": 0,
            "metrics_discarded": 0,
            "normalizers": af_otlp::installed_normalizers().into_iter().map(|normalizer| json!({
                "id": normalizer.id,
                "signal": normalizer.signal,
                "emits": normalizer.emits,
                "lifecycle": normalizer.lifecycle,
            })).collect::<Vec<_>>(),
        });
        match &self.otlp {
            Some((addr, counters)) => {
                health["endpoint"] = json!(addr.to_string());
                health["logs_accepted"] = json!(counters.logs_accepted());
                health["logs_requests"] = json!(counters.logs_requests());
                health["logs_dropped"] = json!(counters.logs_dropped());
                health["logs_unclaimed"] = json!(counters.logs_unclaimed());
                health["bodies_quarantined"] = json!(counters.bodies_quarantined());
                health["metrics_discarded"] = json!(counters.metrics_discarded());
                health["bodies_timed_out"] = json!(counters.bodies_timed_out());
            }
            None => {
                health["note"] = json!("no OTLP receiver is running in this af watch process");
            }
        }
        health
    }

    /// Whether anyone can see diagnostics: the stderr decision stream
    /// (`--debug`) or someone watching the `/debug` state. Otherwise frames
    /// and decisions are not built at all.
    fn diagnostics(&self) -> bool {
        self.debug || (self.watched && self.debug_state.is_some())
    }

    /// Whether `ts` is inside the window a console is told about.
    fn recent(&self, ts: &str) -> bool {
        self.console_window_ms == 0
            || af_core::parse_ts(ts).is_none_or(|ms| ms >= now_ms() - self.console_window_ms)
    }

    /// The decision and `fact` frame for one event, once.
    fn publish_fact(&mut self, event: &Envelope) {
        if (self.backfill && !self.recent(&event.ts))
            || !self.reported_events.insert(&event.event_id)
        {
            return;
        }
        self.decide(&ingest_decision(event));
        if let Ok(value) = serde_json::to_value(event) {
            self.publish("fact", value);
        }
        if let Payload::ActionSpan(span) = &event.payload {
            self.decide(&span_open_decision(event, span));
        }
    }

    /// Follows consoles coming and going, and serves what one asked for.
    ///
    /// The first to attach is told the present: status, the picker's rows,
    /// and each live session rebuilt with its last minutes of facts and
    /// traces. When the last leaves, the debug state is emptied and the
    /// collector goes back to building none of it.
    fn sync_attachment(&mut self, store: &mut Store) {
        let Some(state) = self.debug_state.clone() else {
            return;
        };
        let Ok((attached, wanted)) = state
            .lock()
            .map(|mut state| (state.attached() > 0, state.take_wanted()))
        else {
            return;
        };
        if self.on_demand && attached != self.watched {
            self.watched = attached;
            if attached {
                self.refresh_agents();
                self.publish_status(Instant::now(), true);
                let live = self.sessions.keys().cloned().collect();
                self.tell(store, &live, true);
            } else {
                if let Ok(mut state) = state.lock() {
                    state.forget_picture();
                }
                self.published_allocs.clear();
                self.reported_events = BoundedSet::new(DEDUP_HORIZON);
            }
        }
        if self.diagnostics() && !wanted.is_empty() {
            self.tell(store, &wanted, false);
        }
    }

    /// Publishes `sessions` as a backfill; with `list`, the picker's rows
    /// for every stored session first. A session the store does not hold
    /// is skipped.
    fn tell(&mut self, store: &Store, sessions: &BTreeSet<String>, list: bool) {
        let summaries = match store.session_summaries() {
            Ok(summaries) => summaries,
            Err(err) => {
                eprintln!("af watch: listing sessions for the console failed: {err:#}");
                return;
            }
        };
        let known: BTreeSet<String> = summaries
            .iter()
            .filter(|summary| sessions.contains(&summary.session_id))
            .map(|summary| summary.session_id.clone())
            .collect();
        if list {
            let rows = summaries
                .iter()
                .map(|summary| {
                    json!({
                        "session_id": summary.session_id,
                        "agent_app": Value::Null,
                        "t_start": summary.first_ts,
                        "t_last": summary.last_ts,
                        "events": summary.counts.iter().map(|(_, n)| n).sum::<u64>(),
                    })
                })
                .collect();
            // Also sent down the stream: a page that connected before this
            // ran has already fetched an empty list.
            let listed = self.debug_state.as_ref().and_then(|state| {
                let mut state = state.lock().ok()?;
                state.set_session_summaries(rows);
                Some(state.session_list())
            });
            if let Some(listed) = listed {
                self.publish("sessions", listed);
            }
        }
        self.backfill = true;
        if let Err(err) = self.show_sessions(store, &known) {
            eprintln!("af watch: describing sessions for the console failed: {err:#}");
        }
        self.backfill = false;
    }

    /// Publishes `sessions` from what the store already holds, and the
    /// health payload. Looking only: nothing is rebuilt or written, so a
    /// console's request can never change a stored result.
    fn show_sessions(&mut self, store: &Store, sessions: &BTreeSet<String>) -> Result<()> {
        // Not cached: the first pass to resolve a zone rebuilds the whole
        // store, and must still find none resolved.
        let zone = match &self.zone {
            Some(zone) => zone.clone(),
            None => {
                let (id, source) = super::resolve_zone(store, self.zone_flag.as_deref())?;
                Zone::unresolved(id, source)
            }
        };
        let mut loaded = BTreeMap::new();
        for session in sessions {
            loaded.insert(
                session.clone(),
                LoadedSession::load(store.events_for_session(session)?, true),
            );
        }
        for (session_id, loaded) in &loaded {
            self.refresh_session(store, session_id, loaded, &zone)?;
        }
        self.publish_health(
            loaded
                .iter()
                .map(|(session_id, loaded)| (session_id.as_str(), loaded.events.as_slice())),
        );
        Ok(())
    }

    /// Emits one decision on both surfaces it has: the `--debug` stderr
    /// stream and the SSE `decision` frame.
    fn decide(&mut self, decision: &Decision) {
        if self.debug {
            eprintln!("{}", decision.stderr_line());
        }
        self.publish("decision", decision.frame());
    }

    fn publish(&mut self, event: &'static str, data: Value) {
        if !self.diagnostics() {
            return;
        }
        let Some(state) = &self.debug_state else {
            return;
        };
        if let Ok(mut state) = state.lock() {
            state.publish(event, data, now_ms());
        }
    }

    /// Stops every sampler the way the sampler's own protocol asks to be
    /// stopped: a `shutdown` op, so the window in flight is flushed to the
    /// spool before the process goes away.
    fn shutdown(&mut self) {
        let mut stopped = 0usize;
        if let Some(sampler) = self.sampler.as_mut() {
            if let Err(err) = sampler.sidecar.send(&json!({"op": "shutdown"})) {
                eprintln!("af watch: sampler shutdown op failed: {err:#}");
            }
            stopped += 1;
        }
        if stopped > 0 {
            // Give the samplers a moment to write their final window before
            // `Drop` kills them; the final ingest pass then picks it up.
            std::thread::sleep(Duration::from_millis(500));
            self.sampler = None;
        }
    }
}

/// A cached `af python doctor` answer, rendered for the health payload.
struct PythonHealth {
    taken_at: Instant,
    fingerprint: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    findings: Vec<Value>,
}

/// What retired sessions' health rows added up to, per collector.
#[derive(Debug, Default)]
struct RetiredCollector {
    sessions: u64,
    events: u64,
    rejected: u64,
}

/// One collector's contribution to a session, as the health payload's
/// collector row reports it.
struct CollectorStats {
    version: String,
    events: u64,
    /// The latest `ts` seen, compared lexicographically — every timestamp
    /// this project handles is fixed-width RFC 3339 UTC, so that is also
    /// chronological order.
    last_seen: String,
    /// The distinct Contract #1 payload types this collector emitted.
    emits: BTreeSet<String>,
}

impl CollectorStats {
    fn new(version: &str) -> Self {
        CollectorStats {
            version: version.to_string(),
            events: 0,
            last_seen: String::new(),
            emits: BTreeSet::new(),
        }
    }
}

/// The collector and session a spool file's name stands for; `None` for a
/// path that is not a spool file.
fn spool_file_of(path: &Path) -> Option<af_spool::SpoolFile> {
    let name = path.file_name()?.to_str()?;
    af_spool::parse_spool_filename(name, PathBuf::new())
}

/// `cc-hooks.sess-1.jsonl` → `cc-hooks:sess-1`, the key the health
/// payload's collector rows use.
///
/// Parsed by `af_spool`'s own filename grammar rather than a second copy of
/// it here, because a reject is located by its file and nothing else: the
/// line failed to parse, so it has no `collector` field of its own to read,
/// and a key derived from a *different* reading of the filename than the
/// one `af_spool::scan` used would not match any collector row.
fn collector_key(origin: &str) -> Option<String> {
    let file = af_spool::parse_spool_filename(origin, PathBuf::new())?;
    Some(format!("{}:{}", file.collector, file.session_id))
}

/// Sums the `total` range of each criterion across `ok` estimates, for the
/// §2.6 `by_model` rows.
///
/// The arithmetic is `af_core::sum_criteria` — the very function
/// `build_joins` sums a unit's remote impacts with — rather than a second
/// implementation of it. This module renders; it does not compute (see
/// `super::debug_frames`), and a per-model total that disagreed with the
/// join's would be two answers to one question with nothing to say which
/// is right.
///
/// What is applied here is the *publication* guard, which `sum_criteria`
/// reports rather than enforces: a criterion any estimate reported in a
/// second unit is **dropped entirely** rather than summed, because a
/// wrong-unit sum is wrong in a way no consumer could detect from the
/// output. Partial coverage is not dropped — a subtotal over the estimates
/// that reported the criterion is a real lower bound.
fn sum_impacts(estimates: &[Value]) -> Value {
    let ok: Vec<&Value> = estimates
        .iter()
        .filter(|estimate| estimate.get("estimation_status").and_then(Value::as_str) == Some("ok"))
        .collect();

    let mut out = Map::new();
    for (criterion, sum) in af_core::sum_criteria(&ok) {
        if sum.mismatches > 0 {
            continue;
        }
        out.insert(
            criterion,
            json!({"unit": sum.unit, "total": {"min": sum.min, "max": sum.max}}),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::rename_table;

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn facts_are_rebuilt_once_the_interval_has_passed() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut dirty = DirtySessions::default();
        dirty.mark(&set(&["a"]), &set(&["a"]));
        assert_eq!(
            dirty.take_due(start, interval),
            Some(set(&["a"])),
            "the first rebuild is immediate"
        );

        dirty.mark(&set(&["a"]), &set(&["a"]));
        assert_eq!(
            dirty.take_due(start + Duration::from_secs(5), interval),
            None
        );
        assert_eq!(
            dirty.take_due(start + Duration::from_secs(10), interval),
            Some(set(&["a"]))
        );
    }

    #[test]
    fn sample_only_staleness_waits_longer_but_is_never_dropped() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut dirty = DirtySessions::default();
        dirty.mark(&set(&["a"]), &set(&["a"]));
        dirty.take_due(start, interval);

        dirty.mark(&set(&["a"]), &BTreeSet::new());
        assert_eq!(
            dirty.take_due(start + Duration::from_secs(10), interval),
            None
        );
        assert_eq!(
            dirty.take_due(start + interval * SAMPLE_REBUILD_FACTOR, interval),
            Some(set(&["a"]))
        );
    }

    #[test]
    fn a_fact_rebuild_also_takes_the_sessions_only_samples_made_stale() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut dirty = DirtySessions::default();
        dirty.mark(&set(&["b"]), &BTreeSet::new());
        dirty.mark(&set(&["a"]), &set(&["a"]));
        assert_eq!(dirty.take_due(start, interval), Some(set(&["a", "b"])));
        assert_eq!(
            dirty.take_due(start + interval * 10, interval),
            None,
            "nothing left"
        );
    }

    #[test]
    fn a_session_never_rebuilt_is_rebuilt_at_once_even_mid_interval() {
        // Its first rebuild is what sends its sampler watch: waiting out
        // another session's interval would lose that much measured energy.
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut dirty = DirtySessions::default();
        dirty.mark(&set(&["a"]), &set(&["a"]));
        dirty.take_due(start, interval);

        dirty.mark(&set(&["b"]), &set(&["b"]));
        assert_eq!(
            dirty.take_due(start + Duration::from_secs(1), interval),
            Some(set(&["b"]))
        );
        dirty.mark(&set(&["b"]), &set(&["b"]));
        assert_eq!(
            dirty.take_due(start + Duration::from_secs(2), interval),
            None,
            "afterwards it waits like everyone else"
        );
    }

    fn quiet_args() -> WatchArgs {
        WatchArgs {
            debug: false,
            no_sidecars: true,
            no_otlp: true,
            require_receiver: false,
            otlp_addr: DEFAULT_OTLP_ADDR.to_string(),
            debug_addr: DEFAULT_DEBUG_ADDR.to_string(),
            interval: 5.0,
            local_grid_zone: None,
            remote_region: None,
            batch_interval: Duration::ZERO,
            console_window_ms: debug_server::SNAPSHOT_WINDOW_MS,
        }
    }

    // --- watcher status ------------------------------------------------------

    fn status_watch() -> (tempfile::TempDir, Watch, Arc<Mutex<DebugState>>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let debug = Arc::new(Mutex::new(DebugState::default()));
        let mut watch = Watch::new(dir.path(), &quiet_args(), Some(Arc::clone(&debug)), None);
        // An empty home: no agent is configured, whatever this machine has.
        watch.home = Some(dir.path().join("home"));
        watch.refresh_agents();
        (dir, watch, debug)
    }

    fn agent<'a>(status: &'a Value, id: &str) -> &'a Value {
        status["agents"]
            .as_array()
            .expect("agents array")
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("no agent row {id} in {status}"))
    }

    #[test]
    fn boot_id_is_16_hex_and_differs_across_processes() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_nanos(1_794_000_000_123_456_789);
        let id = boot_id(t, 1);
        assert_eq!(id.len(), 16);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        assert_eq!(id, boot_id(t, 1), "stable for one process");
        assert_ne!(id, boot_id(t, 2), "another pid at the same instant");
        assert_ne!(
            id,
            // Windows keeps time in steps of 100 ns.
            boot_id(t + Duration::from_micros(1), 1),
            "the same pid, reused later"
        );
    }

    #[test]
    fn status_payload_has_the_contract_fields() {
        let (_dir, watch, _debug) = status_watch();
        let status = watch.status_payload();
        assert_eq!(status["boot_id"], watch.boot_id.as_str());
        assert_eq!(status["boot_id"].as_str().unwrap().len(), 16);
        let started = status["started_at"].as_str().expect("started_at");
        assert!(started.ends_with('Z') && started.contains('T'), "{started}");
        assert_eq!(status["af_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["flush_interval_ms"], 0, "quiet_args flushes at once");
        assert_eq!(status["receiver"]["endpoint"], Value::Null);
        assert!(
            status["receiver"]["note"].is_string(),
            "no receiver says why"
        );
        let ids: Vec<&str> = status["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        let expected: Vec<&str> = agents::AgentId::all().into_iter().map(|a| a.id()).collect();
        assert_eq!(
            ids, expected,
            "one row per supported agent, in a fixed order"
        );
    }

    #[test]
    fn activity_from_an_unknown_collector_changes_nothing() {
        let (_dir, mut watch, _debug) = status_watch();
        let before = watch.status_payload();
        for collector in [
            "otlp-genai",
            "otlp-http",
            "codecarbon",
            "codecarbon-sampler",
            "",
        ] {
            watch.note_activity(collector, "s", 1_000);
        }
        assert_eq!(watch.status_payload(), before);
    }

    #[test]
    fn an_event_flips_its_agent_to_receiving_and_counts_sessions() {
        let (_dir, mut watch, _debug) = status_watch();
        let codex_before = agent(&watch.status_payload(), "codex").clone();
        assert_ne!(
            agent(&watch.status_payload(), "claude-code")["state"],
            "receiving"
        );

        watch.note_activity("otlp-cc", "s1", 1_000);
        watch.note_activity("cc-hooks", "s1", 2_000);
        watch.note_activity("otlp-cc", "s2", 3_000);
        // An older receipt arriving late does not move the clock back.
        watch.note_activity("otlp-cc", "s2", 2_500);

        let status = watch.status_payload();
        let claude = agent(&status, "claude-code");
        assert_eq!(claude["state"], "receiving");
        assert_eq!(claude["sessions"], 2);
        assert_eq!(claude["last_event_at"], af_core::rfc3339_ms(3_000).unwrap());
        assert_eq!(
            agent(&status, "codex"),
            &codex_before,
            "another agent's row"
        );
    }

    #[test]
    fn per_session_activity_keeps_each_sessions_latest_event() {
        let (_dir, mut watch, _debug) = status_watch();
        watch.note_activity("otlp-codex", "s-a", 1_000);
        watch.note_activity("otlp-codex", "s-b", 2_000);
        watch.note_activity("otlp-codex", "s-a", 500);
        let activity = &watch.activity[&AgentId::Codex];
        assert_eq!(activity.sessions["s-a"], 1_000);
        assert_eq!(activity.sessions["s-b"], 2_000);
        assert_eq!(activity.last_event_at_ms, Some(2_000));
    }

    #[test]
    fn a_session_boot_pid_links_a_claude_process_to_its_session() {
        let (_dir, mut watch, _debug) = status_watch();
        let processes = [agents::AgentProcess {
            pid: 42,
            name: "claude".into(),
            started: SystemTime::now(),
        }];
        watch.agent_facts = vec![agents::facts_from(
            AgentId::ClaudeCode,
            true,
            agents::UserConfig::Absent,
            None,
            Some(&processes),
        )];
        watch.note_activity("cc-hooks", "c1", 3_000);
        let before = watch.status_payload();
        assert_eq!(
            agent(&before, "claude-code")["processes"][0]["state"],
            "unknown"
        );

        // The hook's bootstrap span names the Claude Code process.
        watch.note_session_roots("c1", &[42]);
        let status = watch.status_payload();
        let process = &agent(&status, "claude-code")["processes"][0];
        assert_eq!(process["session_id"], "c1");
        assert_eq!(process["state"], "receiving");
        assert_eq!(
            process["last_event_at"],
            af_core::rfc3339_ms(3_000).unwrap()
        );
    }

    #[test]
    fn status_is_published_at_most_every_five_seconds_and_only_on_change() {
        let (_dir, mut watch, debug) = status_watch();
        // A connection's channel is where every published status lands.
        let (_replay, rx) = debug.lock().unwrap().subscribe(None);
        let published = |rx: &std::sync::mpsc::Receiver<Vec<u8>>| rx.try_iter().count();
        let t0 = Instant::now();

        watch.publish_status(t0, true);
        assert_eq!(published(&rx), 1, "the forced first publication");

        watch.note_activity("otlp-cc", "s1", 1_000);
        watch.publish_status(t0 + Duration::from_secs(1), false);
        assert_eq!(published(&rx), 0, "changed, but inside the 5s floor");

        watch.publish_status(t0 + Duration::from_secs(5), false);
        assert_eq!(published(&rx), 1, "changed and the floor has passed");

        watch.publish_status(t0 + Duration::from_secs(10), false);
        watch.publish_status(t0 + Duration::from_secs(60), false);
        assert_eq!(published(&rx), 0, "unchanged: nothing to say");
    }

    #[test]
    fn spool_notices_count_as_activity_by_file_name() {
        let (_dir, mut watch, _debug) = status_watch();
        watch.note_spool_paths(
            &[
                PathBuf::from("/state/spool/cc-hooks.sess-1.jsonl"),
                PathBuf::from("/state/spool/codecarbon.sess-1.jsonl"),
                PathBuf::from("/state/spool/not-a-spool-file.txt"),
                PathBuf::from("/state/spool"),
            ],
            7_000,
        );
        let status = watch.status_payload();
        let claude = agent(&status, "claude-code");
        assert_eq!(claude["state"], "receiving");
        assert_eq!(claude["sessions"], 1);
        assert_ne!(agent(&status, "codex")["state"], "receiving");
    }

    fn one_fact() -> Envelope {
        let line = include_str!(
            "../../../../tests/fixtures/spool/basic-session/cc-hooks.sess-basic.jsonl"
        )
        .lines()
        .next()
        .expect("fixture line");
        af_events::parse_line(line).expect("fixture parses")
    }

    fn reject() -> af_otlp::Reject {
        af_otlp::Reject {
            ts_ms: 0,
            source: "otlp".to_string(),
            reason: "test".to_string(),
            attribute_keys: Vec::new(),
            bytes: 1,
        }
    }

    /// A session of `samples` five-second energy windows, each with its
    /// process sample, under one bootstrap span.
    fn long_session(samples: usize) -> Vec<Envelope> {
        session_from("s", 1_785_326_400_000, samples)
    }

    /// `samples` five-second windows of session `id`, starting at `base_ms`.
    fn session_from(id: &str, base_ms: i64, samples: usize) -> Vec<Envelope> {
        let ts = |seconds: usize| af_core::rfc3339_ms(base_ms + seconds as i64 * 1000).expect("ts");
        let mut boot = af_events::fixtures::action_span("boot", &ts(0), &ts(1));
        boot.tool_name = af_core::BOOTSTRAP_TOOL_NAME.to_string();
        boot.pids = Some(vec![42]);
        let mut events = vec![af_events::fixtures::envelope(
            &format!("evt-boot-{id}-0001"),
            id,
            &ts(0),
            Payload::ActionSpan(boot),
        )];
        for i in 0..samples {
            let (t0, t1) = (ts(i * 5), ts(i * 5 + 5));
            events.push(af_events::fixtures::envelope(
                &format!("evt-energy-{id}-{i:010}"),
                id,
                &t1,
                Payload::EnergySample(af_events::fixtures::energy_sample(&t0, &t1, 1.0)),
            ));
            events.push(af_events::fixtures::envelope(
                &format!("evt-procs-{id}-{i:010}"),
                id,
                &t1,
                Payload::ProcessSample(af_events::fixtures::process_sample(
                    &t0,
                    &t1,
                    &[(42, 100, None)],
                )),
            ));
        }
        events
    }

    #[test]
    fn a_refresh_publishes_only_traces_not_published_before_however_long_the_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("state.db")).expect("open");
        let debug = Arc::new(Mutex::new(DebugState::with_limits(100_000, 16)));
        let mut watch = Watch::new(dir.path(), &quiet_args(), Some(Arc::clone(&debug)), None);
        // Longer than the old shared dedup horizon, which made every refresh
        // of such a session re-publish all of it.
        let loaded = LoadedSession::load(long_session(DEDUP_HORIZON + 1), true);
        let zone = Zone::unresolved("FRA", "test");

        watch
            .refresh_session(&store, "s", &loaded, &zone)
            .expect("first refresh");
        let first = debug.lock().expect("lock").frames_of("alloc");
        assert_eq!(first, DEDUP_HORIZON + 1);
        watch
            .refresh_session(&store, "s", &loaded, &zone)
            .expect("second refresh");
        assert_eq!(
            debug.lock().expect("lock").frames_of("alloc"),
            first,
            "nothing new to say"
        );

        watch.forget_session("s");
        assert!(watch.published_allocs.is_empty(), "retirement releases it");
    }

    /// A collector with a console socket and nobody on it.
    fn unattended(dir: &Path) -> (Watch, Arc<Mutex<DebugState>>) {
        let debug = Arc::new(Mutex::new(DebugState::with_limits(100_000, 16)));
        let mut watch = Watch::new(dir, &quiet_args(), Some(Arc::clone(&debug)), None);
        watch.home = Some(dir.join("home"));
        watch.on_demand = true;
        watch.watched = false;
        (watch, debug)
    }

    #[test]
    fn with_nobody_attached_nothing_diagnostic_is_built_or_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("state.db")).expect("open");
        let (mut watch, debug) = unattended(dir.path());
        assert!(!watch.diagnostics());
        let loaded = LoadedSession::load(long_session(10), watch.diagnostics());
        watch
            .refresh_session(&store, "s", &loaded, &Zone::unresolved("FRA", "test"))
            .expect("refresh");
        watch.decide(&Decision {
            kind: crate::cmd::debug_frames::DecisionKind::Attr,
            ts: "2026-01-01T00:00:00.000Z".into(),
            text: "x".into(),
            reference: None,
        });
        let state = debug.lock().expect("lock");
        assert_eq!(state.frames_of("alloc") + state.frames_of("decision"), 0);
        assert_eq!(state.session_list(), json!([]));
        assert!(watch.published_allocs.is_empty());
    }

    #[test]
    fn a_backfill_tells_only_the_last_three_minutes_and_never_the_rest_later() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("state.db")).expect("open");
        let debug = Arc::new(Mutex::new(DebugState::with_limits(100_000, 16)));
        let mut watch = Watch::new(dir.path(), &quiet_args(), Some(Arc::clone(&debug)), None);
        let zone = Zone::unresolved("FRA", "test");
        // Twelve windows ending now: 60 s, all inside the window. And a
        // session from long ago.
        let recent = LoadedSession::load(session_from("new", now_ms() - 60_000, 12), true);
        let old = LoadedSession::load(session_from("old", 1_785_326_400_000, 12), true);

        watch.backfill = true;
        watch
            .refresh_session(&store, "new", &recent, &zone)
            .expect("new");
        watch
            .refresh_session(&store, "old", &old, &zone)
            .expect("old");
        watch.backfill = false;
        {
            let state = debug.lock().expect("lock");
            assert_eq!(state.frames_of("alloc"), 12, "the recent session's only");
            assert!(state.frames_of("fact") >= 24, "its recent facts too");
            assert_eq!(
                state.session_list().as_array().unwrap().len(),
                2,
                "both are listed"
            );
        }
        // The old session's traces were passed over, not postponed.
        watch
            .refresh_session(&store, "old", &old, &zone)
            .expect("old again");
        assert_eq!(debug.lock().expect("lock").frames_of("alloc"), 12);
    }

    #[test]
    fn attaching_tells_the_live_sessions_lists_the_rest_and_leaving_forgets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        store
            .insert_events(&session_from("live", now_ms() - 60_000, 12))
            .expect("insert");
        store
            .insert_events(&session_from("past", 1_785_326_400_000, 3))
            .expect("insert");
        let (mut watch, debug) = unattended(dir.path());
        watch
            .sessions
            .insert("live".into(), SessionState::seen(Instant::now()));

        watch.sync_attachment(&mut store);
        assert!(!watch.watched, "nobody yet");
        assert_eq!(debug.lock().unwrap().session_list(), json!([]));

        debug.lock().unwrap().set_attached(1);
        watch.sync_attachment(&mut store);
        assert!(watch.diagnostics());
        {
            let state = debug.lock().unwrap();
            let list = state.session_list();
            let ids: Vec<&str> = list
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, ["live", "past"], "most recent first");
            assert_eq!(list[1]["events"], 7, "the past one from the store's count");
            assert_eq!(
                state.frames_of("alloc"),
                12,
                "live traces of the last minutes"
            );
            assert_eq!(
                state.frames_of("session"),
                1,
                "only the live session is rebuilt"
            );
            assert_eq!(
                state.frames_of("sessions"),
                1,
                "the list goes down the stream too"
            );
        }

        debug.lock().unwrap().set_attached(0);
        watch.sync_attachment(&mut store);
        assert!(!watch.diagnostics());
        assert_eq!(debug.lock().unwrap().session_list(), json!([]));
        assert!(watch.published_allocs.is_empty());

        // And again: the same picture, not an empty one.
        debug.lock().unwrap().set_attached(1);
        watch.sync_attachment(&mut store);
        assert_eq!(debug.lock().unwrap().frames_of("alloc"), 12);
    }

    #[test]
    fn a_session_asked_for_is_published_and_an_unknown_one_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        store
            .insert_events(&session_from("past", 1_785_326_400_000, 3))
            .expect("insert");
        let (mut watch, debug) = unattended(dir.path());
        debug.lock().unwrap().set_attached(1);
        watch.sync_attachment(&mut store);
        assert_eq!(debug.lock().unwrap().frames_of("session"), 0);

        debug.lock().unwrap().want("past");
        debug.lock().unwrap().want("no-such-session");
        watch.sync_attachment(&mut store);
        {
            let state = debug.lock().unwrap();
            assert_eq!(state.frames_of("session"), 1);
            assert_eq!(state.frames_of("alloc"), 0, "its old traces stay untold");
        }
        // Looking is not writing: a request from a console must never
        // rewrite a session's stored results (here it would, under a zone
        // this watch has not resolved yet).
        assert!(
            store.joins_for_session("past").expect("joins").is_empty(),
            "showing a session stores nothing"
        );
    }

    #[test]
    fn attaching_to_an_idle_collector_still_tells_its_health() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        let (mut watch, debug) = unattended(dir.path());
        debug.lock().unwrap().set_attached(1);
        watch.sync_attachment(&mut store);
        assert!(
            debug.lock().unwrap().has_health(),
            "receiver and runtime health"
        );
    }

    #[test]
    fn a_headless_refresh_keeps_no_publication_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("state.db")).expect("open");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);
        let loaded = LoadedSession::load(long_session(10), watch.diagnostics());
        watch
            .refresh_session(&store, "s", &loaded, &Zone::unresolved("FRA", "test"))
            .expect("refresh");
        assert!(watch.published_allocs.is_empty());
    }

    #[test]
    fn python_health_is_recomputed_when_the_runtime_changes_not_on_a_timer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);
        let missing = watch.python_health();
        assert!(missing[0]["value"]
            .as_str()
            .unwrap_or("")
            .contains("missing"));
        let taken = watch.python_health.as_ref().expect("cached").taken_at;
        watch.python_health();
        assert_eq!(
            watch.python_health.as_ref().expect("cached").taken_at,
            taken,
            "reused"
        );

        std::fs::create_dir_all(dir.path().join("venv")).expect("venv appears");
        let changed = watch.python_health();
        assert_ne!(changed, missing, "a new runtime is asked about again");
    }

    #[test]
    fn a_retired_session_leaves_totals_not_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let debug = Arc::new(Mutex::new(DebugState::default()));
        let mut watch = Watch::new(dir.path(), &quiet_args(), Some(debug), None);
        let events = long_session(3);
        watch.publish_health([("s", events.as_slice())]);
        assert_eq!(watch.health_rows.len(), 1);

        watch.forget_session("s");
        assert!(watch.health_rows.is_empty());
        let totals = &watch.retired_collectors["test-collector"];
        assert_eq!((totals.sessions, totals.events), (1, events.len() as u64));
    }

    #[test]
    fn a_flush_writes_telemetry_estimates_and_rebuilds_in_one_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        let mut store = Store::open(&db).expect("open");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);
        let fact = one_fact();
        watch.estimates.push(af_store::ImpactEstimate {
            event_id: fact.event_id.clone(),
            methodology_version: "test".to_string(),
            json: json!({"status": "ok"}),
        });
        watch.estimated_sessions.insert(fact.session_id.clone());

        let before = crate::cmd::test_support::wal_commits(&db);
        let outcome = watch.write_flush(
            &mut store,
            Inbox {
                facts: vec![fact.clone()],
                rejects: vec![reject()],
            },
            true,
            None,
        );

        assert!(outcome.unwritten.is_empty() && !outcome.reconcile);
        assert_eq!(crate::cmd::test_support::wal_commits(&db) - before, 1);
        assert!(watch.estimates.is_empty(), "stored rows are let go");
        assert!(
            watch.dirty.rebuilt.contains(&fact.session_id),
            "the new session was rebuilt in the same flush"
        );
        assert_eq!(
            store
                .events_for_session(&fact.session_id)
                .expect("read")
                .len(),
            1
        );
        assert_eq!(store.recent_rejects(10).expect("rejects").len(), 1);
    }

    #[test]
    fn estimates_wait_for_a_flush() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        let mut store = Store::open(&db).expect("open");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);
        watch.estimates.push(af_store::ImpactEstimate {
            event_id: "evt".to_string(),
            methodology_version: "test".to_string(),
            json: json!({"status": "ok"}),
        });

        watch.write_flush(&mut store, Inbox::default(), false, None);
        assert_eq!(
            watch.estimates.len(),
            1,
            "not a flush: kept for the next one"
        );
    }

    #[test]
    fn estimates_the_store_refused_are_kept_though_the_flush_commits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        drop(Store::open(&db).expect("create"));
        rename_table(&db, "impact_estimates", "impact_estimates_away");
        let mut store = Store::open(&db).expect("open");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);
        watch.estimates.push(af_store::ImpactEstimate {
            event_id: "evt".to_string(),
            methodology_version: "test".to_string(),
            json: json!({"status": "ok"}),
        });

        watch.write_flush(&mut store, Inbox::default(), true, None);
        assert_eq!(
            watch.estimates.len(),
            1,
            "the worker has already let these go: dropping them loses them"
        );
    }

    #[test]
    fn the_loop_sleeps_until_its_next_deadline_within_bounds() {
        let now = Instant::now();
        assert_eq!(
            loop_wait(now, [None, None]),
            IDLE_POLL,
            "idle: the poll ceiling"
        );
        assert_eq!(
            loop_wait(
                now,
                [
                    Some(now + Duration::from_millis(300)),
                    Some(now + IDLE_POLL * 5)
                ]
            ),
            Duration::from_millis(300)
        );
        assert_eq!(loop_wait(now, [Some(now)]), MIN_WAIT, "past due: no spin");
    }

    #[test]
    fn sample_only_staleness_is_due_after_its_longer_wait() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut dirty = DirtySessions::default();
        assert_eq!(
            dirty.next_due(start, interval),
            None,
            "nothing dirty, no deadline"
        );
        let s = BTreeSet::from(["s".to_string()]);
        dirty.mark(&s, &s);
        assert_eq!(
            dirty.next_due(start, interval),
            Some((start, false)),
            "never rebuilt: now, without waiting for a flush"
        );
        dirty.take_due(start, interval).expect("due");
        dirty.mark(&s, &BTreeSet::new());
        assert_eq!(
            dirty.next_due(start, interval),
            Some((start + interval * SAMPLE_REBUILD_FACTOR, true))
        );
    }

    #[test]
    fn interval_rebuilds_wait_for_a_flush_but_new_sessions_do_not() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut dirty = DirtySessions::default();
        let s = BTreeSet::from(["s".to_string()]);
        dirty.mark(&s, &s);
        assert!(dirty.due(start, interval, false), "never rebuilt: at once");
        dirty.take_due(start, interval).expect("due");

        dirty.mark(&s, &s);
        let later = start + interval;
        assert!(!dirty.due(later, interval, false), "waits for the flush");
        assert!(dirty.due(later, interval, true));

        let t = BTreeSet::from(["t".to_string()]);
        dirty.mark(&t, &t);
        assert!(dirty.due(start, interval, false));
    }

    #[test]
    fn facts_that_could_not_be_stored_are_handed_back_for_a_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        drop(Store::open(&db).expect("create"));
        rename_table(&db, "raw_events", "raw_events_away");
        let mut store = Store::open(&db).expect("open");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);

        let inbox = Inbox {
            facts: vec![one_fact()],
            rejects: Vec::new(),
        };
        watch
            .absorb_inbox(&mut store, &inbox)
            .expect_err("the store refuses the facts");

        rename_table(&db, "raw_events_away", "raw_events");
        watch
            .absorb_inbox(&mut store, &inbox)
            .expect("the retry stores them");
        assert_eq!(
            store.session_ids_for_collector("cc-hooks").expect("read"),
            vec!["sess-basic"]
        );
    }

    #[test]
    fn a_reject_log_failure_does_not_cost_the_facts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        drop(Store::open(&db).expect("create"));
        rename_table(&db, "rejects", "rejects_away");
        let mut store = Store::open(&db).expect("open");
        let mut watch = Watch::new(dir.path(), &quiet_args(), None, None);

        let inbox = Inbox {
            facts: vec![one_fact()],
            rejects: vec![reject()],
        };
        watch
            .absorb_inbox(&mut store, &inbox)
            .expect("facts are stored even though the reject log is broken");
        assert_eq!(
            store.session_ids_for_collector("cc-hooks").expect("read"),
            vec!["sess-basic"]
        );
    }

    #[test]
    fn writes_are_batched_to_one_flush_per_interval() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut clock = FlushClock::default();
        assert!(
            clock.due(start, interval, false),
            "the first flush is immediate"
        );
        clock.flushed(start);
        assert!(!clock.due(start + Duration::from_secs(3), interval, false));
        assert!(clock.due(start + Duration::from_secs(10), interval, false));
    }

    #[test]
    fn something_new_is_flushed_without_waiting() {
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        let mut clock = FlushClock::default();
        clock.flushed(start);
        assert!(
            clock.due(start + Duration::from_secs(1), interval, true),
            "a new session or spool file is written at once"
        );
    }

    #[test]
    fn shutdown_takes_everything_still_dirty() {
        let mut dirty = DirtySessions::default();
        dirty.mark(&set(&["a"]), &set(&["a"]));
        dirty.mark(&set(&["b"]), &BTreeSet::new());
        assert_eq!(dirty.take_all(), set(&["a", "b"]));
        assert!(dirty.take_all().is_empty());
    }

    #[test]
    fn watcher_errors_force_reconciliation_without_losing_dirty_paths() {
        let mut paths = BTreeSet::from([PathBuf::from("spool/a.session.jsonl")]);
        let mut force_reconcile = false;

        absorb_notice(
            FsNotice::Error("backend overflow".to_string()),
            &mut paths,
            &mut force_reconcile,
        );

        assert!(force_reconcile);
        assert_eq!(paths.len(), 1);
    }

    fn quiet_for(secs: u64, now: Instant) -> SessionState {
        SessionState::seen(now - Duration::from_secs(secs))
    }

    /// Nothing in Contract #1 says a session ended, so the known-session
    /// map only ever grew and every entry kept a Python sampler polling.
    /// The retirement rule is inactivity, and it must fire on exactly the
    /// sessions past the timeout and no others.
    #[test]
    fn only_sessions_past_the_idle_timeout_are_retired() {
        let now = Instant::now();
        let timeout = Duration::from_secs(600);
        let sessions = BTreeMap::from([
            ("live".to_string(), quiet_for(5, now)),
            ("quiet".to_string(), quiet_for(599, now)),
            ("idle".to_string(), quiet_for(601, now)),
            ("ancient".to_string(), quiet_for(86_400, now)),
        ]);

        let mut idle = idle_sessions(&sessions, now, timeout);
        idle.sort();
        assert_eq!(idle, vec!["ancient".to_string(), "idle".to_string()]);
    }

    /// The boundary is inclusive, and an empty map retires nothing —
    /// a fresh watch must not decide every session is idle.
    #[test]
    fn the_idle_boundary_is_inclusive_and_an_empty_map_retires_nothing() {
        let now = Instant::now();
        let timeout = Duration::from_secs(600);

        assert!(idle_sessions(&BTreeMap::new(), now, timeout).is_empty());

        let exactly = BTreeMap::from([("s".to_string(), SessionState::seen(now - timeout))]);
        assert_eq!(idle_sessions(&exactly, now, timeout), vec!["s".to_string()]);
    }

    /// A clock that appears to go backwards (a session recorded "after"
    /// now) must not be read as infinitely idle.
    #[test]
    fn a_future_timestamp_is_not_treated_as_idle() {
        let now = Instant::now();
        let sessions = BTreeMap::from([(
            "s".to_string(),
            SessionState::seen(now + Duration::from_secs(10)),
        )]);
        assert!(idle_sessions(&sessions, now, Duration::from_secs(600)).is_empty());
    }

    /// The dedup memories are the two structures in a resident watch that
    /// could grow for the life of the process. Past the horizon the oldest
    /// ids are forgotten — and the *newest* are still deduplicated, which
    /// is the property that matters: a bound that evicted the live window
    /// would re-announce whatever just happened.
    #[test]
    fn the_dedup_memory_forgets_the_oldest_ids_rather_than_growing_forever() {
        let mut set = BoundedSet::new(4);
        for id in ["a", "b", "c", "d"] {
            assert!(set.insert(id), "{id} is new");
        }
        assert!(!set.insert("a"), "still inside the window");

        // One more id pushes the oldest out.
        assert!(set.insert("e"));
        assert_eq!(set.order.len(), 4, "the window never exceeds its capacity");
        assert!(set.insert("a"), "evicted, so it reads as new again");
        assert!(!set.insert("e"), "and the newest is still remembered");
        assert_eq!(set.seen.len(), 4, "the set tracks the queue exactly");
    }

    #[test]
    fn impacts_sum_across_ok_estimates_only() {
        let estimates = vec![
            json!({"estimation_status": "ok", "impacts": {
                "energy": {"unit": "kWh", "total": {"min": 1.0, "max": 2.0}}}}),
            json!({"estimation_status": "ok", "impacts": {
                "energy": {"unit": "kWh", "total": {"min": 0.5, "max": 0.5}}}}),
            json!({"estimation_status": "unknown_model"}),
            json!({"estimation_status": "pending"}),
        ];
        let summed = sum_impacts(&estimates);
        assert_eq!(summed["energy"]["total"]["min"], json!(1.5));
        assert_eq!(summed["energy"]["total"]["max"], json!(2.5));
        assert_eq!(summed["energy"]["unit"], json!("kWh"));
    }

    #[test]
    fn a_criterion_reported_in_two_units_is_dropped_rather_than_summed() {
        let estimates = vec![
            json!({"estimation_status": "ok", "impacts": {
                "energy": {"unit": "kWh", "total": {"min": 1.0, "max": 1.0}},
                "gwp": {"unit": "kgCO2eq", "total": {"min": 2.0, "max": 2.0}}}}),
            json!({"estimation_status": "ok", "impacts": {
                "energy": {"unit": "Wh", "total": {"min": 900.0, "max": 900.0}},
                "gwp": {"unit": "kgCO2eq", "total": {"min": 3.0, "max": 3.0}}}}),
        ];
        let summed = sum_impacts(&estimates);
        assert!(
            summed.get("energy").is_none(),
            "mismatched units must not sum"
        );
        assert_eq!(summed["gwp"]["total"]["min"], json!(5.0));
    }

    #[test]
    fn no_ok_estimates_yields_an_empty_object_not_a_zero_impact() {
        let summed = sum_impacts(&[json!({"estimation_status": "pending"})]);
        assert_eq!(summed, json!({}));
    }

    #[test]
    fn the_respawn_backoff_doubles_and_then_stops_at_the_cap() {
        let secs = |failures| respawn_delay(failures).as_secs();
        // The first death waits the base delay, not zero: a sampler that
        // died on startup will die the same way if respawned immediately.
        assert_eq!(secs(1), 2);
        assert_eq!(secs(2), 4);
        assert_eq!(secs(3), 8);
        assert_eq!(secs(4), 16);
        assert_eq!(secs(5), 32);
        assert_eq!(secs(6), 60, "capped, not 64");
        assert_eq!(secs(30), 60);
        // Saturating, not wrapping: a shift by `failures` would overflow
        // long before this and hand back a *shorter* delay than the step
        // before it, which is the opposite of a backoff.
        assert_eq!(secs(u32::MAX), 60);
        // Defensive: a delay is asked for only after a failure, but zero
        // must still not mean "retry instantly, forever".
        assert_eq!(secs(0), 2);
    }
}
