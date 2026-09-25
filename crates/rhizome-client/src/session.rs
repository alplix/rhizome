//! The protocol state machine: everything the client knows and decides,
//! with no sockets and no clock.
//!
//! A [`Session`] is fed parsed messages and answers with an [`Output`]: lines
//! to send and events to report. Because it never touches the network, the
//! whole connect-time handshake, channel tracking and message routing can be
//! tested by feeding it transcripts, and the same code runs unchanged under
//! the terminal example, the desktop app and Android.
//!
//! The driver in [`crate::connection`] owns the socket and translates between
//! it and this.

use std::collections::{BTreeSet, HashMap};

use rhizome_proto::cap::{self, CapEvent};
use rhizome_proto::{
    ctcp, sasl, split, Capabilities, CaseMapping, Command, ISupport, Mechanism, Message, ModeKind,
    Source,
};

use crate::config::Config;
use crate::event::{ChatMessage, Event, Member, MessageKind};

/// The client name and version reported to `CTCP VERSION`.
const VERSION_REPLY: &str = concat!("Rhizome ", env!("CARGO_PKG_VERSION"));

/// How many times to retry with a longer nick before giving up on
/// registering.
const MAX_NICK_ATTEMPTS: u32 = 10;

/// How long a `JOIN` list may get before it is split across several lines.
/// Well under the 512-byte limit, leaving room for `JOIN ` and the CRLF.
const JOIN_LINE_BYTES: usize = 400;

/// What handling one input produced.
#[derive(Debug, Default)]
pub struct Output {
    /// Lines to send, in order. Not yet validated: the driver checks each with
    /// [`Message::validate_for_send`] before writing it.
    pub send: Vec<Message>,
    /// Things to report, in order.
    pub events: Vec<Event>,
}

impl Output {
    fn error(text: impl Into<String>) -> Output {
        Output {
            send: Vec::new(),
            events: vec![Event::Error {
                code: 0,
                text: text.into(),
            }],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `CAP LS` sent; waiting for the full list.
    Negotiating,
    /// `CAP REQ` sent; waiting for it to be acknowledged.
    AwaitingAck,
    /// `AUTHENTICATE` under way.
    Authenticating,
    /// `CAP END` sent; waiting for the welcome.
    Registering,
    Registered,
    /// Finished, by failure or because the server ended the connection.
    Closed,
}

#[derive(Debug)]
struct Channel {
    /// The name as the server first gave it, for display.
    name: String,
    topic: Option<String>,
    /// Members keyed by case-folded nick.
    members: HashMap<String, Member>,
}

/// The state of one connection to one network.
#[derive(Debug)]
pub struct Session {
    config: Config,
    phase: Phase,
    caps: Capabilities,
    isupport: ISupport,
    nick: String,
    /// Our own `nick!user@host` as the server sees it, once learned. Needed
    /// to size outgoing lines correctly.
    mask: Option<String>,
    channels: HashMap<String, Channel>,
    /// `NAMES` replies arrive in several lines and only become a member list
    /// at the terminating `366`.
    pending_names: HashMap<String, Vec<Member>>,
    nick_attempts: u32,
    /// Capabilities requested and not yet acknowledged.
    pending_ack: BTreeSet<String>,
    /// Whether a refused request has already been retried one capability at a
    /// time.
    retrying: bool,
    sasl: Option<Mechanism>,
    /// Set when the session ended for a reason retrying will not fix.
    failure: Option<String>,
    /// Set when the server closed the connection with `ERROR`.
    server_closed: Option<String>,
}

impl Session {
    pub fn new(config: Config) -> Session {
        let nick = config.nick.clone();
        Session {
            config,
            phase: Phase::Negotiating,
            caps: Capabilities::new(),
            isupport: ISupport::default(),
            nick,
            mask: None,
            channels: HashMap::new(),
            pending_names: HashMap::new(),
            nick_attempts: 0,
            pending_ack: BTreeSet::new(),
            retrying: false,
            sasl: None,
            failure: None,
            server_closed: None,
        }
    }

    /// The opening lines: ask for capabilities, then identify.
    ///
    /// Registration is held open until capability negotiation finishes, so
    /// that SASL can complete before anyone sees the connection.
    pub fn start(&mut self) -> Output {
        let mut out = Output::default();
        out.send.push(Message::new("CAP", ["LS", "302"]));
        if let Some(password) = &self.config.server_password {
            out.send.push(Message::new("PASS", [password.expose()]));
        }
        out.send.push(Message::new("NICK", [self.nick.clone()]));
        out.send.push(Message::with_trailing(
            "USER",
            [self.config.username.clone(), "0".to_owned(), "*".to_owned()],
            &self.config.realname,
        ));
        out
    }

    // ---- observers -------------------------------------------------------

    /// Our current nick.
    pub fn nick(&self) -> &str {
        &self.nick
    }

    pub fn is_registered(&self) -> bool {
        self.phase == Phase::Registered
    }

    /// Whether the session has ended and the connection should be dropped.
    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// Why the session ended, if it ended for a reason retrying will not fix
    /// (such as a failed login).
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Why the server closed the connection, if it said.
    pub fn server_closed(&self) -> Option<&str> {
        self.server_closed.as_deref()
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    pub fn isupport(&self) -> &ISupport {
        &self.isupport
    }

    pub fn casemapping(&self) -> CaseMapping {
        self.isupport.casemapping()
    }

    /// The channels we are in, as the server named them.
    pub fn channel_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.channels.values().map(|c| c.name.clone()).collect();
        names.sort();
        names
    }

    /// The members of a channel we are in, ordered by privilege then name.
    pub fn members(&self, channel: &str) -> Option<Vec<Member>> {
        let channel = self.channels.get(&self.fold(channel))?;
        Some(self.sorted_members(channel.members.values().cloned().collect()))
    }

    // ---- input from the server ------------------------------------------

    /// Handles one message from the server.
    pub fn handle(&mut self, msg: &Message) -> Output {
        let mut out = Output::default();
        if self.phase == Phase::Closed {
            return out;
        }

        if let Some(event) = cap::parse(msg) {
            self.on_cap(event, &mut out);
            return out;
        }

        match &msg.command {
            Command::Numeric(code) => self.on_numeric(*code, msg, &mut out),
            Command::Named(name) => match name.as_str() {
                "PING" => {
                    // Echo the token back exactly; servers use it to match
                    // replies to requests.
                    let token = msg.param(0).unwrap_or_default();
                    out.send.push(Message::new("PONG", [token]));
                }
                "AUTHENTICATE" => self.on_authenticate(msg, &mut out),
                "PRIVMSG" => self.on_message(msg, false, &mut out),
                "NOTICE" => self.on_message(msg, true, &mut out),
                "JOIN" => self.on_join(msg, &mut out),
                "PART" => self.on_part(msg, &mut out),
                "KICK" => self.on_kick(msg, &mut out),
                "QUIT" => self.on_quit(msg, &mut out),
                "NICK" => self.on_nick(msg, &mut out),
                "TOPIC" => self.on_topic(msg, &mut out),
                "MODE" => self.on_mode(msg, &mut out),
                "ERROR" => {
                    let text = msg.trailing().unwrap_or("connection closed").to_owned();
                    out.events.push(Event::ServerError(text.clone()));
                    self.server_closed = Some(text);
                    self.phase = Phase::Closed;
                }
                _ => {}
            },
        }
        out
    }

    // ---- capability negotiation and login -------------------------------

    fn on_cap(&mut self, event: CapEvent, out: &mut Output) {
        match &event {
            CapEvent::Ls { .. } => {
                self.caps.apply(&event);
                if self.phase == Phase::Negotiating && self.caps.ls_complete() {
                    self.request_capabilities(out);
                }
            }
            CapEvent::Ack(names) => {
                self.caps.apply(&event);
                if self.phase == Phase::AwaitingAck {
                    for name in names {
                        self.pending_ack.remove(name.trim_start_matches('-'));
                    }
                    if self.pending_ack.is_empty() {
                        self.after_capabilities(out);
                    }
                }
            }
            CapEvent::Nak(names) => {
                if self.phase != Phase::AwaitingAck {
                    return;
                }
                if !self.retrying && names.len() > 1 {
                    // The server refuses a request as a unit, so one
                    // capability it dislikes would cost us all of them. Ask
                    // again one at a time to keep the ones it accepts.
                    self.retrying = true;
                    self.pending_ack = names.iter().cloned().collect();
                    for name in names {
                        out.send.push(Message::with_body("CAP", "REQ", name));
                    }
                } else {
                    for name in names {
                        self.pending_ack.remove(name);
                    }
                    if self.pending_ack.is_empty() {
                        self.after_capabilities(out);
                    }
                }
            }
            CapEvent::New(caps) => {
                self.caps.apply(&event);
                if self.phase == Phase::Registered {
                    // A capability that appeared after registration, such as a
                    // server enabling history support. Take what we want,
                    // except SASL, which is only meaningful at login.
                    let wanted: Vec<&str> = caps
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .filter(|name| *name != "sasl" && cap::WANTED.contains(name))
                        .collect();
                    if !wanted.is_empty() {
                        out.send
                            .push(Message::with_body("CAP", "REQ", &wanted.join(" ")));
                    }
                }
            }
            CapEvent::Del(_) | CapEvent::List { .. } => self.caps.apply(&event),
        }
    }

    /// The server's capability list is complete: ask for what we want.
    fn request_capabilities(&mut self, out: &mut Output) {
        self.sasl = sasl::select(
            &self.caps.sasl_mechanisms(),
            &self.config.sasl,
            self.config.tls,
        )
        .cloned();

        if !self.config.sasl.is_empty() && self.sasl.is_none() {
            let reason = if !self.caps.is_available("sasl") {
                "the server does not offer SASL".to_owned()
            } else if !self.config.tls {
                "SASL PLAIN is refused on a connection without TLS".to_owned()
            } else {
                "the server offers no SASL mechanism we can use".to_owned()
            };
            self.fail(reason, out);
            return;
        }

        let request: Vec<&'static str> = self
            .caps
            .to_request()
            .into_iter()
            .filter(|name| *name != "sasl" || self.sasl.is_some())
            .collect();

        if request.is_empty() {
            self.end_negotiation(out);
            return;
        }
        self.pending_ack = request.iter().map(|s| (*s).to_owned()).collect();
        self.phase = Phase::AwaitingAck;
        out.send
            .push(Message::with_body("CAP", "REQ", &request.join(" ")));
    }

    /// Every requested capability has been answered.
    fn after_capabilities(&mut self, out: &mut Output) {
        match &self.sasl {
            Some(mechanism) if self.caps.is_enabled("sasl") => {
                out.send
                    .push(Message::new("AUTHENTICATE", [mechanism.name()]));
                self.phase = Phase::Authenticating;
            }
            Some(_) => {
                self.fail("the server refused to enable SASL".to_owned(), out);
            }
            None => self.end_negotiation(out),
        }
    }

    fn end_negotiation(&mut self, out: &mut Output) {
        out.send.push(Message::new("CAP", ["END"]));
        self.phase = Phase::Registering;
    }

    fn on_authenticate(&mut self, msg: &Message, out: &mut Output) {
        if self.phase != Phase::Authenticating || msg.param(0) != Some("+") {
            // Only the empty challenge is expected for PLAIN and EXTERNAL.
            return;
        }
        if let Some(mechanism) = &self.sasl {
            for payload in mechanism.payloads() {
                out.send.push(Message::new("AUTHENTICATE", [payload]));
            }
        }
    }

    /// Abandons the connection for a reason retrying will not fix.
    ///
    /// Reconnecting after a failed login would repeat the same wrong password
    /// until the account or address is locked out.
    fn fail(&mut self, reason: String, out: &mut Output) {
        out.events.push(Event::AuthFailed(reason.clone()));
        out.send.push(Message::with_trailing(
            "QUIT",
            Vec::<String>::new(),
            "Authentication failed",
        ));
        self.failure = Some(reason);
        self.phase = Phase::Closed;
    }

    // ---- numeric replies -------------------------------------------------

    fn on_numeric(&mut self, code: u16, msg: &Message, out: &mut Output) {
        match code {
            1 => self.on_welcome(msg, out),
            5 => {
                let before = self.isupport.network().map(str::to_owned);
                self.isupport.ingest(msg);
                let after = self.isupport.network().map(str::to_owned);
                if after != before {
                    if let Some(name) = after {
                        out.events.push(Event::Network(name));
                    }
                }
            }
            // RPL_LOGGEDIN: `<nick> <nick!user@host> <account> :You are now
            // logged in`. The mask is the one thing here we need.
            900 => {
                if let Some(mask) = msg.param(1).filter(|m| m.contains('!') && m.contains('@')) {
                    self.mask = Some(mask.to_owned());
                }
            }
            // RPL_SASLSUCCESS, and ERR_SASLALREADY (already authenticated,
            // which is as good as success).
            903 | 907 if self.phase == Phase::Authenticating => self.end_negotiation(out),
            // The SASL failures: nick locked, bad credentials, message too
            // long, aborted, and the mechanism list.
            902 | 904 | 905 | 906 | 908 if self.phase == Phase::Authenticating => {
                let text = msg.trailing().unwrap_or("SASL authentication failed");
                self.fail(format!("{text} ({code})"), out);
            }
            // Nick collisions while registering. Once registered, the same
            // numeric is an ordinary error from `/nick`.
            433 | 436 if self.phase != Phase::Registered => self.retry_nick(out),
            432 if self.phase != Phase::Registered => {
                self.fail(
                    format!("the server rejected the nick {:?} as invalid", self.nick),
                    out,
                );
            }
            // ERR_UNKNOWNCOMMAND for CAP: the server predates capabilities.
            421 if self.phase == Phase::Negotiating && msg.param(1) == Some("CAP") => {
                if self.config.sasl.is_empty() {
                    self.phase = Phase::Registering;
                } else {
                    self.fail("the server does not support SASL".to_owned(), out);
                }
            }
            // RPL_HOSTHIDDEN: our host changed (a cloak was applied).
            396 => {
                if let (Some(host), Some(mask)) = (msg.param(1), self.mask.as_mut()) {
                    if let Some((prefix, _)) = mask.split_once('@') {
                        *mask = format!("{prefix}@{host}");
                    }
                }
            }
            331 => {}
            332 => {
                if let (Some(channel), Some(topic)) = (msg.param(1), msg.param(2)) {
                    if let Some(ch) = self.channels.get_mut(&self.isupport.casemapping().fold(channel)) {
                        ch.topic = Some(topic.to_owned());
                    }
                    out.events.push(Event::Topic {
                        channel: channel.to_owned(),
                        topic: Some(topic.to_owned()),
                    });
                }
            }
            353 => self.on_names_line(msg),
            366 => self.on_names_end(msg, out),
            // The message of the day.
            372 | 375 | 376 => {
                if let Some(text) = msg.trailing() {
                    out.events.push(Event::Server(text.to_owned()));
                }
            }
            // Anything else in the error ranges is the server refusing
            // something we asked for; surface it rather than swallow it.
            400..=599 => {
                let text = msg
                    .params
                    .iter()
                    .skip(1)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                out.events.push(Event::Error { code, text });
            }
            _ => {}
        }
    }

    fn on_welcome(&mut self, msg: &Message, out: &mut Output) {
        if let Some(nick) = msg.param(0) {
            self.nick = nick.to_owned();
        }
        // Older servers put our mask at the end of the welcome text.
        if let Some(mask) = msg
            .trailing()
            .and_then(|t| t.split_whitespace().last())
            .filter(|w| w.contains('!') && w.contains('@'))
        {
            self.mask = Some(mask.to_owned());
        }
        self.phase = Phase::Registered;
        out.events.push(Event::Registered {
            nick: self.nick.clone(),
        });
        out.send.extend(join_messages(&self.config.autojoin));
    }

    fn retry_nick(&mut self, out: &mut Output) {
        self.nick_attempts += 1;
        if self.nick_attempts > MAX_NICK_ATTEMPTS {
            self.fail(
                "no free nick could be found".to_owned(),
                out,
            );
            return;
        }
        self.nick.push('_');
        out.send.push(Message::new("NICK", [self.nick.clone()]));
    }

    // ---- channel state ---------------------------------------------------

    fn fold(&self, s: &str) -> String {
        self.isupport.casemapping().fold(s)
    }

    fn is_me(&self, nick: &str) -> bool {
        self.isupport.casemapping().eq(nick, &self.nick)
    }

    fn sorted_members(&self, mut members: Vec<Member>) -> Vec<Member> {
        let map = self.isupport.casemapping();
        members.sort_by_cached_key(|m| {
            let rank = m
                .top_prefix()
                .map_or_else(|| self.isupport.prefix_rank(' '), |p| self.isupport.prefix_rank(p));
            (rank, map.fold(&m.nick))
        });
        members
    }

    fn on_join(&mut self, msg: &Message, out: &mut Output) {
        let (Some(Source::User { nick, user, host }), Some(channel)) =
            (msg.source.as_ref(), msg.param(0))
        else {
            return;
        };
        let key = self.fold(channel);

        if self.is_me(nick) {
            if let (Some(u), Some(h)) = (user, host) {
                self.mask = Some(format!("{nick}!{u}@{h}"));
            }
            self.channels.insert(
                key,
                Channel {
                    name: channel.to_owned(),
                    topic: None,
                    members: HashMap::new(),
                },
            );
            out.events.push(Event::Joined {
                channel: channel.to_owned(),
            });
            return;
        }

        // With `extended-join` the account follows the channel; `*` means the
        // user is not logged in.
        let account = msg
            .param(1)
            .filter(|a| *a != "*" && self.caps.is_enabled("extended-join"))
            .map(str::to_owned);

        if let Some(ch) = self.channels.get_mut(&key) {
            ch.members.insert(
                self.isupport.casemapping().fold(nick),
                Member {
                    nick: nick.clone(),
                    prefixes: String::new(),
                },
            );
        }
        out.events.push(Event::MemberJoined {
            channel: channel.to_owned(),
            nick: nick.clone(),
            account,
        });
    }

    fn on_part(&mut self, msg: &Message, out: &mut Output) {
        let (Some(source), Some(channel)) = (msg.source.as_ref(), msg.param(0)) else {
            return;
        };
        let Some(nick) = source.nick() else { return };
        let reason = msg.param(1).map(str::to_owned);
        let key = self.fold(channel);

        if self.is_me(nick) {
            self.channels.remove(&key);
            out.events.push(Event::Parted {
                channel: channel.to_owned(),
                reason,
            });
        } else {
            let nick_key = self.fold(nick);
            if let Some(ch) = self.channels.get_mut(&key) {
                ch.members.remove(&nick_key);
            }
            out.events.push(Event::MemberParted {
                channel: channel.to_owned(),
                nick: nick.to_owned(),
                reason,
            });
        }
    }

    fn on_kick(&mut self, msg: &Message, out: &mut Output) {
        let (Some(channel), Some(victim)) = (msg.param(0), msg.param(1)) else {
            return;
        };
        let by = msg
            .source
            .as_ref()
            .map(|s| s.display_name().to_owned())
            .unwrap_or_default();
        let reason = msg.param(2).map(str::to_owned);
        let key = self.fold(channel);

        if self.is_me(victim) {
            self.channels.remove(&key);
            out.events.push(Event::Kicked {
                channel: channel.to_owned(),
                by,
                reason,
            });
        } else {
            let victim_key = self.fold(victim);
            if let Some(ch) = self.channels.get_mut(&key) {
                ch.members.remove(&victim_key);
            }
            out.events.push(Event::MemberKicked {
                channel: channel.to_owned(),
                nick: victim.to_owned(),
                by,
                reason,
            });
        }
    }

    fn on_quit(&mut self, msg: &Message, out: &mut Output) {
        let Some(nick) = msg.source.as_ref().and_then(Source::nick) else {
            return;
        };
        let nick_key = self.fold(nick);
        let mut channels = Vec::new();
        for ch in self.channels.values_mut() {
            if ch.members.remove(&nick_key).is_some() {
                channels.push(ch.name.clone());
            }
        }
        channels.sort();
        out.events.push(Event::MemberQuit {
            nick: nick.to_owned(),
            reason: msg.param(0).map(str::to_owned),
            channels,
        });
    }

    fn on_nick(&mut self, msg: &Message, out: &mut Output) {
        let (Some(old), Some(new)) = (msg.source.as_ref().and_then(Source::nick), msg.param(0))
        else {
            return;
        };
        let own = self.is_me(old);
        let map = self.isupport.casemapping();
        let (old_key, new_key) = (map.fold(old), map.fold(new));

        let mut channels = Vec::new();
        for ch in self.channels.values_mut() {
            if let Some(mut member) = ch.members.remove(&old_key) {
                member.nick = new.to_owned();
                ch.members.insert(new_key.clone(), member);
                channels.push(ch.name.clone());
            }
        }
        channels.sort();

        if own {
            self.nick = new.to_owned();
            if let Some(mask) = self.mask.as_mut() {
                if let Some((_, rest)) = mask.split_once('!') {
                    *mask = format!("{new}!{rest}");
                }
            }
        }
        out.events.push(Event::NickChanged {
            old: old.to_owned(),
            new: new.to_owned(),
            channels,
            own,
        });
    }

    fn on_topic(&mut self, msg: &Message, out: &mut Output) {
        let Some(channel) = msg.param(0) else { return };
        let topic = msg.param(1).filter(|t| !t.is_empty()).map(str::to_owned);
        let key = self.fold(channel);
        if let Some(ch) = self.channels.get_mut(&key) {
            ch.topic = topic.clone();
        }
        out.events.push(Event::Topic {
            channel: channel.to_owned(),
            topic,
        });
    }

    fn on_names_line(&mut self, msg: &Message) {
        // `<me> <symbol> <channel> :<names>`
        let (Some(channel), Some(names)) = (msg.param(2), msg.param(3)) else {
            return;
        };
        let key = self.fold(channel);
        let entries = self.pending_names.entry(key).or_default();
        for entry in names.split_whitespace() {
            let (prefixes, rest) = self.isupport.split_prefixes(entry);
            // `userhost-in-names` sends the full mask; we only want the nick.
            let nick = rest.split('!').next().unwrap_or(rest);
            if !nick.is_empty() {
                entries.push(Member {
                    nick: nick.to_owned(),
                    prefixes: prefixes.to_owned(),
                });
            }
        }
    }

    fn on_names_end(&mut self, msg: &Message, out: &mut Output) {
        let Some(channel) = msg.param(1) else { return };
        let key = self.fold(channel);
        let members = self.pending_names.remove(&key).unwrap_or_default();

        let map = self.isupport.casemapping();
        if let Some(ch) = self.channels.get_mut(&key) {
            ch.members = members
                .iter()
                .map(|m| (map.fold(&m.nick), m.clone()))
                .collect();
        }
        out.events.push(Event::Names {
            channel: channel.to_owned(),
            members: self.sorted_members(members),
        });
    }

    fn on_mode(&mut self, msg: &Message, out: &mut Output) {
        let (Some(target), Some(modes)) = (msg.param(0), msg.param(1)) else {
            return;
        };
        out.events.push(Event::Mode {
            target: target.to_owned(),
            by: msg
                .source
                .as_ref()
                .map(|s| s.display_name().to_owned())
                .unwrap_or_default(),
            modes: msg.params[1..].join(" "),
        });
        if !self.isupport.is_channel(target) {
            return;
        }

        // Walk the mode string, consuming an argument for each mode that takes
        // one. Getting this wrong misattributes every later change on the
        // line, so the rules come from the network's own CHANMODES.
        let args = &msg.params[2..];
        let mut next_arg = 0usize;
        let mut adding = true;
        let key = self.fold(target);

        for mode in modes.chars() {
            match mode {
                '+' => adding = true,
                '-' => adding = false,
                m if self.isupport.is_membership_mode(m) => {
                    let nick = args.get(next_arg);
                    next_arg += 1;
                    let (Some(nick), Some(prefix)) = (nick, self.isupport.prefix_for_mode(m))
                    else {
                        continue;
                    };
                    let nick_key = self.isupport.casemapping().fold(nick);
                    let isupport = &self.isupport;
                    if let Some(member) = self
                        .channels
                        .get_mut(&key)
                        .and_then(|ch| ch.members.get_mut(&nick_key))
                    {
                        if adding && !member.prefixes.contains(prefix) {
                            member.prefixes.push(prefix);
                        } else if !adding {
                            member.prefixes.retain(|c| c != prefix);
                        }
                        let mut chars: Vec<char> = member.prefixes.chars().collect();
                        chars.sort_by_key(|c| isupport.prefix_rank(*c));
                        member.prefixes = chars.into_iter().collect();
                    }
                }
                m => match self.isupport.chanmodes().kind(m) {
                    Some(ModeKind::List | ModeKind::Setting) => next_arg += 1,
                    Some(ModeKind::SettingOnSet) if adding => next_arg += 1,
                    _ => {}
                },
            }
        }
    }

    // ---- messages --------------------------------------------------------

    fn on_message(&mut self, msg: &Message, notice: bool, out: &mut Output) {
        let (Some(target), Some(body)) = (msg.param(0), msg.param(1)) else {
            return;
        };

        // Text from the server itself (connection notices, MOTD fragments) is
        // not a conversation.
        let Some(Source::User { nick: sender, user, host }) = msg.source.as_ref() else {
            out.events.push(Event::Server(body.to_owned()));
            return;
        };

        let own = self.is_me(sender);
        if own {
            if let (Some(u), Some(h)) = (user, host) {
                self.mask = Some(format!("{sender}!{u}@{h}"));
            }
        }

        let (kind, text) = if let Some(action) = ctcp::action_text(body) {
            (MessageKind::Action, action.to_owned())
        } else if let Some(request) = ctcp::parse(body) {
            if !own {
                out.events.push(Event::Ctcp {
                    from: sender.clone(),
                    command: request.command.clone(),
                    params: request.params.clone(),
                    reply: notice,
                });
                // Answer requests, never replies: two clients answering each
                // other's answers would loop.
                if !notice {
                    if let Some(reply) = ctcp_reply(&request) {
                        out.send.push(Message::with_body("NOTICE", sender, &reply));
                    }
                }
            }
            return;
        } else if notice {
            (MessageKind::Notice, body.to_owned())
        } else {
            (MessageKind::Privmsg, body.to_owned())
        };

        // `@#channel` addresses only the operators of a channel but belongs in
        // that channel's buffer.
        let (_, addressed) = self.isupport.split_statusmsg(target);
        let is_channel = self.isupport.is_channel(addressed);
        let buffer = if is_channel {
            addressed.to_owned()
        } else if own {
            // An echo of a private message we sent belongs with the person we
            // sent it to, not with ourselves.
            target.to_owned()
        } else {
            sender.clone()
        };

        let highlight = !own
            && (!is_channel || mentions(&text, &self.nick, self.isupport.casemapping()));

        out.events.push(Event::Message(ChatMessage {
            buffer,
            sender: sender.clone(),
            text,
            kind,
            time: msg.server_time().map(str::to_owned),
            msgid: msg.tags.get("msgid").map(str::to_owned),
            own,
            highlight,
        }));
    }

    // ---- commands from the user -----------------------------------------

    /// Sends text to a channel or person.
    ///
    /// Text is split on line breaks (each line becomes its own message, so a
    /// pasted block cannot smuggle a command onto the wire) and each line is
    /// cut to fit the server's limit. If the server will not echo our messages
    /// back, they are reported locally so the sender still sees them.
    pub fn send_message(&mut self, target: &str, text: &str, kind: MessageKind) -> Output {
        if target.is_empty() || target.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Output::error(format!("invalid message target {target:?}"));
        }

        let mut out = Output::default();
        let (command, wrapped) = match kind {
            MessageKind::Privmsg => ("PRIVMSG", false),
            MessageKind::Notice => ("NOTICE", false),
            MessageKind::Action => ("PRIVMSG", true),
        };
        // `\x01ACTION ` before and `\x01` after the text.
        const ACTION_OVERHEAD: usize = 9;
        let budget = split::payload_budget(command, target, self.mask.as_deref())
            .saturating_sub(if wrapped { ACTION_OVERHEAD } else { 0 })
            .max(1);

        let echoed_by_server = self.caps.is_enabled("echo-message");
        let (_, addressed) = self.isupport.split_statusmsg(target);
        let buffer = addressed.to_owned();

        for line in text.split('\n') {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            for chunk in split::split_utf8(line, budget) {
                let body = if wrapped {
                    ctcp::action(chunk)
                } else {
                    chunk.to_owned()
                };
                out.send.push(Message::with_body(command, target, &body));
                if !echoed_by_server {
                    out.events.push(Event::Message(ChatMessage {
                        buffer: buffer.clone(),
                        sender: self.nick.clone(),
                        text: chunk.to_owned(),
                        kind,
                        time: None,
                        msgid: None,
                        own: true,
                        highlight: false,
                    }));
                }
            }
        }
        out
    }

    /// Joins channels.
    pub fn join(&mut self, channels: &[String]) -> Output {
        let (valid, invalid): (Vec<&String>, Vec<&String>) =
            channels.iter().partition(|c| is_plain_token(c));
        let mut out = Output::default();
        for bad in invalid {
            out.events.push(Event::Error {
                code: 0,
                text: format!("invalid channel name {bad:?}"),
            });
        }
        let valid: Vec<String> = valid.into_iter().cloned().collect();
        out.send.extend(join_messages(&valid));
        out
    }

    /// Leaves a channel.
    pub fn part(&mut self, channel: &str, reason: Option<&str>) -> Output {
        if !is_plain_token(channel) {
            return Output::error(format!("invalid channel name {channel:?}"));
        }
        let msg = match reason {
            Some(r) => Message::with_trailing("PART", [channel], r),
            None => Message::new("PART", [channel]),
        };
        Output {
            send: vec![msg],
            events: Vec::new(),
        }
    }

    /// Asks for a new nick.
    pub fn change_nick(&mut self, nick: &str) -> Output {
        if !is_plain_token(nick) {
            return Output::error(format!("invalid nick {nick:?}"));
        }
        Output {
            send: vec![Message::new("NICK", [nick])],
            events: Vec::new(),
        }
    }

    /// Leaves the network.
    pub fn quit(&mut self, reason: Option<&str>) -> Output {
        let msg = match reason {
            Some(r) => Message::with_trailing("QUIT", Vec::<String>::new(), r),
            None => Message::new("QUIT", Vec::<String>::new()),
        };
        Output {
            send: vec![msg],
            events: Vec::new(),
        }
    }

    /// Sends a line as typed, for `/quote`.
    pub fn raw(&mut self, line: &str) -> Output {
        match Message::parse(line) {
            Ok(msg) => Output {
                send: vec![msg],
                events: Vec::new(),
            },
            Err(e) => Output::error(format!("not a valid IRC line: {e}")),
        }
    }
}

/// Whether a value can be used as a single bare parameter: non-empty, with no
/// whitespace or control characters.
fn is_plain_token(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Builds `JOIN` lines from a list of channels, splitting so that no line
/// approaches the size limit.
fn join_messages(channels: &[String]) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut line = String::new();
    for channel in channels {
        if !line.is_empty() && line.len() + 1 + channel.len() > JOIN_LINE_BYTES {
            messages.push(Message::new("JOIN", [std::mem::take(&mut line)]));
        }
        if !line.is_empty() {
            line.push(',');
        }
        line.push_str(channel);
    }
    if !line.is_empty() {
        messages.push(Message::new("JOIN", [line]));
    }
    messages
}

/// The answer to a CTCP request we understand.
fn ctcp_reply(request: &ctcp::Ctcp) -> Option<String> {
    match request.command.as_str() {
        "VERSION" => Some(ctcp::build("VERSION", Some(VERSION_REPLY))),
        "PING" => {
            // Echo the payload, capped: it is attacker-controlled and the
            // reply must still fit in one line.
            let payload = request
                .params
                .as_deref()
                .and_then(|p| split::split_utf8(p, 200).into_iter().next());
            Some(ctcp::build("PING", payload))
        }
        "CLIENTINFO" => Some(ctcp::build("CLIENTINFO", Some("ACTION CLIENTINFO PING VERSION"))),
        _ => None,
    }
}

/// Whether `text` addresses `nick`, as a whole word under the network's
/// case-folding rules.
///
/// A plain substring match would highlight "alp" inside "palpitate", so the
/// characters on either side must not be ones that can continue a nick.
fn mentions(text: &str, nick: &str, map: CaseMapping) -> bool {
    if nick.is_empty() {
        return false;
    }
    // Folding rewrites only ASCII bytes to other single ASCII bytes, so byte
    // offsets in the folded copy line up with the original and land on
    // character boundaries.
    let hay = map.fold(text);
    let needle = map.fold(nick);
    let continues_a_nick = |c: char| c.is_alphanumeric() || "[]\\`_^{|}-".contains(c);

    let mut from = 0;
    while let Some(found) = hay[from..].find(&needle) {
        let start = from + found;
        let end = start + needle.len();
        let before_ok = hay[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !continues_a_nick(c));
        let after_ok = hay[end..]
            .chars()
            .next()
            .is_none_or(|c| !continues_a_nick(c));
        if before_ok && after_ok {
            return true;
        }
        // Step past this occurrence's first character and look again.
        from = start + hay[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::new("irc.example.net", "alp").autojoin(["#rhizome"])
    }

    fn sent(out: &Output) -> Vec<String> {
        out.send.iter().map(Message::to_wire).collect()
    }

    fn feed(session: &mut Session, line: &str) -> Output {
        session.handle(&Message::parse(line).expect("test line should parse"))
    }

    /// Runs a session to the registered state with the given capabilities on
    /// offer, returning everything it sent along the way.
    fn registered(config: Config, offered: &str) -> (Session, Vec<String>) {
        let mut s = Session::new(config);
        let mut all = sent(&s.start());
        all.extend(sent(&feed(&mut s, &format!(":srv CAP * LS :{offered}"))));
        // Acknowledge whatever was requested.
        let requested = all
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix("CAP REQ :").map(str::to_owned));
        if let Some(req) = requested {
            all.extend(sent(&feed(&mut s, &format!(":srv CAP * ACK :{req}"))));
        }
        all.extend(sent(&feed(&mut s, ":srv 001 alp :Welcome")));
        (s, all)
    }

    fn events(out: &Output) -> &[Event] {
        &out.events
    }

    // ---- handshake -------------------------------------------------------

    #[test]
    fn opens_with_cap_ls_then_identifies() {
        let mut s = Session::new(cfg().realname("Alp Yılmaz"));
        assert_eq!(
            sent(&s.start()),
            vec!["CAP LS 302", "NICK alp", "USER alp 0 * :Alp Yılmaz"]
        );
    }

    #[test]
    fn a_server_password_is_sent_before_the_nick() {
        let mut c = cfg();
        c.server_password = Some(crate::config::Secret::new("hunter2 with spaces"));
        let mut s = Session::new(c);
        let lines = sent(&s.start());
        assert_eq!(lines[1], "PASS :hunter2 with spaces");
        assert_eq!(lines[2], "NICK alp");
    }

    #[test]
    fn registration_without_sasl_ends_negotiation_and_joins() {
        let (s, lines) = registered(cfg(), "server-time multi-prefix");
        assert!(lines.contains(&"CAP REQ :server-time multi-prefix".to_owned()));
        assert!(lines.contains(&"CAP END".to_owned()));
        assert_eq!(lines.last().map(String::as_str), Some("JOIN #rhizome"));
        assert!(s.is_registered());
    }

    #[test]
    fn nothing_to_request_ends_negotiation_immediately() {
        let mut s = Session::new(cfg());
        s.start();
        let out = feed(&mut s, ":srv CAP * LS :some-vendor-thing");
        assert_eq!(sent(&out), vec!["CAP END"]);
    }

    #[test]
    fn sasl_plain_runs_the_full_exchange_before_cap_end() {
        let config = cfg().sasl_plain("alp", "hunter2");
        let mut s = Session::new(config);
        s.start();

        let out = feed(&mut s, ":srv CAP * LS :sasl=PLAIN,EXTERNAL server-time");
        assert_eq!(sent(&out), vec!["CAP REQ :server-time sasl"]);

        let out = feed(&mut s, ":srv CAP * ACK :server-time sasl");
        assert_eq!(sent(&out), vec!["AUTHENTICATE PLAIN"]);

        let out = feed(&mut s, "AUTHENTICATE +");
        assert_eq!(sent(&out), vec!["AUTHENTICATE AGFscABodW50ZXIy"]);

        // The account name arrives with 900, then 903 ends the exchange.
        feed(&mut s, ":srv 900 alp alp!~a@host alp :You are now logged in as alp");
        let out = feed(&mut s, ":srv 903 alp :SASL authentication successful");
        assert_eq!(sent(&out), vec!["CAP END"]);
        assert!(!s.is_closed());
    }

    #[test]
    fn failed_sasl_abandons_the_connection_rather_than_continuing() {
        let mut s = Session::new(cfg().sasl_plain("alp", "wrong"));
        s.start();
        feed(&mut s, ":srv CAP * LS :sasl=PLAIN");
        feed(&mut s, ":srv CAP * ACK :sasl");
        feed(&mut s, "AUTHENTICATE +");
        let out = feed(&mut s, ":srv 904 alp :SASL authentication failed");

        assert!(s.is_closed());
        assert!(s.failure().unwrap().contains("904"));
        assert!(matches!(events(&out)[0], Event::AuthFailed(_)));
        assert_eq!(sent(&out), vec!["QUIT :Authentication failed"]);
    }

    #[test]
    fn sasl_plain_is_refused_without_tls_and_the_password_is_never_sent() {
        let mut s = Session::new(cfg().plaintext().sasl_plain("alp", "hunter2"));
        s.start();
        let out = feed(&mut s, ":srv CAP * LS :sasl=PLAIN");

        assert!(s.is_closed());
        assert!(s.failure().unwrap().contains("TLS"));
        let wire = sent(&out).join("\n");
        assert!(!wire.contains("AUTHENTICATE"));
        assert!(!wire.contains("hunter2"));
    }

    #[test]
    fn configured_sasl_on_a_server_without_it_fails_loudly() {
        let mut s = Session::new(cfg().sasl_plain("alp", "x"));
        s.start();
        feed(&mut s, ":srv CAP * LS :server-time");
        assert!(s.is_closed());
        assert!(s.failure().unwrap().contains("does not offer SASL"));
    }

    #[test]
    fn a_refused_request_is_retried_one_capability_at_a_time() {
        let mut s = Session::new(cfg());
        s.start();
        let out = feed(&mut s, ":srv CAP * LS :server-time multi-prefix away-notify");
        assert_eq!(sent(&out), vec!["CAP REQ :server-time multi-prefix away-notify"]);

        // The whole request is refused because of one of them.
        let out = feed(
            &mut s,
            ":srv CAP * NAK :server-time multi-prefix away-notify",
        );
        assert_eq!(
            sent(&out),
            vec![
                "CAP REQ :server-time",
                "CAP REQ :multi-prefix",
                "CAP REQ :away-notify"
            ]
        );

        // The server accepts two and refuses one; negotiation ends only after
        // every request is answered.
        assert!(sent(&feed(&mut s, ":srv CAP * ACK :server-time")).is_empty());
        assert!(sent(&feed(&mut s, ":srv CAP * NAK :away-notify")).is_empty());
        let out = feed(&mut s, ":srv CAP * ACK :multi-prefix");
        assert_eq!(sent(&out), vec!["CAP END"]);

        assert!(s.capabilities().is_enabled("server-time"));
        assert!(s.capabilities().is_enabled("multi-prefix"));
        assert!(!s.capabilities().is_enabled("away-notify"));
    }

    #[test]
    fn an_ack_split_across_lines_waits_for_the_rest() {
        let mut s = Session::new(cfg());
        s.start();
        feed(&mut s, ":srv CAP * LS :server-time multi-prefix");
        assert!(sent(&feed(&mut s, ":srv CAP * ACK :server-time")).is_empty());
        assert_eq!(
            sent(&feed(&mut s, ":srv CAP * ACK :multi-prefix")),
            vec!["CAP END"]
        );
    }

    #[test]
    fn a_taken_nick_gets_an_underscore_and_is_retried() {
        let mut s = Session::new(cfg());
        s.start();
        let out = feed(&mut s, ":srv 433 * alp :Nickname is already in use");
        assert_eq!(sent(&out), vec!["NICK alp_"]);
        let out = feed(&mut s, ":srv 433 * alp_ :Nickname is already in use");
        assert_eq!(sent(&out), vec!["NICK alp__"]);
        assert_eq!(s.nick(), "alp__");
    }

    #[test]
    fn giving_up_on_nicks_is_a_failure_not_an_endless_loop() {
        let mut s = Session::new(cfg());
        s.start();
        for _ in 0..=MAX_NICK_ATTEMPTS {
            feed(&mut s, ":srv 433 * alp :in use");
        }
        assert!(s.is_closed());
        assert!(s.failure().is_some());
    }

    #[test]
    fn the_welcome_sets_the_nick_the_server_actually_gave_us() {
        let mut s = Session::new(cfg());
        s.start();
        feed(&mut s, ":srv CAP * LS :");
        let out = feed(&mut s, ":srv 001 alp2 :Welcome");
        assert_eq!(s.nick(), "alp2");
        assert!(matches!(&events(&out)[0], Event::Registered { nick } if nick == "alp2"));
    }

    #[test]
    fn a_server_without_cap_support_still_registers() {
        let mut s = Session::new(cfg());
        s.start();
        feed(&mut s, ":srv 421 alp CAP :Unknown command");
        feed(&mut s, ":srv 001 alp :Welcome");
        assert!(s.is_registered());
    }

    #[test]
    fn many_autojoin_channels_are_split_across_lines() {
        let channels: Vec<String> = (0..60).map(|i| format!("#channel-number-{i}")).collect();
        let msgs = join_messages(&channels);
        assert!(msgs.len() > 1);
        let rejoined: Vec<&str> = msgs
            .iter()
            .flat_map(|m| m.param(0).unwrap().split(','))
            .collect();
        assert_eq!(rejoined.len(), 60);
        for m in &msgs {
            assert!(m.to_wire_line().len() <= 512);
        }
    }

    #[test]
    fn ping_is_answered_with_the_same_token() {
        let mut s = Session::new(cfg());
        let out = feed(&mut s, "PING :LAG1234");
        assert_eq!(sent(&out), vec!["PONG LAG1234"]);
        let out = feed(&mut s, "PING :token with spaces");
        assert_eq!(sent(&out), vec!["PONG :token with spaces"]);
    }

    #[test]
    fn the_server_closing_with_error_ends_the_session() {
        let (mut s, _) = registered(cfg(), "");
        let out = feed(&mut s, "ERROR :Closing Link: alp (Ping timeout)");
        assert!(s.is_closed());
        assert_eq!(s.server_closed(), Some("Closing Link: alp (Ping timeout)"));
        assert!(s.failure().is_none(), "a dropped connection is retryable");
        assert!(matches!(events(&out)[0], Event::ServerError(_)));
    }

    // ---- channels --------------------------------------------------------

    fn in_channel() -> Session {
        let (mut s, _) = registered(cfg(), "server-time echo-message multi-prefix");
        feed(
            &mut s,
            ":srv 005 alp PREFIX=(qaohv)~&@%+ CHANTYPES=# CASEMAPPING=rfc1459 \
             CHANMODES=eIbq,k,flj,CFLMPQScgimnprstz NETWORK=TestNet STATUSMSG=@+ \
             :are supported by this server",
        );
        feed(&mut s, ":alp!~alp@host JOIN #rhizome");
        feed(
            &mut s,
            ":srv 353 alp = #rhizome :@alp +bob carol ~owner",
        );
        feed(&mut s, ":srv 366 alp #rhizome :End of /NAMES list.");
        s
    }

    #[test]
    fn joining_a_channel_reports_it_and_learns_our_mask() {
        let (mut s, _) = registered(cfg(), "");
        let out = feed(&mut s, ":alp!~alp@user/alp JOIN #rhizome");
        assert_eq!(
            events(&out),
            &[Event::Joined {
                channel: "#rhizome".into()
            }]
        );
        assert_eq!(s.channel_names(), vec!["#rhizome"]);
    }

    #[test]
    fn names_are_ordered_by_privilege_then_name() {
        let s = in_channel();
        let names: Vec<_> = s
            .members("#rhizome")
            .unwrap()
            .into_iter()
            .map(|m| format!("{}{}", m.prefixes, m.nick))
            .collect();
        assert_eq!(names, vec!["~owner", "@alp", "+bob", "carol"]);
    }

    #[test]
    fn a_names_reply_split_over_several_lines_is_joined_at_the_end() {
        let (mut s, _) = registered(cfg(), "");
        feed(&mut s, ":alp!~a@h JOIN #big");
        assert!(events(&feed(&mut s, ":srv 353 alp = #big :a b")).is_empty());
        assert!(events(&feed(&mut s, ":srv 353 alp = #big :c d")).is_empty());
        let out = feed(&mut s, ":srv 366 alp #big :End");
        match &events(&out)[0] {
            Event::Names { members, .. } => assert_eq!(members.len(), 4),
            other => panic!("expected Names, got {other:?}"),
        }
    }

    #[test]
    fn channel_lookups_use_the_networks_casemapping() {
        let mut s = in_channel();
        // Same channel, different case; and nick brackets under rfc1459.
        feed(&mut s, ":Dave[x]!d@h JOIN #RHIZOME");
        let out = feed(&mut s, ":dave{x}!d@h PART #rhizome :bye");
        assert!(matches!(&events(&out)[0], Event::MemberParted { .. }));
        assert!(s
            .members("#Rhizome")
            .unwrap()
            .iter()
            .all(|m| !m.nick.to_lowercase().starts_with("dave")));
    }

    #[test]
    fn a_member_joining_and_parting_updates_the_list() {
        let mut s = in_channel();
        feed(&mut s, ":dave!d@h JOIN #rhizome");
        assert!(s.members("#rhizome").unwrap().iter().any(|m| m.nick == "dave"));
        feed(&mut s, ":dave!d@h PART #rhizome");
        assert!(!s.members("#rhizome").unwrap().iter().any(|m| m.nick == "dave"));
    }

    #[test]
    fn extended_join_reports_the_account() {
        let (mut s, _) = registered(cfg(), "extended-join");
        feed(&mut s, ":alp!~a@h JOIN #rhizome dave-account :Alp");
        let out = feed(&mut s, ":dave!d@h JOIN #rhizome dave-account :Dave");
        assert!(matches!(
            &events(&out)[0],
            Event::MemberJoined { account: Some(a), .. } if a == "dave-account"
        ));
        let out = feed(&mut s, ":eve!e@h JOIN #rhizome * :Eve");
        assert!(matches!(
            &events(&out)[0],
            Event::MemberJoined { account: None, .. }
        ));
    }

    #[test]
    fn a_quit_reports_only_the_channels_we_shared() {
        let mut s = in_channel();
        feed(&mut s, ":alp!~a@h JOIN #other");
        feed(&mut s, ":srv 353 alp = #other :alp dave");
        feed(&mut s, ":srv 366 alp #other :End");
        let out = feed(&mut s, ":bob!b@h QUIT :Ping timeout");
        assert_eq!(
            events(&out),
            &[Event::MemberQuit {
                nick: "bob".into(),
                reason: Some("Ping timeout".into()),
                channels: vec!["#rhizome".into()],
            }]
        );
        assert!(!s.members("#rhizome").unwrap().iter().any(|m| m.nick == "bob"));
    }

    #[test]
    fn a_nick_change_renames_the_member_and_keeps_their_prefix() {
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h NICK robert");
        assert!(matches!(
            &events(&out)[0],
            Event::NickChanged { own: false, channels, .. } if channels == &vec!["#rhizome".to_owned()]
        ));
        let m = s.members("#rhizome").unwrap();
        let robert = m.iter().find(|m| m.nick == "robert").unwrap();
        assert_eq!(robert.prefixes, "+");
        assert!(!m.iter().any(|m| m.nick == "bob"));
    }

    #[test]
    fn our_own_nick_change_updates_the_session() {
        let mut s = in_channel();
        let out = feed(&mut s, ":alp!~alp@host NICK alp_away");
        assert!(matches!(&events(&out)[0], Event::NickChanged { own: true, .. }));
        assert_eq!(s.nick(), "alp_away");
        // Our messages are recognised under the new nick.
        let out = feed(&mut s, ":alp_away!~alp@host PRIVMSG #rhizome :hi");
        assert!(matches!(&events(&out)[0], Event::Message(m) if m.own));
    }

    #[test]
    fn being_kicked_removes_the_channel() {
        let mut s = in_channel();
        let out = feed(&mut s, ":op!o@h KICK #rhizome alp :behave");
        assert_eq!(
            events(&out),
            &[Event::Kicked {
                channel: "#rhizome".into(),
                by: "op".into(),
                reason: Some("behave".into()),
            }]
        );
        assert!(s.channel_names().is_empty());
    }

    #[test]
    fn mode_changes_move_prefixes_using_the_networks_own_rules() {
        let mut s = in_channel();
        // `+b` takes an argument, `+l` takes one, `+m` takes none, `+o` takes a
        // nick. Misjudging any of them would give the ops to the wrong person.
        feed(&mut s, ":op!o@h MODE #rhizome +bml *!*@spam 50");
        feed(&mut s, ":op!o@h MODE #rhizome +o carol");
        feed(&mut s, ":op!o@h MODE #rhizome -v+h bob carol");

        let m = s.members("#rhizome").unwrap();
        let prefix_of = |n: &str| m.iter().find(|m| m.nick == n).unwrap().prefixes.clone();
        assert_eq!(prefix_of("bob"), "", "voice removed");
        assert_eq!(prefix_of("carol"), "@%", "op then halfop, highest first");
    }

    #[test]
    fn the_topic_is_tracked_from_both_the_reply_and_the_command() {
        let mut s = in_channel();
        let out = feed(&mut s, ":srv 332 alp #rhizome :welcome");
        assert!(matches!(&events(&out)[0], Event::Topic { topic: Some(t), .. } if t == "welcome"));
        let out = feed(&mut s, ":op!o@h TOPIC #rhizome :new topic");
        assert!(matches!(&events(&out)[0], Event::Topic { topic: Some(t), .. } if t == "new topic"));
        let out = feed(&mut s, ":op!o@h TOPIC #rhizome :");
        assert!(matches!(&events(&out)[0], Event::Topic { topic: None, .. }));
    }

    #[test]
    fn the_network_name_is_reported_once() {
        let (mut s, _) = registered(cfg(), "");
        let out = feed(&mut s, ":srv 005 alp NETWORK=TestNet :are supported");
        assert_eq!(events(&out), &[Event::Network("TestNet".into())]);
        let out = feed(&mut s, ":srv 005 alp NICKLEN=16 :are supported");
        assert!(events(&out).is_empty());
    }

    // ---- messages --------------------------------------------------------

    #[test]
    fn a_channel_message_lands_in_that_channel_with_its_metadata() {
        let mut s = in_channel();
        let out = feed(
            &mut s,
            "@time=2026-09-25T10:00:00.000Z;msgid=abc :bob!b@h PRIVMSG #rhizome :merhaba",
        );
        assert_eq!(
            events(&out),
            &[Event::Message(ChatMessage {
                buffer: "#rhizome".into(),
                sender: "bob".into(),
                text: "merhaba".into(),
                kind: MessageKind::Privmsg,
                time: Some("2026-09-25T10:00:00.000Z".into()),
                msgid: Some("abc".into()),
                own: false,
                highlight: false,
            })]
        );
    }

    #[test]
    fn a_private_message_lands_in_a_buffer_named_for_the_sender() {
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h PRIVMSG alp :psst");
        match &events(&out)[0] {
            Event::Message(m) => {
                assert_eq!(m.buffer, "bob");
                assert!(m.highlight, "a private message always draws attention");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_echo_of_our_own_private_message_lands_with_the_recipient() {
        let mut s = in_channel();
        let out = feed(&mut s, ":alp!~alp@host PRIVMSG bob :hi bob");
        match &events(&out)[0] {
            Event::Message(m) => {
                assert_eq!(m.buffer, "bob");
                assert!(m.own);
                assert!(!m.highlight);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_statusmsg_message_belongs_to_the_channel() {
        let mut s = in_channel();
        let out = feed(&mut s, ":op!o@h PRIVMSG @#rhizome :ops only");
        assert!(matches!(&events(&out)[0], Event::Message(m) if m.buffer == "#rhizome"));
    }

    #[test]
    fn highlights_match_whole_words_case_insensitively() {
        let mut s = in_channel();
        let hl = |s: &mut Session, text: &str| match &feed(
            s,
            &format!(":bob!b@h PRIVMSG #rhizome :{text}"),
        )
        .events[0]
        {
            Event::Message(m) => m.highlight,
            other => panic!("{other:?}"),
        };
        assert!(hl(&mut s, "alp: are you there?"));
        assert!(hl(&mut s, "hey ALP"));
        assert!(hl(&mut s, "thanks, alp."));
        assert!(!hl(&mut s, "palpitations"), "must not match inside a word");
        assert!(!hl(&mut s, "alpine skiing"));
        assert!(!hl(&mut s, "nothing to see"));
    }

    #[test]
    fn an_action_is_decoded_not_shown_as_control_characters() {
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h PRIVMSG #rhizome :\u{1}ACTION waves\u{1}");
        match &events(&out)[0] {
            Event::Message(m) => {
                assert_eq!(m.kind, MessageKind::Action);
                assert_eq!(m.text, "waves");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_notice_from_a_user_is_a_notice_and_from_a_server_is_server_text() {
        let mut s = in_channel();
        let out = feed(&mut s, ":NickServ!NickServ@services. NOTICE alp :You are logged in");
        assert!(matches!(&events(&out)[0], Event::Message(m)
            if m.kind == MessageKind::Notice && m.buffer == "NickServ"));

        let out = feed(&mut s, ":irc.example.net NOTICE * :*** Looking up your hostname");
        assert_eq!(
            events(&out),
            &[Event::Server("*** Looking up your hostname".into())]
        );
    }

    #[test]
    fn ctcp_version_is_answered_with_a_notice() {
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h PRIVMSG alp :\u{1}VERSION\u{1}");
        assert_eq!(
            sent(&out),
            vec![format!("NOTICE bob :\u{1}VERSION {VERSION_REPLY}\u{1}")]
        );
        assert!(matches!(&events(&out)[0], Event::Ctcp { reply: false, .. }));
    }

    #[test]
    fn ctcp_ping_echoes_its_payload() {
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h PRIVMSG alp :\u{1}PING 12345\u{1}");
        assert_eq!(sent(&out), vec!["NOTICE bob :\u{1}PING 12345\u{1}"]);
    }

    #[test]
    fn a_ctcp_reply_is_never_answered() {
        // Answering replies would let two clients loop forever.
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h NOTICE alp :\u{1}VERSION SomeClient 1.0\u{1}");
        assert!(sent(&out).is_empty());
        assert!(matches!(&events(&out)[0], Event::Ctcp { reply: true, .. }));
    }

    #[test]
    fn an_unknown_ctcp_request_is_reported_but_not_answered() {
        let mut s = in_channel();
        let out = feed(&mut s, ":bob!b@h PRIVMSG alp :\u{1}FINGER\u{1}");
        assert!(sent(&out).is_empty());
        assert!(matches!(&events(&out)[0], Event::Ctcp { .. }));
    }

    #[test]
    fn a_giant_ctcp_ping_payload_cannot_overflow_the_reply() {
        let mut s = in_channel();
        let line = format!(":bob!b@h PRIVMSG alp :\u{1}PING {}\u{1}", "x".repeat(450));
        let out = feed(&mut s, &line);
        assert_eq!(out.send.len(), 1);
        assert_eq!(out.send[0].validate_for_send(), Ok(()));
    }

    // ---- sending ---------------------------------------------------------

    #[test]
    fn sending_without_server_echo_reports_the_message_locally() {
        let (mut s, _) = registered(cfg(), "");
        let out = s.send_message("#rhizome", "merhaba", MessageKind::Privmsg);
        assert_eq!(sent(&out), vec!["PRIVMSG #rhizome :merhaba"]);
        assert!(matches!(&events(&out)[0], Event::Message(m) if m.own && m.buffer == "#rhizome"));
    }

    #[test]
    fn sending_with_server_echo_does_not_show_the_message_twice() {
        let (mut s, _) = registered(cfg(), "echo-message");
        let out = s.send_message("#rhizome", "merhaba", MessageKind::Privmsg);
        assert_eq!(sent(&out), vec!["PRIVMSG #rhizome :merhaba"]);
        assert!(
            events(&out).is_empty(),
            "the server's echo is the one source of truth"
        );
    }

    #[test]
    fn a_pasted_line_break_becomes_separate_messages_never_a_second_command() {
        let (mut s, _) = registered(cfg(), "");
        let out = s.send_message(
            "#rhizome",
            "hello\r\nQUIT :pwned\nPRIVMSG NickServ :identify hunter2",
            MessageKind::Privmsg,
        );
        assert_eq!(
            sent(&out),
            vec![
                "PRIVMSG #rhizome :hello",
                "PRIVMSG #rhizome :QUIT :pwned",
                "PRIVMSG #rhizome :PRIVMSG NickServ :identify hunter2",
            ]
        );
        for m in &out.send {
            assert_eq!(m.validate_for_send(), Ok(()));
        }
    }

    #[test]
    fn blank_lines_in_a_paste_are_dropped() {
        let (mut s, _) = registered(cfg(), "");
        let out = s.send_message("#c", "a\n\n   \nb\n", MessageKind::Privmsg);
        assert_eq!(sent(&out), vec!["PRIVMSG #c :a", "PRIVMSG #c :b"]);
    }

    #[test]
    fn a_long_turkish_message_is_split_and_every_line_fits() {
        let (mut s, _) = registered(cfg(), "");
        let text = "Merhaba dünya, şu uzun Türkçe cümleyi bölmek gerekiyor. ".repeat(20);
        let out = s.send_message("#rhizome", text.trim_end(), MessageKind::Privmsg);
        assert!(out.send.len() > 1);
        for m in &out.send {
            assert_eq!(m.validate_for_send(), Ok(()), "{}", m.to_wire());
        }
    }

    #[test]
    fn an_action_is_wrapped_and_still_fits() {
        let (mut s, _) = registered(cfg(), "");
        let out = s.send_message("#c", "waves", MessageKind::Action);
        assert_eq!(sent(&out), vec!["PRIVMSG #c :\u{1}ACTION waves\u{1}"]);

        let long = "word ".repeat(200);
        let out = s.send_message("#c", &long, MessageKind::Action);
        for m in &out.send {
            assert_eq!(m.validate_for_send(), Ok(()));
        }
    }

    #[test]
    fn a_bad_target_is_refused_before_anything_is_sent() {
        let (mut s, _) = registered(cfg(), "");
        for bad in ["", "#a b", "#a\nQUIT", "x\r"] {
            let out = s.send_message(bad, "hi", MessageKind::Privmsg);
            assert!(out.send.is_empty(), "target {bad:?} should be refused");
            assert!(matches!(&events(&out)[0], Event::Error { code: 0, .. }));
        }
    }

    #[test]
    fn join_part_nick_and_quit_build_the_right_lines() {
        let (mut s, _) = registered(cfg(), "");
        assert_eq!(
            sent(&s.join(&["#a".to_owned(), "#b".to_owned()])),
            vec!["JOIN #a,#b"]
        );
        assert_eq!(sent(&s.part("#a", None)), vec!["PART #a"]);
        assert_eq!(sent(&s.part("#a", Some("later"))), vec!["PART #a :later"]);
        assert_eq!(sent(&s.change_nick("alp2")), vec!["NICK alp2"]);
        assert_eq!(sent(&s.quit(Some("bye now"))), vec!["QUIT :bye now"]);
        assert_eq!(sent(&s.quit(None)), vec!["QUIT"]);
    }

    #[test]
    fn join_refuses_names_that_are_not_a_single_token() {
        let (mut s, _) = registered(cfg(), "");
        let out = s.join(&["#ok".to_owned(), "#bad name".to_owned(), "#x\nQUIT".to_owned()]);
        assert_eq!(sent(&out), vec!["JOIN #ok"]);
        assert_eq!(out.events.len(), 2);
    }

    #[test]
    fn raw_lines_are_parsed_and_bad_ones_reported() {
        let (mut s, _) = registered(cfg(), "");
        assert_eq!(sent(&s.raw("WHOIS bob")), vec!["WHOIS bob"]);
        assert!(matches!(&s.raw("@only-tags").events[0], Event::Error { .. }));
    }

    #[test]
    fn errors_from_the_server_are_surfaced() {
        let mut s = in_channel();
        let out = feed(&mut s, ":srv 474 alp #secret :Cannot join channel (+b)");
        assert_eq!(
            events(&out),
            &[Event::Error {
                code: 474,
                text: "#secret Cannot join channel (+b)".into()
            }]
        );
    }

    #[test]
    fn a_nick_collision_after_registration_is_an_ordinary_error() {
        let mut s = in_channel();
        let out = feed(&mut s, ":srv 433 alp taken :Nickname is already in use");
        assert!(sent(&out).is_empty(), "must not start guessing nicks now");
        assert!(matches!(&events(&out)[0], Event::Error { code: 433, .. }));
        assert_eq!(s.nick(), "alp");
    }

    // ---- helpers ---------------------------------------------------------

    #[test]
    fn mentions_handles_non_ascii_neighbours() {
        let map = CaseMapping::Rfc1459;
        assert!(mentions("Merhaba alp, nasılsın?", "alp", map));
        assert!(mentions("şu alp'e sor", "alp", map));
        assert!(!mentions("çalpa", "alp", map));
        assert!(!mentions("", "alp", map));
        assert!(!mentions("alp", "", map));
    }

    #[test]
    fn mentions_uses_rfc1459_folding_for_brackets() {
        assert!(mentions("hi [dave]", "{dave}", CaseMapping::Rfc1459));
        assert!(!mentions("hi [dave]", "{dave}", CaseMapping::Ascii));
    }
}
