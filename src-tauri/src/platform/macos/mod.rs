//! macOS Backends: the pasteboard watcher (`MacBackend`) and auto-paste
//! (`MacPaster`, `MacFocus`). Decisions live in `core::pasteboard`; this
//! module only calls AppKit and CoreGraphics through safe objc2 bindings.

pub mod paste;
pub mod pasteboard;

pub use paste::{MacFocus, MacPaster};
pub use pasteboard::{MacBackend, PasteboardName};
