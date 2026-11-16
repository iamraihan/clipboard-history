//! Types shared with the frontend over IPC (camelCase on the wire).
//!
//! Each derives `TS`; `cargo test` regenerates its TypeScript twin in
//! `src/lib/bindings/`. Never hand-write those types.

use serde::{Deserialize, Serialize};
#[cfg(test)]
use ts_rs::TS;

/// What a Clip mainly is, decided from its Formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub enum ClipKind {
    Text,
    RichText,
    Image,
    Files,
}

/// One row of the popup list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(TS), ts(export))]
pub struct ClipSummary {
    #[cfg_attr(test, ts(type = "number"))]
    pub id: i64,
    pub kind: ClipKind,
    pub preview: String,
    pub thumbnail_png_base64: Option<String>,
    pub pinned: bool,
    /// RFC3339 timestamp.
    pub created_at: String,
    pub source_app: Option<String>,
}
