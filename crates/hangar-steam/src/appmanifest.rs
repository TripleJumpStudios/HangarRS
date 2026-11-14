//! `appmanifest_<id>.acf` — Steam's per-app install record, in VDF.

use crate::vdf;
use crate::{Result, SteamError};
use std::path::Path;

/// The fields of an app manifest this application needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppManifest {
    pub app_id: String,
    pub name: String,
    /// Directory under `steamapps/common`, which is **not** derivable from the
    /// name: MSFS 2024 installs to `MSFS2024`.
    pub install_dir: String,
}

/// Parse an `appmanifest_*.acf`.
pub fn parse(path: &Path) -> Result<AppManifest> {
    let content = std::fs::read_to_string(path).map_err(|source| SteamError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    from_str(&content).map_err(|reason| SteamError::Vdf {
        path: path.to_path_buf(),
        reason,
    })
}

/// Parse manifest text. Separate from [`parse`] so it can be tested directly.
pub fn from_str(content: &str) -> std::result::Result<AppManifest, String> {
    let root = vdf::parse(content)?;
    let state = root
        .get_path("appstate")
        .and_then(vdf::VdfNode::as_map)
        .ok_or("missing AppState block")?;

    // `installdir` is the only truly required field: without it there is no way
    // to find the game. A manifest missing the name is odd but still usable.
    let install_dir = state
        .get("installdir")
        .and_then(vdf::VdfNode::as_str)
        .ok_or("missing installdir in AppState")?
        .to_owned();

    Ok(AppManifest {
        app_id: state
            .get("appid")
            .and_then(vdf::VdfNode::as_str)
            .unwrap_or_default()
            .to_owned(),
        name: state
            .get("name")
            .and_then(vdf::VdfNode::as_str)
            .unwrap_or_default()
            .to_owned(),
        install_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field order and surrounding keys mirror a real MSFS 2024 manifest.
    const REAL_SHAPE: &str = r#"
"AppState"
{
	"appid"		"2537590"
	"universe"		"1"
	"name"		"Microsoft Flight Simulator 2024"
	"StateFlags"		"4"
	"installdir"		"MSFS2024"
	"LastUpdated"		"1786658183"
	"SizeOnDisk"		"9719799594"
	"buildid"		"24649942"
	"InstalledDepots"
	{
		"2537591" { "manifest" "123" "size" "456" }
	}
}
"#;

    #[test]
    fn parses_the_fields_we_need() {
        let m = from_str(REAL_SHAPE).unwrap();
        assert_eq!(m.app_id, "2537590");
        assert_eq!(m.name, "Microsoft Flight Simulator 2024");
        assert_eq!(m.install_dir, "MSFS2024");
    }

    /// The install directory is not derivable from the name, which is why it
    /// has to be read rather than guessed.
    #[test]
    fn install_dir_is_not_the_app_name() {
        let m = from_str(REAL_SHAPE).unwrap();
        assert_ne!(m.install_dir, m.name);
    }

    #[test]
    fn keys_are_matched_case_insensitively() {
        let m = from_str(r#""appstate" { "APPID" "1" "Name" "N" "InstallDir" "D" }"#).unwrap();
        assert_eq!(
            (m.app_id.as_str(), m.name.as_str(), m.install_dir.as_str()),
            ("1", "N", "D")
        );
    }

    #[test]
    fn a_manifest_without_installdir_is_rejected() {
        assert!(from_str(r#""AppState" { "appid" "1" "name" "X" }"#).is_err());
        assert!(from_str(r#""Something" { "installdir" "X" }"#).is_err());
        assert!(from_str("not vdf {{{").is_err());
    }

    /// A missing name is odd but must not make the install unusable.
    #[test]
    fn a_missing_name_still_yields_a_usable_manifest() {
        let m = from_str(r#""AppState" { "installdir" "MSFS2024" }"#).unwrap();
        assert_eq!(m.install_dir, "MSFS2024");
        assert!(m.name.is_empty());
    }
}
