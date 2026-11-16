//! App updates: what the tray offers and when to check. Pure, no side effects.

/// Where the app's update stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    /// No check has finished yet.
    #[default]
    Unchecked,
    /// The user asked for a check and it is running.
    Checking,
    /// The last check found nothing newer.
    UpToDate,
    /// The check the user asked for failed (offline, GitHub unreachable).
    Failed,
    /// A newer release exists.
    Available { version: String },
    /// The newer release is downloading and installing; the app restarts after.
    Installing { version: String },
}

/// How an update is applied on this OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMethod {
    /// Download, install and restart in place (Windows, Linux).
    InPlace,
    /// Open the release page (macOS: in-place updates need a Developer ID-signed build).
    ReleasePage,
}

/// The tray menu line for updates. Always present, so the user can check by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayUpdateItem {
    pub label: String,
    pub enabled: bool,
    /// Shown at the top of the menu (a newer release) instead of near "Quit".
    pub top: bool,
}

/// The tray line for `status`; `current` is the running version.
pub fn tray_item(status: &UpdateStatus, method: UpdateMethod, current: &str) -> TrayUpdateItem {
    let line = |label: String, enabled: bool, top: bool| TrayUpdateItem { label, enabled, top };
    match status {
        UpdateStatus::Unchecked => line(format!("Check for updates (v{current})"), true, false),
        UpdateStatus::Checking => line("Checking for updates\u{2026}".into(), false, false),
        UpdateStatus::UpToDate => line(format!("Up to date (v{current}) \u{b7} Check again"), true, false),
        UpdateStatus::Failed => line("Update check failed \u{b7} Try again".into(), true, false),
        UpdateStatus::Available { version } => line(
            match method {
                UpdateMethod::InPlace => format!("Update to v{version} and restart"),
                UpdateMethod::ReleasePage => format!("Download v{version}\u{2026}"),
            },
            true,
            true,
        ),
        UpdateStatus::Installing { version } => line(format!("Installing v{version}\u{2026}"), false, true),
    }
}

/// What a click on the tray line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    Install,
    Check,
    Nothing,
}

pub fn click_action(status: &UpdateStatus) -> ClickAction {
    match status {
        UpdateStatus::Available { .. } => ClickAction::Install,
        UpdateStatus::Unchecked | UpdateStatus::UpToDate | UpdateStatus::Failed => ClickAction::Check,
        UpdateStatus::Checking | UpdateStatus::Installing { .. } => ClickAction::Nothing,
    }
}

/// The status after a check failed: the user hears about it only when they
/// asked for the check; a background failure keeps what the tray shows.
pub fn after_failed_check(status: &UpdateStatus) -> UpdateStatus {
    match status {
        UpdateStatus::Checking => UpdateStatus::Failed,
        other => other.clone(),
    }
}

/// A periodic check must not replace the status of an install in progress.
pub fn should_check(status: &UpdateStatus) -> bool {
    !matches!(status, UpdateStatus::Installing { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = "0.1.1";

    fn available() -> UpdateStatus {
        UpdateStatus::Available { version: "0.2.0".into() }
    }

    fn item(status: UpdateStatus) -> TrayUpdateItem {
        tray_item(&status, UpdateMethod::InPlace, CURRENT)
    }

    #[test]
    fn before_any_check_the_tray_offers_one_with_the_running_version() {
        let i = item(UpdateStatus::Unchecked);
        assert_eq!(i.label, "Check for updates (v0.1.1)");
        assert!(i.enabled && !i.top);
    }

    #[test]
    fn a_running_check_shows_a_disabled_line() {
        let i = item(UpdateStatus::Checking);
        assert_eq!(i.label, "Checking for updates\u{2026}");
        assert!(!i.enabled && !i.top);
    }

    #[test]
    fn up_to_date_says_so_and_can_check_again() {
        let i = item(UpdateStatus::UpToDate);
        assert_eq!(i.label, "Up to date (v0.1.1) \u{b7} Check again");
        assert!(i.enabled && !i.top);
    }

    #[test]
    fn a_failed_check_can_be_retried() {
        let i = item(UpdateStatus::Failed);
        assert_eq!(i.label, "Update check failed \u{b7} Try again");
        assert!(i.enabled && !i.top);
    }

    #[test]
    fn in_place_update_offers_install_and_restart_at_the_top() {
        let i = item(available());
        assert_eq!(i.label, "Update to v0.2.0 and restart");
        assert!(i.enabled && i.top);
    }

    #[test]
    fn release_page_update_offers_download() {
        let i = tray_item(&available(), UpdateMethod::ReleasePage, CURRENT);
        assert_eq!(i.label, "Download v0.2.0\u{2026}");
        assert!(i.enabled && i.top);
    }

    #[test]
    fn installing_shows_a_disabled_line() {
        let i = item(UpdateStatus::Installing { version: "0.2.0".into() });
        assert_eq!(i.label, "Installing v0.2.0\u{2026}");
        assert!(!i.enabled && i.top);
    }

    #[test]
    fn clicking_installs_an_available_update_and_otherwise_checks() {
        assert_eq!(click_action(&available()), ClickAction::Install);
        assert_eq!(click_action(&UpdateStatus::Unchecked), ClickAction::Check);
        assert_eq!(click_action(&UpdateStatus::UpToDate), ClickAction::Check);
        assert_eq!(click_action(&UpdateStatus::Failed), ClickAction::Check);
        assert_eq!(click_action(&UpdateStatus::Checking), ClickAction::Nothing);
        assert_eq!(click_action(&UpdateStatus::Installing { version: "0.2.0".into() }), ClickAction::Nothing);
    }

    #[test]
    fn a_failed_check_only_reports_failure_when_the_user_is_waiting_on_it() {
        assert_eq!(after_failed_check(&UpdateStatus::Checking), UpdateStatus::Failed);
        // A background check that fails keeps what the tray already shows.
        assert_eq!(after_failed_check(&available()), available());
        assert_eq!(after_failed_check(&UpdateStatus::UpToDate), UpdateStatus::UpToDate);
    }

    #[test]
    fn checks_unless_installing() {
        assert!(should_check(&UpdateStatus::UpToDate));
        assert!(should_check(&UpdateStatus::Checking));
        assert!(should_check(&available()));
        assert!(!should_check(&UpdateStatus::Installing { version: "0.2.0".into() }));
    }
}
