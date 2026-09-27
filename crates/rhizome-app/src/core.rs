//! The application's brain, independent of any window.
//!
//! [`Core`] connects to networks, turns what the engine reports into the
//! interface's terms, records messages in the log, and answers the interface's
//! questions from it. It knows nothing about Tauri: it takes commands as method
//! calls and reports through a channel of [`Envelope`]s. That is what lets the
//! whole application be tested against a scripted server with no window, and
//! what will let a second front end reuse it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rhizome_client::{spawn, ChatMessage, Client, Config, Event, Handle, MessageKind};
use rhizome_store::{Kind, NewMessage, SearchOptions, SearchOrder};
use tokio::sync::mpsc;

use crate::dto::{event_lines, Envelope, UiBuffer, UiCursor, UiEvent, UiHit, UiMessage};
use crate::secrets::SecretStore;
use crate::storehost::StoreHost;

/// The quit message shown to others when we disconnect.
const QUIT_MESSAGE: &str = "Rhizome";

struct Running {
    handle: Handle,
    /// Distinguishes this connection from a later one under the same id, so a
    /// slow shutdown cannot remove its replacement.
    generation: u64,
}

struct Inner {
    store: StoreHost,
    /// Where remembered passwords live. Consulted here only to discard one that
    /// has just been shown not to work.
    secrets: Arc<dyn SecretStore>,
    out: mpsc::UnboundedSender<Envelope>,
    networks: Mutex<HashMap<String, Running>>,
    next_generation: AtomicU64,
}

/// Manages every network connection and the log.
#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Core")
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere while holding this lock must not take the whole
    // application down with it; the map is always left in a valid state.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The current time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn new_message(network: &str, m: &ChatMessage, received_ms: i64) -> NewMessage {
    NewMessage {
        network: network.to_owned(),
        buffer: m.buffer.clone(),
        sender: m.sender.clone(),
        kind: match m.kind {
            MessageKind::Privmsg => Kind::Privmsg,
            MessageKind::Notice => Kind::Notice,
            MessageKind::Action => Kind::Action,
        },
        text: m.text.clone(),
        server_time: m.time.clone(),
        received_ms,
        msgid: m.msgid.clone(),
        own: m.own,
        highlight: m.highlight,
    }
}

impl Core {
    /// Creates the core. Everything that happens is sent to the returned
    /// receiver, which the caller drains into the interface.
    pub fn new(
        store: StoreHost,
        secrets: Arc<dyn SecretStore>,
    ) -> (Core, mpsc::UnboundedReceiver<Envelope>) {
        let (out, events) = mpsc::unbounded_channel();
        let core = Core {
            inner: Arc::new(Inner {
                store,
                secrets,
                out,
                networks: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(1),
            }),
        };
        (core, events)
    }

    /// Reports something not tied to a network, such as a failure to write the
    /// log.
    pub fn report(&self, text: String) {
        let _ = self.inner.out.send(Envelope {
            network: String::new(),
            event: UiEvent::Error { code: 0, text },
        });
    }

    // ---- connections -----------------------------------------------------

    /// Starts connecting. Must be called from within a Tokio runtime.
    pub fn connect(&self, id: &str, config: Config) -> Result<(), String> {
        let mut networks = lock(&self.inner.networks);
        if networks.contains_key(id) {
            return Err(format!("{id} is already connected"));
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        // The engine's task handle is dropped: the connection is stopped by
        // asking it to quit, never by killing it, so the server hears goodbye.
        let Client { handle, events, .. } = spawn(config);
        networks.insert(id.to_owned(), Running { handle, generation });
        drop(networks);

        tokio::spawn(pump(self.inner.clone(), id.to_owned(), generation, events));
        Ok(())
    }

    /// Leaves a network. The connection may take a few seconds to finish
    /// delivering queued messages first; a later `connect` with the same id is
    /// allowed straight away.
    pub fn disconnect(&self, id: &str) -> Result<(), String> {
        let running = lock(&self.inner.networks)
            .remove(id)
            .ok_or_else(|| format!("{id} is not connected"))?;
        running
            .handle
            .quit(Some(QUIT_MESSAGE))
            .map_err(|e| e.to_string())
    }

    pub fn is_connected(&self, id: &str) -> bool {
        lock(&self.inner.networks).contains_key(id)
    }

    /// Every network currently connected (or connecting), for saying goodbye
    /// to each of them before the application quits.
    pub fn connected_ids(&self) -> Vec<String> {
        lock(&self.inner.networks).keys().cloned().collect()
    }

    fn handle(&self, id: &str) -> Result<Handle, String> {
        lock(&self.inner.networks)
            .get(id)
            .map(|r| r.handle.clone())
            .ok_or_else(|| format!("{id} is not connected"))
    }

    pub fn send_message(
        &self,
        id: &str,
        target: &str,
        text: &str,
        kind: MessageKind,
    ) -> Result<(), String> {
        let handle = self.handle(id)?;
        let result = match kind {
            MessageKind::Privmsg => handle.message(target, text),
            MessageKind::Notice => handle.notice(target, text),
            MessageKind::Action => handle.action(target, text),
        };
        result.map_err(|e| e.to_string())
    }

    pub fn join(&self, id: &str, channels: &[String]) -> Result<(), String> {
        let names: Vec<&str> = channels.iter().map(String::as_str).collect();
        self.handle(id)?.join(&names).map_err(|e| e.to_string())
    }

    pub fn part(&self, id: &str, channel: &str, reason: Option<&str>) -> Result<(), String> {
        self.handle(id)?
            .part(channel, reason)
            .map_err(|e| e.to_string())
    }

    pub fn set_nick(&self, id: &str, nick: &str) -> Result<(), String> {
        self.handle(id)?.nick(nick).map_err(|e| e.to_string())
    }

    /// Sends a line as typed. The engine still refuses anything that would not
    /// survive the wire format.
    pub fn raw(&self, id: &str, line: &str) -> Result<(), String> {
        self.handle(id)?.raw(line).map_err(|e| e.to_string())
    }

    // ---- the log ---------------------------------------------------------

    /// One page of a buffer's history, oldest first.
    pub async fn scrollback(
        &self,
        network: &str,
        buffer: &str,
        before: Option<UiCursor>,
        limit: usize,
    ) -> Result<Vec<UiMessage>, String> {
        let messages = self
            .inner
            .store
            .scrollback(
                network.to_owned(),
                buffer.to_owned(),
                before.map(Into::into),
                limit,
            )
            .await?;
        Ok(messages.iter().map(UiMessage::from_stored).collect())
    }

    pub async fn search(
        &self,
        query: &str,
        network: Option<&str>,
        newest_first: bool,
        limit: usize,
    ) -> Result<Vec<UiHit>, String> {
        let options = SearchOptions {
            network: network.map(str::to_owned),
            order: if newest_first {
                SearchOrder::Newest
            } else {
                SearchOrder::Relevance
            },
            limit,
            ..SearchOptions::default()
        };
        let hits = self.inner.store.search(query.to_owned(), options).await?;
        Ok(hits.iter().map(UiHit::from).collect())
    }

    pub async fn around(&self, id: i64, radius: usize) -> Result<Vec<UiMessage>, String> {
        let messages = self.inner.store.around(id, radius).await?;
        Ok(messages.iter().map(UiMessage::from_stored).collect())
    }

    /// The conversations with logged messages on a network, so private-message
    /// buffers reappear after a restart.
    pub async fn buffers(&self, network: &str) -> Result<Vec<UiBuffer>, String> {
        let buffers = self.inner.store.buffers(network.to_owned()).await?;
        Ok(buffers
            .into_iter()
            .map(|b| UiBuffer {
                name: b.name,
                messages: b.messages,
                last_time_ms: b.last_time_ms,
                unread: b.unread,
                highlights: b.highlights,
            })
            .collect())
    }

    /// Records that a conversation has been read up to `time_ms`, so its unread
    /// count survives a restart.
    pub fn mark_read(&self, network: &str, buffer: &str, time_ms: i64) {
        self.inner
            .store
            .mark_read(network.to_owned(), buffer.to_owned(), time_ms);
    }

    /// Deletes a conversation's history from the log. Returns how many lines
    /// were removed.
    pub async fn clear_history(&self, network: &str, buffer: &str) -> Result<usize, String> {
        self.inner
            .store
            .clear(network.to_owned(), buffer.to_owned())
            .await
    }

    // ---- remembered passwords --------------------------------------------

    /// The password remembered for a network, if any.
    pub fn saved_password(&self, id: &str) -> Result<Option<String>, String> {
        self.inner.secrets.get(id)
    }

    pub fn remember_password(&self, id: &str, password: &str) -> Result<(), String> {
        self.inner.secrets.set(id, password)
    }

    pub fn forget_password(&self, id: &str) -> Result<(), String> {
        self.inner.secrets.delete(id)
    }
}

/// Forwards one connection's events: to the log if they are chat or something
/// that happened in a conversation, and to the interface always.
async fn pump(inner: Arc<Inner>, id: String, generation: u64, mut events: mpsc::Receiver<Event>) {
    // Our own nick, for the history lines that are about us. The engine tells us
    // when we register and whenever we change it.
    let mut nick = String::new();

    'events: while let Some(event) = events.recv().await {
        let now = now_ms();
        let mut forgot_password = false;
        match &event {
            Event::Registered { nick: n } => nick.clone_from(n),
            Event::NickChanged { new, own: true, .. } => nick.clone_from(new),
            // A password that was just refused must not be tried again by the
            // next automatic connection: repeating a wrong password is how an
            // account gets locked.
            Event::AuthFailed(_) => {
                forgot_password = matches!(inner.secrets.get(&id), Ok(Some(_)));
                if let Err(e) = inner.secrets.delete(&id) {
                    let _ = inner.out.send(Envelope {
                        network: String::new(),
                        event: UiEvent::Error { code: 0, text: e },
                    });
                }
            }
            _ => {}
        }

        if let Event::Message(message) = &event {
            inner.store.log(new_message(&id, message, now));
        }

        let mut ui = UiEvent::from_event(&id, &event, now);
        if let UiEvent::AuthFailed {
            forgot_password: flag,
            ..
        } = &mut ui
        {
            *flag = forgot_password;
        }
        let envelope = Envelope {
            network: id.clone(),
            event: ui,
        };
        if inner.out.send(envelope).is_err() {
            // The window is gone; there is nobody to tell.
            break;
        }

        // Things that happened in a conversation are history too. They reach the
        // interface as ordinary lines, the same shape as the ones it reads back
        // from the log, so a line is never drawn twice in two forms.
        for line in event_lines(&event, &nick) {
            inner.store.log(line.to_new_message(&id, now));
            let envelope = Envelope {
                network: id.clone(),
                event: UiEvent::Message {
                    message: line.to_ui(&id, now),
                },
            };
            if inner.out.send(envelope).is_err() {
                break 'events;
            }
        }
    }

    {
        let mut networks = lock(&inner.networks);
        if networks.get(&id).map(|r| r.generation) == Some(generation) {
            networks.remove(&id);
        }
    }
    let _ = inner.out.send(Envelope {
        network: id,
        event: UiEvent::Closed,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::secrets::MemorySecrets;
    use rhizome_store::Store;
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
            let (socket, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
            let (r, w) = socket.into_split();
            Peer {
                reader: BufReader::new(r),
                writer: w,
            }
        }
        async fn line(&mut self) -> String {
            let mut line = String::new();
            let n = timeout(WAIT, self.reader.read_line(&mut line))
                .await
                .expect("timed out waiting for the client")
                .unwrap();
            assert!(n > 0, "client closed the connection");
            line.trim_end().to_owned()
        }
        async fn expect(&mut self, wanted: &str) {
            assert_eq!(self.line().await, wanted);
        }
        async fn send(&mut self, line: &str) {
            self.writer
                .write_all(format!("{line}\r\n").as_bytes())
                .await
                .unwrap();
        }
        async fn register(&mut self) {
            self.expect("CAP LS 302").await;
            self.expect("NICK alp").await;
            self.expect("USER alp 0 * :Rhizome").await;
            self.send(":srv CAP * LS :").await;
            self.expect("CAP END").await;
            self.send(":srv 001 alp :Welcome").await;
        }
    }

    fn core() -> (Core, mpsc::UnboundedReceiver<Envelope>) {
        core_with(Arc::new(MemorySecrets::default()))
    }

    fn core_with(secrets: Arc<dyn SecretStore>) -> (Core, mpsc::UnboundedReceiver<Envelope>) {
        let store = StoreHost::spawn(Store::open_in_memory().unwrap(), Box::new(|_| {}));
        Core::new(store, secrets)
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

    async fn next(rx: &mut mpsc::UnboundedReceiver<Envelope>) -> Envelope {
        timeout(WAIT, rx.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("event stream ended")
    }

    /// Reads envelopes until one matches, returning it.
    async fn until(
        rx: &mut mpsc::UnboundedReceiver<Envelope>,
        wanted: impl Fn(&UiEvent) -> bool,
    ) -> Envelope {
        loop {
            let envelope = next(rx).await;
            if wanted(&envelope.event) {
                return envelope;
            }
        }
    }

    /// Polls the log until `count` messages are in a buffer: writes are
    /// asynchronous, so a read straight after an event may run first.
    async fn wait_for_log(
        core: &Core,
        network: &str,
        buffer: &str,
        count: usize,
    ) -> Vec<UiMessage> {
        for _ in 0..100 {
            let page = core.scrollback(network, buffer, None, 100).await.unwrap();
            if page.len() >= count {
                return page;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the log never reached {count} messages in {buffer}");
    }

    #[tokio::test]
    async fn a_conversation_is_forwarded_logged_and_searchable() {
        let (core, mut rx) = core();
        let (listener, port) = listen().await;

        let server = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.register().await;
            peer.expect("JOIN #rhizome").await;
            peer.send(":alp!~a@h JOIN #rhizome").await;
            peer.send(":srv 353 alp = #rhizome :@alp bob").await;
            peer.send(":srv 366 alp #rhizome :End").await;
            peer.send(
                "@time=2026-09-25T10:00:00.000Z;msgid=m1 :bob!b@h PRIVMSG #rhizome \
                 :alp: attach the \u{2}backtrace\u{2} please",
            )
            .await;
            // Hold the connection open until told otherwise.
            peer.line().await
        });

        core.connect("test", config(port).autojoin(["#rhizome"]))
            .unwrap();

        until(&mut rx, |e| matches!(e, UiEvent::Registered { .. })).await;
        until(&mut rx, |e| matches!(e, UiEvent::Joined { .. })).await;
        let names = until(&mut rx, |e| matches!(e, UiEvent::Names { .. })).await;
        match names.event {
            UiEvent::Names { members, .. } => {
                assert_eq!(members[0].nick, "alp");
                assert_eq!(members[0].prefixes, "@");
            }
            _ => unreachable!(),
        }

        let message = until(&mut rx, |e| matches!(e, UiEvent::Message { .. })).await;
        assert_eq!(message.network, "test");
        let UiEvent::Message { message } = message.event else {
            unreachable!()
        };
        assert_eq!(message.buffer, "#rhizome");
        assert!(message.highlight, "it names us");
        assert_eq!(message.plain, "alp: attach the backtrace please");
        let bold: Vec<_> = message.spans.iter().filter(|s| s.bold).collect();
        assert_eq!(bold.len(), 1);
        assert_eq!(bold[0].text, "backtrace");

        // It reached the log, and can be found again.
        let logged = wait_for_log(&core, "test", "#rhizome", 1).await;
        assert_eq!(logged[0].msgid.as_deref(), Some("m1"));
        assert!(logged[0].id.is_some());

        let hits = core
            .search("backtrace from:bob", Some("test"), false, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0]
            .snippet
            .iter()
            .any(|p| p.hit && p.text.to_lowercase() == "backtrace"));

        // The context is the conversation around it, which includes our own join.
        let context = core.around(hits[0].message.id.unwrap(), 3).await.unwrap();
        assert!(context.iter().any(|m| m.plain.contains("backtrace")));
        assert!(context
            .iter()
            .any(|m| m.event.as_ref().is_some_and(|e| e.verb == "join")));

        assert_eq!(core.buffers("test").await.unwrap()[0].name, "#rhizome");

        core.disconnect("test").unwrap();
        assert!(server.await.unwrap().starts_with("QUIT"));
    }

    #[tokio::test]
    async fn a_message_sent_through_the_core_reaches_the_server_and_the_log() {
        let (core, mut rx) = core();
        let (listener, port) = listen().await;
        let server = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.register().await;
            let sent = peer.line().await;
            let quit = peer.line().await;
            (sent, quit)
        });

        core.connect("test", config(port)).unwrap();
        until(&mut rx, |e| matches!(e, UiEvent::Registered { .. })).await;

        core.send_message("test", "#c", "merhaba dünya", MessageKind::Privmsg)
            .unwrap();

        // No server echo here, so the engine reports it locally.
        let echoed = until(&mut rx, |e| matches!(e, UiEvent::Message { .. })).await;
        let UiEvent::Message { message } = echoed.event else {
            unreachable!()
        };
        assert!(message.own);
        assert_eq!(message.plain, "merhaba dünya");

        wait_for_log(&core, "test", "#c", 1).await;
        core.disconnect("test").unwrap();

        let (sent, quit) = server.await.unwrap();
        assert_eq!(sent, "PRIVMSG #c :merhaba dünya");
        assert!(quit.starts_with("QUIT"));
    }

    #[tokio::test]
    async fn disconnecting_ends_the_connection_and_allows_reconnecting() {
        let (core, mut rx) = core();
        let (listener, port) = listen().await;
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let mut peer = Peer::accept(&listener).await;
                peer.register().await;
                assert!(peer.line().await.starts_with("QUIT"));
            }
        });

        core.connect("test", config(port)).unwrap();
        assert!(core.is_connected("test"));
        until(&mut rx, |e| matches!(e, UiEvent::Registered { .. })).await;

        core.disconnect("test").unwrap();
        assert!(!core.is_connected("test"));
        until(&mut rx, |e| matches!(e, UiEvent::Closed)).await;

        // The same id can be used again, and the old connection's shutdown must
        // not have removed the new one.
        core.connect("test", config(port)).unwrap();
        until(&mut rx, |e| matches!(e, UiEvent::Registered { .. })).await;
        assert!(core.is_connected("test"));
        core.disconnect("test").unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connected_ids_lists_only_networks_currently_connected() {
        let (core, mut rx) = core();
        let (listener_a, port_a) = listen().await;
        let (listener_b, port_b) = listen().await;
        // Two independent servers, each waiting on its own connection: both
        // must be able to register before either is asked to quit.
        let server_a = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener_a).await;
            peer.register().await;
            assert!(peer.line().await.starts_with("QUIT"));
        });
        let server_b = tokio::spawn(async move {
            let mut peer = Peer::accept(&listener_b).await;
            peer.register().await;
            assert!(peer.line().await.starts_with("QUIT"));
        });

        assert_eq!(core.connected_ids(), Vec::<String>::new());
        core.connect("a", config(port_a)).unwrap();
        core.connect("b", config(port_b)).unwrap();
        until(&mut rx, |e| matches!(e, UiEvent::Registered { .. })).await;
        until(&mut rx, |e| matches!(e, UiEvent::Registered { .. })).await;
        let mut ids = core.connected_ids();
        ids.sort();
        assert_eq!(ids, vec!["a".to_owned(), "b".to_owned()]);

        core.disconnect("a").unwrap();
        assert_eq!(core.connected_ids(), vec!["b".to_owned()]);
        core.disconnect("b").unwrap();
        server_a.await.unwrap();
        server_b.await.unwrap();
    }

    #[tokio::test]
    async fn connecting_twice_under_one_id_is_refused() {
        let (core, _rx) = core();
        let (_listener, port) = listen().await;
        core.connect("test", config(port)).unwrap();
        let err = core.connect("test", config(port)).unwrap_err();
        assert!(err.contains("already connected"));
        core.disconnect("test").unwrap();
    }

    #[tokio::test]
    async fn commands_for_a_network_that_is_not_connected_are_errors() {
        let (core, _rx) = core();
        assert!(core
            .send_message("nope", "#c", "x", MessageKind::Privmsg)
            .unwrap_err()
            .contains("not connected"));
        assert!(core.join("nope", &["#c".into()]).is_err());
        assert!(core.part("nope", "#c", None).is_err());
        assert!(core.set_nick("nope", "x").is_err());
        assert!(core.raw("nope", "PING x").is_err());
        assert!(core.disconnect("nope").is_err());
    }

    #[tokio::test]
    async fn a_log_failure_can_be_reported_to_the_interface() {
        let (core, mut rx) = core();
        core.report("could not save 1 message(s) to the log: disk full".into());
        let envelope = next(&mut rx).await;
        assert_eq!(envelope.network, "");
        assert!(matches!(envelope.event, UiEvent::Error { code: 0, .. }));
    }

    #[tokio::test]
    async fn a_dead_server_is_reported_as_a_retryable_disconnect() {
        let (core, mut rx) = core();
        let port = {
            let (listener, port) = listen().await;
            drop(listener);
            port
        };
        core.connect("test", config(port)).unwrap();
        let envelope = until(&mut rx, |e| matches!(e, UiEvent::Disconnected { .. })).await;
        match envelope.event {
            UiEvent::Disconnected { retry_in_ms, .. } => assert!(retry_in_ms.is_some()),
            _ => unreachable!(),
        }
        core.disconnect("test").unwrap();
    }

    // ---- history lines, unread counts and clearing ------------------------------

    /// Runs a session in which other people join, talk, change nick, change the
    /// topic and leave, and returns the core once it has all been seen.
    async fn busy_channel() -> (Core, mpsc::UnboundedReceiver<Envelope>) {
        let (core, mut rx) = core();
        let (listener, port) = listen().await;
        tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.register().await;
            peer.expect("JOIN #c").await;
            peer.send(":alp!~a@h JOIN #c").await;
            peer.send(":srv 353 alp = #c :alp bob").await;
            peer.send(":srv 366 alp #c :End").await;
            peer.send(":carol!c@h JOIN #c").await;
            peer.send(
                "@time=2026-09-25T10:00:00.000Z;msgid=a1 :bob!b@h PRIVMSG #c :the needle is here",
            )
            .await;
            peer.send(":bob!b@h NICK robert").await;
            peer.send(":op!o@h TOPIC #c :new topic").await;
            peer.send(":op!o@h MODE #c +o carol").await;
            peer.send(":carol!c@h PART #c :later").await;
            peer.send(":robert!b@h QUIT :Ping timeout").await;
            // Hold the connection open.
            peer.line().await;
        });
        core.connect("t", config(port).autojoin(["#c"])).unwrap();
        // The quit is the last thing the server sends.
        until(&mut rx, |e| matches!(e, UiEvent::Message { message } if message.event.as_ref().is_some_and(|x| x.verb == "quit"))).await;
        (core, rx)
    }

    #[tokio::test]
    async fn things_that_happen_in_a_channel_become_history_lines_in_order() {
        let (core, _rx) = busy_channel().await;
        let logged = loop {
            let page = core.scrollback("t", "#c", None, 100).await.unwrap();
            if page.iter().filter(|m| m.event.is_some()).count() >= 7 {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let events: Vec<(String, String, Vec<String>, bool)> = logged
            .iter()
            .filter_map(|m| {
                m.event
                    .as_ref()
                    .map(|e| (e.verb.clone(), m.sender.clone(), e.args.clone(), m.own))
            })
            .collect();
        assert_eq!(
            events,
            vec![
                ("join".into(), "alp".into(), vec![], true),
                ("join".into(), "carol".into(), vec![], false),
                ("nick".into(), "bob".into(), vec!["robert".into()], false),
                ("topic".into(), "op".into(), vec!["new topic".into()], false),
                ("mode".into(), "op".into(), vec!["+o carol".into()], false),
                ("part".into(), "carol".into(), vec!["later".into()], false),
                (
                    "quit".into(),
                    "robert".into(),
                    vec!["Ping timeout".into()],
                    false
                ),
            ]
        );
        // The chat message is among them, as chat.
        assert!(logged
            .iter()
            .any(|m| m.event.is_none() && m.plain == "the needle is here"));
    }

    #[tokio::test]
    async fn history_lines_are_not_searchable_and_the_topic_on_joining_is_not_one() {
        let (core, _rx) = busy_channel().await;
        // Wait for the log to catch up.
        for _ in 0..100 {
            if core.scrollback("t", "#c", None, 100).await.unwrap().len() >= 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            core.search("needle", Some("t"), false, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            core.search("topic", Some("t"), false, 10)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            core.search("Ping timeout", Some("t"), false, 10)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            core.search("from:op", Some("t"), false, 10)
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn unread_counts_come_from_the_log_and_survive_being_read() {
        let (core, _rx) = busy_channel().await;
        let unread = loop {
            let b = core.buffers("t").await.unwrap();
            if b.first().is_some_and(|b| b.messages == 1) {
                break b;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!((unread[0].unread, unread[0].highlights), (1, 0));

        core.mark_read("t", "#c", 1_790_330_400_000);
        let after = core.buffers("t").await.unwrap();
        assert_eq!(
            after[0].unread, 0,
            "marking read is ordered after the writes it covers"
        );
    }

    #[tokio::test]
    async fn clearing_history_removes_a_conversation_from_the_log() {
        let (core, _rx) = busy_channel().await;
        for _ in 0..100 {
            if core.scrollback("t", "#c", None, 100).await.unwrap().len() >= 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(core.clear_history("t", "#c").await.unwrap() >= 8);
        assert!(core
            .scrollback("t", "#c", None, 100)
            .await
            .unwrap()
            .is_empty());
        assert!(core
            .search("needle", Some("t"), false, 10)
            .await
            .unwrap()
            .is_empty());
    }

    // ---- remembered passwords -----------------------------------------------------

    #[tokio::test]
    async fn a_rejected_login_discards_the_remembered_password_and_says_so() {
        let secrets: Arc<MemorySecrets> = Arc::new(MemorySecrets::default());
        let (core, mut rx) = core_with(secrets.clone());
        core.remember_password("t", "old-wrong-password").unwrap();
        assert_eq!(
            core.saved_password("t").unwrap().as_deref(),
            Some("old-wrong-password")
        );

        let (listener, port) = listen().await;
        tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.expect("CAP LS 302").await;
            peer.expect("NICK alp").await;
            peer.expect("USER alp 0 * :Rhizome").await;
            peer.send(":srv CAP * LS :sasl=PLAIN").await;
            let _ = peer.line().await;
        });
        // Plaintext, so the engine refuses SASL PLAIN and the login fails at once.
        core.connect("t", config(port).sasl_plain("alp", "old-wrong-password"))
            .unwrap();

        let failed = until(&mut rx, |e| matches!(e, UiEvent::AuthFailed { .. })).await;
        match failed.event {
            UiEvent::AuthFailed {
                forgot_password, ..
            } => assert!(forgot_password),
            _ => unreachable!(),
        }
        assert_eq!(
            core.saved_password("t").unwrap(),
            None,
            "it must not be retried"
        );
    }

    #[tokio::test]
    async fn a_failed_login_with_nothing_remembered_does_not_claim_to_have_forgotten_anything() {
        let (core, mut rx) = core();
        let (listener, port) = listen().await;
        tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            peer.expect("CAP LS 302").await;
            peer.expect("NICK alp").await;
            peer.expect("USER alp 0 * :Rhizome").await;
            peer.send(":srv CAP * LS :sasl=PLAIN").await;
            let _ = peer.line().await;
        });
        core.connect("t", config(port).sasl_plain("alp", "pw"))
            .unwrap();
        let failed = until(&mut rx, |e| matches!(e, UiEvent::AuthFailed { .. })).await;
        assert!(matches!(
            failed.event,
            UiEvent::AuthFailed {
                forgot_password: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn remembered_passwords_are_kept_per_network() {
        let (core, _rx) = core();
        core.remember_password("a", "one").unwrap();
        core.remember_password("b", "two").unwrap();
        core.forget_password("a").unwrap();
        assert_eq!(core.saved_password("a").unwrap(), None);
        assert_eq!(core.saved_password("b").unwrap().as_deref(), Some("two"));
    }
}
