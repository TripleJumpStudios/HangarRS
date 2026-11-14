//! `EXE.xml` — the in-sim add-on launch list.
//!
//! MSFS spawns every enabled `<Launch.Addon>` itself, as a child of the sim.
//! That is the whole reason this file needs its own item kind: an add-on
//! registered here is **already going to run**, so adding it to an app runner
//! starts a second, conflicting copy.
//!
//! The file sits beside `UserCfg.opt` in the prefix and declares
//! `encoding="Windows-1252"`, which is why it is decoded by
//! [`crate::xml_encoding`] rather than read with `read_to_string`.
//!
//! Read-only for now. Toggling `<Disabled>` writes into the live prefix and
//! waits on explicit approval.

use crate::{MsfsError, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::path::{Path, PathBuf};

/// One `<Launch.Addon>` entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchAddon {
    pub name: String,
    /// Windows path to the executable, exactly as written.
    pub path: String,
    pub command_line: Option<String>,
    /// `<Disabled>True</Disabled>` — the sim skips this entry.
    pub disabled: bool,
    /// `<ManualLoad>` — the sim loads it on request rather than at start.
    pub manual_load: bool,
    pub new_console: bool,
}

impl LaunchAddon {
    /// Where the executable lives on this filesystem, inside `prefix`.
    pub fn resolve_in_prefix(&self, prefix: &Path) -> Option<PathBuf> {
        crate::env::win_path_in_prefix(prefix, &self.path)
    }
}

/// A parsed `EXE.xml`.
#[derive(Debug, Clone, Default)]
pub struct ExeXml {
    /// Document-level `<Disabled>` — when true the sim loads none of them.
    pub disabled: bool,
    pub addons: Vec<LaunchAddon>,
}

impl ExeXml {
    /// Read and parse `EXE.xml`.
    ///
    /// A missing file is not an error: it simply means no in-sim add-ons are
    /// registered, which is the normal state for most installs.
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Self::parse(&bytes).map_err(|reason| MsfsError::Malformed {
                path: path.to_path_buf(),
                reason,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(MsfsError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Parse `EXE.xml` bytes, honouring the declared encoding.
    ///
    /// Byte-oriented rather than `&str`: the file declares Windows-1252, and
    /// handing it a `&str` would require the caller to have decoded it
    /// correctly first, which is the part that goes wrong.
    pub fn parse(bytes: &[u8]) -> std::result::Result<Self, String> {
        let (text, _encoding) = crate::xml_encoding::decode(bytes);
        let mut reader = Reader::from_str(&text);
        reader.config_mut().trim_text(true);

        let mut doc = ExeXml::default();
        let mut current: Option<LaunchAddon> = None;
        // Which element's text we are collecting. Document-level and per-addon
        // elements share names (`<Disabled>`), so depth alone is not enough —
        // `current` says which one we are inside.
        let mut element = String::new();
        // Tracked so a truncated file is an error rather than partial data.
        // quick-xml reports EOF happily with elements still open, and silently
        // accepting half a launch list is the wrong answer for a file this app
        // may later write back.
        let mut depth: i32 = 0;

        loop {
            match reader.read_event() {
                Err(e) => return Err(format!("at position {}: {e}", reader.buffer_position())),
                Ok(Event::Eof) => {
                    if depth != 0 {
                        return Err(format!("truncated: {depth} element(s) left unclosed"));
                    }
                    break;
                }

                Ok(Event::Start(e)) => {
                    depth += 1;
                    // `local_name` strips a namespace prefix up to the first
                    // ':'. `Launch.Addon` contains a dot, not a colon, so it
                    // survives intact.
                    let name = e.local_name().as_ref().to_owned();
                    if name == "Launch.Addon" {
                        current = Some(LaunchAddon::default());
                    }
                    element = name;
                }

                Ok(Event::End(e)) => {
                    depth -= 1;
                    if e.local_name().as_ref() == "Launch.Addon" {
                        if let Some(addon) = current.take() {
                            doc.addons.push(addon);
                        }
                    }
                    element.clear();
                }

                Ok(Event::Text(e)) => {
                    // XML 1.0 line-ending normalisation; the file is `version="1.0"`.
                    let text = e.xml10_content().trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    match current.as_mut() {
                        Some(addon) => match element.as_str() {
                            "Name" => addon.name = text,
                            "Path" => addon.path = text,
                            "CommandLine" => addon.command_line = Some(text),
                            "Disabled" => addon.disabled = is_true(&text),
                            "ManualLoad" => addon.manual_load = is_true(&text),
                            "NewConsole" => addon.new_console = is_true(&text),
                            _ => {}
                        },
                        None => {
                            if element == "Disabled" {
                                doc.disabled = is_true(&text);
                            }
                        }
                    }
                }

                _ => {}
            }
        }

        Ok(doc)
    }

    /// Add-ons the sim will actually start.
    pub fn enabled(&self) -> impl Iterator<Item = &LaunchAddon> {
        let doc_disabled = self.disabled;
        self.addons
            .iter()
            .filter(move |a| !doc_disabled && !a.disabled)
    }
}

/// MSFS writes `True`/`False`; be liberal about what is accepted.
fn is_true(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real file from this machine, byte for byte.
    const REAL: &str = r#"<?xml version="1.0" encoding="Windows-1252" ?>
<SimBase.Document Type="Launch" version="1,0">
    <Descr>Launch</Descr>
    <Filename>EXE.xml</Filename>
    <Disabled>False</Disabled>
    <Launch.ManualLoad>False</Launch.ManualLoad>
    <Launch.Addon>
        <Disabled>False</Disabled>
        <ManualLoad>False</ManualLoad>
        <Name>TDS Sim Software GTNXi Gauge Integration</Name>
        <Path>C:\ProgramData\TDS\GTNXi\FlightSimEXE\TDSGTNXiFlightSimEXE.exe</Path>
        <CommandLine>-FSIntegration</CommandLine>
        <NewConsole>True</NewConsole>
    </Launch.Addon>
</SimBase.Document>"#;

    #[test]
    fn parses_the_real_file() {
        let doc = ExeXml::parse(REAL.as_bytes()).unwrap();
        assert!(!doc.disabled);
        assert_eq!(doc.addons.len(), 1);

        let a = &doc.addons[0];
        assert_eq!(a.name, "TDS Sim Software GTNXi Gauge Integration");
        assert_eq!(
            a.path,
            r"C:\ProgramData\TDS\GTNXi\FlightSimEXE\TDSGTNXiFlightSimEXE.exe"
        );
        assert_eq!(a.command_line.as_deref(), Some("-FSIntegration"));
        assert!(!a.disabled);
        assert!(!a.manual_load);
        assert!(a.new_console);
    }

    /// The document-level `<Disabled>` sits at the same element name as the
    /// per-addon one. Confusing the two would report every add-on as disabled.
    #[test]
    fn document_and_addon_disabled_are_not_confused() {
        let doc = ExeXml::parse(REAL.as_bytes()).unwrap();
        assert!(!doc.disabled);
        assert!(!doc.addons[0].disabled);

        let off = REAL.replacen("<Disabled>False</Disabled>", "<Disabled>True</Disabled>", 1);
        let doc = ExeXml::parse(off.as_bytes()).unwrap();
        assert!(doc.disabled, "the document-level flag should be set");
        assert!(
            !doc.addons[0].disabled,
            "the add-on's own flag is untouched"
        );
        assert_eq!(doc.enabled().count(), 0, "a disabled document runs nothing");
    }

    #[test]
    fn a_disabled_addon_is_excluded_from_enabled() {
        let xml = REAL.replace(
            "<Disabled>False</Disabled>\n        <ManualLoad>",
            "<Disabled>True</Disabled>\n        <ManualLoad>",
        );
        let doc = ExeXml::parse(xml.as_bytes()).unwrap();
        assert!(doc.addons[0].disabled);
        assert_eq!(doc.enabled().count(), 0);
    }

    #[test]
    fn several_addons_all_parse() {
        let xml = r#"<?xml version="1.0" encoding="Windows-1252" ?>
<SimBase.Document Type="Launch" version="1,0">
    <Disabled>False</Disabled>
    <Launch.Addon>
        <Disabled>False</Disabled><Name>One</Name><Path>C:\a.exe</Path>
    </Launch.Addon>
    <Launch.Addon>
        <Disabled>True</Disabled><Name>Two</Name><Path>C:\b.exe</Path>
    </Launch.Addon>
</SimBase.Document>"#;
        let doc = ExeXml::parse(xml.as_bytes()).unwrap();
        assert_eq!(doc.addons.len(), 2);
        assert_eq!(
            doc.enabled().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["One"]
        );
    }

    /// The declared encoding is Windows-1252, not UTF-8. A vendor name with an
    /// accent must survive; decoding those bytes as UTF-8 fails outright.
    #[test]
    fn windows_1252_text_decodes() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            br#"<?xml version="1.0" encoding="Windows-1252" ?>
<SimBase.Document Type="Launch" version="1,0">
    <Launch.Addon><Name>Caf"#,
        );
        bytes.push(0xE9); // 'é' in Windows-1252
        bytes.extend_from_slice(
            br#" Sim</Name><Path>C:\x.exe</Path></Launch.Addon>
</SimBase.Document>"#,
        );

        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "the fixture must genuinely not be valid UTF-8"
        );
        let doc = ExeXml::parse(&bytes).unwrap();
        assert_eq!(doc.addons[0].name, "Café Sim");
    }

    #[test]
    fn a_missing_file_means_no_addons() {
        let doc = ExeXml::read(Path::new("/nonexistent/EXE.xml")).unwrap();
        assert!(doc.addons.is_empty());
        assert!(!doc.disabled);
    }

    /// Broken and truncated files must be reported, not silently half-parsed.
    #[test]
    fn malformed_and_truncated_xml_are_errors() {
        // Mismatched end tag.
        assert!(ExeXml::parse(b"<SimBase.Document></Wrong>").is_err());
        // Truncated mid-document: quick-xml reports EOF without complaint, so
        // this is caught by the depth counter.
        let err = ExeXml::parse(b"<SimBase.Document><Launch.Addon>").unwrap_err();
        assert!(err.contains("truncated"), "got {err:?}");
        // A file cut off part-way through the real content.
        let half = &REAL.as_bytes()[..REAL.len() / 2];
        assert!(ExeXml::parse(half).is_err());
    }

    /// A well-formed document must not be mistaken for a truncated one — the
    /// depth counter has to handle self-closing elements, which are emitted as
    /// a single event rather than a start and an end.
    #[test]
    fn self_closing_elements_do_not_upset_the_depth_counter() {
        let xml = r#"<?xml version="1.0" ?>
<SimBase.Document>
    <Disabled>False</Disabled>
    <Launch.ManualLoad/>
    <Launch.Addon><Name>A</Name><Path>C:\a.exe</Path></Launch.Addon>
</SimBase.Document>"#;
        let doc = ExeXml::parse(xml.as_bytes()).unwrap();
        assert_eq!(doc.addons.len(), 1);
    }

    #[test]
    fn boolean_spellings_are_accepted_liberally() {
        for (text, expected) in [
            ("True", true),
            ("true", true),
            ("1", true),
            ("False", false),
            ("false", false),
            ("0", false),
            ("", false),
        ] {
            assert_eq!(is_true(text), expected, "for {text:?}");
        }
    }

    #[test]
    fn an_addon_path_resolves_into_the_prefix() {
        let doc = ExeXml::parse(REAL.as_bytes()).unwrap();
        let resolved = doc.addons[0]
            .resolve_in_prefix(Path::new("/prefix"))
            .unwrap();
        assert_eq!(
            resolved,
            Path::new(
                "/prefix/drive_c/ProgramData/TDS/GTNXi/FlightSimEXE/TDSGTNXiFlightSimEXE.exe"
            )
        );
    }
}
