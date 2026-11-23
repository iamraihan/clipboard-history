//! Platform backends: ClipboardBackend, Paster, FocusTracker.
//! ShortcutRegistrar: how the OS is told to run us on the shortcut.

#[cfg(test)]
pub mod fake;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

use crate::core::clip::{ClipEvent, Format};
use crate::core::settings::Settings;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::mpsc::Sender;
#[cfg(test)]
use ts_rs::TS;

/// A failure inside a platform backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "backend error: {}", self.0)
    }
}

impl std::error::Error for BackendError {}

/// What the current Backend can do; the UI adapts to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct Capabilities {
    pub source_app: bool,
    pub auto_paste: bool,
    pub files: bool,
    pub backend_name: String,
}

/// Opaque handle to a window (X11 window id, HWND, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusedWindow(pub u64);

pub trait ClipboardBackend: Send + Sync {
    /// Starts watching; each clipboard change is sent as a ClipEvent.
    fn watch(&self, tx: Sender<ClipEvent>) -> Result<(), BackendError>;
    fn write(&self, formats: &[Format]) -> Result<(), BackendError>;
    fn capabilities(&self) -> Capabilities;
}

pub trait Paster: Send + Sync {
    fn paste(&self, target: &FocusedWindow, plain: bool) -> Result<(), BackendError>;
    /// Whether `paste` can work right now (macOS: the Accessibility grant,
    /// which the user can give or take back while the app runs).
    fn ready(&self) -> bool {
        true
    }
}

pub trait FocusTracker: Send + Sync {
    fn current(&self) -> Option<FocusedWindow>;
    fn restore(&self, w: &FocusedWindow) -> Result<(), BackendError>;
}

/// What Force Super+V took the shortcut away from, for the UI to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct ForceReport {
    /// Human-readable names of the OS bindings that had Super+V.
    pub freed_from: Vec<String>,
}

/// Registers the global shortcut and, where the OS owns Super+V, takes it over
/// (Force Super+V) and gives it back (Reset to default).
pub trait ShortcutRegistrar: Send + Sync {
    /// Makes `combo` (e.g. "Super+V") open the popup.
    fn register(&self, combo: &str) -> Result<(), BackendError>;
    /// Frees Super+V from the OS. Records every original binding in
    /// `settings.saved_bindings` and persists settings before changing anything.
    fn force_super_v(&self, settings: &mut Settings) -> Result<ForceReport, BackendError>;
    /// Puts every saved original back, clears `saved_bindings`, and removes our own binding.
    /// It writes the saved original, overwriting any manual change made to that key since Force.
    fn restore(&self, settings: &mut Settings) -> Result<(), BackendError>;
}
