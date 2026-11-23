//! In-memory backend for tests: records writes, lets the test push events.

use super::{BackendError, Capabilities, ClipboardBackend, FocusTracker, FocusedWindow, Paster};
use crate::core::clip::{ClipEvent, Format};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct FakeBackend {
    writes: Arc<Mutex<Vec<Vec<Format>>>>,
    tx: Arc<Mutex<Option<Sender<ClipEvent>>>>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Formats of every `write` call so far.
    pub fn writes(&self) -> Vec<Vec<Format>> {
        self.writes.lock().unwrap().clone()
    }

    /// Pushes an event as if the clipboard changed. False if not watching.
    pub fn push(&self, e: ClipEvent) -> bool {
        match self.tx.lock().unwrap().as_ref() {
            Some(tx) => tx.send(e).is_ok(),
            None => false,
        }
    }
}

impl ClipboardBackend for FakeBackend {
    fn watch(&self, tx: Sender<ClipEvent>) -> Result<(), BackendError> {
        *self.tx.lock().unwrap() = Some(tx);
        Ok(())
    }

    fn write(&self, formats: &[Format]) -> Result<(), BackendError> {
        self.writes.lock().unwrap().push(formats.to_vec());
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            source_app: false,
            auto_paste: false,
            files: false,
            backend_name: "fake".into(),
        }
    }
}

/// Records paste calls; `fail` makes `paste` fail, `not_ready` makes `ready` false.
#[derive(Clone, Default)]
pub struct FakePaster {
    pub pastes: Arc<Mutex<Vec<(FocusedWindow, bool)>>>,
    pub fail: bool,
    pub not_ready: bool,
}

impl Paster for FakePaster {
    fn ready(&self) -> bool {
        !self.not_ready
    }

    fn paste(&self, target: &FocusedWindow, plain: bool) -> Result<(), BackendError> {
        if self.fail {
            return Err(BackendError("paste failed".into()));
        }
        self.pastes.lock().unwrap().push((*target, plain));
        Ok(())
    }
}

/// Records restore calls; `fail_restore` makes `restore` fail.
#[derive(Clone, Default)]
pub struct FakeFocus {
    pub restored: Arc<Mutex<Vec<FocusedWindow>>>,
    pub fail_restore: bool,
}

impl FocusTracker for FakeFocus {
    fn current(&self) -> Option<FocusedWindow> {
        None
    }

    fn restore(&self, w: &FocusedWindow) -> Result<(), BackendError> {
        if self.fail_restore {
            return Err(BackendError("window gone".into()));
        }
        self.restored.lock().unwrap().push(*w);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_writes_and_delivers_events() {
        let b = FakeBackend::new();
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(!b.push(ClipEvent { formats: vec![], source_app: None, secret: false }));
        b.watch(tx).unwrap();
        let f = Format { mime: "text/plain;charset=utf-8".into(), data: b"a".to_vec() };
        b.write(std::slice::from_ref(&f)).unwrap();
        assert_eq!(b.writes(), vec![vec![f.clone()]]);
        assert!(b.push(ClipEvent { formats: vec![f.clone()], source_app: None, secret: false }));
        assert_eq!(rx.recv().unwrap().formats, vec![f]);
    }
}
