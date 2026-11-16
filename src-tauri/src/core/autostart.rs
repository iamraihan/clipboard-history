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
    fn the_launch_kind_comes_from_the_arguments() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(launch_kind(&args(&["app"])), Launch::Plain);
        assert_eq!(launch_kind(&args(&["app", "--toggle"])), Launch::Toggle);
        assert_eq!(launch_kind(&args(&["app", "--autostart"])), Launch::Autostart);
    }
}
