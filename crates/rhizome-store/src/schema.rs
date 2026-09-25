//! The database layout and how it evolves.
//!
//! The schema version lives in SQLite's `user_version` pragma. Opening a
//! database migrates it forward one version at a time, each step in its own
//! transaction so a failed migration leaves the previous version intact. A
//! database from a *newer* Rhizome is refused outright: writing to a schema we
//! do not understand could corrupt it.

use rusqlite::Connection;

use crate::error::{Error, Result};

/// The schema version this build reads and writes.
pub(crate) const CURRENT_VERSION: i64 = 1;

/// Version 1.
///
/// `messages_fts` is an *external content* FTS5 table: it stores only the
/// index, not a second copy of every message, and reads the text back from
/// `messages` when it needs a snippet. The triggers keep it in step.
///
/// The tokenizer is `unicode61` with `remove_diacritics 2`, which folds
/// `ş`→`s`, `ç`→`c`, `ğ`→`g`, `ü`→`u` and `ö`→`o`, so a search typed without
/// Turkish characters still finds the accented word. `tokenchars '_'` keeps
/// `snake_case` identifiers whole, since these are logs of developer channels
/// and a search for `kmalloc_array` should not also match every message that
/// mentions `array`.
const V1: &str = r#"
CREATE TABLE buffers (
    id      INTEGER PRIMARY KEY,
    network TEXT NOT NULL,
    key     TEXT NOT NULL,
    name    TEXT NOT NULL,
    UNIQUE (network, key)
);

CREATE TABLE messages (
    id        INTEGER PRIMARY KEY,
    buffer_id INTEGER NOT NULL REFERENCES buffers(id) ON DELETE CASCADE,
    time_ms   INTEGER NOT NULL,
    sender    TEXT NOT NULL,
    kind      INTEGER NOT NULL,
    text      TEXT NOT NULL,
    plain     TEXT NOT NULL,
    own       INTEGER NOT NULL,
    highlight INTEGER NOT NULL,
    msgid     TEXT
);

CREATE INDEX messages_by_buffer_time ON messages (buffer_id, time_ms, id);
CREATE INDEX messages_by_sender ON messages (sender COLLATE NOCASE, time_ms);
CREATE UNIQUE INDEX messages_by_msgid ON messages (buffer_id, msgid)
    WHERE msgid IS NOT NULL;

CREATE VIRTUAL TABLE messages_fts USING fts5(
    plain,
    content = 'messages',
    content_rowid = 'id',
    tokenize = "unicode61 remove_diacritics 2 tokenchars '_'"
);

CREATE TRIGGER messages_after_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, plain) VALUES (new.id, new.plain);
END;

CREATE TRIGGER messages_after_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, plain)
        VALUES ('delete', old.id, old.plain);
END;
"#;

/// Brings a freshly opened connection up to date.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let found: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > CURRENT_VERSION {
        return Err(Error::TooNew {
            found,
            supported: CURRENT_VERSION,
        });
    }

    for version in found..CURRENT_VERSION {
        let tx = conn.transaction()?;
        match version {
            0 => tx.execute_batch(V1)?,
            other => unreachable!("no migration defined from schema version {other}"),
        }
        // `PRAGMA` does not take bound parameters, and the value is a constant
        // we control.
        tx.execute_batch(&format!("PRAGMA user_version = {}", version + 1))?;
        tx.commit()?;
    }
    Ok(())
}
