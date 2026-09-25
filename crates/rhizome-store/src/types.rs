//! The values that go into and come out of the store.

/// Wraps the part of a search snippet that matched. These are private-use
/// characters, not markup, so message text that happens to contain `<b>` or
/// `[` cannot be mistaken for a highlight. A UI replaces them with whatever
/// styling it likes.
pub const MARK_START: char = '\u{E000}';
/// See [`MARK_START`].
pub const MARK_END: char = '\u{E001}';

/// What a logged line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Privmsg,
    Notice,
    Action,
    /// Something that happened in the conversation rather than something said
    /// in it: a join, a part, a topic change. The store keeps it so the
    /// scrollback reads as it did, but treats it differently from chat: it is
    /// never searched, never indexed, and never counts as unread. Its `text` is
    /// opaque to the store; the caller decides how to encode and show it.
    Event,
}

impl Kind {
    pub(crate) fn to_db(self) -> i64 {
        match self {
            Kind::Privmsg => 0,
            Kind::Notice => 1,
            Kind::Action => 2,
            Kind::Event => EVENT_KIND,
        }
    }

    /// An unknown value reads as an ordinary message: a row written by a
    /// future version should still be shown, not dropped.
    pub(crate) fn from_db(value: i64) -> Kind {
        match value {
            1 => Kind::Notice,
            2 => Kind::Action,
            EVENT_KIND => Kind::Event,
            _ => Kind::Privmsg,
        }
    }

    /// Whether this is something a person wrote, as opposed to an [`Event`].
    ///
    /// [`Event`]: Kind::Event
    pub fn is_chat(self) -> bool {
        self != Kind::Event
    }
}

/// The database value for [`Kind::Event`]. Every kind below it is chat; the
/// search index and the unread counts rely on that ordering (`kind < 3`).
pub(crate) const EVENT_KIND: i64 = 3;

/// A message to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    /// The network's name, as reported by the server (`Libera.Chat`), or any
    /// stable label the caller chooses.
    pub network: String,
    /// The conversation: a channel name, or the other person's nick for a
    /// private message. Compared case-insensitively.
    pub buffer: String,
    pub sender: String,
    pub kind: Kind,
    /// The text as received, formatting codes included.
    pub text: String,
    /// The server's ISO 8601 timestamp (IRCv3 `server-time`), if it sent one.
    pub server_time: Option<String>,
    /// When the message arrived, in milliseconds since the Unix epoch. Used as
    /// the message's time when the server gave none or gave one that cannot be
    /// read.
    pub received_ms: i64,
    /// The server-assigned message id. When present it makes recording
    /// idempotent: replaying history that overlaps what is already stored adds
    /// nothing.
    pub msgid: Option<String>,
    pub own: bool,
    pub highlight: bool,
}

/// A message read back from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    pub network: String,
    /// The buffer's name as most recently seen, in its original case.
    pub buffer: String,
    /// Milliseconds since the Unix epoch.
    pub time_ms: i64,
    pub sender: String,
    pub kind: Kind,
    /// The text as received, formatting codes included.
    pub text: String,
    pub own: bool,
    pub highlight: bool,
    pub msgid: Option<String>,
}

impl StoredMessage {
    /// A position in the log, for paging backwards from this message.
    pub fn cursor(&self) -> Cursor {
        Cursor {
            time_ms: self.time_ms,
            id: self.id,
        }
    }
}

/// A position in a buffer's history.
///
/// Messages are ordered by time and then by insertion order, so two messages
/// in the same millisecond still have a defined order and paging never skips
/// or repeats one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub time_ms: i64,
    pub id: i64,
}

/// A conversation that is in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferInfo {
    pub network: String,
    pub name: String,
    /// How many chat messages it holds (events are not counted).
    pub messages: i64,
    /// The time of the newest chat message, if any.
    pub last_time_ms: Option<i64>,
    /// Chat messages from other people since it was last marked read.
    pub unread: i64,
    /// How many of those [`unread`](BufferInfo::unread) messages were flagged
    /// as needing attention.
    pub highlights: i64,
}

/// How to order search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchOrder {
    /// Best match first. Good for looking up a rare word.
    #[default]
    Relevance,
    /// Newest first. Good for "what was said about this recently".
    Newest,
}

/// Search settings that are not part of the typed query.
///
/// A `from:` or `in:` written in the query itself takes precedence over the
/// matching field here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    /// Restrict to one network.
    pub network: Option<String>,
    /// Restrict to one buffer.
    pub buffer: Option<String>,
    /// Restrict to one sender.
    pub from: Option<String>,
    pub order: SearchOrder,
    /// The most results to return. Clamped to 1..=1000.
    pub limit: usize,
}

impl Default for SearchOptions {
    fn default() -> SearchOptions {
        SearchOptions {
            network: None,
            buffer: None,
            from: None,
            order: SearchOrder::Relevance,
            limit: 50,
        }
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub message: StoredMessage,
    /// A short excerpt around the match, with the matched words wrapped in
    /// [`MARK_START`] and [`MARK_END`]. For a search with no words (only
    /// filters) this is simply the start of the message.
    pub snippet: String,
}
