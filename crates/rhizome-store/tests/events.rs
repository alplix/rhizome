//! Events (joins, parts, topic changes), read markers, and the move from the
//! first schema version to the second.

use std::path::PathBuf;

use rhizome_store::time::format_ms;
use rhizome_store::{Kind, NewMessage, SearchOptions, Store};

const NET: &str = "libera";

fn chat(buffer: &str, sender: &str, text: &str, time_ms: i64) -> NewMessage {
    NewMessage {
        network: NET.into(),
        buffer: buffer.into(),
        sender: sender.into(),
        kind: Kind::Privmsg,
        text: text.into(),
        server_time: Some(format_ms(time_ms)),
        received_ms: 0,
        msgid: None,
        own: false,
        highlight: false,
    }
}

fn event(buffer: &str, sender: &str, text: &str, time_ms: i64) -> NewMessage {
    NewMessage {
        kind: Kind::Event,
        ..chat(buffer, sender, text, time_ms)
    }
}

fn hits(store: &Store, query: &str) -> usize {
    store
        .search(query, &SearchOptions::default())
        .unwrap()
        .len()
}

struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> TempDb {
        let path = std::env::temp_dir().join(format!(
            "rhizome-events-{}-{name}.sqlite3",
            std::process::id()
        ));
        TempDb::remove(&path);
        TempDb(path)
    }
    fn remove(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        TempDb::remove(&self.0);
    }
}

// ---- events ------------------------------------------------------------------

#[test]
fn events_are_in_the_scrollback_but_never_in_search() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&chat("#c", "bob", "the migration plan is ready", 1000))
        .unwrap();
    s.log_message(&event("#c", "carol", "[\"join\"]", 2000))
        .unwrap();
    s.log_message(&event(
        "#c",
        "dave",
        "[\"topic\",\"migration plan tomorrow\"]",
        3000,
    ))
    .unwrap();

    let page = s.scrollback(NET, "#c", None, 10).unwrap();
    assert_eq!(page.len(), 3, "events belong in the history");
    assert_eq!(page[1].kind, Kind::Event);
    assert_eq!(
        page[1].text, "[\"join\"]",
        "the text is kept exactly as given"
    );
    assert!(!page[1].kind.is_chat() && page[0].kind.is_chat());

    // Only the person's own words match, even though an event mentions the same words.
    assert_eq!(hits(&s, "migration"), 1);
    assert_eq!(hits(&s, "topic"), 0);
    assert_eq!(hits(&s, "join"), 0);
    // And a filter-only search does not list events as things someone said.
    assert_eq!(hits(&s, "from:carol"), 0);
    assert_eq!(hits(&s, "in:#c"), 1);
    s.check_index().unwrap();
}

#[test]
fn deleting_events_does_not_corrupt_the_search_index() {
    // The index trigger that runs on delete must skip rows that were never
    // indexed: removing one anyway breaks the index.
    let mut s = Store::open_in_memory().unwrap();
    for i in 0..20 {
        s.log_message(&chat("#gone", "bob", &format!("needle {i}"), 1000 + i * 2))
            .unwrap();
        s.log_message(&event("#gone", "bob", "[\"join\"]", 1001 + i * 2))
            .unwrap();
    }
    s.log_message(&chat("#kept", "bob", "needle survives", 9000))
        .unwrap();
    assert_eq!(hits(&s, "needle"), 21);

    assert_eq!(
        s.delete_buffer(NET, "#gone").unwrap(),
        40,
        "chat and events both go"
    );
    assert_eq!(hits(&s, "needle"), 1);
    s.check_index().unwrap();
}

#[test]
fn events_are_not_counted_in_a_buffers_totals() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&chat("#c", "bob", "hello", 1000)).unwrap();
    for t in 2000..2010 {
        s.log_message(&event("#c", "x", "[\"join\"]", t)).unwrap();
    }
    let b = &s.buffers(NET).unwrap()[0];
    assert_eq!(b.messages, 1);
    assert_eq!(
        b.last_time_ms,
        Some(1000),
        "a join is not activity worth sorting by"
    );
}

#[test]
fn a_conversation_made_only_of_events_is_still_listed() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&event("#quiet", "alp", "[\"join\"]", 1000))
        .unwrap();
    let buffers = s.buffers(NET).unwrap();
    assert_eq!(buffers.len(), 1);
    assert_eq!(
        (
            buffers[0].messages,
            buffers[0].unread,
            buffers[0].last_time_ms
        ),
        (0, 0, None)
    );
}

// ---- read markers ------------------------------------------------------------

#[test]
fn a_new_conversation_starts_with_its_first_message_unread() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&chat("dave", "dave", "psst", 5000)).unwrap();
    let b = &s.buffers(NET).unwrap()[0];
    assert_eq!((b.unread, b.highlights), (1, 0));
}

#[test]
fn unread_counts_others_messages_after_the_marker_and_highlights_separately() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&chat("#c", "bob", "old", 1000)).unwrap();
    s.mark_read(NET, "#c", 1000).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 0);

    let mut mention = chat("#c", "bob", "alp: ping", 2000);
    mention.highlight = true;
    let mut mine = chat("#c", "alp", "my reply", 2500);
    mine.own = true;
    s.log_message(&mention).unwrap();
    s.log_message(&mine).unwrap();
    s.log_message(&chat("#c", "carol", "later", 3000)).unwrap();
    s.log_message(&event("#c", "dave", "[\"join\"]", 3500))
        .unwrap();

    let b = &s.buffers(NET).unwrap()[0];
    assert_eq!(b.unread, 2, "own messages and events do not count");
    assert_eq!(b.highlights, 1);
}

#[test]
fn marking_read_clears_the_count_and_only_moves_forward() {
    let mut s = Store::open_in_memory().unwrap();
    for t in [1000, 2000, 3000] {
        s.log_message(&chat("#c", "bob", "x", t)).unwrap();
    }
    s.mark_read(NET, "#c", 2000).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 1);
    s.mark_read(NET, "#c", 3000).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 0);

    // A late report from a window that was behind must not resurrect old unreads.
    s.mark_read(NET, "#c", 1000).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 0);
}

#[test]
fn history_older_than_the_marker_never_becomes_unread() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&chat("#c", "bob", "now", 9000)).unwrap();
    s.mark_read(NET, "#c", 9000).unwrap();
    // A reconnect replays messages from before the marker.
    s.log_message(&chat("#c", "bob", "replayed", 5000)).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 0);
}

#[test]
fn marking_is_case_insensitive_and_harmless_for_unknown_conversations() {
    let mut s = Store::open_in_memory().unwrap();
    s.log_message(&chat("#Rhizome", "bob", "x", 1000)).unwrap();
    s.mark_read(NET, "#RHIZOME", 1000).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 0);
    s.mark_read(NET, "#nowhere", 5000).unwrap();
    s.mark_read("othernet", "#Rhizome", 5000).unwrap();
    assert_eq!(s.buffers(NET).unwrap().len(), 1);
}

#[test]
fn read_markers_survive_a_restart() {
    let db = TempDb::new("markers");
    {
        let mut s = Store::open(&db.0).unwrap();
        s.log_message(&chat("#c", "bob", "one", 1000)).unwrap();
        s.log_message(&chat("#c", "bob", "two", 2000)).unwrap();
        s.mark_read(NET, "#c", 1000).unwrap();
    }
    let s = Store::open(&db.0).unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 1);
}

#[test]
fn unread_counts_stay_fast_on_a_long_history() {
    let mut s = Store::open_in_memory().unwrap();
    let batch: Vec<NewMessage> = (0..30_000)
        .map(|i| chat("#big", "bob", "filler", i))
        .collect();
    s.log_messages(&batch).unwrap();
    s.mark_read(NET, "#big", 29_990).unwrap();

    let started = std::time::Instant::now();
    let b = &s.buffers(NET).unwrap()[0];
    assert_eq!(b.unread, 9);
    assert!(
        started.elapsed().as_millis() < 1500,
        "took {:?}",
        started.elapsed()
    );
}

// ---- moving from schema version 1 ------------------------------------------------

/// The first schema, exactly as version 1 shipped it.
const V1: &str = r#"
CREATE TABLE buffers (
    id INTEGER PRIMARY KEY, network TEXT NOT NULL, key TEXT NOT NULL, name TEXT NOT NULL,
    UNIQUE (network, key)
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY,
    buffer_id INTEGER NOT NULL REFERENCES buffers(id) ON DELETE CASCADE,
    time_ms INTEGER NOT NULL, sender TEXT NOT NULL, kind INTEGER NOT NULL,
    text TEXT NOT NULL, plain TEXT NOT NULL, own INTEGER NOT NULL, highlight INTEGER NOT NULL,
    msgid TEXT
);
CREATE INDEX messages_by_buffer_time ON messages (buffer_id, time_ms, id);
CREATE INDEX messages_by_sender ON messages (sender COLLATE NOCASE, time_ms);
CREATE UNIQUE INDEX messages_by_msgid ON messages (buffer_id, msgid) WHERE msgid IS NOT NULL;
CREATE VIRTUAL TABLE messages_fts USING fts5(
    plain, content = 'messages', content_rowid = 'id',
    tokenize = "unicode61 remove_diacritics 2 tokenchars '_'"
);
CREATE TRIGGER messages_after_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts (rowid, plain) VALUES (new.id, new.plain);
END;
CREATE TRIGGER messages_after_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts (messages_fts, rowid, plain) VALUES ('delete', old.id, old.plain);
END;
PRAGMA user_version = 1;
"#;

fn make_v1(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(V1).unwrap();
    conn.execute_batch(
        "INSERT INTO buffers (id, network, key, name) VALUES (1, 'libera', '#old', '#old');
         INSERT INTO messages (buffer_id, time_ms, sender, kind, text, plain, own, highlight, msgid)
         VALUES (1, 1000, 'bob', 0, 'an old message about needles', 'an old message about needles', 0, 0, 'm1'),
                (1, 2000, 'bob', 0, 'and another', 'and another', 0, 1, 'm2');",
    )
    .unwrap();
}

#[test]
fn a_version_1_database_is_upgraded_in_place_without_losing_anything() {
    let db = TempDb::new("upgrade");
    make_v1(&db.0);

    let mut s = Store::open(&db.0).unwrap();
    // Everything that was there still is, and still searches.
    assert_eq!(s.message_count().unwrap(), 2);
    assert_eq!(hits(&s, "needles"), 1);
    s.check_index().unwrap();

    // Old conversations start fully read: an upgrade must not create a wall of badges.
    let b = &s.buffers(NET).unwrap()[0];
    assert_eq!((b.unread, b.highlights), (0, 0));

    // The new behaviour is live: events are accepted and stay out of the index...
    s.log_message(&event("#old", "carol", "[\"join\"]", 3000))
        .unwrap();
    s.log_message(&chat("#old", "dave", "a fresh needles message", 4000))
        .unwrap();
    assert_eq!(hits(&s, "needles"), 2);
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 1);
    // ...and deleting events no longer corrupts it.
    s.delete_buffer(NET, "#old").unwrap();
    s.check_index().unwrap();

    drop(s);
    let version: i64 = rusqlite::Connection::open(&db.0)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

#[test]
fn upgrading_twice_is_harmless() {
    let db = TempDb::new("upgrade-twice");
    make_v1(&db.0);
    drop(Store::open(&db.0).unwrap());
    let s = Store::open(&db.0).unwrap();
    assert_eq!(s.message_count().unwrap(), 2);
    s.check_index().unwrap();
}

#[test]
fn a_channel_first_seen_through_a_join_counts_the_messages_that_follow_as_unread() {
    let mut s = Store::open_in_memory().unwrap();
    // We join at (client) time 9000; the server's clock is behind, so the first
    // messages carry earlier timestamps. They must still count.
    s.log_message(&event("#new", "alp", "[\"join\"]", 9000))
        .unwrap();
    s.log_message(&chat("#new", "bob", "hello from a slow clock", 4000))
        .unwrap();
    assert_eq!(s.buffers(NET).unwrap()[0].unread, 1);
}
