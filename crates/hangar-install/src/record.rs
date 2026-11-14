//! Stage 7 — filing the result.
//!
//! # Filing is recording, not registering
//!
//! The obvious reading of "items enter the library" is that stage 7 inserts
//! them somewhere. It does not, and should not: `hangar-catalog` builds the
//! library by **scanning the filesystem** — Community, the prefixes, `EXE.xml`
//! — so a package that arrived thirty seconds ago is in the library on the
//! next scan whether or not anything was told. An insert would be a second
//! copy of the truth, and the two would drift the first time someone deleted
//! a directory by hand.
//!
//! What is *not* derivable from a scan is everything about the install itself:
//! which installer produced this, when, under which vendor profile, with what
//! command, what the console said, and whether the result verified. A scan of
//! the finished machine cannot reconstruct any of it. So that is what a record
//! holds, and it is the whole of stage 7.
//!
//! # One file per install, never a log to append to
//!
//! An append-only log is one corrupt write from losing every earlier entry,
//! and it makes deleting one record a rewrite of the file. A directory of
//! timestamped files loses at most the one that was being written, and
//! `list` simply reads what is there.
//!
//! Records are written with the same care as the bindings snapshots: to a
//! temporary file in the same directory, then renamed. A rename is atomic
//! within a filesystem, so a reader never sees half a record.

use std::path::{Path, PathBuf};

use hangar_core::time::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::classify::{Concern, Landed, Report as Classified};
use crate::inspect::Inspection;
use crate::run::{Invocation, Ran, Verdict};
use crate::vendor::Profile;
use crate::verify::PackageReport;

/// What one install did.
///
/// No `Eq`: `ran_for_secs` is an `f32`, and a duration is not something two
/// records should ever be compared on anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallRecord {
    /// `2026-08-26T16-13-13Z`, and also this record's file name.
    pub at: String,
    pub installer: PathBuf,
    pub installer_bytes: u64,
    pub engine: String,
    pub product: Option<String>,
    pub vendor: Option<String>,
    /// Which vendor profile decided how it ran, and where that profile came
    /// from — a shipped rule and a hand-written one fail differently.
    pub profile: String,
    pub profile_origin: String,
    pub prefix: String,
    /// The exact command, so a run that went wrong can be repeated by hand.
    pub command: String,
    pub cwd: PathBuf,
    pub ran_for_secs: f32,
    /// `None` means it was killed by a signal rather than exiting.
    pub exit_code: Option<i32>,
    /// What stage 4 made of the run. Never "success".
    pub verdict: String,
    /// Every console line that matched a known-bad signature, kept whole.
    pub bad_console: Vec<String>,
    pub landed: Vec<RecordedItem>,
    pub concerns: Vec<String>,
    /// Stage 6's answer, when a package landed and was checked.
    pub verified: Option<Verified>,

    /// What produced this: `"installer"` or `"drop"`.
    ///
    /// **An `origin` on the one record rather than a second record type.** Two
    /// histories would disagree about what is installed the first time somebody
    /// consulted only one of them. Defaulted, so records written before drops
    /// existed still read.
    #[serde(default = "installer_origin")]
    pub origin: String,

    /// Prefix files moved aside for the duration of the run, by path.
    ///
    /// Empty for a drop and for any run where nothing needed it. Recorded
    /// because an install that failed for months and then worked should say
    /// what changed, rather than looking like luck — see [`crate::shim`].
    /// Defaulted, so records written before this field existed still read.
    #[serde(default)]
    pub shimmed: Vec<String>,

    /// What this replaced and where the old copy went. A drop has this and an
    /// installer run does not; it is the one thing a drop knows that the seven
    /// stages never do.
    #[serde(default)]
    pub replaced: Vec<Replaced>,
}

fn installer_origin() -> String {
    ORIGIN_INSTALLER.to_string()
}

pub const ORIGIN_INSTALLER: &str = "installer";
pub const ORIGIN_DROP: &str = "drop";

/// A package that was moved aside to make room for a new version.
///
/// `moved_to` is the whole value of recording this: it is one rename from
/// undoing a bad update, and nothing else on the machine remembers where it
/// went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replaced {
    pub name: String,
    /// The version that was there, when its manifest declared one.
    pub was_version: Option<String>,
    pub moved_to: PathBuf,
}

/// One thing the install produced, flattened for storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedItem {
    pub kind: String,
    pub name: String,
    /// Where it went, or what it registered.
    pub detail: String,
}

/// Stage 6's result for one package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    pub package: String,
    pub verified: usize,
    pub declared: usize,
    pub missing: usize,
    pub wrong_size: usize,
    pub orphans: usize,
}

impl Verified {
    pub fn of(package: &str, report: &PackageReport) -> Self {
        Verified {
            package: package.to_string(),
            verified: report.verified(),
            declared: report.declared,
            missing: report.missing.len(),
            wrong_size: report.wrong_size.len(),
            orphans: report.orphans.len(),
        }
    }
}

impl InstallRecord {
    /// Build a record from what the earlier stages produced.
    #[allow(clippy::too_many_arguments)]
    pub fn of(
        inspection: &Inspection,
        profile: &Profile,
        invocation: &Invocation,
        ran: &Ran,
        verdict: &Verdict,
        classified: &Classified,
        verified: Option<Verified>,
        shimmed: Vec<String>,
    ) -> Self {
        InstallRecord {
            at: Utc::now().stamp(),
            installer: inspection.path.clone(),
            installer_bytes: inspection.size,
            engine: inspection.engine.label().to_string(),
            product: inspection.identity.product.clone(),
            vendor: inspection.identity.vendor.clone(),
            profile: profile.name.clone(),
            profile_origin: match profile.origin {
                crate::vendor::Origin::Shipped => "shipped".into(),
                crate::vendor::Origin::User => "yours".into(),
            },
            prefix: profile.prefix.label().to_string(),
            command: invocation.command_line(),
            cwd: invocation.cwd.clone(),
            ran_for_secs: ran.after.as_secs_f32(),
            exit_code: ran.code,
            verdict: describe_verdict(verdict),
            bad_console: ran.hits.iter().map(|h| h.line.clone()).collect(),
            landed: classified.landed.iter().map(RecordedItem::of).collect(),
            concerns: classified.concerns.iter().map(describe_concern).collect(),
            verified,
            shimmed,
            origin: ORIGIN_INSTALLER.into(),
            replaced: Vec::new(),
        }
    }

    /// Build a record for a drop-install.
    ///
    /// A drop has no engine, no vendor profile, no command and no console, and
    /// the fields that would hold those are left honestly empty rather than
    /// filled with something that reads like a process ran. What it has instead
    /// is [`Self::replaced`].
    pub fn of_drop(
        dropped: &std::path::Path,
        dropped_bytes: u64,
        community: &std::path::Path,
        placed: &[crate::drop::Placed],
    ) -> Self {
        let first_unchecked = placed
            .iter()
            .find(|p| matches!(p.verification, crate::drop::Verification::Unchecked(_)));

        InstallRecord {
            at: Utc::now().stamp(),
            installer: dropped.to_path_buf(),
            installer_bytes: dropped_bytes,
            engine: match dropped.extension().and_then(|e| e.to_str()) {
                Some(ext) => ext.to_lowercase(),
                None => "folder".into(),
            },
            product: dropped
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            vendor: placed
                .iter()
                .map(|p| p.creator.clone())
                .find(|c| !c.is_empty()),
            profile: "drop-install".into(),
            profile_origin: "built in".into(),
            prefix: "none — a copy into Community".into(),
            command: String::new(),
            cwd: community.to_path_buf(),
            ran_for_secs: 0.0,
            exit_code: None,
            verdict: "placed".into(),
            bad_console: Vec::new(),
            landed: placed
                .iter()
                .map(|p| RecordedItem {
                    kind: "Package".into(),
                    name: p.name.clone(),
                    detail: p.at.display().to_string(),
                })
                .collect(),
            concerns: first_unchecked
                .map(|p| vec![format!("{}: {}", p.name, p.verification.summary())])
                .unwrap_or_default(),
            verified: placed.iter().find_map(|p| match &p.verification {
                crate::drop::Verification::Checked(report) => Some(Verified::of(&p.name, report)),
                crate::drop::Verification::Unchecked(_) => None,
            }),
            shimmed: Vec::new(),
            origin: ORIGIN_DROP.into(),
            // The superseded copy's **own** name, not the placement's: a
            // vendor that renames the folder between versions makes those two
            // different, and recording the new name here would file a record
            // saying a package replaced itself.
            replaced: placed
                .iter()
                .flat_map(|p| p.replaced.iter())
                .map(|old| Replaced {
                    name: old.name.clone(),
                    was_version: old.version.clone(),
                    moved_to: old.moved_to.clone(),
                })
                .collect(),
        }
    }

    /// Whether this came from a dropped folder or archive.
    pub fn is_drop(&self) -> bool {
        self.origin == ORIGIN_DROP
    }

    /// Nothing about this install is worrying, and something arrived.
    ///
    /// Still not a promise that it works — an aeroplane can be complete on
    /// disk and wrong in the air.
    pub fn is_clean(&self) -> bool {
        !self.landed.is_empty() && self.concerns.is_empty()
    }

    /// A line for a history list.
    pub fn summary(&self) -> String {
        let what = if self.landed.is_empty() {
            "nothing".to_string()
        } else {
            self.landed
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let verified = match &self.verified {
            Some(v) => format!(", {}/{} verified", v.verified, v.declared),
            None => String::new(),
        };
        let replaced = match self.replaced.len() {
            0 => String::new(),
            1 => format!(", replacing {}", self.replaced[0].name),
            n => format!(", replacing {n}"),
        };
        format!(
            "{}  {}  → {what}{verified}{replaced}",
            self.at,
            self.product.as_deref().unwrap_or("?")
        )
    }
}

impl RecordedItem {
    fn of(landed: &Landed) -> Self {
        RecordedItem {
            kind: landed.kind().label().to_string(),
            name: landed.name(),
            detail: match landed {
                Landed::Package { name } => format!("Community/{name}"),
                Landed::SimAddon {
                    win_path, resolved, ..
                } => match resolved {
                    Some(p) => format!("{win_path} → {}", p.display()),
                    None => format!("{win_path} (does not resolve in the MSFS prefix)"),
                },
                Landed::CompanionApp { exe, also } => {
                    format!("{} (+{} more)", exe.display(), also.len())
                }
            },
        }
    }
}

fn describe_verdict(verdict: &Verdict) -> String {
    match verdict {
        Verdict::BadConsole(hits) => format!("{} known-bad console line(s)", hits.len()),
        Verdict::Vanished { after } => {
            format!("vanished after {:.1}s", after.as_secs_f32())
        }
        Verdict::Killed { .. } => "killed by a signal".into(),
        Verdict::ExitCode(code) => format!("exited {code}"),
        Verdict::NoComplaint => "no complaint".into(),
    }
}

fn describe_concern(concern: &Concern) -> String {
    match concern {
        Concern::NothingArrived { expected } => {
            format!(
                "nothing arrived, and this profile expects {}",
                expected.label()
            )
        }
        Concern::RunObjected { detail } => format!("the run objected: {detail}"),
        Concern::AddonPathUnreachable { name, win_path } => {
            format!("{name} registered {win_path}, which the sim's prefix cannot follow")
        }
        Concern::Unobserved { place, why } => format!("{place}: {why}"),
        Concern::Removed { place, what } => {
            format!("{place}: {} removed — {}", what.len(), what.join(", "))
        }
    }
}

#[derive(Debug, Error)]
pub enum RecordError {
    #[error("no data directory, so there is nowhere to file this")]
    NoHistoryDir,

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a record: {detail}")]
    Malformed { path: PathBuf, detail: String },
}

/// Write a record into `dir`, atomically.
///
/// Returns where it went. The name is the timestamp, which sorts
/// chronologically as a string — that is what [`Utc::stamp`]'s format is for.
pub fn file(record: &InstallRecord, dir: &Path) -> Result<PathBuf, RecordError> {
    std::fs::create_dir_all(dir).map_err(|source| RecordError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let json = serde_json::to_vec_pretty(record).map_err(|e| RecordError::Malformed {
        path: dir.to_path_buf(),
        detail: e.to_string(),
    })?;

    let final_path = dir.join(format!("{}.json", record.at));
    // Same directory, because a rename is only atomic within one filesystem
    // and the temp directory usually is not one.
    let temp = dir.join(format!(".{}.json.tmp", record.at));
    std::fs::write(&temp, &json).map_err(|source| RecordError::Io {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, &final_path).map_err(|source| RecordError::Io {
        path: final_path.clone(),
        source,
    })?;

    Ok(final_path)
}

/// File a record in the default history directory.
pub fn file_default(record: &InstallRecord) -> Result<PathBuf, RecordError> {
    let dir = hangar_core::paths::install_history_dir().ok_or(RecordError::NoHistoryDir)?;
    file(record, &dir)
}

/// Every record in `dir`, newest first.
///
/// A file that will not parse is **skipped rather than fatal**: one bad record
/// must not hide the history either side of it.
pub fn list(dir: &Path) -> Vec<InstallRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<InstallRecord> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| std::fs::read(&p).ok())
        .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
        .collect();
    found.sort_by(|a, b| b.at.cmp(&a.at));
    found
}

/// Every record in the default history directory, newest first.
pub fn list_default() -> Vec<InstallRecord> {
    hangar_core::paths::install_history_dir()
        .map(|d| list(&d))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor::Produces;
    use std::collections::BTreeSet;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-record-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn record(at: &str) -> InstallRecord {
        InstallRecord {
            at: at.to_string(),
            installer: PathBuf::from("/home/j/Downloads/setup.exe"),
            installer_bytes: 800_000,
            engine: "InstallShield".into(),
            product: Some("Black Square - Commander 114 MSFS2024".into()),
            vendor: Some("Just Flight (MSFS 2024)".into()),
            profile: "Just Flight / Black Square".into(),
            profile_origin: "shipped".into(),
            prefix: "MSFS".into(),
            command: "\"/…/proton\" run /home/j/Downloads/setup.exe".into(),
            cwd: PathBuf::from("/opt/hangar"),
            ran_for_secs: 412.5,
            exit_code: Some(0),
            verdict: "no complaint".into(),
            bad_console: Vec::new(),
            landed: vec![RecordedItem {
                kind: "Package".into(),
                name: "bksq-aircraft-commander114".into(),
                detail: "Community/bksq-aircraft-commander114".into(),
            }],
            concerns: Vec::new(),
            verified: Some(Verified {
                package: "bksq-aircraft-commander114".into(),
                verified: 914,
                declared: 914,
                missing: 0,
                wrong_size: 0,
                orphans: 0,
            }),
            shimmed: Vec::new(),
            origin: ORIGIN_INSTALLER.into(),
            replaced: Vec::new(),
        }
    }

    #[test]
    fn a_record_survives_a_round_trip_through_the_file() {
        let s = Scratch::new("roundtrip");
        let original = record("2026-08-26T16-13-13Z");

        let path = file(&original, &s.0).expect("filed");
        assert_eq!(path.file_name().unwrap(), "2026-08-26T16-13-13Z.json");

        let read: InstallRecord =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
        assert_eq!(read, original);
    }

    #[test]
    fn the_temporary_file_does_not_survive() {
        let s = Scratch::new("temp");
        file(&record("2026-08-26T16-13-13Z"), &s.0).expect("filed");

        let leftovers: Vec<_> = std::fs::read_dir(&s.0)
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn records_come_back_newest_first() {
        let s = Scratch::new("order");
        for at in [
            "2026-08-24T10-00-00Z",
            "2026-08-26T16-13-13Z",
            "2026-08-25T09-30-00Z",
        ] {
            file(&record(at), &s.0).expect("filed");
        }

        let found = list(&s.0);
        assert_eq!(
            found.iter().map(|r| r.at.as_str()).collect::<Vec<_>>(),
            [
                "2026-08-26T16-13-13Z",
                "2026-08-25T09-30-00Z",
                "2026-08-24T10-00-00Z"
            ],
            "the timestamp format sorts chronologically as a string"
        );
    }

    /// One bad record must not hide the history either side of it.
    #[test]
    fn an_unreadable_record_is_skipped_rather_than_fatal() {
        let s = Scratch::new("corrupt");
        file(&record("2026-08-26T16-13-13Z"), &s.0).expect("filed");
        std::fs::write(s.0.join("2026-08-27T00-00-00Z.json"), b"{ truncated").expect("write");

        let found = list(&s.0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].at, "2026-08-26T16-13-13Z");
    }

    #[test]
    fn a_directory_that_does_not_exist_is_an_empty_history() {
        assert!(list(Path::new("/nonexistent/installs")).is_empty());
    }

    #[test]
    fn a_summary_names_what_landed_and_how_much_verified() {
        let line = record("2026-08-26T16-13-13Z").summary();
        assert!(line.contains("Commander 114"));
        assert!(line.contains("bksq-aircraft-commander114"));
        assert!(line.contains("914/914 verified"));
    }

    #[test]
    fn an_install_that_produced_nothing_says_so_rather_than_showing_an_empty_list() {
        let mut r = record("2026-08-26T16-13-13Z");
        r.landed.clear();
        r.verified = None;
        assert!(r.summary().contains("nothing"));
        assert!(!r.is_clean());
    }

    /// A drop and an installer run appear in **one** list. Two histories would
    /// disagree about what is installed the first time somebody consulted only
    /// one of them.
    #[test]
    fn a_drop_is_recorded_beside_an_installer_run() {
        use crate::drop::{Placed, Superseded, Verification};
        use crate::verify::PackageReport;

        let s = Scratch::new("drop");
        file(&record("2026-08-26T16-13-13Z"), &s.0).expect("filed");

        let placed = vec![Placed {
            name: "vbcorp-airport-kpvf-placerville".into(),
            creator: "Vincent Bazillio".into(),
            at: PathBuf::from("/…/Community2024/vbcorp-airport-kpvf-placerville"),
            // The superseded copy under its **own** older folder name: the
            // vendor dropped the `zzz-` prefix between versions, and recording
            // the new name here would file a record saying it replaced itself.
            replaced: vec![Superseded {
                name: "zzz-vbcorp-airport-kpvf-placerville".into(),
                moved_to: PathBuf::from(
                    "/…/.hangar-replaced/2026-08-31T09-00-00Z/zzz-vbcorp-airport-kpvf-placerville",
                ),
                version: Some("1.0.0".into()),
            }],
            verification: Verification::Checked(PackageReport {
                declared: 412,
                ..Default::default()
            }),
            bytes: 692_000_000,
        }];

        let dropped = InstallRecord::of_drop(
            Path::new("/home/j/Downloads/kpvf-v3.zip"),
            660_000_000,
            Path::new("/…/Packages/Community2024"),
            &placed,
        );

        assert!(dropped.is_drop());
        assert_eq!(dropped.engine, "zip", "what it arrived as, honestly");
        assert_eq!(dropped.vendor.as_deref(), Some("Vincent Bazillio"));
        assert!(
            dropped.command.is_empty(),
            "no process ran, so no command is claimed"
        );
        assert_eq!(dropped.replaced.len(), 1);
        assert_eq!(
            dropped.replaced[0].name, "zzz-vbcorp-airport-kpvf-placerville",
            "the record names what was replaced, not what replaced it"
        );
        assert_eq!(dropped.replaced[0].was_version.as_deref(), Some("1.0.0"));
        assert!(
            dropped.summary().contains("replacing"),
            "{}",
            dropped.summary()
        );

        file(&dropped, &s.0).expect("filed");
        let both = list(&s.0);
        assert_eq!(both.len(), 2, "one history, both origins");
        assert_eq!(
            both.iter().filter(|r| r.is_drop()).count(),
            1,
            "and they are distinguishable"
        );
    }

    /// A folder has no extension, and must not read as a nameless archive.
    #[test]
    fn a_dropped_folder_records_what_it_was() {
        let dropped = InstallRecord::of_drop(
            Path::new("/home/j/Downloads/KPVF Placerville v3"),
            0,
            Path::new("/…/Community2024"),
            &[],
        );
        assert_eq!(dropped.engine, "folder");
        assert!(dropped.summary().contains("nothing"), "nothing was placed");
        assert!(!dropped.is_clean());
    }

    /// Records written before drops existed have no `origin` and must still
    /// read.
    #[test]
    fn a_record_from_before_drop_install_still_parses_as_an_installer_run() {
        let older = r#"{
            "at": "2026-08-26T16-13-13Z",
            "installer": "/home/j/setup.exe",
            "installer_bytes": 800000,
            "engine": "InstallShield",
            "product": null, "vendor": null,
            "profile": "p", "profile_origin": "shipped", "prefix": "MSFS",
            "command": "proton run setup.exe", "cwd": "/srv",
            "ran_for_secs": 1.0, "exit_code": 0,
            "verdict": "no complaint", "bad_console": [],
            "landed": [], "concerns": [], "verified": null
        }"#;
        let read: InstallRecord = serde_json::from_str(older).expect("parse");
        assert_eq!(read.origin, ORIGIN_INSTALLER);
        assert!(!read.is_drop());
        assert!(read.replaced.is_empty());
    }

    /// The whole point of a record: none of this is recoverable by scanning
    /// the finished machine.
    #[test]
    fn a_record_is_built_from_the_stages_that_produced_it() {
        use crate::run::ConsoleHit;
        use crate::snapshot::{Diff, Seen, Snapshot};
        use std::time::Duration;

        let empty = || Snapshot {
            community: Seen::Read(BTreeSet::new()),
            executables: Seen::Read(BTreeSet::new()),
            addons: Seen::Read(BTreeSet::new()),
            notes: Vec::new(),
        };
        let classified = crate::classify::classify(
            &Diff::between(&empty(), &empty()),
            &Verdict::NoComplaint,
            Produces::Package,
            Path::new("/nonexistent"),
        );

        let ran = Ran {
            code: Some(0),
            after: Duration::from_secs(3),
            log: Vec::new(),
            hits: vec![ConsoleHit {
                signature: "valid purchase".into(),
                line: "no valid purchase found".into(),
            }],
            spoke: true,
        };
        let invocation = Invocation {
            program: "proton".into(),
            args: Vec::new(),
            cwd: PathBuf::from("/srv"),
            env: Vec::new(),
        };
        let table = crate::vendor::Table::shipped();
        let profile = table
            .match_on(
                crate::engine::Engine::InstallShield,
                "Just Flight (MSFS 2024)",
                "Black Square - Commander 114 MSFS2024",
            )
            .expect("a profile")
            .profile;

        let inspection = crate::inspect::Inspection {
            path: PathBuf::from("/home/j/setup.exe"),
            size: 800_000,
            pe: None,
            engine: crate::engine::Engine::InstallShield,
            evidence: Vec::new(),
            packed: None,
            siblings: Default::default(),
            identity: crate::inspect::Identity {
                product: Some("Black Square - Commander 114 MSFS2024".into()),
                vendor: Some("Just Flight (MSFS 2024)".into()),
                version: Some("0.1.0".into()),
                source: crate::inspect::Source::Siblings,
            },
            hits: Vec::new(),
        };

        let record = InstallRecord::of(
            &inspection,
            profile,
            &invocation,
            &ran,
            &Verdict::BadConsole(ran.hits.clone()),
            &classified,
            None,
            Vec::new(),
        );

        assert_eq!(record.profile, "Just Flight / Black Square");
        assert_eq!(record.profile_origin, "shipped");
        assert_eq!(record.prefix, "MSFS");
        assert_eq!(
            record.bad_console,
            vec!["no valid purchase found".to_string()]
        );
        assert!(record.verdict.contains("known-bad"));
        assert!(!record.is_clean());
        assert!(!record.at.is_empty());
    }
}
