use std::cmp::Ordering;

use crate::{log_debug, mime_types::KDE_PASSWDMANAGER_HINT};

pub mod types {
    pub const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
    pub const TEXT_PLAIN_UTF16: &str = "text/plain;charset=utf-16";
    pub const UTF8_STRING: &str = "UTF8_STRING";
    pub const TEXT_PLAIN: &str = "text/plain";
    pub const TEXT_MARKDOWN: &str = "text/markdown";
    pub const STRING: &str = "STRING";
    pub const TEXT: &str = "TEXT";
    pub const TEXT_HTML: &str = "text/html";
    pub const TEXT_URI_LIST: &str = "text/uri-list";
    pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
    pub const KDE_PASSWDMANAGER_HINT: &str = "x-kde-passwordManagerHint";
    pub const IMAGE_PNG: &str = "image/png";
    pub const IMAGE_JPEG: &str = "image/jpeg";

    pub const KDE_CUT_MARKER: &str = "application/x-kde-cutselection";
    pub const KDE_URI_LIST: &str = "application/x-kde4-urilist";

    /// The plain text types, most preferred first. `copy` offers all of them.
    pub const TEXT_MIME_TYPES: [&str; 5] = [TEXT_PLAIN_UTF8, UTF8_STRING, TEXT_PLAIN, STRING, TEXT];

    pub const PREFERRED_TXT_LIKE_MIME_TYPES: [&str; 2] = [TEXT_HTML, TEXT_MARKDOWN];

    pub const PREFERRED_FILE_LIST_MIME_TYPES: [&str; 3] = [TEXT_URI_LIST, KDE_URI_LIST, GNOME_COPIED_FILES];

    pub const FILE_MIME_MODIFIER_TYPES: [&str; 1] = [KDE_CUT_MARKER];

    pub const PREFERRED_IMAGE_MIME_TYPES: [&str; 2] = [IMAGE_PNG, IMAGE_JPEG];

    pub const KNOWN_MIME_TYPES: [&str; 15] = [
        // text
        TEXT_PLAIN_UTF8,
        UTF8_STRING,
        TEXT_PLAIN,
        TEXT_PLAIN_UTF16,
        STRING,
        TEXT,
        TEXT_HTML,
        TEXT_MARKDOWN,
        //image
        IMAGE_PNG,
        IMAGE_JPEG,
        //file,
        GNOME_COPIED_FILES,
        TEXT_URI_LIST,
        KDE_CUT_MARKER,
        KDE_URI_LIST,
        // password
        KDE_PASSWDMANAGER_HINT,
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum MimeKind {
    Text,
    TextLike,
    TextExtended,
    FileList,
    Image,
    Other,
    Password,
    FileModifier,
}

#[derive(Debug, Clone)]
pub struct Mime<T: AsRef<str>> {
    rank: Option<usize>,
    kind: MimeKind,
    mime: T,
}

impl<T: AsRef<str>> Mime<T> {
    pub fn is_preferred<U: AsRef<str>>(&self, other: &Mime<U>) -> bool {
        matches!(self.cmp_with(other), Ordering::Less)
    }

    pub fn new(mime: T, kind: MimeKind, rank: Option<usize>) -> Self {
        Self { kind, mime, rank }
    }

    pub fn mime(&self) -> &T {
        &self.mime
    }

    pub fn kind(&self) -> MimeKind {
        self.kind
    }

    pub fn cmp_with<U: AsRef<str>>(&self, other: &Mime<U>) -> Ordering {
        self.kind.cmp(&other.kind).then(self.rank.cmp(&other.rank))
    }
}

impl<T> Mime<&T>
where
    T: AsRef<str> + Clone,
{
    pub fn as_cloned_mime(&self) -> Mime<T> {
        Mime {
            kind: self.kind,
            mime: self.mime.clone(),
            rank: self.rank,
        }
    }
}

fn position(list: &[&str], m: &str) -> Option<usize> {
    list.iter().position(|p| *p == m)
}

pub fn classify_mime<S>(m: S) -> Mime<S>
where
    S: AsRef<str>,
{
    let str = m.as_ref();
    if let Some(r) = position(&types::TEXT_MIME_TYPES, str) {
        Mime {
            kind: MimeKind::Text,
            rank: Some(r),
            mime: m,
        }
    } else if let Some(r) = position(&types::PREFERRED_TXT_LIKE_MIME_TYPES, str) {
        Mime {
            kind: MimeKind::TextLike,
            rank: Some(r),
            mime: m,
        }
    } else if let Some(r) = position(&types::PREFERRED_FILE_LIST_MIME_TYPES, str) {
        Mime {
            kind: MimeKind::FileList,
            rank: Some(r),
            mime: m,
        }
    } else if let Some(r) = position(&types::PREFERRED_IMAGE_MIME_TYPES, str) {
        Mime {
            kind: MimeKind::Image,
            rank: Some(r),
            mime: m,
        }
    } else if let Some(r) = position(&types::FILE_MIME_MODIFIER_TYPES, str) {
        Mime {
            kind: MimeKind::FileModifier,
            rank: Some(r),
            mime: m,
        }
    } else if str == types::TEXT_PLAIN_UTF16 {
        Mime {
            kind: MimeKind::TextExtended,
            rank: None,
            mime: m,
        }
    } else if str == KDE_PASSWDMANAGER_HINT {
        Mime {
            kind: MimeKind::Password,
            rank: None,
            mime: m,
        }
    } else if str.starts_with("text/") {
        Mime {
            kind: MimeKind::TextLike,
            rank: Some(types::PREFERRED_TXT_LIKE_MIME_TYPES.len()),
            mime: m,
        }
    } else if str.starts_with("image/") {
        Mime {
            kind: MimeKind::Image,
            rank: Some(types::PREFERRED_IMAGE_MIME_TYPES.len()),
            mime: m,
        }
    } else {
        Mime {
            kind: MimeKind::Other,
            rank: None,
            mime: m,
        }
    }
}

pub fn pick_best_mime<S: AsRef<str>>(mimes: &[S]) -> Option<Mime<&S>> {
    mimes
        .iter()
        .map(classify_mime)
        .filter(|k| k.kind != MimeKind::Password)
        .min_by(|a, b| a.cmp_with(b))
}

pub fn pick_fetch_mimes<S: AsRef<str> + Clone>(mimes: &[S], cap_known: usize) -> Vec<Mime<S>> {
    let (mut text, mut image) = (None::<Mime<&S>>, None::<Mime<&S>>);
    let mut files = Vec::new();
    let mut file_modifier = None::<Mime<&S>>;
    // let mut rest = Vec::new();
    for m in mimes.iter() {
        match classify_mime(m) {
            Mime {
                kind: MimeKind::Password, ..
            } => return Vec::new(),
            k @ Mime { kind, .. }
                if (kind == MimeKind::Text || kind == MimeKind::TextLike || kind == MimeKind::TextExtended)
                    && text.as_ref().is_none_or(|a| k.is_preferred(a)) =>
            {
                text = Some(k)
            }
            k @ Mime { kind, .. } if kind == MimeKind::Image && image.as_ref().is_none_or(|b| k.is_preferred(b)) => image = Some(k),
            k @ Mime {
                kind: MimeKind::FileList, ..
            } => files.push(k),
            k @ Mime {
                kind: MimeKind::FileModifier,
                ..
            } if file_modifier.as_ref().is_none_or(|a| k.is_preferred(a)) => file_modifier = Some(k),
            // MimeKind::Other => rest.push(i),
            _ => {} // a lower-ranked alias or image
        }
    }
    let modifier = file_modifier.filter(|_| !files.is_empty());
    let cap = cap_known.saturating_sub(usize::from(modifier.is_some()));
    let mut result: Vec<Mime<S>> = text.into_iter().chain(image).chain(files).take(cap).map(|m| m.as_cloned_mime()).collect();
    if let Some(modifier) = modifier
        && result.iter().any(|m| m.kind == MimeKind::FileList)
    {
        result.push(modifier.as_cloned_mime());
    }

    log_debug!("Picked {} fetch MIMEs out of {}", result.len(), mimes.len());
    result
}

pub fn parse_mime(bytes: &[u8]) -> std::io::Result<&str> {
    // Theoretical max given 4KB WL buffer - message: 8-byte header + 4-byte length + string + NUL, padded to 4 bytes.
    const MAX_LENGTH: usize = 4000;

    let value =
        std::str::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Failed to parse mime: {}", e)))?;

    if value.is_empty() || value.len() > MAX_LENGTH || value.chars().any(|b| b.is_control()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid mime type - must be non-empty, at most {MAX_LENGTH} bytes, and contain no control characters"),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_world_types_are_accepted_as_is() {
        let libreoffice = r#"application/x-openoffice-objectdescriptor-xml;windows_formatname="Star Object Descriptor (XML)";displayname="file:///tmp/Résumé.docx""#;
        for mime in [
            "text/plain;charset=utf-8",
            "UTF8_STRING",
            r#"application/x-qt-windows-mime;value="Rich Text Format""#,
            libreoffice,
            &"a".repeat(4000),
        ] {
            assert_eq!(parse_mime(mime.as_bytes()).unwrap(), mime);
        }
    }

    #[test]
    fn empty_oversized_invalid_utf8_and_control_characters_are_rejected() {
        let too_long = "a".repeat(4001);
        for bytes in [
            &b""[..],
            too_long.as_bytes(),
            b"text/\xff",
            b"text/\tplain",
            b"text/\x7f",
            b"text/\x1b[31m",
            "text/\u{9b}31m".as_bytes(), // C1 CSI: an escape sequence on some terminals
        ] {
            let err = parse_mime(bytes).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{bytes:?}");
        }
    }
}
