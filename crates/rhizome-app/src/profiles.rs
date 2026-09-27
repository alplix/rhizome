//! Saved connection settings.
//!
//! A profile is everything needed to reconnect to a network *except* the
//! password. Passwords are never written to disk here: a plain JSON file in the
//! user's profile directory is readable by any program they run, so the
//! password is asked for when connecting and lives only in memory.

use std::fs;
use std::path::{Path, PathBuf};

use crate::fsutil::write_atomic;

use rhizome_client::identity::ClientCert;
use rhizome_client::Config;
use serde::{Deserialize, Serialize};

const FILE_VERSION: u32 = 1;

/// Saved settings for one network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    /// A short stable label, used to key the log. Lowercase letters, digits,
    /// `-` and `_`.
    pub id: String,
    /// The name shown in the interface.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub nick: String,
    pub username: String,
    pub realname: String,
    #[serde(default)]
    pub channels: Vec<String>,
    /// The NickServ account to log in to with SASL `PLAIN`, or the account to
    /// act as with SASL `EXTERNAL` when it differs from the certificate's
    /// (usually left empty, and only consulted when `client_cert_path` is
    /// set — see there).
    #[serde(default)]
    pub sasl_account: Option<String>,
    /// A PEM file holding a client certificate and its private key, for SASL
    /// `EXTERNAL` — the "CertFP" a network's services usually document.
    /// Nothing is stored from it here, only this path; the file is read
    /// fresh on each connection attempt. Takes priority over `sasl_account`
    /// as a password-based `PLAIN` login: a profile with both set still logs
    /// in by certificate, not by password.
    #[serde(default)]
    pub client_cert_path: Option<String>,
    /// Connect to this network when the application starts. A network that
    /// needs a password only does so if one has been saved.
    #[serde(default)]
    pub autoconnect: bool,
}

fn has_bad_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl Profile {
    /// Checks that the profile could be used to connect, with a message a
    /// person can act on.
    pub fn validate(&self) -> Result<(), String> {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 32
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !id_ok {
            return Err(
                "the id must be 1-32 characters: lowercase letters, digits, '-' or '_'".into(),
            );
        }
        if self.host.is_empty()
            || self.host.len() > 253
            || has_bad_chars(&self.host)
            || self.host.contains('/')
        {
            return Err("the server address is not valid".into());
        }
        if self.port == 0 {
            return Err("the port must not be 0".into());
        }
        if self.nick.is_empty()
            || self.nick.len() > 64
            || has_bad_chars(&self.nick)
            || self.nick.starts_with(['#', '&', ':', '-'])
            || self.nick.starts_with(|c: char| c.is_ascii_digit())
        {
            return Err("the nick is not valid (no spaces, and it cannot start with a digit, '#', ':' or '-')".into());
        }
        if self.username.is_empty()
            || self.username.len() > 64
            || has_bad_chars(&self.username)
            || self.username.contains('@')
        {
            return Err("the username is not valid".into());
        }
        if self.realname.len() > 200 || self.realname.chars().any(char::is_control) {
            return Err("the real name is not valid".into());
        }
        for channel in &self.channels {
            if channel.is_empty() || has_bad_chars(channel) || channel.contains(',') {
                return Err(format!("the channel {channel:?} is not valid"));
            }
        }
        if let Some(account) = &self.sasl_account {
            if account.is_empty() || has_bad_chars(account) {
                return Err("the SASL account name is not valid".into());
            }
        }
        if let Some(path) = &self.client_cert_path {
            if path.is_empty() || has_bad_chars(path) {
                return Err("the client certificate path is not valid".into());
            }
        }
        Ok(())
    }

    /// Builds the engine's settings. A profile with a SASL account and no
    /// client certificate needs the password supplied here, since it is not
    /// stored; one with a client certificate needs no password at all, and
    /// the file is read fresh from `client_cert_path`.
    pub fn to_config(&self, sasl_password: Option<String>) -> Result<Config, String> {
        self.validate()?;
        let mut config = Config::new(&self.host, &self.nick)
            .port(self.port)
            .username(&self.username)
            .realname(&self.realname)
            .autojoin(self.channels.iter().cloned());
        config.tls = self.tls;
        if let Some(path) = &self.client_cert_path {
            let pem = fs::read(path)
                .map_err(|e| format!("could not read the client certificate at {path}: {e}"))?;
            let cert = ClientCert::from_pem(&pem)
                .map_err(|e| format!("the client certificate at {path} is not usable: {e}"))?;
            let authzid = self.sasl_account.clone().unwrap_or_default();
            config = config.sasl_external(cert, authzid);
        } else if let Some(account) = &self.sasl_account {
            match sasl_password {
                Some(password) if !password.is_empty() => {
                    config = config.sasl_plain(account, password);
                }
                _ => return Err(format!("the password for {account} is needed to log in")),
            }
        }
        Ok(config)
    }
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    profiles: Vec<Profile>,
}

/// The saved profiles, in a JSON file.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(path: impl Into<PathBuf>) -> ProfileStore {
        ProfileStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads every saved profile. A missing file is an empty list; a file that
    /// cannot be read is an error, *not* an empty list, so that a later save
    /// cannot silently replace what the person had.
    pub fn load(&self) -> Result<Vec<Profile>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("could not read {}: {e}", self.path.display())),
        };
        let file: FileFormat = serde_json::from_str(&text).map_err(|e| {
            format!(
                "{} is not a valid profile file ({e}); fix or remove it",
                self.path.display()
            )
        })?;
        if file.version > FILE_VERSION {
            return Err(format!(
                "{} was written by a newer version of Rhizome",
                self.path.display()
            ));
        }
        Ok(file.profiles)
    }

    fn save_all(&self, profiles: &[Profile]) -> Result<(), String> {
        let file = FileFormat {
            version: FILE_VERSION,
            profiles: profiles.to_vec(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        write_atomic(&self.path, &json)
    }

    /// Adds a profile or replaces the one with the same id.
    pub fn upsert(&self, profile: Profile) -> Result<(), String> {
        profile.validate()?;
        let mut profiles = self.load()?;
        match profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile,
            None => profiles.push(profile),
        }
        self.save_all(&profiles)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut profiles = self.load()?;
        profiles.retain(|p| p.id != id);
        self.save_all(&profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile {
            id: "libera".into(),
            name: "Libera.Chat".into(),
            host: "irc.libera.chat".into(),
            port: 6697,
            tls: true,
            nick: "alp".into(),
            username: "alp".into(),
            realname: "Alp".into(),
            channels: vec!["#rhizome".into()],
            sasl_account: None,
            client_cert_path: None,
            autoconnect: false,
        }
    }

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = std::env::temp_dir()
                .join(format!("rhizome-app-test-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Temp(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_reasonable_profile_is_valid() {
        assert_eq!(profile().validate(), Ok(()));
    }

    #[test]
    fn bad_fields_are_rejected_with_a_reason() {
        let cases: Vec<(&str, Profile)> = vec![
            (
                "empty id",
                Profile {
                    id: "".into(),
                    ..profile()
                },
            ),
            (
                "uppercase id",
                Profile {
                    id: "Libera".into(),
                    ..profile()
                },
            ),
            (
                "spacey id",
                Profile {
                    id: "my net".into(),
                    ..profile()
                },
            ),
            (
                "no host",
                Profile {
                    host: "".into(),
                    ..profile()
                },
            ),
            (
                "host with a path",
                Profile {
                    host: "irc.example/x".into(),
                    ..profile()
                },
            ),
            (
                "host with a space",
                Profile {
                    host: "irc example".into(),
                    ..profile()
                },
            ),
            (
                "port zero",
                Profile {
                    port: 0,
                    ..profile()
                },
            ),
            (
                "nick with a space",
                Profile {
                    nick: "a b".into(),
                    ..profile()
                },
            ),
            (
                "nick starting with a digit",
                Profile {
                    nick: "1abc".into(),
                    ..profile()
                },
            ),
            (
                "nick that looks like a channel",
                Profile {
                    nick: "#chan".into(),
                    ..profile()
                },
            ),
            (
                "nick with a line break",
                Profile {
                    nick: "a\nQUIT".into(),
                    ..profile()
                },
            ),
            (
                "username with @",
                Profile {
                    username: "a@b".into(),
                    ..profile()
                },
            ),
            (
                "channel with a comma",
                Profile {
                    channels: vec!["#a,#b".into()],
                    ..profile()
                },
            ),
            (
                "channel with a line break",
                Profile {
                    channels: vec!["#a\nQUIT".into()],
                    ..profile()
                },
            ),
            (
                "empty sasl account",
                Profile {
                    sasl_account: Some("".into()),
                    ..profile()
                },
            ),
            (
                "empty client certificate path",
                Profile {
                    client_cert_path: Some("".into()),
                    ..profile()
                },
            ),
            (
                "client certificate path with a line break",
                Profile {
                    client_cert_path: Some("a\nQUIT".into()),
                    ..profile()
                },
            ),
        ];
        for (label, p) in cases {
            assert!(p.validate().is_err(), "{label} should be rejected");
        }
    }

    #[test]
    fn the_config_carries_the_profile() {
        let mut p = profile();
        p.tls = false;
        p.port = 6667;
        let config = p.to_config(None).unwrap();
        assert_eq!(config.host, "irc.libera.chat");
        assert_eq!(config.port, 6667);
        assert!(!config.tls);
        assert_eq!(config.autojoin, vec!["#rhizome"]);
        assert!(config.sasl.is_empty());
    }

    #[test]
    fn a_sasl_profile_will_not_connect_without_its_password() {
        let mut p = profile();
        p.sasl_account = Some("alp".into());
        assert!(p.to_config(None).is_err());
        assert!(p.to_config(Some(String::new())).is_err());
        let config = p.to_config(Some("hunter2".into())).unwrap();
        assert_eq!(config.sasl.len(), 1);
        // And the password does not leak through Debug.
        assert!(!format!("{config:?}").contains("hunter2"));
    }

    /// A real certificate and key rhizome-client already generated for its
    /// own SASL EXTERNAL tests, reused here rather than keeping a second
    /// copy: see `crates/rhizome-client/testdata/README.md`.
    const TEST_CLIENT_CERT: &str = include_str!("../../rhizome-client/testdata/client.crt");
    const TEST_CLIENT_KEY: &str = include_str!("../../rhizome-client/testdata/client.key");

    #[test]
    fn a_client_certificate_logs_in_by_external_not_plain() {
        let t = Temp::new("client-cert");
        fs::create_dir_all(&t.0).unwrap();
        let pem_path = t.0.join("client.pem");
        fs::write(&pem_path, format!("{TEST_CLIENT_CERT}\n{TEST_CLIENT_KEY}")).unwrap();

        let mut p = profile();
        p.client_cert_path = Some(pem_path.display().to_string());
        // Needs no password at all, even with a NickServ account set too...
        let config = p.to_config(None).unwrap();
        assert!(config.client_cert.is_some());
        assert_eq!(
            config.sasl,
            vec![rhizome_proto::Mechanism::External {
                authzid: String::new()
            }]
        );

        // ...and a client certificate takes priority over a SASL account,
        // used here only as the authzid rather than for a PLAIN login.
        p.sasl_account = Some("services-account".into());
        let config = p.to_config(None).unwrap();
        assert_eq!(
            config.sasl,
            vec![rhizome_proto::Mechanism::External {
                authzid: "services-account".into()
            }]
        );
    }

    #[test]
    fn a_missing_or_unusable_certificate_file_is_a_clear_error() {
        let mut p = profile();
        p.client_cert_path = Some("/no/such/file-at-all.pem".into());
        let err = p.to_config(None).unwrap_err();
        assert!(err.contains("could not read"), "{err}");

        let t = Temp::new("bad-cert");
        fs::create_dir_all(&t.0).unwrap();
        let garbage_path = t.0.join("garbage.pem");
        fs::write(&garbage_path, "this is not a PEM file at all").unwrap();
        p.client_cert_path = Some(garbage_path.display().to_string());
        let err = p.to_config(None).unwrap_err();
        assert!(err.contains("not usable"), "{err}");
    }

    #[test]
    fn profiles_persist_and_replace_by_id() {
        let t = Temp::new("persist");
        let store = ProfileStore::new(t.0.join("profiles.json"));
        assert_eq!(store.load().unwrap(), vec![]);

        store.upsert(profile()).unwrap();
        let mut renamed = profile();
        renamed.nick = "alp2".into();
        store.upsert(renamed).unwrap();
        store
            .upsert(Profile {
                id: "oftc".into(),
                host: "irc.oftc.net".into(),
                ..profile()
            })
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded[0].nick, "alp2",
            "same id replaces rather than duplicates"
        );

        store.remove("libera").unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn a_password_never_reaches_the_file() {
        let t = Temp::new("nopassword");
        let store = ProfileStore::new(t.0.join("profiles.json"));
        let mut p = profile();
        p.sasl_account = Some("alp".into());
        store.upsert(p).unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("sasl_account"));
        assert!(!text.to_lowercase().contains("password"));
    }

    #[test]
    fn an_invalid_profile_is_not_saved() {
        let t = Temp::new("invalid");
        let store = ProfileStore::new(t.0.join("profiles.json"));
        assert!(store
            .upsert(Profile {
                nick: "a b".into(),
                ..profile()
            })
            .is_err());
        assert_eq!(store.load().unwrap(), vec![]);
    }

    #[test]
    fn a_corrupt_file_is_an_error_and_is_not_overwritten() {
        let t = Temp::new("corrupt");
        fs::create_dir_all(&t.0).unwrap();
        let path = t.0.join("profiles.json");
        fs::write(&path, "{ this is not json").unwrap();
        let store = ProfileStore::new(&path);

        assert!(store
            .load()
            .unwrap_err()
            .contains("not a valid profile file"));
        // A save must refuse, not replace what the person may want to recover.
        assert!(store.upsert(profile()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ this is not json");
    }

    #[test]
    fn a_file_from_a_newer_version_is_refused() {
        let t = Temp::new("newer");
        fs::create_dir_all(&t.0).unwrap();
        let path = t.0.join("profiles.json");
        fs::write(&path, r#"{"version": 99, "profiles": []}"#).unwrap();
        assert!(ProfileStore::new(&path)
            .load()
            .unwrap_err()
            .contains("newer"));
    }

    #[test]
    fn no_temporary_file_is_left_behind() {
        let t = Temp::new("tmp");
        let store = ProfileStore::new(t.0.join("profiles.json"));
        store.upsert(profile()).unwrap();
        let names: Vec<String> = fs::read_dir(&t.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["profiles.json"]);
    }

    #[test]
    fn autoconnect_is_saved_and_a_file_from_before_it_existed_still_loads() {
        let t = Temp::new("autoconnect");
        let store = ProfileStore::new(t.0.join("profiles.json"));
        store
            .upsert(Profile {
                autoconnect: true,
                ..profile()
            })
            .unwrap();
        assert!(store.load().unwrap()[0].autoconnect);

        // A file written before the field existed has none, which means off.
        fs::write(
            store.path(),
            r#"{"version":1,"profiles":[{"id":"old","name":"Old","host":"h","port":6667,"tls":false,
               "nick":"n","username":"u","realname":"r","channels":[],"sasl_account":null}]}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].autoconnect);
    }
}
