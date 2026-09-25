//! SASL authentication over the `AUTHENTICATE` command.
//!
//! Authenticating with SASL rather than by messaging NickServ matters for two
//! reasons. It happens before registration completes, so the account is
//! applied before anyone can see the connection — which is what lets a cloak
//! be in place from the first moment, and what lets `+r` channels be joined
//! immediately. And it fails loudly: a wrong password aborts the connection
//! instead of silently leaving an unidentified client sitting in a channel it
//! was never let into.
//!
//! The wire exchange is deliberately awkward. The payload is base64, split
//! into 400-byte pieces, and a piece of exactly 400 bytes must be followed by
//! an empty continuation so the server can tell "the payload ended here" from
//! "more follows". [`encode_payload`] handles that.

/// The maximum size of one `AUTHENTICATE` payload piece.
const CHUNK: usize = 400;

/// The payload that means "empty" — an initial response with no data, or the
/// terminator after a full-size chunk.
const EMPTY: &str = "+";

/// A SASL mechanism, with the credentials it needs.
///
/// `Debug` is implemented by hand so the password never reaches a log line or
/// a panic message.
#[derive(Clone, PartialEq, Eq)]
pub enum Mechanism {
    /// Username and password in the clear, protected only by TLS.
    ///
    /// This is what NickServ accounts use. It must never be offered on a
    /// connection without TLS.
    Plain {
        /// The account name to authenticate as.
        authcid: String,
        password: String,
    },
    /// Authentication by TLS client certificate. Nothing secret crosses the
    /// wire, because the certificate already proved who we are during the
    /// handshake.
    External {
        /// The account to act as, when it differs from the certificate's.
        /// Usually empty.
        authzid: String,
    },
}

impl std::fmt::Debug for Mechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mechanism::Plain { authcid, .. } => f
                .debug_struct("Plain")
                .field("authcid", authcid)
                .field("password", &"<redacted>")
                .finish(),
            Mechanism::External { authzid } => f
                .debug_struct("External")
                .field("authzid", authzid)
                .finish(),
        }
    }
}

impl Mechanism {
    /// The name to send in `AUTHENTICATE <name>`.
    pub fn name(&self) -> &'static str {
        match self {
            Mechanism::Plain { .. } => "PLAIN",
            Mechanism::External { .. } => "EXTERNAL",
        }
    }

    /// Whether this mechanism sends a secret in the clear and therefore
    /// requires TLS.
    pub fn requires_tls(&self) -> bool {
        matches!(self, Mechanism::Plain { .. })
    }

    /// The raw response bytes, before base64 and chunking.
    ///
    /// PLAIN is `authzid NUL authcid NUL password`, with an empty authzid
    /// meaning "the same account I am authenticating as".
    fn response(&self) -> Vec<u8> {
        match self {
            Mechanism::Plain { authcid, password } => {
                let mut out = Vec::with_capacity(authcid.len() * 2 + password.len() + 2);
                out.push(0); // empty authzid
                out.extend_from_slice(authcid.as_bytes());
                out.push(0);
                out.extend_from_slice(password.as_bytes());
                out
            }
            Mechanism::External { authzid } => authzid.as_bytes().to_vec(),
        }
    }

    /// The `AUTHENTICATE` payloads to send, in order, once the server has
    /// answered our mechanism proposal with `AUTHENTICATE +`.
    pub fn payloads(&self) -> Vec<String> {
        encode_payload(&self.response())
    }
}

/// Splits a response into `AUTHENTICATE` payloads.
///
/// An empty response is a single `+`. A response whose base64 is an exact
/// multiple of 400 bytes gets a trailing `+`, because without it the server
/// cannot tell that the payload has ended and will wait forever.
pub fn encode_payload(response: &[u8]) -> Vec<String> {
    if response.is_empty() {
        return vec![EMPTY.to_owned()];
    }
    let encoded = base64::encode(response);
    let mut out: Vec<String> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect();
    if encoded.len() % CHUNK == 0 {
        out.push(EMPTY.to_owned());
    }
    out
}

/// Decodes an `AUTHENTICATE` payload sent by the server.
///
/// `+` means an empty payload, which is the usual challenge for both PLAIN and
/// EXTERNAL. Returns `None` if the payload is not valid base64.
pub fn decode_payload(payload: &str) -> Option<Vec<u8>> {
    if payload == EMPTY {
        return Some(Vec::new());
    }
    base64::decode(payload)
}

/// Chooses a mechanism from the ones the server advertises.
///
/// EXTERNAL is preferred when we have a client certificate, since it sends no
/// secret at all. PLAIN is used only over TLS; offering it on a plaintext
/// connection would put the account password on the wire.
pub fn select<'a>(
    advertised: &[String],
    candidates: &'a [Mechanism],
    tls: bool,
) -> Option<&'a Mechanism> {
    candidates
        .iter()
        .find(|m| advertised.iter().any(|a| a == m.name()) && (tls || !m.requires_tls()))
}

/// A minimal base64 codec.
///
/// Inlined rather than taken as a dependency so that this crate stays free of
/// them, which is what lets it be shared unchanged by the desktop app, an
/// Android build and any headless tool.
mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;

            out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6 & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    pub(super) fn decode(input: &str) -> Option<Vec<u8>> {
        let trimmed = input.trim_end_matches('=');
        let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
        let mut acc: u32 = 0;
        let mut bits: u32 = 0;
        for byte in trimmed.bytes() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            } as u32;
            acc = (acc << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_known_vectors() {
        // RFC 4648 section 10.
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64::encode(input.as_bytes()), expected);
            assert_eq!(
                base64::decode(expected).unwrap(),
                input.as_bytes(),
                "decoding {expected}"
            );
        }
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        assert_eq!(base64::decode(&base64::encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn base64_rejects_invalid_input() {
        assert_eq!(base64::decode("not valid!"), None);
    }

    #[test]
    fn debug_output_never_contains_the_password() {
        let m = Mechanism::Plain {
            authcid: "alp".into(),
            password: "hunter2".into(),
        };
        let shown = format!("{m:?}");
        assert!(shown.contains("alp"));
        assert!(!shown.contains("hunter2"), "password leaked: {shown}");
    }

    #[test]
    fn plain_encodes_nul_separated_credentials() {
        let m = Mechanism::Plain {
            authcid: "alp".into(),
            password: "hunter2".into(),
        };
        assert_eq!(m.name(), "PLAIN");
        assert_eq!(m.response(), b"\0alp\0hunter2");

        let payloads = m.payloads();
        assert_eq!(payloads.len(), 1);
        assert_eq!(decode_payload(&payloads[0]).unwrap(), b"\0alp\0hunter2");
    }

    #[test]
    fn plain_handles_non_ascii_passwords() {
        // The password is bytes, not characters; a UTF-8 password must survive
        // the round trip exactly.
        let m = Mechanism::Plain {
            authcid: "alp".into(),
            password: "şifreÇok".into(),
        };
        let decoded = decode_payload(&m.payloads()[0]).unwrap();
        assert_eq!(decoded, "\0alp\0şifreÇok".as_bytes());
    }

    #[test]
    fn external_with_no_authzid_sends_an_empty_payload() {
        let m = Mechanism::External {
            authzid: String::new(),
        };
        assert_eq!(m.name(), "EXTERNAL");
        assert_eq!(m.payloads(), vec!["+"]);
        assert!(!m.requires_tls());
    }

    #[test]
    fn a_payload_of_exactly_400_bytes_is_terminated() {
        // 300 bytes encode to exactly 400 base64 characters. Without the
        // trailing "+", the server waits for a continuation that never comes
        // and the connection hangs at authentication.
        let response = vec![b'x'; 300];
        let payloads = encode_payload(&response);
        assert_eq!(payloads.len(), 2);
        assert_eq!(payloads[0].len(), 400);
        assert_eq!(payloads[1], "+");
    }

    #[test]
    fn a_long_payload_is_split_into_400_byte_pieces() {
        let response = vec![b'x'; 1000];
        let payloads = encode_payload(&response);
        // 1000 bytes encode to 1336 characters: 400 + 400 + 400 + 136.
        assert_eq!(payloads.len(), 4);
        assert!(payloads[..3].iter().all(|p| p.len() == 400));
        assert_eq!(payloads[3].len(), 136);

        let rejoined = payloads.concat();
        assert_eq!(decode_payload(&rejoined).unwrap(), response);
    }

    #[test]
    fn empty_server_challenge_decodes_to_nothing() {
        assert_eq!(decode_payload("+").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn external_is_preferred_over_plain() {
        let candidates = vec![
            Mechanism::External {
                authzid: String::new(),
            },
            Mechanism::Plain {
                authcid: "alp".into(),
                password: "x".into(),
            },
        ];
        let advertised = vec!["PLAIN".to_owned(), "EXTERNAL".to_owned()];
        assert_eq!(
            select(&advertised, &candidates, true).map(Mechanism::name),
            Some("EXTERNAL")
        );
    }

    #[test]
    fn plain_is_never_selected_without_tls() {
        // Selecting it would put the account password on the wire in the clear.
        let candidates = vec![Mechanism::Plain {
            authcid: "alp".into(),
            password: "x".into(),
        }];
        let advertised = vec!["PLAIN".to_owned()];
        assert!(select(&advertised, &candidates, false).is_none());
        assert!(select(&advertised, &candidates, true).is_some());
    }

    #[test]
    fn a_mechanism_the_server_does_not_offer_is_not_selected() {
        let candidates = vec![Mechanism::External {
            authzid: String::new(),
        }];
        let advertised = vec!["PLAIN".to_owned(), "SCRAM-SHA-256".to_owned()];
        assert!(select(&advertised, &candidates, true).is_none());
    }
}
