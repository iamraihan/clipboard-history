//! Linux Backends. X11, which is also the GNOME Wayland Backend through
//! XWayland (see docs/adr/0001-gnome-wayland-clipboard-via-xwayland.md).

pub mod gnome_keys;
pub mod pointer;
pub mod x11;
pub mod x11_paste;
#[cfg(test)]
mod x11_test_support;
mod x11_transfer;

pub use gnome_keys::{GnomeRegistrar, SystemGSettings};
pub use x11::X11Backend;

/// Which Backend to use for the current session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxBackendChoice {
    X11,
    /// GNOME Wayland, watched through XWayland (ADR 0001).
    XWayland,
    /// No Backend for this session; the text is shown to the user.
    Unsupported(String),
}

/// Pure choice from the session environment. ADR 0001 decided XWayland for
/// GNOME Wayland, so that case needs `DISPLAY` and never `Unsupported` on
/// the Shell-extension path.
pub fn choose(
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    display: Option<&str>,
    desktop: Option<&str>,
) -> LinuxBackendChoice {
    let wayland_display = wayland_display.filter(|s| !s.is_empty());
    let display = display.filter(|s| !s.is_empty());
    if session_type == Some("x11") || (wayland_display.is_none() && display.is_some()) {
        return LinuxBackendChoice::X11;
    }
    let gnome = desktop.is_some_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("GNOME")));
    match (gnome, display) {
        (true, Some(_)) => LinuxBackendChoice::XWayland,
        (true, None) => LinuxBackendChoice::Unsupported(
            "XWayland is not running, so the clipboard cannot be watched on this GNOME session".into(),
        ),
        (false, _) if desktop.is_some_and(|d| !d.is_empty()) => {
            LinuxBackendChoice::Unsupported("KDE/Sway support arrives in a later version".into())
        }
        (false, _) => LinuxBackendChoice::Unsupported("No supported desktop session found".into()),
    }
}

/// `choose` reading the environment through `get` (injectable for tests).
pub fn choose_from_env(get: impl Fn(&str) -> Option<String>) -> LinuxBackendChoice {
    let session_type = get("XDG_SESSION_TYPE");
    let wayland_display = get("WAYLAND_DISPLAY");
    let display = get("DISPLAY");
    let desktop = get("XDG_CURRENT_DESKTOP");
    choose(session_type.as_deref(), wayland_display.as_deref(), display.as_deref(), desktop.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn x11_session_uses_x11() {
        assert_eq!(choose(Some("x11"), None, Some(":0"), Some("ubuntu:GNOME")), LinuxBackendChoice::X11);
        // Session type unset, no Wayland, DISPLAY set.
        assert_eq!(choose(None, None, Some(":0"), None), LinuxBackendChoice::X11);
    }

    #[test]
    fn gnome_wayland_with_display_uses_xwayland() {
        assert_eq!(
            choose(Some("wayland"), Some("wayland-0"), Some(":1"), Some("ubuntu:GNOME")),
            LinuxBackendChoice::XWayland
        );
    }

    #[test]
    fn wayland_without_display_is_unsupported() {
        assert!(matches!(
            choose(Some("wayland"), Some("wayland-0"), None, Some("ubuntu:GNOME")),
            LinuxBackendChoice::Unsupported(_)
        ));
        assert_eq!(
            choose(Some("wayland"), Some("wayland-0"), Some(":1"), None),
            LinuxBackendChoice::Unsupported("No supported desktop session found".into())
        );
        assert_eq!(
            choose(None, None, None, None),
            LinuxBackendChoice::Unsupported("No supported desktop session found".into())
        );
    }

    #[test]
    fn kde_wayland_is_unsupported_for_now() {
        assert_eq!(
            choose(Some("wayland"), Some("wayland-0"), Some(":1"), Some("KDE")),
            LinuxBackendChoice::Unsupported("KDE/Sway support arrives in a later version".into())
        );
    }

    #[test]
    fn choose_from_env_reads_exact_variable_names() {
        let asked = RefCell::new(Vec::new());
        let _ = choose_from_env(|k| {
            asked.borrow_mut().push(k.to_string());
            None
        });
        let mut got = asked.into_inner();
        got.sort();
        assert_eq!(got, ["DISPLAY", "WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_SESSION_TYPE"]);

        let env = |k: &str| match k {
            "XDG_SESSION_TYPE" => Some("wayland".to_string()),
            "WAYLAND_DISPLAY" => Some("wayland-0".to_string()),
            "DISPLAY" => Some(":1".to_string()),
            "XDG_CURRENT_DESKTOP" => Some("GNOME".to_string()),
            _ => None,
        };
        assert_eq!(choose_from_env(env), LinuxBackendChoice::XWayland);
    }
}
