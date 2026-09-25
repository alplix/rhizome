//! The whole path a message takes: server line, through the connection
//! engine's session, into the store, and back out through search.
//!
//! The store does not depend on the engine (only this test does), so the
//! mapping from the engine's events to the store's input is written out here.
//! The desktop app will carry the same ten lines.

use rhizome_client::{ChatMessage, Config, Event, MessageKind, Session};
use rhizome_proto::Message;
use rhizome_store::{Kind, NewMessage, SearchOptions, Store};

const NET: &str = "Libera.Chat";

fn to_new_message(network: &str, m: &ChatMessage, received_ms: i64) -> NewMessage {
    NewMessage {
        network: network.to_owned(),
        buffer: m.buffer.clone(),
        sender: m.sender.clone(),
        kind: match m.kind {
            MessageKind::Privmsg => Kind::Privmsg,
            MessageKind::Notice => Kind::Notice,
            MessageKind::Action => Kind::Action,
        },
        text: m.text.clone(),
        server_time: m.time.clone(),
        received_ms,
        msgid: m.msgid.clone(),
        own: m.own,
        highlight: m.highlight,
    }
}

/// Feeds a transcript to a fresh session and returns the messages it reports.
fn play(lines: &[&str]) -> (Session, Vec<ChatMessage>) {
    let mut session = Session::new(Config::new("irc.libera.chat", "alp"));
    session.start();
    let mut messages = Vec::new();
    for line in lines {
        let out = session.handle(&Message::parse(line).unwrap());
        for event in out.events {
            if let Event::Message(m) = event {
                messages.push(m);
            }
        }
    }
    (session, messages)
}

const TRANSCRIPT: &[&str] = &[
    ":srv CAP * LS :",
    ":srv 001 alp :Welcome",
    ":srv 005 alp CHANTYPES=# PREFIX=(ov)@+ NETWORK=Libera.Chat :are supported",
    ":alp!~alp@host JOIN #rhizome",
    "@time=2026-09-25T10:00:00.000Z;msgid=m1 :bob!b@h PRIVMSG #rhizome :kernel panic: null pointer dereference in kmalloc_array",
    "@time=2026-09-25T10:00:05.000Z;msgid=m2 :carol!c@h PRIVMSG #rhizome :Merhaba dünya, şu Türkçe hatayı gördüm",
    "@time=2026-09-25T10:00:09.000Z;msgid=m3 :bob!b@h PRIVMSG #rhizome :alp: did you capture the \u{2}backtrace\u{2}?",
    "@time=2026-09-25T10:00:12.000Z;msgid=m4 :carol!c@h PRIVMSG #rhizome :\u{1}ACTION facepalms at the backtrace\u{1}",
    "@time=2026-09-25T10:01:00.000Z;msgid=m5 :dave!d@h PRIVMSG alp :psst, check the backtrace I mailed you",
];

#[test]
fn a_conversation_flows_from_the_wire_into_search() {
    let (mut session, messages) = play(TRANSCRIPT);
    assert_eq!(messages.len(), 5);

    // One message of our own, sent while the server does not echo.
    let out = session.send_message("#rhizome", "yes, attaching it now", MessageKind::Privmsg);
    let own: Vec<ChatMessage> = out
        .events
        .into_iter()
        .filter_map(|e| match e {
            Event::Message(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(own.len(), 1);
    assert!(own[0].own);

    let mut store = Store::open_in_memory().unwrap();
    for m in messages.iter().chain(own.iter()) {
        store
            .log_message(&to_new_message(NET, m, 1_790_330_500_000))
            .unwrap();
    }

    // Two conversations: the channel, and the private one keyed by the sender.
    let names: Vec<String> = store
        .buffers(NET)
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"#rhizome".to_owned()) && names.contains(&"dave".to_owned()));

    // "Who mentioned the backtrace": a plain word, across channel and query,
    // including the action and the one wrapped in a bold code.
    let found = store
        .search("backtrace", &SearchOptions::default())
        .unwrap();
    assert_eq!(found.len(), 3);
    let buffers: Vec<&str> = found.iter().map(|h| h.message.buffer.as_str()).collect();
    assert!(buffers.contains(&"#rhizome") && buffers.contains(&"dave"));

    // The action is stored as an action, with its text unwrapped from CTCP.
    let action = found
        .iter()
        .find(|h| h.message.kind == Kind::Action)
        .unwrap();
    assert_eq!(action.message.text, "facepalms at the backtrace");
    assert_eq!(action.message.sender, "carol");

    // The highlight the session computed is what the log remembers.
    let mention = found
        .iter()
        .find(|h| h.message.msgid.as_deref() == Some("m3"))
        .unwrap();
    assert!(mention.message.highlight);
    assert!(
        mention.message.text.contains('\u{2}'),
        "raw text keeps its bold codes"
    );

    // Narrowing with the filters a developer would actually type.
    let by_carol = store
        .search("backtrace from:carol", &SearchOptions::default())
        .unwrap();
    assert_eq!(by_carol.len(), 1);
    let private = store
        .search("backtrace in:dave", &SearchOptions::default())
        .unwrap();
    assert_eq!(private[0].message.sender, "dave");

    // Turkish, typed without Turkish characters.
    let turkish = store
        .search("dunya hatayi", &SearchOptions::default())
        .unwrap();
    assert!(
        turkish.is_empty(),
        "'hatayı' has a dotless ı, which is not folded to i"
    );
    let turkish = store
        .search("dunya turkce", &SearchOptions::default())
        .unwrap();
    assert_eq!(turkish.len(), 1);
    assert_eq!(turkish[0].message.sender, "carol");

    // An identifier stays whole.
    assert_eq!(
        store
            .search("kmalloc_array", &SearchOptions::default())
            .unwrap()
            .len(),
        1
    );

    // Our own message, stamped with its arrival time since it has no server time.
    let ours = store
        .search("attaching", &SearchOptions::default())
        .unwrap();
    assert!(ours[0].message.own);
    assert_eq!(ours[0].message.time_ms, 1_790_330_500_000);

    // Server timestamps put the channel in order regardless of arrival order.
    let channel = store.scrollback(NET, "#rhizome", None, 20).unwrap();
    let times: Vec<i64> = channel.iter().map(|m| m.time_ms).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted);
    store.check_index().unwrap();
}

#[test]
fn a_reconnect_that_replays_history_does_not_duplicate_the_log() {
    let mut store = Store::open_in_memory().unwrap();

    let (_, first) = play(TRANSCRIPT);
    for m in &first {
        store.log_message(&to_new_message(NET, m, 0)).unwrap();
    }
    let after_first = store.message_count().unwrap();
    assert_eq!(after_first, 5);

    // The connection drops and the server replays the same window on return.
    let (_, replay) = play(TRANSCRIPT);
    let batch: Vec<NewMessage> = replay.iter().map(|m| to_new_message(NET, m, 0)).collect();
    assert_eq!(store.log_messages(&batch).unwrap(), 0);
    assert_eq!(store.message_count().unwrap(), after_first);

    // Genuinely new messages in the overlap window are still added.
    let mut more: Vec<&str> = TRANSCRIPT.to_vec();
    more.push("@time=2026-09-25T10:02:00.000Z;msgid=m6 :bob!b@h PRIVMSG #rhizome :one more thing");
    let (_, extended) = play(&more);
    let batch: Vec<NewMessage> = extended.iter().map(|m| to_new_message(NET, m, 0)).collect();
    assert_eq!(store.log_messages(&batch).unwrap(), 1);
    assert_eq!(store.message_count().unwrap(), 6);
}
