//! macOS auto-paste: give the keyboard back to the app that was in front
//! before the popup, then post Cmd+V to it.
//!
//! Tauri windows are not non-activating panels, so the popup activates our
//! app. `MacFocus::restore` hides our app (macOS then reactivates the
//! previous one) and waits until that app is frontmost. `MacPaster` posts a
//! keystroke only to the recorded app, and only with the Accessibility grant.

use crate::platform::{BackendError, FocusTracker, FocusedWindow, Paster};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use std::time::{Duration, Instant};

/// How long `restore` waits for the previous app to be frontmost.
const FOCUS_WAIT: Duration = Duration::from_millis(300);
const FOCUS_STEP: Duration = Duration::from_millis(20);
/// `kVK_ANSI_V` (Carbon `Events.h`): the key in the US-layout V position.
/// On layouts that move V (plain Dvorak) this is another letter; checklist.
const KEY_V: u16 = 0x09;

/// Calls `check` every `step` until it is true (true) or `timeout` has passed
/// (false). `check` always runs at least once.
pub fn wait_until(timeout: Duration, step: Duration, mut check: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(step);
    }
}

fn frontmost_pid() -> Option<i32> {
    NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier())
}

/// The previous app as a FocusedWindow: its process id.
pub struct MacFocus {
    /// Hides our app so macOS gives activation back (`popup::hide`).
    hide_app: Box<dyn Fn() + Send + Sync>,
}

impl MacFocus {
    pub fn new(hide_app: impl Fn() + Send + Sync + 'static) -> Self {
        Self { hide_app: Box::new(hide_app) }
    }
}

impl FocusTracker for MacFocus {
    fn current(&self) -> Option<FocusedWindow> {
        let pid = frontmost_pid().filter(|p| *p > 0)?;
        // Our own popup in front is no target to paste into.
        if pid == NSRunningApplication::currentApplication().processIdentifier() {
            return None;
        }
        u64::try_from(pid).ok().map(FocusedWindow)
    }

    fn restore(&self, w: &FocusedWindow) -> Result<(), BackendError> {
        let pid = i32::try_from(w.0).map_err(|_| BackendError(format!("{} is not a process id", w.0)))?;
        (self.hide_app)();
        // Activation is cooperative on macOS 14+: a hidden app cannot pull the
        // target forward, so there is nothing to retry. Copied, not pasted.
        if wait_until(FOCUS_WAIT, FOCUS_STEP, || frontmost_pid() == Some(pid)) {
            Ok(())
        } else {
            Err(BackendError("the previous app did not come back to the front".into()))
        }
    }
}

/// Posts Cmd+V; terminals on macOS paste with Cmd+V too.
pub struct MacPaster;

impl Paster for MacPaster {
    fn ready(&self) -> bool {
        CGPreflightPostEventAccess()
    }

    fn paste(&self, target: &FocusedWindow, _plain: bool) -> Result<(), BackendError> {
        // Never type into whatever happens to be in front.
        if frontmost_pid().and_then(|p| u64::try_from(p).ok()) != Some(target.0) {
            return Err(BackendError("the target app is no longer in front".into()));
        }
        if !self.ready() {
            // Shows macOS's Accessibility prompt; this pick stays a copy.
            CGRequestPostEventAccess();
            return Err(BackendError("Accessibility permission is needed to paste".into()));
        }
        for key_down in [true, false] {
            let event = CGEvent::new_keyboard_event(None, KEY_V, key_down)
                .ok_or_else(|| BackendError("cannot create the Cmd+V event".into()))?;
            CGEvent::set_flags(Some(&event), CGEventFlags::MaskCommand);
            CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&event));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_until_returns_true_once_the_check_passes() {
        let mut calls = 0;
        assert!(wait_until(Duration::from_millis(100), Duration::from_millis(1), || {
            calls += 1;
            calls == 3
        }));
        assert_eq!(calls, 3);
    }

    #[test]
    fn wait_until_gives_up_after_the_timeout() {
        let start = Instant::now();
        assert!(!wait_until(Duration::from_millis(30), Duration::from_millis(5), || false));
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn paste_refused_when_target_is_not_frontmost() {
        // No such process is ever in front, so no key is posted and no
        // permission prompt is shown, with or without the grant.
        assert!(MacPaster.paste(&FocusedWindow(u64::from(u32::MAX)), false).is_err());
    }
}
