//! GNOME shortcut registration through the system `gsettings` binary.
//!
//! GNOME grabs Super+V itself (`toggle-message-tray`), so on GNOME the shortcut
//! is a custom keybinding that runs `<this app> --toggle`, forwarded to the
//! running instance by tauri-plugin-single-instance. "Force Super+V" frees the
//! combo from every other GNOME binding first, saving each original so Reset
//! to default can put it back exactly.

use crate::core::settings::{SavedBinding, Settings};
use crate::platform::{BackendError, ForceReport, ShortcutRegistrar};
use std::process::Command;

const SHELL_KEYBINDINGS: &str = "org.gnome.shell.keybindings";
const WM_KEYBINDINGS: &str = "org.gnome.desktop.wm.keybindings";
const MUTTER_KEYBINDINGS: &str = "org.gnome.mutter.keybindings";
const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_LIST_KEY: &str = "custom-keybindings";
const CUSTOM_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
/// Where our own custom keybinding lives.
pub const OUR_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipboard-history/";

/// Called with the settings (originals recorded) before any GNOME setting is written.
pub type Persist = Box<dyn Fn(&Settings) -> std::io::Result<()> + Send + Sync>;

/// Parses a GVariant string array as `gsettings get` prints it: `['a', 'b']` or `@as []`.
pub fn parse_strv(s: &str) -> Vec<String> {
    let s = s.trim();
    let s = s.strip_prefix("@as").unwrap_or(s).trim();
    let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) else { return Vec::new() };
    let mut out = Vec::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let mut item = String::new();
        while let Some(d) = chars.next() {
            match d {
                '\\' => item.extend(chars.next()),
                d if d == c => break,
                d => item.push(d),
            }
        }
        out.push(item);
    }
    out
}

/// Escapes and single-quotes `s` as a GVariant string.
pub fn gvariant_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The GVariant text of a string array; an empty one needs its type (`@as []`).
pub fn format_strv(v: &[String]) -> String {
    if v.is_empty() {
        return "@as []".into();
    }
    format!("[{}]", v.iter().map(|s| gvariant_quote(s)).collect::<Vec<_>>().join(", "))
}

/// Whether `accel` is exactly Super+V: only the Super modifier (any case) and the key `v`.
fn is_super_v(accel: &str) -> bool {
    let mut rest = accel.trim();
    let mut mods = Vec::new();
    while let Some(r) = rest.strip_prefix('<') {
        let Some((m, after)) = r.split_once('>') else { return false };
        mods.push(m.to_ascii_lowercase());
        rest = after;
    }
    !mods.is_empty() && mods.iter().all(|m| m == "super") && rest.eq_ignore_ascii_case("v")
}

/// `v` without Super+V.
pub fn remove_super_v(v: &[String]) -> Vec<String> {
    v.iter().filter(|a| !is_super_v(a)).cloned().collect()
}

/// `existing` plus `path`, unless already there.
pub fn add_custom_binding_path(existing: &[String], path: &str) -> Vec<String> {
    let mut out = existing.to_vec();
    if !out.iter().any(|p| p == path) {
        out.push(path.to_string());
    }
    out
}

/// "Super+V" -> "<Super>v". Modifiers Super/Ctrl/Alt/Shift, then one letter,
/// digit or F1-F24. A shortcut needs at least one modifier.
pub fn to_gnome_accel(combo: &str) -> Option<String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last()?;
    if mods.is_empty() {
        return None;
    }
    let mut out = String::new();
    for m in mods {
        out.push_str(match m.to_ascii_lowercase().as_str() {
            "super" => "<Super>",
            "ctrl" | "control" => "<Control>",
            "alt" => "<Alt>",
            "shift" => "<Shift>",
            _ => return None,
        });
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => out.push(c.to_ascii_lowercase()),
        (Some('F' | 'f'), Some(_)) => {
            let n: u8 = key[1..].parse().ok().filter(|n| (1..=24).contains(n))?;
            out.push_str(&format!("F{n}"));
        }
        _ => return None,
    }
    Some(out)
}

/// Single-quotes `s` for a POSIX-style shell when it needs it (GNOME splits the command like a shell).
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@:,=%".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The command GNOME runs on the shortcut.
pub fn toggle_command(exe: &str) -> String {
    format!("{} --toggle", shell_quote(exe))
}

/// `gsettings`, as far as we need it. Faked in tests.
pub trait GSettings: Send + Sync {
    /// GVariant text of the value. `schema` may carry a path (`schema:/path/`).
    fn get(&self, schema: &str, key: &str) -> Result<String, BackendError>;
    fn set(&self, schema: &str, key: &str, value: &str) -> Result<(), BackendError>;
    fn reset(&self, schema: &str, key: &str) -> Result<(), BackendError>;
    fn list_keys(&self, schema: &str) -> Result<Vec<String>, BackendError>;
}

/// The system `gsettings` binary. `env` is added to its environment: tests pass
/// `GSETTINGS_BACKEND=memory` so nothing reaches the real session's dconf.
#[derive(Debug, Clone, Default)]
pub struct SystemGSettings {
    env: Vec<(String, String)>,
}

impl SystemGSettings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_env(env: Vec<(String, String)>) -> Self {
        Self { env }
    }

    fn run(&self, args: &[&str]) -> Result<String, BackendError> {
        let out = Command::new("gsettings")
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .output()
            .map_err(|e| BackendError(format!("cannot run gsettings: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(BackendError(format!("gsettings {} failed: {}", args.join(" "), err.trim())));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl GSettings for SystemGSettings {
    fn get(&self, schema: &str, key: &str) -> Result<String, BackendError> {
        self.run(&["get", schema, key])
    }
    fn set(&self, schema: &str, key: &str, value: &str) -> Result<(), BackendError> {
        self.run(&["set", schema, key, value]).map(drop)
    }
    fn reset(&self, schema: &str, key: &str) -> Result<(), BackendError> {
        self.run(&["reset", schema, key]).map(drop)
    }
    fn list_keys(&self, schema: &str) -> Result<Vec<String>, BackendError> {
        Ok(self.run(&["list-keys", schema])?.lines().map(str::to_string).collect())
    }
}

/// The GNOME registrar. `exe` is the path the keybinding will run, or why
/// there is none; `register` then fails while `force_super_v` and `restore` still work.
pub struct GnomeRegistrar<G: GSettings> {
    gs: G,
    exe: Result<String, String>,
    persist: Persist,
}

fn our_schema() -> String {
    format!("{CUSTOM_SCHEMA}:{OUR_PATH}")
}

impl<G: GSettings> GnomeRegistrar<G> {
    pub fn new(gs: G, exe: Result<String, String>, persist: Persist) -> Self {
        Self { gs, exe, persist }
    }

    /// The custom keybinding paths. An error must stop any write of the list:
    /// treating "unreadable" as "empty" would erase the user's other keybindings.
    fn custom_paths(&self) -> Result<Vec<String>, BackendError> {
        self.gs.get(MEDIA_KEYS, CUSTOM_LIST_KEY).map(|s| parse_strv(&s))
    }

    /// Every binding of Super+V outside our own: (what to show, saved original, new value).
    fn find_super_v(&self) -> Vec<(String, SavedBinding, String)> {
        let mut found = Vec::new();
        for schema in [SHELL_KEYBINDINGS, WM_KEYBINDINGS, MUTTER_KEYBINDINGS] {
            // A missing schema (other GNOME versions) is not an error.
            let Ok(keys) = self.gs.list_keys(schema) else { continue };
            for key in keys {
                let Ok(value) = self.gs.get(schema, &key) else { continue };
                if !value.trim_start().starts_with(['[', '@']) {
                    continue;
                }
                let accels = parse_strv(&value);
                let kept = remove_super_v(&accels);
                if kept.len() != accels.len() {
                    let saved = SavedBinding { schema: schema.into(), key: key.clone(), original: value };
                    found.push((format!("{schema} {key}"), saved, format_strv(&kept)));
                }
            }
        }
        // An unreadable list finds nothing here; nothing is written from an empty list.
        for path in self.custom_paths().unwrap_or_default().into_iter().filter(|p| p != OUR_PATH) {
            let schema = format!("{CUSTOM_SCHEMA}:{path}");
            let Ok(value) = self.gs.get(&schema, "binding") else { continue };
            if parse_strv(&format!("[{value}]")).first().is_some_and(|a| is_super_v(a)) {
                let name = self.gs.get(&schema, "name").ok().and_then(|n| parse_strv(&format!("[{n}]")).pop());
                let label = format!("custom shortcut \"{}\"", name.as_deref().unwrap_or(&path));
                found.push((label, SavedBinding { schema, key: "binding".into(), original: value }, "''".into()));
            }
        }
        found
    }
}

impl<G: GSettings> ShortcutRegistrar for GnomeRegistrar<G> {
    fn register(&self, combo: &str) -> Result<(), BackendError> {
        let accel = to_gnome_accel(combo).ok_or_else(|| BackendError(format!("cannot use \"{combo}\" as a GNOME shortcut")))?;
        let exe = self.exe.as_ref().map_err(|e| BackendError(e.clone()))?;
        // Read the list before writing anything: if it fails, nothing changes.
        let paths = add_custom_binding_path(&self.custom_paths()?, OUR_PATH);
        let schema = our_schema();
        self.gs.set(&schema, "name", &gvariant_quote("Clipboard History"))?;
        self.gs.set(&schema, "command", &gvariant_quote(&toggle_command(exe)))?;
        self.gs.set(&schema, "binding", &gvariant_quote(&accel))?;
        self.gs.set(MEDIA_KEYS, CUSTOM_LIST_KEY, &format_strv(&paths))
    }

    fn force_super_v(&self, settings: &mut Settings) -> Result<ForceReport, BackendError> {
        let found = self.find_super_v();
        let before = settings.saved_bindings.len();
        let mut freed_from = Vec::new();
        let mut writes = Vec::new();
        for (label, saved, new_value) in found {
            // An earlier force already saved the true original; never overwrite it.
            if !settings.saved_bindings.iter().any(|b| b.schema == saved.schema && b.key == saved.key) {
                settings.saved_bindings.push(saved.clone());
            }
            freed_from.push(label);
            writes.push((saved, new_value));
        }
        if writes.is_empty() {
            return Ok(ForceReport { freed_from });
        }
        // Originals reach the disk before GNOME changes, so a crash can always be undone.
        if let Err(e) = (self.persist)(settings) {
            settings.saved_bindings.truncate(before);
            return Err(BackendError(format!("cannot save settings: {e}")));
        }
        for (saved, new_value) in writes {
            self.gs.set(&saved.schema, &saved.key, &new_value)?;
        }
        Ok(ForceReport { freed_from })
    }

    fn restore(&self, settings: &mut Settings) -> Result<(), BackendError> {
        let mut failed = Vec::new();
        let mut first_error = None;
        for saved in std::mem::take(&mut settings.saved_bindings) {
            if let Err(e) = self.gs.set(&saved.schema, &saved.key, &saved.original) {
                first_error.get_or_insert(e);
                failed.push(saved);
            }
        }
        settings.saved_bindings = failed;
        if let Some(e) = first_error {
            // Keep the unrestored entries on disk so Reset can be tried again.
            let _ = (self.persist)(settings);
            return Err(e);
        }
        // Unreadable list: leave it alone (never write from an empty guess).
        let paths = self.custom_paths().unwrap_or_default();
        let kept: Vec<String> = paths.iter().filter(|p| *p != OUR_PATH).cloned().collect();
        if kept.len() != paths.len() {
            self.gs.set(MEDIA_KEYS, CUSTOM_LIST_KEY, &format_strv(&kept))?;
        }
        let schema = our_schema();
        for key in ["name", "command", "binding"] {
            self.gs.reset(&schema, key)?;
        }
        (self.persist)(settings).map_err(|e| BackendError(format!("cannot save settings: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::{SavedBinding, Settings};
    use crate::platform::{BackendError, ShortcutRegistrar};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parses_and_formats_strv() {
        assert_eq!(parse_strv("['<Super>v', '<Super>m']"), v(&["<Super>v", "<Super>m"]));
        assert_eq!(format_strv(&v(&["<Super>v", "<Super>m"])), "['<Super>v', '<Super>m']");
        assert_eq!(parse_strv("@as []"), Vec::<String>::new());
        assert_eq!(parse_strv("[]"), Vec::<String>::new());
        assert_eq!(format_strv(&[]), "@as []");
        assert_eq!(parse_strv(&format_strv(&v(&["a'b", "c\\d"]))), v(&["a'b", "c\\d"]));
    }

    #[test]
    fn remove_super_v_keeps_super_m() {
        assert_eq!(remove_super_v(&v(&["<Super>v", "<Super>m"])), v(&["<Super>m"]));
        assert_eq!(remove_super_v(&v(&["<Super><Shift>v"])), v(&["<Super><Shift>v"]));
    }

    #[test]
    fn remove_super_v_is_case_insensitive() {
        assert_eq!(remove_super_v(&v(&["<Super>V", "<super>v", "<SUPER>v", "<Super>m"])), v(&["<Super>m"]));
    }

    #[test]
    fn add_custom_binding_path_no_duplicates() {
        let once = add_custom_binding_path(&v(&["/a/"]), "/b/");
        assert_eq!(once, v(&["/a/", "/b/"]));
        assert_eq!(add_custom_binding_path(&once, "/b/"), once);
        assert_eq!(add_custom_binding_path(&[], "/b/"), v(&["/b/"]));
    }

    #[test]
    fn accelerators_translate_to_gnome_syntax() {
        assert_eq!(to_gnome_accel("Super+V").as_deref(), Some("<Super>v"));
        assert_eq!(to_gnome_accel("Ctrl+Alt+H").as_deref(), Some("<Control><Alt>h"));
        assert_eq!(to_gnome_accel("Shift+Super+7").as_deref(), Some("<Shift><Super>7"));
        assert_eq!(to_gnome_accel("Ctrl+F12").as_deref(), Some("<Control>F12"));
        assert_eq!(to_gnome_accel("super + v").as_deref(), Some("<Super>v"));
        assert_eq!(to_gnome_accel("V"), None, "a bare key is not a shortcut");
        assert_eq!(to_gnome_accel("Super+"), None);
        assert_eq!(to_gnome_accel("Super+Foo"), None);
        assert_eq!(to_gnome_accel("Hyper+V"), None);
        assert_eq!(to_gnome_accel("Super+F25"), None);
        assert_eq!(to_gnome_accel(""), None);
    }

    #[test]
    fn quoting_for_shell_and_gvariant() {
        assert_eq!(shell_quote("/usr/bin/app"), "/usr/bin/app");
        assert_eq!(shell_quote("/home/a b/app"), "'/home/a b/app'");
        assert_eq!(shell_quote("/home/it's/app"), "'/home/it'\\''s/app'");
        assert_eq!(gvariant_quote("a'b\\c"), "'a\\'b\\\\c'");
        assert_eq!(toggle_command("/home/a b/app"), "'/home/a b/app' --toggle");
    }

    /// In-memory gsettings. Every operation is logged in order; `persist` shares the log.
    #[derive(Clone, Default)]
    struct FakeGSettings {
        map: Arc<Mutex<BTreeMap<(String, String), String>>>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl FakeGSettings {
        fn with(entries: &[(&str, &str, &str)]) -> Self {
            let f = Self::default();
            for (s, k, val) in entries {
                f.map.lock().unwrap().insert(((*s).into(), (*k).into()), (*val).into());
            }
            f
        }
        fn value(&self, schema: &str, key: &str) -> Option<String> {
            self.map.lock().unwrap().get(&(schema.into(), key.into())).cloned()
        }
    }

    impl GSettings for FakeGSettings {
        fn get(&self, schema: &str, key: &str) -> Result<String, BackendError> {
            self.value(schema, key).ok_or_else(|| BackendError(format!("no key {schema} {key}")))
        }
        fn set(&self, schema: &str, key: &str, value: &str) -> Result<(), BackendError> {
            self.log.lock().unwrap().push(format!("set {schema} {key} {value}"));
            self.map.lock().unwrap().insert((schema.into(), key.into()), value.into());
            Ok(())
        }
        fn reset(&self, schema: &str, key: &str) -> Result<(), BackendError> {
            self.log.lock().unwrap().push(format!("reset {schema} {key}"));
            self.map.lock().unwrap().remove(&(schema.into(), key.into()));
            Ok(())
        }
        fn list_keys(&self, schema: &str) -> Result<Vec<String>, BackendError> {
            Ok(self.map.lock().unwrap().keys().filter(|(s, _)| s == schema).map(|(_, k)| k.clone()).collect())
        }
    }

    const SHELL: &str = "org.gnome.shell.keybindings";
    const WM: &str = "org.gnome.desktop.wm.keybindings";
    const MEDIA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const OURS: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipboard-history/";
    const OTHER: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/";
    const OTHER_SCHEMA: &str =
        "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/";
    const OURS_SCHEMA: &str =
        "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipboard-history/";

    fn desktop() -> FakeGSettings {
        FakeGSettings::with(&[
            (SHELL, "toggle-message-tray", "['<Super>v', '<Super>m']"),
            (SHELL, "toggle-overview", "['<Super>s']"),
            (WM, "close", "['<Alt>F4']"),
            (MEDIA, "custom-keybindings", &format!("['{OTHER}']")),
            (OTHER_SCHEMA, "name", "'Paste thing'"),
            (OTHER_SCHEMA, "binding", "'<Super>v'"),
            (OTHER_SCHEMA, "command", "'thing'"),
        ])
    }

    fn registrar(gs: &FakeGSettings) -> (GnomeRegistrar<FakeGSettings>, Arc<Mutex<Vec<String>>>) {
        let log = gs.log.clone();
        let persist_log = log.clone();
        let persist: Persist = Box::new(move |s: &Settings| {
            persist_log.lock().unwrap().push(format!("persist saved={}", s.saved_bindings.len()));
            Ok(())
        });
        (GnomeRegistrar::new(gs.clone(), Ok("/home/a b/app".into()), persist), log)
    }

    #[test]
    fn force_removes_super_v_from_every_binding_and_records_originals() {
        let gs = desktop();
        let (reg, _log) = registrar(&gs);
        let mut settings = Settings::default();
        let report = reg.force_super_v(&mut settings).unwrap();

        assert_eq!(gs.value(SHELL, "toggle-message-tray").unwrap(), "['<Super>m']");
        assert_eq!(gs.value(OTHER_SCHEMA, "binding").unwrap(), "''");
        assert_eq!(gs.value(SHELL, "toggle-overview").unwrap(), "['<Super>s']", "untouched");
        assert_eq!(
            settings.saved_bindings,
            vec![
                SavedBinding { schema: SHELL.into(), key: "toggle-message-tray".into(), original: "['<Super>v', '<Super>m']".into() },
                SavedBinding { schema: OTHER_SCHEMA.into(), key: "binding".into(), original: "'<Super>v'".into() },
            ]
        );
        assert_eq!(report.freed_from.len(), 2);
        assert!(report.freed_from[0].contains("toggle-message-tray"), "{:?}", report.freed_from);
        assert!(report.freed_from[1].contains("Paste thing"), "{:?}", report.freed_from);
    }

    #[test]
    fn force_persists_originals_before_the_first_write() {
        let gs = desktop();
        let (reg, log) = registrar(&gs);
        reg.force_super_v(&mut Settings::default()).unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log[0], "persist saved=2", "{log:?}");
        assert!(log[1].starts_with("set "));
    }

    #[test]
    fn force_writes_nothing_when_persisting_fails() {
        let gs = desktop();
        let persist: Persist = Box::new(|_| Err(std::io::Error::other("disk full")));
        let reg = GnomeRegistrar::new(gs.clone(), Ok("/app".into()), persist);
        assert!(reg.force_super_v(&mut Settings::default()).is_err());
        assert!(gs.log.lock().unwrap().is_empty());
    }

    #[test]
    fn force_twice_keeps_the_first_originals_and_skips_our_own_binding() {
        let gs = desktop();
        let (reg, _log) = registrar(&gs);
        let mut settings = Settings::default();
        reg.force_super_v(&mut settings).unwrap();
        reg.register("Super+V").unwrap();
        let again = reg.force_super_v(&mut settings).unwrap();
        assert!(again.freed_from.is_empty());
        assert_eq!(settings.saved_bindings.len(), 2);
        assert_eq!(gs.value(OURS_SCHEMA, "binding").unwrap(), "'<Super>v'", "our own binding stays");
    }

    #[test]
    fn register_writes_the_custom_keybinding() {
        let gs = desktop();
        let (reg, _log) = registrar(&gs);
        reg.register("Super+V").unwrap();
        reg.register("Super+V").unwrap();
        assert_eq!(gs.value(OURS_SCHEMA, "name").unwrap(), "'Clipboard History'");
        assert_eq!(gs.value(OURS_SCHEMA, "command").unwrap(), "'\\'/home/a b/app\\' --toggle'");
        assert_eq!(gs.value(OURS_SCHEMA, "binding").unwrap(), "'<Super>v'");
        assert_eq!(gs.value(MEDIA, "custom-keybindings").unwrap(), format!("['{OTHER}', '{OURS}']"));
        assert!(reg.register("Hyper+V").is_err());
    }

    #[test]
    fn register_refuses_when_the_custom_keybinding_list_cannot_be_read() {
        // No `custom-keybindings` key: `get` fails, as when dconf is unreachable.
        let gs = FakeGSettings::with(&[(SHELL, "toggle-message-tray", "['<Super>v', '<Super>m']")]);
        let (reg, _log) = registrar(&gs);
        assert!(reg.register("Super+V").is_err());
        assert!(gs.log.lock().unwrap().is_empty(), "nothing written, other custom keybindings are safe");
    }

    #[test]
    fn register_refuses_when_the_exe_path_is_unusable() {
        let gs = desktop();
        let persist: Persist = Box::new(|_| Ok(()));
        let reg = GnomeRegistrar::new(gs.clone(), Err("path is not UTF-8".into()), persist);
        assert!(reg.register("Super+V").is_err());
        assert!(gs.log.lock().unwrap().is_empty());
    }

    #[test]
    fn restore_puts_back_exact_originals() {
        let gs = desktop();
        let before = gs.map.lock().unwrap().clone();
        let (reg, log) = registrar(&gs);
        let mut settings = Settings::default();
        reg.force_super_v(&mut settings).unwrap();
        reg.register("Super+V").unwrap();
        reg.restore(&mut settings).unwrap();

        assert_eq!(gs.value(SHELL, "toggle-message-tray").unwrap(), "['<Super>v', '<Super>m']");
        assert_eq!(gs.value(OTHER_SCHEMA, "binding").unwrap(), "'<Super>v'");
        assert_eq!(gs.value(MEDIA, "custom-keybindings").unwrap(), format!("['{OTHER}']"));
        assert_eq!(gs.value(OURS_SCHEMA, "binding"), None);
        assert!(settings.saved_bindings.is_empty());
        assert_eq!(*gs.map.lock().unwrap(), before, "everything is as before");
        assert_eq!(log.lock().unwrap().last().unwrap(), "persist saved=0");
    }

    #[test]
    fn restore_keeps_unrestored_entries_when_a_write_fails() {
        struct FailingSet(FakeGSettings);
        impl GSettings for FailingSet {
            fn get(&self, s: &str, k: &str) -> Result<String, BackendError> { self.0.get(s, k) }
            fn set(&self, _: &str, _: &str, _: &str) -> Result<(), BackendError> { Err(BackendError("dconf down".into())) }
            fn reset(&self, s: &str, k: &str) -> Result<(), BackendError> { self.0.reset(s, k) }
            fn list_keys(&self, s: &str) -> Result<Vec<String>, BackendError> { self.0.list_keys(s) }
        }
        let persist: Persist = Box::new(|_| Ok(()));
        let reg = GnomeRegistrar::new(FailingSet(desktop()), Ok("/app".into()), persist);
        let mut settings = Settings {
            saved_bindings: vec![SavedBinding { schema: SHELL.into(), key: "toggle-message-tray".into(), original: "['<Super>v']".into() }],
            ..Settings::default()
        };
        assert!(reg.restore(&mut settings).is_err());
        assert_eq!(settings.saved_bindings.len(), 1, "still restorable later");
    }

    // The real gsettings binary with an in-memory backend: nothing reaches the
    // user's dconf. Each gsettings process has its own memory, so these only
    // check that the shell-out and output parsing work, not persistence.
    fn memory_gsettings() -> Option<SystemGSettings> {
        let gs = SystemGSettings::with_env(vec![("GSETTINGS_BACKEND".into(), "memory".into())]);
        gs.get(SHELL, "toggle-message-tray").ok().map(|_| gs)
    }

    #[test]
    fn system_gsettings_reads_schema_defaults_from_the_memory_backend() {
        let Some(gs) = memory_gsettings() else {
            eprintln!("skipped: no gsettings binary or GNOME schemas");
            return;
        };
        assert_eq!(parse_strv(&gs.get(SHELL, "toggle-message-tray").unwrap()), v(&["<Super>v", "<Super>m"]));
        assert!(gs.list_keys(SHELL).unwrap().contains(&"toggle-message-tray".to_string()));
        assert!(gs.get(SHELL, "no-such-key").is_err());
    }

    #[test]
    fn system_gsettings_writes_stay_in_the_memory_backend() {
        let Some(gs) = memory_gsettings() else { return };
        let real = |_: ()| {
            let out = std::process::Command::new("gsettings").args(["get", SHELL, "toggle-message-tray"]).output().unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let before = real(());
        gs.set(SHELL, "toggle-message-tray", "['<Super>m']").unwrap();
        gs.reset(SHELL, "toggle-message-tray").unwrap();
        assert_eq!(real(()), before, "the real session is untouched");
    }
}
