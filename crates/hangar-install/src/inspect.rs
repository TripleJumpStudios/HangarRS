//! Stage 1 — everything knowable about an installer before it runs.
//!
//! Nothing here spawns a process, touches a prefix or writes a byte. It reads
//! the file's headers, the leading window of its bytes and the directory it
//! sits in, and reports what those say. The point is that stages 2 and 3 —
//! vendor lookup and the pre-flight gate — get to decide from facts rather
//! than from a filename.

use std::path::{Path, PathBuf};

use crate::engine::{self, Detection, Engine, Evidence, Facts, OLE_MAGIC, SIGNATURES};
use crate::pe::{Bitness, Pe, VersionInfo};
use crate::scan::{self, Hit};
use crate::siblings::Siblings;
use crate::InstallError;

/// Where a product name came from.
///
/// Kept because the name and its version have to come from the *same* place.
/// Just Flight's `setup.exe` reports `ProductVersion = 16.0` in its version
/// resource, which is InstallShield's version and not Commander 114's — so a
/// product read from `setup.ini` beside a version read from the resource would
/// pair a real product with the wrong number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `setup.ini` beside the installer.
    Siblings,
    /// The `.exe`'s own version resource.
    VersionResource,
    /// Nothing said, so the file name is all there is.
    FileName,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Siblings => "setup.ini",
            Source::VersionResource => "version resource",
            Source::FileName => "file name",
        }
    }
}

/// Who made this and what it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub product: Option<String>,
    pub vendor: Option<String>,
    pub version: Option<String>,
    pub source: Source,
}

impl Identity {
    /// Prefer the siblings, because an InstallShield stub's version resource
    /// describes InstallShield.
    pub fn read(version: &VersionInfo, siblings: &Siblings, path: &Path) -> Identity {
        if let Some(product) = siblings.product() {
            return Identity {
                product: Some(product.to_string()),
                vendor: siblings.company().map(str::to_string),
                // `setup.ini` carries no version field; leaving it empty is
                // better than borrowing the engine's.
                version: None,
                source: Source::Siblings,
            };
        }
        if let Some(product) = version.product_name() {
            return Identity {
                product: Some(product.to_string()),
                vendor: version.company_name().map(str::to_string),
                version: version
                    .product_version()
                    .or_else(|| version.file_version())
                    .map(str::to_string),
                source: Source::VersionResource,
            };
        }
        Identity {
            product: path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string),
            vendor: None,
            version: None,
            source: Source::FileName,
        }
    }

    /// A one-line name for logs and the vendor lookup.
    pub fn describe(&self) -> String {
        match (&self.vendor, &self.product, &self.version) {
            (Some(v), Some(p), Some(n)) => format!("{v} — {p} {n}"),
            (Some(v), Some(p), None) => format!("{v} — {p}"),
            (None, Some(p), Some(n)) => format!("{p} {n}"),
            (None, Some(p), None) => p.clone(),
            _ => "unidentified".to_string(),
        }
    }
}

/// The whole of stage 1.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub path: PathBuf,
    pub size: u64,
    /// `None` for a file that is not a PE at all — an `.msi` is an OLE
    /// compound document, and saying so is more useful than an error.
    pub pe: Option<Pe>,
    pub engine: Engine,
    pub evidence: Vec<Evidence>,
    pub packed: Option<String>,
    pub siblings: Siblings,
    pub identity: Identity,
    pub hits: Vec<Hit>,
}

impl Inspection {
    pub fn bitness(&self) -> Option<Bitness> {
        self.pe.as_ref().map(|pe| pe.bitness)
    }

    /// A crude estimate of how much room the install needs, in bytes.
    ///
    /// Nobody ships this number, and not estimating it would leave the gate's
    /// space check permanently unchecked. Two cases, both deliberately
    /// generous:
    ///
    /// * **Cabs beside it.** An InstallShield stub is under a megabyte and
    ///   the payload is in `data1.cab` and its siblings, so the stub's own
    ///   size says nothing at all: Commander 114 is a 0.8 MB `setup.exe` in
    ///   front of two gigabytes. The cabs are compressed, so three times
    ///   their total.
    /// * **A self-contained file.** Four times its size.
    ///
    /// Both are estimates and neither is a promise. A refusal on this number
    /// should be read as "there is not obviously room", not as arithmetic.
    pub fn payload_estimate(&self) -> u64 {
        let cabs: u64 = self
            .siblings
            .cabs
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();

        if cabs > 0 {
            cabs.saturating_mul(3)
        } else {
            self.size.saturating_mul(4)
        }
    }

    /// Did anything actually identify this as an installer?
    ///
    /// A plain application answers no, and should: TDS GPS Manager is a
    /// Themida-packed Win32 app whose only self-description is
    /// `InternalName = GPS Installer`, and running it as though it were an
    /// installer would wait for an outcome that never arrives.
    pub fn looks_like_installer(&self) -> bool {
        self.engine != Engine::Unknown
    }
}

/// Inspect one file. Reads; never runs anything.
pub fn inspect(path: &Path) -> Result<Inspection, InstallError> {
    let size = std::fs::metadata(path)
        .map_err(|source| InstallError::Read {
            path: path.to_path_buf(),
            source,
        })?
        .len();

    let window = scan::window(path)?;
    let ole_document = window.starts_with(&OLE_MAGIC);
    let hits = scan::find(&window, SIGNATURES);
    let siblings = Siblings::beside(path);

    // An MSI has no PE headers to read, and asking for them would turn a
    // recognised file into an error.
    let pe = if ole_document {
        None
    } else {
        Some(Pe::read(path)?)
    };

    let sections: Vec<String> = pe
        .as_ref()
        .map(|pe| pe.sections.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let empty = VersionInfo::default();
    let version = pe.as_ref().map(|pe| &pe.version).unwrap_or(&empty);

    let Detection {
        engine,
        evidence,
        packed,
    } = engine::detect(&Facts {
        sections: &sections,
        version,
        hits: &hits,
        siblings: &siblings,
        ole_document,
    });

    let identity = Identity::read(version, &siblings, path);

    Ok(Inspection {
        path: path.to_path_buf(),
        size,
        pe,
        engine,
        evidence,
        packed,
        siblings,
        identity,
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::siblings::SetupIni;

    fn version(pairs: &[(&str, &str)]) -> VersionInfo {
        VersionInfo {
            strings: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn just_flight_siblings() -> Siblings {
        Siblings {
            setup_ini: Some(SetupIni {
                product: Some("Black Square - Commander 114 MSFS2024".into()),
                company: Some("Just Flight (MSFS 2024)".into()),
                ..SetupIni::default()
            }),
            ..Siblings::default()
        }
    }

    /// The whole reason `setup.ini` is read: the stub's resource says
    /// Acresso / InstallShield, which is the engine that built it.
    #[test]
    fn the_siblings_outrank_the_engines_own_resource() {
        let id = Identity::read(
            &version(&[
                ("ProductName", "InstallShield"),
                ("CompanyName", "Acresso Software Inc."),
                ("ProductVersion", "16.0"),
            ]),
            &just_flight_siblings(),
            Path::new("/tmp/setup.exe"),
        );
        assert_eq!(id.source, Source::Siblings);
        assert_eq!(
            id.product.as_deref(),
            Some("Black Square - Commander 114 MSFS2024")
        );
        assert_eq!(id.vendor.as_deref(), Some("Just Flight (MSFS 2024)"));
    }

    /// And the version has to come from the same place as the name. `16.0` is
    /// InstallShield's version, not Commander 114's, so pairing it with a
    /// product read from `setup.ini` would state a version nobody shipped.
    #[test]
    fn a_version_is_never_borrowed_from_the_other_source() {
        let id = Identity::read(
            &version(&[("ProductName", "InstallShield"), ("ProductVersion", "16.0")]),
            &just_flight_siblings(),
            Path::new("/tmp/setup.exe"),
        );
        assert_eq!(id.version, None);
    }

    #[test]
    fn the_version_resource_is_used_when_there_are_no_siblings() {
        let id = Identity::read(
            &version(&[
                ("ProductName", "Garmin Aviation Trainers"),
                ("CompanyName", "Garmin"),
                ("ProductVersion", "3.12.1.0"),
            ]),
            &Siblings::default(),
            Path::new("/tmp/GarminTrainerSetup_31210.exe"),
        );
        assert_eq!(id.source, Source::VersionResource);
        assert_eq!(id.version.as_deref(), Some("3.12.1.0"));
        assert_eq!(id.describe(), "Garmin — Garmin Aviation Trainers 3.12.1.0");
    }

    #[test]
    fn a_file_with_nothing_to_say_falls_back_to_its_name() {
        let id = Identity::read(
            &VersionInfo::default(),
            &Siblings::default(),
            Path::new("/tmp/some-installer.exe"),
        );
        assert_eq!(id.source, Source::FileName);
        assert_eq!(id.product.as_deref(), Some("some-installer"));
        assert_eq!(id.describe(), "some-installer");
    }

    #[test]
    fn a_missing_file_is_an_error_rather_than_an_empty_report() {
        assert!(inspect(Path::new("/nonexistent/setup.exe")).is_err());
    }
}
