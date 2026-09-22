//! IRCv3 capability negotiation: asking the server what it can do before
//! finishing registration.
//!
//! Everything that makes IRC tolerable in 2026 is a capability the client has
//! to request during a short window at connect time. `server-time` is what
//! lets a replayed backlog carry real timestamps; `message-tags` and
//! `echo-message` are what let a sent message be correlated with the copy that
//! comes back; `sasl` is what lets us authenticate before anyone sees our
//! host. Miss the window and the connection registers without them.
//!
//! This module is the bookkeeping only. It parses `CAP` replies and tracks
//! what is available and enabled; the crate above drives the exchange.

use std::collections::{BTreeMap, BTreeSet};

use crate::message::Message;

/// The capabilities Rhizome requests when the server offers them.
///
/// Requesting a capability the server did not advertise is an error on some
/// networks, so this is a wish list filtered against `CAP LS`, not a demand.
pub const WANTED: &[&str] = &[
    // Timestamps from the server, so replayed history is not stamped with the
    // time we happened to receive it.
    "server-time",
    // Arbitrary tags on messages; the prerequisite for msgid, replies and
    // most later extensions.
    "message-tags",
    // Our own messages come back to us, so what we display is what the
    // network actually delivered rather than a local guess.
    "echo-message",
    // Authenticate before the connection is visible to anyone.
    "sasl",
    // Every prefix a user holds in NAMES, not just the highest.
    "multi-prefix",
    // Account name attached to each message, so we can tell two people with
    // confusingly similar nicks apart.
    "account-tag",
    "account-notify",
    "extended-join",
    // Away state without polling with WHO.
    "away-notify",
    // One QUIT for a netsplit instead of hundreds of lines.
    "chghost",
    // Batched history and netsplit grouping.
    "batch",
    // Correlate a command with its reply.
    "labeled-response",
    // Ask the server for scrollback we missed. Still draft, and the main
    // reason a bouncer feels seamless when it works.
    "draft/chathistory",
    // Send a paste as one logical message instead of N flood-triggering ones.
    "draft/multiline",
];

/// A parsed `CAP` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapEvent {
    /// The server listed capabilities. `more` is true when a `*` continuation
    /// marker says another `LS` line follows.
    Ls {
        caps: Vec<(String, Option<String>)>,
        more: bool,
    },
    /// The server listed the capabilities currently enabled.
    List {
        caps: Vec<String>,
        more: bool,
    },
    /// The server granted the requested capabilities.
    Ack(Vec<String>),
    /// The server refused them. The whole request is refused as a unit.
    Nak(Vec<String>),
    /// A capability became available after registration.
    New(Vec<(String, Option<String>)>),
    /// A capability was withdrawn after registration.
    Del(Vec<String>),
}

/// Parses a `CAP` message, or returns `None` if this is not one.
///
/// The layout is `CAP <target> <subcommand> [*] :<capabilities>`, where the
/// target is our nick or `*` before we have one, and the optional `*` marks a
/// continuation.
pub fn parse(message: &Message) -> Option<CapEvent> {
    if !message.command.is("CAP") {
        return None;
    }
    let subcommand = message.param(1)?.to_ascii_uppercase();

    // A `*` in the third position means more lines follow; the capability list
    // is then in the fourth.
    let (more, list) = match message.param(2) {
        Some("*") => (true, message.param(3).unwrap_or_default()),
        Some(list) => (false, list),
        None => (false, ""),
    };

    Some(match subcommand.as_str() {
        "LS" => CapEvent::Ls {
            caps: parse_cap_list(list),
            more,
        },
        "LIST" => CapEvent::List {
            caps: list.split_whitespace().map(str::to_owned).collect(),
            more,
        },
        "ACK" => CapEvent::Ack(split_names(list)),
        "NAK" => CapEvent::Nak(split_names(list)),
        "NEW" => CapEvent::New(parse_cap_list(list)),
        "DEL" => CapEvent::Del(split_names(list)),
        _ => return None,
    })
}

fn split_names(list: &str) -> Vec<String> {
    list.split_whitespace().map(str::to_owned).collect()
}

/// Splits a capability list, separating `name=value` forms.
fn parse_cap_list(list: &str) -> Vec<(String, Option<String>)> {
    list.split_whitespace()
        .map(|item| match item.split_once('=') {
            Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
            None => (item.to_owned(), None),
        })
        .collect()
}

/// What the server offers and what we have turned on.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    available: BTreeMap<String, Option<String>>,
    enabled: BTreeSet<String>,
    ls_complete: bool,
}

impl Capabilities {
    pub fn new() -> Capabilities {
        Capabilities::default()
    }

    /// Applies a parsed `CAP` event.
    pub fn apply(&mut self, event: &CapEvent) {
        match event {
            CapEvent::Ls { caps, more } => {
                for (name, value) in caps {
                    self.available.insert(name.clone(), value.clone());
                }
                if !more {
                    self.ls_complete = true;
                }
            }
            CapEvent::New(caps) => {
                for (name, value) in caps {
                    self.available.insert(name.clone(), value.clone());
                }
            }
            CapEvent::Ack(names) => {
                for name in names {
                    // An ACK for "-name" confirms we turned it off.
                    match name.strip_prefix('-') {
                        Some(off) => {
                            self.enabled.remove(off);
                        }
                        None => {
                            self.enabled.insert(name.clone());
                        }
                    }
                }
            }
            CapEvent::Del(names) => {
                for name in names {
                    self.available.remove(name);
                    self.enabled.remove(name);
                }
            }
            // A NAK changes nothing: the request was refused as a unit, and
            // the server state is what it was before we asked.
            CapEvent::Nak(_) => {}
            CapEvent::List { caps, more } => {
                if *more {
                    self.enabled.extend(caps.iter().cloned());
                } else {
                    // A complete LIST is authoritative.
                    self.enabled = caps.iter().cloned().collect();
                }
            }
        }
    }

    /// Whether the server has finished listing its capabilities.
    pub fn ls_complete(&self) -> bool {
        self.ls_complete
    }

    /// Whether a capability is enabled on this connection.
    ///
    /// This, not [`Capabilities::is_available`], is what behaviour should
    /// branch on.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    /// Whether the server offers a capability, enabled or not.
    pub fn is_available(&self, name: &str) -> bool {
        self.available.contains_key(name)
    }

    /// The value attached to an advertised capability, such as the mechanism
    /// list in `sasl=PLAIN,EXTERNAL`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.available.get(name)?.as_deref()
    }

    /// The SASL mechanisms the server advertises.
    ///
    /// An advertised `sasl` with no value means the server supports SASL but
    /// did not say which mechanisms, which in practice means `PLAIN`.
    pub fn sasl_mechanisms(&self) -> Vec<String> {
        match self.available.get("sasl") {
            Some(Some(list)) => list
                .split(',')
                .filter(|m| !m.is_empty())
                .map(|m| m.to_ascii_uppercase())
                .collect(),
            Some(None) => vec!["PLAIN".to_owned()],
            None => Vec::new(),
        }
    }

    /// The subset of [`WANTED`] this server actually offers, which is what to
    /// put in `CAP REQ`.
    pub fn to_request(&self) -> Vec<&'static str> {
        WANTED
            .iter()
            .copied()
            .filter(|name| self.available.contains_key(*name))
            .collect()
    }

    /// Every enabled capability, for a debug view.
    pub fn enabled(&self) -> impl Iterator<Item = &str> {
        self.enabled.iter().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(line: &str) -> CapEvent {
        parse(&Message::parse(line).unwrap()).expect("should parse as a CAP event")
    }

    #[test]
    fn non_cap_messages_are_ignored() {
        assert_eq!(parse(&Message::parse("PING :x").unwrap()), None);
        // An unknown subcommand is not an event we know how to apply.
        assert_eq!(parse(&Message::parse("CAP * WHAT :x").unwrap()), None);
    }

    #[test]
    fn ls_with_values_is_parsed() {
        let e = event(":s CAP * LS :sasl=PLAIN,EXTERNAL server-time multi-prefix");
        assert_eq!(
            e,
            CapEvent::Ls {
                caps: vec![
                    ("sasl".into(), Some("PLAIN,EXTERNAL".into())),
                    ("server-time".into(), None),
                    ("multi-prefix".into(), None),
                ],
                more: false,
            }
        );
    }

    #[test]
    fn multiline_ls_is_accumulated_and_only_the_last_line_completes_it() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP * LS * :sasl=PLAIN server-time"));
        assert!(!caps.ls_complete(), "a continuation must not complete LS");
        assert!(caps.is_available("sasl"));

        caps.apply(&event(":s CAP * LS :multi-prefix echo-message"));
        assert!(caps.ls_complete());
        assert!(caps.is_available("multi-prefix"));
        // The earlier line survives.
        assert_eq!(caps.value("sasl"), Some("PLAIN"));
    }

    #[test]
    fn ack_enables_and_negated_ack_disables() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP alp ACK :server-time multi-prefix"));
        assert!(caps.is_enabled("server-time"));
        assert!(caps.is_enabled("multi-prefix"));

        caps.apply(&event(":s CAP alp ACK :-multi-prefix"));
        assert!(caps.is_enabled("server-time"));
        assert!(!caps.is_enabled("multi-prefix"));
    }

    #[test]
    fn nak_leaves_state_untouched() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP * LS :server-time sasl"));
        caps.apply(&event(":s CAP alp ACK :server-time"));
        caps.apply(&event(":s CAP alp NAK :sasl"));

        assert!(caps.is_enabled("server-time"));
        assert!(!caps.is_enabled("sasl"));
        // A refusal does not withdraw the advertisement.
        assert!(caps.is_available("sasl"));
    }

    #[test]
    fn del_withdraws_a_capability_entirely() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP * LS :away-notify"));
        caps.apply(&event(":s CAP alp ACK :away-notify"));
        caps.apply(&event(":s CAP alp DEL :away-notify"));

        assert!(!caps.is_enabled("away-notify"));
        assert!(!caps.is_available("away-notify"));
    }

    #[test]
    fn new_advertises_a_capability_after_registration() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP alp NEW :draft/chathistory=50"));
        assert!(caps.is_available("draft/chathistory"));
        assert_eq!(caps.value("draft/chathistory"), Some("50"));
        // Advertised is not enabled; we still have to ask.
        assert!(!caps.is_enabled("draft/chathistory"));
    }

    #[test]
    fn sasl_mechanisms_are_normalized() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP * LS :sasl=plain,external"));
        assert_eq!(caps.sasl_mechanisms(), vec!["PLAIN", "EXTERNAL"]);
    }

    #[test]
    fn valueless_sasl_means_plain() {
        // Older servers advertise bare `sasl`; assuming no mechanisms would
        // make us silently skip authentication.
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP * LS :sasl"));
        assert_eq!(caps.sasl_mechanisms(), vec!["PLAIN"]);
    }

    #[test]
    fn no_sasl_advertised_means_no_mechanisms() {
        let caps = Capabilities::new();
        assert!(caps.sasl_mechanisms().is_empty());
    }

    #[test]
    fn request_is_our_wish_list_intersected_with_the_offer() {
        let mut caps = Capabilities::new();
        caps.apply(&event(
            ":s CAP * LS :sasl server-time some-vendor-thing multi-prefix",
        ));

        let request = caps.to_request();
        assert!(request.contains(&"sasl"));
        assert!(request.contains(&"server-time"));
        assert!(request.contains(&"multi-prefix"));
        // We never ask for something the server did not offer...
        assert!(!request.contains(&"echo-message"));
        // ...nor for something we did not want.
        assert!(!request.contains(&"some-vendor-thing"));
    }

    #[test]
    fn complete_list_replaces_but_continued_list_accumulates() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP alp ACK :server-time sasl"));

        caps.apply(&event(":s CAP alp LIST * :multi-prefix"));
        assert!(caps.is_enabled("server-time"));
        assert!(caps.is_enabled("multi-prefix"));

        caps.apply(&event(":s CAP alp LIST :away-notify"));
        assert!(caps.is_enabled("away-notify"));
        assert!(!caps.is_enabled("server-time"), "a complete LIST is authoritative");
    }

    #[test]
    fn empty_capability_list_is_handled() {
        let mut caps = Capabilities::new();
        caps.apply(&event(":s CAP * LS :"));
        assert!(caps.ls_complete());
        assert!(caps.to_request().is_empty());
    }
}
