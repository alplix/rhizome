//! End-to-end tests: the real driver over real TCP sockets against a scripted
//! fake server.
//!
//! These cover what the sans-IO session tests cannot: that the driver reads
//! and writes correctly, applies the rate limit, reconnects, and shuts down
//! cleanly.

use std::time::Duration;

use rhizome_client::{spawn, Client, Config, Event};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

struct Peer {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Peer {
    async fn accept(listener: &TcpListener) -> Peer {
        let (socket, _) = timeout(WAIT, listener.accept())
            .await
            .expect("client should connect")
            .unwrap();
        let (r, w) = socket.into_split();
        Peer {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// Reads one line from the client, without its line ending.
    async fn line(&mut self) -> String {
        let mut line = String::new();
        let n = timeout(WAIT, self.reader.read_line(&mut line))
            .await
            .expect("timed out waiting for a line from the client")
            .unwrap();
        assert!(n > 0, "client closed the connection unexpectedly");
        line.trim_end().to_owned()
    }

    async fn expect(&mut self, wanted: &str) {
        let got = self.line().await;
        assert_eq!(got, wanted);
    }

    async fn send(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    /// Everything the client sends until it closes the connection.
    async fn rest(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            match timeout(WAIT, self.reader.read_line(&mut line)).await {
                Ok(Ok(0)) | Err(_) | Ok(Err(_)) => return lines,
                Ok(Ok(_)) => lines.push(line.trim_end().to_owned()),
            }
        }
    }

    /// Runs the opening exchange for a server that offers no capabilities.
    async fn register(&mut self, nick: &str) {
        self.expect("CAP LS 302").await;
        self.expect(&format!("NICK {nick}")).await;
        self.expect(&format!("USER {nick} 0 * :Rhizome")).await;
        self.send(":srv CAP * LS :").await;
        self.expect("CAP END").await;
        self.send(&format!(":srv 001 {nick} :Welcome to the test network"))
            .await;
    }
}

async fn listen() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

fn config(port: u16) -> Config {
    let mut c = Config::new("127.0.0.1", "alp").plaintext().port(port);
    c.reconnect_min = Duration::from_millis(30);
    c.reconnect_max = Duration::from_millis(60);
    c
}

async fn next(client: &mut Client) -> Event {
    timeout(WAIT, client.events.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event stream ended")
}

#[tokio::test]
async fn registers_joins_and_exchanges_messages() {
    let (listener, port) = listen().await;

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.expect("CAP LS 302").await;
        peer.expect("NICK alp").await;
        peer.expect("USER alp 0 * :Rhizome").await;

        peer.send(":srv CAP * LS :server-time echo-message multi-prefix")
            .await;
        peer.expect("CAP REQ :server-time echo-message multi-prefix")
            .await;
        peer.send(":srv CAP alp ACK :server-time echo-message multi-prefix")
            .await;
        peer.expect("CAP END").await;

        peer.send(":srv 001 alp :Welcome to the test network alp")
            .await;
        peer.send(
            ":srv 005 alp PREFIX=(ov)@+ CHANTYPES=# CASEMAPPING=rfc1459 NETWORK=TestNet \
             :are supported by this server",
        )
        .await;
        peer.expect("JOIN #rhizome").await;

        let t = "@time=2026-09-25T10:00:00.000Z";
        peer.send(&format!("{t} :alp!~alp@host JOIN #rhizome")).await;
        peer.send(":srv 353 alp = #rhizome :@alp +bob carol").await;
        peer.send(":srv 366 alp #rhizome :End of /NAMES list.").await;
        peer.send(&format!("{t} :bob!b@h PRIVMSG #rhizome :merhaba alp"))
            .await;

        // The client answers; the server echoes it back, as echo-message says.
        peer.expect("PRIVMSG #rhizome :selam bob, nasılsın?").await;
        peer.send(&format!(
            "{t} :alp!~alp@host PRIVMSG #rhizome :selam bob, nasılsın?"
        ))
        .await;

        peer.expect("QUIT :görüşürüz").await;
    });

    let mut client = spawn(config(port).autojoin(["#rhizome"]));

    assert_eq!(next(&mut client).await, Event::Connecting);
    assert_eq!(next(&mut client).await, Event::Connected);
    assert_eq!(
        next(&mut client).await,
        Event::Registered { nick: "alp".into() }
    );
    assert_eq!(next(&mut client).await, Event::Network("TestNet".into()));
    assert_eq!(
        next(&mut client).await,
        Event::Joined {
            channel: "#rhizome".into()
        }
    );
    match next(&mut client).await {
        Event::Names { channel, members } => {
            assert_eq!(channel, "#rhizome");
            let names: Vec<_> = members
                .iter()
                .map(|m| format!("{}{}", m.prefixes, m.nick))
                .collect();
            assert_eq!(names, vec!["@alp", "+bob", "carol"]);
        }
        other => panic!("expected Names, got {other:?}"),
    }
    match next(&mut client).await {
        Event::Message(m) => {
            assert_eq!(m.sender, "bob");
            assert_eq!(m.buffer, "#rhizome");
            assert_eq!(m.text, "merhaba alp");
            assert_eq!(m.time.as_deref(), Some("2026-09-25T10:00:00.000Z"));
            assert!(m.highlight);
            assert!(!m.own);
        }
        other => panic!("expected Message, got {other:?}"),
    }

    // Non-ASCII text survives the whole round trip.
    client
        .handle
        .message("#rhizome", "selam bob, nasılsın?")
        .unwrap();
    match next(&mut client).await {
        Event::Message(m) => {
            assert!(m.own);
            assert_eq!(m.text, "selam bob, nasılsın?");
        }
        other => panic!("expected our own echoed Message, got {other:?}"),
    }

    client.handle.quit(Some("görüşürüz")).unwrap();
    timeout(WAIT, server).await.unwrap().unwrap();
    // A deliberate quit ends the task without a reconnect.
    timeout(WAIT, client.wait()).await.unwrap();
}

#[tokio::test]
async fn sasl_plain_is_refused_over_plaintext_and_the_password_never_leaves() {
    let (listener, port) = listen().await;

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.expect("CAP LS 302").await;
        peer.expect("NICK alp").await;
        peer.expect("USER alp 0 * :Rhizome").await;
        peer.send(":srv CAP * LS :sasl=PLAIN").await;
        peer.rest().await
    });

    let mut client = spawn(config(port).sasl_plain("alp", "hunter2-secret"));

    let mut failure = None;
    let mut disconnected = None;
    while disconnected.is_none() {
        match next(&mut client).await {
            Event::AuthFailed(reason) => failure = Some(reason),
            Event::Disconnected { retry_in, .. } => disconnected = Some(retry_in),
            _ => {}
        }
    }

    assert!(failure.expect("should report the failure").contains("TLS"));
    assert_eq!(
        disconnected,
        Some(None),
        "a failed login must not schedule a reconnect"
    );

    let received = timeout(WAIT, server).await.unwrap().unwrap();
    assert_eq!(received, vec!["QUIT :Authentication failed"]);
    let everything = received.join("\n");
    assert!(!everything.contains("hunter2"));
    assert!(!everything.contains("AUTHENTICATE"));
}

#[tokio::test]
async fn a_dropped_connection_reconnects_and_rejoins_channels_joined_meanwhile() {
    let (listener, port) = listen().await;

    let server = tokio::spawn(async move {
        // First connection: register, let the user join a channel, then drop.
        {
            let mut peer = Peer::accept(&listener).await;
            peer.register("alp").await;
            peer.expect("JOIN #extra").await;
            peer.send(":alp!~a@h JOIN #extra").await;
            peer.send(":srv 353 alp = #extra :alp").await;
            peer.send(":srv 366 alp #extra :End").await;
            // Give the client a moment to process the join before the drop.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Second connection: the channel must be rejoined without being asked.
        let mut peer = Peer::accept(&listener).await;
        peer.register("alp").await;
        peer.expect("JOIN #extra").await;
        peer.send(":alp!~a@h JOIN #extra").await;
        peer.expect("QUIT").await;
    });

    // No autojoin: #extra is only known because the user joined it.
    let mut client = spawn(config(port));

    assert_eq!(next(&mut client).await, Event::Connecting);
    assert_eq!(next(&mut client).await, Event::Connected);
    assert!(matches!(next(&mut client).await, Event::Registered { .. }));
    client.handle.join(&["#extra"]).unwrap();
    assert!(matches!(next(&mut client).await, Event::Joined { .. }));
    assert!(matches!(next(&mut client).await, Event::Names { .. }));

    match next(&mut client).await {
        Event::Disconnected { retry_in, .. } => {
            assert!(retry_in.is_some(), "a dropped connection should be retried")
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    assert_eq!(next(&mut client).await, Event::Connecting);
    assert_eq!(next(&mut client).await, Event::Connected);
    assert!(matches!(next(&mut client).await, Event::Registered { .. }));
    assert_eq!(
        next(&mut client).await,
        Event::Joined {
            channel: "#extra".into()
        }
    );

    client.handle.quit(None).unwrap();
    timeout(WAIT, server).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_pasted_line_break_cannot_inject_a_command_over_the_real_socket() {
    let (listener, port) = listen().await;

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.register("alp").await;
        // Whatever the paste contained, each of these arrives as chat.
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(peer.line().await);
        }
        peer.expect("QUIT").await;
        got
    });

    let mut client = spawn(config(port));
    while !matches!(next(&mut client).await, Event::Registered { .. }) {}

    client
        .handle
        .message("#c", "hello\r\nQUIT :pwned\nJOIN #evil")
        .unwrap();
    client.handle.quit(None).unwrap();

    let got = timeout(WAIT, server).await.unwrap().unwrap();
    assert_eq!(
        got,
        vec![
            "PRIVMSG #c :hello",
            "PRIVMSG #c :QUIT :pwned",
            "PRIVMSG #c :JOIN #evil",
        ]
    );
}

#[tokio::test]
async fn ping_is_answered_even_while_a_paste_is_queued_behind_the_rate_limit() {
    let (listener, port) = listen().await;

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.register("alp").await;

        // The first line of the paste goes out at once (it uses the burst) and
        // the other nineteen are now queued, at one per second.
        peer.expect("PRIVMSG #c :line 0").await;

        // Ask a question while that queue is backed up.
        peer.send("PING :are-you-there").await;

        // The PONG must overtake the queue. If it waited its turn it would
        // arrive behind up to nineteen seconds of paste, and a real server
        // would have dropped us for ping timeout by then.
        let mut before_pong = Vec::new();
        loop {
            let line = peer.line().await;
            if line == "PONG are-you-there" {
                return before_pong;
            }
            before_pong.push(line);
        }
    });

    let mut cfg = config(port);
    cfg.rate_burst = 1;
    cfg.rate_per_second = 1.0;
    let mut client = spawn(cfg);
    while !matches!(next(&mut client).await, Event::Registered { .. }) {}

    let paste = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("
");
    client.handle.message("#c", &paste).unwrap();

    let before_pong = timeout(WAIT, server).await.unwrap().unwrap();
    assert!(
        before_pong.len() <= 1,
        "the PONG was stuck behind {} queued lines: {before_pong:?}",
        before_pong.len()
    );
    client.abort();
}

#[tokio::test]
async fn a_quit_delivers_the_messages_sent_just_before_it() {
    let (listener, port) = listen().await;

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.register("alp").await;
        peer.rest().await
    });

    let mut client = spawn(config(port));
    while !matches!(next(&mut client).await, Event::Registered { .. }) {}

    client.handle.message("#c", "last words").unwrap();
    client.handle.quit(Some("bye")).unwrap();

    let received = timeout(WAIT, server).await.unwrap().unwrap();
    assert_eq!(received, vec!["PRIVMSG #c :last words", "QUIT :bye"]);
}

#[tokio::test]
async fn nothing_listening_is_reported_and_retried_not_fatal() {
    // Bind to learn a free port, then close it so nothing is listening there.
    let port = {
        let (listener, port) = listen().await;
        drop(listener);
        port
    };
    let mut client = spawn(config(port));

    assert_eq!(next(&mut client).await, Event::Connecting);
    match next(&mut client).await {
        Event::Disconnected { reason, retry_in } => {
            assert!(reason.contains("could not connect"), "{reason}");
            assert!(retry_in.is_some());
        }
        other => panic!("expected Disconnected, got {other:?}"),
    }
    // And it keeps trying.
    assert_eq!(next(&mut client).await, Event::Connecting);

    client.handle.quit(None).unwrap();
    timeout(WAIT, client.wait()).await.unwrap();
}

#[tokio::test]
async fn dropping_the_event_receiver_stops_the_task() {
    let (listener, port) = listen().await;
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.register("alp").await;
        peer.rest().await
    });

    let mut client = spawn(config(port));
    while !matches!(next(&mut client).await, Event::Registered { .. }) {}

    let Client { handle, events, .. } = client;
    drop(events);
    // The next thing that produces an event notices nobody is listening.
    handle.join(&["#a"]).unwrap();
    handle.message("#a", "hello").ok();

    let received = timeout(WAIT, server).await.unwrap().unwrap();
    // The server sees the connection end; it must not hang forever.
    assert!(received.len() <= 3);
}
