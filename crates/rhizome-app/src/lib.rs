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
//! * the window's capabilities are limited to listening for the backend's events,
//!   calling the application's own commands and showing notifications: no
//!   filesystem, shell or network access of its own;
//! * a remembered password lives in the operating system's credential store,
//!   never in a file of ours, and is discarded the moment a server refuses it.

pub mod core;
pub mod dto;
pub mod fsutil;
pub mod profiles;
pub mod secrets;
pub mod settings;
pub mod storehost;

use std::sync::Arc;

use rhizome_client::MessageKind;
use rhizome_store::Store;
use serde::Serialize;
use tauri::{Emitter, Manager, State, Url};
use tokio::sync::mpsc;

use crate::core::Core;
use crate::dto::{UiBuffer, UiCursor, UiHit, UiMessage};
use crate::profiles::{Profile, ProfileStore};
use crate::secrets::{SecretStore, SystemSecrets};
use crate::settings::{Settings, SettingsStore};
use crate::storehost::StoreHost;

/// The event name the interface listens on.
const EVENT_NAME: &str = "rhizome://event";

/// The most a page of history or a set of search results may hold. The
/// interface asks for less; this only bounds a misbehaving caller.
const MAX_PAGE: usize = 500;

/// How the application is named in the operating system's credential store.
const CREDENTIAL_SERVICE: &str = "Rhizome";

struct AppState {
    core: Core,
    profiles: ProfileStore,
    settings: SettingsStore,
    data_dir: String,
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

/// What connecting with a profile needs, decided from what the person typed and
/// what has been remembered.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The password to log in with, if the profile logs in at all.
    pub password: Option<String>,
    /// A problem worth telling the person about that does not stop connecting,
    /// such as the password could not be remembered.
    pub warning: Option<String>,
}

/// Works out the password for a connection and keeps the credential store in
/// step with what the person asked for.
///
/// * A profile with no SASL account needs no password.
/// * A password typed now is used. If "remember" was ticked it is saved; if it
///   was not, any older saved one is removed, because unticking the box means
///   "do not keep this".
/// * With nothing typed, the remembered password is used, and it is an error if
///   there is none.
pub fn resolve_password(
    secrets: &dyn SecretStore,
    profile: &Profile,
    typed: Option<String>,
    remember: bool,
) -> Result<Resolved, String> {
    let Some(account) = &profile.sasl_account else {
        return Ok(Resolved {
            password: None,
            warning: None,
        });
    };
    match typed.filter(|p| !p.is_empty()) {
        Some(password) => {
            let outcome = if remember {
                secrets.set(&profile.id, &password)
            } else {
                secrets.delete(&profile.id)
            };
            Ok(Resolved {
                password: Some(password),
                warning: outcome.err(),
            })
        }
        None => match secrets.get(&profile.id)? {
            Some(password) => Ok(Resolved {
                password: Some(password),
                warning: None,
            }),
            None => Err(format!("the password for {account} is needed to log in")),
        },
    }
}

// ---- commands ------------------------------------------------------------

#[tauri::command]
fn startup_notices(state: State<'_, AppState>) -> Vec<String> {
    state.notices.clone()
}

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    license: &'static str,
    data_dir: String,
}

#[tauri::command]
fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        name: "Rhizome",
        version: env!("CARGO_PKG_VERSION"),
        license: "GPL-3.0-or-later",
        data_dir: state.data_dir.clone(),
    }
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.load().0
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    state.settings.save(&settings)
}

#[tauri::command]
fn list_profiles(state: State<'_, AppState>) -> Result<Vec<Profile>, String> {
    state.profiles.load()
}

#[tauri::command]
fn save_profile(state: State<'_, AppState>, profile: Profile) -> Result<(), String> {
    // A remembered password belongs to an account. If the account changes, or
    // login is turned off, the old password no longer means anything, and
    // keeping it around would offer it to the wrong account.
    let previous = state
        .profiles
        .load()?
        .into_iter()
        .find(|p| p.id == profile.id);
    let account_changed = previous.is_some_and(|p| p.sasl_account != profile.sasl_account);
    state.profiles.upsert(profile.clone())?;
    if account_changed {
        state.core.forget_password(&profile.id)?;
    }
    Ok(())
}

#[tauri::command]
fn delete_profile(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.profiles.remove(&id)?;
    state.core.forget_password(&id)
}

/// Whether a password is remembered for this network, without revealing it.
#[tauri::command]
fn has_saved_password(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    Ok(state.core.saved_password(&id)?.is_some())
}

#[tauri::command]
fn forget_password(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.core.forget_password(&id)
}

/// Connects using a saved profile.
///
/// `sasl_password` is what the person typed, if anything; `remember` asks for it
/// to be kept in the system credential store.
#[tauri::command]
async fn connect(
    state: State<'_, AppState>,
    id: String,
    sasl_password: Option<String>,
    remember: Option<bool>,
) -> Result<(), String> {
    let profile = state
        .profiles
        .load()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("there is no saved network called {id}"))?;
    let secrets = SecretsView(&state.core);
    let resolved = resolve_password(&secrets, &profile, sasl_password, remember.unwrap_or(false))?;
    if let Some(warning) = resolved.warning {
        state.core.report(warning);
    }
    state
        .core
        .connect(&id, profile.to_config(resolved.password)?)
}

/// Lets [`resolve_password`] work through the core's view of the credential
/// store, so there is one place the store is reached from.
struct SecretsView<'a>(&'a Core);

impl SecretStore for SecretsView<'_> {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.0.saved_password(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.0.remember_password(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.forget_password(key)
    }
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

/// Records that a conversation has been read up to a time.
#[tauri::command]
fn mark_read(state: State<'_, AppState>, network: String, buffer: String, time_ms: i64) {
    state.core.mark_read(&network, &buffer, time_ms);
}

/// Deletes a conversation's history from the log.
#[tauri::command]
async fn clear_history(
    state: State<'_, AppState>,
    network: String,
    buffer: String,
) -> Result<usize, String> {
    state.core.clear_history(&network, &buffer).await
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

/// Brings the existing window forward when a second copy is started.
#[cfg(desktop)]
fn focus_existing_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Starts the application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // Two copies would both connect to the same networks and write the same log.
    // This must be the first plugin registered.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        focus_existing_window(app);
    }));
    // Remember where the window was and how big it was.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_window_state::Builder::default().build());

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let mut notices = Vec::new();

            let (_, settings_note) = SettingsStore::new(dir.join("settings.json")).load();
            notices.extend(settings_note);

            // Failures on the store's thread are queued here and forwarded to
            // the interface once the core exists.
            let (failure_tx, mut failures) = mpsc::unbounded_channel::<String>();
            let store = StoreHost::spawn(
                open_store(&dir.join("rhizome.sqlite3"), &mut notices),
                Box::new(move |failure| {
                    let _ = failure_tx.send(failure);
                }),
            );
            let secrets: Arc<dyn SecretStore> = Arc::new(SystemSecrets::new(CREDENTIAL_SERVICE));
            let (core, mut events) = Core::new(store, secrets);

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
                settings: SettingsStore::new(dir.join("settings.json")),
                data_dir: dir.display().to_string(),
                notices,
            });

            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Rhizome")
            .inner_size(1200.0, 780.0)
            .min_inner_size(720.0, 480.0)
            .on_navigation(is_app_url)
            .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup_notices,
            app_info,
            get_settings,
            save_settings,
            list_profiles,
            save_profile,
            delete_profile,
            has_saved_password,
            forget_password,
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
            mark_read,
            clear_history,
            open_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Rhizome application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;

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
            assert!(
                !is_app_url(&url(hostile)),
                "{hostile} must not be navigable"
            );
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
        let dir =
            std::env::temp_dir().join(format!("rhizome-app-openstore-{}", std::process::id()));
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

    // ---- which password to connect with ----------------------------------------

    fn profile(account: Option<&str>) -> Profile {
        Profile {
            id: "libera".into(),
            name: "Libera".into(),
            host: "irc.libera.chat".into(),
            port: 6697,
            tls: true,
            nick: "alp".into(),
            username: "alp".into(),
            realname: "Alp".into(),
            channels: vec![],
            sasl_account: account.map(str::to_owned),
            autoconnect: false,
        }
    }

    #[test]
    fn a_profile_without_an_account_needs_no_password_and_touches_no_secrets() {
        let secrets = MemorySecrets::default();
        secrets.set("libera", "leftover").unwrap();
        let r = resolve_password(&secrets, &profile(None), Some("typed".into()), true).unwrap();
        assert_eq!(
            r,
            Resolved {
                password: None,
                warning: None
            }
        );
        assert_eq!(secrets.get("libera").unwrap().as_deref(), Some("leftover"));
    }

    #[test]
    fn a_typed_password_is_used_and_kept_only_if_asked() {
        let secrets = MemorySecrets::default();
        let r = resolve_password(
            &secrets,
            &profile(Some("alp")),
            Some("hunter2".into()),
            true,
        )
        .unwrap();
        assert_eq!(r.password.as_deref(), Some("hunter2"));
        assert_eq!(secrets.get("libera").unwrap().as_deref(), Some("hunter2"));

        // Same again without "remember": it is used, and the old one is dropped.
        let r =
            resolve_password(&secrets, &profile(Some("alp")), Some("other".into()), false).unwrap();
        assert_eq!(r.password.as_deref(), Some("other"));
        assert_eq!(
            secrets.get("libera").unwrap(),
            None,
            "unticking remember means do not keep"
        );
    }

    #[test]
    fn with_nothing_typed_the_remembered_password_is_used() {
        let secrets = MemorySecrets::default();
        secrets.set("libera", "remembered").unwrap();
        for typed in [None, Some(String::new())] {
            let r = resolve_password(&secrets, &profile(Some("alp")), typed, false).unwrap();
            assert_eq!(r.password.as_deref(), Some("remembered"));
        }
        assert_eq!(
            secrets.get("libera").unwrap().as_deref(),
            Some("remembered"),
            "using it does not delete it"
        );
    }

    #[test]
    fn with_nothing_typed_and_nothing_remembered_it_is_an_error_that_names_the_account() {
        let err = resolve_password(&MemorySecrets::default(), &profile(Some("alp")), None, true)
            .unwrap_err();
        assert!(err.contains("alp"), "{err}");
    }

    #[test]
    fn failing_to_remember_a_password_does_not_stop_the_connection() {
        struct Broken;
        impl SecretStore for Broken {
            fn get(&self, _: &str) -> Result<Option<String>, String> {
                Err("locked".into())
            }
            fn set(&self, _: &str, _: &str) -> Result<(), String> {
                Err("the store is locked".into())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Err("the store is locked".into())
            }
        }
        let r = resolve_password(&Broken, &profile(Some("alp")), Some("pw".into()), true).unwrap();
        assert_eq!(
            r.password.as_deref(),
            Some("pw"),
            "the typed password still works"
        );
        assert!(r.warning.unwrap().contains("locked"));

        // But with nothing typed, an unreadable store is a real problem.
        assert!(resolve_password(&Broken, &profile(Some("alp")), None, false).is_err());
    }
}
