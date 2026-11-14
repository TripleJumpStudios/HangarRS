//! What sits in the directory beside the `.exe`.
//!
//! An InstallShield `setup.exe` is a stub. Everything that identifies the
//! *product* is in the files next to it, and the stub's own version resource
//! describes the engine that built it:
//!
//! ```text
//! setup.exe  version resource   CompanyName = Acresso Software Inc.
//!                               ProductName = InstallShield
//! setup.ini  [Startup]          CompanyName = Just Flight (MSFS 2024)
//!                               Product     = Black Square - Commander 114 MSFS2024
//! ```
//!
//! Keying a vendor lookup on the version resource alone would therefore file
//! every Just Flight title, every Carenado title and every other
//! InstallScript build under "Acresso Software" — one bucket, no vendor.

use std::fs;
use std::path::{Path, PathBuf};

/// The InstallShield cab magic: `ISc(`.
const CAB_MAGIC: &[u8; 4] = b"ISc(";

/// What the directory beside the installer holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Siblings {
    /// `data1.cab`, `data2.cab`, … in name order.
    pub cabs: Vec<PathBuf>,
    /// `data1.hdr`, the cab header, if present.
    pub header: Option<PathBuf>,
    /// The InstallShield version decoded from the cab header.
    pub cab_version: Option<CabVersion>,
    /// `setup.ini`'s `[Startup]` section, which names the real product.
    pub setup_ini: Option<SetupIni>,
    /// `ISSetup.dll` — the InstallScript engine, alongside `setup.inx`.
    pub issetup_dll: bool,
    /// A `.nupkg` beside the binary, which is what a Squirrel install looks
    /// like on disk.
    pub nupkg: bool,
    /// An `.msi` in the same directory.
    pub msi: bool,
}

impl Siblings {
    /// Read the directory containing `installer`.
    ///
    /// A directory that cannot be read is not an error: an installer is
    /// perfectly allowed to be a single file with nothing beside it, and the
    /// empty answer says the same thing.
    pub fn beside(installer: &Path) -> Siblings {
        let Some(dir) = installer.parent() else {
            return Siblings::default();
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return Siblings::default();
        };

        let mut found = Siblings::default();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("data") && lower.ends_with(".cab") {
                found.cabs.push(path.clone());
            }
            if lower == "data1.hdr" {
                found.cab_version = read_cab_version(&path);
                found.header = Some(path.clone());
            }
            if lower == "setup.ini" {
                found.setup_ini = SetupIni::read(&path);
            }
            if lower == "issetup.dll" {
                found.issetup_dll = true;
            }
            if lower.ends_with(".nupkg") {
                found.nupkg = true;
            }
            if lower.ends_with(".msi") {
                found.msi = true;
            }
        }
        found.cabs.sort();

        // A cab set with no header still dates itself: data1.cab carries the
        // same magic and version words as data1.hdr.
        if found.cab_version.is_none() {
            if let Some(first) = found.cabs.first() {
                found.cab_version = read_cab_version(first);
            }
        }
        found
    }

    /// The product name a person would recognise, if the siblings know it.
    pub fn product(&self) -> Option<&str> {
        self.setup_ini.as_ref()?.product.as_deref()
    }

    /// The vendor a person would recognise, if the siblings know it.
    pub fn company(&self) -> Option<&str> {
        self.setup_ini.as_ref()?.company.as_deref()
    }
}

/// The InstallShield family and version stamped into a cab header.
///
/// Confirmed against `data1.hdr` from Black Square Commander 114: the first
/// four bytes are `ISc(` and the next `u32` little-endian is `0x02000640` —
/// family 2, version `0x0640` = 1600 = **16.00**, which is the InstallShield
/// 2010 / Setup Player V16 family that `unshield` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CabVersion {
    pub family: u8,
    /// The raw version word: 1600 means 16.00.
    pub version: u16,
}

impl CabVersion {
    /// Decode the header words. `None` if the magic is not `ISc(`.
    pub fn parse(bytes: &[u8]) -> Option<CabVersion> {
        if bytes.len() < 8 || &bytes[0..4] != CAB_MAGIC {
            return None;
        }
        let word = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        Some(CabVersion {
            family: (word >> 24) as u8,
            version: (word & 0xffff) as u16,
        })
    }

    /// The major version a person would say out loud: 1600 → 16.
    pub fn major(self) -> u16 {
        self.version / 100
    }

    pub fn label(self) -> String {
        format!(
            "InstallShield {}.{:02} (family {})",
            self.major(),
            self.version % 100,
            self.family
        )
    }
}

fn read_cab_version(path: &Path) -> Option<CabVersion> {
    let bytes = fs::read(path).ok()?;
    CabVersion::parse(&bytes)
}

/// The `[Startup]` section of an InstallShield `setup.ini`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupIni {
    pub product: Option<String>,
    pub company: Option<String>,
    pub product_guid: Option<String>,
    /// `ScriptDriven=4` marks an InstallScript build rather than Basic MSI.
    pub script_driven: Option<String>,
}

impl SetupIni {
    pub fn read(path: &Path) -> Option<SetupIni> {
        // Not UTF-8 in general: these are written in the system code page, and
        // a vendor name with an accent in it should not lose the whole file.
        let bytes = fs::read(path).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        Some(SetupIni::parse(&text))
    }

    /// Read only `[Startup]`. The rest of the file is language tables.
    pub fn parse(text: &str) -> SetupIni {
        let mut out = SetupIni::default();
        let mut in_startup = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_startup = line.eq_ignore_ascii_case("[Startup]");
                continue;
            }
            if !in_startup {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match key.trim().to_ascii_lowercase().as_str() {
                "product" => out.product = Some(value),
                "companyname" => out.company = Some(value),
                "productguid" => out.product_guid = Some(value),
                "scriptdriven" => out.script_driven = Some(value),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first eight bytes of the real `data1.hdr` shipped with Black Square
    /// Commander 114: `ISc(` then `0x02000640`.
    const REAL_HEADER: [u8; 8] = [b'I', b'S', b'c', b'(', 0x40, 0x06, 0x00, 0x02];

    #[test]
    fn decodes_the_installshield_16_cab_header() {
        let version = CabVersion::parse(&REAL_HEADER).expect("magic matches");
        assert_eq!(version.family, 2);
        assert_eq!(version.version, 1600);
        assert_eq!(version.major(), 16);
        assert_eq!(version.label(), "InstallShield 16.00 (family 2)");
    }

    #[test]
    fn a_cab_that_is_not_installshield_is_not_claimed() {
        assert!(CabVersion::parse(b"MSCF\0\0\0\0").is_none());
        assert!(CabVersion::parse(b"ISc").is_none());
        assert!(CabVersion::parse(&[]).is_none());
    }

    /// Verbatim from the `setup.ini` beside Commander 114's `setup.exe`.
    const REAL_INI: &str = "\
[Startup]
Product=Black Square - Commander 114 MSFS2024
ProductGUID=E660EDD8-B87A-4E5B-A99A-08327AD4E122
CompanyName=Just Flight (MSFS 2024)
ErrorReportURL=http://www.installshield.com/isetup/ProErrorCentral.asp?ErrorCode=%d : 0x%x&ErrorInfo=%s
MediaFormat=1
CmdLine=
ScriptDriven=4

[Languages]
Default=0x0409
CompanyName=not this one
";

    #[test]
    fn reads_the_product_the_engine_does_not_know() {
        let ini = SetupIni::parse(REAL_INI);
        assert_eq!(
            ini.product.as_deref(),
            Some("Black Square - Commander 114 MSFS2024")
        );
        assert_eq!(ini.company.as_deref(), Some("Just Flight (MSFS 2024)"));
        assert_eq!(ini.script_driven.as_deref(), Some("4"));
        assert_eq!(
            ini.product_guid.as_deref(),
            Some("E660EDD8-B87A-4E5B-A99A-08327AD4E122")
        );
    }

    /// `[Languages]` also has a `CompanyName`. Reading the whole file as flat
    /// key/value pairs takes the last one and renames the vendor.
    #[test]
    fn only_the_startup_section_is_read() {
        assert_eq!(
            SetupIni::parse(REAL_INI).company.as_deref(),
            Some("Just Flight (MSFS 2024)")
        );
    }

    #[test]
    fn an_empty_value_is_not_a_value() {
        let ini = SetupIni::parse("[Startup]\nProduct=\nCompanyName=Acme\n");
        assert_eq!(ini.product, None);
        assert_eq!(ini.company.as_deref(), Some("Acme"));
    }

    #[test]
    fn a_file_with_no_startup_section_says_nothing() {
        assert_eq!(
            SetupIni::parse("[Languages]\nProduct=x\n"),
            SetupIni::default()
        );
    }
}
