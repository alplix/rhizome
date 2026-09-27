//! The network driver: sockets, TLS, timing and reconnects.
//!
//! This is the thin layer around [`Session`]. It reads bytes, cuts them into
//! lines, hands each line to the session, and writes what the session answers
//! — through the rate limiter, and after checking every line is safe to send.
//! All protocol decisions live in the session; nothing here knows what a
//! `PRIVMSG` means.
//!
//! [`spawn`] starts the driver as a background task and returns a [`Client`]:
//! a [`Handle`] for sending commands and a channel of [`Event`]s to render.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rhizome_proto::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use crate::backoff::Backoff;
use crate::codec::LineCodec;
use crate::config::Config;
use crate::event::{Event, MessageKind};
use crate::ratelimit::{self, TokenBucket};
use crate::session::{Output, Session};

/// How long to wait for the TCP and TLS handshakes together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the connection is checked for silence.
const KEEPALIVE_TICK: Duration = Duration::from_secs(15);
/// Silence after which we ping the server to see if it is still there.
const PING_AFTER: Duration = Duration::from_secs(60);
/// Silence after which the connection is declared dead.
const DEAD_AFTER: Duration = Duration::from_secs(180);
/// How long a quit waits for queued messages to go out first.
const QUIT_DRAIN: Duration = Duration::from_secs(5);

/// An instruction for the running connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Send {
        target: String,
        text: String,
        kind: MessageKind,
    },
    Join(Vec<String>),
    Part {
        channel: String,
        reason: Option<String>,
    },
    Nick(String),
    /// A CTCP request of our own, such as a `DCC SEND` offer. Never echoed
    /// locally as a chat message.
    Ctcp {
        target: String,
        command: String,
        params: Option<String>,
    },
    /// A line to send as typed.
    Raw(String),
    /// Leave the network and stop, without reconnecting.
    Quit(Option<String>),
}

/// The connection task has ended and can no longer take commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the connection task has ended")
    }
}

impl std::error::Error for Closed {}

/// A cheap, cloneable way to send commands to the connection.
#[derive(Debug, Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    pub fn send(&self, command: Command) -> Result<(), Closed> {
        self.tx.send(command).map_err(|_| Closed)
    }

    /// Sends an ordinary message.
    pub fn message(&self, target: &str, text: &str) -> Result<(), Closed> {
        self.send(Command::Send {
            target: target.to_owned(),
            text: text.to_owned(),
            kind: MessageKind::Privmsg,
        })
    }

    /// Sends a `/me` action.
    pub fn action(&self, target: &str, text: &str) -> Result<(), Closed> {
        self.send(Command::Send {
            target: target.to_owned(),
            text: text.to_owned(),
            kind: MessageKind::Action,
        })
    }

    pub fn notice(&self, target: &str, text: &str) -> Result<(), Closed> {
        self.send(Command::Send {
            target: target.to_owned(),
            text: text.to_owned(),
            kind: MessageKind::Notice,
        })
    }

    pub fn join(&self, channels: &[&str]) -> Result<(), Closed> {
        self.send(Command::Join(
            channels.iter().map(|c| (*c).to_owned()).collect(),
        ))
    }

    pub fn part(&self, channel: &str, reason: Option<&str>) -> Result<(), Closed> {
        self.send(Command::Part {
            channel: channel.to_owned(),
            reason: reason.map(str::to_owned),
        })
    }

    pub fn nick(&self, nick: &str) -> Result<(), Closed> {
        self.send(Command::Nick(nick.to_owned()))
    }

    pub fn raw(&self, line: &str) -> Result<(), Closed> {
        self.send(Command::Raw(line.to_owned()))
    }

    /// Sends a CTCP request of our own, such as a `DCC SEND` offer.
    pub fn ctcp(&self, target: &str, command: &str, params: Option<&str>) -> Result<(), Closed> {
        self.send(Command::Ctcp {
            target: target.to_owned(),
            command: command.to_owned(),
            params: params.map(str::to_owned),
        })
    }

    pub fn quit(&self, reason: Option<&str>) -> Result<(), Closed> {
        self.send(Command::Quit(reason.map(str::to_owned)))
    }
}

/// A running connection.
#[derive(Debug)]
pub struct Client {
    /// Send commands here.
    pub handle: Handle,
    /// Everything that happens, in order. When this yields `None` the
    /// connection task has finished.
    pub events: mpsc::Receiver<Event>,
    task: JoinHandle<()>,
}

impl Client {
    /// Waits for the connection task to finish.
    pub async fn wait(self) {
        let _ = self.task.await;
    }

    /// Stops the connection task immediately, without saying goodbye to the
    /// server. Prefer [`Handle::quit`].
    pub fn abort(&self) {
        self.task.abort();
    }
}

/// Starts connecting in the background. Must be called from within a Tokio
/// runtime.
///
/// The connection is re-established automatically after a drop, with
/// exponential backoff, and previously joined channels are rejoined. It stops
/// for good on [`Handle::quit`], on a failed login, or when the returned
/// `Client`'s event receiver is dropped.
pub fn spawn(config: Config) -> Client {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    // Bounded, so a UI that stops draining events pushes back on the socket
    // instead of letting memory grow.
    let (event_tx, event_rx) = mpsc::channel(1024);
    let task = tokio::spawn(run(config, command_rx, event_tx));
    Client {
        handle: Handle { tx: command_tx },
        events: event_rx,
        task,
    }
}

// ---- transport -----------------------------------------------------------

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}
type BoxedStream = Box<dyn AsyncStream>;

fn root_store() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

/// The ordinary connector, with no client certificate: built once and
/// shared, since it is the same for every connection that does not need
/// SASL `EXTERNAL`.
fn tls_connector() -> TlsConnector {
    static CONNECTOR: OnceLock<TlsConnector> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("the ring provider supports the default protocol versions")
                .with_root_certificates(root_store())
                .with_no_client_auth();
            TlsConnector::from(Arc::new(config))
        })
        .clone()
}

/// A connector presenting `cert` during the handshake, for SASL `EXTERNAL`.
/// Built fresh each time: unlike the ordinary connector, this one varies per
/// network, so nothing here is worth caching across connections.
fn tls_connector_with_cert(cert: &crate::identity::ClientCert) -> io::Result<TlsConnector> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions")
        .with_root_certificates(root_store())
        .with_client_auth_cert(cert.chain.clone(), cert.key.clone_key())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn connect(config: &Config) -> io::Result<BoxedStream> {
    let tcp = TcpStream::connect((config.host.as_str(), config.port)).await?;
    // Chat lines are tiny and latency matters more than packing.
    tcp.set_nodelay(true)?;
    if !config.tls {
        return Ok(Box::new(tcp));
    }
    let name = ServerName::try_from(config.host.clone())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let connector = match &config.client_cert {
        Some(cert) => tls_connector_with_cert(cert)?,
        None => tls_connector(),
    };
    let tls = connector.connect(name, tcp).await?;
    Ok(Box::new(tls))
}

// ---- the write side ------------------------------------------------------

/// The socket's write half plus the queue that rate-limits it.
struct Link {
    writer: WriteHalf<BoxedStream>,
    queue: VecDeque<Message>,
    bucket: TokenBucket,
}

impl Link {
    /// Sends a message now if it is exempt from rate limiting, otherwise
    /// queues it behind whatever is already waiting.
    async fn push(&mut self, message: Message) -> io::Result<()> {
        if ratelimit::is_exempt(&message) {
            self.write(&message).await
        } else {
            self.queue.push_back(message);
            Ok(())
        }
    }

    async fn write(&mut self, message: &Message) -> io::Result<()> {
        self.writer
            .write_all(message.to_wire_line().as_bytes())
            .await?;
        // Without this, TLS may hold a short line in its buffer.
        self.writer.flush().await
    }

    /// Writes as many queued messages as the bucket allows right now.
    async fn flush_ready(&mut self) -> io::Result<()> {
        while !self.queue.is_empty() && self.bucket.try_take(Instant::now()) {
            if let Some(message) = self.queue.pop_front() {
                self.write(&message).await?;
            }
        }
        Ok(())
    }

    /// How long until the next queued message may go, or `None` if nothing is
    /// waiting.
    fn next_delay(&mut self) -> Option<Duration> {
        if self.queue.is_empty() {
            None
        } else {
            Some(self.bucket.wait_time(Instant::now()))
        }
    }

    /// Sends everything queued, waiting out the rate limit, but gives up after
    /// `limit` so a quit cannot hang on a huge backlog.
    async fn drain(&mut self, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            if self.flush_ready().await.is_err() || self.queue.is_empty() {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let wait = self.bucket.wait_time(now).min(deadline - now);
            time::sleep(wait).await;
        }
    }
}

// ---- one connection ------------------------------------------------------

enum Outcome {
    /// The user asked to stop, or nobody is listening any more.
    Stopped,
    /// Something went wrong that retrying will not fix.
    Fatal(String),
    /// The connection ended; try again.
    Lost {
        reason: String,
        /// Whether it got as far as registering, which resets the backoff.
        registered: bool,
        /// The channels to rejoin.
        joined: Vec<String>,
    },
}

/// Why a step stopped the connection.
enum Stop {
    Io(io::Error),
    /// The event receiver was dropped.
    ConsumerGone,
}

/// Sends what the session produced and reports its events.
async fn dispatch(
    output: Output,
    link: &mut Link,
    events: &mpsc::Sender<Event>,
) -> Result<(), Stop> {
    for message in output.send {
        // The last line of defence: nothing reaches the wire unchecked, so
        // text that slipped past every earlier layer still cannot inject a
        // command. Only the command name is reported, as the line may carry a
        // password.
        match message.validate_for_send() {
            Ok(()) => link.push(message).await.map_err(Stop::Io)?,
            Err(reason) => {
                let event = Event::Error {
                    code: 0,
                    text: format!("refused to send {}: {reason}", message.command),
                };
                events.send(event).await.map_err(|_| Stop::ConsumerGone)?;
            }
        }
    }
    for event in output.events {
        events.send(event).await.map_err(|_| Stop::ConsumerGone)?;
    }
    Ok(())
}

fn lost(session: &Session, joined_before: &[String], reason: impl Into<String>) -> Outcome {
    Outcome::Lost {
        reason: reason.into(),
        registered: session.is_registered(),
        // A connection that never registered knows nothing new; keep what we
        // were told to rejoin last time.
        joined: if session.is_registered() {
            session.channel_names()
        } else {
            joined_before.to_vec()
        },
    }
}

fn after_stop(stop: Stop, session: &Session, joined_before: &[String]) -> Outcome {
    match stop {
        Stop::Io(e) => lost(session, joined_before, format!("write failed: {e}")),
        Stop::ConsumerGone => Outcome::Stopped,
    }
}

fn apply(session: &mut Session, command: Command) -> Output {
    match command {
        Command::Send { target, text, kind } => session.send_message(&target, &text, kind),
        Command::Join(channels) => session.join(&channels),
        Command::Part { channel, reason } => session.part(&channel, reason.as_deref()),
        Command::Nick(nick) => session.change_nick(&nick),
        Command::Ctcp {
            target,
            command,
            params,
        } => session.send_ctcp(&target, &command, params.as_deref()),
        Command::Raw(line) => session.raw(&line),
        // Handled by the caller, which has to stop the loop.
        Command::Quit(_) => Output::default(),
    }
}

async fn run_once(
    config: &Config,
    joined: &[String],
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::Sender<Event>,
) -> Outcome {
    let mut config = config.clone();
    for channel in joined {
        if !config
            .autojoin
            .iter()
            .any(|c| c.eq_ignore_ascii_case(channel))
        {
            config.autojoin.push(channel.clone());
        }
    }

    if events.send(Event::Connecting).await.is_err() {
        return Outcome::Stopped;
    }
    let stream = match time::timeout(CONNECT_TIMEOUT, connect(&config)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            return Outcome::Lost {
                reason: format!("could not connect: {e}"),
                registered: false,
                joined: joined.to_vec(),
            }
        }
        Err(_) => {
            return Outcome::Lost {
                reason: "connection attempt timed out".to_owned(),
                registered: false,
                joined: joined.to_vec(),
            }
        }
    };
    if events.send(Event::Connected).await.is_err() {
        return Outcome::Stopped;
    }

    let (mut reader, writer) = tokio::io::split(stream);
    let mut link = Link {
        writer,
        queue: VecDeque::new(),
        bucket: TokenBucket::new(config.rate_burst, config.rate_per_second, Instant::now()),
    };
    let mut session = Session::new(config);
    let mut codec = LineCodec::new();
    let mut buffer = vec![0u8; 8192];
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let mut keepalive = time::interval(KEEPALIVE_TICK);
    keepalive.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

    let opening = session.start();
    if let Err(stop) = dispatch(opening, &mut link, events).await {
        return after_stop(stop, &session, joined);
    }

    loop {
        let delay = link.next_delay();
        tokio::select! {
            read = reader.read(&mut buffer) => {
                let n = match read {
                    Ok(0) => return lost(&session, joined, "connection closed by the server"),
                    Ok(n) => n,
                    Err(e) => return lost(&session, joined, format!("read failed: {e}")),
                };
                last_rx = Instant::now();
                codec.push(&buffer[..n]);
                while let Some(line) = codec.next_line() {
                    // A line we cannot parse is skipped, not fatal: one odd
                    // message should not cost the whole connection.
                    let Ok(message) = Message::parse(&line) else { continue };
                    let output = session.handle(&message);
                    if let Err(stop) = dispatch(output, &mut link, events).await {
                        return after_stop(stop, &session, joined);
                    }
                }
            }

            command = commands.recv() => {
                match command {
                    // Every handle was dropped: nobody can give orders any
                    // more, so leave cleanly.
                    None => {
                        say_goodbye(&mut session, &mut link, None).await;
                        return Outcome::Stopped;
                    }
                    Some(Command::Quit(reason)) => {
                        say_goodbye(&mut session, &mut link, reason.as_deref()).await;
                        return Outcome::Stopped;
                    }
                    Some(command) => {
                        let output = apply(&mut session, command);
                        if let Err(stop) = dispatch(output, &mut link, events).await {
                            return after_stop(stop, &session, joined);
                        }
                    }
                }
            }

            () = async {
                match delay {
                    Some(d) => time::sleep(d).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Err(e) = link.flush_ready().await {
                    return lost(&session, joined, format!("write failed: {e}"));
                }
            }

            _ = keepalive.tick() => {
                let silent_for = last_rx.elapsed();
                if silent_for > DEAD_AFTER {
                    return lost(&session, joined, "ping timeout");
                }
                if silent_for > PING_AFTER && last_ping.elapsed() > PING_AFTER {
                    last_ping = Instant::now();
                    let ping = Message::new("PING", ["rhizome"]);
                    if let Err(e) = link.push(ping).await {
                        return lost(&session, joined, format!("write failed: {e}"));
                    }
                }
            }
        }

        if session.is_closed() {
            // Let the final line (a QUIT after a failed login) go out.
            link.drain(QUIT_DRAIN).await;
            return match session.failure() {
                Some(reason) => Outcome::Fatal(reason.to_owned()),
                None => {
                    let reason = session
                        .server_closed()
                        .map_or_else(|| "connection ended".to_owned(), str::to_owned);
                    lost(&session, joined, reason)
                }
            };
        }
    }
}

/// Sends `QUIT` after everything already queued, then closes the socket.
async fn say_goodbye(session: &mut Session, link: &mut Link, reason: Option<&str>) {
    for message in session.quit(reason).send {
        link.queue.push_back(message);
    }
    link.drain(QUIT_DRAIN).await;
    let _ = link.writer.shutdown().await;
}

// ---- the outer loop ------------------------------------------------------

async fn run(
    config: Config,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::Sender<Event>,
) {
    let mut backoff = Backoff::new(config.reconnect_min, config.reconnect_max);
    let mut joined: Vec<String> = Vec::new();

    loop {
        match run_once(&config, &joined, &mut commands, &events).await {
            Outcome::Stopped => return,
            Outcome::Fatal(reason) => {
                let _ = events
                    .send(Event::Disconnected {
                        reason,
                        retry_in: None,
                    })
                    .await;
                return;
            }
            Outcome::Lost {
                reason,
                registered,
                joined: rejoin,
            } => {
                if registered {
                    backoff.reset();
                }
                joined = rejoin;
                let delay = backoff.next_delay();
                let gone = events
                    .send(Event::Disconnected {
                        reason,
                        retry_in: Some(delay),
                    })
                    .await
                    .is_err();
                if gone {
                    return;
                }

                // Wait out the delay, but stay responsive to a quit: someone
                // who closes the app during a reconnect wait should not have
                // to wait for it.
                let pause = time::sleep(delay);
                tokio::pin!(pause);
                loop {
                    tokio::select! {
                        () = &mut pause => break,
                        command = commands.recv() => match command {
                            None | Some(Command::Quit(_)) => return,
                            // Other commands have nowhere to go while
                            // disconnected.
                            Some(_) => {}
                        },
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tls_client_cert_tests {
    //! Whether a configured client certificate is actually presented during
    //! a real TLS handshake, and whether a server that demands one — and
    //! does not trust it — actually refuses the connection. Everything else
    //! about SASL `EXTERNAL` (choosing it, the `AUTHENTICATE` exchange
    //! itself) is sans-I/O and tested from transcripts in `session.rs`; this
    //! is the one part that needs a real socket and a real TLS stack on
    //! both ends, which is why it lives here rather than in
    //! `tests/connection.rs`: it needs `ClientCert`'s fields to build a test
    //! `ClientConfig` that trusts a private test CA, not the public roots
    //! `tls_connector_with_cert` uses for real networks.

    use std::io;
    use std::sync::Arc;

    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use tokio_rustls::rustls::server::WebPkiClientVerifier;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    use crate::identity::ClientCert;

    const CA_CERT: &str = include_str!("../testdata/ca.crt");
    const SERVER_CERT: &str = include_str!("../testdata/server.crt");
    const SERVER_KEY: &str = include_str!("../testdata/server.key");
    const CLIENT_CERT: &str = include_str!("../testdata/client.crt");
    const CLIENT_KEY: &str = include_str!("../testdata/client.key");
    const OTHER_CERT: &str = include_str!("../testdata/other.crt");
    const OTHER_KEY: &str = include_str!("../testdata/other.key");

    fn certs(pem: &str) -> Vec<CertificateDer<'static>> {
        rustls_pemfile::certs(&mut io::Cursor::new(pem.as_bytes()))
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn key(pem: &str) -> PrivateKeyDer<'static> {
        rustls_pemfile::private_key(&mut io::Cursor::new(pem.as_bytes()))
            .unwrap()
            .unwrap()
    }

    fn test_ca_roots() -> Arc<RootCertStore> {
        let mut roots = RootCertStore::empty();
        roots.add(certs(CA_CERT).remove(0)).unwrap();
        Arc::new(roots)
    }

    /// A server config trusting our test CA for the client certificate it
    /// demands, presenting its own certificate (also signed by that CA).
    fn server_config() -> ServerConfig {
        let verifier = WebPkiClientVerifier::builder(test_ca_roots())
            .build()
            .unwrap();
        ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs(SERVER_CERT), key(SERVER_KEY))
            .unwrap()
    }

    /// A client config that trusts our test CA for the *server's*
    /// certificate too — otherwise every scenario below would fail during
    /// the server's half of the handshake, before the client certificate
    /// this file is actually testing is ever presented.
    fn client_config(cert: Option<&ClientCert>) -> ClientConfig {
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates((*test_ca_roots()).clone());
        match cert {
            Some(cert) => builder
                .with_client_auth_cert(cert.chain.clone(), cert.key.clone_key())
                .unwrap(),
            None => builder.with_no_client_auth(),
        }
    }

    /// Runs one handshake attempt in each direction concurrently and returns
    /// whether each side thought it succeeded.
    async fn attempt(client_cert: Option<&ClientCert>) -> (io::Result<()>, io::Result<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = TlsAcceptor::from(Arc::new(server_config()));
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            acceptor.accept(tcp).await.map(|_| ())
        });

        let connector = TlsConnector::from(Arc::new(client_config(client_cert)));
        let name = ServerName::try_from("localhost").unwrap();
        let client_result = async {
            let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
            connector.connect(name, tcp).await.map(|_| ())
        }
        .await;

        let server_result = server.await.unwrap();
        (client_result, server_result)
    }

    #[tokio::test]
    async fn tls_connector_with_cert_builds_successfully_for_a_valid_certificate() {
        let cert = ClientCert::from_pem(format!("{CLIENT_CERT}\n{CLIENT_KEY}").as_bytes()).unwrap();
        assert!(super::tls_connector_with_cert(&cert).is_ok());
    }

    #[tokio::test]
    async fn a_client_certificate_lets_a_real_mutual_tls_handshake_succeed() {
        let cert = ClientCert::from_pem(format!("{CLIENT_CERT}\n{CLIENT_KEY}").as_bytes()).unwrap();
        let (client_result, server_result) = attempt(Some(&cert)).await;
        assert!(client_result.is_ok(), "{client_result:?}");
        assert!(server_result.is_ok(), "{server_result:?}");
    }

    #[tokio::test]
    async fn a_certificate_from_an_untrusted_ca_is_refused_by_the_server() {
        let cert = ClientCert::from_pem(format!("{OTHER_CERT}\n{OTHER_KEY}").as_bytes()).unwrap();
        let (_, server_result) = attempt(Some(&cert)).await;
        assert!(server_result.is_err(), "the server must not accept it");
    }

    #[tokio::test]
    async fn no_client_certificate_is_refused_by_a_server_that_requires_one() {
        let (_, server_result) = attempt(None).await;
        assert!(server_result.is_err());
    }
}
