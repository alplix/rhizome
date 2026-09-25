# Contributing

Everything is GPL-3.0-or-later; by contributing you agree your work is too.

## Before you open a pull request

```bash
cargo fmt --check
cargo clippy --workspace --all-targets     # must print no warnings
cargo test --workspace
node --test "ui/tests/*.test.mjs"          # Node 20+; the glob needs the quotes
```

On Windows, also run the end-to-end test when you touch the app or the interface:

```bash
cargo build -p rhizome-app
python crates/rhizome-app/e2e/webview_e2e.py
```

## House rules

- **No I/O in `rhizome-proto`, no clock or sockets in `Session`.** Logic goes in
  the sans-I/O layers so it can be tested from transcripts.
- **Message text is never markup.** Use `textContent`, never `innerHTML`, for
  anything that came from the network.
- **No inline scripts or `style` attributes:** the content security policy
  forbids them.
- **Every user-visible string goes through `t()`** and exists in both
  `ui/locales/en.js` and `tr.js`; the tests enforce this.
- **New theme colours must pass** `ui/tests/themes.test.mjs` (WCAG contrast).
- Compare nicks and channels with the network's case mapping, never `to_lowercase`.
- Fix a bug with a test that fails first.
