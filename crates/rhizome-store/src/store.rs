//! The store itself.

use std::path::Path;

use rhizome_proto::{format, CaseMapping};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, ToSql};

use crate::error::Result;
use crate::query;
use crate::schema;
use crate::time::parse_server_time;
use crate::types::{
    BufferInfo, Cursor, Kind, NewMessage, SearchHit, SearchOptions, SearchOrder, StoredMessage,
    MARK_END, MARK_START,
};

/// The columns every message query selects, in the order [`read_message`]
/// expects them. Queries alias `messages` as `m` and `buffers` as `b`.
const MESSAGE_COLUMNS: &str =
    "m.id, b.network, b.name, m.time_ms, m.sender, m.kind, m.text, m.own, m.highlight, m.msgid";

/// How many characters of a message to show as the snippet of a result that
/// has no matched words to centre on.
const PLAIN_SNIPPET_CHARS: usize = 160;

/// Buffers are matched case-insensitively, under RFC 1459 rules, which is what
/// nearly every network uses. A network with strict ASCII casemapping would
/// treat `[` and `{` as different, so two such buffers could be merged here;
/// that needs a nick or channel differing only in those characters and is the
/// price of the store not needing to know each network's `ISUPPORT`.
fn buffer_key(name: &str) -> String {
    CaseMapping::default().fold(name)
}

/// The message log.
///
/// One `Store` owns one SQLite connection. It is `Send` but not `Sync`, so
/// give it a thread of its own and talk to it over a channel.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the log at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        Store::init(Connection::open(path)?, true)
    }

    /// A log that lives only in memory, for tests and previews.
    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?, false)
    }

    fn init(mut conn: Connection, file_backed: bool) -> Result<Store> {
        // Wait for a competing writer instead of failing at once; a second
        // window or a backup tool may briefly hold the file.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        if file_backed {
            // Write-ahead logging lets a reader (the search box) run while the
            // connection is being written to, and makes each commit cheap.
            let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
            // In WAL mode NORMAL cannot corrupt the database on a crash; it can
            // lose the last few commits on power loss, which for a chat log is
            // the right trade against a sync on every message.
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        schema::migrate(&mut conn)?;
        Ok(Store { conn })
    }

    // ---- writing ---------------------------------------------------------

    /// Records a message.
    ///
    /// Returns its id, or `None` if it was already stored: a message with the
    /// same `msgid` in the same buffer is a duplicate, so replaying history
    /// that overlaps the log adds nothing.
    pub fn log_message(&mut self, message: &NewMessage) -> Result<Option<i64>> {
        let tx = self.conn.transaction()?;
        let id = insert(&tx, message)?;
        tx.commit()?;
        Ok(id)
    }

    /// Records many messages in one transaction, which is far faster than one
    /// at a time and the right way to load a history replay. Returns how many
    /// were new.
    pub fn log_messages(&mut self, messages: &[NewMessage]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut added = 0;
        for message in messages {
            if insert(&tx, message)?.is_some() {
                added += 1;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Deletes a buffer and every message in it, and removes them from the
    /// search index. Returns how many messages were removed.
    pub fn delete_buffer(&mut self, network: &str, buffer: &str) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let removed: i64 = tx.query_row(
            "SELECT count(*) FROM messages m JOIN buffers b ON b.id = m.buffer_id
             WHERE b.network = ?1 AND b.key = ?2",
            params![network, buffer_key(buffer)],
            |r| r.get(0),
        )?;
        // The messages go with it (ON DELETE CASCADE), and their delete
        // triggers keep the index consistent.
        tx.execute(
            "DELETE FROM buffers WHERE network = ?1 AND key = ?2",
            params![network, buffer_key(buffer)],
        )?;
        tx.commit()?;
        Ok(usize::try_from(removed).unwrap_or(0))
    }

    // ---- reading ---------------------------------------------------------

    /// One page of a buffer's history, oldest first, ready to display.
    ///
    /// With `before: None` this is the most recent `limit` messages. To load
    /// further back, pass the cursor of the first message of the page you
    /// already have; paging by cursor rather than by offset means messages
    /// arriving in the meantime cannot shift the page and cause a repeat or a
    /// skip.
    pub fn scrollback(
        &self,
        network: &str,
        buffer: &str,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let mut sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m JOIN buffers b ON b.id = m.buffer_id
             WHERE b.network = ?1 AND b.key = ?2",
        );
        let mut args: Vec<Box<dyn ToSql>> =
            vec![Box::new(network.to_owned()), Box::new(buffer_key(buffer))];
        if let Some(cursor) = before {
            sql.push_str(" AND (m.time_ms, m.id) < (?, ?)");
            args.push(Box::new(cursor.time_ms));
            args.push(Box::new(cursor.id));
        }
        sql.push_str(" ORDER BY m.time_ms DESC, m.id DESC LIMIT ?");
        args.push(Box::new(clamp_limit(limit)));

        let mut messages = self.query_messages(&sql, &args)?;
        messages.reverse();
        Ok(messages)
    }

    /// A message and the ones around it in the same buffer, oldest first.
    ///
    /// This is what "jump to this search result" needs: the hit in its
    /// conversation. Returns an empty list if `id` does not exist.
    pub fn around(&self, id: i64, radius: usize) -> Result<Vec<StoredMessage>> {
        let Some((buffer_id, time_ms)) = self
            .conn
            .query_row(
                "SELECT buffer_id, time_ms FROM messages WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(Vec::new());
        };

        let radius = clamp_limit(radius);
        let side = |comparison: &str, order: &str| {
            format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages m JOIN buffers b ON b.id = m.buffer_id
                 WHERE m.buffer_id = ?1 AND (m.time_ms, m.id) {comparison} (?2, ?3)
                 ORDER BY m.time_ms {order}, m.id {order} LIMIT ?4"
            )
        };
        let position: Vec<Box<dyn ToSql>> = vec![
            Box::new(buffer_id),
            Box::new(time_ms),
            Box::new(id),
            Box::new(radius),
        ];

        let mut before = self.query_messages(&side("<", "DESC"), &position)?;
        before.reverse();
        let after = self.query_messages(&side(">", "ASC"), &position)?;

        let centre = self.query_messages(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages m JOIN buffers b ON b.id = m.buffer_id
                 WHERE m.id = ?1"
            ),
            &[Box::new(id) as Box<dyn ToSql>],
        )?;

        let mut all = before;
        all.extend(centre);
        all.extend(after);
        Ok(all)
    }

    /// Searches the log for what a person typed into a search box.
    ///
    /// See the [`query`](crate::query) module for the syntax. A query with
    /// nothing in it returns no results rather than everything.
    pub fn search(&self, query_text: &str, options: &SearchOptions) -> Result<Vec<SearchHit>> {
        let parsed = query::parse(query_text);
        // What is typed in the query wins over the option fields.
        let from = parsed.from.clone().or_else(|| options.from.clone());
        let buffer = parsed.buffer.clone().or_else(|| options.buffer.clone());

        if parsed.fts.is_none() && from.is_none() && buffer.is_none() {
            return Ok(Vec::new());
        }

        let mut args: Vec<Box<dyn ToSql>> = Vec::new();
        let mut sql = match &parsed.fts {
            Some(expression) => {
                args.push(Box::new(MARK_START.to_string()));
                args.push(Box::new(MARK_END.to_string()));
                args.push(Box::new(expression.clone()));
                format!(
                    "SELECT {MESSAGE_COLUMNS}, snippet(messages_fts, 0, ?, ?, '…', 20)
                     FROM messages_fts
                     JOIN messages m ON m.id = messages_fts.rowid
                     JOIN buffers b ON b.id = m.buffer_id
                     WHERE messages_fts MATCH ?"
                )
            }
            None => format!(
                "SELECT {MESSAGE_COLUMNS}, NULL FROM messages m
                 JOIN buffers b ON b.id = m.buffer_id WHERE 1 = 1"
            ),
        };

        if let Some(network) = &options.network {
            sql.push_str(" AND b.network = ?");
            args.push(Box::new(network.clone()));
        }
        if let Some(buffer) = &buffer {
            sql.push_str(" AND b.key = ?");
            args.push(Box::new(buffer_key(buffer)));
        }
        if let Some(from) = &from {
            sql.push_str(" AND m.sender = ? COLLATE NOCASE");
            args.push(Box::new(from.clone()));
        }

        sql.push_str(match (parsed.fts.is_some(), options.order) {
            (true, SearchOrder::Relevance) => {
                " ORDER BY bm25(messages_fts), m.time_ms DESC, m.id DESC"
            }
            _ => " ORDER BY m.time_ms DESC, m.id DESC",
        });
        sql.push_str(" LIMIT ?");
        args.push(Box::new(clamp_limit(options.limit)));

        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |row| {
            let message = read_message(row)?;
            let snippet: Option<String> = row.get(10)?;
            Ok((message, snippet))
        })?;

        let mut hits = Vec::new();
        for row in rows {
            let (message, snippet) = row?;
            let snippet = snippet.unwrap_or_else(|| {
                format::strip(&message.text)
                    .chars()
                    .take(PLAIN_SNIPPET_CHARS)
                    .collect()
            });
            hits.push(SearchHit { message, snippet });
        }
        Ok(hits)
    }

    /// The conversations on a network that have messages, most recently active
    /// first.
    pub fn buffers(&self, network: &str) -> Result<Vec<BufferInfo>> {
        let mut statement = self.conn.prepare(
            "SELECT b.network, b.name, count(m.id), max(m.time_ms)
             FROM buffers b LEFT JOIN messages m ON m.buffer_id = b.id
             WHERE b.network = ?1
             GROUP BY b.id
             ORDER BY max(m.time_ms) DESC, b.name",
        )?;
        let rows = statement.query_map(params![network], |r| {
            Ok(BufferInfo {
                network: r.get(0)?,
                name: r.get(1)?,
                messages: r.get(2)?,
                last_time_ms: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The names of the networks that have anything logged.
    pub fn networks(&self) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT network FROM buffers ORDER BY network")?;
        let rows = statement.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// How many messages are stored in total.
    pub fn message_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?)
    }

    /// Asks SQLite to verify the search index against the messages it indexes.
    /// Returns an error if they disagree. Slow on a large log; intended for
    /// tests and a "repair" command.
    pub fn check_index(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages_fts (messages_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )?;
        Ok(())
    }

    fn query_messages(&self, sql: &str, args: &[Box<dyn ToSql>]) -> Result<Vec<StoredMessage>> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(
            params_from_iter(args.iter().map(|a| a.as_ref())),
            read_message,
        )?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

fn clamp_limit(limit: usize) -> i64 {
    i64::try_from(limit.clamp(1, 1000)).unwrap_or(1000)
}

fn read_message(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        id: row.get(0)?,
        network: row.get(1)?,
        buffer: row.get(2)?,
        time_ms: row.get(3)?,
        sender: row.get(4)?,
        kind: Kind::from_db(row.get(5)?),
        text: row.get(6)?,
        own: row.get(7)?,
        highlight: row.get(8)?,
        msgid: row.get(9)?,
    })
}

/// Finds or creates the buffer, keeping its display name current: a channel
/// that was first seen as `#rhizome` and later as `#Rhizome` is one buffer,
/// shown the way it was last written.
fn buffer_id(conn: &Connection, network: &str, name: &str) -> rusqlite::Result<i64> {
    conn.prepare_cached(
        "INSERT INTO buffers (network, key, name) VALUES (?1, ?2, ?3)
         ON CONFLICT (network, key) DO UPDATE SET name = excluded.name
         RETURNING id",
    )?
    .query_row(params![network, buffer_key(name), name], |r| r.get(0))
}

fn insert(conn: &Connection, m: &NewMessage) -> rusqlite::Result<Option<i64>> {
    let buffer_id = buffer_id(conn, &m.network, &m.buffer)?;
    let time_ms = m
        .server_time
        .as_deref()
        .and_then(parse_server_time)
        .unwrap_or(m.received_ms);
    // An empty id is no id: treating it as one would make every such message
    // in a buffer a duplicate of the first.
    let msgid = m.msgid.as_deref().filter(|id| !id.is_empty());

    let changed = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO messages
                 (buffer_id, time_ms, sender, kind, text, plain, own, highlight, msgid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?
        .execute(params![
            buffer_id,
            time_ms,
            m.sender,
            m.kind.to_db(),
            m.text,
            format::strip(&m.text),
            m.own,
            m.highlight,
            msgid,
        ])?;
    Ok((changed == 1).then(|| conn.last_insert_rowid()))
}
