//! Does the sim's own prefix expose SimConnect over TCP? A read, never a write.
//!
//! MSFS listens on the ports its server config names, in
//! `<sim pfx>/drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024/SimConnect.xml`.
//! Without an IPv4 entry on the port `simconnect-rs` dials, companion apps and
//! HangarRS telemetry never connect, and nothing says why. This reads that file
//! and answers with exactly one [`Check`].
//!
//! The expected port is passed in rather than named here: it is
//! `simconnect_tcp::DEFAULT_PORT`, and `simconnect-tcp` is only a
//! dev-dependency of this crate. The app passes the constant; the tests below
//! use the same one.
//!
//! The parse is tolerant: a byte-order mark, a Windows-1252 byte, unknown
//! elements, several `<SimConnect.Comm>` blocks, and any case or surrounding
//! whitespace in tag names and values are all accepted. A block marked
//! `<Disabled>True</Disabled>` does not count.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Where the sim's SimConnect server config lives, relative to its `pfx/`.
pub const RELATIVE: &str =
    "drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator 2024/SimConnect.xml";

/// The loopback address a local IPv4 entry binds.
const LOOPBACK: &str = "127.0.0.1";

/// The server config for a sim prefix.
pub fn path(sim_prefix: &Path) -> PathBuf {
    sim_prefix.join(RELATIVE)
}

/// One `<SimConnect.Comm>` block, as written (trimmed). `None` is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comm {
    pub protocol: Option<String>,
    pub scope: Option<String>,
    pub address: Option<String>,
    pub port: Option<String>,
    pub disabled: bool,
}

impl Comm {
    /// IPv4, local scope or 127.0.0.1, on `port`, and not disabled.
    pub fn exposes(&self, port: u16) -> bool {
        let is = |v: &Option<String>, want: &str| {
            v.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(want))
        };
        !self.disabled
            && is(&self.protocol, "IPv4")
            && (is(&self.scope, "local") || is(&self.address, LOOPBACK))
            && self
                .port
                .as_deref()
                .and_then(|p| p.parse::<u16>().ok())
                .is_some_and(|p| p == port)
    }

    /// `IPv4 local 127.0.0.1:5557`, with `?` for what is absent.
    pub fn describe(&self) -> String {
        let q = |v: &Option<String>| v.clone().unwrap_or_else(|| "?".into());
        let mut line = format!(
            "{} {} {}:{}",
            q(&self.protocol),
            q(&self.scope),
            q(&self.address),
            q(&self.port)
        );
        if self.disabled {
            line.push_str(" (disabled)");
        }
        line
    }
}

/// Why the file could not be read as a SimConnect config.
#[derive(Debug, Error)]
pub enum ReadError {
    #[error("could not be read: {0}")]
    Io(#[from] std::io::Error),
    #[error("is not valid XML: {0}")]
    Xml(#[from] roxmltree::Error),
}

/// The answer: exactly one of four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// Present, with an entry exposing IPv4 locally on the expected port.
    Exposed { port: u16 },
    /// Present, but no entry does. `found` is every block it holds.
    NoMatch { port: u16, found: Vec<Comm> },
    /// No file.
    Missing,
    /// There, but unreadable or not parsable.
    Unreadable { why: String },
}

impl Check {
    pub fn is_exposed(&self) -> bool {
        matches!(self, Check::Exposed { .. })
    }

    /// A short state word for the Prefix tab.
    pub fn word(&self) -> &'static str {
        match self {
            Check::Exposed { .. } => "exposed",
            Check::NoMatch { .. } => "no local IPv4 entry",
            Check::Missing => "missing",
            Check::Unreadable { .. } => "unreadable",
        }
    }

    /// The value beside the word: what was found, in one line.
    pub fn detail(&self) -> String {
        match self {
            Check::Exposed { port } => format!("IPv4 {LOOPBACK}:{port}"),
            Check::NoMatch { found, .. } if found.is_empty() => "no SimConnect.Comm entry".into(),
            Check::NoMatch { found, .. } => found
                .iter()
                .map(Comm::describe)
                .collect::<Vec<_>>()
                .join(", "),
            Check::Missing => String::new(),
            Check::Unreadable { why } => why.clone(),
        }
    }
}

/// Read the sim prefix's `SimConnect.xml` and judge it against `expected_port`.
pub fn check(sim_prefix: &Path, expected_port: u16) -> Check {
    let file = path(sim_prefix);
    match std::fs::read(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Check::Missing,
        Err(e) => Check::Unreadable {
            why: ReadError::from(e).to_string(),
        },
        Ok(bytes) => match parse(&String::from_utf8_lossy(&bytes)) {
            Ok(found) => judge(found, expected_port),
            Err(e) => Check::Unreadable { why: e.to_string() },
        },
    }
}

/// Exposed if any block exposes; otherwise every block found.
pub fn judge(found: Vec<Comm>, expected_port: u16) -> Check {
    if found.iter().any(|c| c.exposes(expected_port)) {
        Check::Exposed {
            port: expected_port,
        }
    } else {
        Check::NoMatch {
            port: expected_port,
            found,
        }
    }
}

/// Every `<SimConnect.Comm>` block in a config, wherever it sits.
pub fn parse(text: &str) -> Result<Vec<Comm>, ReadError> {
    let text = text.trim_start_matches('\u{feff}');
    let doc = roxmltree::Document::parse(text)?;
    let named = |n: &roxmltree::Node, want: &str| {
        n.is_element() && n.tag_name().name().trim().eq_ignore_ascii_case(want)
    };
    Ok(doc
        .descendants()
        .filter(|n| named(n, "SimConnect.Comm"))
        .map(|block| {
            let field = |want: &str| {
                block
                    .children()
                    .find(|c| named(c, want))
                    .and_then(|c| c.text())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
            };
            Comm {
                protocol: field("Protocol"),
                scope: field("Scope"),
                address: field("Address"),
                port: field("Port"),
                disabled: field("Disabled").is_some_and(|d| d.eq_ignore_ascii_case("true")),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use simconnect_tcp::DEFAULT_PORT;

    /// A fake sim prefix under the temp dir. Never the live one.
    fn scratch() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hangar-sim-simconnect-{}-{}/pfx",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_file(bytes: &[u8]) -> PathBuf {
        let pfx = scratch();
        let file = path(&pfx);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, bytes).unwrap();
        pfx
    }

    fn cleanup(pfx: &Path) {
        let _ = std::fs::remove_dir_all(pfx.parent().unwrap());
    }

    /// The shape of a real, working sim prefix's file.
    const REAL_SHAPE: &str = r#"<?xml version="1.0" encoding="Windows-1252"?>
<SimBase.Document Type="SimConnect" version="1,0">
    <Descr>SimConnect Server Configuration</Descr>
    <Filename>SimConnect.xml</Filename>
    <SimConnect.Comm>
        <Descr>Static IP4 port</Descr>
        <Protocol>IPv4</Protocol>
        <Scope>local</Scope>
        <Address>127.0.0.1</Address>
        <MaxClients>64</MaxClients>
        <Port>5557</Port>
    </SimConnect.Comm>
</SimBase.Document>
"#;

    #[test]
    fn the_expected_port_is_simconnect_rs_default() {
        assert_eq!(DEFAULT_PORT, 5557);
    }

    #[test]
    fn the_real_shape_is_exposed() {
        let pfx = with_file(REAL_SHAPE.as_bytes());
        let found = check(&pfx, DEFAULT_PORT);
        assert_eq!(found, Check::Exposed { port: 5557 });
        assert!(found.is_exposed());
        assert_eq!(found.detail(), "IPv4 127.0.0.1:5557");
        cleanup(&pfx);
    }

    #[test]
    fn a_missing_file_is_missing() {
        let pfx = scratch();
        assert_eq!(check(&pfx, DEFAULT_PORT), Check::Missing);
        cleanup(&pfx);
    }

    #[test]
    fn a_file_with_no_matching_entry_says_what_it_found() {
        let pfx = with_file(
            br#"<SimBase.Document><SimConnect.Comm>
                <Protocol>IPv6</Protocol><Scope>global</Scope>
                <Address>::</Address><Port>500</Port>
            </SimConnect.Comm></SimBase.Document>"#,
        );
        let found = check(&pfx, DEFAULT_PORT);
        let Check::NoMatch { port, found: comms } = &found else {
            panic!("{found:?}");
        };
        assert_eq!(*port, 5557);
        assert_eq!(comms.len(), 1);
        assert_eq!(found.detail(), "IPv6 global :::500");
        cleanup(&pfx);
    }

    #[test]
    fn the_right_entry_on_the_wrong_port_does_not_count() {
        let found = judge(
            parse(&REAL_SHAPE.replace("5557", "5558")).unwrap(),
            DEFAULT_PORT,
        );
        assert!(matches!(found, Check::NoMatch { .. }), "{found:?}");
        assert_eq!(found.detail(), "IPv4 local 127.0.0.1:5558");
    }

    #[test]
    fn a_disabled_entry_does_not_count() {
        let text = REAL_SHAPE.replace(
            "<Port>5557</Port>",
            "<Port>5557</Port><Disabled>True</Disabled>",
        );
        let found = judge(parse(&text).unwrap(), DEFAULT_PORT);
        assert!(matches!(found, Check::NoMatch { .. }), "{found:?}");
        assert!(found.detail().ends_with("(disabled)"));
    }

    #[test]
    fn a_document_with_no_comm_block_is_no_match() {
        let found = judge(parse("<SimBase.Document/>").unwrap(), DEFAULT_PORT);
        assert_eq!(found.detail(), "no SimConnect.Comm entry");
    }

    #[test]
    fn unparsable_xml_is_unreadable() {
        let pfx = with_file(b"<SimBase.Document><SimConnect.Comm>");
        let found = check(&pfx, DEFAULT_PORT);
        let Check::Unreadable { why } = &found else {
            panic!("{found:?}");
        };
        assert!(why.starts_with("is not valid XML"), "{why}");
        cleanup(&pfx);
    }

    #[test]
    fn a_directory_in_the_files_place_is_unreadable() {
        let pfx = scratch();
        std::fs::create_dir_all(path(&pfx)).unwrap();
        let found = check(&pfx, DEFAULT_PORT);
        assert!(matches!(found, Check::Unreadable { .. }), "{found:?}");
        cleanup(&pfx);
    }

    /// BOM, a Windows-1252 byte, unknown elements, case and whitespace, and
    /// the matching entry second of three.
    #[test]
    fn the_parse_is_tolerant() {
        let mut bytes = "\u{feff}".as_bytes().to_vec();
        bytes.extend_from_slice(
            br#"<?xml version="1.0" encoding="Windows-1252"?>
<SimBase.Document Type="SimConnect" version="1,0">
  <Descr>Caf"#,
        );
        bytes.push(0xE9); // Windows-1252 e-acute, invalid UTF-8
        bytes.extend_from_slice(
            br#"</Descr>
  <Unknown><Nested>x</Nested></Unknown>
  <SimConnect.Comm><Protocol>Pipe</Protocol><Address>.</Address><Port>Custom/SimConnect</Port></SimConnect.Comm>
  <simconnect.comm>
    <PROTOCOL>  ipv4 </PROTOCOL>
    <Scope>
      LOCAL
    </Scope>
    <Port> 5557 </Port>
    <Something>else</Something>
  </simconnect.comm>
  <SimConnect.Comm><Protocol>IPv4</Protocol><Scope>global</Scope><Address>0.0.0.0</Address><Port>500</Port></SimConnect.Comm>
</SimBase.Document>"#,
        );
        let pfx = with_file(&bytes);
        assert_eq!(check(&pfx, DEFAULT_PORT), Check::Exposed { port: 5557 });
        cleanup(&pfx);
    }

    #[test]
    fn loopback_address_without_scope_counts() {
        let found = judge(
            parse(
                "<D><SimConnect.Comm><Protocol>IPv4</Protocol><Address>127.0.0.1</Address>\
                 <Port>5557</Port></SimConnect.Comm></D>",
            )
            .unwrap(),
            DEFAULT_PORT,
        );
        assert!(found.is_exposed());
    }

    #[test]
    fn a_read_writes_nothing() {
        let pfx = scratch();
        let _ = check(&pfx, DEFAULT_PORT);
        assert!(!path(&pfx).exists());
        assert_eq!(std::fs::read_dir(&pfx).unwrap().count(), 0);
        cleanup(&pfx);
    }
}
