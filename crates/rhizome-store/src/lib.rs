//! The message log and full-text search.
//!
//! Every message is written to a local SQLite database and indexed with FTS5,
//! so that "who pasted that backtrace three months ago" is a query and not an
//! act of scrolling. This is the part of Rhizome that exists because other
//! clients search only what happens to be in memory.
//!
//! The crate is deliberately independent of the connection engine: it takes
//! plain [`NewMessage`] values and knows nothing about sockets or sessions, so
//! the same store can back a desktop window, a phone app or a headless logger.
//!
//! # What is stored and what is searched
//!
//! The original text is stored exactly as received, formatting codes and all,
//! so it can be rendered faithfully later. The search index is built from a
//! copy with those codes removed, so a colour code in the middle of a word
//! does not stop it matching.
//!
//! # Searching
//!
//! [`Store::search`] takes what a person types into a search box. Ordinary
//! words are ANDed together, `"double quotes"` match a phrase, a trailing `*`
//! matches a prefix, and `from:nick` and `in:#channel` narrow the results.
//! Nothing a user can type is an error: FTS5's own query syntax is never
//! exposed, so a stray quote or the word `AND` cannot produce a syntax error.
//!
//! ```
//! use rhizome_store::{NewMessage, Kind, SearchOptions, Store};
//!
//! let mut store = Store::open_in_memory().unwrap();
//! store.log_message(&NewMessage {
//!     network: "Libera.Chat".into(),
//!     buffer: "#rhizome".into(),
//!     sender: "bob".into(),
//!     kind: Kind::Privmsg,
//!     text: "the backtrace shows a null pointer in kmalloc".into(),
//!     server_time: Some("2026-09-25T10:00:00.000Z".into()),
//!     received_ms: 0,
//!     msgid: None,
//!     own: false,
//!     highlight: false,
//! }).unwrap();
//!
//! let hits = store.search("backtrace from:bob", &SearchOptions::default()).unwrap();
//! assert_eq!(hits.len(), 1);
//! assert_eq!(hits[0].message.buffer, "#rhizome");
//! ```
//!
//! # Threading
//!
//! A [`Store`] owns one SQLite connection and is not `Sync`. Give it its own
//! thread (or `spawn_blocking`) and talk to it over a channel.

mod error;
mod schema;
mod store;
mod types;

pub mod query;
pub mod time;

pub use error::{Error, Result};
pub use store::Store;
pub use types::{
    BufferInfo, Cursor, Kind, NewMessage, SearchHit, SearchOptions, SearchOrder, StoredMessage,
    MARK_END, MARK_START,
};
