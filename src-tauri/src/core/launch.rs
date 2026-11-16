//! Start at login and how the app was launched: the decisions. Pure, no side
//! effects; `crate::autostart` applies them.

/// Passed by the GNOME keybinding (and any launcher) to toggle the popup.
pub const TOGGLE_ARG: &str = "--toggle";
/// Passed by the login entry the autostart plugin writes: start in the tray, no popup.
pub const AUTOSTART_ARG: &str = "--autostart";

/// How this process was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// The app grid, a terminal.
    Plain,
    /// The shortcut.
    Toggle,
    /// Login.
    Autostart,
}

/// `args` as from `std::env::args()` (the program name first).
pub fn launch_kind(args: &[String]) -> Launch {
    if args.iter().any(|a| a == AUTOSTART_ARG) {
        Launch::Autostart
    } else if args.iter().any(|a| a == TOGGLE_ARG) {
        Launch::Toggle
    } else {
        Launch::Plain
    }
}

/// What happened to the login entry, for `applied_after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartEvent {
    /// Startup turned the default on.
    DefaultEnabled,
    /// Startup tried to turn the default on and failed; it tries again next start.
    DefaultFailed,
    /// The user switched it on or off in the tray.
    UserChose,
}

/// `Settings::autostart_default_applied` after `event`: once the default
/// worked or the user chose, startup never switches it on again.
pub fn applied_after(applied: bool, event: AutostartEvent) -> bool {
    applied || matches!(event, AutostartEvent::DefaultEnabled | AutostartEvent::UserChose)
}

/// Whether the tray's "Start at login" can be clicked: never in a debug build,
/// where one click would make login start a `target/debug` binary.
pub fn tray_toggle_enabled(release_build: bool) -> bool {
    release_build
}

/// Start at login is on by default: turn it on the first time a release
/// build runs (`applied` is `Settings::autostart_default_applied`). After that
/// the user's choice in the tray stands. A debug build never registers itself,
/// or login would start a `target/debug` binary.
pub fn enable_by_default(applied: bool, release_build: bool) -> bool {
    release_build && !applied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_build_turns_autostart_on_once() {
        assert!(enable_by_default(false, true));
        // Already applied: the user's later choice (maybe off) stands.
        assert!(!enable_by_default(true, true));
    }

    #[test]
    fn a_debug_build_never_registers_itself() {
        assert!(!enable_by_default(false, false));
    }

    #[test]
    fn any_choice_in_the_tray_ends_the_default() {
        // Turning the default on failed, then the user switched it off: the
        // next start must not switch it back on.
        let applied = applied_after(false, AutostartEvent::DefaultFailed);
        assert!(!applied);
        let applied = applied_after(applied, AutostartEvent::UserChose);
        assert!(applied);
        assert!(!enable_by_default(applied, true));
        assert!(applied_after(false, AutostartEvent::DefaultEnabled));
        assert!(applied_after(true, AutostartEvent::DefaultFailed));
    }

    #[test]
    fn only_a_release_build_offers_the_tray_toggle() {
        // One click in `pnpm tauri dev` would register the target/debug binary.
        assert!(tray_toggle_enabled(true));
        assert!(!tray_toggle_enabled(false));
    }

    #[test]
    fn the_launch_kind_comes_from_the_arguments() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(launch_kind(&args(&["app"])), Launch::Plain);
        assert_eq!(launch_kind(&args(&["app", "--toggle"])), Launch::Toggle);
        assert_eq!(launch_kind(&args(&["app", "--autostart"])), Launch::Autostart);
    }
}
