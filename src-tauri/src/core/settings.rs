//! User settings and their tolerant on-disk form.

use std::io;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
#[cfg(test)]
use ts_rs::TS;

pub use crate::core::rules::Limits;

/// An OS binding saved before Force Super+V replaced it, so Reset to default
/// can restore it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct SavedBinding {
    pub schema: String,
    pub key: String,
    pub original: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct Settings {
    pub limits: Limits,
    pub paused: bool,
    pub excluded_apps: Vec<String>,
    pub shortcut: String,
    pub force_super_v: bool,
    pub auto_paste: bool,
    pub first_run_done: bool,
    pub saved_bindings: Vec<SavedBinding>,
    /// Start at login was turned on once (its default); from then on the OS
    /// entry itself is the user's choice.
    pub autostart_default_applied: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            paused: false,
            excluded_apps: Vec::new(),
            shortcut: default_shortcut(std::env::consts::OS).into(),
            force_super_v: false,
            auto_paste: true,
            first_run_done: false,
            saved_bindings: Vec::new(),
            autostart_default_applied: false,
        }
    }
}

/// The shortcut a new install gets. On macOS Super is Cmd and Cmd+V is the
/// system paste, so it is Ctrl+Cmd+V there.
pub fn default_shortcut(os: &str) -> &'static str {
    if os == "macos" { "Control+Super+V" } else { "Super+V" }
}

/// Decode one field; a missing key or a wrong type gives `default`.
fn field<T: DeserializeOwned>(map: &Map<String, Value>, key: &str, default: T) -> T {
    match map.get(key) {
        None => default,
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            eprintln!("settings: ignoring invalid `{key}`: {e}");
            default
        }),
    }
}

fn limits_from(value: Option<&Value>) -> Limits {
    let d = Limits::default();
    let Some(Value::Object(map)) = value else {
        if value.is_some() {
            eprintln!("settings: ignoring invalid `limits`");
        }
        return d;
    };
    Limits {
        max_items: field(map, "maxItems", d.max_items),
        max_age_days: field(map, "maxAgeDays", d.max_age_days),
        max_item_bytes: field(map, "maxItemBytes", d.max_item_bytes),
    }
}

fn from_value(value: Value) -> Settings {
    let Value::Object(map) = value else {
        eprintln!("settings: not a JSON object, using defaults");
        return Settings::default();
    };
    let d = Settings::default();
    Settings {
        limits: limits_from(map.get("limits")),
        paused: field(&map, "paused", d.paused),
        excluded_apps: field(&map, "excludedApps", d.excluded_apps),
        shortcut: field(&map, "shortcut", d.shortcut),
        force_super_v: field(&map, "forceSuperV", d.force_super_v),
        auto_paste: field(&map, "autoPaste", d.auto_paste),
        first_run_done: field(&map, "firstRunDone", d.first_run_done),
        saved_bindings: field(&map, "savedBindings", d.saved_bindings),
        autostart_default_applied: field(&map, "autostartDefaultApplied", d.autostart_default_applied),
    }
}

/// Load settings, never failing: a missing or unreadable file, invalid JSON
/// or a non-object gives defaults; a bad field falls back on its own.
pub fn load(path: &Path) -> Settings {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                eprintln!("settings: cannot read {}: {e}", path.display());
            }
            return Settings::default();
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => from_value(v),
        Err(e) => {
            eprintln!("settings: invalid JSON in {}: {e}", path.display());
            Settings::default()
        }
    }
}

/// Write to `*.tmp`, then rename over the target.
pub fn save(path: &Path, s: &Settings) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(s).map_err(io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clipboard-history-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn macos_default_shortcut_is_ctrl_cmd_v() {
        assert_eq!(default_shortcut("macos"), "Control+Super+V");
        assert_eq!(default_shortcut("linux"), "Super+V");
        assert_eq!(default_shortcut("windows"), "Super+V");
    }

    #[test]
    fn missing_file_gives_defaults() {
        let p = temp_path("missing");
        let s = load(&p);
        assert_eq!(s.limits.max_items, 100);
        assert_eq!(s.shortcut, default_shortcut(std::env::consts::OS));
        assert!(s.auto_paste && !s.force_super_v && !s.paused);
        assert!(!s.autostart_default_applied);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn old_file_without_new_fields_loads() {
        let p = temp_path("old");
        std::fs::write(&p, r#"{"paused":true}"#).unwrap();
        let s = load(&p);
        assert!(s.paused);
        assert_eq!(s, Settings { paused: true, ..Settings::default() });
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn wrong_type_field_falls_back_only_that_field() {
        let p = temp_path("wrong");
        std::fs::write(&p, r#"{"paused":"yes","shortcut":"Ctrl+Alt+H","limits":{"maxItems":"x","maxAgeDays":7}}"#).unwrap();
        let s = load(&p);
        assert!(!s.paused);
        assert_eq!(s.shortcut, "Ctrl+Alt+H");
        assert_eq!(s.limits.max_items, 100);
        assert_eq!(s.limits.max_age_days, Some(7));
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_json_or_non_object_gives_defaults() {
        let p = temp_path("invalid");
        std::fs::write(&p, "{nope").unwrap();
        assert_eq!(load(&p), Settings::default());
        std::fs::write(&p, "[1,2]").unwrap();
        assert_eq!(load(&p), Settings::default());
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_then_load_round_trips() {
        let p = temp_path("roundtrip");
        let s = Settings {
            limits: Limits { max_items: 500, max_age_days: Some(30), max_item_bytes: 1024 },
            paused: true,
            excluded_apps: vec!["keepassxc".into()],
            shortcut: "Ctrl+Shift+V".into(),
            force_super_v: true,
            auto_paste: false,
            first_run_done: true,
            saved_bindings: vec![SavedBinding { schema: "org.gnome.a".into(), key: "k".into(), original: "['<Super>v']".into() }],
            autostart_default_applied: true,
        };
        save(&p, &s).unwrap();
        assert!(!p.with_extension("tmp").exists());
        assert_eq!(load(&p), s);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
}
