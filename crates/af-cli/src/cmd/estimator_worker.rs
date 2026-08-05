use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use af_core::{estimate_events, fetch_zone_factors, EstimationOutcome, EstimationRegion, Zone};
use af_sidecar::Sidecar;
use af_store::Store;

const ESTIMATE_BATCH: usize = 1;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Config {
    zone_id: String,
    zone_source: String,
    remote_region: EstimationRegion,
}

#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub sessions: BTreeSet<String>,
    pub zone: Option<Zone>,
    pub outcome: EstimationOutcome,
    pub pending: usize,
    pub error: Option<String>,
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
    completion_rx: Receiver<()>,
    config: Arc<Mutex<Option<Config>>>,
    completion: Arc<Mutex<Completion>>,
    health: Arc<Mutex<Health>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(state_dir: &Path, mut sidecar: Sidecar) -> anyhow::Result<Self> {
        sidecar.set_timeout(REQUEST_TIMEOUT);
        let db_path = state_dir.join("state.db");
        let mut store = Store::open(&db_path)?;
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let worker_wake = wake_tx.clone();
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let config: Arc<Mutex<Option<Config>>> = Arc::new(Mutex::new(None));
        let completion = Arc::new(Mutex::new(Completion::default()));
        let health = Arc::new(Mutex::new(Health::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let worker_config = Arc::clone(&config);
        let worker_completion = Arc::clone(&completion);
        let worker_health = Arc::clone(&health);
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("af-estimator".to_string())
            .spawn(move || {
                let mut resolved_zone: Option<String> = None;
                while wake_rx.recv().is_ok() && !worker_stop.load(Ordering::Relaxed) {
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

                    let events = match store.llm_calls_without_estimate_limit(Some(ESTIMATE_BATCH))
                    {
                        Ok(events) => events,
                        Err(error) => {
                            update.error = Some(format!("pending query: {error:#}"));
                            Vec::new()
                        }
                    };
                    update
                        .sessions
                        .extend(events.iter().map(|event| event.session_id.clone()));

                    if update.error.is_none() && !events.is_empty() {
                        match estimate_events(
                            &mut store,
                            &mut sidecar,
                            &config.remote_region,
                            &events,
                        ) {
                            Ok(outcome) => update.outcome = outcome,
                            Err(error) => update.error = Some(format!("estimation: {error:#}")),
                        }
                    }
                    update.pending = store.count_llm_calls_without_estimate().unwrap_or(0) as usize;

                    merge_completion(&worker_completion, &update);
                    update_health(&worker_health, &update);
                    let _ = completion_tx.try_send(());

                    if update.error.is_none()
                        && update.pending > 0
                        && !worker_stop.load(Ordering::Relaxed)
                    {
                        let _ = worker_wake.try_send(());
                    }
                }
                set_state(&worker_health, "stopped");
            })?;

        Ok(Self {
            wake_tx,
            completion_rx,
            config,
            completion,
            health,
            stop,
            thread: Some(thread),
        })
    }

    pub fn request(&self, zone_id: &str, zone_source: &str, remote_region: &EstimationRegion) {
        if let Ok(mut config) = self.config.lock() {
            *config = Some(Config {
                zone_id: zone_id.to_string(),
                zone_source: zone_source.to_string(),
                remote_region: remote_region.clone(),
            });
        }
        match self.wake_tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => {}
            Err(TrySendError::Disconnected(())) => set_state(&self.health, "stopped"),
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

fn merge_completion(target: &Arc<Mutex<Completion>>, update: &Completion) {
    let Ok(mut target) = target.lock() else {
        return;
    };
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
