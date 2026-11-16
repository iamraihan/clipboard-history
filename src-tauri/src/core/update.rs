//! App updates: what the tray offers and when to check. Pure, no side effects.

/// Where the app's update stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    /// No check yet, or the last check found nothing newer.
    #[default]
    UpToDate,
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

/// The tray menu line for an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayUpdateItem {
    pub label: String,
    pub enabled: bool,
}

/// The tray line to show, or None when there is nothing to offer.
pub fn tray_item(status: &UpdateStatus, method: UpdateMethod) -> Option<TrayUpdateItem> {
    match status {
        UpdateStatus::UpToDate => None,
        UpdateStatus::Available { version } => Some(TrayUpdateItem {
            label: match method {
                UpdateMethod::InPlace => format!("Update to v{version} and restart"),
                UpdateMethod::ReleasePage => format!("Download v{version}\u{2026}"),
            },
            enabled: true,
        }),
        UpdateStatus::Installing { version } => Some(TrayUpdateItem {
            label: format!("Installing v{version}\u{2026}"),
            enabled: false,
        }),
    }
}

/// A periodic check must not replace the status of an install in progress.
pub fn should_check(status: &UpdateStatus) -> bool {
    !matches!(status, UpdateStatus::Installing { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> UpdateStatus {
        UpdateStatus::Available { version: "0.2.0".into() }
    }

    #[test]
    fn nothing_to_offer_when_up_to_date() {
        assert_eq!(tray_item(&UpdateStatus::UpToDate, UpdateMethod::InPlace), None);
        assert_eq!(tray_item(&UpdateStatus::UpToDate, UpdateMethod::ReleasePage), None);
    }

    #[test]
    fn in_place_update_offers_install_and_restart() {
        let item = tray_item(&available(), UpdateMethod::InPlace).unwrap();
        assert_eq!(item.label, "Update to v0.2.0 and restart");
        assert!(item.enabled);
    }

    #[test]
    fn release_page_update_offers_download() {
        let item = tray_item(&available(), UpdateMethod::ReleasePage).unwrap();
        assert_eq!(item.label, "Download v0.2.0\u{2026}");
        assert!(item.enabled);
    }

    #[test]
    fn installing_shows_a_disabled_line() {
        let status = UpdateStatus::Installing { version: "0.2.0".into() };
        let item = tray_item(&status, UpdateMethod::InPlace).unwrap();
        assert_eq!(item.label, "Installing v0.2.0\u{2026}");
        assert!(!item.enabled);
    }

    #[test]
    fn checks_unless_installing() {
        assert!(should_check(&UpdateStatus::UpToDate));
        assert!(should_check(&available()));
        assert!(!should_check(&UpdateStatus::Installing { version: "0.2.0".into() }));
    }
}
