//! Stage 3 — the pre-flight gate: everything that can be known *before* an
//! installer is allowed to run.
//!
//! Five checks, all read-only:
//!
//! 1. the target prefix exists and looks like itself — `UserCfg.opt` for the
//!    MSFS prefix, an initialised `drive_c` for the companion one;
//! 2. every runtime requirement named in the vendor profile is demonstrably
//!    met;
//! 3. there is room where the payload will land;
//! 4. the sim is not running when the install targets Community;
//! 5. what `powershell.exe` is in the prefix, because Wine's stub exits 0 for
//!    every query and that reads as *the application is already running* to
//!    every electron-builder installer — see [`crate::shim`].
//!
//! The gate **reads** the prefix's text registry and calls `statvfs`. It never
//! runs Wine, never writes anything, and starts no process — starting a Wine
//! process to answer a question about a prefix changes the thing being
//! measured.
//!
//! # Three answers, not two
//!
//! Each check ends in [`Outcome::Pass`] with its evidence, [`Outcome::Refuse`]
//! with a typed [`Refusal`], or [`Outcome::Unchecked`] with why it could not
//! be answered. Unchecked is a real answer: a user's vendor table may name a
//! requirement HangarRS has no checker for yet, and reporting "cannot tell"
//! is the honest alternative to both passing it silently and refusing
//! everything unrecognised. Policy belongs to the caller; the report only has
//! to be impossible to misread.
//!
//! Every check runs even after one refuses. An installer that needs three
//! fixes should say so in one pass rather than in three attempts.
//!
//! # Why exit codes are not consulted
//!
//! They are stage 4's problem, and four of the five known failure modes exit
//! 0 anyway. Nothing here inspects the installer at all — stage 1 did that.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::registry::{self, Hive};
use crate::shim;
use crate::vendor::{self, Prefix, Produces, Profile};
use hangar_core::disk;

/// Where the runtime requirements are checked. All shipped entries that name
/// runtimes target the MSFS prefix, so this follows [`Profile::prefix`].
///
/// `Software\dotnet\Setup\InstalledVersions\<arch>` is written by Wine's MSI
/// run when a .NET runtime lands — **but the two arches land in different
/// views**, as they do on Windows: the x64 installer writes
/// `HKLM\Software\dotnet`, and the x86 installer is reflected into
/// `HKLM\Software\Wow6432Node\dotnet`. A real prefix carries both shapes, so
/// each arch tries its own view first and falls back to the other rather than
/// refusing on a technicality.
fn dotnet_keys(arch: &str) -> Vec<String> {
    let base = |view: &str| format!(r"{view}\dotnet\Setup\InstalledVersions\{arch}");
    if arch == "x86" {
        vec![base(r"Software\Wow6432Node"), base(r"Software")]
    } else {
        vec![base(r"Software"), base(r"Software\Wow6432Node")]
    }
}

/// The `InstallLocation` for an arch, from whichever of its two registry
/// views carries it. `Err` holds the outcome to report when neither does,
/// naming every place that was looked. The failing [`Refusal::RuntimeMissing`]
/// reports `id_on_failure`, so a refusal always names the requirement the
/// vendor table actually wrote.
fn install_location(
    prefix: &Path,
    arch: &str,
    id_on_failure: &str,
) -> Result<(String, String), Outcome> {
    let keys = dotnet_keys(arch);
    for key in &keys {
        if let Some(loc) = registry::value(prefix, Hive::LocalMachine, key, "InstallLocation") {
            if !loc.trim().is_empty() {
                return Ok((loc, key.clone()));
            }
        }
    }
    Err(Outcome::Refuse(Refusal::RuntimeMissing {
        id: id_on_failure.to_owned(),
        detail: format!(
            "no InstallLocation under InstalledVersions\\{arch} — tried {}",
            keys.iter()
                .map(|k| k.replace('\\', "\\\\"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }))
}

/// Everything the gate needs to know, carried in rather than discovered.
///
/// Discovery is someone else's job (the app actor, or the `gate` example);
/// keeping it out of here is what makes every check testable against a
/// scratch directory, and what keeps `sim_running` an injection point rather
/// than a `/proc` scan a test cannot control.
pub struct Request<'a> {
    pub profile: &'a Profile,
    /// The MSFS Proton prefix — `…/compatdata/2537590/pfx`.
    pub msfs_prefix: &'a Path,
    /// The companion prefix, when one is known.
    pub companion_prefix: Option<&'a Path>,
    /// Bytes the install is expected to need. `None` means nobody estimated,
    /// which the space check reports as unchecked rather than guessing.
    pub payload_bytes: Option<u64>,
    /// Whether the sim is running right now.
    pub sim_running: bool,
    /// Whether an install into the target prefix can write what the sim
    /// reads — its Community folder and `EXE.xml`.
    ///
    /// Carried in rather than worked out here, like everything else in this
    /// struct. [`reaches_sim_content`] computes it from two prefix paths;
    /// `None` means nobody did, and the check reports `Unchecked`.
    pub reaches_sim_content: Option<bool>,
}

impl Request<'_> {
    /// The prefix this profile's checks run against.
    fn target(&self) -> Result<(&'static str, &Path), Refusal> {
        match self.profile.prefix {
            Prefix::Msfs => Ok(("MSFS", self.msfs_prefix)),
            Prefix::Companion => self
                .companion_prefix
                .map(|p| ("companion", p))
                .ok_or(Refusal::NoCompanionPrefix),
        }
    }
}

/// One check's ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Held, with what was observed.
    Pass(String),
    /// Could not be judged, with why. Never counts as met.
    Unchecked(String),
    /// Failed, and the install must not start.
    Refuse(Refusal),
}

/// Why an install may not start. Typed, because a refusal a caller can match
/// on is one it can explain, log and count — unlike a string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    #[error("no usable {kind} prefix at {path}")]
    NoPrefix { kind: &'static str, path: PathBuf },

    #[error("no companion prefix is known to HangarRS")]
    NoCompanionPrefix,

    #[error("{id}: {detail}")]
    RuntimeMissing { id: String, detail: String },

    #[error("cannot tell how much room is free at {path}")]
    SpaceUnknown { path: PathBuf },

    #[error(
        "not enough room at {path}: {} needed, {} free",
        disk::human(*.need),
        disk::human(*.have)
    )]
    NotEnoughSpace { path: PathBuf, have: u64, need: u64 },

    #[error(
        "MSFS is running — a package landing in Community mid-flight can be \
         loaded half-written"
    )]
    SimRunning,

    // ── Drop-install refusals, on the same type deliberately ──
    // A drop that refuses is refused in exactly the same shape as an installer
    // that refuses for lack of room.
    #[error(
        "writing to the Community folder is switched off — the toggle is in \
         Settings, and it is off by default"
    )]
    WritesNotAllowed,

    #[error("no Community folder was found to install into")]
    NoCommunity,

    #[error("{count} Community folders were found and nothing has said which — {names}")]
    GenerationUnchosen { count: usize, names: String },

    #[error("{path} is no longer a discovered Community folder — choose a destination again")]
    CommunityNotDiscovered { path: std::path::PathBuf },
}

/// One named check and how it ended.
#[derive(Debug, Clone)]
pub struct Checked {
    pub label: String,
    pub outcome: Outcome,
}

/// Every check's ending. Inspect [`Report::outcomes`] directly, or use the
/// helpers.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub outcomes: Vec<Checked>,
}

impl Report {
    /// Nothing refused. Unchecked requirements do not block here — they are
    /// surfaced by [`Self::unchecked`] for whoever sets policy.
    pub fn is_clear(&self) -> bool {
        self.refusals().is_empty()
    }

    pub fn refusals(&self) -> Vec<&Refusal> {
        self.outcomes
            .iter()
            .filter_map(|c| c.outcome.refusal())
            .collect()
    }

    /// Labels of the checks that could not be judged.
    pub fn unchecked(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|c| matches!(c.outcome, Outcome::Unchecked(_)))
            .map(|c| c.label.as_str())
            .collect()
    }
}

impl Outcome {
    fn refusal(&self) -> Option<&Refusal> {
        match self {
            Outcome::Refuse(r) => Some(r),
            _ => None,
        }
    }
}

/// Run every check the request names. Reads only; writes nothing.
pub fn run(request: Request<'_>) -> Report {
    let mut outcomes = Vec::new();
    let target = request.target();

    // 1. The target prefix exists and looks like itself.
    match &target {
        Ok((kind, path)) => outcomes.push(prefix_check(kind, path)),
        Err(refusal) => outcomes.push(Checked {
            label: "prefix".into(),
            outcome: Outcome::Refuse(refusal.clone()),
        }),
    }

    // 2. Runtime requirements. Without a target there is nothing to look in,
    //    so each is reported unchecked rather than invented.
    for id in &request.profile.runtimes {
        let outcome = match &target {
            Ok((_, path)) => runtime_check(path, id),
            Err(_) => Outcome::Unchecked(format!("{id} could not be checked — no target prefix")),
        };
        outcomes.push(Checked {
            label: format!("runtime {id}"),
            outcome,
        });
    }

    // 3. Room where the payload will land.
    let space = match &target {
        Ok((_, _))
            if request.profile.payload == crate::vendor::Payload::Downloads
                && request.payload_bytes.is_none() =>
        {
            Outcome::Unchecked("installer downloads its payload; required space is unknown".into())
        }
        Ok((_, path)) => space_check(path, request.payload_bytes),
        Err(_) => Outcome::Unchecked("space could not be checked — no target prefix".into()),
    };
    outcomes.push(Checked {
        label: "space".into(),
        outcome: space,
    });

    // 4. An install must not race the sim if it can reach what the sim reads.
    //    Which prefix it targets does not settle that — see `sim_check`.
    outcomes.push(Checked {
        label: "sim quiet".into(),
        outcome: sim_check(
            request.reaches_sim_content,
            request.profile.produces,
            request.sim_running,
        ),
    });

    // 5. Wine's PowerShell stub answers 0 to everything, which reads as "the
    //    application is already running" to every electron-builder installer.
    let powershell = match &target {
        Ok((_, path)) => powershell_check(path, request.profile.powershell),
        Err(_) => Outcome::Unchecked("PowerShell could not be checked — no target prefix".into()),
    };
    outcomes.push(Checked {
        label: "powershell".into(),
        outcome: powershell,
    });

    Report { outcomes }
}

/// Check 5. Reports what is in the prefix and what stage 4 will do about it.
///
/// Never a [`Refusal`]: a stub that is about to be parked is not a reason to
/// stop, it is a reason the run will work. The two answers that matter are
/// both visible before anything starts, which is the whole point of the gate.
///
/// [`vendor::PowerShell::Keep`] against a stub is `Unchecked` rather than
/// `Pass`, because the install is then proceeding with a known-wrong oracle in
/// the prefix and the report must not read as though that were fine.
fn powershell_check(prefix: &Path, policy: vendor::PowerShell) -> Outcome {
    let seen = shim::survey(prefix);
    // Named relative to the prefix. The absolute paths are long enough that a
    // column-limited report elides the tail, and the tail is the only part
    // that says *which* of the two — `system32` or `syswow64` — was found.
    let name = |path: &Path| {
        path.strip_prefix(prefix)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let stubs: Vec<_> = seen
        .iter()
        .filter(|s| s.kind == shim::Kind::Builtin)
        .map(|s| name(&s.path))
        .collect();

    if let Some(why) = seen.iter().find_map(|s| match &s.kind {
        shim::Kind::Unreadable(why) => Some(format!("{}: {why}", name(&s.path))),
        _ => None,
    }) {
        return Outcome::Unchecked(format!("could not read {why}"));
    }

    if stubs.is_empty() {
        let real = seen.iter().filter(|s| s.kind == shim::Kind::Real).count();
        return Outcome::Pass(if real > 0 {
            format!("{real} real PowerShell in this prefix — left alone")
        } else {
            "no PowerShell in this prefix — nothing to answer wrongly".into()
        });
    }

    match policy {
        vendor::PowerShell::Park => Outcome::Pass(format!(
            "Wine's PowerShell stub exits 0 for every query ({}) — parked for the run \
             so a running-check falls back to tasklist",
            stubs.join(", ")
        )),
        vendor::PowerShell::Keep => Outcome::Unchecked(format!(
            "Wine's PowerShell stub is present ({}) and this profile keeps it — any \
             running-check it answers will say the application is running",
            stubs.join(", ")
        )),
    }
}

/// Check 1. What "looks like itself" means depends on the prefix: the MSFS
/// one is proven by `UserCfg.opt`, the file every later stage keys off; a
/// companion prefix by an initialised `drive_c`.
fn prefix_check(kind: &'static str, prefix: &Path) -> Checked {
    let proof = match kind {
        "MSFS" => msfs_user_cfg(prefix),
        _ => prefix.join("drive_c"),
    };
    if proof.exists() {
        Checked {
            label: format!("{kind} prefix"),
            outcome: Outcome::Pass(match kind {
                "MSFS" => format!("UserCfg.opt found at {}", elide_home(&proof)),
                _ => format!("initialised prefix at {}", proof.display()),
            }),
        }
    } else {
        Checked {
            label: format!("{kind} prefix"),
            outcome: Outcome::Refuse(Refusal::NoPrefix { kind, path: proof }),
        }
    }
}

/// Check 2, dispatched on the requirement id from the vendor table. An id
/// with no checker here is `Unchecked`, never `Pass` — a table entry naming a
/// requirement must not have its silence mistaken for satisfaction.
fn runtime_check(prefix: &Path, id: &str) -> Outcome {
    match id {
        "dotnet-install-location" => check_install_location(prefix),
        "dotnet-desktop-x64" => check_desktop_runtime(prefix, "x64", None),
        "dotnet-desktop-6-x86" => check_desktop_runtime(prefix, "x86", Some(6)),
        "prefix-fonts" => check_fonts(prefix),
        other => Outcome::Unchecked(format!(
            "no checker for requirement id {other:?} yet — add one or accept it by hand"
        )),
    }
}

/// The value the whole dotnet story turns on. Wine's MSI run writes
/// `InstalledVersions\x64\sharedhost` but omits `InstallLocation`; without it
/// a 64-bit app spawned by a 32-bit installer inherits
/// `ProgramFiles=C:\Program Files (x86)` and looks where the runtime is not —
/// while `dotnet --list-runtimes` works fine from a shell throughout.
fn check_install_location(prefix: &Path) -> Outcome {
    match install_location(prefix, "x64", "dotnet-install-location") {
        Ok((loc, key)) => Outcome::Pass(format!("InstallLocation = {loc}  [{key}]")),
        Err(outcome) => outcome,
    }
}

/// The Windows Desktop Runtime is proven by what it lays down:
/// `<InstallLocation>\shared\Microsoft.WindowsDesktop.App\<versions>`.
///
/// `need_major` pins the generation — the Garmin stack's x86 launcher wants
/// .NET 6 specifically, and an 8.x beside it does not satisfy that.
fn check_desktop_runtime(prefix: &Path, arch: &str, need_major: Option<u32>) -> Outcome {
    let id = match need_major {
        Some(major) => format!("dotnet-desktop-{major}-{arch}"),
        None => format!("dotnet-desktop-{arch}"),
    };

    let (location, key) = match install_location(prefix, arch, &id) {
        Ok(found) => found,
        Err(outcome) => return outcome,
    };

    let Some(root) = win_to_linux(prefix, &location) else {
        return Outcome::Refuse(Refusal::RuntimeMissing {
            detail: format!("{location} is not a drive-letter path"),
            id,
        });
    };

    let desktop = root.join("shared").join("Microsoft.WindowsDesktop.App");
    let mut versions: Vec<String> = std::fs::read_dir(&desktop)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    versions.sort();

    if versions.is_empty() {
        return Outcome::Refuse(Refusal::RuntimeMissing {
            detail: format!("no Windows Desktop Runtime under {}", desktop.display()),
            id,
        });
    }

    if let Some(major) = need_major {
        let wanted = format!("{major}.");
        if !versions.iter().any(|v| v.starts_with(&wanted)) {
            return Outcome::Refuse(Refusal::RuntimeMissing {
                detail: format!("found [{}], need a {major}.x", versions.join(", ")),
                id,
            });
        }
    }

    Outcome::Pass(format!("{} at {}  [{key}]", versions.join(", "), location))
}

/// WPF lays out text at startup, and an empty `C:\windows\Fonts`
/// hard-crashes it — a fresh-prefix failure. Fonts present is a fact; how many
/// is the evidence.
fn check_fonts(prefix: &Path) -> Outcome {
    let fonts = prefix.join("drive_c/windows/Fonts");
    let count = std::fs::read_dir(&fonts)
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    if count == 0 {
        Outcome::Refuse(Refusal::RuntimeMissing {
            detail: format!(
                "{} is empty or missing — WPF hard-crashes during text layout \
                 in a fontless prefix",
                fonts.display()
            ),
            id: "prefix-fonts".into(),
        })
    } else {
        Outcome::Pass(format!("{count} font files"))
    }
}

/// Check 3. `statvfs` on the prefix root answers for the filesystem it sits
/// on, which is the one the payload lands on. Unknown is not zero: a full
/// disk and an absent disk are different problems with different fixes.
fn space_check(target: &Path, payload: Option<u64>) -> Outcome {
    let Some(need) = payload else {
        return Outcome::Unchecked("no size estimate supplied".into());
    };
    let Some(have) = disk::free_bytes(target) else {
        return Outcome::Refuse(Refusal::SpaceUnknown {
            path: target.to_path_buf(),
        });
    };
    if have < need {
        Outcome::Refuse(Refusal::NotEnoughSpace {
            path: target.to_path_buf(),
            have,
            need,
        })
    } else {
        Outcome::Pass(format!(
            "{} free against {} needed",
            disk::human(have),
            disk::human(need)
        ))
    }
}

/// Check 4. Whether the install may race a running sim.
///
/// **What the profile produces is not enough to answer this.** FSUIPC7 is a
/// `CompanionApp` by every static measure — NSIS, companion prefix, no
/// Community payload — yet installing it adds a `<Launch.Addon>` to the
/// *sim's* `EXE.xml` when the companion prefix symlinks the sim's
/// `AppData/Roaming/Microsoft Flight Simulator 2024` into the MSFS prefix.
///
/// So the question is about the *filesystem*, not the vendor table: can an
/// install into this prefix reach what the sim reads? `reaches_sim_content`
/// answers it by resolving the link. A caller that has not worked it out
/// passes `None`, and gets `Unchecked` rather than a pass.
fn sim_check(reaches_sim: Option<bool>, produces: Produces, sim_running: bool) -> Outcome {
    match reaches_sim {
        None => Outcome::Unchecked(
            "nobody worked out whether this prefix can reach what the sim reads".into(),
        ),
        Some(false) => Outcome::Pass(format!(
            "targets {}, in a prefix that cannot reach the sim's content",
            produces.label()
        )),
        Some(true) if sim_running => Outcome::Refuse(Refusal::SimRunning),
        Some(true) => Outcome::Pass("sim not running".into()),
    }
}

/// Can an install into `target_prefix` write what the sim reads?
///
/// The sim's Community folder and `EXE.xml` both live under
/// `drive_c/users/steamuser/AppData/Roaming/Microsoft Flight Simulator …`.
/// A prefix that is genuinely separate has its own copy of that path, or none
/// at all; a prefix that has been linked to the sim's resolves to the same
/// place. Both are real configurations, and only the filesystem knows which
/// one is in front of you.
///
/// `None` means the question could not be answered — neither prefix had the
/// directory, or it would not canonicalise. Never guess `false` there: a
/// wrong `false` lets an install write `EXE.xml` while the gate says the sim
/// is irrelevant.
pub fn reaches_sim_content(target_prefix: &Path, msfs_prefix: &Path) -> Option<bool> {
    // Same prefix, nothing to resolve.
    if let (Ok(a), Ok(b)) = (target_prefix.canonicalize(), msfs_prefix.canonicalize()) {
        if a == b {
            return Some(true);
        }
    }

    let msfs_root = msfs_prefix.canonicalize().ok()?;

    for generation in SIM_APPDATA_NAMES {
        let candidate = target_prefix
            .join("drive_c/users/steamuser/AppData/Roaming")
            .join(generation);
        let Ok(resolved) = candidate.canonicalize() else {
            continue;
        };
        // Resolved inside the sim's prefix — one file, two names.
        return Some(resolved.starts_with(&msfs_root));
    }

    None
}

/// The sim's AppData directory names, newest first.
const SIM_APPDATA_NAMES: &[&str] = &[
    "Microsoft Flight Simulator 2024",
    "Microsoft Flight Simulator",
];

/// `C:\Program Files\dotnet` → `<prefix>/drive_c/Program Files/dotnet`.
///
/// Accepts either drive case and both separators, because the value comes
/// from whatever the installer wrote.
fn win_to_linux(prefix: &Path, win: &str) -> Option<PathBuf> {
    let trimmed = win.trim();
    let drive = trimmed.chars().next()?.to_ascii_lowercase();
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    let rest = trimmed.get(1..)?.strip_prefix(':')?;
    let rel: String = rest
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    Some(prefix.join("drive_c").join(rel))
}

fn msfs_user_cfg(msfs_prefix: &Path) -> PathBuf {
    msfs_prefix
        .join("drive_c/users/steamuser/AppData/Roaming")
        .join(hangar_core::MSFS2024_APPDATA_DIR)
        .join("UserCfg.opt")
}

/// Paths printed as evidence get long; keep them readable without hiding
/// anything.
fn elide_home(path: &Path) -> String {
    let text = path.display().to_string();
    if text.chars().count() <= 70 {
        return text;
    }
    let head: String = text.chars().take(67).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::vendor::Table;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hangar-gate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An MSFS prefix reduced to what the gate reads: UserCfg.opt and
    /// system.reg. Nothing else is looked at.
    fn msfs_prefix(name: &str, system_reg: &str) -> PathBuf {
        let prefix = scratch(name);
        let app_data = prefix
            .join("drive_c/users/steamuser/AppData/Roaming")
            .join(hangar_core::MSFS2024_APPDATA_DIR);
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::write(app_data.join("UserCfg.opt"), "[Packages]\n").unwrap();
        if !system_reg.is_empty() {
            std::fs::write(prefix.join("system.reg"), system_reg).unwrap();
        }
        prefix
    }

    /// Verbatim shape from the real prefix: `InstallLocation` present.
    const WITH_INSTALL_LOCATION: &str = r#"
[Software\\dotnet\\Setup\\InstalledVersions\\x64] 1785710557
#time=1dd22d0377dc946
"InstallLocation"="C:\\Program Files\\dotnet"

[Software\\dotnet\\Setup\\InstalledVersions\\x64\\sharedhost] 1785709223
#time=1dd22cd1c9069de
"Path"="C:\\Program Files\\dotnet\\"
"Version"="8.0.29"
"#;

    /// The trap: Wine's MSI run writes `sharedhost` and omits the parent's
    /// `InstallLocation`. Everything else about the prefix says .NET is fine.
    const SHAREDHOST_ONLY: &str = r#"
[Software\\dotnet\\Setup\\InstalledVersions\\x64] 1785710557
#time=1dd22d0377dc946

[Software\\dotnet\\Setup\\InstalledVersions\\x64\\sharedhost] 1785709223
#time=1dd22cd1c9069de
"Path"="C:\\Program Files\\dotnet\\"
"Version"="8.0.29"
"#;

    fn shipped_profile(engine: Engine, company: &str, product: &str) -> Profile {
        Table::shipped()
            .match_on(engine, company, product)
            .expect("a shipped profile")
            .profile
            .clone()
    }

    fn just_flight() -> Profile {
        shipped_profile(
            Engine::InstallShield,
            "Just Flight (MSFS 2024)",
            "Black Square - Commander 114 MSFS2024",
        )
    }

    fn nsis_any() -> Profile {
        shipped_profile(Engine::Nsis, "Anyone", "Anything")
    }

    #[test]
    fn an_unbuilt_msfs_prefix_is_refused_on_its_own_evidence() {
        let prefix = scratch("empty");
        let report = run(Request {
            profile: &just_flight(),
            msfs_prefix: &prefix,
            companion_prefix: None,
            payload_bytes: None,
            sim_running: false,
            reaches_sim_content: Some(true),
        });
        assert!(!report.is_clear());
        assert!(matches!(
            report.outcomes[0].outcome,
            Outcome::Refuse(Refusal::NoPrefix { .. })
        ));
    }

    #[test]
    fn a_companion_target_without_a_known_prefix_is_refused_differently() {
        // No companion prefix known at all is not the same answer as one that
        // exists but is empty, and the two have different fixes.
        let prefix = scratch("msfs-side");
        let report = run(Request {
            profile: &nsis_any(),
            msfs_prefix: &prefix,
            companion_prefix: None,
            payload_bytes: None,
            sim_running: false,
            reaches_sim_content: Some(true),
        });
        assert!(matches!(
            report.outcomes[0].outcome,
            Outcome::Refuse(Refusal::NoCompanionPrefix)
        ));
    }

    #[test]
    fn an_uninitialised_companion_prefix_is_refused_by_its_missing_drive_c() {
        let bare = scratch("bare-companion");
        let msfs = msfs_prefix("side", "");
        let report = run(Request {
            profile: &nsis_any(),
            msfs_prefix: &msfs,
            companion_prefix: Some(&bare),
            payload_bytes: None,
            sim_running: false,
            reaches_sim_content: Some(true),
        });
        assert!(matches!(
            report.outcomes[0].outcome,
            Outcome::Refuse(Refusal::NoPrefix {
                kind: "companion",
                ..
            })
        ));

        std::fs::create_dir_all(bare.join("drive_c")).unwrap();
        let report = run(Request {
            profile: &nsis_any(),
            msfs_prefix: &msfs,
            companion_prefix: Some(&bare),
            payload_bytes: None,
            sim_running: false,
            reaches_sim_content: Some(true),
        });
        assert!(report.outcomes[0].outcome.refusal().is_none());
    }

    #[test]
    fn install_location_present_is_passed_with_the_value_as_evidence() {
        let prefix = msfs_prefix("loc-ok", WITH_INSTALL_LOCATION);
        let outcome = check_install_location(&prefix);
        match outcome {
            Outcome::Pass(evidence) => assert!(evidence.contains(r"C:\Program Files\dotnet")),
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    #[test]
    fn the_sharedhost_trap_is_caught() {
        // sharedhost carries a Version and a Path; neither substitutes for the
        // parent's InstallLocation. This is the failure that hides behind a
        // working `dotnet --list-runtimes`.
        let prefix = msfs_prefix("loc-trap", SHAREDHOST_ONLY);
        match check_install_location(&prefix) {
            Outcome::Refuse(Refusal::RuntimeMissing { id, detail }) => {
                assert_eq!(id, "dotnet-install-location");
                assert!(detail.contains("InstallLocation"), "{detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn fake_dotnet(prefix: &Path, arch: &str, versions: &[&str]) {
        let base = prefix.join("drive_c/Program Files/dotnet/shared/Microsoft.WindowsDesktop.App");
        for v in versions {
            std::fs::create_dir_all(base.join(v)).unwrap();
        }
        let reg = format!(
            r#"[Software\\dotnet\\Setup\\InstalledVersions\\{arch}]
"InstallLocation"="C:\\Program Files\\dotnet"
"#
        );
        std::fs::write(prefix.join("system.reg"), reg).unwrap();
    }

    #[test]
    fn desktop_x64_passes_on_any_generation_it_finds() {
        let prefix = msfs_prefix("x64", "");
        fake_dotnet(&prefix, "x64", &["8.0.29"]);
        match check_desktop_runtime(&prefix, "x64", None) {
            Outcome::Pass(evidence) => assert!(evidence.contains("8.0.29"), "{evidence}"),
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    #[test]
    fn desktop_6_x86_demands_a_six_and_says_what_was_found_instead() {
        let prefix = msfs_prefix("x86-8-only", "");
        fake_dotnet(&prefix, "x86", &["8.0.29"]);
        match check_desktop_runtime(&prefix, "x86", Some(6)) {
            Outcome::Refuse(Refusal::RuntimeMissing { detail, .. }) => {
                assert!(detail.contains("8.0.29"), "{detail}");
                assert!(detail.contains("6.x"), "{detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        fake_dotnet(&prefix, "x86", &["6.0.11"]);
        assert!(check_desktop_runtime(&prefix, "x86", Some(6))
            .refusal()
            .is_none());
    }

    #[test]
    fn desktop_runtime_without_an_install_location_names_its_arch() {
        let prefix = msfs_prefix("x86-nothing", "");
        match check_desktop_runtime(&prefix, "x86", Some(6)) {
            Outcome::Refuse(Refusal::RuntimeMissing { detail, .. }) => {
                assert!(detail.contains("x86"), "{detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn fontless_prefix_is_refused_and_counted_prefix_is_not() {
        let prefix = msfs_prefix("fonts-none", "");
        match check_fonts(&prefix) {
            Outcome::Refuse(Refusal::RuntimeMissing { detail, .. }) => {
                assert!(detail.contains("Fonts"), "{detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        std::fs::create_dir_all(prefix.join("drive_c/windows/Fonts")).unwrap();
        std::fs::write(prefix.join("drive_c/windows/Fonts/arial.ttf"), b"x").unwrap();
        match check_fonts(&prefix) {
            Outcome::Pass(evidence) => assert!(evidence.contains('1'), "{evidence}"),
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_requirement_id_is_unchecked_never_passed() {
        let prefix = msfs_prefix("unknown-id", "");
        match runtime_check(&prefix, "vcredist-2022") {
            Outcome::Unchecked(why) => assert!(why.contains("vcredist-2022")),
            other => panic!("expected unchecked, got {other:?}"),
        }
    }

    #[test]
    fn space_unknown_is_not_space_insufficient() {
        // A missing mount and a full disk refuse differently; collapsing them
        // would send someone to clean files off a disk that is not attached.
        let nowhere = Path::new("/nonexistent/hangar-gate");
        match space_check(nowhere, Some(1024)) {
            Outcome::Refuse(Refusal::SpaceUnknown { .. }) => {}
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_web_installer_reports_unknown_download_space() {
        let prefix = msfs_prefix("web-installer", "");
        let profile = shipped_profile(Engine::Nsis, "Orbx", "Orbx Central");
        let report = run(Request {
            profile: &profile,
            msfs_prefix: &prefix,
            companion_prefix: Some(&prefix),
            payload_bytes: None,
            sim_running: false,
            reaches_sim_content: Some(false),
        });
        assert!(report.outcomes.iter().any(|check| matches!(&check.outcome, Outcome::Unchecked(why) if why.contains("downloads its payload"))));
    }

    #[test]
    fn no_estimate_leaves_space_unchecked_rather_than_guessing() {
        let prefix = msfs_prefix("no-estimate", "");
        match space_check(&prefix, None) {
            Outcome::Unchecked(why) => assert!(why.contains("estimate")),
            other => panic!("expected unchecked, got {other:?}"),
        }
    }

    #[test]
    fn a_payload_that_cannot_fit_is_refused_in_human_units() {
        let prefix = msfs_prefix("too-big", "");
        match space_check(&prefix, Some(u64::MAX / 2)) {
            Outcome::Refuse(refusal) => {
                let text = refusal.to_string();
                assert!(text.contains("needed"), "{text}");
                assert!(text.contains("free"), "{text}");
                // Raw bytes would be a number nobody can act on.
                assert!(!text.contains("9223372036854775807"), "{text}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_install_that_can_reach_the_sim_must_not_race_it() {
        assert!(matches!(
            sim_check(Some(true), Produces::Package, true),
            Outcome::Refuse(Refusal::SimRunning)
        ));
        assert!(sim_check(Some(true), Produces::Package, false)
            .refusal()
            .is_none());
    }

    /// FSUIPC7 is a `CompanionApp` by every static measure, yet installing it
    /// writes a `<Launch.Addon>` into the sim's `EXE.xml` when the companion
    /// prefix symlinks the sim's AppData into the MSFS prefix. Keying on
    /// `produces` would skip the check.
    #[test]
    fn a_companion_app_is_not_automatically_safe_from_a_running_sim() {
        assert!(
            matches!(
                sim_check(Some(true), Produces::CompanionApp, true),
                Outcome::Refuse(Refusal::SimRunning)
            ),
            "a companion install that can reach the sim's content races it \
             exactly like a package does"
        );
        // Genuinely separate prefix: the running sim is irrelevant.
        assert!(sim_check(Some(false), Produces::CompanionApp, true)
            .refusal()
            .is_none());
    }

    /// Unchecked is never silently a pass — a wrong `false` here lets an
    /// install write `EXE.xml` unremarked.
    #[test]
    fn a_prefix_nobody_resolved_is_unchecked_rather_than_safe() {
        assert!(matches!(
            sim_check(None, Produces::CompanionApp, true),
            Outcome::Unchecked(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_prefix_is_detected_and_a_separate_one_is_not() {
        const APPDATA: &str = "drive_c/users/steamuser/AppData/Roaming";
        let sim = "Microsoft Flight Simulator 2024";

        let root = scratch("reaches");
        let msfs = root.join("msfs-pfx");
        std::fs::create_dir_all(msfs.join(APPDATA).join(sim)).unwrap();

        // A companion prefix with its own, separate directory.
        let separate = root.join("separate-pfx");
        std::fs::create_dir_all(separate.join(APPDATA).join(sim)).unwrap();
        assert_eq!(reaches_sim_content(&separate, &msfs), Some(false));

        // A companion prefix linked into the sim's.
        let linked = root.join("linked-pfx");
        std::fs::create_dir_all(linked.join(APPDATA)).unwrap();
        std::os::unix::fs::symlink(msfs.join(APPDATA).join(sim), linked.join(APPDATA).join(sim))
            .unwrap();
        assert_eq!(reaches_sim_content(&linked, &msfs), Some(true));

        // Nothing there at all: unanswerable, not "safe".
        let bare = root.join("bare-pfx");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(reaches_sim_content(&bare, &msfs), None);
    }

    #[test]
    fn x86_registers_under_wow6432node_and_that_is_where_it_is_read() {
        // Verbatim from the real prefix. Looking only at Software\dotnet
        // refuses here — but the .NET 6 x86 runtime IS installed, reflected
        // into the 32-bit view exactly as Windows does it.
        const X86_UNDER_WOW: &str = r#"
[Software\\Wow6432Node\\dotnet\\Setup\\InstalledVersions\\x86] 1787342436
#time=1dd31a7bb8474fc
"InstallLocation"="C:\\Program Files (x86)\\dotnet\\"

[Software\\Wow6432Node\\dotnet\\Setup\\InstalledVersions\\x86\\sharedhost] 1787342436
#time=1dd31a7bb845d3c
"Version"="6.0.11"
"#;
        let prefix = msfs_prefix("wow-x86", X86_UNDER_WOW);
        std::fs::create_dir_all(
            prefix.join(
                "drive_c/Program Files (x86)/dotnet/shared/Microsoft.WindowsDesktop.App/6.0.11",
            ),
        )
        .unwrap();

        match check_desktop_runtime(&prefix, "x86", Some(6)) {
            Outcome::Pass(evidence) => {
                assert!(evidence.contains("6.0.11"), "{evidence}");
                assert!(evidence.contains("Wow6432Node"), "{evidence}");
            }
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    #[test]
    fn every_runtime_id_in_the_shipped_table_has_a_checker() {
        // A new id in vendors.toml without a checker here would ride through
        // as Unchecked forever. The shipped table is ours: it ships fully
        // understood or the test fails.
        const KNOWN: &[&str] = &[
            "dotnet-install-location",
            "dotnet-desktop-x64",
            "dotnet-desktop-6-x86",
            "prefix-fonts",
        ];
        for profile in Table::shipped().profiles {
            for id in &profile.runtimes {
                assert!(
                    KNOWN.contains(&id.as_str()),
                    "shipped table names {id:?}, which gate.rs cannot check"
                );
            }
        }
    }

    #[test]
    fn a_fully_provisioned_fake_environment_clears_every_check() {
        let prefix = msfs_prefix("happy", WITH_INSTALL_LOCATION);
        fake_dotnet(&prefix, "x64", &["8.0.29"]);
        std::fs::create_dir_all(prefix.join("drive_c/windows/Fonts")).unwrap();

        let report = run(Request {
            profile: &just_flight(),
            msfs_prefix: &prefix,
            companion_prefix: None,
            payload_bytes: Some(1024),
            sim_running: false,
            reaches_sim_content: Some(true),
        });

        assert!(report.is_clear(), "{:#?}", report.outcomes);
        assert!(report.unchecked().is_empty(), "{:#?}", report.outcomes);
        assert_eq!(report.outcomes.len(), 6); // prefix + 2 runtimes + space + sim + powershell
    }

    /// A prefix with Wine's stub in it passes, and says what stage 4 will do
    /// about it. Not a refusal: the stub is why the run will work, not why it
    /// must not start.
    #[test]
    fn a_wine_powershell_stub_is_reported_and_parked() {
        let prefix = msfs_prefix("ps-stub", WITH_INSTALL_LOCATION);
        put_stub(&prefix);

        let outcome = powershell_check(&prefix, vendor::PowerShell::Park);
        match outcome {
            Outcome::Pass(why) => {
                assert!(why.contains("exits 0"), "{why}");
                assert!(why.contains("parked"), "{why}");
            }
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    /// `keep` against a stub is never a pass. The install proceeds with an
    /// oracle in the prefix that is known to answer wrongly, and a report that
    /// read as clear would be hiding exactly the thing this check exists for.
    #[test]
    fn keeping_a_stub_is_unchecked_rather_than_a_pass() {
        let prefix = msfs_prefix("ps-keep", WITH_INSTALL_LOCATION);
        put_stub(&prefix);

        match powershell_check(&prefix, vendor::PowerShell::Keep) {
            Outcome::Unchecked(why) => {
                assert!(why.contains("will say the application is running"), "{why}")
            }
            other => panic!("expected unchecked, got {other:?}"),
        }
    }

    /// The common case on a prefix nobody has run an Electron installer into.
    #[test]
    fn no_powershell_at_all_passes_plainly() {
        let prefix = msfs_prefix("ps-none", WITH_INSTALL_LOCATION);
        match powershell_check(&prefix, vendor::PowerShell::Park) {
            Outcome::Pass(why) => assert!(why.contains("nothing to answer wrongly"), "{why}"),
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    /// A real PowerShell is left alone and said so, so a prefix somebody has
    /// deliberately provisioned does not get quietly taken apart.
    #[test]
    fn a_real_powershell_is_passed_untouched() {
        let prefix = msfs_prefix("ps-real", WITH_INSTALL_LOCATION);
        for rel in crate::shim::LOCATIONS {
            let path = prefix.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"MZ\x90\x00 a real one").unwrap();
        }
        match powershell_check(&prefix, vendor::PowerShell::Park) {
            Outcome::Pass(why) => assert!(why.contains("real PowerShell"), "{why}"),
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    /// Wine's builtin marker, in the shape it appears in a real Proton build.
    fn put_stub(prefix: &Path) {
        let mut bytes = b"MZ\x90\x00".to_vec();
        bytes.extend_from_slice(crate::shim::BUILTIN_MARKER);
        bytes.extend_from_slice(b"\r\nThis program cannot be run in DOS mode.\r\n");
        for rel in crate::shim::LOCATIONS {
            let path = prefix.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
    }

    #[test]
    fn windows_paths_translate_inside_the_prefix_only() {
        let prefix = Path::new("/pfx");
        assert_eq!(
            win_to_linux(prefix, r"C:\Program Files\dotnet"),
            Some(PathBuf::from("/pfx/drive_c/Program Files/dotnet"))
        );
        // Lower-case drive, doubled separators, and a bare root all resolve.
        assert_eq!(
            win_to_linux(prefix, r"c:\\dir\\"),
            Some(PathBuf::from("/pfx/drive_c/dir"))
        );
        assert_eq!(
            win_to_linux(prefix, r"C:\"),
            Some(PathBuf::from("/pfx/drive_c"))
        );
        // Not a Windows path: refused rather than mangled into something that
        // almost exists.
        assert_eq!(win_to_linux(prefix, "/usr/share"), None);
    }
}
