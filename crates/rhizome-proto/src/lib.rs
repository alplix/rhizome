//! The IRC line protocol, with no I/O and no dependencies.
//!
//! This crate knows how to turn bytes into messages and messages back into
//! bytes, and it knows the per-network rules that decide what those messages
//! mean. It does not open sockets, keep connection state, or track channels;
//! that belongs to `rhizome-client`, which is built on top of this.
//!
//! Keeping the two apart means the desktop app, the Android app and any
//! headless tool can share one parser, and that the parser can be tested
//! exhaustively without a network.
//!
//! # Four rules worth knowing before using this
//!
//! **Compare names with [`CaseMapping`], never with `to_lowercase`.** On most
//! networks `nick[]` and `nick{}` are the same user. See [`casemap`].
//!
//! **Size outgoing messages in bytes, not characters,** and subtract your own
//! hostmask, because the 512-byte limit applies to the line the server
//! relays. See [`split`].
//!
//! **Read capabilities from [`ISupport`], do not assume them.** Channel
//! prefixes, membership modes and name lengths all vary by network. See
//! [`isupport`].
//!
//! **A message body may not be text.** It can carry formatting codes
//! ([`format`]) or be a CTCP request in disguise ([`ctcp`]).
//!
//! # Example
//!
//! ```
//! use rhizome_proto::{ctcp, format, Message};
//!
//! let msg = Message::parse(":alp!~alp@user/alp PRIVMSG #rhizome :\x02merhaba\x02").unwrap();
//! assert_eq!(msg.source.as_ref().unwrap().nick(), Some("alp"));
//!
//! let body = msg.trailing().unwrap();
//! assert_eq!(format::strip(body), "merhaba");
//! assert!(ctcp::parse(body).is_none());
//! ```

pub mod cap;
pub mod casemap;
pub mod ctcp;
pub mod format;
pub mod isupport;
pub mod message;
pub mod sasl;
pub mod split;

pub use cap::{CapEvent, Capabilities};
pub use casemap::CaseMapping;
pub use ctcp::Ctcp;
pub use format::{Color, Span, Style};
pub use isupport::{ChanModes, ISupport, ModeKind};
pub use message::{Command, Message, ParseError, SendError, Source, Tags};
pub use sasl::Mechanism;
pub use split::{payload_budget, split_for_send, split_utf8};

#[cfg(test)]
mod integration {
    //! Tests that cut across modules, exercising the path a real message takes
    //! from the socket to the screen.

    use super::*;

    #[test]
    fn an_action_in_a_channel_is_fully_decoded() {
        let line = "@time=2026-09-23T22:10:00.000Z \
                    :alp!~alp@user/alp PRIVMSG #rhizome :\u{01}ACTION \u{02}waves\u{02}\u{01}";
        let msg = Message::parse(line).unwrap();

        assert_eq!(msg.server_time(), Some("2026-09-23T22:10:00.000Z"));
        assert_eq!(msg.param(0), Some("#rhizome"));

        let body = msg.trailing().unwrap();
        let action = ctcp::action_text(body).expect("should be an action");
        assert_eq!(format::strip(action), "waves");
    }

    #[test]
    fn a_names_reply_is_split_using_the_networks_own_prefixes() {
        let mut support = ISupport::default();
        support.ingest(
            &Message::parse(
                ":irc.libera.chat 005 alp PREFIX=(qaohv)~&@%+ CHANTYPES=# \
                 CASEMAPPING=rfc1459 :are supported by this server",
            )
            .unwrap(),
        );

        let names = Message::parse(":s 353 alp = #rhizome :@alp +Guest ~owner plain").unwrap();
        let entries: Vec<_> = names
            .trailing()
            .unwrap()
            .split(' ')
            .map(|e| support.split_prefixes(e))
            .collect();

        assert_eq!(
            entries,
            vec![("@", "alp"), ("+", "Guest"), ("~", "owner"), ("", "plain")]
        );

        // The nick we hold is the same user the server means, despite the case.
        let mapping = support.casemapping();
        assert!(mapping.eq("ALP", "alp"));
    }

    #[test]
    fn a_long_turkish_message_is_split_so_every_relayed_line_fits() {
        let mask = "alp!~alp@user/alp";
        let target = "#rhizome";
        let sentence = "Merhaba dünya, şu uzun Türkçe cümleyi bölmek gerekiyor.";
        let text = vec![sentence; 20].join(" ");

        let chunks = split_for_send("PRIVMSG", target, Some(mask), &text);
        assert!(chunks.len() > 1);

        for chunk in &chunks {
            let mut relayed = Message::with_body("PRIVMSG", target, chunk);
            relayed.source = Some(Source::parse(mask));
            assert!(
                relayed.to_wire_line().len() <= message::MAX_MESSAGE_BYTES,
                "relayed line was {} bytes",
                relayed.to_wire_line().len()
            );
            // Each chunk must still be valid UTF-8 on its own.
            assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
        }

        // Nothing is lost or duplicated: the chunks rejoin into the original.
        assert_eq!(chunks.join(" "), text);
    }

    #[test]
    fn the_full_registration_handshake_produces_the_right_lines() {
        use sasl::Mechanism;

        let mut caps = Capabilities::new();
        let mut sent: Vec<String> = Vec::new();

        // We open by asking for the capability list, then send our identity
        // without ending negotiation.
        sent.push(Message::new("CAP", ["LS", "302"]).to_wire());
        sent.push(Message::new("NICK", ["alp"]).to_wire());
        sent.push(Message::with_trailing("USER", ["alp", "0", "*"], "Alp").to_wire());

        // The server answers across two lines.
        for line in [
            ":irc.libera.chat CAP * LS * :sasl=PLAIN,EXTERNAL server-time message-tags",
            ":irc.libera.chat CAP * LS :multi-prefix echo-message away-notify",
        ] {
            let event = cap::parse(&Message::parse(line).unwrap()).unwrap();
            caps.apply(&event);
        }
        assert!(caps.ls_complete());

        // We ask only for what was offered.
        let request = caps.to_request();
        assert!(request.contains(&"sasl"));
        assert!(!request.contains(&"draft/chathistory"));
        sent.push(Message::with_body("CAP", "REQ", &request.join(" ")).to_wire());

        let ack = ":irc.libera.chat CAP alp ACK :sasl server-time message-tags multi-prefix";
        caps.apply(&cap::parse(&Message::parse(ack).unwrap()).unwrap());
        assert!(caps.is_enabled("sasl"));
        assert!(caps.is_enabled("server-time"));

        // With no client certificate we fall back to PLAIN, which is only
        // acceptable because this connection is TLS.
        let candidates = vec![Mechanism::Plain {
            authcid: "alp".into(),
            password: "hunter2".into(),
        }];
        let mechanism = sasl::select(&caps.sasl_mechanisms(), &candidates, true)
            .expect("PLAIN is advertised and we are on TLS");
        sent.push(Message::new("AUTHENTICATE", [mechanism.name()]).to_wire());

        // The server's empty challenge invites the payload.
        let challenge = Message::parse("AUTHENTICATE +").unwrap();
        assert_eq!(
            sasl::decode_payload(challenge.trailing().unwrap()).unwrap(),
            Vec::<u8>::new()
        );
        for payload in mechanism.payloads() {
            sent.push(Message::new("AUTHENTICATE", [payload]).to_wire());
        }

        // 903 is RPL_SASLSUCCESS; only then do we end negotiation.
        let success = Message::parse(":irc.libera.chat 903 alp :SASL authentication successful");
        assert_eq!(success.unwrap().command.numeric(), Some(903));
        sent.push(Message::new("CAP", ["END"]).to_wire());

        // The request covers everything offered from both LS lines, in our
        // own preference order rather than the server's.
        let expected_req =
            "CAP REQ :server-time message-tags echo-message sasl multi-prefix away-notify";
        assert_eq!(
            sent,
            vec![
                "CAP LS 302",
                "NICK alp",
                "USER alp 0 * :Alp",
                expected_req,
                "AUTHENTICATE PLAIN",
                "AUTHENTICATE AGFscABodW50ZXIy",
                "CAP END",
            ]
        );

        // Nothing was requested that the server had not offered.
        assert!(request.iter().all(|c| caps.is_available(c)));
    }

    #[test]
    fn a_nak_leaves_us_able_to_finish_registration_unauthenticated() {
        let mut caps = Capabilities::new();
        caps.apply(
            &cap::parse(&Message::parse(":s CAP * LS :sasl=PLAIN server-time").unwrap()).unwrap(),
        );
        caps.apply(
            &cap::parse(&Message::parse(":s CAP alp NAK :sasl server-time").unwrap()).unwrap(),
        );

        // Nothing got enabled, so we must not start an AUTHENTICATE exchange
        // that the server will never answer.
        assert!(!caps.is_enabled("sasl"));
        assert!(!caps.is_enabled("server-time"));
    }

    #[test]
    fn a_mode_change_is_parsed_against_the_networks_chanmodes() {
        let mut support = ISupport::default();
        support.ingest(
            &Message::parse(
                ":s 005 alp PREFIX=(ov)@+ CHANMODES=eIbq,k,flj,CFLMPQScgimnprstz \
                 :are supported by this server",
            )
            .unwrap(),
        );

        // +o takes a nick, +l takes a limit, +m takes nothing.
        let msg = Message::parse(":s MODE #rhizome +oml alp 50").unwrap();
        let modes = msg.param(1).unwrap();
        let mut args = msg.params[2..].iter();

        let mut applied = Vec::new();
        for mode in modes.trim_start_matches('+').chars() {
            let takes_arg = support.is_membership_mode(mode)
                || matches!(
                    support.chanmodes().kind(mode),
                    Some(ModeKind::List | ModeKind::Setting | ModeKind::SettingOnSet)
                );
            applied.push((mode, if takes_arg { args.next().cloned() } else { None }));
        }

        assert_eq!(
            applied,
            vec![
                ('o', Some("alp".to_owned())),
                ('m', None),
                ('l', Some("50".to_owned())),
            ]
        );
        assert!(args.next().is_none(), "every argument should be consumed");
    }
}
