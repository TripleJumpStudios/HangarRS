//! A bounded byte scan for engine signatures.
//!
//! # Both encodings, always
//!
//! Half of what identifies an installer lives in its resource section, which
//! is UTF-16. A plain ASCII scan of Just Flight's `setup.exe` finds
//! `InstallShield` and misses `Setup Player` entirely — the first is a literal
//! in the code and the second is a version string two bytes per character.
//! Searching for one encoding and concluding from its absence is how a file
//! that is unmistakably InstallShield gets reported as unknown.
//!
//! # Bounded on purpose
//!
//! An installer is mostly payload, and a signature found four hundred
//! megabytes into a compressed archive says something about the archive rather
//! than about the installer. Every signature observed in real installers is
//! inside the first megabyte:
//!
//! | File | Signature | Offset |
//! |---|---|---|
//! | Project Airports Setup 0.1.33 | `Nullsoft Install System` | 70,606 |
//! | Garmin Trainer 3.12.1.0 | `WixBundle` (UTF-16) | 302,456 |
//! | .NET Desktop Runtime 8.0.29 | `WixBundle` (UTF-16) | 306,896 |
//! | Just Flight setup.exe | `Setup Player` (UTF-16) | 798,986 |
//!
//! [`WINDOW`] is four times the largest of those, which keeps the scan to a
//! few milliseconds on a two-gigabyte file.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::InstallError;

/// How far into a file signatures are looked for.
pub const WINDOW: usize = 4 * 1024 * 1024;

/// How the pattern was written in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Ascii,
    Utf16Le,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Ascii => "ascii",
            Encoding::Utf16Le => "utf-16",
        }
    }
}

/// One signature found, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub needle: &'static str,
    pub encoding: Encoding,
    pub offset: usize,
}

/// Read the leading [`WINDOW`] of a file, or all of it if it is shorter.
pub fn window(path: &Path) -> Result<Vec<u8>, InstallError> {
    let mut file = File::open(path).map_err(|source| InstallError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut buf = Vec::new();
    file.by_ref()
        .take(WINDOW as u64)
        .read_to_end(&mut buf)
        .map_err(|source| InstallError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(buf)
}

/// Every needle found in `buf`, in both encodings, first occurrence only.
pub fn find(buf: &[u8], needles: &[&'static str]) -> Vec<Hit> {
    let mut hits = Vec::new();
    for needle in needles {
        if let Some(offset) = find_bytes(buf, needle.as_bytes()) {
            hits.push(Hit {
                needle,
                encoding: Encoding::Ascii,
                offset,
            });
        }
        let wide: Vec<u8> = needle
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        if let Some(offset) = find_bytes(buf, &wide) {
            hits.push(Hit {
                needle,
                encoding: Encoding::Utf16Le,
                offset,
            });
        }
    }
    hits.sort_by_key(|h| h.offset);
    hits
}

/// Was this needle found in either encoding?
pub fn saw(hits: &[Hit], needle: &str) -> bool {
    hits.iter().any(|h| h.needle == needle)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn finds_a_signature_in_either_encoding() {
        let mut buf = b"padding".to_vec();
        buf.extend_from_slice(b"Nullsoft Install System");
        buf.extend_from_slice(&wide("Setup Player"));

        let hits = find(&buf, &["Nullsoft Install System", "Setup Player"]);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].needle, "Nullsoft Install System");
        assert_eq!(hits[0].encoding, Encoding::Ascii);
        assert_eq!(hits[1].needle, "Setup Player");
        assert_eq!(hits[1].encoding, Encoding::Utf16Le);
    }

    /// The mistake this exists to prevent: `Setup Player` lives in Just
    /// Flight's resource section as UTF-16, so an ASCII-only scan finds
    /// nothing and reports a file that is unmistakably InstallShield as
    /// unrecognised.
    #[test]
    fn an_ascii_only_scan_would_miss_the_resource_strings() {
        let buf = wide("Setup Player");
        assert!(saw(&find(&buf, &["Setup Player"]), "Setup Player"));
        assert!(!buf.windows(12).any(|w| w == b"Setup Player"));
    }

    #[test]
    fn hits_come_back_in_file_order() {
        let mut buf = vec![0u8; 64];
        buf.extend_from_slice(b"Squirrel");
        buf.extend_from_slice(&[0u8; 64]);
        buf.extend_from_slice(b"WixBundle");
        let hits = find(&buf, &["WixBundle", "Squirrel"]);
        assert!(hits[0].offset < hits[1].offset);
        assert_eq!(hits[0].needle, "Squirrel");
    }

    #[test]
    fn nothing_found_is_not_an_error() {
        assert!(find(b"", &["Squirrel"]).is_empty());
        assert!(find(b"short", &["a much longer needle"]).is_empty());
        assert!(!saw(&[], "Squirrel"));
    }
}
