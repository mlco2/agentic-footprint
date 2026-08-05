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
//! `SIGINT`/`SIGTERM` set a flag the loop checks every [`POLL`]. Shutdown
//! is graceful and ordered: `shutdown` op to every sampler (so its last
//! window reaches the spool), then a final ingest pass to pick that window
//! up, then the servers, then exit 0. A killed sampler would lose the
//! window in flight — that is the difference between a clean stop and a
//! coverage gap.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use af_core::{
    apportion_traced, correlate, rebuild_prepared_stored, Apportionment, EstimationRegion,
    PreparedSession, SampleTrace, SessionTree, Zone,
};
use af_events::{EnergySample, Envelope, Payload, ProcessSample};
use af_sidecar::{doctor, venv_python, Severity, Sidecar};
use af_store::Store;

use super::debug_frames::{
    alloc_trace, attr_decision, ingest_decision, orphan_decision, policy_schema_name, reject_frame,
    span_open_decision, watchdog_entries, Decision,
};
use super::debug_server::{self, DebugState};
use super::estimator;
use super::estimator_worker::Worker as EstimatorWorker;
use super::ingest::{ingest, ingest_paths, IngestFileState, IngestSummary};
use super::{now_ms, sidecar_script};

/// How often the loop wakes to check the shutdown flag. Also the ceiling on
/// how long a `SIGTERM` waits before anything happens.
const POLL: Duration = Duration::from_millis(200);
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

/// The sampler script's path relative to the Python source root
/// (see [`super::sidecar_script`]).
const SAMPLER_SCRIPT: &str = "af_sampler/__main__.py";

/// The methodology artifact this build carries.
///
/// The one there is is the estimator sidecar's ecologits pin, and it is
/// only knowable once a call has been estimated. Until then, say so.
const METHODOLOGY_VERSION: &str = "unknown until the first estimate";

/// How long a `af python doctor` result is reused before it is recomputed.
///
/// `doctor` forks the managed venv's interpreter to ask what it can import,
/// which is the better part of a second, and the health payload it feeds is
/// republished on **every** pass — at least once per [`TICK`]. A resident
/// `af watch` therefore spent most of its wall time forking Python to
/// re-answer a question whose answer changes only when someone runs `af
/// python setup`. A minute is far shorter than that and far longer than a
/// pass.
const PYTHON_HEALTH_TTL: Duration = Duration::from_secs(60);
const SAMPLER_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How many event ids the "already said this" memories keep.
///
/// [`Watch::published_allocs`] and [`Watch::reported_events`] are dedup
/// sets, and an unbounded dedup set in a process designed to stay resident
/// for a working day is a leak with a nice name: one entry per event ever
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
    pub otlp_addr: String,
    pub debug_addr: String,
    pub interval: f64,
    pub local_grid_zone: Option<String>,
    pub remote_region: Option<String>,
}

/// Runs the resident loop until `SIGINT`/`SIGTERM`.
pub fn run(state_dir: &Path, args: WatchArgs) -> Result<()> {
    let spool_dir = state_dir.join("spool");
    std::fs::create_dir_all(&spool_dir)
        .with_context(|| format!("creating spool dir {}", spool_dir.display()))?;

    // Two handlers per signal, in this order, and the order is the feature:
    // the *conditional* one exits only if the flag is already set, so the
    // first Ctrl-C falls through to the flag registration below and runs the
    // graceful path (shutdown ops, final ingest pass), while a **second**
    // Ctrl-C exits immediately with 130. Without the escape hatch a user
    // watching a sampler take its time to flush has nothing to press —
    // graceful shutdown becomes a hang they can only answer with `kill -9`,
    // which is the outcome the graceful path existed to avoid.
    let term = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register_conditional_shutdown(signal, 130, Arc::clone(&term))
            .with_context(|| format!("registering the second-signal escape for {signal}"))?;
        signal_hook::flag::register(signal, Arc::clone(&term))
            .with_context(|| format!("registering signal handler for {signal}"))?;
    }

    // The OTLP receiver is best-effort: a port already in use (a second
    // `af watch`, or Claude Code pointed at someone else's collector) must
    // not cost the user their spool ingestion, which needs no network at
    // all. The failure is reported, loudly, and the loop continues.
    let otlp = if args.no_otlp {
        eprintln!(
            "af watch: OTLP receiver disabled (--no-otlp); llm_call telemetry will not be received"
        );
        None
    } else {
        let addr: SocketAddr = args
            .otlp_addr
            .parse()
            .with_context(|| format!("invalid --otlp-addr {}", args.otlp_addr))?;
        match af_otlp::serve(addr, spool_dir.clone()) {
            Ok(handle) => {
                eprintln!(
                    "af watch: OTLP http/json receiver on http://{} (POST /v1/logs, /v1/metrics)",
                    handle.addr()
                );
                Some(handle)
            }
            Err(err) => {
                eprintln!("af watch: OTLP receiver unavailable ({err:#}); continuing without it");
                None
            }
        }
    };

    let debug_server = if args.debug {
        let addr: SocketAddr = args
            .debug_addr
            .parse()
            .with_context(|| format!("invalid --debug-addr {}", args.debug_addr))?;
        let server = debug_server::serve(addr, Arc::new(Mutex::new(DebugState::default())))
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
    let mut effective_args = WatchArgs {
        debug: args.debug,
        no_sidecars: args.no_sidecars || suppress_duplicate_sidecars,
        no_otlp: args.no_otlp,
        otlp_addr: args.otlp_addr.clone(),
        debug_addr: args.debug_addr.clone(),
        interval: args.interval,
        local_grid_zone: args.local_grid_zone.clone(),
        remote_region: args.remote_region.clone(),
    };
    let mut watch = Watch::new(
        state_dir,
        &effective_args,
        debug_server.as_ref().map(|s| s.state()),
        otlp.as_ref()
            .map(|handle| (handle.addr(), handle.counters())),
    );
    watch.opaque_events = store.count_opaque_events().unwrap_or(0);
    effective_args.no_sidecars = true;

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

    let mut last_pass = Instant::now();
    let mut last_reconcile = Instant::now();
    let mut pending_since: Option<Instant> = None;
    let mut dirty_paths = BTreeSet::new();
    let mut force_reconcile = false;
    while !term.load(Ordering::Relaxed) {
        let estimated_sessions = watch.take_estimator_completion(&store);
        if let Ok(notice) = fs_rx.recv_timeout(POLL) {
            absorb_notice(notice, &mut dirty_paths, &mut force_reconcile);
            pending_since = Some(Instant::now());
        }
        while let Ok(notice) = fs_rx.try_recv() {
            absorb_notice(notice, &mut dirty_paths, &mut force_reconcile);
            pending_since = Some(Instant::now());
        }
        if term.load(Ordering::Relaxed) {
            break;
        }

        let debounced = pending_since
            .map(|at| at.elapsed() >= DEBOUNCE)
            .unwrap_or(false);
        let reconcile_due = force_reconcile || last_reconcile.elapsed() >= RECONCILE;
        let pass_kind = if !estimated_sessions.is_empty() {
            Some(PassKind::EstimatesReady(estimated_sessions))
        } else if reconcile_due {
            dirty_paths.clear();
            force_reconcile = false;
            pending_since = None;
            last_reconcile = Instant::now();
            Some(PassKind::FullReconcile)
        } else if debounced {
            pending_since = None;
            Some(PassKind::Dirty(std::mem::take(&mut dirty_paths)))
        } else if last_pass.elapsed() >= TICK {
            Some(PassKind::Supervise)
        } else {
            None
        };
        if let Some(pass_kind) = pass_kind {
            last_pass = Instant::now();
            if let Err(err) = watch.pass(&mut store, pass_kind) {
                // A failed pass is not a failed watch: the next one may
                // well succeed (a half-written file, a locked db), and
                // exiting would take the OTLP receiver down with it.
                eprintln!("af watch: pass failed: {err:#}");
                force_reconcile = true;
            }
        }
    }

    eprintln!("af watch: shutting down");
    watch.shutdown();
    if let Err(err) = watch.pass(&mut store, PassKind::FullReconcile) {
        eprintln!("af watch: final ingest pass failed: {err:#}");
    }
    // Dropping the server is what stops it — see [`debug_server::DebugServer`].
    drop(debug_server);
    if let Some(handle) = otlp {
        handle.shutdown();
    }
    Ok(())
}

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
    EstimatesReady(BTreeSet<String>),
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
    /// The last `af python doctor` result and when it was taken. See
    /// [`PYTHON_HEALTH_TTL`].
    python_health: Option<(Instant, Vec<Value>)>,
    /// Traces already published, so a re-run of the pipeline over the same
    /// session does not re-emit them.
    published_allocs: BoundedSet,
    /// Decisions already emitted, keyed by the `event_id` they refer to.
    reported_events: BoundedSet,
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
}

struct LoadedSession {
    events: Vec<Envelope>,
    tree: SessionTree,
    apportionment: Apportionment,
    traces: Vec<SampleTrace>,
    samples: Vec<EnergySample>,
    sample_ids: Vec<String>,
    procs: Vec<ProcessSample>,
}

impl LoadedSession {
    fn load(events: Vec<Envelope>) -> Self {
        let tree = correlate(&events);
        let mut sample_ids = Vec::new();
        let mut samples = Vec::new();
        let mut procs = Vec::new();
        for event in &events {
            match &event.payload {
                Payload::EnergySample(sample) => {
                    sample_ids.push(event.event_id.clone());
                    samples.push(sample.clone());
                }
                Payload::ProcessSample(sample) => procs.push(sample.clone()),
                _ => {}
            }
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
            published_allocs: BoundedSet::new(DEDUP_HORIZON),
            reported_events: BoundedSet::new(DEDUP_HORIZON),
            rejects: VecDeque::new(),
            rejects_dropped: 0,
            rejects_total: 0,
            opaque_events: 0,
            rejects_by_collector: BTreeMap::new(),
            spool_state: BTreeMap::new(),
            health_rows: BTreeMap::new(),
            otlp,
        }
    }

    /// One wake's work: ingest, supervise, rebuild, publish.
    fn pass(&mut self, store: &mut Store, kind: PassKind) -> Result<()> {
        if matches!(kind, PassKind::Supervise) {
            self.supervise(&BTreeSet::new());
            return Ok(());
        }
        if let PassKind::EstimatesReady(sessions) = kind {
            return self.rebuild_sessions(store, &sessions, true);
        }
        let summary = match kind {
            PassKind::FullReconcile => ingest(store, &self.state_dir)?,
            PassKind::Dirty(paths) => ingest_paths(store, &self.state_dir, &paths)?,
            PassKind::EstimatesReady(_) | PassKind::Supervise => unreachable!(),
        };
        self.update_spool_state(&summary);
        self.opaque_events = self.opaque_events.saturating_add(summary.opaque as u64);
        self.publish_ingest_metrics(&summary);

        let mut touched: BTreeSet<String> = BTreeSet::new();
        let mut active: BTreeSet<String> = BTreeSet::new();
        let mut zone_may_have_changed = false;
        for event in &summary.events {
            touched.insert(event.session_id.clone());
            if event.collector.name != "codecarbon" {
                active.insert(event.session_id.clone());
            }
            // The only input to zone resolution that a pass can change.
            zone_may_have_changed |= matches!(event.payload, Payload::SessionMeta(_));
            if !self.reported_events.insert(&event.event_id) {
                continue;
            }
            self.decide(&ingest_decision(event));
            if let Ok(value) = serde_json::to_value(event) {
                self.publish("fact", value);
            }
            if let Payload::ActionSpan(span) = &event.payload {
                self.decide(&span_open_decision(event, span));
            }
        }

        for record in &summary.rejects {
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

        if summary.events.is_empty() {
            if !summary.rejects.is_empty() {
                let sessions = summary
                    .rejects
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
        if let Some(estimator) = &self.estimator {
            estimator.request(&zone.id, &zone.source, &self.remote_region);
        }

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
        self.rebuild_sessions(store, &rebuild_sessions, false)
    }

    fn rebuild_sessions(
        &mut self,
        store: &mut Store,
        touched: &BTreeSet<String>,
        estimates_ready: bool,
    ) -> Result<()> {
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
                LoadedSession::load(store.events_for_session(session)?),
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
        if estimates_ready && self.debug {
            eprintln!(
                "af watch: estimator completed; rebuilt {} session(s), {} call(s) still pending",
                touched.len(),
                outcome.pending_llm_calls
            );
        }
        Ok(())
    }

    fn take_estimator_completion(&mut self, store: &Store) -> BTreeSet<String> {
        let Some(estimator) = &self.estimator else {
            return BTreeSet::new();
        };
        let Some(completion) = estimator.take_completion() else {
            return BTreeSet::new();
        };
        let zone_changed = completion.zone.is_some();
        if let Some(zone) = completion.zone {
            self.zone = Some(zone);
        }
        if let Some(error) = completion.error {
            eprintln!("af watch: estimator worker degraded ({error})");
        }
        let mut sessions = completion.sessions;
        if zone_changed {
            sessions.extend(
                store
                    .session_summaries()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|summary| summary.session_id),
            );
        }
        sessions
    }

    fn update_spool_state(&mut self, summary: &IngestSummary) {
        if summary.metrics.full_scan {
            self.spool_state.clear();
        }
        for IngestFileState {
            collector,
            session_id,
            path,
            offset,
        } in &summary.file_states
        {
            self.spool_state.insert(
                (collector.clone(), session_id.clone()),
                (path.clone(), *offset),
            );
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

    fn publish_ingest_metrics(&mut self, summary: &IngestSummary) {
        if !self.debug {
            return;
        }
        let metrics = &summary.metrics;
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
        let root_pid = loaded.tree.root_pids.first().copied();
        for trace in &loaded.traces {
            let Some(sample_event_id) = loaded.sample_ids.get(trace.sample_index) else {
                continue;
            };
            if !self.published_allocs.insert(sample_event_id) {
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

        self.send_watch_op(session_id, &loaded.tree);
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
        }
        if self.sessions.is_empty() {
            if let Some(sampler) = self.sampler.as_mut() {
                let _ = sampler.sidecar.send(&json!({"op": "shutdown"}));
            }
            self.sampler = None;
        }
    }

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
        let args = [
            "--state-dir",
            state_dir.as_str(),
            "--session",
            first_session.as_str(),
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
        if self.debug_state.is_none() {
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
        if self.debug_state.is_none() {
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
        if self.debug_state.is_none() {
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
                        "transport": if name == "otlp-cc" { "POST /v1/logs" } else { "jsonl spool" },
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

        // OTLP losses are only in the receiver's own counters: the
        // reject list this loop maintains is fed by the spool tail, which
        // a quarantined body never reaches.
        let otlp_losses = self
            .otlp
            .as_ref()
            .map(|(_, counters)| counters.logs_dropped() + counters.bodies_quarantined())
            .unwrap_or(0);

        let payload = json!({
            "collectors": collectors,
            "otlp_receiver": self.otlp_health(),
            // The window, plus what it does not show. A truncated list that
            // did not say it was truncated would under-report a spool that
            // is failing wholesale as if it had failed a hundred times.
            "rejected": self.rejects.iter().cloned().collect::<Vec<_>>(),
            // The total is every record this process is known to have lost,
            // not only the spool's share of them. An OTLP body quarantined
            // by the receiver and a record it could not map never touch the
            // spool, so `rejected_total` used to read 0 while `rejected/`
            // filled up on disk. The components stay separately visible
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
        if let Some((taken_at, findings)) = &self.python_health {
            if taken_at.elapsed() < PYTHON_HEALTH_TTL {
                return findings.clone();
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
        self.python_health = Some((Instant::now(), rendered.clone()));
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
            // bodies it wrote to `rejected/`. Both are losses, and neither
            // ever passed through the spool, so neither was visible in any
            // reject count before.
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
            }
            None => {
                health["note"] = json!("no OTLP receiver is running in this af watch process");
            }
        }
        health
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
