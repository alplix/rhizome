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
        self.send(Command::Join(channels.iter().map(|c| (*c).to_owned()).collect()))
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

fn tls_connector() -> TlsConnector {
    static CONNECTOR: OnceLock<TlsConnector> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
            let config = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("the ring provider supports the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            TlsConnector::from(Arc::new(config))
        })
        .clone()
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
    let tls = tls_connector().connect(name, tcp).await?;
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
        if !config.autojoin.iter().any(|c| c.eq_ignore_ascii_case(channel)) {
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
