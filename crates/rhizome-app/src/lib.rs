//! The Rhizome desktop application.
//!
//! This crate is the glue between three things: the connection engine
//! (`rhizome-client`), the message log (`rhizome-store`) and a web interface in
//! `ui/`. Everything that is not Tauri-specific lives in [`core`], which can be
//! driven and tested without a window; this file only turns its methods into
//! commands the interface can call and forwards its events to the window.
//!
//! # Security posture
//!
//! Everything a stranger on an IRC network sends ends up in this window, so the
//! window is treated as untrusted-input territory:
//!
//! * message text reaches the interface as styled spans, never as markup, and the
//!   interface builds it with `textContent` only;
//! * the content security policy allows scripts and styles from the app itself
//!   and nothing else;
//! * the window may not navigate anywhere except the app ([`is_app_url`]), so a
//!   link that slips past the interface's own handling cannot replace the
//!   application with a web page;
//! * links are opened by [`open_url`], in the system browser, and only when they
//!   are plain `http` or `https` addresses ([`checked_web_url`]);
//! * the window's capability set is just `core:default`: no filesystem, shell or
//!   network access of its own.

pub mod core;
pub mod dto;
pub mod profiles;
pub mod storehost;

use rhizome_client::MessageKind;
use rhizome_store::Store;
use tauri::{Emitter, Manager, State, Url};
use tokio::sync::mpsc;

use crate::core::Core;
use crate::dto::{UiBuffer, UiCursor, UiHit, UiMessage};
use crate::profiles::{Profile, ProfileStore};
use crate::storehost::StoreHost;

/// The event name the interface listens on.
const EVENT_NAME: &str = "rhizome://event";

/// The most a page of history or a set of search results may hold. The
/// interface asks for less; this only bounds a misbehaving caller.
const MAX_PAGE: usize = 500;

struct AppState {
    core: Core,
    profiles: ProfileStore,
    /// Problems found while starting up, shown when the interface first loads.
    /// An event emitted before the page is listening would be lost.
    notices: Vec<String>,
}

/// Whether a URL is part of the application itself.
///
/// The window is allowed to be at these addresses and nowhere else. On Windows
/// and Android the app is served from `http://tauri.localhost`; elsewhere from
/// the `tauri` scheme.
pub fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

/// Validates an address the interface wants opened in the browser.
///
/// Only plain web addresses pass. Anything else a message could contain, such
/// as `javascript:`, `file:`, `data:` or a registered app scheme like
/// `ms-msdt:`, is refused: opening one would hand a stranger a way to run
/// something on this machine.
pub fn checked_web_url(input: &str) -> Result<Url, String> {
    if input.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("that is not a plain web address".into());
    }
    let url = Url::parse(input).map_err(|e| format!("not a valid address: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err("only http and https addresses can be opened".into());
    }
    Ok(url)
}

fn parse_kind(kind: &str) -> Result<MessageKind, String> {
    match kind {
        "privmsg" => Ok(MessageKind::Privmsg),
        "notice" => Ok(MessageKind::Notice),
        "action" => Ok(MessageKind::Action),
        other => Err(format!("unknown message kind {other:?}")),
    }
}

fn page_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_PAGE)
}

// ---- commands ------------------------------------------------------------

#[tauri::command]
fn startup_notices(state: State<'_, AppState>) -> Vec<String> {
    state.notices.clone()
}

#[tauri::command]
fn list_profiles(state: State<'_, AppState>) -> Result<Vec<Profile>, String> {
    state.profiles.load()
}

#[tauri::command]
fn save_profile(state: State<'_, AppState>, profile: Profile) -> Result<(), String> {
    state.profiles.upsert(profile)
}

#[tauri::command]
fn delete_profile(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.profiles.remove(&id)
}

/// Connects using a saved profile. The password, if the profile needs one, is
/// supplied here and is not stored anywhere.
#[tauri::command]
async fn connect(
    state: State<'_, AppState>,
    id: String,
    sasl_password: Option<String>,
) -> Result<(), String> {
    let profile = state
        .profiles
        .load()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("there is no saved network called {id}"))?;
    state.core.connect(&id, profile.to_config(sasl_password)?)
}

#[tauri::command]
fn disconnect(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.core.disconnect(&id)
}

#[tauri::command]
fn send_message(
    state: State<'_, AppState>,
    network: String,
    target: String,
    text: String,
    kind: String,
) -> Result<(), String> {
    state
        .core
        .send_message(&network, &target, &text, parse_kind(&kind)?)
}

#[tauri::command]
fn join(state: State<'_, AppState>, network: String, channels: Vec<String>) -> Result<(), String> {
    state.core.join(&network, &channels)
}

#[tauri::command]
fn part(
    state: State<'_, AppState>,
    network: String,
    channel: String,
    reason: Option<String>,
) -> Result<(), String> {
    state.core.part(&network, &channel, reason.as_deref())
}

#[tauri::command]
fn set_nick(state: State<'_, AppState>, network: String, nick: String) -> Result<(), String> {
    state.core.set_nick(&network, &nick)
}

#[tauri::command]
fn raw(state: State<'_, AppState>, network: String, line: String) -> Result<(), String> {
    state.core.raw(&network, &line)
}

#[tauri::command]
async fn scrollback(
    state: State<'_, AppState>,
    network: String,
    buffer: String,
    before: Option<UiCursor>,
    limit: usize,
) -> Result<Vec<UiMessage>, String> {
    state
        .core
        .scrollback(&network, &buffer, before, page_limit(limit))
        .await
}

#[tauri::command]
async fn search(
    state: State<'_, AppState>,
    query: String,
    network: Option<String>,
    newest_first: bool,
    limit: usize,
) -> Result<Vec<UiHit>, String> {
    state
        .core
        .search(&query, network.as_deref(), newest_first, page_limit(limit))
        .await
}

#[tauri::command]
async fn around(
    state: State<'_, AppState>,
    id: i64,
    radius: usize,
) -> Result<Vec<UiMessage>, String> {
    state.core.around(id, radius.min(MAX_PAGE)).await
}

#[tauri::command]
async fn buffers(state: State<'_, AppState>, network: String) -> Result<Vec<UiBuffer>, String> {
    state.core.buffers(&network).await
}

/// Opens a link from a message in the system browser.
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    let url = checked_web_url(&url)?;
    tauri_plugin_opener::open_url(url.as_str(), None::<&str>).map_err(|e| e.to_string())
}

// ---- startup -------------------------------------------------------------

/// Opens the log, falling back to an in-memory one if the file cannot be used,
/// and says so. Losing history is bad; losing the whole application because the
/// log will not open is worse.
fn open_store(path: &std::path::Path, notices: &mut Vec<String>) -> Store {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match Store::open(path) {
        Ok(store) => store,
        Err(e) => {
            notices.push(format!(
                "The message log at {} could not be opened ({e}). \
                 Messages will be kept for this session only.",
                path.display()
            ));
            Store::open_in_memory().expect("an in-memory database can always be created")
        }
    }
}

/// Starts the application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let mut notices = Vec::new();

            // Failures on the store's thread are queued here and forwarded to
            // the interface once the core exists.
            let (failure_tx, mut failures) = mpsc::unbounded_channel::<String>();
            let store = StoreHost::spawn(
                open_store(&dir.join("rhizome.sqlite3"), &mut notices),
                Box::new(move |failure| {
                    let _ = failure_tx.send(failure);
                }),
            );
            let (core, mut events) = Core::new(store);

            let reporter = core.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(failure) = failures.recv().await {
                    reporter.report(failure);
                }
            });

            let window_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Some(envelope) = events.recv().await {
                    let _ = window_handle.emit(EVENT_NAME, &envelope);
                }
            });

            app.manage(AppState {
                core,
                profiles: ProfileStore::new(dir.join("profiles.json")),
                notices,
            });

            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("Rhizome")
                .inner_size(1200.0, 780.0)
                .min_inner_size(720.0, 480.0)
                .on_navigation(is_app_url)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup_notices,
            list_profiles,
            save_profile,
            delete_profile,
            connect,
            disconnect,
            send_message,
            join,
            part,
            set_nick,
            raw,
            scrollback,
            search,
            around,
            buffers,
            open_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Rhizome application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn only_the_app_itself_is_an_allowed_navigation_target() {
        assert!(is_app_url(&url("tauri://localhost/index.html")));
        assert!(is_app_url(&url("http://tauri.localhost/index.html")));
        assert!(is_app_url(&url("https://tauri.localhost/")));

        for hostile in [
            "https://evil.example/",
            "http://evil.example/index.html",
            "http://tauri.localhost.evil.example/",
            "https://evil.example/tauri.localhost",
            "http://localhost/",
            "http://127.0.0.1:8080/",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "about:blank",
            "ms-msdt:/id",
        ] {
            assert!(!is_app_url(&url(hostile)), "{hostile} must not be navigable");
        }
    }

    #[test]
    fn ordinary_web_addresses_may_be_opened() {
        for ok in [
            "https://github.com/alplix/rhizome/issues/1",
            "http://example.com",
            "https://example.com:8443/a?b=c#d",
            "https://tr.wikipedia.org/wiki/İstanbul",
        ] {
            assert!(checked_web_url(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn anything_that_is_not_plain_http_is_refused() {
        for bad in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "file:///C:/Windows/System32/cmd.exe",
            "data:text/html,hi",
            "ms-msdt:/id PCWDiagnostic",
            "search-ms:query=x",
            "vscode://file/etc/passwd",
            "ftp://example.com/",
            "mailto:a@b.c",
            "//example.com",
            "example.com",
            "",
            "https://",
            "http://",
        ] {
            assert!(checked_web_url(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn what_is_opened_is_the_normalised_address_not_the_raw_text() {
        // The URL standard ignores extra slashes after `http:`, so this is the
        // host "path". That is harmless, but it shows why the opener is given the
        // parsed form: the string that was checked is the string that is used.
        let url = checked_web_url("http:///path").unwrap();
        assert_eq!(url.host_str(), Some("path"));
        assert_eq!(url.as_str(), "http://path/");

        // Userinfo is kept visible in the parsed form rather than hidden.
        let tricky = checked_web_url("https://github.com@evil.example/").unwrap();
        assert_eq!(tricky.host_str(), Some("evil.example"));
    }

    #[test]
    fn an_address_with_whitespace_or_control_characters_is_refused() {
        // A newline or tab can change how a hand-off to the OS is parsed.
        for bad in [
            "https://example.com/a b",
            "https://example.com/\nfoo",
            "https://example.com/\u{0}",
            "https://example.com/\r\n",
            " https://example.com",
        ] {
            assert!(checked_web_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn message_kinds_parse_and_unknown_ones_are_errors() {
        assert_eq!(parse_kind("privmsg"), Ok(MessageKind::Privmsg));
        assert_eq!(parse_kind("notice"), Ok(MessageKind::Notice));
        assert_eq!(parse_kind("action"), Ok(MessageKind::Action));
        assert!(parse_kind("PRIVMSG").is_err());
        assert!(parse_kind("").is_err());
    }

    #[test]
    fn page_sizes_are_bounded() {
        assert_eq!(page_limit(0), 1);
        assert_eq!(page_limit(50), 50);
        assert_eq!(page_limit(usize::MAX), MAX_PAGE);
    }

    #[test]
    fn a_log_that_cannot_be_opened_falls_back_and_says_so() {
        let dir = std::env::temp_dir().join(format!("rhizome-app-openstore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A directory where the database file should be cannot be opened as one.
        let path = dir.join("rhizome.sqlite3");
        std::fs::create_dir_all(&path).unwrap();

        let mut notices = Vec::new();
        let _store = open_store(&path, &mut notices);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("this session only"));

        let mut none = Vec::new();
        let good = dir.join("ok.sqlite3");
        let _ = open_store(&good, &mut none);
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
