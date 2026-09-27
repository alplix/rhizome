//! Connection settings.

use std::fmt;
use std::time::Duration;

use rhizome_proto::Mechanism;

use crate::identity::ClientCert;

/// A string that refuses to print itself.
///
/// The server password is a credential. Wrapping it means that a stray
/// `{:?}` on a [`Config`], in a log line or a panic message, cannot leak it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    /// The underlying value. Named to make each use easy to find and audit.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Everything needed to connect to one network.
#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Whether to wrap the connection in TLS. Leave this on: without it,
    /// nothing you send is private, and SASL `PLAIN` is refused outright.
    pub tls: bool,

    pub nick: String,
    pub username: String,
    pub realname: String,

    /// A server password (`PASS`), for networks and bouncers that use one.
    /// This is not the same as a NickServ or SASL password.
    pub server_password: Option<Secret>,

    /// SASL mechanisms to try, most preferred first. If this is non-empty and
    /// none of them can be used, the connection is abandoned rather than
    /// continued unauthenticated: silently proceeding would leave you sitting
    /// in channels without the identity you meant to have.
    pub sasl: Vec<Mechanism>,

    /// Presented during the TLS handshake, for SASL `EXTERNAL`. Only
    /// meaningful together with a matching [`Mechanism::External`] in
    /// [`Config::sasl`] — see [`Config::sasl_external`], which sets up both
    /// at once. Ignored when [`Config::tls`] is off; there is no handshake to
    /// present it in.
    pub client_cert: Option<ClientCert>,

    /// Channels to join once registered, and to rejoin after a reconnect.
    pub autojoin: Vec<String>,

    /// How many messages may be sent back-to-back before the limiter starts
    /// spacing them out.
    pub rate_burst: u32,
    /// The steady send rate afterwards, in messages per second.
    pub rate_per_second: f64,

    /// The first reconnect delay; it doubles on each consecutive failure.
    pub reconnect_min: Duration,
    /// The longest reconnect delay.
    pub reconnect_max: Duration,
}

impl Config {
    /// A TLS connection to `host` on the standard port, with sensible
    /// defaults for everything else.
    pub fn new(host: impl Into<String>, nick: impl Into<String>) -> Config {
        let nick = nick.into();
        Config {
            host: host.into(),
            port: 6697,
            tls: true,
            username: nick.clone(),
            realname: "Rhizome".to_owned(),
            nick,
            server_password: None,
            sasl: Vec::new(),
            client_cert: None,
            autojoin: Vec::new(),
            rate_burst: 5,
            rate_per_second: 1.0,
            reconnect_min: Duration::from_secs(2),
            reconnect_max: Duration::from_secs(300),
        }
    }

    /// Connect without TLS, on the traditional plaintext port unless
    /// [`Config::port`] is set afterwards.
    pub fn plaintext(mut self) -> Config {
        self.tls = false;
        self.port = 6667;
        self
    }

    pub fn port(mut self, port: u16) -> Config {
        self.port = port;
        self
    }

    /// Authenticate with a NickServ account by SASL `PLAIN`.
    pub fn sasl_plain(mut self, account: impl Into<String>, password: impl Into<String>) -> Config {
        self.sasl.push(Mechanism::Plain {
            authcid: account.into(),
            password: password.into(),
        });
        self
    }

    /// Authenticate by SASL `EXTERNAL`: presents `cert` during the TLS
    /// handshake and asks the server to authenticate the connection by it,
    /// rather than a password. `authzid` names the account to act as, when it
    /// differs from the one the certificate itself is registered to —
    /// usually left empty.
    pub fn sasl_external(mut self, cert: ClientCert, authzid: impl Into<String>) -> Config {
        self.client_cert = Some(cert);
        self.sasl.push(Mechanism::External {
            authzid: authzid.into(),
        });
        self
    }

    pub fn autojoin<I, S>(mut self, channels: I) -> Config
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.autojoin.extend(channels.into_iter().map(Into::into));
        self
    }

    pub fn realname(mut self, realname: impl Into<String>) -> Config {
        self.realname = realname.into();
        self
    }

    pub fn username(mut self, username: impl Into<String>) -> Config {
        self.username = username.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_secure_and_conventional() {
        let c = Config::new("irc.libera.chat", "alp");
        assert!(c.tls);
        assert_eq!(c.port, 6697);
        assert_eq!(c.username, "alp");
        assert!(c.sasl.is_empty());
    }

    #[test]
    fn plaintext_switches_to_the_plaintext_port() {
        let c = Config::new("h", "n").plaintext();
        assert!(!c.tls);
        assert_eq!(c.port, 6667);
        assert_eq!(Config::new("h", "n").plaintext().port(7000).port, 7000);
    }

    #[test]
    fn debug_output_never_contains_a_password() {
        let mut c = Config::new("h", "alp").sasl_plain("alp", "sasl-secret-1");
        c.server_password = Some(Secret::new("server-secret-2"));
        let shown = format!("{c:?}");
        assert!(
            !shown.contains("sasl-secret-1"),
            "SASL password leaked: {shown}"
        );
        assert!(!shown.contains("server-secret-2"), "PASS leaked: {shown}");
        assert!(shown.contains("alp"));
    }

    #[test]
    fn sasl_external_sets_both_the_certificate_and_the_mechanism() {
        let cert = ClientCert::from_pem(
            format!(
                "{}\n{}",
                include_str!("../testdata/client.crt"),
                include_str!("../testdata/client.key")
            )
            .as_bytes(),
        )
        .unwrap();
        let c = Config::new("h", "alp").sasl_external(cert, "services-account");
        assert!(c.client_cert.is_some());
        assert_eq!(
            c.sasl,
            vec![Mechanism::External {
                authzid: "services-account".into()
            }]
        );
        // The private key never turns up in Config's own Debug output either.
        let shown = format!("{c:?}");
        for line in include_str!("../testdata/client.key")
            .lines()
            .filter(|l| !l.starts_with("-----"))
        {
            assert!(!shown.contains(line), "key material leaked: {shown}");
        }
    }

    #[test]
    fn autojoin_accumulates() {
        let c = Config::new("h", "n")
            .autojoin(["#a", "#b"])
            .autojoin(["#c"]);
        assert_eq!(c.autojoin, vec!["#a", "#b", "#c"]);
    }
}
