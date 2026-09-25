//! The shapes that cross the boundary to the web UI.
//!
//! The engine and store use their own types; these are what the interface
//! receives as JSON. Keeping them separate means the engine never needs to know
//! about serialisation, and the interface contract is written down in one
//! place.
//!
//! Message text is turned into styled spans here, from the formatting codes,
//! so that the interface never parses control characters and never builds
//! markup out of text that a stranger on the network controls.

use rhizome_client::{ChatMessage, Event, Member, MessageKind};
use rhizome_proto::format::{self, Color};
use rhizome_store::{Cursor, Kind, NewMessage, SearchHit, StoredMessage, MARK_END, MARK_START};
use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

/// A run of text sharing one style. Absent fields mean "off" or "default", so
/// plain text costs only its `text`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiSpan {
    pub text: String,
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub strike: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub mono: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub reverse: bool,
    /// `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
}

fn hex(color: Color) -> Option<String> {
    // The extended palette (16-98) and "default" (99) resolve to nothing, so
    // they render in the theme's own colours rather than in guessed ones.
    color
        .to_rgb()
        .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
}

/// Decodes a message body into styled spans.
pub fn spans(text: &str) -> Vec<UiSpan> {
    format::parse(text)
        .into_iter()
        .map(|span| UiSpan {
            text: span.text,
            bold: span.style.bold,
            italic: span.style.italic,
            underline: span.style.underline,
            strike: span.style.strikethrough,
            mono: span.style.monospace,
            reverse: span.style.reverse,
            fg: span.style.fg.and_then(hex),
            bg: span.style.bg.and_then(hex),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiKind {
    Privmsg,
    Notice,
    Action,
    /// Something that happened rather than something said: a join, a part.
    /// Such a message carries [`UiMessage::event`] instead of text.
    Event,
}

impl From<MessageKind> for UiKind {
    fn from(kind: MessageKind) -> UiKind {
        match kind {
            MessageKind::Privmsg => UiKind::Privmsg,
            MessageKind::Notice => UiKind::Notice,
            MessageKind::Action => UiKind::Action,
        }
    }
}

impl From<Kind> for UiKind {
    fn from(kind: Kind) -> UiKind {
        match kind {
            Kind::Privmsg => UiKind::Privmsg,
            Kind::Notice => UiKind::Notice,
            Kind::Action => UiKind::Action,
            Kind::Event => UiKind::Event,
        }
    }
}

/// A message to show, whether it arrived just now or was read from the log.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiMessage {
    /// The log's id for it. Absent for a message that has only just arrived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub network: String,
    pub buffer: String,
    pub sender: String,
    pub kind: UiKind,
    pub spans: Vec<UiSpan>,
    /// The text with formatting removed, for copying and notifications.
    pub plain: String,
    pub time_ms: i64,
    pub own: bool,
    pub highlight: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msgid: Option<String>,
    /// For an event line: what happened. The interface words it in the person's
    /// language, so this carries a verb and its arguments and no prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<UiEventLine>,
}

/// What an event line says, without saying it in any language.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiEventLine {
    /// `join`, `part`, `quit`, `kick`, `nick`, `topic` or `mode`.
    pub verb: String,
    pub args: Vec<String>,
}

/// Packs an event into the text the log stores for it: a JSON array, verb first.
///
/// JSON rather than a separator character, because the arguments are text from
/// strangers (a quit reason, a topic) that can contain any character.
pub fn encode_event(verb: &str, args: &[String]) -> String {
    let mut parts = vec![verb.to_owned()];
    parts.extend(args.iter().cloned());
    serde_json::to_string(&parts).expect("a list of strings always serialises")
}

/// The inverse of [`encode_event`]. Text that is not one (a row written by a
/// different version) decodes to the verb `unknown`, so a bad row is shown as
/// something rather than breaking the history it is in.
pub fn decode_event(text: &str) -> UiEventLine {
    match serde_json::from_str::<Vec<String>>(text) {
        Ok(mut parts) if !parts.is_empty() => {
            let verb = parts.remove(0);
            UiEventLine { verb, args: parts }
        }
        _ => UiEventLine {
            verb: "unknown".to_owned(),
            args: Vec::new(),
        },
    }
}

/// Something that happened in a conversation, ready to be shown and logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    pub buffer: String,
    /// Who did it (the actor), or for a join, part or quit, who it happened to.
    pub sender: String,
    pub verb: &'static str,
    pub args: Vec<String>,
    /// Whether it concerns us: we joined, we left, we were removed, we renamed.
    pub own: bool,
}

impl EventLine {
    pub fn to_new_message(&self, network: &str, now_ms: i64) -> NewMessage {
        NewMessage {
            network: network.to_owned(),
            buffer: self.buffer.clone(),
            sender: self.sender.clone(),
            kind: Kind::Event,
            text: encode_event(self.verb, &self.args),
            server_time: None,
            received_ms: now_ms,
            msgid: None,
            own: self.own,
            highlight: false,
        }
    }

    pub fn to_ui(&self, network: &str, now_ms: i64) -> UiMessage {
        UiMessage {
            id: None,
            network: network.to_owned(),
            buffer: self.buffer.clone(),
            sender: self.sender.clone(),
            kind: UiKind::Event,
            spans: Vec::new(),
            plain: String::new(),
            time_ms: now_ms,
            own: self.own,
            highlight: false,
            msgid: None,
            event: Some(UiEventLine {
                verb: self.verb.to_owned(),
                args: self.args.clone(),
            }),
        }
    }
}

fn is_channel(name: &str) -> bool {
    name.starts_with(['#', '&', '!', '+'])
}

fn plain(text: &str) -> String {
    format::strip(text)
}

/// The lines an engine event should leave in the history.
///
/// `own_nick` is our nick, for the events that are about us. Most events give
/// one line; a quit or a nick change is reported by the network once but
/// belongs in every channel the person was in, so it gives one per channel.
/// Events that are only state (a member list, a connection change) give none.
pub fn event_lines(event: &Event, own_nick: &str) -> Vec<EventLine> {
    let line =
        |buffer: &str, sender: &str, verb: &'static str, args: Vec<String>, own: bool| EventLine {
            buffer: buffer.to_owned(),
            sender: sender.to_owned(),
            verb,
            args,
            own,
        };
    let reason = |r: &Option<String>| plain(r.as_deref().unwrap_or(""));

    match event {
        Event::Joined { channel } => vec![line(channel, own_nick, "join", vec![], true)],
        Event::Parted { channel, reason: r } => {
            vec![line(channel, own_nick, "part", vec![reason(r)], true)]
        }
        Event::Kicked {
            channel,
            by,
            reason: r,
        } => {
            vec![line(
                channel,
                by,
                "kick",
                vec![own_nick.to_owned(), reason(r)],
                true,
            )]
        }
        Event::MemberJoined { channel, nick, .. } => {
            vec![line(channel, nick, "join", vec![], false)]
        }
        Event::MemberParted {
            channel,
            nick,
            reason: r,
        } => {
            vec![line(channel, nick, "part", vec![reason(r)], false)]
        }
        Event::MemberKicked {
            channel,
            nick,
            by,
            reason: r,
        } => {
            vec![line(
                channel,
                by,
                "kick",
                vec![nick.clone(), reason(r)],
                false,
            )]
        }
        Event::MemberQuit {
            nick,
            reason: r,
            channels,
        } => channels
            .iter()
            .map(|c| line(c, nick, "quit", vec![reason(r)], false))
            .collect(),
        Event::NickChanged {
            old,
            new,
            channels,
            own,
        } => channels
            .iter()
            .map(|c| line(c, old, "nick", vec![new.clone()], *own))
            .collect(),
        // A topic reply on joining has no author and is not a change.
        Event::Topic {
            channel,
            topic,
            by: Some(by),
        } => vec![line(
            channel,
            by,
            "topic",
            vec![plain(topic.as_deref().unwrap_or(""))],
            false,
        )],
        Event::Mode { target, by, modes } if is_channel(target) => {
            vec![line(target, by, "mode", vec![modes.clone()], false)]
        }
        _ => Vec::new(),
    }
}

impl UiMessage {
    /// A live message. `now_ms` stands in for the time when the server gave none.
    pub fn from_chat(network: &str, m: &ChatMessage, now_ms: i64) -> UiMessage {
        UiMessage {
            id: None,
            network: network.to_owned(),
            buffer: m.buffer.clone(),
            sender: m.sender.clone(),
            kind: m.kind.into(),
            spans: spans(&m.text),
            plain: format::strip(&m.text),
            time_ms: m
                .time
                .as_deref()
                .and_then(rhizome_store::time::parse_server_time)
                .unwrap_or(now_ms),
            own: m.own,
            highlight: m.highlight,
            msgid: m.msgid.clone(),
            event: None,
        }
    }

    pub fn from_stored(m: &StoredMessage) -> UiMessage {
        let is_event = m.kind == Kind::Event;
        UiMessage {
            id: Some(m.id),
            network: m.network.clone(),
            buffer: m.buffer.clone(),
            sender: m.sender.clone(),
            kind: m.kind.into(),
            // An event's text is a packed description, not something to render.
            spans: if is_event { Vec::new() } else { spans(&m.text) },
            plain: if is_event {
                String::new()
            } else {
                format::strip(&m.text)
            },
            time_ms: m.time_ms,
            own: m.own,
            highlight: m.highlight,
            msgid: m.msgid.clone(),
            event: is_event.then(|| decode_event(&m.text)),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiMember {
    pub nick: String,
    pub prefixes: String,
}

impl From<&Member> for UiMember {
    fn from(m: &Member) -> UiMember {
        UiMember {
            nick: m.nick.clone(),
            prefixes: m.prefixes.clone(),
        }
    }
}

/// One piece of a search snippet: text, and whether it is the matched part.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SnippetPart {
    pub text: String,
    pub hit: bool,
}

/// Splits a snippet on the store's match markers.
///
/// The markers are private-use characters, so text that merely contains
/// brackets or tags is never mistaken for a highlight, and the interface can
/// style the parts without parsing anything.
pub fn snippet_parts(snippet: &str) -> Vec<SnippetPart> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut hit = false;
    for c in snippet.chars() {
        if c == MARK_START || c == MARK_END {
            if !current.is_empty() {
                parts.push(SnippetPart {
                    text: std::mem::take(&mut current),
                    hit,
                });
            }
            hit = c == MARK_START;
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        parts.push(SnippetPart { text: current, hit });
    }
    parts
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiHit {
    pub message: UiMessage,
    pub snippet: Vec<SnippetPart>,
}

impl From<&SearchHit> for UiHit {
    fn from(hit: &SearchHit) -> UiHit {
        UiHit {
            message: UiMessage::from_stored(&hit.message),
            snippet: snippet_parts(&hit.snippet),
        }
    }
}

/// A position in a buffer's history, as the interface holds it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiCursor {
    pub time_ms: i64,
    pub id: i64,
}

impl From<UiCursor> for Cursor {
    fn from(c: UiCursor) -> Cursor {
        Cursor {
            time_ms: c.time_ms,
            id: c.id,
        }
    }
}

impl From<Cursor> for UiCursor {
    fn from(c: Cursor) -> UiCursor {
        UiCursor {
            time_ms: c.time_ms,
            id: c.id,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UiBuffer {
    pub name: String,
    pub messages: i64,
    pub last_time_ms: Option<i64>,
    /// Messages from others since the conversation was last marked read.
    pub unread: i64,
    /// How many of those mention us or are private messages.
    pub highlights: i64,
}

/// What happened, in the interface's terms.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Connecting,
    Connected,
    Registered {
        nick: String,
    },
    Network {
        name: String,
    },
    Disconnected {
        reason: String,
        /// Milliseconds until the next attempt, or absent if it has given up.
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_in_ms: Option<u64>,
    },
    /// The connection task has ended for good; nothing more will arrive.
    Closed,

    Message {
        message: UiMessage,
    },
    Server {
        text: String,
    },
    Ctcp {
        from: String,
        command: String,
        reply: bool,
    },

    Joined {
        channel: String,
    },
    Parted {
        channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Kicked {
        channel: String,
        by: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    MemberJoined {
        channel: String,
        nick: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    MemberParted {
        channel: String,
        nick: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    MemberKicked {
        channel: String,
        nick: String,
        by: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    MemberQuit {
        nick: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        channels: Vec<String>,
    },
    NickChanged {
        old: String,
        new: String,
        channels: Vec<String>,
        own: bool,
    },
    Topic {
        channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        topic: Option<String>,
        /// Who changed it; absent for the topic reported on joining.
        #[serde(skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    Names {
        channel: String,
        members: Vec<UiMember>,
    },
    Mode {
        target: String,
        by: String,
        modes: String,
    },

    Error {
        code: u16,
        text: String,
    },
    ServerError {
        text: String,
    },
    AuthFailed {
        reason: String,
        /// Whether a saved password was discarded because of it, so the person
        /// is not left wondering why the next attempt asks again.
        forgot_password: bool,
    },
}

impl UiEvent {
    /// Translates an engine event. `now_ms` timestamps messages the server did
    /// not.
    pub fn from_event(network: &str, event: &Event, now_ms: i64) -> UiEvent {
        match event {
            Event::Connecting => UiEvent::Connecting,
            Event::Connected => UiEvent::Connected,
            Event::Registered { nick } => UiEvent::Registered { nick: nick.clone() },
            Event::Network(name) => UiEvent::Network { name: name.clone() },
            Event::Disconnected { reason, retry_in } => UiEvent::Disconnected {
                reason: reason.clone(),
                retry_in_ms: retry_in.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            },
            Event::Message(m) => UiEvent::Message {
                message: UiMessage::from_chat(network, m, now_ms),
            },
            Event::Server(text) => UiEvent::Server { text: text.clone() },
            Event::Ctcp {
                from,
                command,
                reply,
                ..
            } => UiEvent::Ctcp {
                from: from.clone(),
                command: command.clone(),
                reply: *reply,
            },
            Event::Joined { channel } => UiEvent::Joined {
                channel: channel.clone(),
            },
            Event::Parted { channel, reason } => UiEvent::Parted {
                channel: channel.clone(),
                reason: reason.clone(),
            },
            Event::Kicked {
                channel,
                by,
                reason,
            } => UiEvent::Kicked {
                channel: channel.clone(),
                by: by.clone(),
                reason: reason.clone(),
            },
            Event::MemberJoined {
                channel,
                nick,
                account,
            } => UiEvent::MemberJoined {
                channel: channel.clone(),
                nick: nick.clone(),
                account: account.clone(),
            },
            Event::MemberParted {
                channel,
                nick,
                reason,
            } => UiEvent::MemberParted {
                channel: channel.clone(),
                nick: nick.clone(),
                reason: reason.clone(),
            },
            Event::MemberKicked {
                channel,
                nick,
                by,
                reason,
            } => UiEvent::MemberKicked {
                channel: channel.clone(),
                nick: nick.clone(),
                by: by.clone(),
                reason: reason.clone(),
            },
            Event::MemberQuit {
                nick,
                reason,
                channels,
            } => UiEvent::MemberQuit {
                nick: nick.clone(),
                reason: reason.clone(),
                channels: channels.clone(),
            },
            Event::NickChanged {
                old,
                new,
                channels,
                own,
            } => UiEvent::NickChanged {
                old: old.clone(),
                new: new.clone(),
                channels: channels.clone(),
                own: *own,
            },
            Event::Topic { channel, topic, by } => UiEvent::Topic {
                channel: channel.clone(),
                topic: topic.clone(),
                by: by.clone(),
            },
            Event::Names { channel, members } => UiEvent::Names {
                channel: channel.clone(),
                members: members.iter().map(UiMember::from).collect(),
            },
            Event::Mode { target, by, modes } => UiEvent::Mode {
                target: target.clone(),
                by: by.clone(),
                modes: modes.clone(),
            },
            Event::Error { code, text } => UiEvent::Error {
                code: *code,
                text: text.clone(),
            },
            Event::ServerError(text) => UiEvent::ServerError { text: text.clone() },
            Event::AuthFailed(reason) => UiEvent::AuthFailed {
                reason: reason.clone(),
                forgot_password: false,
            },
        }
    }
}

/// An event tagged with the network it came from: the unit the interface
/// receives.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Envelope {
    /// The network's id, or empty for something not tied to one (a log error).
    pub network: String,
    pub event: UiEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn plain_text_is_one_span_with_nothing_else_serialised() {
        let s = spans("hello");
        assert_eq!(s.len(), 1);
        assert_eq!(serde_json::to_string(&s[0]).unwrap(), r#"{"text":"hello"}"#);
    }

    #[test]
    fn formatting_codes_become_styled_spans() {
        let s = spans("a \u{2}bold\u{2} \u{3}04red\u{3} \u{1D}it\u{1D}");
        let bold = s.iter().find(|s| s.text == "bold").unwrap();
        assert!(bold.bold);
        let red = s.iter().find(|s| s.text == "red").unwrap();
        assert_eq!(red.fg.as_deref(), Some("#ff0000"));
        assert!(s.iter().find(|s| s.text == "it").unwrap().italic);
        // No control characters survive into the text.
        assert!(s.iter().all(|s| !s.text.chars().any(|c| c.is_control())));
    }

    #[test]
    fn hex_and_extended_palette_colours_resolve_sensibly() {
        let s = spans("\u{4}ff8800orange");
        assert_eq!(s[0].fg.as_deref(), Some("#ff8800"));
        // Extended palette indices have no table, so they fall back to the
        // theme instead of a wrong colour.
        let s = spans("\u{3}50text");
        assert_eq!(s[0].fg, None);
    }

    #[test]
    fn hostile_markup_in_a_message_is_just_text() {
        let s = spans("<img src=x onerror=alert(1)><script>alert(2)</script>");
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].text,
            "<img src=x onerror=alert(1)><script>alert(2)</script>"
        );
    }

    #[test]
    fn snippet_markers_split_into_parts() {
        let s = format!("see {MARK_START}needle{MARK_END} in [the] <b>hay</b>");
        assert_eq!(
            snippet_parts(&s),
            vec![
                SnippetPart {
                    text: "see ".into(),
                    hit: false
                },
                SnippetPart {
                    text: "needle".into(),
                    hit: true
                },
                SnippetPart {
                    text: " in [the] <b>hay</b>".into(),
                    hit: false
                },
            ]
        );
        assert_eq!(snippet_parts(""), vec![]);
        assert_eq!(snippet_parts("no marks").len(), 1);
    }

    #[test]
    fn a_live_message_uses_the_server_time_and_falls_back_to_now() {
        let mut chat = ChatMessage {
            buffer: "#c".into(),
            sender: "bob".into(),
            text: "hi".into(),
            kind: MessageKind::Privmsg,
            time: Some("2026-09-25T10:00:00.000Z".into()),
            msgid: Some("m1".into()),
            own: false,
            highlight: true,
        };
        assert_eq!(
            UiMessage::from_chat("net", &chat, 5).time_ms,
            1_790_330_400_000
        );
        chat.time = None;
        assert_eq!(UiMessage::from_chat("net", &chat, 5).time_ms, 5);
        let m = UiMessage::from_chat("net", &chat, 5);
        assert_eq!(m.network, "net");
        assert!(m.highlight);
        assert_eq!(m.id, None);
    }

    #[test]
    fn events_serialise_with_a_type_tag_the_interface_can_switch_on() {
        let e = Envelope {
            network: "libera".into(),
            event: UiEvent::Disconnected {
                reason: "ping timeout".into(),
                retry_in_ms: Some(2500),
            },
        };
        let json: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(json["network"], "libera");
        assert_eq!(json["event"]["type"], "disconnected");
        assert_eq!(json["event"]["retry_in_ms"], 2500);

        let gave_up = UiEvent::Disconnected {
            reason: "x".into(),
            retry_in_ms: None,
        };
        let json = serde_json::to_value(&gave_up).unwrap();
        assert!(json.get("retry_in_ms").is_none());
    }

    #[test]
    fn a_names_event_carries_its_members() {
        let event = Event::Names {
            channel: "#c".into(),
            members: vec![Member {
                nick: "alp".into(),
                prefixes: "@".into(),
            }],
        };
        let json = serde_json::to_value(UiEvent::from_event("n", &event, 0)).unwrap();
        assert_eq!(json["type"], "names");
        assert_eq!(json["members"][0]["prefixes"], "@");
    }

    #[test]
    fn the_retry_delay_is_reported_in_milliseconds() {
        let event = Event::Disconnected {
            reason: "x".into(),
            retry_in: Some(Duration::from_millis(1500)),
        };
        match UiEvent::from_event("n", &event, 0) {
            UiEvent::Disconnected { retry_in_ms, .. } => assert_eq!(retry_in_ms, Some(1500)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cursors_round_trip_through_json() {
        let c = UiCursor { time_ms: 5, id: 9 };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<UiCursor>(&json).unwrap(), c);
        let store: Cursor = c.into();
        assert_eq!(UiCursor::from(store), c);
    }

    // ---- event lines --------------------------------------------------------

    #[test]
    fn an_event_survives_being_packed_for_the_log() {
        for args in [
            vec![],
            vec!["".to_owned()],
            vec!["going home, bye".to_owned()],
            vec![
                "\"quoted\" \\ back\\slash".to_owned(),
                "line\nbreak\ttab".to_owned(),
            ],
            vec!["Türkçe şğüöç İ ı 🎉".to_owned()],
            vec!["a".repeat(2000)],
        ] {
            let text = encode_event("part", &args);
            let back = decode_event(&text);
            assert_eq!(back.verb, "part");
            assert_eq!(back.args, args, "through {text}");
        }
    }

    #[test]
    fn a_row_that_is_not_a_packed_event_still_decodes_to_something() {
        for junk in ["", "not json", "{\"a\":1}", "[]", "[1,2]", "null"] {
            let e = decode_event(junk);
            assert_eq!(e.verb, "unknown", "{junk:?}");
            assert!(e.args.is_empty());
        }
    }

    fn lines_of(event: Event) -> Vec<(String, String, &'static str, Vec<String>, bool)> {
        event_lines(&event, "alp")
            .into_iter()
            .map(|l| (l.buffer, l.sender, l.verb, l.args, l.own))
            .collect()
    }

    fn s(x: &str) -> String {
        x.to_owned()
    }

    #[test]
    fn joins_and_parts_become_lines_about_the_right_person() {
        assert_eq!(
            lines_of(Event::Joined { channel: s("#c") }),
            vec![(s("#c"), s("alp"), "join", vec![], true)],
            "our own join names us and is flagged"
        );
        assert_eq!(
            lines_of(Event::MemberJoined {
                channel: s("#c"),
                nick: s("bob"),
                account: None
            }),
            vec![(s("#c"), s("bob"), "join", vec![], false)]
        );
        assert_eq!(
            lines_of(Event::Parted {
                channel: s("#c"),
                reason: Some(s("\u{2}bye\u{2}"))
            }),
            vec![(s("#c"), s("alp"), "part", vec![s("bye")], true)],
            "formatting codes are stripped from reasons"
        );
        assert_eq!(
            lines_of(Event::MemberParted {
                channel: s("#c"),
                nick: s("bob"),
                reason: None
            }),
            vec![(s("#c"), s("bob"), "part", vec![s("")], false)]
        );
    }

    #[test]
    fn a_kick_names_the_actor_and_the_victim() {
        assert_eq!(
            lines_of(Event::MemberKicked {
                channel: s("#c"),
                nick: s("bob"),
                by: s("op"),
                reason: Some(s("spam"))
            }),
            vec![(s("#c"), s("op"), "kick", vec![s("bob"), s("spam")], false)]
        );
        assert_eq!(
            lines_of(Event::Kicked {
                channel: s("#c"),
                by: s("op"),
                reason: None
            }),
            vec![(s("#c"), s("op"), "kick", vec![s("alp"), s("")], true)],
            "when it is us, the victim is us"
        );
    }

    #[test]
    fn a_quit_or_nick_change_leaves_a_line_in_every_shared_channel() {
        let quit = lines_of(Event::MemberQuit {
            nick: s("bob"),
            reason: Some(s("Ping timeout")),
            channels: vec![s("#a"), s("#b")],
        });
        assert_eq!(quit.len(), 2);
        assert_eq!(
            quit[0],
            (s("#a"), s("bob"), "quit", vec![s("Ping timeout")], false)
        );
        assert_eq!(quit[1].0, "#b");

        let nick = lines_of(Event::NickChanged {
            old: s("bob"),
            new: s("robert"),
            channels: vec![s("#a")],
            own: false,
        });
        assert_eq!(
            nick,
            vec![(s("#a"), s("bob"), "nick", vec![s("robert")], false)]
        );
        let mine = lines_of(Event::NickChanged {
            old: s("alp"),
            new: s("alp2"),
            channels: vec![s("#a")],
            own: true,
        });
        assert!(mine[0].4);

        // Nobody in common: nothing to record anywhere.
        assert!(lines_of(Event::MemberQuit {
            nick: s("x"),
            reason: None,
            channels: vec![]
        })
        .is_empty());
    }

    #[test]
    fn a_topic_change_is_a_line_but_the_topic_shown_on_joining_is_not() {
        assert_eq!(
            lines_of(Event::Topic {
                channel: s("#c"),
                topic: Some(s("new topic")),
                by: Some(s("op"))
            }),
            vec![(s("#c"), s("op"), "topic", vec![s("new topic")], false)]
        );
        assert!(lines_of(Event::Topic {
            channel: s("#c"),
            topic: Some(s("x")),
            by: None
        })
        .is_empty());
        // Clearing a topic is a change with nothing in it.
        assert_eq!(
            lines_of(Event::Topic {
                channel: s("#c"),
                topic: None,
                by: Some(s("op"))
            })[0]
                .3,
            vec![s("")]
        );
    }

    #[test]
    fn channel_modes_are_lines_but_user_modes_are_not() {
        assert_eq!(
            lines_of(Event::Mode {
                target: s("#c"),
                by: s("op"),
                modes: s("+o bob")
            }),
            vec![(s("#c"), s("op"), "mode", vec![s("+o bob")], false)]
        );
        assert!(lines_of(Event::Mode {
            target: s("alp"),
            by: s("alp"),
            modes: s("+i")
        })
        .is_empty());
    }

    #[test]
    fn state_only_events_leave_no_line() {
        for e in [
            Event::Connecting,
            Event::Connected,
            Event::Registered { nick: s("alp") },
            Event::Names {
                channel: s("#c"),
                members: vec![],
            },
            Event::Server(s("hello")),
            Event::Error {
                code: 1,
                text: s("x"),
            },
        ] {
            assert!(event_lines(&e, "alp").is_empty(), "{e:?}");
        }
    }

    #[test]
    fn an_event_line_serialises_as_a_verb_and_arguments() {
        let ui = event_lines(
            &Event::MemberQuit {
                nick: s("bob"),
                reason: Some(s("gone")),
                channels: vec![s("#c")],
            },
            "alp",
        )
        .remove(0)
        .to_ui("net", 42);
        let json = serde_json::to_value(&ui).unwrap();
        assert_eq!(json["kind"], "event");
        assert_eq!(json["event"]["verb"], "quit");
        assert_eq!(json["event"]["args"][0], "gone");
        assert_eq!(json["time_ms"], 42);
        assert_eq!(json["spans"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_stored_event_comes_back_as_an_event_not_as_text() {
        let line = event_lines(
            &Event::MemberParted {
                channel: s("#c"),
                nick: s("bob"),
                reason: Some(s("later")),
            },
            "alp",
        )
        .remove(0);
        let new = line.to_new_message("net", 1000);
        assert_eq!(new.kind, Kind::Event);
        let stored = StoredMessage {
            id: 7,
            network: new.network,
            buffer: new.buffer,
            time_ms: 1000,
            sender: new.sender,
            kind: new.kind,
            text: new.text,
            own: new.own,
            highlight: false,
            msgid: None,
        };
        let ui = UiMessage::from_stored(&stored);
        assert_eq!(ui.kind, UiKind::Event);
        assert_eq!(ui.id, Some(7));
        let event = ui.event.expect("the event is unpacked");
        assert_eq!(
            (event.verb.as_str(), event.args),
            ("part", vec![s("later")])
        );
        assert!(ui.spans.is_empty() && ui.plain.is_empty());
    }

    #[test]
    fn a_topic_event_reports_who_changed_it() {
        let event = Event::Topic {
            channel: s("#c"),
            topic: Some(s("t")),
            by: Some(s("op")),
        };
        let json = serde_json::to_value(UiEvent::from_event("n", &event, 0)).unwrap();
        assert_eq!(json["by"], "op");
        let join = Event::Topic {
            channel: s("#c"),
            topic: Some(s("t")),
            by: None,
        };
        assert!(serde_json::to_value(UiEvent::from_event("n", &join, 0))
            .unwrap()
            .get("by")
            .is_none());
    }
}
