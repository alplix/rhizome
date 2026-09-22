//! CTCP: the convention that hides a second protocol inside `PRIVMSG`.
//!
//! A message body wrapped in `\x01` is not chat text but a client-to-client
//! request. `ACTION` is the one users see — it is what `/me` sends — and the
//! rest (`VERSION`, `PING`, `TIME`) are requests other clients expect an
//! answer to, delivered as a `NOTICE` so that clients do not loop replying to
//! each other.
//!
//! A client that does not recognize these renders `\x01ACTION waves\x01` as
//! literal text, which is the usual symptom of having skipped this layer.
//!
//! The original specification also defines a `\x10`-based low-level quoting
//! scheme. Effectively nothing implements it, and applying it would corrupt
//! ordinary messages containing `\x10`, so it is deliberately not implemented
//! here.

/// The delimiter that marks a body as CTCP.
pub const DELIM: char = '\u{01}';

/// A parsed CTCP request or reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctcp {
    /// The command, uppercased, e.g. `ACTION` or `VERSION`.
    pub command: String,
    /// Everything after the command, if any.
    pub params: Option<String>,
}

/// Parses a message body as CTCP, or returns `None` if it is ordinary text.
///
/// The trailing delimiter is optional: some clients omit it, and rejecting
/// those messages would drop otherwise valid actions.
pub fn parse(body: &str) -> Option<Ctcp> {
    let inner = body.strip_prefix(DELIM)?;
    let inner = inner.strip_suffix(DELIM).unwrap_or(inner);
    if inner.is_empty() {
        return None;
    }
    let (command, params) = match inner.split_once(' ') {
        Some((c, p)) => (c, Some(p.to_owned())),
        None => (inner, None),
    };
    Some(Ctcp {
        command: command.to_ascii_uppercase(),
        params,
    })
}

/// The text of an `ACTION`, if this body is one.
///
/// This is the common case by a wide margin, so it gets a shortcut that avoids
/// allocating for every ordinary message.
pub fn action_text(body: &str) -> Option<&str> {
    let inner = body.strip_prefix(DELIM)?;
    let inner = inner.strip_suffix(DELIM).unwrap_or(inner);
    let rest = inner.strip_prefix("ACTION")?;
    match rest.strip_prefix(' ') {
        Some(text) => Some(text),
        // "\x01ACTION\x01" with no text is a valid, empty action.
        None if rest.is_empty() => Some(""),
        None => None,
    }
}

/// Whether a body is CTCP at all.
pub fn is_ctcp(body: &str) -> bool {
    body.starts_with(DELIM)
}

/// Wraps text as an `ACTION` body, for `/me`.
pub fn action(text: &str) -> String {
    format!("{DELIM}ACTION {text}{DELIM}")
}

/// Wraps a command and optional parameters as a CTCP body.
pub fn build(command: &str, params: Option<&str>) -> String {
    match params {
        Some(p) => format!("{DELIM}{} {p}{DELIM}", command.to_ascii_uppercase()),
        None => format!("{DELIM}{}{DELIM}", command.to_ascii_uppercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_not_ctcp() {
        assert_eq!(parse("hello"), None);
        assert_eq!(action_text("hello"), None);
        assert!(!is_ctcp("hello"));
    }

    #[test]
    fn action_is_recognized() {
        assert_eq!(action_text("\u{01}ACTION waves\u{01}"), Some("waves"));
        assert_eq!(
            parse("\u{01}ACTION waves\u{01}"),
            Some(Ctcp {
                command: "ACTION".into(),
                params: Some("waves".into())
            })
        );
    }

    #[test]
    fn missing_trailing_delimiter_is_tolerated() {
        // Several clients omit it; rejecting these would silently drop actions.
        assert_eq!(action_text("\u{01}ACTION waves"), Some("waves"));
    }

    #[test]
    fn empty_action_is_valid() {
        assert_eq!(action_text("\u{01}ACTION\u{01}"), Some(""));
        assert_eq!(action_text("\u{01}ACTION \u{01}"), Some(""));
    }

    #[test]
    fn command_case_is_normalized_but_text_is_not() {
        let c = parse("\u{01}version\u{01}").unwrap();
        assert_eq!(c.command, "VERSION");
        assert_eq!(c.params, None);

        let c = parse("\u{01}ACTION Merhaba Dünya\u{01}").unwrap();
        assert_eq!(c.params.as_deref(), Some("Merhaba Dünya"));
    }

    #[test]
    fn a_command_that_merely_starts_with_action_is_not_one() {
        assert_eq!(action_text("\u{01}ACTIONS foo\u{01}"), None);
    }

    #[test]
    fn empty_ctcp_body_is_rejected() {
        assert_eq!(parse("\u{01}\u{01}"), None);
        assert_eq!(parse("\u{01}"), None);
    }

    #[test]
    fn build_round_trips() {
        assert_eq!(action("waves"), "\u{01}ACTION waves\u{01}");
        assert_eq!(action_text(&action("şapka çıkarır")), Some("şapka çıkarır"));
        assert_eq!(
            parse(&build("ping", Some("12345"))),
            Some(Ctcp {
                command: "PING".into(),
                params: Some("12345".into())
            })
        );
        assert_eq!(
            parse(&build("version", None)),
            Some(Ctcp {
                command: "VERSION".into(),
                params: None
            })
        );
    }
}
