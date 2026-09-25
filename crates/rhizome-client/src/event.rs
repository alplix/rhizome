//! What the client reports to whatever is driving it.
//!
//! Events are the whole interface between the engine and a UI. A UI never
//! reads socket state or calls back into the session; it renders events in
//! the order they arrive. That keeps every front end (the terminal example,
//! the Tauri window, a future headless logger) a thin consumer of the same
//! stream.

use std::time::Duration;

/// How a message was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// An ordinary message.
    Privmsg,
    /// A `NOTICE`, which by convention must not be auto-replied to.
    Notice,
    /// A `/me` action.
    Action,
}

/// One member of a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub nick: String,
    /// Membership prefixes held, most privileged first (for example `@+`).
    /// Empty for a plain member.
    pub prefixes: String,
}

impl Member {
    /// The most privileged prefix, for display beside the nick.
    pub fn top_prefix(&self) -> Option<char> {
        self.prefixes.chars().next()
    }
}

/// A message to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// The conversation it belongs to: a channel name, or for a private
    /// message the other person's nick. Sending yourself a message in a
    /// channel and receiving your own echo both land in the same buffer.
    pub buffer: String,
    pub sender: String,
    /// The text, still carrying any mIRC formatting codes. Decode with
    /// `rhizome_proto::format::parse`, or strip with `format::strip`.
    pub text: String,
    pub kind: MessageKind,
    /// The server's timestamp (IRCv3 `server-time`), as ISO 8601. Absent when
    /// the server does not provide one; use the arrival time then.
    pub time: Option<String>,
    /// The server-assigned message id, when the network provides one.
    pub msgid: Option<String>,
    /// Whether we sent it.
    pub own: bool,
    /// Whether it should draw attention: it names us, or it is a private
    /// message from someone else.
    pub highlight: bool,
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Opening a connection.
    Connecting,
    /// The transport (and TLS, if used) is up. Registration is still under
    /// way.
    Connected,
    /// Registration finished; the connection is usable.
    Registered {
        nick: String,
    },
    /// The server told us the network's name.
    Network(String),
    /// The connection ended. `retry_in` is `Some` when a reconnect is
    /// scheduled and `None` when the client has given up.
    Disconnected {
        reason: String,
        retry_in: Option<Duration>,
    },

    Message(ChatMessage),
    /// Informational text from the server itself: the message of the day,
    /// server notices and the like.
    Server(String),
    /// A CTCP request or reply that is not an ordinary `/me`. Requests we
    /// know how to answer (`VERSION`, `PING`) are answered automatically.
    Ctcp {
        from: String,
        command: String,
        params: Option<String>,
        reply: bool,
    },

    /// We joined a channel.
    Joined {
        channel: String,
    },
    /// We left a channel.
    Parted {
        channel: String,
        reason: Option<String>,
    },
    /// We were removed from a channel.
    Kicked {
        channel: String,
        by: String,
        reason: Option<String>,
    },
    MemberJoined {
        channel: String,
        nick: String,
        /// The services account, if the network shares it.
        account: Option<String>,
    },
    MemberParted {
        channel: String,
        nick: String,
        reason: Option<String>,
    },
    MemberKicked {
        channel: String,
        nick: String,
        by: String,
        reason: Option<String>,
    },
    /// Someone left the network. `channels` lists the ones we shared with
    /// them, so a UI can show the quit in the right places.
    MemberQuit {
        nick: String,
        reason: Option<String>,
        channels: Vec<String>,
    },
    NickChanged {
        old: String,
        new: String,
        channels: Vec<String>,
        /// Whether the nick that changed was ours.
        own: bool,
    },
    /// A channel's topic. `by` is who changed it, and is present only for a
    /// change made while we watched. The reply the server sends when we join
    /// carries no author, so a UI can show the topic without announcing a
    /// change that did not happen.
    Topic {
        channel: String,
        topic: Option<String>,
        by: Option<String>,
    },
    /// The full member list of a channel, ordered by privilege then name.
    Names {
        channel: String,
        members: Vec<Member>,
    },
    /// A mode change, as sent: `modes` is the mode string followed by its
    /// arguments (`+o alp`).
    Mode {
        target: String,
        by: String,
        modes: String,
    },

    /// The server rejected something. `code` is the numeric reply, or `0` for
    /// a problem detected locally.
    Error {
        code: u16,
        text: String,
    },
    /// The server sent `ERROR`, which precedes it closing the connection.
    ServerError(String),
    /// Authentication failed. The client does not retry: repeating a wrong
    /// password only risks locking the account.
    AuthFailed(String),
}
