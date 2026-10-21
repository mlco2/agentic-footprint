//! Helpers shared by unit tests across `cmd`.

use std::fs;
use std::path::Path;

/// Commits recorded in a WAL file: each commit's last frame carries the
/// database size after the commit (non-zero), every other frame zero.
pub(crate) fn wal_commits(db: &Path) -> usize {
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    let Ok(bytes) = fs::read(&wal) else {
        return 0;
    };
    if bytes.len() < 32 {
        return 0;
    }
    let page_size = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut commits = 0;
    let mut at = 32;
    while at + 24 + page_size <= bytes.len() {
        if u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap()) != 0 {
            commits += 1;
        }
        at += 24 + page_size;
    }
    commits
}
