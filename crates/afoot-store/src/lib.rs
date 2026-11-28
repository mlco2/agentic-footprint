//! afoot-store: local SQLite state for agentic-footprint.
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
//! 2 (the read-path indexes — see [`SCHEMA_V2`]), 3 (opaque events), 4
//! (the joins' `session_id` column — see [`SCHEMA_V4`]), 5 (the reject log —
//! see [`SCHEMA_V5`]). Each is applied only when
//! the recorded version is below it, so an existing v1 database is upgraded
//! in place on the next open without touching a row.

mod derived;
mod raw;
mod rejects;

pub use derived::ImpactEstimate;
pub use raw::{SessionSummary, TelemetryGap};
pub use rejects::{RejectRow, REJECT_RING};

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// How long a writer blocks on a lock held by another `afoot` process before
/// giving up. `afoot watch` holds the database for the length of one pass
/// while a concurrently launched `afoot report` wants the same file; without a
/// busy timeout SQLite fails such an overlap *immediately* with
/// `SQLITE_BUSY`, which is a spurious error for a wait that would have been
/// over in milliseconds.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The same, for the read-only handle behind `afoot statusline`. A render path
/// must never *wait*: five seconds of blocking on a lock held by `afoot watch`
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
///   query runs once per session on every `afoot report`/`afoot watch` pass.
/// * `raw_events_type` serves the `type = 'llm_call'` /
///   `type = 'session_meta'` filters, which are highly selective — an
///   `llm_call` is a small fraction of a session's events.
/// * `impact_joins_session` was an **expression** index (replaced by a
///   plain column in [`SCHEMA_V4`]) on the same
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

/// Schema version 4: joins carry their session as a real column.
///
/// The v2 expression index made every join upsert re-evaluate
/// `json_extract` over the record and maintain an index entry from it;
/// `upsert_joins` now writes the session id next to the record, so the
/// lookup is a plain column index and the JSON is never parsed by SQLite.
///
/// The column itself is added by [`migrate`] only when missing: `ALTER
/// TABLE ... ADD COLUMN` has no `IF NOT EXISTS`, and a step must be safe
/// to re-run.
const SCHEMA_V4: &str = "
UPDATE impact_joins SET session_id = json_extract(json, '$.unit.session_id');
DROP INDEX IF EXISTS impact_joins_session;
CREATE INDEX IF NOT EXISTS impact_joins_session_id ON impact_joins(session_id, unit_key);
";

/// Schema version 5: the receiver's reject log. Metadata only — a reason,
/// the attribute *names* and a size, never a payload — capped as a ring by
/// [`Store::insert_rejects`].
const SCHEMA_V5: &str = "
CREATE TABLE IF NOT EXISTS rejects (
    id             INTEGER PRIMARY KEY,
    ts             TEXT NOT NULL,
    source         TEXT NOT NULL,
    reason         TEXT NOT NULL,
    attribute_keys TEXT NOT NULL,
    bytes          INTEGER NOT NULL
);
";

/// Best effort `0600`: SQLite creates `-wal`/`-shm` with the database
/// file's mode, so restricting the database first covers files created
/// later too.
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);",
    )?;

    let steps: [(i64, &str); 5] = [
        (1, SCHEMA_V1),
        (2, SCHEMA_V2),
        (3, SCHEMA_V3),
        (4, SCHEMA_V4),
        (5, SCHEMA_V5),
    ];
    for (version, sql) in steps {
        // One write transaction per step, with the version re-read inside
        // it: a step killed half-way rolls back instead of leaving a
        // schema its recorded version does not describe, and two processes
        // opening the same database at once cannot both apply it.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let applied = (|| -> Result<()> {
            let current: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )?;
            if current >= version {
                return Ok(());
            }
            if version == 4 && !has_column(conn, "impact_joins", "session_id")? {
                conn.execute_batch("ALTER TABLE impact_joins ADD COLUMN session_id TEXT;")?;
            }
            conn.execute_batch(sql)?;
            conn.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                [version],
            )?;
            Ok(())
        })();
        match applied {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(err);
            }
        }
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

impl Store {
    /// Opens (creating if absent) the SQLite database at `path` and runs
    /// migrations. The literal path `":memory:"` opens a private in-memory
    /// database instead of a file — used by tests that don't need
    /// persistence across close/reopen.
    ///
    /// **Concurrency.** A file-backed database is opened in **WAL** journal
    /// mode with a 5s busy timeout, because two `afoot` processes over one
    /// state dir is the normal case, not an edge one: `afoot watch` is resident
    /// and writing while the user runs `afoot report` in another terminal.
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
            restrict_to_owner(path);
            // `PRAGMA journal_mode` *returns* the resulting mode, so it is a
            // query, not an `execute` — `pragma_update` would fail with
            // "execute returned results". A refusal to switch (a database on
            // a filesystem without shared memory, say) is not fatal: the
            // store still works, just with the old locking behaviour.
            let _: std::result::Result<String, _> =
                conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0));
            // WAL with `synchronous=NORMAL` is durable across application
            // crashes and loses at most the last commits on power loss. With
            // the default FULL every commit is an fsync, which is what turned
            // a few hundred join upserts per pass into gigabytes a day.
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_owned();
                sidecar.push(suffix);
                restrict_to_owner(Path::new(&sidecar));
            }
            conn
        };
        conn.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&conn)?;
        Ok(Store(conn))
    }

    /// Runs `body` as **one** write transaction: everything it writes
    /// commits together, or — when it returns an error — not at all.
    ///
    /// Each commit writes whole pages to the WAL (and again at checkpoint),
    /// so the number of commits, not the bytes of new data, is what a
    /// resident writer's disk wear follows. The store's own multi-row
    /// writes use savepoints, so they nest inside this. So does this: called
    /// inside another transaction it is a savepoint, and its failure rolls
    /// back only its own writes.
    pub fn in_transaction<T, E>(
        &mut self,
        body: impl FnOnce(&mut Store) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<Error>,
    {
        let (begin, commit, rollback) = if self.0.is_autocommit() {
            ("BEGIN IMMEDIATE", "COMMIT", "ROLLBACK")
        } else {
            (
                "SAVEPOINT afoot_nested",
                "RELEASE afoot_nested",
                "ROLLBACK TO afoot_nested; RELEASE afoot_nested",
            )
        };
        self.0
            .execute_batch(begin)
            .map_err(|err| E::from(Error::from(err)))?;
        match body(self) {
            Ok(value) => match self.0.execute_batch(commit) {
                Ok(()) => Ok(value),
                Err(err) => {
                    // A failed COMMIT may leave the transaction open (SQLite
                    // says "may or may not" for FULL, IOERR, BUSY, NOMEM).
                    // Left open, every later call would nest inside it as a
                    // savepoint and report success while nothing commits.
                    if !self.0.is_autocommit() {
                        let _ = self.0.execute_batch(rollback);
                    }
                    Err(E::from(Error::from(err)))
                }
            },
            Err(err) => {
                let _ = self.0.execute_batch(rollback);
                Err(err)
            }
        }
    }

    /// Opens an **existing** database without creating it and without
    /// running migrations, for readers that must never mutate the state
    /// dir. Errors if there is no database at `path` — the caller decides
    /// what "no state yet" means (`afoot statusline` prints zeros).
    ///
    /// Written for `afoot statusline`, which runs on every status-line refresh
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
    /// caller degrades (`afoot statusline` prints zeros) and the next
    /// `afoot report`/`afoot watch`, which legitimately owns the database, does
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

    #[test]
    fn a_v3_database_gains_the_join_session_column_in_place() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
             INSERT INTO schema_migrations (version) VALUES (1), (2), (3);",
        )
        .expect("seed versions");
        conn.execute_batch(SCHEMA_V1).expect("v1");
        conn.execute_batch(SCHEMA_V2).expect("v2");
        conn.execute_batch(SCHEMA_V3).expect("v3");
        conn.execute(
            "INSERT INTO impact_joins (unit_key, json) VALUES (?1, ?2)",
            rusqlite::params![
                "session:s1",
                r#"{"unit":{"level":"session","session_id":"s1"}}"#
            ],
        )
        .expect("seed join");

        migrate(&conn).expect("migrate to current");

        assert!(
            has_column(&conn, "impact_joins", "session_id").expect("table info"),
            "v4 adds impact_joins.session_id"
        );
        let store = Store(conn);
        let joins = store.joins_for_session("s1").expect("read after migration");
        assert_eq!(joins.len(), 1, "an existing join must still be found");
        assert_eq!(joins[0].0, "session:s1");
    }

    #[test]
    fn an_identical_rewrite_still_fills_a_missing_session_column() {
        // A join an older binary wrote after the v4 migration has a NULL
        // session_id; the identical record from a new rebuild must fill it,
        // or `joins_for_session` never finds that join again.
        let mut store = Store::open(Path::new(":memory:")).expect("open");
        let record = r#"{"unit":{"level":"session","session_id":"s1"}}"#;
        store
            .0
            .execute(
                "INSERT INTO impact_joins (unit_key, json) VALUES ('session:s1', ?1)",
                [record],
            )
            .expect("seed a join without session_id");

        let json: serde_json::Value = serde_json::from_str(record).unwrap();
        store
            .upsert_joins(&[("session:s1".to_string(), json)])
            .expect("rewrite");
        assert_eq!(store.joins_for_session("s1").expect("read").len(), 1);
    }

    #[test]
    fn a_half_applied_v4_migration_recovers_on_the_next_open() {
        // Killed after the ALTER but before version 4 was recorded: the
        // column exists, the version says 3. The next open must not fail
        // with "duplicate column name".
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
             INSERT INTO schema_migrations (version) VALUES (1), (2), (3);",
        )
        .expect("seed versions");
        conn.execute_batch(SCHEMA_V1).expect("v1");
        conn.execute_batch(SCHEMA_V2).expect("v2");
        conn.execute_batch(SCHEMA_V3).expect("v3");
        conn.execute_batch("ALTER TABLE impact_joins ADD COLUMN session_id TEXT;")
            .expect("the half-applied step");

        migrate(&conn).expect("a half-applied migration must recover");
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("version");
        assert_eq!(version, 5);
    }

    #[test]
    fn a_file_store_commits_without_a_full_fsync_per_transaction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("state.db")).expect("open");
        let synchronous: i64 = store
            .0
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("read synchronous");
        assert_eq!(synchronous, 1, "WAL + NORMAL (1), not FULL (2)");
    }

    /// A COMMIT that fails can leave its transaction open. Every later
    /// `in_transaction` would then nest inside it as a savepoint, "succeed",
    /// and never reach the disk.
    #[test]
    fn a_failed_commit_leaves_no_transaction_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = Store::open(&dir.path().join("state.db")).expect("open");
        // A deferred foreign key is checked at COMMIT, and a violation fails
        // the COMMIT while keeping the transaction active.
        store
            .0
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE child (parent INTEGER REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);",
            )
            .expect("schema");
        let failed: Result<()> = store.in_transaction(|store| {
            store.0.execute("INSERT INTO child VALUES (42)", [])?;
            Ok(())
        });
        assert!(failed.is_err(), "the COMMIT must fail");
        assert!(
            store.0.is_autocommit(),
            "the failed transaction is still open"
        );
    }
}
