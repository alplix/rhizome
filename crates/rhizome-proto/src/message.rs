//! Parsing and serialization of a single IRC protocol line.
//!
//! The grammar is RFC 1459 framing extended with the IRCv3 `message-tags`
//! section:
//!
//! ```text
//! ['@' tags SPACE] [':' source SPACE] command *[SPACE param] [SPACE ':' trailing] CRLF
//! ```
//!
//! Parsing is deliberately lenient: real networks emit runs of spaces between
//! components, omit the source, and send commands in mixed case. Anything we
//! can make sense of, we accept; we only reject a line with no command at all.

use std::fmt;

/// Maximum size of the tag section, including the leading `@` and the space
/// that terminates it (IRCv3 `message-tags`).
pub const MAX_TAGS_BYTES: usize = 8191;

/// Maximum size of everything after the tag section, including the trailing
/// CRLF (RFC 1459).
pub const MAX_MESSAGE_BYTES: usize = 512;

/// A line that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The line contained tags and/or a source but no command token.
    MissingCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingCommand => f.write_str("message has no command"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Why a message was refused by [`Message::validate_for_send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The command is empty or contains a space or control character.
    BadCommand,
    /// A tag key is empty or contains a space, `;`, `=` or control character.
    BadTag,
    /// A parameter contains CR, LF or NUL. Sending it would let the text
    /// after the line break be read as a second command.
    ForbiddenCharacter { index: usize },
    /// A parameter other than the last is empty, contains a space, or starts
    /// with `:`, so it could not be told apart from its neighbours.
    MalformedParameter { index: usize },
    /// The line exceeds the protocol size limit.
    TooLong { bytes: usize },
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::BadCommand => f.write_str("invalid command"),
            SendError::BadTag => f.write_str("invalid tag key"),
            SendError::ForbiddenCharacter { index } => {
                write!(f, "parameter {index} contains a line break or NUL")
            }
            SendError::MalformedParameter { index } => {
                write!(f, "parameter {index} cannot be sent in non-final position")
            }
            SendError::TooLong { bytes } => write!(f, "line is {bytes} bytes, over the limit"),
        }
    }
}

impl std::error::Error for SendError {}

/// Where a message came from, parsed from the `:prefix` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A server name.
    Server(String),
    /// A user, in `nick!user@host` form. `user` and `host` are frequently
    /// absent on messages the local client echoes or synthesizes.
    User {
        nick: String,
        user: Option<String>,
        host: Option<String>,
    },
}

impl Source {
    /// Parses a prefix.
    ///
    /// A prefix containing `!` is always a user. Without one the form is
    /// ambiguous, so we fall back to the convention every client uses: a
    /// prefix containing a dot is a server name, anything else is a bare nick.
    pub fn parse(s: &str) -> Source {
        if let Some((nick, rest)) = s.split_once('!') {
            let (user, host) = match rest.split_once('@') {
                Some((u, h)) => (Some(u.to_owned()), Some(h.to_owned())),
                None => (Some(rest.to_owned()), None),
            };
            Source::User {
                nick: nick.to_owned(),
                user,
                host,
            }
        } else if let Some((nick, host)) = s.split_once('@') {
            Source::User {
                nick: nick.to_owned(),
                user: None,
                host: Some(host.to_owned()),
            }
        } else if s.contains('.') {
            Source::Server(s.to_owned())
        } else {
            Source::User {
                nick: s.to_owned(),
                user: None,
                host: None,
            }
        }
    }

    /// The nick, for a user source.
    pub fn nick(&self) -> Option<&str> {
        match self {
            Source::User { nick, .. } => Some(nick),
            Source::Server(_) => None,
        }
    }

    /// The name to show in a UI: the nick for users, the server name otherwise.
    pub fn display_name(&self) -> &str {
        match self {
            Source::User { nick, .. } => nick,
            Source::Server(name) => name,
        }
    }

    /// The `nick!user@host` mask, filling in `*` for missing components.
    /// Used to size outgoing messages against the 512-byte limit.
    pub fn mask(&self) -> String {
        match self {
            Source::Server(name) => name.clone(),
            Source::User { nick, user, host } => format!(
                "{}!{}@{}",
                nick,
                user.as_deref().unwrap_or("*"),
                host.as_deref().unwrap_or("*")
            ),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Server(name) => f.write_str(name),
            Source::User { nick, user, host } => {
                f.write_str(nick)?;
                if let Some(user) = user {
                    write!(f, "!{user}")?;
                }
                if let Some(host) = host {
                    write!(f, "@{host}")?;
                }
                Ok(())
            }
        }
    }
}

/// An IRC command: either a three-digit numeric reply or a named command.
///
/// This is intentionally not an exhaustive enum of every command. The set of
/// commands a network can send is open-ended, and an exhaustive enum would
/// force this crate to change every time we teach the client a new one.
/// Dispatch belongs in the layer above; the only distinction that matters at
/// the protocol level is numeric versus named.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Command {
    /// A numeric reply, e.g. `001` (RPL_WELCOME) or `433` (ERR_NICKNAMEINUSE).
    Numeric(u16),
    /// A named command, normalized to uppercase.
    Named(String),
}

impl Command {
    /// Parses a command token, uppercasing named commands so that comparisons
    /// upstream do not have to be case-insensitive.
    pub fn parse(token: &str) -> Command {
        let bytes = token.as_bytes();
        if bytes.len() == 3 && bytes.iter().all(u8::is_ascii_digit) {
            // Cannot overflow: three ASCII digits is at most 999.
            let n = (bytes[0] - b'0') as u16 * 100
                + (bytes[1] - b'0') as u16 * 10
                + (bytes[2] - b'0') as u16;
            Command::Numeric(n)
        } else {
            Command::Named(token.to_ascii_uppercase())
        }
    }

    /// The numeric value, if this is a numeric reply.
    pub fn numeric(&self) -> Option<u16> {
        match self {
            Command::Numeric(n) => Some(*n),
            Command::Named(_) => None,
        }
    }

    /// Whether this is the given named command. The argument must already be
    /// uppercase.
    pub fn is(&self, name: &str) -> bool {
        matches!(self, Command::Named(n) if n == name)
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Numeric(n) => write!(f, "{n:03}"),
            Command::Named(name) => f.write_str(name),
        }
    }
}

/// The IRCv3 tag section, kept in wire order so that a parsed message
/// round-trips byte-for-byte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags(Vec<(String, String)>);

impl Tags {
    /// Parses the tag section, without its leading `@`.
    pub fn parse(s: &str) -> Tags {
        let mut tags = Vec::new();
        for item in s.split(';') {
            if item.is_empty() {
                continue;
            }
            let (key, value) = match item.split_once('=') {
                Some((k, v)) => (k, unescape_tag_value(v)),
                // A tag with no `=` has an empty value, which is distinct from
                // an absent tag but equal to `key=`.
                None => (item, String::new()),
            };
            tags.push((key.to_owned(), value));
        }
        Tags(tags)
    }

    /// The value of a tag, or `None` if it is absent.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Sets a tag, replacing it in place if it already exists.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => *v = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

impl fmt::Display for Tags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (key, value)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(";")?;
            }
            f.write_str(key)?;
            if !value.is_empty() {
                f.write_str("=")?;
                write_escaped_tag_value(f, value)?;
            }
        }
        Ok(())
    }
}

/// Decodes the escape sequences defined by IRCv3 `message-tags`.
///
/// A backslash before an unlisted character yields that character, and a lone
/// trailing backslash is dropped, both as the specification requires.
fn unescape_tag_value(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {} // trailing lone backslash: dropped
        }
    }
    out
}

fn write_escaped_tag_value(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            ';' => f.write_str("\\:")?,
            ' ' => f.write_str("\\s")?,
            '\\' => f.write_str("\\\\")?,
            '\r' => f.write_str("\\r")?,
            '\n' => f.write_str("\\n")?,
            _ => f.write_fmt(format_args!("{c}"))?,
        }
    }
    Ok(())
}

/// A parsed IRC message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub tags: Tags,
    pub source: Option<Source>,
    pub command: Command,
    pub params: Vec<String>,
    /// Whether the final parameter was written in trailing form, after a `:`.
    ///
    /// The wire format allows a single-word final parameter either way, so
    /// this cannot be recovered from `params` alone. Keeping it lets
    /// [`Message::to_wire`] reproduce the line byte-for-byte, which the raw
    /// protocol log depends on.
    pub trailing_form: bool,
}

impl Message {
    /// Builds a message with no tags and no source, for sending.
    ///
    /// The final parameter is quoted only if the wire format requires it, so
    /// `JOIN #chan` stays `JOIN #chan`. Use [`Message::with_body`] for
    /// commands whose last parameter is human-written text.
    pub fn new(command: &str, params: impl IntoIterator<Item = impl Into<String>>) -> Message {
        let params: Vec<String> = params.into_iter().map(Into::into).collect();
        Message {
            tags: Tags::default(),
            source: None,
            command: Command::parse(command),
            trailing_form: params.last().is_some_and(|p| requires_trailing_form(p)),
            params,
        }
    }

    /// Builds a message whose final parameter is a message body, such as
    /// `PRIVMSG` or `NOTICE`.
    ///
    /// The body is always sent in trailing form. Doing so unconditionally
    /// matters: a body that happens to be one word would otherwise be sent
    /// bare, and a body that later grows a leading `:` or becomes empty would
    /// change meaning.
    pub fn with_body(command: &str, target: &str, body: &str) -> Message {
        Message::with_trailing(command, [target], body)
    }

    /// Builds a message with any number of leading parameters and a final
    /// parameter that is always sent in trailing form, such as
    /// `USER alp 0 * :Real Name`.
    pub fn with_trailing(
        command: &str,
        params: impl IntoIterator<Item = impl Into<String>>,
        trailing: &str,
    ) -> Message {
        let mut params: Vec<String> = params.into_iter().map(Into::into).collect();
        params.push(trailing.to_owned());
        Message {
            tags: Tags::default(),
            source: None,
            command: Command::parse(command),
            params,
            trailing_form: true,
        }
    }

    /// Checks that this message can be written to the socket without changing
    /// meaning.
    ///
    /// Call this on every outgoing message. A parameter containing CR or LF
    /// would end the line early and let whatever follows be read by the server
    /// as a second command, so text a user typed (or pasted) must never reach
    /// the wire unchecked. The other rules catch messages the wire format
    /// cannot represent: a non-final parameter with a space in it would be
    /// split in two, and a line over the size limit would be truncated.
    pub fn validate_for_send(&self) -> Result<(), SendError> {
        let control = |s: &str| s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0));

        let name = self.command.to_string();
        if name.is_empty() || name.bytes().any(|b| b == b' ' || b.is_ascii_control()) {
            return Err(SendError::BadCommand);
        }
        for (key, _) in self.tags.iter() {
            if key.is_empty()
                || key
                    .bytes()
                    .any(|b| matches!(b, b' ' | b';' | b'=') || b.is_ascii_control())
            {
                return Err(SendError::BadTag);
            }
        }

        let last = self.params.len().saturating_sub(1);
        for (index, param) in self.params.iter().enumerate() {
            if control(param) {
                return Err(SendError::ForbiddenCharacter { index });
            }
            if index < last && requires_trailing_form(param) {
                return Err(SendError::MalformedParameter { index });
            }
        }

        let wire = self.to_wire();
        let tags_bytes = if self.tags.is_empty() {
            0
        } else {
            self.tags.to_string().len() + 2 // the '@' and the space
        };
        if tags_bytes > MAX_TAGS_BYTES {
            return Err(SendError::TooLong { bytes: tags_bytes });
        }
        let body_bytes = wire.len() - tags_bytes + 2; // the CRLF
        if body_bytes > MAX_MESSAGE_BYTES {
            return Err(SendError::TooLong { bytes: body_bytes });
        }
        Ok(())
    }

    /// Parses one line. Any trailing CR and LF are stripped first, so the
    /// caller may pass the line with or without its terminator.
    pub fn parse(line: &str) -> Result<Message, ParseError> {
        let mut rest = line.trim_end_matches(['\r', '\n']);

        let tags = match rest.strip_prefix('@') {
            Some(after_at) => {
                let (tag_str, remainder) =
                    split_at_space(after_at).ok_or(ParseError::MissingCommand)?;
                rest = remainder;
                Tags::parse(tag_str)
            }
            None => Tags::default(),
        };

        let source = match rest.strip_prefix(':') {
            Some(after_colon) => {
                let (src, remainder) =
                    split_at_space(after_colon).ok_or(ParseError::MissingCommand)?;
                rest = remainder;
                Some(Source::parse(src))
            }
            None => None,
        };

        let (command_token, mut rest) = match split_at_space(rest) {
            Some((token, remainder)) => (token, remainder),
            // No space left: the whole remainder is the command, if anything.
            None => (rest, ""),
        };
        if command_token.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        let command = Command::parse(command_token);

        let mut params = Vec::new();
        let mut trailing_form = false;
        loop {
            rest = rest.trim_start_matches(' ');
            if rest.is_empty() {
                break;
            }
            // A `:` introduces the trailing parameter, which runs to the end of
            // the line and may contain spaces.
            if let Some(trailing) = rest.strip_prefix(':') {
                params.push(trailing.to_owned());
                trailing_form = true;
                break;
            }
            match rest.find(' ') {
                Some(i) => {
                    params.push(rest[..i].to_owned());
                    rest = &rest[i..];
                }
                None => {
                    params.push(rest.to_owned());
                    break;
                }
            }
        }

        Ok(Message {
            tags,
            source,
            command,
            params,
            trailing_form,
        })
    }

    /// Parameter at `index`, or `None` if the message is shorter than that.
    pub fn param(&self, index: usize) -> Option<&str> {
        self.params.get(index).map(String::as_str)
    }

    /// The last parameter, which for most commands carries the message text.
    pub fn trailing(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    /// The `time` tag from the IRCv3 `server-time` extension, as the raw
    /// ISO 8601 string the server sent.
    pub fn server_time(&self) -> Option<&str> {
        self.tags.get("time")
    }

    /// Serializes the message without the trailing CRLF.
    pub fn to_wire(&self) -> String {
        self.to_string()
    }

    /// Serializes the message with its trailing CRLF, ready to write to the
    /// socket.
    pub fn to_wire_line(&self) -> String {
        let mut s = self.to_string();
        s.push_str("\r\n");
        s
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.tags.is_empty() {
            write!(f, "@{} ", self.tags)?;
        }
        if let Some(source) = &self.source {
            write!(f, ":{source} ")?;
        }
        write!(f, "{}", self.command)?;

        let last = self.params.len().saturating_sub(1);
        for (i, param) in self.params.iter().enumerate() {
            // Only the final parameter can take the trailing form. It does so
            // when the sender asked for it, and must do so when the parameter
            // contains a space, is empty, or would be mistaken for the `:`
            // marker itself.
            let needs_colon = i == last && (self.trailing_form || requires_trailing_form(param));
            if needs_colon {
                write!(f, " :{param}")?;
            } else {
                write!(f, " {param}")?;
            }
        }
        Ok(())
    }
}

/// Whether a parameter cannot be written bare and must use the `:` form.
fn requires_trailing_form(param: &str) -> bool {
    param.is_empty() || param.contains(' ') || param.starts_with(':')
}

/// Splits at the first run of spaces, returning the text before it and the
/// text after. Returns `None` when there is no space, meaning the caller has
/// consumed the whole line without finding the component it needed.
fn split_at_space(s: &str) -> Option<(&str, &str)> {
    let i = s.find(' ')?;
    Some((&s[..i], s[i..].trim_start_matches(' ')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_privmsg() {
        let m = Message::parse(":nick!user@host PRIVMSG #chan :hello world\r\n").unwrap();
        assert_eq!(
            m.source,
            Some(Source::User {
                nick: "nick".into(),
                user: Some("user".into()),
                host: Some("host".into()),
            })
        );
        assert!(m.command.is("PRIVMSG"));
        assert_eq!(m.params, vec!["#chan", "hello world"]);
        assert_eq!(m.trailing(), Some("hello world"));
    }

    #[test]
    fn numeric_is_parsed_as_number_not_name() {
        let m = Message::parse(":server.example 001 alp :Welcome").unwrap();
        assert_eq!(m.command, Command::Numeric(1));
        assert_eq!(m.command.numeric(), Some(1));
        assert_eq!(m.source, Some(Source::Server("server.example".into())));
    }

    #[test]
    fn command_case_is_normalized() {
        assert!(Message::parse("privmsg #c :hi")
            .unwrap()
            .command
            .is("PRIVMSG"));
    }

    #[test]
    fn tags_are_parsed_and_unescaped() {
        let m = Message::parse(
            "@time=2026-09-23T10:00:00.000Z;+draft/reply=abc;msgid=x\\swith\\:stuff \
             :n!u@h PRIVMSG #c :hi",
        )
        .unwrap();
        assert_eq!(m.server_time(), Some("2026-09-23T10:00:00.000Z"));
        assert_eq!(m.tags.get("+draft/reply"), Some("abc"));
        assert_eq!(m.tags.get("msgid"), Some("x with;stuff"));
        assert_eq!(m.tags.get("absent"), None);
    }

    #[test]
    fn valueless_tag_reads_as_empty_string() {
        let m = Message::parse("@bot :n!u@h PRIVMSG #c :hi").unwrap();
        assert_eq!(m.tags.get("bot"), Some(""));
    }

    #[test]
    fn trailing_lone_backslash_in_tag_is_dropped() {
        // Required by the message-tags spec; a naive unescaper keeps it.
        let m = Message::parse("@k=value\\ PING :x").unwrap();
        assert_eq!(m.tags.get("k"), Some("value"));
    }

    #[test]
    fn message_with_no_source_parses() {
        let m = Message::parse("PING :12345").unwrap();
        assert_eq!(m.source, None);
        assert_eq!(m.params, vec!["12345"]);
    }

    #[test]
    fn empty_trailing_is_preserved() {
        let m = Message::parse(":n!u@h PRIVMSG #c :").unwrap();
        assert_eq!(m.params, vec!["#c", ""]);
    }

    #[test]
    fn runs_of_spaces_are_tolerated() {
        let m = Message::parse(":n!u@h   PRIVMSG    #c   :hi  there").unwrap();
        assert_eq!(m.params, vec!["#c", "hi  there"]);
    }

    #[test]
    fn trailing_colon_inside_text_is_not_a_separator() {
        let m = Message::parse(":n!u@h PRIVMSG #c :see: http://x").unwrap();
        assert_eq!(m.trailing(), Some("see: http://x"));
    }

    #[test]
    fn bare_nick_source_is_a_user_but_dotted_source_is_a_server() {
        assert_eq!(
            Source::parse("alp"),
            Source::User {
                nick: "alp".into(),
                user: None,
                host: None
            }
        );
        assert_eq!(
            Source::parse("irc.libera.chat"),
            Source::Server("irc.libera.chat".into())
        );
    }

    #[test]
    fn line_with_only_tags_is_rejected() {
        assert_eq!(Message::parse("@time=x"), Err(ParseError::MissingCommand));
        assert_eq!(Message::parse(":src"), Err(ParseError::MissingCommand));
    }

    #[test]
    fn round_trips_through_the_wire_format() {
        for line in [
            ":nick!user@host PRIVMSG #chan :hello world",
            "@time=2026-09-23T10:00:00.000Z :n!u@h PRIVMSG #c :hi",
            "@msgid=a\\sb :n!u@h NOTICE #c :x",
            "PING :12345",
            ":server.example 001 alp :Welcome",
            ":n!u@h PRIVMSG #c :",
            "CAP REQ :sasl message-tags",
            // Both spellings of a single-word final parameter must survive.
            "PRIVMSG #c :hi",
            "PRIVMSG #c hi",
        ] {
            let parsed = Message::parse(line).unwrap();
            assert_eq!(parsed.to_wire(), line, "round-trip failed for: {line}");
        }
    }

    #[test]
    fn trailing_form_is_remembered_because_the_wire_allows_both() {
        let quoted = Message::parse("PRIVMSG #c :hi").unwrap();
        let bare = Message::parse("PRIVMSG #c hi").unwrap();

        // Same meaning, different bytes. `params` alone cannot tell them apart.
        assert_eq!(quoted.params, bare.params);
        assert!(quoted.trailing_form);
        assert!(!bare.trailing_form);
    }

    #[test]
    fn with_body_always_quotes_the_body() {
        // A one-word body must still be quoted, so that the same code path
        // works when the body is empty or grows a leading colon.
        assert_eq!(
            Message::with_body("PRIVMSG", "#c", "hi").to_wire(),
            "PRIVMSG #c :hi"
        );
        assert_eq!(
            Message::with_body("PRIVMSG", "#c", "").to_wire(),
            "PRIVMSG #c :"
        );
        assert_eq!(
            Message::with_body("NOTICE", "alp", "see: x").to_wire(),
            "NOTICE alp :see: x"
        );
    }

    #[test]
    fn built_message_quotes_only_what_needs_quoting() {
        let m = Message::new("JOIN", ["#chan"]);
        assert_eq!(m.to_wire(), "JOIN #chan");

        let m = Message::new("PRIVMSG", ["#chan", "two words"]);
        assert_eq!(m.to_wire(), "PRIVMSG #chan :two words");

        // A single-word body still needs the colon if it starts with one.
        let m = Message::new("PRIVMSG", ["#chan", ":-)"]);
        assert_eq!(m.to_wire(), "PRIVMSG #chan ::-)");
    }

    #[test]
    fn wire_line_terminates_with_crlf() {
        assert_eq!(Message::new("PING", ["x"]).to_wire_line(), "PING x\r\n");
    }

    #[test]
    fn with_trailing_keeps_leading_parameters_separate() {
        let m = Message::with_trailing("USER", ["alp", "0", "*"], "Alp Yılmaz");
        assert_eq!(m.to_wire(), "USER alp 0 * :Alp Yılmaz");
        assert_eq!(m.params.len(), 4);
        assert_eq!(m.validate_for_send(), Ok(()));
    }

    #[test]
    fn a_line_break_in_a_parameter_is_refused() {
        // The injection this guards against: a pasted "hi\r\nQUIT" would put a
        // second command on the wire.
        let m = Message::with_body("PRIVMSG", "#c", "hi\r\nQUIT :gone");
        assert_eq!(
            m.validate_for_send(),
            Err(SendError::ForbiddenCharacter { index: 1 })
        );
        for bad in ["a\nb", "a\rb", "a\0b"] {
            assert!(Message::with_body("PRIVMSG", "#c", bad)
                .validate_for_send()
                .is_err());
        }
        assert!(Message::with_body("PRIVMSG", "#c\nQUIT", "x")
            .validate_for_send()
            .is_err());
    }

    #[test]
    fn a_non_final_parameter_with_a_space_is_refused() {
        let m = Message::new("JOIN", ["#a b", "key"]);
        assert_eq!(
            m.validate_for_send(),
            Err(SendError::MalformedParameter { index: 0 })
        );
        assert!(Message::new("MODE", ["#c", "+o", "alp"])
            .validate_for_send()
            .is_ok());
    }

    #[test]
    fn oversized_lines_are_refused() {
        let ok = Message::with_body("PRIVMSG", "#c", &"x".repeat(400));
        assert_eq!(ok.validate_for_send(), Ok(()));
        let too_long = Message::with_body("PRIVMSG", "#c", &"x".repeat(600));
        assert!(matches!(
            too_long.validate_for_send(),
            Err(SendError::TooLong { .. })
        ));
    }

    #[test]
    fn tags_do_not_count_against_the_512_byte_body_limit() {
        let mut m = Message::with_body("PRIVMSG", "#c", &"x".repeat(480));
        m.tags.set("+draft/reply", "a".repeat(200));
        assert_eq!(m.validate_for_send(), Ok(()));
    }

    #[test]
    fn a_tag_value_with_a_line_break_is_escaped_not_refused() {
        let mut m = Message::new("PING", ["x"]);
        m.tags.set("k", "a\nb");
        assert_eq!(m.validate_for_send(), Ok(()));
        assert_eq!(m.to_wire(), "@k=a\\nb PING x");
    }

    #[test]
    fn a_bad_tag_key_is_refused() {
        let mut m = Message::new("PING", ["x"]);
        m.tags.set("bad key", "v");
        assert_eq!(m.validate_for_send(), Err(SendError::BadTag));
    }
}
