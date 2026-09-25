//! Running the store on a thread of its own.
//!
//! SQLite calls block, and a `Store` is not `Sync`, so it cannot be shared
//! between async tasks. Instead it is moved onto a dedicated thread and every
//! operation is a message to it, answered over a one-shot channel. Writes are
//! fire-and-forget and are batched: when a burst of messages arrives (a
//! reconnect replaying history, a busy channel), everything queued is written in
//! one transaction rather than one commit per line.

use std::sync::mpsc;
use std::thread;

use rhizome_store::{
    BufferInfo, Cursor, NewMessage, SearchHit, SearchOptions, Store, StoredMessage,
};
use tokio::sync::oneshot;

type Reply<T> = oneshot::Sender<Result<T, String>>;

enum Request {
    Log(NewMessage),
    Search {
        query: String,
        options: SearchOptions,
        reply: Reply<Vec<SearchHit>>,
    },
    Scrollback {
        network: String,
        buffer: String,
        before: Option<Cursor>,
        limit: usize,
        reply: Reply<Vec<StoredMessage>>,
    },
    Around {
        id: i64,
        radius: usize,
        reply: Reply<Vec<StoredMessage>>,
    },
    Buffers {
        network: String,
        reply: Reply<Vec<BufferInfo>>,
    },
}

/// A handle to the store thread. Cloning is cheap; the thread exits when the
/// last clone is dropped.
#[derive(Clone)]
pub struct StoreHost {
    tx: mpsc::Sender<Request>,
}

impl std::fmt::Debug for StoreHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreHost")
    }
}

/// Called (on the store thread) when a write fails, so the failure can reach
/// the person instead of vanishing. A log that silently stops recording is the
/// kind of failure people only discover when they go looking for something.
pub type ErrorSink = Box<dyn Fn(String) + Send + 'static>;

impl StoreHost {
    pub fn spawn(store: Store, on_error: ErrorSink) -> StoreHost {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("rhizome-store".into())
            .spawn(move || run(store, rx, on_error))
            .expect("the operating system should be able to start a thread");
        StoreHost { tx }
    }

    /// Queues a message to be recorded. Never blocks.
    pub fn log(&self, message: NewMessage) {
        // If the thread has gone, there is nowhere to record to; the error sink
        // already reported whatever ended it.
        let _ = self.tx.send(Request::Log(message));
    }

    async fn ask<T>(&self, build: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(build(reply))
            .map_err(|_| "the message log is not running".to_owned())?;
        answer
            .await
            .map_err(|_| "the message log stopped before answering".to_owned())?
    }

    pub async fn search(
        &self,
        query: String,
        options: SearchOptions,
    ) -> Result<Vec<SearchHit>, String> {
        self.ask(|reply| Request::Search {
            query,
            options,
            reply,
        })
        .await
    }

    pub async fn scrollback(
        &self,
        network: String,
        buffer: String,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, String> {
        self.ask(|reply| Request::Scrollback {
            network,
            buffer,
            before,
            limit,
            reply,
        })
        .await
    }

    pub async fn around(&self, id: i64, radius: usize) -> Result<Vec<StoredMessage>, String> {
        self.ask(|reply| Request::Around { id, radius, reply }).await
    }

    pub async fn buffers(&self, network: String) -> Result<Vec<BufferInfo>, String> {
        self.ask(|reply| Request::Buffers { network, reply }).await
    }
}

fn answer<T>(reply: Reply<T>, result: rhizome_store::Result<T>) {
    // The asker may have given up (the window closed); that is not an error.
    let _ = reply.send(result.map_err(|e| e.to_string()));
}

fn flush(store: &mut Store, batch: &mut Vec<NewMessage>, on_error: &ErrorSink) {
    if batch.is_empty() {
        return;
    }
    if let Err(e) = store.log_messages(batch) {
        on_error(format!("could not save {} message(s) to the log: {e}", batch.len()));
    }
    batch.clear();
}

fn run(mut store: Store, rx: mpsc::Receiver<Request>, on_error: ErrorSink) {
    let mut batch: Vec<NewMessage> = Vec::new();
    while let Ok(first) = rx.recv() {
        let mut next = Some(first);
        while let Some(request) = next.take() {
            match request {
                Request::Log(message) => {
                    batch.push(message);
                    // Take whatever else is already waiting, so a burst is one
                    // transaction.
                    next = rx.try_recv().ok();
                    if next.is_none() {
                        flush(&mut store, &mut batch, &on_error);
                    }
                }
                other => {
                    // A read must see every write queued before it.
                    flush(&mut store, &mut batch, &on_error);
                    match other {
                        Request::Search {
                            query,
                            options,
                            reply,
                        } => answer(reply, store.search(&query, &options)),
                        Request::Scrollback {
                            network,
                            buffer,
                            before,
                            limit,
                            reply,
                        } => answer(reply, store.scrollback(&network, &buffer, before, limit)),
                        Request::Around { id, radius, reply } => {
                            answer(reply, store.around(id, radius))
                        }
                        Request::Buffers { network, reply } => {
                            answer(reply, store.buffers(&network))
                        }
                        Request::Log(_) => unreachable!("handled above"),
                    }
                    next = rx.try_recv().ok();
                }
            }
        }
    }
    flush(&mut store, &mut batch, &on_error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhizome_store::Kind;
    use std::sync::{Arc, Mutex};

    fn message(text: &str, msgid: &str, time_ms: i64) -> NewMessage {
        NewMessage {
            network: "net".into(),
            buffer: "#c".into(),
            sender: "bob".into(),
            kind: Kind::Privmsg,
            text: text.into(),
            server_time: None,
            received_ms: time_ms,
            msgid: Some(msgid.into()),
            own: false,
            highlight: false,
        }
    }

    fn host() -> (StoreHost, Arc<Mutex<Vec<String>>>) {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let host = StoreHost::spawn(
            Store::open_in_memory().unwrap(),
            Box::new(move |e| sink.lock().unwrap().push(e)),
        );
        (host, errors)
    }

    #[tokio::test]
    async fn a_read_sees_every_write_queued_before_it() {
        let (host, _) = host();
        for i in 0..200 {
            host.log(message(&format!("line {i}"), &format!("id{i}"), i));
        }
        // No pause: the read must not overtake the writes ahead of it.
        let page = host.scrollback("net".into(), "#c".into(), None, 1000).await.unwrap();
        assert_eq!(page.len(), 200);
        assert_eq!(page[0].text, "line 0");
        assert_eq!(page[199].text, "line 199");
    }

    #[tokio::test]
    async fn search_and_context_work_through_the_thread() {
        let (host, _) = host();
        host.log(message("the backtrace is here", "a", 1));
        host.log(message("unrelated", "b", 2));
        let hits = host.search("backtrace".into(), SearchOptions::default()).await.unwrap();
        assert_eq!(hits.len(), 1);
        let around = host.around(hits[0].message.id, 5).await.unwrap();
        assert_eq!(around.len(), 2);
        let buffers = host.buffers("net".into()).await.unwrap();
        assert_eq!(buffers[0].messages, 2);
    }

    #[tokio::test]
    async fn duplicates_are_still_ignored_when_batched() {
        let (host, _) = host();
        for _ in 0..5 {
            host.log(message("same", "one-id", 1));
        }
        let page = host.scrollback("net".into(), "#c".into(), None, 10).await.unwrap();
        assert_eq!(page.len(), 1);
    }

    #[tokio::test]
    async fn a_failed_write_is_reported_not_swallowed() {
        // Break the database from a second connection: with the search index
        // gone, the trigger that feeds it makes every insert fail.
        let path = std::env::temp_dir().join(format!(
            "rhizome-storehost-test-{}.sqlite3",
            std::process::id()
        ));
        let cleanup = |path: &std::path::Path| {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = path.as_os_str().to_owned();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        };
        cleanup(&path);

        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let host = StoreHost::spawn(
            Store::open(&path).unwrap(),
            Box::new(move |e| sink.lock().unwrap().push(e)),
        );
        host.log(message("fine", "ok", 1));
        host.buffers("net".into()).await.unwrap(); // let it land

        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE messages_fts")
            .unwrap();

        host.log(message("doomed", "bad", 2));
        // A read after the write waits for it to have been attempted.
        host.buffers("net".into()).await.unwrap();

        let reported = errors.lock().unwrap().clone();
        assert_eq!(reported.len(), 1, "the failure must reach the sink: {reported:?}");
        assert!(reported[0].contains("could not save 1 message"), "{}", reported[0]);

        drop(host);
        // Give the store thread a moment to release the file before removing it.
        std::thread::sleep(std::time::Duration::from_millis(100));
        cleanup(&path);
    }

    #[tokio::test]
    async fn the_store_thread_stays_alive_while_any_clone_exists() {
        let (host, _) = host();
        let other = host.clone();
        drop(host);
        assert!(other.buffers("net".into()).await.is_ok());
    }
}
