//! System tray icon and menu.
//!
//! Menu: the update line (while a newer release exists or installs), "Open clipboard history (<shortcut>)", "Use Super+V…" (GNOME, only while no shortcut
//! is set), "Reset shortcuts to default", "Start at login" (a check item), the update line when there is no
//! newer release ("Check for updates", "Up to date…"), "Quit". The
//! tooltip carries the shortcut and startup problems. Tauri ignores tooltips on
//! Linux, so when there is a problem the menu starts with it as disabled lines.

use crate::shortcut;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::AppHandle;

const TRAY_ID: &str = "main";
const OPEN: &str = "open";
const USE_SUPER_V: &str = "use-super-v";
const RESET: &str = "reset";
const QUIT: &str = "quit";
const UPDATE: &str = "update";
const START_AT_LOGIN: &str = "start-at-login";

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let text = shortcut::current_tooltip(app);
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    let problems: Vec<&str> = text.lines().skip(1).collect();
    for line in &problems {
        items.push(Box::new(MenuItem::new(app, *line, false, None::<&str>)?));
    }
    if !problems.is_empty() {
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    let update = crate::updater::current_tray_item(app);
    if update.top {
        items.push(Box::new(MenuItem::with_id(app, UPDATE, &update.label, update.enabled, None::<&str>)?));
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    items.push(Box::new(MenuItem::with_id(app, OPEN, shortcut::open_label(shortcut::current_shortcut(app).as_deref()), true, None::<&str>)?));
    if shortcut::show_use_super_v(app) {
        items.push(Box::new(MenuItem::with_id(app, USE_SUPER_V, "Use Super+V\u{2026}", true, None::<&str>)?));
    }
    items.push(Box::new(MenuItem::with_id(app, RESET, "Reset shortcuts to default", true, None::<&str>)?));
    let at_login = crate::autostart::is_enabled(app);
    items.push(Box::new(CheckMenuItem::with_id(app, START_AT_LOGIN, "Start at login", crate::autostart::can_toggle(), at_login, None::<&str>)?));
    if !update.top {
        items.push(Box::new(MenuItem::with_id(app, UPDATE, &update.label, update.enabled, None::<&str>)?));
    }
    items.push(Box::new(MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?));
    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|i| i.as_ref()).collect();
    Menu::with_items(app, &refs)
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&build_menu(app)?)
        .tooltip(shortcut::current_tooltip(app))
        .on_menu_event(|app, event| match event.id.as_ref() {
            OPEN => crate::popup::toggle(app),
            USE_SUPER_V => shortcut::show_settings(app, shortcut::SettingsView::FirstRun),
            RESET => {
                let app = app.clone();
                // gsettings runs here; keep it off the main thread.
                std::thread::spawn(move || {
                    if let Err(e) = shortcut::run_reset(&app) {
                        eprintln!("reset shortcuts: {e}");
                    }
                });
            }
            QUIT => shortcut::request_quit(app),
            UPDATE => crate::updater::on_click(app),
            START_AT_LOGIN => crate::autostart::toggle(app),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Rebuilds the menu and tooltip after the shortcut state or the StartupReport changed.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    if let Ok(menu) = build_menu(app) {
        let _ = tray.set_menu(Some(menu));
    }
    let _ = tray.set_tooltip(Some(shortcut::current_tooltip(app)));
}
