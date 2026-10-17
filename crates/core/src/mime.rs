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
    pub const KDE_CUT_MARKER: &str = "application/x-kde-cutselection";
    pub const KDE_URI_LIST: &str = "application/x-kde4-urilist";

    /// The plain text types, most preferred first. `copy` offers all of them.
    pub const TEXT_MIME_TYPES: [&str; 5] = [TEXT_PLAIN_UTF8, UTF8_STRING, TEXT_PLAIN, STRING, TEXT];

    pub const PREFERRED_TXT_LIKE_MIME_TYPES: [&str; 2] = [TEXT_HTML, TEXT_MARKDOWN];

    pub const PREFERRED_FILE_LIST_MIME_TYPES: [&str; 4] = [
        TEXT_URI_LIST,
        KDE_URI_LIST,
        KDE_CUT_MARKER,
        GNOME_COPIED_FILES,
    ];

    pub const PREFERRED_IMAGE_MIME_TYPES: [&str; 1] = [IMAGE_PNG];

    pub const KNOWN_MIME_TYPES: [&str; 14] = [
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
    Text(usize),
    TextLike(usize),
    /// Text in an encoding a terminal can't show (UTF-16): ranked below HTML and other `text/` types.
    TextExtended,
    FileList(usize),
    Image(usize),
    Other,
    Password,
}

fn position(list: &[&str], m: &str) -> Option<usize> {
    list.iter().position(|p| *p == m)
}

pub fn classify_mime(m: &str) -> MimeKind {
    if let Some(r) = position(&types::TEXT_MIME_TYPES, m) {
        MimeKind::Text(r)
    } else if let Some(r) = position(&types::PREFERRED_TXT_LIKE_MIME_TYPES, m) {
        MimeKind::TextLike(r)
    } else if let Some(r) = position(&types::PREFERRED_FILE_LIST_MIME_TYPES, m) {
        MimeKind::FileList(r)
    } else if let Some(r) = position(&types::PREFERRED_IMAGE_MIME_TYPES, m) {
        MimeKind::Image(r)
    } else if m == types::TEXT_PLAIN_UTF16 {
        MimeKind::TextExtended
    } else if m == KDE_PASSWDMANAGER_HINT {
        MimeKind::Password
    } else if m.starts_with("text/") {
        MimeKind::TextLike(types::PREFERRED_TXT_LIKE_MIME_TYPES.len())
    } else if m.starts_with("image/") {
        MimeKind::Image(types::PREFERRED_IMAGE_MIME_TYPES.len())
    } else {
        MimeKind::Other
    }
}

pub fn pick_best_mime<S: AsRef<str>>(mimes: &[S]) -> Option<&S> {
    mimes
        .iter()
        .map(|m| (classify_mime(m.as_ref()), m))
        .filter(|(k, _)| *k != MimeKind::Password)
        .min_by_key(|(k, _)| *k)
        .map(|(_, m)| m)
}

pub fn pick_fetch_mimes<S: AsRef<str> + Clone>(mimes: &[S], cap_known: usize) -> Vec<S> {
    let (mut alias, mut image) = (None::<(MimeKind, usize)>, None::<(MimeKind, usize)>);
    let mut files = Vec::new();
    // let mut rest = Vec::new();
    for (i, m) in mimes.iter().enumerate() {
        match classify_mime(m.as_ref()) {
            MimeKind::Password => return Vec::new(),
            k @ (MimeKind::Text(_) | MimeKind::TextLike(_) | MimeKind::TextExtended)
                if alias.is_none_or(|(b, _)| k < b) =>
            {
                alias = Some((k, i))
            }
            k @ MimeKind::Image(_) if image.is_none_or(|(b, _)| k < b) => image = Some((k, i)),
            MimeKind::FileList(_) => files.push(i),
            // MimeKind::Other => rest.push(i),
            _ => {} // a lower-ranked alias or image
        }
    }
    let result: Vec<S> = alias
        .into_iter()
        .chain(image)
        .map(|(_, i)| i)
        .chain(files)
        .take(cap_known)
        // .chain(rest)
        .map(|i| mimes[i].clone())
        .collect();

    log_debug!("Picked {} fetch MIMEs out of {}", result.len(), mimes.len());
    result
}

pub fn parse_mime(bytes: &[u8]) -> std::io::Result<&str> {
    // Theoretical max given 4KB WL buffer - message: 8-byte header + 4-byte length + string + NUL, padded to 4 bytes.
    const MAX_LENGTH: usize = 4000;

    let value = std::str::from_utf8(bytes).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Failed to parse mime: {}", e),
        )
    })?;

    if value.is_empty() || value.len() > MAX_LENGTH || value.chars().any(|b| b.is_control()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid mime type - must be non-empty, at most {MAX_LENGTH} bytes, and contain no control characters"
            ),
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
