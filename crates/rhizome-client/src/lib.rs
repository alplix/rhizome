//! The Rhizome IRC client engine.
//!
//! This crate connects to a network, registers, tracks channels and members,
//! and reports what happens as a stream of [`Event`]s. It has no UI: the
//! terminal example, the desktop app and any other front end are thin
//! consumers of the same events.
//!
//! # Layers
//!
//! * [`Session`] is the protocol state machine. It does no I/O and reads no
//!   clock: feed it parsed messages and it answers with lines to send and
//!   events to report. All connect-time and channel logic lives here, so it is
//!   tested with transcripts and no network.
//! * [`connection`] is the driver around it: sockets, TLS, keepalive, the send
//!   rate limit and reconnects with backoff.
//! * [`codec`], [`ratelimit`] and [`backoff`] are the small pieces the driver
//!   uses, each testable on its own.
//!
//! # Example
//!
//! ```no_run
//! use rhizome_client::{spawn, Config, Event};
//!
//! # async fn demo() {
//! let config = Config::new("irc.libera.chat", "my_nick").autojoin(["#rhizome"]);
//! let mut client = spawn(config);
//!
//! while let Some(event) = client.events.recv().await {
//!     match event {
//!         Event::Message(m) => println!("<{}> {}", m.sender, m.text),
//!         Event::Disconnected { retry_in: None, .. } => break,
//!         _ => {}
//!     }
//! }
//! # }
//! ```
//!
//! # Safety properties
//!
//! Nothing user-supplied reaches the socket without passing
//! [`rhizome_proto::Message::validate_for_send`], so a pasted line break
//! cannot smuggle in a second command. Passwords are wrapped so that they do
//! not appear in `Debug` output. SASL `PLAIN` is refused without TLS, and a
//! failed login ends the connection rather than retrying.

pub mod backoff;
pub mod codec;
pub mod config;
pub mod connection;
pub mod event;
pub mod ratelimit;
pub mod session;

pub use config::{Config, Secret};
pub use connection::{spawn, Client, Closed, Command, Handle};
pub use event::{ChatMessage, Event, Member, MessageKind};
pub use session::{Output, Session};
