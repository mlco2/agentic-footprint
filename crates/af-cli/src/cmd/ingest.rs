//! Ingest pipeline: scan the spool for collector files, tail each from its
//! last recorded offset, insert newly parsed events into the store,
//! quarantine lines that failed to parse, and persist the new offset.
//!
//! Idempotent by construction: a spool file's offset only advances past
//! bytes that were actually consumed, and `Store::insert_events` dedupes on
//! `event_id`, so running `ingest` twice against an unchanged spool ingests
//! and rejects nothing new on the second run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;

use af_events::Envelope;
use af_spool::{quarantine, scan, spool_file_from_path, tail, SpoolFile};
use af_store::Store;

/// One quarantined line, located well enough to be found again by hand.
///
/// `af watch --debug` streams these to the console's Health tab, which
/// renders the reason, the origin file and the offset side by side — the
/// three facts that turn "a line was rejected" into something a developer
/// can act on.
pub struct RejectRecord {
    /// When the line was quarantined (RFC 3339, UTC). The line's own `ts`
    /// is unavailable by definition: it failed to parse.
    pub ts: String,
    pub reason: String,
    /// Spool file name the line came from.
    pub origin: String,
    /// 1-based line number within that file.
    pub line: u64,
    pub byte_offset: u64,
    pub raw: String,
}

/// Outcome of one [`ingest`] call.
pub struct IngestSummary {
    /// Newly inserted events across all spool files (dedup-adjusted).
    pub ingested: usize,
    /// Lines quarantined because they failed to parse.
    pub rejected: usize,
    /// Unknown event types preserved verbatim for audit/replay.
    pub opaque: usize,
    /// Spool files scanned.
    pub files: usize,
    /// The events this pass read off the spool, in file order.
    ///
    /// This is what the tail *consumed*, which is one more than "what the
    /// store gained" whenever an `event_id` was already present — a
    /// collector re-delivering after a truncation is the only way that
    /// happens, since offsets never move backwards otherwise. `ingested`
    /// stays the dedup-adjusted count; this list is what the live debug
    /// stream replays, where showing a re-delivered event is better than
    /// silently dropping it.
    pub events: Vec<Envelope>,
    /// Lines that failed to parse this pass.
    pub rejects: Vec<RejectRecord>,
    /// Internal evidence for spool lifecycle and dirty-path decisions. These
    /// counters are intentionally not a stable public telemetry contract.
    pub metrics: IngestMetrics,
    /// Last known consumed offset for every file touched by this pass. Watch
    /// uses this to render health without rescanning the directory or issuing
    /// another offset query per collector.
    pub file_states: Vec<IngestFileState>,
}

#[derive(Debug, Clone)]
pub struct IngestFileState {
    pub collector: String,
    pub session_id: String,
    pub path: PathBuf,
    pub offset: u64,
}

#[derive(Debug, Clone, Default)]
pub struct IngestMetrics {
    pub full_scan: bool,
    pub dirty_paths: usize,
    pub spool_files_total: usize,
    pub spool_bytes_total: u64,
    pub files_opened: usize,
    pub bytes_read: u64,
    pub complete_lines: u64,
    pub partial_lines: usize,
    pub events_parsed: usize,
    pub events_inserted: usize,
    pub events_deduplicated: usize,
    pub opaque_events_parsed: usize,
    pub opaque_events_inserted: usize,
    pub offset_reads: usize,
    pub offset_writes: usize,
    pub unchanged_offset_writes_skipped: usize,
    pub empty_insert_batches: usize,
    pub discovery_duration: Duration,
    pub tail_duration: Duration,
    pub validation_duration: Duration,
    pub insert_duration: Duration,
    pub offset_duration: Duration,
    pub total_duration: Duration,
}

/// Runs one ingest pass against `state_dir` (`spool/`, `rejected/`
/// beneath it — see [`crate::paths::state_dir`]), inserting into `store`.
///
/// The store is the **caller's**, not one opened here. `af watch` runs this
/// once per pass and then does its own work against the same database; when
/// ingest opened its own connection, every pass paid for a second sqlite
/// handle, a second set of `PRAGMA`s and a second migration check, and the
/// two connections could contend with each other over the same file. The
/// caller owning the connection also makes "one `Store` per process" a
/// property one can see rather than hope for.
pub fn ingest(store: &mut Store, state_dir: &Path) -> Result<IngestSummary> {
    let total_started = Instant::now();
    let spool_dir = state_dir.join("spool");

    let discovery_started = Instant::now();
    let files = scan(&spool_dir);
    let discovery_duration = discovery_started.elapsed();
    ingest_files(
        store,
        state_dir,
        files,
        IngestMetrics {
            full_scan: true,
            discovery_duration,
            ..IngestMetrics::default()
        },
        total_started,
    )
}

/// Ingests only spool files named by filesystem notifications.
///
/// Paths are deduplicated by the caller during debounce. This function still
/// filters them through the canonical spool grammar because notify backends
/// may report directory metadata, rename sources, or unrelated files.
pub fn ingest_paths(
    store: &mut Store,
    state_dir: &Path,
    paths: &std::collections::BTreeSet<PathBuf>,
) -> Result<IngestSummary> {
    let total_started = Instant::now();
    let spool_dir = state_dir.join("spool");
    let discovery_started = Instant::now();
    let files = paths
        .iter()
        .filter_map(|path| spool_file_from_path(&spool_dir, path))
        .collect();
    let discovery_duration = discovery_started.elapsed();
    ingest_files(
        store,
        state_dir,
        files,
        IngestMetrics {
            dirty_paths: paths.len(),
            discovery_duration,
            ..IngestMetrics::default()
        },
        total_started,
    )
}

fn ingest_files(
    store: &mut Store,
    state_dir: &Path,
    files: Vec<SpoolFile>,
    mut metrics: IngestMetrics,
    total_started: Instant,
) -> Result<IngestSummary> {
    let rejected_dir = state_dir.join("rejected");
    let mut ingested = 0usize;
    let mut rejected = 0usize;
    let mut opaque = 0usize;
    let mut events = Vec::new();
    let mut rejects = Vec::new();
    let mut file_states = Vec::with_capacity(files.len());
    metrics.spool_files_total = files.len();

    for file in &files {
        metrics.spool_bytes_total += file.path.metadata().map(|m| m.len()).unwrap_or(0);
        let offset_started = Instant::now();
        let offset = store.get_offset(&file.collector, &file.session_id)?;
        metrics.offset_duration += offset_started.elapsed();
        metrics.offset_reads += 1;

        let tail_started = Instant::now();
        let result = tail(file, offset)?;
        metrics.tail_duration += tail_started.elapsed();
        metrics.validation_duration += result.validation_duration;
        metrics.files_opened += 1;
        metrics.bytes_read += result.bytes_read;
        metrics.complete_lines += result.complete_lines;
        metrics.partial_lines += usize::from(result.partial_line);
        metrics.events_parsed += result.events.len();
        metrics.opaque_events_parsed += result.opaque_events.len();

        if result.events.is_empty() {
            metrics.empty_insert_batches += 1;
        }
        let insert_started = Instant::now();
        let inserted = store.insert_events(&result.events)?;
        metrics.insert_duration += insert_started.elapsed();
        metrics.events_inserted += inserted;
        metrics.events_deduplicated += result.events.len().saturating_sub(inserted);
        ingested += inserted;
        events.extend(result.events);

        let opaque_inserted = store.insert_opaque_events(&result.opaque_events)?;
        metrics.opaque_events_inserted += opaque_inserted;
        opaque += opaque_inserted;

        for entry in &result.rejected {
            quarantine(&rejected_dir, file, &entry.line, &entry.reason);
            rejected += 1;
            rejects.push(RejectRecord {
                ts: crate::cmd::now_rfc3339(),
                reason: entry.reason.to_string(),
                origin: file
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.path.display().to_string()),
                line: entry.line_number,
                byte_offset: entry.byte_offset,
                raw: entry.line.clone(),
            });
        }

        if result.new_offset != offset {
            let offset_started = Instant::now();
            store.set_offset(&file.collector, &file.session_id, result.new_offset)?;
            metrics.offset_duration += offset_started.elapsed();
            metrics.offset_writes += 1;
        } else {
            metrics.unchanged_offset_writes_skipped += 1;
        }
        file_states.push(IngestFileState {
            collector: file.collector.clone(),
            session_id: file.session_id.clone(),
            path: file.path.clone(),
            offset: result.new_offset,
        });
    }

    metrics.total_duration = total_started.elapsed();

    Ok(IngestSummary {
        ingested,
        rejected,
        opaque,
        files: files.len(),
        events,
        rejects,
        metrics,
        file_states,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::env;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    fn event_line(event_id: &str, session_id: &str) -> String {
        format!(
            r#"{{"schema_version":"0.1.0","event_id":"{event_id:-<16}","ts":"2026-07-26T00:00:00.000Z","collector":{{"name":"bench","version":"1"}},"session_id":"{session_id}","type":"session_meta","payload":{{"agent_app":{{"name":"bench"}}}}}}"#
        )
    }

    fn fixture(file_count: usize, events_per_file: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = dir.path().join("spool");
        fs::create_dir_all(&spool).expect("spool dir");
        for file_index in 0..file_count {
            let session = format!("session-{file_index}");
            let path = spool.join(af_spool::spool_file_name("bench", &session));
            let mut file = fs::File::create(path).expect("fixture file");
            for event_index in 0..events_per_file {
                writeln!(
                    file,
                    "{}",
                    event_line(&format!("event-{file_index}-{event_index}"), &session)
                )
                .expect("fixture event");
            }
        }
        dir
    }

    #[derive(Clone, Copy)]
    struct BenchmarkCase {
        name: &'static str,
        files: usize,
        events_per_file: usize,
        appended_files: usize,
    }

    const BENCHMARK_CASES: [BenchmarkCase; 5] = [
        BenchmarkCase {
            name: "small-baseline",
            files: 1,
            events_per_file: 100,
            appended_files: 1,
        },
        BenchmarkCase {
            name: "historical-file-penalty",
            files: 100,
            events_per_file: 100,
            appended_files: 1,
        },
        BenchmarkCase {
            name: "directory-open-scaling",
            files: 1_000,
            events_per_file: 10,
            appended_files: 1,
        },
        BenchmarkCase {
            name: "large-file-tail",
            files: 100,
            events_per_file: 10_000,
            appended_files: 1,
        },
        BenchmarkCase {
            name: "burst-throughput",
            files: 100,
            events_per_file: 100,
            appended_files: 100,
        },
    ];

    fn benchmark_samples() -> usize {
        env::var("AF_INGEST_BENCH_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|samples| *samples > 0)
            .unwrap_or(100)
    }

    fn percentile(durations: &[Duration], percentile: usize) -> Duration {
        let mut durations = durations.to_vec();
        durations.sort_unstable();
        let rank = percentile
            .saturating_mul(durations.len())
            .div_ceil(100)
            .saturating_sub(1)
            .min(durations.len() - 1);
        durations[rank]
    }

    fn append_events(state_dir: &Path, case: BenchmarkCase, sample: usize) -> BTreeSet<PathBuf> {
        let first_file = case.files - case.appended_files;
        (first_file..case.files)
            .map(|file_index| {
                let session = format!("session-{file_index}");
                let path = state_dir
                    .join("spool")
                    .join(af_spool::spool_file_name("bench", &session));
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .expect("append benchmark event");
                writeln!(
                    file,
                    "{}",
                    event_line(&format!("append-{sample}-{file_index}"), &session)
                )
                .expect("write benchmark event");
                path
            })
            .collect()
    }

    fn print_benchmark_result(case: BenchmarkCase, mode: &str, samples: &[IngestMetrics]) {
        let durations: Vec<_> = samples
            .iter()
            .map(|metrics| metrics.total_duration)
            .collect();
        let total_files_opened: usize = samples.iter().map(|metrics| metrics.files_opened).sum();
        let total_bytes_read: u64 = samples.iter().map(|metrics| metrics.bytes_read).sum();
        let total_offset_reads: usize = samples.iter().map(|metrics| metrics.offset_reads).sum();
        let total_offset_writes: usize = samples.iter().map(|metrics| metrics.offset_writes).sum();
        let total_events_inserted: usize =
            samples.iter().map(|metrics| metrics.events_inserted).sum();
        eprintln!(
            "ingest-bench case={} mode={mode} files={} events_per_file={} appended_files={} samples={} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} files_opened_total={total_files_opened} bytes_read_total={total_bytes_read} offset_reads_total={total_offset_reads} offset_writes_total={total_offset_writes} events_inserted_total={total_events_inserted}",
            case.name,
            case.files,
            case.events_per_file,
            case.appended_files,
            samples.len(),
            percentile(&durations, 50).as_secs_f64() * 1_000.0,
            percentile(&durations, 95).as_secs_f64() * 1_000.0,
            percentile(&durations, 99).as_secs_f64() * 1_000.0,
        );
    }

    #[test]
    fn unchanged_pass_skips_empty_transactions_and_offset_writes() {
        let dir = fixture(2, 2);
        let mut store = Store::open(&dir.path().join("state.db")).expect("store");
        let first = ingest(&mut store, dir.path()).expect("first ingest");
        assert_eq!(first.metrics.offset_writes, 2);
        assert_eq!(first.metrics.events_inserted, 4);

        let second = ingest(&mut store, dir.path()).expect("second ingest");
        assert_eq!(second.metrics.empty_insert_batches, 2);
        assert_eq!(second.metrics.offset_writes, 0);
        assert_eq!(second.metrics.unchanged_offset_writes_skipped, 2);
        assert_eq!(second.metrics.bytes_read, 0);
    }

    #[test]
    fn targeted_ingest_opens_only_named_spool_files() {
        let dir = fixture(100, 1);
        let mut store = Store::open(&dir.path().join("state.db")).expect("store");
        ingest(&mut store, dir.path()).expect("initial ingest");

        let dirty = dir
            .path()
            .join("spool")
            .join(af_spool::spool_file_name("bench", "session-99"));
        let mut file = OpenOptions::new()
            .append(true)
            .open(&dirty)
            .expect("append");
        writeln!(file, "{}", event_line("targeted-event", "session-99")).expect("append event");

        let paths = BTreeSet::from([
            dirty,
            dir.path().join("spool"),
            dir.path().join("spool").join("unrelated.tmp"),
        ]);
        let summary = ingest_paths(&mut store, dir.path(), &paths).expect("targeted ingest");
        assert!(!summary.metrics.full_scan);
        assert_eq!(summary.metrics.dirty_paths, 3);
        assert_eq!(summary.metrics.files_opened, 1);
        assert_eq!(summary.metrics.offset_reads, 1);
        assert_eq!(summary.metrics.events_inserted, 1);
    }

    #[test]
    #[ignore = "performance evidence; run with --ignored --nocapture"]
    fn ingest_benchmark_matrix_evidence() {
        let sample_count = benchmark_samples();
        for case in BENCHMARK_CASES {
            let dir = fixture(case.files, case.events_per_file);
            let mut full_scan_store =
                Store::open(&dir.path().join("full-scan.db")).expect("full-scan store");
            let mut targeted_store =
                Store::open(&dir.path().join("targeted.db")).expect("targeted store");
            ingest(&mut full_scan_store, dir.path()).expect("initial full-scan ingest");
            ingest(&mut targeted_store, dir.path()).expect("initial targeted ingest");

            let mut full_scan_samples = Vec::with_capacity(sample_count);
            let mut targeted_samples = Vec::with_capacity(sample_count);
            for sample in 0..sample_count {
                let dirty_paths = append_events(dir.path(), case, sample);
                let (full_scan, targeted) = if sample % 2 == 0 {
                    let full_scan = ingest(&mut full_scan_store, dir.path())
                        .expect("full-scan benchmark ingest");
                    let targeted = ingest_paths(&mut targeted_store, dir.path(), &dirty_paths)
                        .expect("targeted benchmark ingest");
                    (full_scan, targeted)
                } else {
                    let targeted = ingest_paths(&mut targeted_store, dir.path(), &dirty_paths)
                        .expect("targeted benchmark ingest");
                    let full_scan = ingest(&mut full_scan_store, dir.path())
                        .expect("full-scan benchmark ingest");
                    (full_scan, targeted)
                };

                assert_eq!(full_scan.metrics.events_inserted, case.appended_files);
                assert_eq!(targeted.metrics.events_inserted, case.appended_files);
                full_scan_samples.push(full_scan.metrics);
                targeted_samples.push(targeted.metrics);
            }

            print_benchmark_result(case, "full-scan", &full_scan_samples);
            print_benchmark_result(case, "targeted", &targeted_samples);
        }
    }
}
