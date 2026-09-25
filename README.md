# Rhizome

A modern IRC client for developer networks — Libera.Chat, OFTC and the rest.

A rhizome is a root system with no centre: every node can connect to every
other, and cutting it anywhere leaves the rest intact. That is IRC's federated
shape, and the opposite of the centralised chat platforms that replaced it.

## Status

Early, but it connects: the engine registers on a real network over TLS,
tracks channels and members, and reports everything as a stream of events. There
is no graphical interface yet; a small terminal client (see below) exercises the
engine.

| Crate | State |
|---|---|
| `rhizome-proto` | ✅ Complete — 107 tests |
| `rhizome-client` | ✅ Engine complete — 89 tests, verified live against Libera.Chat |
| `rhizome-store` | ⬜ Not started — SQLite log with FTS5 search |
| `rhizome-app` | ⬜ Not started — Tauri v2 shell |
| `ui/` | ⬜ Not started — web frontend |

## Why build this

Halloy and senpai are good clients, and if all you need is to join a channel
and read it, use one of them. Rhizome exists for four things none of them do
well, all aimed at how developers actually use IRC:

**Search that works.** Every client's scrollback search is a substring scan
over what is currently in memory. A SQLite log with an FTS5 index makes
"who pasted that backtrace three months ago" an instant answer.

**Paste that does not get you killed.** Pasting twenty lines of code into a
channel either floods you off the network or dumps unreadable text on everyone.
Rhizome detects a code paste, renders it locally as a highlighted block, and
sends it as a link or via IRCv3 `multiline`.

**Links that unfurl.** A GitHub or GitLab issue link should show its title and
state inline. A commit link should show a diff summary.

**Bouncers as a first-class concept.** Most clients treat a bouncer as just
another server and leave you to reconcile the gaps by hand. Merging bouncer
history with the local log via `draft/chathistory` belongs in the design from
the start, not bolted on.

## Architecture

```
crates/
  rhizome-proto/   line protocol: framing, IRCv3 tags, ISUPPORT, casemapping,
                   formatting codes, CTCP, capability negotiation, SASL.
                   No I/O, no dependencies.
  rhizome-client/  registration, TLS, session state, rate limit,
                   reconnect. The state machine is sans-I/O; a thin
                   tokio driver wraps it.
  rhizome-store/   SQLite message log and FTS5 search               (planned)
  rhizome-app/     Tauri commands and event stream                  (planned)
ui/                web frontend                                     (planned)
```

Two deliberate choices:

**Rust core, webview UI.** A chat client is fundamentally a text layout
problem: selectable text, virtualised scrollback, emoji, bidi, link detection,
code blocks. A webview solves all of that for free, while the socket, TLS,
protocol state machine and log stay in Rust. WebView2 ships with Windows 11, so
binaries land around 8–10 MB rather than Electron's 150 MB.

**`rhizome-proto` has no dependencies and no I/O.** It is pure `std`. That
keeps it exhaustively testable without a network, and lets the desktop app, the
Android app and any future headless tool share one parser.

**`rhizome-client` keeps decisions out of the socket code.** `Session` is a
state machine that takes parsed messages and returns lines to send and events to
report; it reads no clock and opens no connection. The handshake, SASL, channel
tracking and message routing are therefore tested from transcripts, and the
driver around it stays thin. Every front end consumes the same `Event` stream.

## Four things the protocol layer exists to get right

These are the mistakes that make a client subtly wrong in ways users report as
"it corrupts names sometimes".

**Never compare names with `to_lowercase`.** RFC 1459 inherits a character set
where `{}|` are the lowercase forms of `[]\`, so on most networks `nick[]` and
`nick{}` are the same user. The rule is per-network, announced in `CASEMAPPING`.
Use `CaseMapping::eq` or `CaseMapping::fold` for every nick and channel
comparison, map key and lookup. See `casemap.rs`.

**Size messages in bytes, not characters — and subtract your own hostmask.**
The 512-byte limit applies to the line *the server relays*, which has
`:nick!user@host ` prepended. A message sized to 512 locally arrives truncated.
And Turkish, Greek or CJK text hits the byte limit well before the character
count suggests; cutting by character count eventually slices a UTF-8 sequence
in half. See `split.rs`.

**Read capabilities from `ISUPPORT`, never assume them.** Channel prefixes,
membership modes, name lengths and mode parameter rules all vary by network.
Hardcoding the common answers works on Libera and corrupts the user list
somewhere else. See `isupport.rs`.

**A message body may not be text.** It can carry mIRC formatting codes
(`format.rs`) or be a CTCP request in disguise (`ctcp.rs`) — `/me` is
`\x01ACTION ...\x01` inside an ordinary `PRIVMSG`.

Registration is covered too. `cap.rs` tracks capability negotiation — including
multiline `LS`, negated `ACK`s, and the rule that a `NAK` changes nothing
because the request is refused as a unit. `sasl.rs` handles authentication,
including the trap where a base64 payload of exactly 400 bytes needs an empty
continuation or the server waits for it forever, and the rule that `PLAIN` is
never selected without TLS.

## Safety properties

- **No command injection.** Every outgoing line passes
  `Message::validate_for_send`, which refuses CR, LF and NUL in any parameter.
  A pasted `hello
QUIT` becomes chat text, never a second command; this is
  tested over a real socket.
- **Passwords stay out of logs.** SASL and server passwords are redacted in
  `Debug` output.
- **SASL `PLAIN` needs TLS,** and a failed login ends the connection instead of
  retrying: repeating a wrong password risks locking the account.
- **A configured login is never silently skipped.** If SASL is requested and
  cannot be used, the connection is abandoned rather than continued
  unauthenticated.
- **The keepalive reply is never queued behind chat.** A pasted block is
  rate-limited, but a `PONG` overtakes it, otherwise the flood control that
  protects the connection would get it dropped for ping timeout.

## Building

Needs Rust 1.75+ and, on Windows, the MSVC build tools.

```bash
cargo test
```

## Trying it

A minimal terminal client is included:

```bash
cargo run -p rhizome-client --example connect --     --nick my_nick --join "#rhizome"
```

For a NickServ account, add `--sasl-user NAME` and put the password in the
`RHIZOME_SASL_PASSWORD` environment variable; it is deliberately not an
argument, since those end up in shell history. Type text to talk in the current
channel; `/join`, `/part`, `/me`, `/msg`, `/nick`, `/buf`, `/raw` and `/quit`
are supported.

## Next steps

1. `rhizome-store`: SQLite schema and FTS5 index, fed from the `Event` stream.
2. `rhizome-app` + `ui/`: the Tauri shell and the first usable window.
3. Client certificates for SASL `EXTERNAL`. The protocol layer already models
   it; the driver does not yet load a certificate.
4. An opt-in way to accept a self-signed server certificate.
5. Android target once the desktop MVP works — the shared core is already
   free of platform code, so this is a build-system task, not a rewrite.

## Licence

GPL-3.0-or-later, for every crate. See `LICENSE`.

## Open decisions

- **Send rate.** The default is a burst of 5 then 1 message per second,
  deliberately conservative and configurable per connection. It has not been
  tuned against any particular network's actual flood limit.
- **Android NDK.** Not installed; deferred until the desktop MVP runs.
- **Bouncer.** Optional. The client works standalone with its own local log;
  a bouncer such as soju only matters for staying connected while the client
  is closed.
