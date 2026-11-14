//! Decoding XML that is not UTF-8.
//!
//! MSFS writes `EXE.xml` as `encoding="Windows-1252"`. Decoding those bytes as
//! UTF-8 fails outright the moment a vendor name carries an accent, and the
//! failure is a parse error rather than mojibake, so it takes the whole file
//! down.
//!
//! `quick-xml`'s `encoding` feature does read the declaration, but it decodes
//! the declaration itself before switching, and on the buffered path the
//! switch did not reach our text events. Doing it explicitly is a few lines,
//! is testable, and hands back **which** encoding was used — which the
//! bindings writer needs, because a binding file has to be written back in the encoding it
//! arrived in for the round-trip to be byte-identical.

use encoding_rs::Encoding;

/// How many bytes to scan for the declaration. The XML declaration must be
/// first in the document, and is far shorter than this.
const DECL_SCAN: usize = 512;

/// Decode XML bytes, returning the text and the encoding used.
///
/// Order of precedence, matching the XML specification: a byte-order mark
/// wins, then the `encoding=` pseudo-attribute of the declaration, then UTF-8.
pub fn decode(bytes: &[u8]) -> (String, &'static Encoding) {
    if let Some((encoding, without_bom)) = strip_bom(bytes) {
        let (text, _, _) = encoding.decode(without_bom);
        return (text.into_owned(), encoding);
    }

    let encoding = declared_encoding(bytes).unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    (text.into_owned(), encoding)
}

/// Detect and strip a byte-order mark.
fn strip_bom(bytes: &[u8]) -> Option<(&'static Encoding, &[u8])> {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => Some((encoding_rs::UTF_8, rest)),
        [0xFF, 0xFE, rest @ ..] => Some((encoding_rs::UTF_16LE, rest)),
        [0xFE, 0xFF, rest @ ..] => Some((encoding_rs::UTF_16BE, rest)),
        _ => None,
    }
}

/// Read `encoding="…"` out of the XML declaration.
///
/// Scanned over raw bytes rather than a decoded string, because decoding is the
/// thing this is trying to decide. The declaration is ASCII by definition of
/// the format, so a lossy read of the first block is safe here.
pub fn declared_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = &bytes[..bytes.len().min(DECL_SCAN)];
    let head = String::from_utf8_lossy(head);

    let decl_end = head.find("?>")?;
    let decl = &head[..decl_end];
    if !decl.trim_start().starts_with("<?xml") {
        return None;
    }

    let at = decl.find("encoding")?;
    let rest = &decl[at + "encoding".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = rest[1..].split(quote).next()?;

    Encoding::for_label(value.trim().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_declared_encoding() {
        let d = declared_encoding(br#"<?xml version="1.0" encoding="Windows-1252" ?><a/>"#);
        assert_eq!(d, Some(encoding_rs::WINDOWS_1252));
    }

    #[test]
    fn accepts_single_quotes_and_odd_spacing() {
        for decl in [
            br#"<?xml version='1.0' encoding='windows-1252'?><a/>"#.to_vec(),
            br#"<?xml version="1.0" encoding = "WINDOWS-1252" ?><a/>"#.to_vec(),
        ] {
            assert_eq!(
                declared_encoding(&decl),
                Some(encoding_rs::WINDOWS_1252),
                "{decl:?}"
            );
        }
    }

    #[test]
    fn no_declaration_means_no_opinion() {
        assert_eq!(declared_encoding(b"<a/>"), None);
        assert_eq!(declared_encoding(br#"<?xml version="1.0"?><a/>"#), None);
        assert_eq!(declared_encoding(b""), None);
        // A bare processing instruction is not a declaration.
        assert_eq!(declared_encoding(br#"<?php encoding="x" ?>"#), None);
    }

    /// The case that matters: 0xE9 is 'é' in Windows-1252 and invalid UTF-8.
    #[test]
    fn windows_1252_high_bytes_decode() {
        let mut bytes = br#"<?xml version="1.0" encoding="Windows-1252" ?><n>Caf"#.to_vec();
        bytes.push(0xE9);
        bytes.extend_from_slice(b"</n>");

        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "fixture must not be UTF-8"
        );
        let (text, encoding) = decode(&bytes);
        assert_eq!(encoding, encoding_rs::WINDOWS_1252);
        assert!(text.contains("Café"), "got {text:?}");
    }

    #[test]
    fn utf8_is_the_default() {
        let (text, encoding) = decode("<n>Café</n>".as_bytes());
        assert_eq!(encoding, encoding_rs::UTF_8);
        assert_eq!(text, "<n>Café</n>");
    }

    #[test]
    fn a_bom_overrides_the_declaration() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(
            r#"<?xml version="1.0" encoding="Windows-1252"?><n>Café</n>"#.as_bytes(),
        );
        let (text, encoding) = decode(&bytes);
        assert_eq!(encoding, encoding_rs::UTF_8);
        assert!(
            text.starts_with("<?xml"),
            "the BOM must be stripped: {text:?}"
        );
        assert!(text.contains("Café"));
    }

    #[test]
    fn an_unknown_encoding_label_falls_back_to_utf8() {
        let (_, encoding) = decode(br#"<?xml version="1.0" encoding="not-a-charset"?><n/>"#);
        assert_eq!(encoding, encoding_rs::UTF_8);
    }
}
