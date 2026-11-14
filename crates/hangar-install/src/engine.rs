//! Which installer engine built this `.exe`, and what said so.
//!
//! Detection takes *facts* rather than a path — section names, version
//! strings, signature hits, the files beside it — so the rules can be tested
//! without a two-gigabyte installer on disk, and so every answer can carry the
//! evidence it was reached from. An engine guessed with nothing to show for it
//! is worse than an honest [`Engine::Unknown`]: the vendor table keys off this,
//! and a wrong key runs an installer under someone else's rules.
//!
//! # What each rule is grounded in
//!
//! Every rule below except Inno Setup was checked against a real installer:
//!
//! | Engine | Rule | Seen in |
//! |---|---|---|
//! | WiX Burn | a `.wixburn` section | Garmin Trainer 3.12.1.0, .NET Desktop Runtime 8.0.29 |
//! | InstallShield | version resource `ProductName = InstallShield`, `Setup Player` in `.rsrc`, `ISc(` cabs beside it | Just Flight / Black Square Commander 114 |
//! | NSIS | an `.ndata` section, `Nullsoft Install System` | Project Airports Setup 0.1.33 |
//! | MSI | the OLE compound-document magic at offset 0 | — (shape check only) |
//! | Squirrel | a `.nupkg` beside the binary | — (shape check only) |
//! | Inno Setup | `Inno Setup Setup Data` | **not verified** against a real installer |
//!
//! # Electron does not show up in an Electron installer
//!
//! Project Airports is an Electron app and its installer says so nowhere.
//! `electron`, `app-64.7z` and `.nupkg` are all absent from the whole 84 MB
//! file, because electron-builder wraps the app in an LZMA archive inside an
//! NSIS installer. So the engine is NSIS and the fact that it produces an
//! Electron app is not a static property of the installer — it is discovered
//! in stage 5, from what lands in the prefix.

use crate::pe::VersionInfo;
use crate::scan::{self, Hit};
use crate::siblings::Siblings;

/// Signatures worth looking for. Scanned in both ASCII and UTF-16.
pub const SIGNATURES: &[&str] = &[
    "Nullsoft Install System",
    "Inno Setup Setup Data",
    "Setup Player",
    "InstallShield",
    "WixBundle",
    "Squirrel",
];

/// The OLE compound-document magic that starts every `.msi`.
pub const OLE_MAGIC: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

/// Section names that mean the binary was packed, so its strings are not
/// visible until it runs.
const PACKERS: &[&str] = &[
    ".themida", ".vmp0", ".vmp1", "UPX0", "UPX1", ".aspack", ".boot",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    InstallShield,
    WixBurn,
    Nsis,
    InnoSetup,
    Msi,
    Squirrel,
    /// Nothing matched. Says only that: an unpacked binary with no installer
    /// signature is very likely an ordinary application.
    Unknown,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Engine::InstallShield => "InstallShield",
            Engine::WixBurn => "WiX Burn",
            Engine::Nsis => "NSIS",
            Engine::InnoSetup => "Inno Setup",
            Engine::Msi => "Windows Installer package",
            Engine::Squirrel => "Squirrel",
            Engine::Unknown => "unrecognised",
        }
    }

    /// Does this engine unpack itself and hand off to a second process?
    ///
    /// It decides how a launch is judged, not just how it is spawned. A stub
    /// exits within seconds of starting while the engine it spawned carries on
    /// — so exit code, exit time and an empty console all mean nothing here,
    /// and `proton run` will tear the prefix down around the engine the moment
    /// the stub returns.
    pub fn self_extracting(self) -> bool {
        matches!(
            self,
            Engine::InstallShield | Engine::WixBurn | Engine::Squirrel
        )
    }
}

/// One thing that was actually observed, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub fact: String,
    pub source: String,
}

impl Evidence {
    fn new(fact: impl Into<String>, source: impl Into<String>) -> Evidence {
        Evidence {
            fact: fact.into(),
            source: source.into(),
        }
    }
}

/// What detection was given to work with.
pub struct Facts<'a> {
    pub sections: &'a [String],
    pub version: &'a VersionInfo,
    pub hits: &'a [Hit],
    pub siblings: &'a Siblings,
    /// The file begins with [`OLE_MAGIC`], so it is not a PE at all.
    pub ole_document: bool,
}

/// An engine, and everything that pointed at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub engine: Engine,
    pub evidence: Vec<Evidence>,
    /// A packer was seen. Reported because it explains an [`Engine::Unknown`]
    /// that would otherwise look like a failure of the scan.
    pub packed: Option<String>,
}

/// Identify the engine, strongest evidence first.
pub fn detect(facts: &Facts) -> Detection {
    let mut evidence = Vec::new();
    let packed = facts
        .sections
        .iter()
        .find(|name| PACKERS.iter().any(|p| p.eq_ignore_ascii_case(name)))
        .cloned()
        .or_else(|| {
            // Packers also leave sections with no name at all, which no
            // ordinary linker does.
            facts
                .sections
                .iter()
                .any(|n| n.is_empty())
                .then(|| "unnamed sections".to_string())
        });

    // An MSI is not a PE, so this comes before anything that reads headers.
    if facts.ole_document {
        evidence.push(Evidence::new("OLE compound document", "first eight bytes"));
        return Detection {
            engine: Engine::Msi,
            evidence,
            packed,
        };
    }

    // A `.wixburn` section is written by the Burn linker and by nothing else.
    if let Some(section) = facts.sections.iter().find(|n| *n == ".wixburn") {
        evidence.push(Evidence::new(format!("{section} section"), "section table"));
        if scan::saw(facts.hits, "WixBundle") {
            evidence.push(Evidence::new("WixBundle", "signature scan"));
        }
        return Detection {
            engine: Engine::WixBurn,
            evidence,
            packed,
        };
    }

    // InstallShield says so in three places, and any one of them is enough --
    // the stub's own version resource describes the engine rather than the
    // product it installs.
    let mut installshield = false;
    if facts.version.product_name() == Some("InstallShield") {
        evidence.push(Evidence::new(
            "ProductName = InstallShield",
            "version resource",
        ));
        installshield = true;
    }
    if let Some(desc) = facts.version.file_description() {
        if desc.contains("InstallScript Setup Launcher") {
            evidence.push(Evidence::new(
                format!("FileDescription = {desc}"),
                "version resource",
            ));
            installshield = true;
        }
    }
    if scan::saw(facts.hits, "Setup Player") {
        evidence.push(Evidence::new("Setup Player", "signature scan"));
        installshield = true;
    }
    if let Some(cab) = facts.siblings.cab_version {
        evidence.push(Evidence::new(cab.label(), "data1.hdr"));
        installshield = true;
    }
    if facts.siblings.issetup_dll {
        evidence.push(Evidence::new("ISSetup.dll", "beside the installer"));
        installshield = true;
    }
    if installshield {
        return Detection {
            engine: Engine::InstallShield,
            evidence,
            packed,
        };
    }

    // NSIS keeps its uncompressed data in a section it invented.
    let ndata = facts.sections.iter().any(|n| n == ".ndata");
    let nullsoft = scan::saw(facts.hits, "Nullsoft Install System");
    if ndata || nullsoft {
        if ndata {
            evidence.push(Evidence::new(".ndata section", "section table"));
        }
        if nullsoft {
            evidence.push(Evidence::new("Nullsoft Install System", "signature scan"));
        }
        return Detection {
            engine: Engine::Nsis,
            evidence,
            packed,
        };
    }

    if scan::saw(facts.hits, "Inno Setup Setup Data") {
        evidence.push(Evidence::new("Inno Setup Setup Data", "signature scan"));
        return Detection {
            engine: Engine::InnoSetup,
            evidence,
            packed,
        };
    }

    if facts.siblings.nupkg || scan::saw(facts.hits, "Squirrel") {
        if facts.siblings.nupkg {
            evidence.push(Evidence::new(".nupkg", "beside the installer"));
        }
        if scan::saw(facts.hits, "Squirrel") {
            evidence.push(Evidence::new("Squirrel", "signature scan"));
        }
        return Detection {
            engine: Engine::Squirrel,
            evidence,
            packed,
        };
    }

    Detection {
        engine: Engine::Unknown,
        evidence,
        packed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::VersionInfo;
    use crate::siblings::CabVersion;

    fn sections(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn version(pairs: &[(&str, &str)]) -> VersionInfo {
        VersionInfo {
            strings: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn detect_with(
        sections: &[String],
        version: &VersionInfo,
        hits: &[Hit],
        siblings: &Siblings,
    ) -> Detection {
        detect(&Facts {
            sections,
            version,
            hits,
            siblings,
            ole_document: false,
        })
    }

    fn hit(needle: &'static str) -> Hit {
        Hit {
            needle,
            encoding: scan::Encoding::Utf16Le,
            offset: 0,
        }
    }

    /// Garmin Trainer 3.12.1.0 and the .NET Desktop Runtime 8.0.29 both.
    #[test]
    fn a_wixburn_section_is_burn() {
        let found = detect_with(
            &sections(&[".text", ".rdata", ".data", ".wixburn", ".rsrc", ".reloc"]),
            &version(&[("ProductName", "Garmin Aviation Trainers")]),
            &[hit("WixBundle")],
            &Siblings::default(),
        );
        assert_eq!(found.engine, Engine::WixBurn);
        assert!(found.evidence.iter().any(|e| e.source == "section table"));
    }

    /// Just Flight's `setup.exe`: the version resource describes the engine.
    #[test]
    fn the_engines_own_version_resource_identifies_it() {
        let found = detect_with(
            &sections(&[".text", ".rdata", ".data", ".rsrc"]),
            &version(&[
                ("ProductName", "InstallShield"),
                ("FileDescription", "InstallScript Setup Launcher"),
                ("CompanyName", "Acresso Software Inc."),
            ]),
            &[hit("Setup Player")],
            &Siblings::default(),
        );
        assert_eq!(found.engine, Engine::InstallShield);
    }

    /// A repackaged installer whose stub says nothing is still identified by
    /// the cab set beside it.
    #[test]
    fn the_cabs_alone_are_enough() {
        let siblings = Siblings {
            cab_version: Some(CabVersion {
                family: 2,
                version: 1600,
            }),
            issetup_dll: true,
            ..Siblings::default()
        };
        let found = detect_with(
            &sections(&[".text", ".rsrc"]),
            &VersionInfo::default(),
            &[],
            &siblings,
        );
        assert_eq!(found.engine, Engine::InstallShield);
        assert!(found
            .evidence
            .iter()
            .any(|e| e.fact.contains("InstallShield 16.00")));
    }

    /// Project Airports Setup 0.1.33 — electron-builder ships NSIS.
    #[test]
    fn an_ndata_section_is_nsis() {
        let found = detect_with(
            &sections(&[".text", ".rdata", ".data", ".ndata", ".rsrc"]),
            &version(&[("ProductName", "Project Airports")]),
            &[hit("Nullsoft Install System")],
            &Siblings::default(),
        );
        assert_eq!(found.engine, Engine::Nsis);
        assert_eq!(found.evidence.len(), 2, "section and signature both");
    }

    #[test]
    fn an_ole_document_is_an_msi_before_anything_reads_a_header() {
        let found = detect(&Facts {
            sections: &[],
            version: &VersionInfo::default(),
            hits: &[],
            siblings: &Siblings::default(),
            ole_document: true,
        });
        assert_eq!(found.engine, Engine::Msi);
    }

    /// TDS GPS Manager is a Themida-packed Win32 application whose internal
    /// name is `GPS Installer`. Nothing about it should be run as an
    /// installer, and the packer is why nothing was found.
    #[test]
    fn a_packed_application_is_unknown_and_says_why() {
        let found = detect_with(
            &sections(&["", "", ".idata", ".rsrc", ".themida", ".boot"]),
            &version(&[
                ("ProductName", "TDSGPSManager"),
                ("InternalName", "GPS Installer"),
            ]),
            &[],
            &Siblings::default(),
        );
        assert_eq!(found.engine, Engine::Unknown);
        assert_eq!(found.packed.as_deref(), Some(".themida"));
    }

    #[test]
    fn unnamed_sections_are_reported_as_packing() {
        let found = detect_with(
            &sections(&["", "", ".rsrc"]),
            &VersionInfo::default(),
            &[],
            &Siblings::default(),
        );
        assert_eq!(found.packed.as_deref(), Some("unnamed sections"));
    }

    /// iTunes' installer is a real installer with a proprietary engine. The
    /// honest answer is that nothing identified it — stage 2 then has no
    /// vendor profile and has to ask, which is the right outcome.
    #[test]
    fn an_unrecognised_engine_carries_no_evidence() {
        let found = detect_with(
            &sections(&[".text", ".rdata", ".data", ".rsrc", ".reloc"]),
            &version(&[
                ("ProductName", "iTunes"),
                ("FileDescription", "iTunes Installer"),
            ]),
            &[],
            &Siblings::default(),
        );
        assert_eq!(found.engine, Engine::Unknown);
        assert!(found.evidence.is_empty());
        assert_eq!(found.packed, None);
    }

    #[test]
    fn only_the_handing_off_engines_are_self_extracting() {
        assert!(Engine::InstallShield.self_extracting());
        assert!(Engine::WixBurn.self_extracting());
        assert!(!Engine::Nsis.self_extracting());
        assert!(!Engine::Unknown.self_extracting());
    }
}
