# Rhizome

A modern IRC client for developer networks — Libera.Chat, OFTC and the rest.

A rhizome is a root system with no centre: every node can connect to every
other, and cutting it anywhere leaves the rest intact. That is IRC's federated
shape, and the opposite of the centralised chat platforms that replaced it.

## Status

Version 0.3.0: a desktop app for Windows, macOS and Linux that connects to IRC
networks over TLS, follows channels and members, keeps a searchable log of
everything it sees, remembers what you have read, and survives a restart. Seven
themes, English and Turkish. It has been exercised end to end in its real
window (see *Testing*), but it has not yet had a week of daily use.

| Crate | State |
|---|---|
| `rhizome-proto` | ✅ Complete — 108 tests |
| `rhizome-client` | ✅ Engine complete — 90 tests, verified live against Libera.Chat |
| `rhizome-store` | ✅ Complete — 88 tests; message log and full-text search on SQLite FTS5 |
| `rhizome-app` | ✅ Working — Tauri v2 window joining engine and store; 81 tests + 42 end-to-end checks |
| `ui/` | ✅ Working — plain JavaScript, no build step; 130 tests |

Every push is built and tested on Windows, macOS and Linux by CI (see the
badge... there is no badge yet, see the Actions tab). Only **Windows** has been
driven by a person end to end, in the real window described under *Testing*;
the macOS and Linux installers come from the same CI matrix that runs
`cargo test --workspace` there, but nobody has clicked through them by hand
yet — treat them as "compiles and passes its tests" rather than "used daily".

## Install

Installers for all three desktops are attached to each
[release](https://github.com/alplix/rhizome/releases).

**Windows:** `Rhizome_<version>_x64-setup.exe`. Installs for the current user
only, no administrator rights needed. Windows 11 already includes WebView2;
the installer fetches it on Windows 10.

**macOS:** `Rhizome_<version>_aarch64.dmg` (Apple Silicon / M-series only; an
Intel build is not cross-compiled yet, and Rosetta only translates the other
direction, so it will not run on an Intel Mac). Open the disk image and drag
Rhizome into Applications.

**Linux:** `rhizome_<version>_amd64.deb` for Debian/Ubuntu-based
distributions, or the portable `rhizome_<version>_amd64.AppImage` for
anything else (`chmod +x` it, then run it). Needs a WebKitGTK 4.1 runtime,
which the `.deb` pulls in as a dependency and the AppImage bundles itself.

**None of the installers are code-signed**, so:
- Windows SmartScreen says the publisher is unknown ("More info" → "Run anyway").
- macOS Gatekeeper refuses to open it the first time; right-click the app →
  Open, then confirm, or `xattr -d com.apple.quarantine Rhizome.app`.

Signing needs a paid certificate on both platforms; until there is one, verify a
download against the `.sha256` checksum file published alongside it, or build
from source.

There is no auto-updater: install a newer version over the old one, and your
networks, settings and log are kept.

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
  rhizome-store/   SQLite message log and FTS5 search. Independent of
                   the engine: takes plain values, knows no sockets.
  rhizome-app/     Tauri window. `core` (networks, log, event
                   translation) is independent of Tauri and tested
                   without a window; lib.rs is thin glue.
ui/                the interface: ES modules, no bundler, no npm.
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

## Search

Every message is written to a local SQLite log and indexed with FTS5. The
search box understands what a developer types:

| You type | It means |
|---|---|
| `null pointer` | both words, anywhere, in any order |
| `"null pointer"` | that exact phrase |
| `kmall*` | any word starting with `kmall` |
| `from:bob` | only messages from `bob` |
| `in:#kernel` | only messages in `#kernel` |

- **Nothing typed is a search error.** FTS5's own query language is never
  exposed: every word is quoted as a literal, so a stray `"`, the word `AND`,
  or `C++` cannot produce a syntax error. A half-typed `from:` is ignored
  rather than searched for as text.
- **Formatting codes are not part of a word.** A colour change in the middle of
  `error` does not stop it matching, while the stored text keeps its codes so it
  renders faithfully.
- **`snake_case` identifiers stay whole,** so `kmalloc_array` does not also match
  every message that merely mentions `array`. Use `kmalloc*` to match the family.
- **Turkish is searchable without Turkish keys.** `dunya`, `turkce` and `cumleyi`
  find `dünya`, `Türkçe` and `cümleyi`; capital `İ` matches `i`. The dotless `ı`
  is a distinct Turkish letter, not an accented `i`, so SQLite's own tokenizer
  cannot fold it — `hatayi` finds `hatayı` (and the reverse) because the query
  parser tries both spellings itself wherever either letter appears, not
  because the index was changed; the search result still shows the real text.
- **History replays are idempotent.** A message with the same server-assigned
  id in the same buffer is stored once, so reconnecting and re-fetching
  overlapping history adds nothing.
- **Paging is by cursor, not offset,** so messages arriving while you scroll
  cannot shift a page and repeat or skip lines.

What is logged is chat text (messages, notices and `/me` actions) and, as
separate lines, joins, parts, quits, kicks, nick changes, topic changes and
channel mode changes. Event lines are shown in the scrollback and can be
switched off in Settings, but they are never searched, never counted as unread
and never raise a notification.

## The window

- **Three panes:** networks and conversations with unread and mention badges;
  the messages with day dividers and an "unread" marker; the channel's members.
  On a narrow screen the side panes become drawers. A first run shows a welcome
  screen with one-click presets for Libera.Chat, OFTC and hackint.
- **Themes:** Graphite, Midnight, Forest, Paper, Daylight and a High contrast
  theme, or *System* to follow the operating system. Six accent colours,
  comfortable or compact spacing, three text sizes. Every theme and accent is
  checked by a test against WCAG contrast thresholds, including all nick colours.
- **Languages:** English and Türkçe, chosen automatically or in Settings. A test
  fails if a translation is missing, has a different placeholder, or is unused.
- **Read markers are remembered** in the log, so unread counts and the "unread"
  line are right after a restart.
- **Notifications** for mentions and private messages while the window is not in
  front; switchable in Settings.
- **Autoconnect** per network, and **Remember password**, which stores it in the
  operating system's credential store (Windows Credential Manager). It is never
  written to a file in the data directory, and a failed login deletes it.
- **Search (Ctrl+K)** over everything logged, across networks or in one, by best
  match or newest. Opening a result shows it in its conversation, with a way back
  to the live end.
- **Typing:** Tab completes nicks, ↑ recalls what you sent, `/help` lists the
  commands (including `/clear`). A typo such as `/joinn` is reported, never
  posted as chat.
- **Pasting** more than three lines asks first, and each line then goes out as its
  own message under the rate limit.
- **IRC colours stay readable:** colours chosen for a white or black page are
  nudged until they have enough contrast on the current theme.
- **One window:** starting Rhizome again focuses the running one, and the window
  reopens where you left it.
- **Closing the window keeps you connected.** By default the X button (or
  Alt+F4/Cmd+Q) hides the window to a tray icon rather than quitting; every
  network stays joined, and the tray icon's menu shows the window again or
  quits for real. Turn this off in Settings → Behaviour if you would rather
  the window closing end the session — that still says goodbye to every
  server properly before the process exits, instead of just vanishing.

## Safety properties

Everything a stranger on an IRC network sends ends up in this window, so it is
treated as hostile input.

- **Message text is never markup.** The backend turns formatting codes into
  styled spans and the interface builds them with `textContent`; there is no
  `innerHTML` for message content anywhere.
- **Links are `http(s)` only,** opened in the system browser by the backend, which
  refuses `javascript:`, `file:`, `data:` and app-registered schemes such as
  `ms-msdt:`.
- **The window cannot leave the app.** Navigation to anything but the app's own
  origin is refused, so a link that slipped past the interface still cannot
  replace Rhizome with a web page.
- **Scripts and styles come from the app only** (a content security policy, with
  no inline scripts or `style` attributes), and the window's capability set is
  `core:default`: no filesystem, shell or extra plugin access.
- **A profile is validated by the backend,** whatever the interface allowed
  through, and a corrupt profile file is reported rather than overwritten.

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

Needs Node (for the Tauri CLI) alongside Rust. Platform prerequisites:

- **Windows:** the MSVC build tools and WebView2 (already part of Windows 11). NSIS
  is fetched automatically.
- **macOS:** Xcode command line tools (`xcode-select --install`).
- **Linux:** WebKitGTK 4.1 and friends —
  `sudo apt-get install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libdbus-1-dev pkg-config file`
  on Debian/Ubuntu; equivalent packages elsewhere.

To produce the installer for whichever platform you are building on:

```bash
cd crates/rhizome-app
npx @tauri-apps/cli@2 build
```

`tauri.conf.json` sets the bundle target to `"all"`, which resolves to
whatever the host can build: NSIS on Windows, a `.app`/`.dmg` on macOS, and a
`.deb`/AppImage on Linux. Pass `--bundles nsis` (or `dmg`, `deb`, `appimage`) to
build only one. The result lands under `target/release/bundle/<kind>/`.

`.github/workflows/release.yml` builds all three from one tag push, on real
Windows, macOS and Linux runners, and attaches the results plus a `.sha256`
checksum for each to the GitHub release — that is how the *Install* section's
downloads are produced; nobody hand-builds a release.

Minimum Rust versions, taken from what each crate's dependencies declare:
`rhizome-proto` 1.75, `rhizome-client` and `rhizome-store` 1.85, `rhizome-app`
1.88. Only the current stable toolchain (1.98) has actually been built with.

```bash
cargo test --workspace
```

## Trying it

Run the application:

```bash
cargo run -p rhizome-app
```

Pick a preset on the welcome screen (or add a network with the **+** button), connect, and join a channel with
`/join #channel`. Your messages and settings are kept in the application's data
directory (`%APPDATA%\org.rhizome.irc` on Windows).

To work on the interface without the application, serve `ui/` with any static
server and open it in a browser. With no Tauri around it runs against a demo
backend (`ui/mock.js`) with a fake network, history and a bot, including
deliberately hostile messages, so the interface can be looked at and tested
without connecting anywhere:

```bash
python -m http.server 8770 --directory ui
```

A minimal terminal client is also included:

```bash
cargo run -p rhizome-client --example connect -- --nick my_nick --join "#rhizome"
```

For a NickServ account, add `--sasl-user NAME` and put the password in the
`RHIZOME_SASL_PASSWORD` environment variable; it is deliberately not an
argument, since those end up in shell history. Type text to talk in the current
channel; `/join`, `/part`, `/me`, `/msg`, `/nick`, `/buf`, `/raw` and `/quit`
are supported.

That client does not write to the log; the desktop app does.

## Testing

| What | Where | Run |
|---|---|---|
| Protocol, engine, store, app core | Rust unit and integration tests | `cargo test --workspace` |
| Interface logic (commands, links, state) | `ui/tests` | `node --test "ui/tests/*.test.mjs"` |
| **The real window** | `crates/rhizome-app/e2e/webview_e2e.py` | `python crates/rhizome-app/e2e/webview_e2e.py` (Windows, after `cargo build -p rhizome-app`; set `RHIZOME_EXE` to test the release build) |

The last one launches the actual application, drives its WebView2 page over the
DevTools Protocol and connects it to a scripted IRC server on localhost. It is
the only test that exercises the Tauri glue: the IPC commands, event delivery,
the capability set, the content security policy, the navigation guard, closing
the window to the tray versus really quitting (with a real `WM_CLOSE`, not a
script call), and settings, read markers and the log surviving a restart. It
runs against a throwaway directory (the `RHIZOME_DATA_DIR` environment
variable, honoured only when set) rather than the application's real data
directory, so it can never see or touch a real profile, log or settings file,
however many are already saved there.

## Known limitations

Things that are not done, stated plainly:

- **macOS and Linux are built and tested by CI, but not yet used by a person.**
  Only the Windows build has been driven end to end in its real window. No
  Intel Mac build (GitHub's `macos-latest` runner is Apple Silicon; an Intel
  build would need cross-compiling, which is not set up).
- **No Android build.** The NDK is not installed and the app crate is not set
  up as a mobile library.
- **No auto-updater and no code signing on any platform** (see *Install*).
- **No SASL `EXTERNAL`.** The protocol layer models it; the driver does not load
  a client certificate.
- **No way to accept a self-signed server certificate.** A server whose
  certificate does not verify cannot be connected to.
- **No link previews and no multiline paste yet** — the paste and unfurl ideas in
  *Why build this* are the plan, not the present.
- **Right-to-left text** has not been designed for or tested.
- **Nothing here has had weeks of daily use.**

## Licence

GPL-3.0-or-later, for every crate. See `LICENSE`.

## Open decisions

- **Send rate.** The default is a burst of 5 then 1 message per second,
  deliberately conservative and configurable per connection. It has not been
  tuned against any particular network's actual flood limit.
- **Android.** Tauri v2 targets Android, but the app crate is not yet set up as a
  mobile library and the NDK is not installed. The layout already collapses to
  drawers on a narrow screen.
- **Bouncer.** Optional. The client works standalone with its own local log;
  a bouncer such as soju only matters for staying connected while the client
  is closed.
