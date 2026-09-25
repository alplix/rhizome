//! The person's preferences: theme, language, and how much to show.
//!
//! Stored as a small JSON file. Two properties matter more than the file format:
//!
//! * **A bad value never costs the rest.** Each setting is read on its own and
//!   falls back to its default if it is missing or not something this version
//!   understands. A file written by a newer Rhizome (with a theme this one has
//!   not heard of) still keeps the other settings.
//! * **A file that is not JSON at all is set aside, not overwritten,** so it can
//!   be recovered by hand, and the application starts with defaults and says so.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fsutil::write_atomic;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the operating system's light or dark setting.
    #[default]
    System,
    Graphite,
    Midnight,
    Forest,
    Paper,
    Daylight,
    Contrast,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    /// Use the theme's own accent colour.
    #[default]
    Theme,
    Blue,
    Violet,
    Orange,
    Rose,
    Teal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontSize {
    Small,
    #[default]
    Medium,
    Large,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Use the operating system's language.
    #[default]
    Auto,
    En,
    Tr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeFormat {
    #[default]
    #[serde(rename = "24h")]
    H24,
    #[serde(rename = "12h")]
    H12,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub theme: Theme,
    pub accent: Accent,
    pub density: Density,
    pub font_size: FontSize,
    pub language: Language,
    pub time_format: TimeFormat,
    /// Show joins, parts, quits and topic changes among the messages.
    pub show_events: bool,
    /// Tell the desktop about mentions and private messages while the window is
    /// in the background.
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            theme: Theme::default(),
            accent: Accent::default(),
            density: Density::default(),
            font_size: FontSize::default(),
            language: Language::default(),
            time_format: TimeFormat::default(),
            show_events: true,
            notifications: true,
        }
    }
}

impl Settings {
    /// Reads settings from JSON, one field at a time. Anything missing or
    /// unrecognised takes its default, so this never fails.
    pub fn from_value(value: &Value) -> Settings {
        fn field<T: for<'de> Deserialize<'de> + Default>(value: &Value, name: &str) -> T {
            value
                .get(name)
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default()
        }
        fn flag(value: &Value, name: &str, default: bool) -> bool {
            value.get(name).and_then(Value::as_bool).unwrap_or(default)
        }
        Settings {
            theme: field(value, "theme"),
            accent: field(value, "accent"),
            density: field(value, "density"),
            font_size: field(value, "font_size"),
            language: field(value, "language"),
            time_format: field(value, "time_format"),
            show_events: flag(value, "show_events", true),
            notifications: flag(value, "notifications", true),
        }
    }
}

/// The settings file.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> SettingsStore {
        SettingsStore { path: path.into() }
    }

    /// Loads the settings. Returns them along with a message for the person if
    /// the file had to be set aside.
    pub fn load(&self) -> (Settings, Option<String>) {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Settings::default(), None)
            }
            Err(e) => {
                return (
                    Settings::default(),
                    Some(format!(
                        "Could not read {} ({e}); using default settings.",
                        self.path.display()
                    )),
                )
            }
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(value) if value.is_object() => (Settings::from_value(&value), None),
            _ => {
                let mut backup = self.path.as_os_str().to_owned();
                backup.push(".bad");
                let note = match fs::rename(&self.path, PathBuf::from(&backup)) {
                    Ok(()) => format!(
                        "The settings file was not readable, so it was set aside as {} and defaults are in use.",
                        PathBuf::from(&backup).display()
                    ),
                    Err(e) => format!("The settings file was not readable ({e}); defaults are in use."),
                };
                (Settings::default(), Some(note))
            }
        }
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
        write_atomic(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Temp {
            let d = std::env::temp_dir()
                .join(format!("rhizome-settings-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            Temp(d)
        }
        fn file(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_are_sensible() {
        let s = Settings::default();
        assert_eq!(s.theme, Theme::System);
        assert_eq!(s.language, Language::Auto);
        assert!(s.show_events && s.notifications);
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let t = Temp::new("roundtrip");
        let store = SettingsStore::new(t.file());
        let chosen = Settings {
            theme: Theme::Midnight,
            accent: Accent::Rose,
            density: Density::Compact,
            font_size: FontSize::Large,
            language: Language::Tr,
            time_format: TimeFormat::H12,
            show_events: false,
            notifications: false,
        };
        store.save(&chosen).unwrap();
        assert_eq!(store.load(), (chosen, None));
    }

    #[test]
    fn the_file_uses_plain_names_a_person_could_edit() {
        let t = Temp::new("names");
        let store = SettingsStore::new(t.file());
        store
            .save(&Settings {
                time_format: TimeFormat::H12,
                theme: Theme::Forest,
                ..Settings::default()
            })
            .unwrap();
        let text = fs::read_to_string(t.file()).unwrap();
        assert!(text.contains("\"theme\": \"forest\""), "{text}");
        assert!(text.contains("\"time_format\": \"12h\""), "{text}");
    }

    #[test]
    fn a_missing_file_gives_defaults_without_a_warning() {
        let t = Temp::new("missing");
        assert_eq!(
            SettingsStore::new(t.file()).load(),
            (Settings::default(), None)
        );
    }

    #[test]
    fn one_bad_value_does_not_cost_the_others() {
        let s = Settings::from_value(&json!({
            "theme": "a-theme-from-the-future",
            "accent": "blue",
            "density": 7,
            "font_size": "large",
            "language": "tr",
            "show_events": "yes",
            "notifications": false,
        }));
        assert_eq!(s.theme, Theme::System, "unknown theme falls back");
        assert_eq!(s.accent, Accent::Blue, "the rest survive");
        assert_eq!(s.density, Density::Comfortable, "wrong type falls back");
        assert_eq!(s.font_size, FontSize::Large);
        assert_eq!(s.language, Language::Tr);
        assert!(s.show_events, "a non-boolean takes the default");
        assert!(!s.notifications);
    }

    #[test]
    fn unknown_fields_from_a_newer_version_are_ignored() {
        let s = Settings::from_value(&json!({"theme": "paper", "some_new_option": {"x": 1}}));
        assert_eq!(s.theme, Theme::Paper);
    }

    #[test]
    fn a_file_that_is_not_json_is_set_aside_and_reported() {
        let t = Temp::new("corrupt");
        fs::write(t.file(), "{ definitely not json").unwrap();
        let (settings, note) = SettingsStore::new(t.file()).load();
        assert_eq!(settings, Settings::default());
        let note = note.expect("the person is told");
        assert!(note.contains("set aside"), "{note}");
        assert!(!t.file().exists(), "the bad file is moved out of the way");
        assert_eq!(
            fs::read_to_string(t.0.join("settings.json.bad")).unwrap(),
            "{ definitely not json",
            "and kept, so it can be recovered"
        );
    }

    #[test]
    fn valid_json_of_the_wrong_shape_is_treated_the_same() {
        let t = Temp::new("shape");
        fs::write(t.file(), "[1, 2, 3]").unwrap();
        let (settings, note) = SettingsStore::new(t.file()).load();
        assert_eq!(settings, Settings::default());
        assert!(note.is_some());
    }

    #[test]
    fn saving_after_a_reset_writes_a_fresh_file() {
        let t = Temp::new("after-reset");
        fs::write(t.file(), "garbage").unwrap();
        let store = SettingsStore::new(t.file());
        let (defaults, _) = store.load();
        store
            .save(&Settings {
                theme: Theme::Contrast,
                ..defaults
            })
            .unwrap();
        assert_eq!(store.load().0.theme, Theme::Contrast);
    }
}
