//! Clip model helpers: kind, content hash, preview.

use crate::models::ClipKind;

const PREVIEW_CHARS: usize = 200;
const MIME_FILES: &str = "text/uri-list";
const MIME_IMAGE: &str = "image/png";
const MIME_HTML: &str = "text/html";
const MIME_TEXT: &str = "text/plain;charset=utf-8";

/// One representation of a Clip's content, identified by MIME type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub mime: String,
    pub data: Vec<u8>,
}

/// What a Backend emits when the clipboard changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipEvent {
    pub formats: Vec<Format>,
    pub source_app: Option<String>,
    pub secret: bool,
}

fn find<'a>(formats: &'a [Format], mime: &str) -> Option<&'a Format> {
    formats.iter().find(|f| f.mime == mime)
}

/// The Clip's kind: files, then image, then rich text, then text.
pub fn kind_of(formats: &[Format]) -> Option<ClipKind> {
    [
        (MIME_FILES, ClipKind::Files),
        (MIME_IMAGE, ClipKind::Image),
        (MIME_HTML, ClipKind::RichText),
        (MIME_TEXT, ClipKind::Text),
    ]
    .into_iter()
    .find(|(mime, _)| find(formats, mime).is_some())
    .map(|(_, kind)| kind)
}

/// BLAKE3 over the Formats sorted by MIME, each as `mime \0 len(u64 LE) data`.
pub fn content_hash(formats: &[Format]) -> [u8; 32] {
    let mut sorted: Vec<&Format> = formats.iter().collect();
    sorted.sort_by(|a, b| a.mime.cmp(&b.mime));
    let mut hasher = blake3::Hasher::new();
    for f in sorted {
        hasher.update(f.mime.as_bytes());
        hasher.update(&[0]);
        hasher.update(&(f.data.len() as u64).to_le_bytes());
        hasher.update(&f.data);
    }
    *hasher.finalize().as_bytes()
}

/// Sum of all Format sizes in bytes.
pub fn total_size(formats: &[Format]) -> u64 {
    formats.iter().map(|f| f.data.len() as u64).sum()
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Plain text (or tag-stripped HTML), whitespace collapsed, first 200 chars.
pub fn text_preview(formats: &[Format]) -> Option<String> {
    let raw = match find(formats, MIME_TEXT) {
        Some(f) => String::from_utf8_lossy(&f.data).into_owned(),
        None => strip_tags(&String::from_utf8_lossy(&find(formats, MIME_HTML)?.data)),
    };
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(collapsed.chars().take(PREVIEW_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(mime: &str, data: &[u8]) -> Format {
        Format { mime: mime.into(), data: data.to_vec() }
    }

    #[test]
    fn kind_prefers_files_then_image_then_html_then_text() {
        let all = vec![
            f("text/plain;charset=utf-8", b"t"),
            f("text/html", b"h"),
            f("image/png", b"i"),
            f("text/uri-list", b"u"),
        ];
        assert_eq!(kind_of(&all), Some(ClipKind::Files));
        assert_eq!(kind_of(&all[..3]), Some(ClipKind::Image));
        assert_eq!(kind_of(&all[..2]), Some(ClipKind::RichText));
        assert_eq!(kind_of(&all[..1]), Some(ClipKind::Text));
        assert_eq!(kind_of(&[]), None);
    }

    #[test]
    fn hash_ignores_format_order() {
        let a = f("text/plain;charset=utf-8", b"x");
        let b = f("text/html", b"<b>x</b>");
        assert_eq!(content_hash(&[a.clone(), b.clone()]), content_hash(&[b, a]));
    }

    #[test]
    fn hash_differs_when_bytes_differ() {
        let a = [f("text/plain;charset=utf-8", b"x")];
        let b = [f("text/plain;charset=utf-8", b"y")];
        assert_ne!(content_hash(&a), content_hash(&b));
    }

    #[test]
    fn preview_collapses_whitespace_and_truncates_to_200() {
        let t = [f("text/plain;charset=utf-8", b"a\n\n  b")];
        assert_eq!(text_preview(&t).as_deref(), Some("a b"));
        let long = [f("text/plain;charset=utf-8", "x".repeat(500).as_bytes())];
        assert_eq!(text_preview(&long), Some("x".repeat(200)));
    }

    #[test]
    fn preview_strips_html_when_no_plain_text() {
        let h = [f("text/html", b"<p>Hi <b>there</b></p>")];
        assert_eq!(text_preview(&h).as_deref(), Some("Hi there"));
    }
}
