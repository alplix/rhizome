# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow
[Semantic Versioning](https://semver.org/) once 1.0 is reached; until then a
minor version may change things.

## [0.6.0] — 2026-09-27

### Added
- **SASL `EXTERNAL`.** A network profile can point at a PEM file holding a
  TLS client certificate and its private key; it is presented during the TLS
  handshake and the account authenticates by it, no password at all. Takes
  priority over a SASL `PLAIN` account on the same profile, which is then
  only consulted as the `authzid` (who to act as), for a network where that
  differs from the certificate's own identity.
- `rhizome_client::identity::ClientCert`: reads a certificate chain and its
  key from PEM text, in either order, cert and key together in one file or
  from two concatenated.
- `Config::sasl_external` / the TLS connector now presents a per-connection
  client certificate, verified against a real mutual-TLS handshake (and a
  certificate from an untrusted CA, and no certificate at all, both actually
  refused by a server that requires one — not just asserted, run against a
  real TLS server built for the test).

### Known limitations
No way yet to accept a self-signed *server* certificate — a different thing
from presenting a client one, and still on the list.

## [0.5.0] — 2026-09-27

### Added
- **File transfer, over DCC.** `/dcc send nick path` offers a file to
  someone; an offer they send you shows up as an Accept/Decline card in your
  conversation with them, with a progress bar while it moves and the saved
  path once it lands (under this application's own downloads folder, never
  overwriting a file already there). Active DCC only — your side listens,
  theirs connects, so it needs your machine reachable on the port it opens,
  the same requirement every classic DCC client has. "Reverse" (passive) DCC
  is not implemented; an offer that needs it is shown, but marked as one this
  client cannot accept, rather than silently failing.
- `crates/rhizome-proto/src/dcc.rs`: parses and builds the `DCC SEND` CTCP
  request itself (the filename, the IPv4 address as the classic 32-bit
  integer, port, size), independent of any socket.
- `crates/rhizome-client/src/dcc.rs`: the transfer itself — a second, plain
  TCP connection alongside the chat one, moving the file's bytes with
  progress reported as it goes.
- `Handle::ctcp` / `Session::send_ctcp`: a way to send a CTCP request of our
  own (needed to make a DCC offer) that is never echoed locally as a chat
  message, the same as how a CTCP *reply* already worked.

## [0.4.0] — 2026-09-27

### Added
- **Close to tray.** By default, closing the window (X, Alt+F4, Cmd+Q) hides
  it to a new tray icon instead of quitting; every network stays connected.
  The tray icon's menu shows the window again or quits for real. A new
  Settings → Behaviour toggle turns this off, in which case closing the
  window disconnects from every network properly (a real `QUIT`, with a short
  grace period for it to reach the wire) before the process exits, instead of
  just killing it. Verified against the real window with an actual `WM_CLOSE`,
  not a script call.
- Search now finds a message regardless of whether it (or the query) used a
  plain `i` or Turkish's dotless `ı`: "hatayi" finds "hatayı" and back again,
  and the result still shows the real text. Closes the "dotless ı" item under
  *Known limitations*.

### Changed
- `crates/rhizome-app/e2e/webview_e2e.py` now runs against a throwaway
  directory (`RHIZOME_DATA_DIR`) instead of the application's real data
  directory, so it can never see or touch a real profile, log or settings
  file. Previously it refused to run at all if that directory already
  existed; it no longer needs to.

## [0.3.0] — 2026-09-27

### Added
- **macOS (`.dmg`) and Linux (`.deb`, AppImage) installers**, built alongside
  the Windows one from a single tag push. `crates/rhizome-app/tauri.conf.json`
  now bundles `"all"` targets for whichever platform builds it; the release
  workflow builds all three explicitly and attaches a `.sha256` checksum to
  each.
- CI (`ci.yml`) now runs `cargo test --workspace` on Windows, macOS and Linux on
  every push, not only Windows.
- A full icon set generated from the 512×512 source, including macOS `.icns`.

### Changed
- Nothing in the application itself; this release is packaging only.

### Known limitations
macOS and Linux builds are new and CI-verified only (compiles, passes its
tests) — nobody has run them by hand yet. See the README's *Known limitations*
for the full, unchanged list otherwise.

## [0.2.0] — 2026-09-25

First packaged release.

### Added
- **Themes:** Graphite, Midnight, Forest, Paper, Daylight, High contrast, and
  System. Six accent colours, compact spacing, three text sizes.
- **Languages:** English and Türkçe, automatic or chosen in Settings.
- **Settings dialog**, stored in `settings.json`.
- **Welcome screen** with presets for Libera.Chat, OFTC and hackint.
- **Event lines:** joins, parts, quits, kicks, nick changes, topic changes and
  channel modes are logged and shown in the scrollback (switchable). They are
  not searched, not counted as unread and never notify.
- **Read markers** persist, so unread counts and the "unread" line survive a restart.
- **Remember password** in the operating system's credential store; a failed
  login deletes it.
- **Autoconnect** per network.
- **Desktop notifications** for mentions and private messages.
- `/clear`; window position and size are remembered; a second launch focuses the
  first.
- Windows installer (NSIS, per-user, English and Turkish).

### Changed
- Log schema is now version 2 (read markers, event lines). Older logs are
  migrated in place on first start; a log written by a newer version is refused
  rather than damaged.
- Interface text is translated; command errors are translation keys.

### Known limitations
See the README: Windows only, unsigned installer, no auto-updater, no SASL
`EXTERNAL`, no self-signed certificate option, dotless `ı` not folded in search.

## [0.1.0]

Protocol layer, connection engine, message log with full-text search, and a
first desktop window. Not packaged.
