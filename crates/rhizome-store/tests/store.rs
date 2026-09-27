//! Behaviour of the store through its public API.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rhizome_store::time::format_ms;
use rhizome_store::{
    Cursor, Error, Kind, NewMessage, SearchOptions, SearchOrder, Store, MARK_END, MARK_START,
};

const NET: &str = "Libera.Chat";

fn msg(buffer: &str, sender: &str, text: &str, time_ms: i64) -> NewMessage {
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

fn with_id(mut m: NewMessage, id: &str) -> NewMessage {
    m.msgid = Some(id.into());
    m
}

fn store() -> Store {
    Store::open_in_memory().unwrap()
}

fn texts(messages: &[rhizome_store::StoredMessage]) -> Vec<&str> {
    messages.iter().map(|m| m.text.as_str()).collect()
}

fn hits(store: &Store, query: &str) -> Vec<String> {
    store
        .search(query, &SearchOptions::default())
        .unwrap()
        .into_iter()
        .map(|h| h.message.text)
        .collect()
}

/// A path in the temp directory that is removed on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> TempDb {
        let path = std::env::temp_dir().join(format!(
            "rhizome-store-test-{}-{name}.sqlite3",
            std::process::id()
        ));
        TempDb::remove(&path);
        TempDb(path)
    }

    fn remove(path: &Path) {
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

// ---- writing and reading back -------------------------------------------

#[test]
fn messages_come_back_in_time_order_whatever_order_they_arrived() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "third", 3000)).unwrap();
    s.log_message(&msg("#c", "a", "first", 1000)).unwrap();
    s.log_message(&msg("#c", "a", "second", 2000)).unwrap();

    let page = s.scrollback(NET, "#c", None, 10).unwrap();
    assert_eq!(texts(&page), vec!["first", "second", "third"]);
}

#[test]
fn messages_in_the_same_millisecond_keep_the_order_they_were_stored() {
    let mut s = store();
    for word in ["one", "two", "three", "four"] {
        s.log_message(&msg("#c", "a", word, 5000)).unwrap();
    }
    let page = s.scrollback(NET, "#c", None, 10).unwrap();
    assert_eq!(texts(&page), vec!["one", "two", "three", "four"]);
}

#[test]
fn every_field_survives_the_round_trip() {
    let mut s = store();
    let mut m = msg(
        "#Rhizome",
        "Bob",
        "\u{2}bold\u{2} text ş",
        1_758_794_400_123,
    );
    m.kind = Kind::Action;
    m.own = true;
    m.highlight = true;
    m.msgid = Some("abc123".into());
    let id = s.log_message(&m).unwrap().unwrap();

    let page = s.scrollback(NET, "#rhizome", None, 10).unwrap();
    let got = &page[0];
    assert_eq!(got.id, id);
    assert_eq!(got.network, NET);
    assert_eq!(got.buffer, "#Rhizome", "the display name keeps its case");
    assert_eq!(got.sender, "Bob");
    assert_eq!(got.kind, Kind::Action);
    assert_eq!(
        got.text, "\u{2}bold\u{2} text ş",
        "formatting codes are kept"
    );
    assert_eq!(got.time_ms, 1_758_794_400_123);
    assert!(got.own && got.highlight);
    assert_eq!(got.msgid.as_deref(), Some("abc123"));
}

#[test]
fn the_server_time_is_used_and_arrival_time_is_the_fallback() {
    let mut s = store();
    let mut with_server = msg("#c", "a", "has time", 0);
    with_server.server_time = Some("2026-09-25T10:00:00.000Z".into());
    with_server.received_ms = 999;

    let mut none = msg("#c", "a", "no time", 0);
    none.server_time = None;
    none.received_ms = 111;

    let mut garbage = msg("#c", "a", "bad time", 0);
    garbage.server_time = Some("yesterday-ish".into());
    garbage.received_ms = 222;

    for m in [&with_server, &none, &garbage] {
        s.log_message(m).unwrap();
    }
    let page = s.scrollback(NET, "#c", None, 10).unwrap();
    let time_of = |t: &str| page.iter().find(|m| m.text == t).unwrap().time_ms;
    assert_eq!(time_of("has time"), 1_790_330_400_000);
    assert_eq!(time_of("no time"), 111);
    assert_eq!(time_of("bad time"), 222);
}

// ---- paging ---------------------------------------------------------------

#[test]
fn paging_by_cursor_visits_every_message_exactly_once() {
    let mut s = store();
    for i in 0..25 {
        s.log_message(&msg("#c", "a", &format!("m{i:02}"), 1000 + i))
            .unwrap();
    }

    let mut collected: Vec<String> = Vec::new();
    let mut before: Option<Cursor> = None;
    // 25 messages in pages of 10 is three pages and an empty one. A pager that
    // fails to advance would otherwise loop forever and hang the suite instead
    // of failing it.
    for round in 0.. {
        assert!(
            round < 10,
            "paging did not terminate; collected {collected:?}"
        );
        let page = s.scrollback(NET, "#c", before, 10).unwrap();
        if page.is_empty() {
            break;
        }
        before = Some(page[0].cursor());
        // Pages come back oldest-first, and we are walking backwards, so each
        // older page is prepended.
        let mut older: Vec<String> = page.iter().map(|m| m.text.clone()).collect();
        older.extend(collected);
        collected = older;
    }

    let expected: Vec<String> = (0..25).map(|i| format!("m{i:02}")).collect();
    assert_eq!(collected, expected);
}

#[test]
fn paging_across_messages_sharing_a_millisecond_does_not_skip_or_repeat() {
    let mut s = store();
    for i in 0..9 {
        s.log_message(&msg("#c", "a", &format!("m{i}"), 7000))
            .unwrap();
    }
    let newest = s.scrollback(NET, "#c", None, 4).unwrap();
    assert_eq!(texts(&newest), vec!["m5", "m6", "m7", "m8"]);
    let older = s
        .scrollback(NET, "#c", Some(newest[0].cursor()), 4)
        .unwrap();
    assert_eq!(texts(&older), vec!["m1", "m2", "m3", "m4"]);
    let oldest = s.scrollback(NET, "#c", Some(older[0].cursor()), 4).unwrap();
    assert_eq!(texts(&oldest), vec!["m0"]);
}

#[test]
fn new_messages_arriving_between_pages_do_not_shift_them() {
    let mut s = store();
    for i in 0..10 {
        s.log_message(&msg("#c", "a", &format!("m{i}"), 1000 + i))
            .unwrap();
    }
    let first = s.scrollback(NET, "#c", None, 5).unwrap();
    // Traffic arrives while the user is reading. With offset paging this would
    // push everything down and repeat messages.
    for i in 10..15 {
        s.log_message(&msg("#c", "a", &format!("m{i}"), 1000 + i))
            .unwrap();
    }
    let second = s.scrollback(NET, "#c", Some(first[0].cursor()), 5).unwrap();
    assert_eq!(texts(&second), vec!["m0", "m1", "m2", "m3", "m4"]);
}

#[test]
fn scrollback_of_an_unknown_buffer_is_empty_not_an_error() {
    let s = store();
    assert!(s.scrollback(NET, "#nowhere", None, 10).unwrap().is_empty());
}

// ---- duplicates -----------------------------------------------------------

#[test]
fn a_repeated_msgid_in_the_same_buffer_is_stored_once() {
    let mut s = store();
    let m = with_id(msg("#c", "a", "hello", 1000), "id-1");
    assert!(s.log_message(&m).unwrap().is_some());
    assert!(
        s.log_message(&m).unwrap().is_none(),
        "the replay is ignored"
    );
    assert_eq!(s.message_count().unwrap(), 1);
}

#[test]
fn replaying_overlapping_history_adds_only_what_is_new() {
    let mut s = store();
    let history: Vec<NewMessage> = (0..10)
        .map(|i| {
            with_id(
                msg("#c", "a", &format!("m{i}"), 1000 + i),
                &format!("id-{i}"),
            )
        })
        .collect();
    assert_eq!(s.log_messages(&history[..6]).unwrap(), 6);
    // A reconnect fetches a window overlapping the first by three messages.
    assert_eq!(s.log_messages(&history[3..]).unwrap(), 4);
    assert_eq!(s.message_count().unwrap(), 10);
    s.check_index().unwrap();
}

#[test]
fn the_same_msgid_in_different_buffers_is_not_a_duplicate() {
    let mut s = store();
    s.log_message(&with_id(msg("#a", "x", "one", 1000), "same"))
        .unwrap();
    assert!(s
        .log_message(&with_id(msg("#b", "x", "two", 1000), "same"))
        .unwrap()
        .is_some());
}

#[test]
fn messages_without_an_id_are_never_treated_as_duplicates() {
    let mut s = store();
    for _ in 0..3 {
        s.log_message(&msg("#c", "a", "again", 1000)).unwrap();
    }
    assert_eq!(s.message_count().unwrap(), 3, "people do repeat themselves");

    // An empty id is no id, not a shared id.
    for _ in 0..2 {
        s.log_message(&with_id(msg("#c", "a", "empty id", 1000), ""))
            .unwrap();
    }
    assert_eq!(s.message_count().unwrap(), 5);
}

// ---- buffers ----------------------------------------------------------------

#[test]
fn buffer_names_match_case_insensitively_and_show_the_latest_spelling() {
    let mut s = store();
    s.log_message(&msg("#rhizome", "a", "one", 1000)).unwrap();
    s.log_message(&msg("#Rhizome", "a", "two", 2000)).unwrap();

    let buffers = s.buffers(NET).unwrap();
    assert_eq!(buffers.len(), 1);
    assert_eq!(buffers[0].name, "#Rhizome");
    assert_eq!(buffers[0].messages, 2);
    assert_eq!(s.scrollback(NET, "#RHIZOME", None, 10).unwrap().len(), 2);
}

#[test]
fn rfc1459_bracket_folding_applies_to_private_message_buffers() {
    let mut s = store();
    s.log_message(&msg("Dave[away]", "Dave[away]", "hi", 1000))
        .unwrap();
    s.log_message(&msg("dave{away}", "dave{away}", "back", 2000))
        .unwrap();
    assert_eq!(s.buffers(NET).unwrap().len(), 1);
}

#[test]
fn networks_are_kept_apart() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "on libera", 1000)).unwrap();
    let mut other = msg("#c", "a", "on oftc", 1000);
    other.network = "OFTC".into();
    s.log_message(&other).unwrap();

    assert_eq!(
        texts(&s.scrollback(NET, "#c", None, 10).unwrap()),
        vec!["on libera"]
    );
    assert_eq!(
        texts(&s.scrollback("OFTC", "#c", None, 10).unwrap()),
        vec!["on oftc"]
    );
    assert_eq!(s.networks().unwrap(), vec!["Libera.Chat", "OFTC"]);

    let opts = SearchOptions {
        network: Some("OFTC".into()),
        ..SearchOptions::default()
    };
    let found = s.search("on", &opts).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message.network, "OFTC");
}

#[test]
fn buffer_listing_is_ordered_by_recent_activity_and_counts_messages() {
    let mut s = store();
    s.log_message(&msg("#old", "a", "x", 1000)).unwrap();
    s.log_message(&msg("#busy", "a", "x", 2000)).unwrap();
    s.log_message(&msg("#busy", "a", "y", 9000)).unwrap();
    let buffers = s.buffers(NET).unwrap();
    assert_eq!(
        buffers.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        vec!["#busy", "#old"]
    );
    assert_eq!(buffers[0].messages, 2);
    assert_eq!(buffers[0].last_time_ms, Some(9000));
}

// ---- searching --------------------------------------------------------------

#[test]
fn words_are_anded_in_any_order() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "null pointer in kmalloc", 1000))
        .unwrap();
    s.log_message(&msg("#c", "a", "a pointer to nothing", 2000))
        .unwrap();
    s.log_message(&msg("#c", "a", "totally unrelated", 3000))
        .unwrap();

    assert_eq!(hits(&s, "pointer").len(), 2);
    assert_eq!(hits(&s, "null pointer"), vec!["null pointer in kmalloc"]);
    assert_eq!(hits(&s, "pointer null"), vec!["null pointer in kmalloc"]);
    assert!(hits(&s, "pointer banana").is_empty());
}

#[test]
fn a_quoted_phrase_must_match_in_order() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "the null pointer bug", 1000))
        .unwrap();
    s.log_message(&msg("#c", "a", "pointer to a null value", 2000))
        .unwrap();
    assert_eq!(hits(&s, "\"null pointer\""), vec!["the null pointer bug"]);
}

#[test]
fn a_trailing_star_matches_word_prefixes() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "kmalloc failed", 1000))
        .unwrap();
    s.log_message(&msg("#c", "a", "kmemdup too", 2000)).unwrap();
    assert_eq!(hits(&s, "kmal*"), vec!["kmalloc failed"]);
    assert_eq!(hits(&s, "km*").len(), 2);
    assert!(
        hits(&s, "kmal").is_empty(),
        "without the star it is a whole word"
    );
}

#[test]
fn search_ignores_case() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "Segmentation FAULT", 1000))
        .unwrap();
    assert_eq!(hits(&s, "segmentation fault").len(), 1);
}

#[test]
fn formatting_codes_do_not_break_a_word_and_are_not_searchable() {
    let mut s = store();
    // A colour change in the middle of the word "error".
    s.log_message(&msg("#c", "a", "\u{3}04err\u{3}or: null", 1000))
        .unwrap();
    s.log_message(&msg("#c", "a", "\u{2}bold\u{2} claim", 2000))
        .unwrap();

    assert_eq!(hits(&s, "error").len(), 1);
    assert_eq!(hits(&s, "bold").len(), 1);
    // The codes are not words, so their parameters are not matched.
    assert!(hits(&s, "04err").is_empty());
    // The stored text still has the codes, for faithful display.
    let stored = &s.search("error", &SearchOptions::default()).unwrap()[0].message;
    assert!(stored.text.starts_with('\u{3}'));
}

#[test]
fn the_sender_is_not_part_of_what_words_match() {
    let mut s = store();
    s.log_message(&msg("#c", "bob", "hello there", 1000))
        .unwrap();
    assert!(
        hits(&s, "bob").is_empty(),
        "use from:bob to search by sender"
    );
}

#[test]
fn snake_case_identifiers_stay_whole() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "use kmalloc_array here", 1000))
        .unwrap();
    s.log_message(&msg("#c", "a", "an array of things", 2000))
        .unwrap();

    // The identifier is one token: "array" alone does not find it...
    assert_eq!(hits(&s, "array"), vec!["an array of things"]);
    // ...but the full name and a prefix both do.
    assert_eq!(hits(&s, "kmalloc_array"), vec!["use kmalloc_array here"]);
    assert_eq!(hits(&s, "kmalloc*"), vec!["use kmalloc_array here"]);
}

#[test]
fn turkish_text_is_found_with_or_without_diacritics() {
    let mut s = store();
    s.log_message(&msg(
        "#c",
        "a",
        "Merhaba dünya, şu Türkçe cümleyi bul",
        1000,
    ))
    .unwrap();

    // Typed exactly, and typed on a keyboard with no Turkish characters.
    for query in [
        "dünya", "dunya", "türkçe", "turkce", "cümleyi", "cumleyi", "şu", "su",
    ] {
        assert_eq!(hits(&s, query).len(), 1, "query {query:?}");
    }
    // Capital dotted İ folds to a plain i.
    s.log_message(&msg("#c", "a", "İstanbul'a gidiyoruz", 2000))
        .unwrap();
    assert_eq!(hits(&s, "istanbul").len(), 1);
    assert_eq!(hits(&s, "İstanbul").len(), 1);
}

#[test]
fn a_dotless_i_and_a_plain_i_find_each_other() {
    let mut s = store();
    // Typed with the real Turkish letter.
    s.log_message(&msg("#c", "a", "şu Türkçe hatayı gördüm", 1000))
        .unwrap();
    // Typed on a keyboard with no Turkish characters at all.
    assert_eq!(hits(&s, "hatayi").len(), 1);
    // And the reverse: a message typed without Turkish characters is found
    // by someone typing the real letter.
    s.log_message(&msg("#c", "b", "izmir'e gidiyorum", 2000))
        .unwrap();
    assert_eq!(hits(&s, "ızmir").len(), 1);
    // The displayed snippet is the real message, not a folded stand-in.
    let found = s.search("hatayi", &SearchOptions::default()).unwrap();
    assert!(found[0].snippet.contains('ı'), "{}", found[0].snippet);
}

#[test]
fn search_finds_non_latin_scripts_and_emoji_text() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "привет мир", 1000)).unwrap();
    s.log_message(&msg("#c", "a", "你好 世界 done 🎉", 2000))
        .unwrap();
    assert_eq!(hits(&s, "привет").len(), 1);
    assert_eq!(hits(&s, "done").len(), 1);
}

#[test]
fn from_and_in_narrow_the_results() {
    let mut s = store();
    s.log_message(&msg("#kernel", "bob", "oops in the driver", 1000))
        .unwrap();
    s.log_message(&msg("#kernel", "carol", "oops in the scheduler", 2000))
        .unwrap();
    s.log_message(&msg("#rust", "bob", "oops in the borrow checker", 3000))
        .unwrap();

    assert_eq!(hits(&s, "oops").len(), 3);
    assert_eq!(hits(&s, "oops from:bob").len(), 2);
    assert_eq!(
        hits(&s, "oops from:BOB").len(),
        2,
        "sender match ignores case"
    );
    assert_eq!(hits(&s, "oops in:#kernel").len(), 2);
    assert_eq!(hits(&s, "oops in:#KERNEL").len(), 2);
    assert_eq!(
        hits(&s, "oops from:bob in:#kernel"),
        vec!["oops in the driver"]
    );
}

#[test]
fn filters_alone_list_matching_messages_newest_first() {
    let mut s = store();
    s.log_message(&msg("#c", "bob", "first", 1000)).unwrap();
    s.log_message(&msg("#c", "carol", "not bob", 2000)).unwrap();
    s.log_message(&msg("#c", "bob", "second", 3000)).unwrap();

    assert_eq!(hits(&s, "from:bob"), vec!["second", "first"]);
    assert_eq!(hits(&s, "in:#c").len(), 3);
}

#[test]
fn a_typed_filter_overrides_the_option_field() {
    let mut s = store();
    s.log_message(&msg("#a", "x", "word", 1000)).unwrap();
    s.log_message(&msg("#b", "x", "word", 2000)).unwrap();
    let opts = SearchOptions {
        buffer: Some("#a".into()),
        ..SearchOptions::default()
    };
    let found = s.search("word in:#b", &opts).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message.buffer, "#b");
    // And the option applies when nothing is typed.
    assert_eq!(s.search("word", &opts).unwrap()[0].message.buffer, "#a");
}

#[test]
fn an_empty_query_returns_nothing_rather_than_everything() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "something", 1000)).unwrap();
    for q in ["", "   ", "\"\"", "::", "from:", "in: from:"] {
        assert!(hits(&s, q).is_empty(), "query {q:?}");
    }
}

#[test]
fn relevance_and_recency_order_results_differently() {
    let mut s = store();
    // The older message is the better match: a short message that is all
    // about the term outranks a long one that mentions it in passing.
    s.log_message(&msg("#c", "a", "segfault", 1000)).unwrap();
    s.log_message(&msg(
        "#c",
        "a",
        "so I was reading a long thread about the kernel and other unrelated things \
             when somebody mentioned segfault in passing and then moved on to lunch",
        9000,
    ))
    .unwrap();

    let relevance = s.search("segfault", &SearchOptions::default()).unwrap();
    assert_eq!(relevance[0].message.text, "segfault");

    let newest = s
        .search(
            "segfault",
            &SearchOptions {
                order: SearchOrder::Newest,
                ..SearchOptions::default()
            },
        )
        .unwrap();
    assert_eq!(newest[0].message.time_ms, 9000);
}

#[test]
fn the_snippet_marks_the_match_without_being_fooled_by_markup_in_the_text() {
    let mut s = store();
    s.log_message(&msg(
        "#c",
        "a",
        "see <b>the</b> [needle] in [the] haystack",
        1000,
    ))
    .unwrap();
    let hit = &s.search("needle", &SearchOptions::default()).unwrap()[0];
    assert!(
        hit.snippet
            .contains(&format!("{MARK_START}needle{MARK_END}")),
        "snippet was {:?}",
        hit.snippet
    );
    // The literal brackets and tags in the message are just text.
    assert!(hit.snippet.contains("<b>"));
    assert_eq!(hit.snippet.matches(MARK_START).count(), 1);
}

#[test]
fn a_filter_only_hit_gets_the_start_of_the_message_as_its_snippet() {
    let mut s = store();
    s.log_message(&msg(
        "#c",
        "bob",
        &format!("\u{2}{}\u{2}", "x".repeat(500)),
        1000,
    ))
    .unwrap();
    let hit = &s.search("from:bob", &SearchOptions::default()).unwrap()[0];
    assert_eq!(hit.snippet.chars().count(), 160);
    assert!(
        !hit.snippet.contains('\u{2}'),
        "formatting codes are stripped"
    );
}

#[test]
fn nothing_a_person_can_type_is_a_search_error() {
    let mut s = store();
    s.log_message(&msg("#c", "a", "some text to search", 1000))
        .unwrap();
    let hostile = [
        "\"",
        "\"\"\"",
        "'",
        "AND",
        "OR",
        "NOT",
        "a AND",
        "NEAR(a b)",
        "NEAR/2",
        "*",
        "***",
        "((((",
        "))))",
        "a OR b NOT",
        "col:foo",
        "plain:text",
        "{plain}: x",
        "-",
        "^start",
        "a b c d e f g h i j k l m n o p",
        "\u{0}",
        "\\",
        "; DROP TABLE messages; --",
        "' OR 1=1 --",
        "\" OR \"1\"=\"1",
        "🎉",
        "\u{FFFD}",
        &"x".repeat(10_000),
    ];
    for query in hostile {
        s.search(query, &SearchOptions::default())
            .unwrap_or_else(|e| panic!("query {query:?} raised {e}"));
    }
    // And none of it damaged anything.
    assert_eq!(s.message_count().unwrap(), 1);
    assert_eq!(hits(&s, "search").len(), 1);
}

#[test]
fn the_result_limit_is_clamped() {
    let mut s = store();
    for i in 0..30 {
        s.log_message(&msg("#c", "a", &format!("needle {i}"), 1000 + i))
            .unwrap();
    }
    let with = |limit| SearchOptions {
        limit,
        ..SearchOptions::default()
    };
    assert_eq!(
        s.search("needle", &with(0)).unwrap().len(),
        1,
        "zero is raised to one"
    );
    assert_eq!(s.search("needle", &with(10)).unwrap().len(), 10);
    assert_eq!(s.search("needle", &with(usize::MAX)).unwrap().len(), 30);
}

// ---- context ------------------------------------------------------------------

#[test]
fn around_returns_a_hit_in_its_conversation() {
    let mut s = store();
    let mut ids = Vec::new();
    for i in 0..9 {
        ids.push(
            s.log_message(&msg("#c", "a", &format!("m{i}"), 1000 + i))
                .unwrap()
                .unwrap(),
        );
    }
    s.log_message(&msg("#other", "a", "elsewhere", 1004))
        .unwrap();

    let context = s.around(ids[4], 2).unwrap();
    assert_eq!(texts(&context), vec!["m2", "m3", "m4", "m5", "m6"]);
}

#[test]
fn around_at_the_edges_returns_what_exists() {
    let mut s = store();
    let first = s.log_message(&msg("#c", "a", "m0", 1000)).unwrap().unwrap();
    s.log_message(&msg("#c", "a", "m1", 2000)).unwrap();
    assert_eq!(texts(&s.around(first, 5).unwrap()), vec!["m0", "m1"]);
    assert!(s.around(999_999, 5).unwrap().is_empty());
}

#[test]
fn a_search_hit_leads_to_its_context() {
    let mut s = store();
    for (i, text) in ["setup", "the backtrace is here", "and the fix"]
        .iter()
        .enumerate()
    {
        s.log_message(&msg("#c", "a", text, 1000 + i as i64))
            .unwrap();
    }
    let hit = &s.search("backtrace", &SearchOptions::default()).unwrap()[0];
    let context = s.around(hit.message.id, 1).unwrap();
    assert_eq!(
        texts(&context),
        vec!["setup", "the backtrace is here", "and the fix"]
    );
}

// ---- deleting -----------------------------------------------------------------

#[test]
fn deleting_a_buffer_removes_it_from_search_and_keeps_the_index_consistent() {
    let mut s = store();
    s.log_message(&msg("#gone", "a", "secret plans", 1000))
        .unwrap();
    s.log_message(&msg("#gone", "a", "more secret plans", 2000))
        .unwrap();
    s.log_message(&msg("#kept", "a", "public plans", 3000))
        .unwrap();
    assert_eq!(hits(&s, "plans").len(), 3);

    assert_eq!(s.delete_buffer(NET, "#GONE").unwrap(), 2);

    assert_eq!(hits(&s, "plans"), vec!["public plans"]);
    assert!(
        hits(&s, "secret").is_empty(),
        "deleted text must not stay searchable"
    );
    assert_eq!(s.message_count().unwrap(), 1);
    assert_eq!(s.buffers(NET).unwrap().len(), 1);
    s.check_index().unwrap();
}

#[test]
fn deleting_a_buffer_that_does_not_exist_is_a_no_op() {
    let mut s = store();
    assert_eq!(s.delete_buffer(NET, "#nope").unwrap(), 0);
}

// ---- durability ------------------------------------------------------------

#[test]
fn the_log_survives_closing_and_reopening() {
    let db = TempDb::new("reopen");
    {
        let mut s = Store::open(&db.0).unwrap();
        s.log_message(&msg("#c", "a", "remember me", 1000)).unwrap();
    }
    let s = Store::open(&db.0).unwrap();
    assert_eq!(hits(&s, "remember"), vec!["remember me"]);
    s.check_index().unwrap();
}

#[test]
fn a_reader_can_search_while_another_connection_is_writing() {
    let db = TempDb::new("concurrent");
    let mut writer = Store::open(&db.0).unwrap();
    writer.log_message(&msg("#c", "a", "before", 1000)).unwrap();

    let reader = Store::open(&db.0).unwrap();
    writer.log_message(&msg("#c", "a", "after", 2000)).unwrap();
    // With write-ahead logging the reader sees committed work without waiting
    // for the writer to close.
    assert_eq!(hits(&reader, "after"), vec!["after"]);
}

#[test]
fn a_database_from_a_newer_version_is_refused_untouched() {
    let db = TempDb::new("too-new");
    {
        let conn = rusqlite::Connection::open(&db.0).unwrap();
        conn.execute_batch("PRAGMA user_version = 99; CREATE TABLE marker (x);")
            .unwrap();
    }
    match Store::open(&db.0) {
        Err(Error::TooNew { found, supported }) => {
            assert_eq!(found, 99);
            assert_eq!(supported, 2);
        }
        other => panic!("expected TooNew, got {other:?}"),
    }
    // Refusing must not have modified it.
    let conn = rusqlite::Connection::open(&db.0).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 99);
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'messages'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}

#[test]
fn opening_twice_does_not_re_run_the_schema() {
    let db = TempDb::new("idempotent");
    drop(Store::open(&db.0).unwrap());
    drop(Store::open(&db.0).unwrap());
    let mut s = Store::open(&db.0).unwrap();
    s.log_message(&msg("#c", "a", "fine", 1000)).unwrap();
}

// ---- scale ---------------------------------------------------------------------

#[test]
fn a_large_log_loads_quickly_and_stays_searchable_and_consistent() {
    let mut s = store();
    let batch: Vec<NewMessage> = (0..20_000)
        .map(|i| {
            let text = if i % 1000 == 0 {
                format!("the needle appears at {i}")
            } else {
                format!("ordinary chatter number {i} about nothing")
            };
            with_id(
                msg(if i % 2 == 0 { "#even" } else { "#odd" }, "a", &text, i),
                &format!("id{i}"),
            )
        })
        .collect();

    let started = Instant::now();
    assert_eq!(s.log_messages(&batch).unwrap(), 20_000);
    let loaded = started.elapsed();

    let started = Instant::now();
    let found = hits(&s, "needle");
    let searched = started.elapsed();

    assert_eq!(found.len(), 20);
    // Generous bounds: this checks for a pathological slowdown, not a benchmark.
    assert!(
        loaded.as_secs() < 30,
        "loading 20k messages took {loaded:?}"
    );
    assert!(
        searched.as_secs() < 2,
        "searching 20k messages took {searched:?}"
    );

    // Replaying everything is all duplicates.
    assert_eq!(s.log_messages(&batch).unwrap(), 0);
    s.check_index().unwrap();
}
