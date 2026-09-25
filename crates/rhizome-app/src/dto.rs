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
use rhizome_store::{Cursor, Kind, SearchHit, StoredMessage, MARK_END, MARK_START};
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
        }
    }

    pub fn from_stored(m: &StoredMessage) -> UiMessage {
        UiMessage {
            id: Some(m.id),
            network: m.network.clone(),
            buffer: m.buffer.clone(),
            sender: m.sender.clone(),
            kind: m.kind.into(),
            spans: spans(&m.text),
            plain: format::strip(&m.text),
            time_ms: m.time_ms,
            own: m.own,
            highlight: m.highlight,
            msgid: m.msgid.clone(),
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
            Event::Topic { channel, topic } => UiEvent::Topic {
                channel: channel.clone(),
                topic: topic.clone(),
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
        assert_eq!(s[0].text, "<img src=x onerror=alert(1)><script>alert(2)</script>");
    }

    #[test]
    fn snippet_markers_split_into_parts() {
        let s = format!("see {MARK_START}needle{MARK_END} in [the] <b>hay</b>");
        assert_eq!(
            snippet_parts(&s),
            vec![
                SnippetPart { text: "see ".into(), hit: false },
                SnippetPart { text: "needle".into(), hit: true },
                SnippetPart { text: " in [the] <b>hay</b>".into(), hit: false },
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
        assert_eq!(UiMessage::from_chat("net", &chat, 5).time_ms, 1_790_330_400_000);
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
}
