//! Live-db connections are parked, never closed, while the process runs.
//!
//! POSIX `fcntl` locks belong to the process, and closing ANY descriptor on a
//! file drops every lock the process holds on it. SQLite works around that
//! inside one library build (it defers the close while its own connections
//! hold locks), but the kernel links a second build that cannot see
//! node:sqlite's. So when a kernel connection on the live db closed, the
//! node:sqlite connections in the same process silently lost their db and
//! `-shm` locks, dead-man switch included. Another process then took itself
//! for the only connection: on close it checkpointed and deleted the live
//! WAL while this process kept writing into the unlinked file, and a
//! concurrent writer's WAL could land over a newer db — an index reported
//! `database disk image is malformed` after a daemon auto-sync and a CLI
//! sync overlapped.
//!
//! A parked connection keeps its descriptors open, so the locks survive,
//! and the next resolver on the same file reuses it. It keeps its WAL
//! descriptor too, so it is reused only while the db and its `-wal` are the
//! files they were at park time: once every node:sqlite connection closes,
//! SQLite deletes the WAL, and a connection still reading the old file would
//! miss every later write. Such a connection stays parked, cache released,
//! and is never read again. An idle read-only connection holds no
//! transaction, so it never pins a checkpoint. Snapshot copies are other
//! files and close normally. Windows locks belong to the handle, so there
//! the connection simply closes.

use rusqlite::Connection;

#[cfg(unix)]
mod imp {
    use super::Connection;
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;

    /// A file's (device, inode); `None` when it doesn't exist.
    type FileId = Option<(u64, u64)>;

    struct Parked {
        path: String,
        /// (db, -wal) identities at park time; a gone file is `None`.
        files: (FileId, FileId),
        conn: Connection,
    }

    static PARKED: Mutex<Vec<Parked>> = Mutex::new(Vec::new());

    fn file_id(path: &str) -> FileId {
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }

    fn files(path: &str) -> (FileId, FileId) {
        (file_id(path), file_id(&format!("{path}-wal")))
    }

    pub fn park(path: &str, conn: Connection) {
        // Parked even when the files are gone: closing would drop this
        // process's locks on whatever inode the conn holds.
        let _ = conn.execute_batch("PRAGMA shrink_memory");
        if let Ok(mut parked) = PARKED.lock() {
            parked.push(Parked { path: path.to_string(), files: files(path), conn });
        }
    }

    /// A parked connection whose db and `-wal` are still the files `path`
    /// names now. Stale entries stay parked (see the module comment).
    pub fn take(path: &str) -> Option<Connection> {
        let now = files(path);
        now.0?;
        let mut parked = PARKED.lock().ok()?;
        let i = parked.iter().position(|p| p.path == path && p.files == now)?;
        Some(parked.swap_remove(i).conn)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::Connection;
    pub fn park(_path: &str, _conn: Connection) {}
    pub fn take(_path: &str) -> Option<Connection> {
        None
    }
}

pub(super) use imp::{park, take};
