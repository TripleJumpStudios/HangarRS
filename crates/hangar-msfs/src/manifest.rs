//! `manifest.json` — the file that makes a directory an MSFS package.
//!
//! Tolerance here is deliberate. Surveying the 24 packages actually installed on
//! the development machine: only `content_type`, `title`, `creator`,
//! `manufacturer`, `package_version`, `minimum_game_version`, `dependencies`
//! and `release_notes` appear in all of them. `total_package_size` is in 13 of
//! 24; `builder`, `export_type`, `package_order_hint` and
//! `minimum_compatibility_version` in roughly half. A struct that requires any
//! of those fails to deserialise and the package silently vanishes from the
//! library, which is exactly the wrong failure for a scanner.

use serde::Deserialize;

/// A package's declared content type.
///
/// The observed values are not the tidy set the SDK documents. Real packages on
/// this machine carry `SCENERY`, `AIRCRAFT`, `INSTRUMENTS`, `LIVERY`, `MISC`
/// and `"Sound Set"` — note the space and the mixed case on that last one.
/// Airports arrive as `SCENERY`; there is no `AIRPORT` type in practice.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentType {
    Aircraft,
    Scenery,
    Livery,
    Instruments,
    SoundSet,
    Mission,
    Core,
    Misc,
    /// Anything unrecognised, keeping the original spelling.
    ///
    /// Not folded into [`ContentType::Misc`]: a type nobody has seen before is
    /// still worth showing the user by name, and collapsing it loses the only
    /// evidence that it exists.
    Other(String),
}

impl ContentType {
    /// Classify a raw `content_type` string.
    ///
    /// Case- and separator-insensitive, so `"Sound Set"`, `"SOUND_SET"` and
    /// `"soundset"` all land in the same place.
    pub fn parse(s: &str) -> Self {
        let key: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();

        match key.as_str() {
            "aircraft" => Self::Aircraft,
            "scenery" | "airport" => Self::Scenery,
            "livery" => Self::Livery,
            // Singular and plural both appear in the wild.
            "instruments" | "instrument" => Self::Instruments,
            "soundset" | "sound" => Self::SoundSet,
            "mission" => Self::Mission,
            "core" => Self::Core,
            "misc" | "" => Self::Misc,
            _ => Self::Other(s.trim().to_owned()),
        }
    }

    /// Label for display.
    pub fn label(&self) -> &str {
        match self {
            Self::Aircraft => "Aircraft",
            Self::Scenery => "Scenery",
            Self::Livery => "Livery",
            Self::Instruments => "Instruments",
            Self::SoundSet => "Sound Set",
            Self::Mission => "Mission",
            Self::Core => "Core",
            Self::Misc => "Misc",
            Self::Other(s) => s,
        }
    }
}

/// A parsed `manifest.json`.
///
/// Every field is optional. A manifest that parses as JSON at all yields a
/// usable record; one that does not is reported rather than skipped.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ManifestJson {
    pub title: String,
    pub creator: String,
    pub manufacturer: String,
    pub package_version: String,
    pub minimum_game_version: String,
    pub content_type: String,
    /// A byte count *as a string*, and absent from roughly half of real
    /// packages.
    pub total_package_size: Option<String>,
    pub builder: Option<String>,
    pub export_type: Option<String>,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Dependency {
    pub name: String,
    pub package_version: String,
}

impl ManifestJson {
    /// Parse manifest text.
    ///
    /// Strips a UTF-8 BOM first: some vendor tooling writes one, and
    /// `serde_json` rejects it as a leading character.
    ///
    /// Named `parse` rather than `from_str` so it cannot be confused with
    /// `std::str::FromStr`, which this deliberately does not implement — the
    /// error type is `serde_json`'s.
    pub fn parse(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s.trim_start_matches('\u{feff}'))
    }

    /// The declared content type.
    pub fn content_type(&self) -> ContentType {
        ContentType::parse(&self.content_type)
    }

    /// Declared size in bytes, if the manifest says.
    ///
    /// `None` means "not declared", which is different from zero — a package
    /// reporting 0 B in the UI because the field was missing looks broken.
    pub fn size_bytes(&self) -> Option<u64> {
        self.total_package_size.as_deref()?.trim().parse().ok()
    }

    /// Best available human name for the package.
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            "(untitled)"
        } else {
            self.title.trim()
        }
    }
}

/// Render a byte count for display.
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copied field-for-field from a real installed package.
    const REAL: &str = r#"{
  "dependencies": [],
  "content_type": "MISC",
  "title": "REDMOND",
  "manufacturer": "",
  "creator": "bullfrogsim",
  "package_version": "2.1.0",
  "minimum_game_version": "1.38.2",
  "release_notes": { "neutral": { "LastUpdate": "", "OlderHistory": "" } }
}"#;

    #[test]
    fn parses_a_real_manifest() {
        let m = ManifestJson::parse(REAL).unwrap();
        assert_eq!(m.title, "REDMOND");
        assert_eq!(m.creator, "bullfrogsim");
        assert_eq!(m.package_version, "2.1.0");
        assert_eq!(m.content_type(), ContentType::Misc);
    }

    /// The important one: `total_package_size` is missing from 11 of the 24
    /// packages installed here, and `release_notes` is a shape this struct does
    /// not model. Neither may cause the package to disappear.
    #[test]
    fn a_manifest_without_optional_fields_still_parses() {
        let m = ManifestJson::parse(REAL).unwrap();
        assert_eq!(m.size_bytes(), None, "absent size must not read as zero");
        assert_eq!(m.builder, None);
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn an_almost_empty_manifest_still_parses() {
        let m = ManifestJson::parse("{}").unwrap();
        assert_eq!(m.content_type(), ContentType::Misc);
        assert_eq!(m.display_title(), "(untitled)");
        assert_eq!(m.size_bytes(), None);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let m = ManifestJson::parse(r#"{"title":"X","some_future_field":{"a":[1,2]}}"#).unwrap();
        assert_eq!(m.title, "X");
    }

    #[test]
    fn a_bom_does_not_break_parsing() {
        let m = ManifestJson::parse("\u{feff}{\"title\":\"X\"}").unwrap();
        assert_eq!(m.title, "X");
    }

    #[test]
    fn broken_json_is_an_error_not_a_default() {
        assert!(ManifestJson::parse("{not json").is_err());
        assert!(ManifestJson::parse("").is_err());
    }

    /// Every `content_type` observed across the real install.
    #[test]
    fn real_content_types_classify() {
        assert_eq!(ContentType::parse("SCENERY"), ContentType::Scenery);
        assert_eq!(ContentType::parse("AIRCRAFT"), ContentType::Aircraft);
        assert_eq!(ContentType::parse("INSTRUMENTS"), ContentType::Instruments);
        assert_eq!(ContentType::parse("LIVERY"), ContentType::Livery);
        assert_eq!(ContentType::parse("MISC"), ContentType::Misc);
        // Mixed case, with a space — the one that motivated the normalisation.
        assert_eq!(ContentType::parse("Sound Set"), ContentType::SoundSet);
    }

    #[test]
    fn classification_ignores_case_and_separators() {
        for s in ["Sound Set", "SOUND_SET", "soundset", "sound-set"] {
            assert_eq!(ContentType::parse(s), ContentType::SoundSet, "for {s:?}");
        }
        assert_eq!(ContentType::parse("instrument"), ContentType::Instruments);
        // Airports ship as SCENERY, but accept the documented spelling too.
        assert_eq!(ContentType::parse("AIRPORT"), ContentType::Scenery);
    }

    /// An unrecognised type keeps its name instead of collapsing to Misc.
    #[test]
    fn an_unknown_content_type_keeps_its_spelling() {
        let t = ContentType::parse("Flight Model Tweak");
        assert_eq!(t, ContentType::Other("Flight Model Tweak".into()));
        assert_eq!(t.label(), "Flight Model Tweak");
        assert_ne!(t, ContentType::Misc);
    }

    #[test]
    fn size_parses_from_the_string_field() {
        let m = ManifestJson::parse(r#"{"total_package_size":"00000000000129542144"}"#).unwrap();
        assert_eq!(m.size_bytes(), Some(129_542_144));
        let m = ManifestJson::parse(r#"{"total_package_size":"not a number"}"#).unwrap();
        assert_eq!(m.size_bytes(), None);
    }

    #[test]
    fn sizes_format_at_each_scale() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
