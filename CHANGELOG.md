# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow
[Semantic Versioning](https://semver.org/) once 1.0 is reached; until then a
minor version may change things.

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
