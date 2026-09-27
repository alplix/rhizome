//! DCC SEND: a file offer carried inside a CTCP request.
//!
//! `DCC SEND` is itself a CTCP command (see [`crate::ctcp`]), sent as the body
//! of an ordinary `PRIVMSG`. Its own parameters are a second, older
//! convention: whitespace-separated fields, a filename quoted only if it
//! contains a space, and an IPv4 address written out as one 32-bit unsigned
//! integer rather than four dotted numbers — a survival from the protocol's
//! age, not something any other part of IRC does.
//!
//! This module only reads and writes those parameters. Opening a socket and
//! moving bytes over it is real I/O and lives in `rhizome-client`, next to the
//! connection driver, exactly as this crate never touches a socket for chat
//! either.

use std::fmt;
use std::net::Ipv4Addr;

/// A `DCC SEND` offer, parsed from a CTCP `DCC` request's parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Send {
    /// The name of the file, with any directory part already gone: whatever
    /// the sender put there is theirs to name, not a path to write to.
    pub filename: String,
    pub ip: Ipv4Addr,
    pub port: u16,
    pub size: u64,
}

impl Send {
    /// A port of `0` is "reverse" (passive) DCC: the sender is not listening
    /// and expects the receiver to listen instead, coordinated by a token
    /// this module does not parse. Not implemented here, so this is the one
    /// thing worth checking before offering to accept.
    pub fn is_passive(&self) -> bool {
        self.port == 0
    }
}

/// Splits on whitespace, keeping a `"quoted phrase"` together as one field.
fn fields(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = input.trim().chars().peekable();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut field = String::new();
        if first == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                field.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                field.push(c);
                chars.next();
            }
        }
        out.push(field);
    }
    out
}

/// The filename alone, with any directory component removed. A peer choosing
/// what looks like a path (`../../etc/passwd`, `C:\Windows\x.dll`) must never
/// be able to name where a file lands, only what it is called.
fn basename(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Parses the parameters of a CTCP `DCC` request as a `SEND` offer.
///
/// `params` is everything after `DCC` itself, e.g. `SEND "a file.txt"
/// 3232235777 5000 1048576`. Anything else (`DCC CHAT`, `DCC RESUME`, a
/// malformed `SEND`) is `None`: those are not offers this client can act on.
pub fn parse_send(params: &str) -> Option<Send> {
    let mut fields = fields(params);
    if fields.is_empty() || !fields.remove(0).eq_ignore_ascii_case("SEND") {
        return None;
    }
    let mut fields = fields.into_iter();
    let filename = fields.next()?;
    let ip: u32 = fields.next()?.parse().ok()?;
    let port: u16 = fields.next()?.parse().ok()?;
    let size: u64 = fields.next()?.parse().ok()?;
    let filename = basename(&filename);
    if filename.is_empty() || filename == "." || filename == ".." {
        return None;
    }
    Some(Send {
        filename: filename.to_owned(),
        ip: Ipv4Addr::from(ip),
        port,
        size,
    })
}

/// Quotes a filename if it contains whitespace, the one case the format
/// itself distinguishes.
fn quote_if_needed(name: &str) -> String {
    if name.chars().any(char::is_whitespace) {
        format!("\"{name}\"")
    } else {
        name.to_owned()
    }
}

/// Builds the parameters of a `DCC SEND` offer, ready to pass to
/// [`crate::ctcp::build`] as `build("DCC", Some(&params))`.
pub fn build_send(filename: &str, ip: Ipv4Addr, port: u16, size: u64) -> String {
    let ip: u32 = ip.into();
    format!("SEND {} {ip} {port} {size}", quote_if_needed(filename))
}

impl fmt::Display for Send {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({} bytes) from {}:{}",
            self.filename, self.size, self.ip, self.port
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_filename_needs_no_quotes() {
        let send = parse_send("SEND report.pdf 3232235777 5000 1048576").unwrap();
        assert_eq!(send.filename, "report.pdf");
        assert_eq!(send.ip, Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(send.port, 5000);
        assert_eq!(send.size, 1048576);
    }

    #[test]
    fn a_filename_with_spaces_is_quoted() {
        let send = parse_send("SEND \"kernel panic.log\" 16777343 6000 42").unwrap();
        assert_eq!(send.filename, "kernel panic.log");
        assert_eq!(send.ip, Ipv4Addr::new(1, 0, 0, 127));
    }

    #[test]
    fn the_command_is_case_insensitive() {
        assert!(parse_send("send x.txt 1 1 1").is_some());
        assert!(parse_send("Send x.txt 1 1 1").is_some());
    }

    #[test]
    fn only_the_bare_filename_is_kept_never_a_path() {
        assert_eq!(
            parse_send("SEND ../../etc/passwd 1 1 1").unwrap().filename,
            "passwd"
        );
        assert_eq!(
            parse_send("SEND \"C:\\Windows\\evil.dll\" 1 1 1")
                .unwrap()
                .filename,
            "evil.dll"
        );
        assert_eq!(parse_send("SEND .. 1 1 1"), None, "a bare .. names nothing");
        assert_eq!(
            parse_send("SEND \"../\" 1 1 1"),
            None,
            "a path ending in a separator names nothing"
        );
    }

    #[test]
    fn a_port_of_zero_is_a_passive_offer() {
        let send = parse_send("SEND x.txt 1 0 1").unwrap();
        assert!(send.is_passive());
        assert!(!parse_send("SEND x.txt 1 5000 1").unwrap().is_passive());
    }

    #[test]
    fn anything_else_is_not_an_offer_this_client_acts_on() {
        assert_eq!(parse_send(""), None);
        assert_eq!(parse_send("CHAT chat 1 5000"), None);
        assert_eq!(parse_send("RESUME x.txt 5000 100"), None);
        assert_eq!(parse_send("SEND x.txt"), None, "missing fields");
        assert_eq!(parse_send("SEND x.txt notanip 5000 1"), None);
        assert_eq!(parse_send("SEND x.txt 1 notaport 1"), None);
        assert_eq!(parse_send("SEND x.txt 1 5000 notasize"), None);
        assert_eq!(parse_send("SEND x.txt 1 -5 1"), None, "a negative port");
        assert_eq!(parse_send("SEND x.txt 1 99999 1"), None, "a port too large");
    }

    #[test]
    fn building_and_parsing_round_trip() {
        let params = build_send("a file.txt", Ipv4Addr::new(203, 0, 113, 9), 4242, 999);
        assert_eq!(params, "SEND \"a file.txt\" 3405803785 4242 999");
        let send = parse_send(&params).unwrap();
        assert_eq!(send.filename, "a file.txt");
        assert_eq!(send.ip, Ipv4Addr::new(203, 0, 113, 9));
        assert_eq!(send.port, 4242);
        assert_eq!(send.size, 999);
    }

    #[test]
    fn a_plain_filename_is_never_quoted() {
        assert_eq!(
            build_send("report.pdf", Ipv4Addr::new(1, 2, 3, 4), 1, 1),
            "SEND report.pdf 16909060 1 1"
        );
    }

    #[test]
    fn the_offer_displays_readably() {
        let send = parse_send("SEND report.pdf 3232235777 5000 1048576").unwrap();
        assert_eq!(
            send.to_string(),
            "report.pdf (1048576 bytes) from 192.168.1.1:5000"
        );
    }
}
