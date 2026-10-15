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
