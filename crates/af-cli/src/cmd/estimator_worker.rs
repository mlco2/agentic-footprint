use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use af_core::{
    compute_estimates, estimator_methodology, fetch_zone_factors, EstimationOutcome,
    EstimationRegion, Zone,
};
use af_sidecar::Sidecar;
use af_store::{ImpactEstimate, Store};

/// Calls estimated per wake before the worker yields its results.
const ESTIMATE_BATCH: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// First wait before a failed wake is tried again on its own, doubled per
/// consecutive failure up to [`RETRY_MAX`]. Nothing else wakes the worker
/// but a new call, and at the end of a session there is none.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// The calls waiting for the sidecar, each once.
///
/// A call can be handed over again — an exporter retry, a truncated spool
/// replayed — while it is still queued, or after its row went back to the
/// loop but before the loop's next flush stored it. The database still
/// calls it pending then, so only the worker can tell it is not new.
#[derive(Default)]
struct WorkQueue {
    order: VecDeque<String>,
    queued: HashSet<String>,
    /// Rows handed to the loop and not yet acknowledged as stored.
    unstored: HashSet<String>,
}

impl WorkQueue {
    fn len(&self) -> usize {
        self.order.len()
    }

    fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    fn extend(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if !self.unstored.contains(&id) && self.queued.insert(id.clone()) {
                self.order.push_back(id);
            }
        }
    }

    /// Replaces the queue with the stored backlog, minus rows the loop has
    /// yet to store.
    fn reseed(&mut self, backlog: impl IntoIterator<Item = String>) {
        self.order.clear();
        self.queued.clear();
        self.extend(backlog);
    }

    fn take(&mut self, n: usize) -> Vec<String> {
        let n = n.min(self.order.len());
        let ids: Vec<String> = self.order.drain(..n).collect();
        for id in &ids {
            self.queued.remove(id);
        }
        ids
    }

    /// Puts back, in order and ahead of the rest, calls a failed attempt
    /// did not finish.
    fn requeue_front(&mut self, ids: Vec<String>) {
        for id in ids.into_iter().rev() {
            if !self.unstored.contains(&id) && self.queued.insert(id.clone()) {
                self.order.push_front(id);
            }
        }
    }

    fn computed(&mut self, ids: impl IntoIterator<Item = String>) {
        self.unstored.extend(ids);
    }

    fn acknowledge(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            self.unstored.remove(&id);
        }
    }
}

#[derive(Clone, PartialEq)]
struct Config {
    zone_id: String,
    zone_source: String,
    remote_region: EstimationRegion,
}

/// What the worker produced since the loop last asked. The worker never
/// writes: `rows` are for the loop that owns the database to store in its
/// next batched commit, after which `sessions` need a rebuild.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub sessions: BTreeSet<String>,
    pub zone: Option<Zone>,
    pub outcome: EstimationOutcome,
    pub pending: usize,
    pub error: Option<String>,
    pub rows: Vec<ImpactEstimate>,
}

#[derive(Debug, Clone)]
pub struct Health {
    pub state: &'static str,
    pub pending: usize,
    pub processed: usize,
    pub failures: usize,
    pub last_error: Option<String>,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            state: "idle",
            pending: 0,
            processed: 0,
            failures: 0,
            last_error: None,
        }
    }
}

pub struct Worker {
    wake_tx: SyncSender<()>,
    /// Event ids of new `llm_call`s, handed over by [`Worker::request`].
    arrivals: Arc<Mutex<Vec<String>>>,
    /// Event ids whose rows the loop has stored, from [`Worker::stored`].
    stored: Arc<Mutex<Vec<String>>>,
    completion_rx: Receiver<()>,
    config: Arc<Mutex<Option<Config>>>,
    completion: Arc<Mutex<Completion>>,
    health: Arc<Mutex<Health>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// The work queue lives in this thread: on its first wake it reads the
    /// stored backlog's ids once (calls never estimated, and verdicts a
    /// newer methodology should revisit); after that it only learns of
    /// calls the loop hands it. An idle queue costs no query at all, where
    /// asking the database "what is pending?" scanned every call ever made.
    pub fn spawn(state_dir: &Path, mut sidecar: Sidecar) -> anyhow::Result<Self> {
        sidecar.set_timeout(REQUEST_TIMEOUT);
        let db_path = state_dir.join("state.db");
        let store = Store::open(&db_path)?;
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let worker_wake = wake_tx.clone();
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let arrivals: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stored: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let completion = Arc::new(Mutex::new(Completion::default()));
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker_arrivals = Arc::clone(&arrivals);
        let worker_stored = Arc::clone(&stored);
        let worker_config = Arc::clone(&config);
        let worker_completion = Arc::clone(&completion);
        let worker_health = Arc::clone(&health);
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("af-estimator".to_string())
            .spawn(move || {
                let mut resolved_zone: Option<String> = None;
                // Fixed for the life of this sidecar process.
                let methodology = estimator_methodology(&mut sidecar);
                let mut queue = WorkQueue::default();
                let mut seeded = false;
                let mut retry_after: Option<Duration> = None;
                loop {
                    let woke = match retry_after {
                        None => wake_rx.recv().is_ok(),
                        Some(wait) => !matches!(
                            wake_rx.recv_timeout(wait),
                            Err(RecvTimeoutError::Disconnected)
                        ),
                    };
                    if !woke || worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(config) = worker_config.lock().ok().and_then(|value| value.clone())
                    else {
                        continue;
                    };
                    set_state(&worker_health, "estimating");
                    let mut update = Completion::default();

                    if resolved_zone.as_deref() != Some(&config.zone_id) {
                        let mut zone = Zone::unresolved(&config.zone_id, &config.zone_source);
                        match fetch_zone_factors(&mut sidecar, &config.zone_id) {
                            Ok(factors) => zone.factors = factors,
                            Err(error) => update.error = Some(format!("zone factors: {error:#}")),
                        }
                        if update.error.is_none() {
                            resolved_zone = Some(config.zone_id.clone());
                        }
                        update.zone = Some(zone);
                    }

                    if let Ok(mut stored) = worker_stored.lock() {
                        queue.acknowledge(stored.drain(..));
                    }
                    if let Ok(mut arrived) = worker_arrivals.lock() {
                        queue.extend(arrived.drain(..));
                    }
                    if !seeded {
                        match store.llm_call_ids_to_estimate(methodology.as_deref()) {
                            Ok(backlog) => {
                                // Anything that also just arrived is in the
                                // backlog already.
                                queue.reseed(backlog);
                                seeded = true;
                            }
                            Err(error) => {
                                update.error = Some(format!("pending query: {error:#}"));
                            }
                        }
                    }

                    if update.error.is_none() {
                        estimate_batch(
                            &store,
                            &mut sidecar,
                            &config.remote_region,
                            methodology.as_deref(),
                            &mut queue,
                            &mut update,
                        );
                    }
                    update.pending = queue.len();
                    let healthy = update.error.is_none();

                    update_health(&worker_health, &update);
                    merge_completion(&worker_completion, update);
                    let _ = completion_tx.try_send(());

                    if healthy {
                        retry_after = None;
                        if !queue.is_empty() && !worker_stop.load(Ordering::Relaxed) {
                            let _ = worker_wake.try_send(());
                        }
                    } else {
                        retry_after =
                            Some(retry_after.map_or(RETRY_FIRST, |wait| (wait * 2).min(RETRY_MAX)));
                    }
                }
                set_state(&worker_health, "stopped");
            })?;

        Ok(Self {
            wake_tx,
            arrivals,
            stored,
            completion_rx,
            config,
            completion,
            health,
            stop,
            thread: Some(thread),
        })
    }

    /// Hands the worker the event ids of newly stored `llm_call`s and the
    /// zone to estimate under. Wakes it only when there is something to do:
    /// new calls, a zone or region it has not been given yet, or the first
    /// request (which reads the stored backlog).
    pub fn request(
        &self,
        zone_id: &str,
        zone_source: &str,
        remote_region: &EstimationRegion,
        new_calls: Vec<String>,
    ) {
        let next = Config {
            zone_id: zone_id.to_string(),
            zone_source: zone_source.to_string(),
            remote_region: remote_region.clone(),
        };
        let config_changed = match self.config.lock() {
            Ok(mut config) => {
                let changed = config.as_ref() != Some(&next);
                *config = Some(next);
                changed
            }
            Err(_) => false,
        };
        let has_calls = !new_calls.is_empty();
        if has_calls {
            if let Ok(mut arrivals) = self.arrivals.lock() {
                arrivals.extend(new_calls);
            }
        }
        if !has_calls && !config_changed {
            return;
        }
        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {}
            Err(TrySendError::Disconnected(())) => set_state(&self.health, "stopped"),
        }
    }

    /// Tells the worker these rows are in the database, so a call handed
    /// again from now on is judged by what is stored. Needs no wake.
    pub fn stored(&self, event_ids: impl IntoIterator<Item = String>) {
        if let Ok(mut stored) = self.stored.lock() {
            stored.extend(event_ids);
        }
    }

    pub fn take_completion(&self) -> Option<Completion> {
        self.completion_rx.try_recv().ok()?;
        let mut completion = self.completion.lock().ok()?;
        Some(std::mem::take(&mut *completion))
    }

    pub fn health(&self) -> Health {
        self.health
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake_tx.try_send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Estimates up to [`ESTIMATE_BATCH`] queued calls into `update`. A call
/// estimated since it was queued (by `af report`, or queued twice) is
/// skipped. On a sidecar failure the calls it did not reach go back to the
/// front of the queue for the next wake.
fn estimate_batch(
    store: &Store,
    sidecar: &mut Sidecar,
    region: &EstimationRegion,
    methodology: Option<&str>,
    queue: &mut WorkQueue,
    update: &mut Completion,
) {
    let ids = queue.take(ESTIMATE_BATCH);
    let events = match store.llm_calls_pending_among(&ids, methodology) {
        Ok(events) => events,
        Err(error) => {
            update.error = Some(format!("pending query: {error:#}"));
            queue.requeue_front(ids);
            return;
        }
    };
    if events.is_empty() {
        return;
    }
    let computed = compute_estimates(sidecar, region, &events, methodology);
    let done: BTreeSet<&str> = computed
        .rows
        .iter()
        .map(|row| row.event_id.as_str())
        .collect();
    for event in &events {
        if done.contains(event.event_id.as_str()) {
            update.sessions.insert(event.session_id.clone());
        }
    }
    if let Some(error) = &computed.error {
        update.error = Some(format!("estimation: {error:#}"));
        queue.requeue_front(
            events
                .iter()
                .filter(|event| !done.contains(event.event_id.as_str()))
                .map(|event| event.event_id.clone())
                .collect(),
        );
    }
    queue.computed(done.iter().map(|id| id.to_string()));
    update.outcome = computed.outcome;
    update.rows = computed.rows;
}

fn merge_completion(target: &Arc<Mutex<Completion>>, update: Completion) {
    let Ok(mut target) = target.lock() else {
        return;
    };
    target.rows.extend(update.rows);
    target.sessions.extend(update.sessions.iter().cloned());
    if update.zone.is_some() {
        target.zone = update.zone.clone();
    }
    target.outcome.estimated += update.outcome.estimated;
    target.outcome.unknown_model += update.outcome.unknown_model;
    target.outcome.missing_zone += update.outcome.missing_zone;
    target.outcome.missing_usage += update.outcome.missing_usage;
    target.outcome.errors += update.outcome.errors;
    target.pending = update.pending;
    if update.error.is_some() {
        target.error = update.error.clone();
    }
}

fn update_health(health: &Arc<Mutex<Health>>, update: &Completion) {
    let Ok(mut health) = health.lock() else {
        return;
    };
    health.pending = update.pending;
    health.processed += update.outcome.processed();
    if let Some(error) = &update.error {
        health.state = "degraded";
        health.failures += 1;
        health.last_error = Some(error.clone());
    } else {
        health.state = "idle";
        health.last_error = None;
    }
}

fn set_state(health: &Arc<Mutex<Health>>, state: &'static str) {
    if let Ok(mut health) = health.lock() {
        health.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_events::{fixtures, Envelope, Payload};
    use std::time::Instant;

    fn fake_sidecar() -> Sidecar {
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/fake_sidecar.py"
        );
        Sidecar::spawn(Path::new("python3"), script, &[]).expect("spawn fake_sidecar.py")
    }

    fn llm_call(event_id: &str) -> Envelope {
        fixtures::envelope(
            event_id,
            "session-1",
            "2026-07-25T12:00:00Z",
            Payload::LlmCall(fixtures::llm_call()),
        )
    }

    /// Everything the worker reports until it has produced `rows` rows.
    fn rows_until(worker: &Worker, rows: usize) -> Completion {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut all = Completion::default();
        while all.rows.len() < rows && Instant::now() < deadline {
            match worker.take_completion() {
                Some(completion) => {
                    all.rows.extend(completion.rows);
                    all.sessions.extend(completion.sessions);
                    all.pending = completion.pending;
                    all.error = all.error.or(completion.error);
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        all
    }

    #[test]
    fn the_worker_estimates_the_backlog_and_new_calls_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        store
            .insert_events(&[llm_call("evt-old"), llm_call("evt-new")])
            .expect("seed calls");
        let worker = Worker::spawn(dir.path(), fake_sidecar()).expect("spawn worker");
        let region = EstimationRegion::explicit("WOR", "test");

        worker.request("WOR", "test", &region, vec!["evt-new".to_string()]);
        let done = rows_until(&worker, 2);

        assert!(done.error.is_none(), "{:?}", done.error);
        let mut ids: Vec<&str> = done.rows.iter().map(|row| row.event_id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            ["evt-new", "evt-old"],
            "the backlog and the new call, once each"
        );
        assert_eq!(done.sessions, BTreeSet::from(["session-1".to_string()]));
        assert_eq!(done.pending, 0);
        assert_eq!(
            store.count_llm_calls_without_estimate().expect("count"),
            2,
            "the worker leaves writing to the loop"
        );
    }

    fn rename_table(db: &Path, from: &str, to: &str) {
        rusqlite::Connection::open(db)
            .expect("open db")
            .execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}"))
            .expect("rename");
    }

    /// The worker is woken by new calls. One that failed must not wait for
    /// the next call to try again: at the end of a session there is none.
    #[test]
    fn a_failed_wake_is_retried_without_a_new_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("state.db");
        let mut store = Store::open(&db).expect("open");
        store
            .insert_events(&[llm_call("evt-a")])
            .expect("seed call");
        let worker = Worker::spawn(dir.path(), fake_sidecar()).expect("spawn worker");
        rename_table(&db, "impact_estimates", "impact_estimates_away");
        let region = EstimationRegion::explicit("WOR", "test");
        worker.request("WOR", "test", &region, Vec::new());
        let deadline = Instant::now() + Duration::from_secs(20);
        let failed = loop {
            if let Some(completion) = worker.take_completion() {
                break completion;
            }
            assert!(Instant::now() < deadline, "the worker never answered");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(failed.error.is_some(), "the backlog query must have failed");

        rename_table(&db, "impact_estimates_away", "impact_estimates");
        let done = rows_until(&worker, 1);
        assert_eq!(done.rows.len(), 1, "retried on its own: {:?}", done.error);
    }

    /// An exporter retry or a replay can hand a call again while its row
    /// still waits in the loop's buffer for the next flush: the database
    /// still calls it pending, but it must not be estimated twice.
    #[test]
    fn a_call_handed_again_before_its_row_is_stored_is_not_estimated_twice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        store
            .insert_events(&[llm_call("evt-a")])
            .expect("seed call");
        let worker = Worker::spawn(dir.path(), fake_sidecar()).expect("spawn worker");
        let region = EstimationRegion::explicit("WOR", "test");
        worker.request("WOR", "test", &region, vec!["evt-a".to_string()]);
        assert_eq!(rows_until(&worker, 1).rows.len(), 1);

        worker.request("WOR", "test", &region, vec!["evt-a".to_string()]);
        let deadline = Instant::now() + Duration::from_secs(20);
        let again = loop {
            if let Some(completion) = worker.take_completion() {
                break completion;
            }
            assert!(Instant::now() < deadline, "the worker never answered");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(again.rows.is_empty(), "estimated twice: {:?}", again.rows);
        assert_eq!(again.pending, 0);
    }

    #[test]
    fn the_queue_holds_each_call_once_until_its_row_is_stored() {
        let mut queue = WorkQueue::default();
        queue.extend(["a", "b", "a"].map(String::from));
        assert_eq!(queue.len(), 2, "a duplicate arrival is one call");
        let taken = queue.take(10);
        assert_eq!(taken, ["a", "b"]);
        queue.computed(["a".to_string()]);
        queue.requeue_front(vec!["b".to_string()]);
        queue.extend(["a", "b"].map(String::from));
        assert_eq!(queue.len(), 1, "a awaits storage, b is queued already");
        queue.reseed(["a", "b", "c"].map(String::from));
        assert_eq!(
            queue.take(10),
            ["b", "c"],
            "the backlog skips rows awaiting storage"
        );
        queue.acknowledge(["a".to_string()]);
        queue.extend(["a".to_string()]);
        assert_eq!(queue.len(), 1, "stored: the database decides from here");
    }

    #[test]
    fn a_call_already_estimated_is_not_asked_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        store
            .insert_events(&[llm_call("evt-a")])
            .expect("seed call");
        let worker = Worker::spawn(dir.path(), fake_sidecar()).expect("spawn worker");
        let region = EstimationRegion::explicit("WOR", "test");
        worker.request("WOR", "test", &region, Vec::new());
        let first = rows_until(&worker, 1);
        assert_eq!(first.rows.len(), 1);
        for row in &first.rows {
            store.upsert_estimate(row).expect("store as the loop would");
        }

        // Handed again (an exporter retry): the worker wakes, finds it
        // estimated, and answers with nothing.
        worker.request("WOR", "test", &region, vec!["evt-a".to_string()]);
        let deadline = Instant::now() + Duration::from_secs(20);
        let again = loop {
            if let Some(completion) = worker.take_completion() {
                break completion;
            }
            assert!(Instant::now() < deadline, "the worker never answered");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(again.rows.is_empty(), "{:?}", again.rows);
    }
}
