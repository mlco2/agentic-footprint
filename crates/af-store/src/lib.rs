//! af-store: local SQLite state for agentic-footprint.
//!
//! Owns two disjoint sets of tables, kept independently rebuildable:
//! - **raw**: `raw_events` (append-only, deduplicated Contract #1 events) and
//!   `ingest_offsets` (per-collector/session tail cursors) — see
//!   [`raw`].
//! - **derived**: `impact_estimates` and `impact_joins`, both fully
//!   rebuildable from raw data plus the estimation methodology — see
//!   [`derived`]. [`Store::wipe_derived`] clears these without touching raw
//!   data, so a methodology change can be replayed from scratch.
//!
//! The one deliberate exception is [`Store::llm_calls_without_estimate`]
//! (added for the Task 7 estimator), which LEFT JOINs `raw_events` against
//! `impact_estimates` to find un-estimated `llm_call` events — a read-only
//! query, not a schema coupling, so the "derived is fully rebuildable from
//! raw" invariant still holds.
//!
//! Migrations run on every [`Store::open`]: a `schema_migrations` table
//! tracks the applied version, and each version's `CREATE ... IF NOT
//! EXISTS` statements are safe to re-run. Versions so far: 1 (the tables),
//! 2 (the read-path indexes — see [`SCHEMA_V2`]). Each is applied only when
//! the recorded version is below it, so an existing v1 database is upgraded
//! in place on the next open without touching a row.

mod derived;
mod raw;

pub use derived::ImpactEstimate;
pub use raw::SessionSummary;

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// How long a writer blocks on a lock held by another `af` process before
/// giving up. `af watch` holds the database for the length of one pass
/// while a concurrently launched `af report` wants the same file; without a
/// busy timeout SQLite fails such an overlap *immediately* with
/// `SQLITE_BUSY`, which is a spurious error for a wait that would have been
/// over in milliseconds.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The same, for the read-only handle behind `af statusline`. A render path
/// must never *wait*: five seconds of blocking on a lock held by `af watch`
/// would freeze the status line rather than degrade it. 250ms is long enough
/// to absorb the millisecond-scale overlaps that are the common case and
/// short enough that the worst case is still a redraw, not a stall — and the
/// caller's failure mode is zeros, which is the honest answer for "the
/// database was busy" anyway.
const READ_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// Local SQLite handle. Wraps a single [`rusqlite::Connection`]; raw and
/// derived tables share the connection but are never joined together.
pub struct Store(Connection);

/// This crate's error type: either a SQLite failure or a JSON
/// (de)serialization failure encountered while reading/writing the `json`
/// columns.
#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Error::Json(e) => write!(f, "json error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::Json(e) => Some(e),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Schema version 1: raw tables (`raw_events`, `ingest_offsets`) and derived
/// tables (`impact_estimates`, `impact_joins`). `CREATE TABLE IF NOT
/// EXISTS` makes re-running this idempotent.
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS raw_events (
    event_id   TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    type       TEXT NOT NULL,
    ts         TEXT NOT NULL,
    json       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ingest_offsets (
    collector  TEXT NOT NULL,
    session_id TEXT NOT NULL,
    offset     INTEGER NOT NULL,
    PRIMARY KEY (collector, session_id)
);

CREATE TABLE IF NOT EXISTS impact_estimates (
    event_id             TEXT PRIMARY KEY,
    json                 TEXT NOT NULL,
    methodology_version  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS impact_joins (
    unit_key TEXT PRIMARY KEY,
    json     TEXT NOT NULL
);
";

/// Schema version 2: the indexes behind the three hot read paths. Adds no
/// column and no table, so a v1 database migrates by building them and
/// nothing else — and `IF NOT EXISTS` keeps the statement re-runnable.
///
/// * `raw_events_session` covers [`Store::events_for_session`]'s
///   `WHERE session_id = ? ORDER BY ts, event_id` end to end: without it
///   every session read scans the whole table and then sorts, and that
///   query runs once per session on every `af report`/`af watch` pass.
/// * `raw_events_type` serves the `type = 'llm_call'` /
///   `type = 'session_meta'` filters, which are highly selective — an
///   `llm_call` is a small fraction of a session's events.
/// * `impact_joins_session` is an **expression** index on the same
///   `json_extract` [`Store::joins_for_session`] filters by. The unit key
///   is deliberately not parsed in SQL (see [`derived`]), so the record's
///   own `unit.session_id` is the only thing to index; `json_extract` is
///   deterministic, which is what makes it indexable at all.
const SCHEMA_V2: &str = "
CREATE INDEX IF NOT EXISTS raw_events_session ON raw_events(session_id, ts, event_id);
CREATE INDEX IF NOT EXISTS raw_events_type ON raw_events(type);
CREATE INDEX IF NOT EXISTS impact_joins_session
    ON impact_joins(json_extract(json, '$.unit.session_id'));
";

const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS opaque_events (
    event_id       TEXT PRIMARY KEY,
    schema_version TEXT NOT NULL,
    session_id     TEXT NOT NULL,
    type           TEXT NOT NULL,
    ts             TEXT NOT NULL,
    collector      TEXT NOT NULL,
    json           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS opaque_events_session
    ON opaque_events(session_id, ts, event_id);
";

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);",
    )?;

    let current_version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;

    if current_version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (1)", [])?;
    }

    if current_version < 2 {
        conn.execute_batch(SCHEMA_V2)?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (2)", [])?;
    }

    if current_version < 3 {
        conn.execute_batch(SCHEMA_V3)?;
        conn.execute("INSERT INTO schema_migrations (version) VALUES (3)", [])?;
    }

    Ok(())
}

impl Store {
    /// Opens (creating if absent) the SQLite database at `path` and runs
    /// migrations. The literal path `":memory:"` opens a private in-memory
    /// database instead of a file — used by tests that don't need
    /// persistence across close/reopen.
    ///
    /// **Concurrency.** A file-backed database is opened in **WAL** journal
    /// mode with a 5s busy timeout, because two `af` processes over one
    /// state dir is the normal case, not an edge one: `af watch` is resident
    /// and writing while the user runs `af report` in another terminal.
    /// Under the rollback journal a reader and a writer exclude each other
    /// outright, so that pair fails with `database is locked`; under WAL
    /// they do not, and the busy timeout absorbs the writer-vs-writer
    /// overlap that remains. WAL is **skipped for `:memory:`** — an
    /// in-memory database has no file to write a `-wal` sidecar to, and
    /// SQLite silently keeps it in `memory` journal mode anyway.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            let conn = Connection::open(path)?;
            // `PRAGMA journal_mode` *returns* the resulting mode, so it is a
            // query, not an `execute` — `pragma_update` would fail with
            // "execute returned results". A refusal to switch (a database on
            // a filesystem without shared memory, say) is not fatal: the
            // store still works, just with the old locking behaviour.
            let _: std::result::Result<String, _> =
                conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0));
            conn
        };
        conn.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&conn)?;
        Ok(Store(conn))
    }

    /// Opens an **existing** database without creating it and without
    /// running migrations, for readers that must never mutate the state
    /// dir. Errors if there is no database at `path` — the caller decides
    /// what "no state yet" means (`af statusline` prints zeros).
    ///
    /// Written for `af statusline`, which runs on every status-line refresh
    /// of every Claude Code session: it must be fast, and it must not
    /// create, migrate or lock anything behind a user who happens to have
    /// no state dir yet. Because no migration runs, a query against a table
    /// this binary's schema version expects but the file does not have
    /// fails as a normal error rather than silently upgrading the file.
    ///
    /// `SQLITE_OPEN_READ_ONLY` is the **only** flag used, with no fallback.
    /// A `READ_WRITE` handle — even one without `CREATE` — is a writing
    /// handle: SQLite may recover a hot WAL through it and will checkpoint
    /// on close, so "no statement writes anything" is not the same claim as
    /// "nothing is written". Losing the read is the cheaper failure: the
    /// caller degrades (`af statusline` prints zeros) and the next
    /// `af report`/`af watch`, which legitimately owns the database, does
    /// the WAL recovery.
    ///
    /// The handle gets its own [`READ_BUSY_TIMEOUT`] rather than the
    /// writer's 5s, so a contended read fails fast instead of stalling the
    /// render.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.busy_timeout(READ_BUSY_TIMEOUT)?;
        Ok(Store(conn))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_read_only_refuses_to_create_a_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.db");

        assert!(
            Store::open_read_only(&path).is_err(),
            "a missing database must be an error, not a freshly created one"
        );
        assert!(
            !path.exists(),
            "open_read_only must never create the database file"
        );
    }

    #[test]
    fn open_read_only_reads_an_existing_database_without_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.db");
        {
            let mut store = Store::open(&path).expect("create store");
            store
                .upsert_join(
                    "session:s1",
                    &serde_json::json!({"unit": {"level": "session", "session_id": "s1"}}),
                )
                .expect("seed join");
        }

        let store = Store::open_read_only(&path).expect("open existing database read-only");
        let joins = store.joins_for_session("s1").expect("read joins");
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].0, "session:s1");
    }

    #[test]
    fn the_read_only_handle_fails_fast_instead_of_waiting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.db");
        drop(Store::open(&path).expect("create store"));

        let read = Store::open_read_only(&path).expect("open existing database read-only");
        let read_timeout: i64 = read
            .0
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("query busy_timeout");
        assert_eq!(
            read_timeout,
            READ_BUSY_TIMEOUT.as_millis() as i64,
            "the render path must not inherit the writer's 5s wait"
        );

        // ...while the read/write handle keeps the writer's patience.
        let write = Store::open(&path).expect("reopen store");
        let write_timeout: i64 = write
            .0
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("query busy_timeout");
        assert_eq!(write_timeout, BUSY_TIMEOUT.as_millis() as i64);
    }
}
