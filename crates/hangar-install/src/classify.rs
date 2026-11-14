//! Stage 5 — reading the three diffs, and saying what actually landed.
//!
//! Stage 4 refuses to conclude success; this is where the conclusion is drawn,
//! and it is drawn from what arrived rather than from what the process said:
//!
//! | What arrived | What it is |
//! |---|---|
//! | a Community directory with `manifest.json` | [`ItemKind::Package`] |
//! | a new `<Launch.Addon>` in `EXE.xml` | [`ItemKind::SimAddonExe`] |
//! | new `.exe`s in the prefix, best of them not an uninstaller | [`ItemKind::CompanionApp`] |
//! | nothing, where something was expected | a [`Concern`], naming the console |
//!
//! # An install can produce several things at once
//!
//! So this returns a list rather than a verdict: FSUIPC7 produces six
//! executables *and* an `EXE.xml` entry from one run, and returning one kind
//! would throw away half of what happened.
//!
//! # Concerns are separate from items
//!
//! What landed and what is worrying about it are different questions. An
//! install can put a real package on disk *and* register an add-on path the
//! sim cannot follow; reporting only the first hides the problem and reporting
//! only the second hides the install. [`Report`] carries both.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use hangar_catalog::ItemKind;
use hangar_msfs::env::win_path_in_prefix;

use crate::run::Verdict;
use crate::snapshot::{Change, Diff};
use crate::vendor::Produces;

/// One thing the install put on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Landed {
    /// A directory in `Community` carrying a `manifest.json`.
    Package { name: String },
    /// A `<Launch.Addon>` the sim will spawn itself.
    ///
    /// `resolved` is where its Windows path lands **in the MSFS prefix**,
    /// because that is the only prefix the sim reads it from. `None` means it
    /// points at nothing the sim can follow.
    SimAddon {
        name: String,
        win_path: String,
        resolved: Option<PathBuf>,
    },
    /// An application with a lifecycle in a prefix.
    ///
    /// `exe` is the one the scoring picked; `also` is everything else that
    /// arrived under the same top-level directory, kept because a person
    /// looking at a bad guess needs to see what it chose between.
    CompanionApp { exe: PathBuf, also: Vec<PathBuf> },
}

impl Landed {
    /// How the catalogue would file this.
    pub fn kind(&self) -> ItemKind {
        match self {
            Landed::Package { .. } => ItemKind::Package,
            Landed::SimAddon { .. } => ItemKind::SimAddonExe,
            Landed::CompanionApp { .. } => ItemKind::CompanionApp,
        }
    }

    pub fn name(&self) -> String {
        match self {
            Landed::Package { name } => name.clone(),
            Landed::SimAddon { name, .. } => name.clone(),
            Landed::CompanionApp { exe, .. } => exe
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| exe.display().to_string()),
        }
    }
}

/// Something about the outcome that a person should see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Concern {
    /// Nothing arrived in any place that could be observed.
    NothingArrived { expected: Produces },
    /// The run itself objected — a bad console string, a vanishing, a
    /// non-zero exit.
    RunObjected { detail: String },
    /// An `EXE.xml` entry whose path does not resolve inside the MSFS prefix.
    ///
    /// FSUIPC7 wrote `C:\FSUIPC7\FSUIPC7.exe` while installing into the
    /// *companion* prefix's `drive_c`. `C:` resolves against whichever prefix
    /// reads it, and the sim's has no `FSUIPC7` — so the sim will try to
    /// launch something that is not there.
    AddonPathUnreachable { name: String, win_path: String },
    /// A place could not be compared, so nothing about it is being claimed.
    Unobserved { place: String, why: String },
    /// Something disappeared. An installer removing things is not by itself
    /// wrong — an upgrade replaces what it supersedes — but it is never
    /// assumed.
    Removed { place: String, what: Vec<String> },
}

/// What stage 5 concluded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub landed: Vec<Landed>,
    pub concerns: Vec<Concern>,
}

impl Report {
    /// Something arrived and nothing about it is worrying.
    ///
    /// The nearest thing to "it worked" that this crate says, and it still
    /// does not check that what landed is *correct* — that is stage 6's job
    /// against `layout.json`.
    pub fn is_clean(&self) -> bool {
        !self.landed.is_empty() && self.concerns.is_empty()
    }
}

/// Read the diffs.
///
/// `msfs_prefix` is needed to resolve an add-on's Windows path, because the
/// sim reads `EXE.xml` from its own prefix whatever prefix wrote it.
pub fn classify(diff: &Diff, verdict: &Verdict, expected: Produces, msfs_prefix: &Path) -> Report {
    let mut landed = Vec::new();
    let mut concerns = Vec::new();

    if let Verdict::BadConsole(hits) = verdict {
        for hit in hits {
            concerns.push(Concern::RunObjected {
                detail: format!("[{}] {}", hit.signature, hit.line),
            });
        }
    } else if verdict.objected() {
        concerns.push(Concern::RunObjected {
            detail: describe(verdict),
        });
    }

    // Community: only a directory with a manifest is a package. One without
    // is reported as nothing at all, because an installer that unpacked into
    // Community and stopped has not produced a package.
    if let Some(added) = diff.community.added() {
        for package in added {
            if package.has_manifest {
                landed.push(Landed::Package {
                    name: package.name.clone(),
                });
            } else {
                concerns.push(Concern::Unobserved {
                    place: "Community".into(),
                    why: format!(
                        "{} arrived without a manifest.json, so it is not a package",
                        package.name
                    ),
                });
            }
        }
    }

    // EXE.xml: every new entry is an add-on, and each is checked against the
    // prefix the sim will read it from.
    if let Some(added) = diff.addons.added() {
        for addon in added {
            let resolved = win_path_in_prefix(msfs_prefix, &addon.path).filter(|p| p.exists());
            if resolved.is_none() {
                concerns.push(Concern::AddonPathUnreachable {
                    name: addon.name.clone(),
                    win_path: addon.path.clone(),
                });
            }
            landed.push(Landed::SimAddon {
                name: addon.name.clone(),
                win_path: addon.path.clone(),
                resolved,
            });
        }
    }

    // Prefix executables: grouped by the directory they landed in, one app
    // per group.
    if let Some(added) = diff.executables.added() {
        landed.extend(companion_apps(added));
    }

    for (place, gone) in [
        (
            "Community",
            removed_names(&diff.community, |p| p.name.clone()),
        ),
        ("EXE.xml", removed_names(&diff.addons, |a| a.name.clone())),
        (
            "prefix executables",
            removed_names(&diff.executables, |p| p.display().to_string()),
        ),
    ] {
        if !gone.is_empty() {
            concerns.push(Concern::Removed {
                place: place.into(),
                what: gone,
            });
        }
    }

    for (place, why) in diff.unknown() {
        concerns.push(Concern::Unobserved {
            place: place.into(),
            why: why.into(),
        });
    }

    // A runtime is installed *for* something else and lands where none of the
    // three places watch, so its silence is expected rather than a failure.
    if landed.is_empty() && expected != Produces::Runtime {
        concerns.push(Concern::NothingArrived { expected });
    }

    Report { landed, concerns }
}

fn removed_names<T: Ord + Clone>(change: &Change<T>, name: impl Fn(&T) -> String) -> Vec<String> {
    change
        .removed()
        .map(|set| set.iter().map(&name).collect())
        .unwrap_or_default()
}

fn describe(verdict: &Verdict) -> String {
    match verdict {
        Verdict::BadConsole(hits) => format!("{} known-bad console line(s)", hits.len()),
        Verdict::Vanished { after } => format!(
            "gone in {:.1}s without saying anything",
            after.as_secs_f32()
        ),
        Verdict::Killed { .. } => "killed by a signal rather than exiting".into(),
        Verdict::ExitCode(code) => format!("exited {code}"),
        Verdict::NoComplaint => "nothing".into(),
    }
}

/// Path segments that hold other programs rather than being one.
///
/// Keying on the executable's **first** path component works for
/// `FSUIPC7/FSUIPC7.exe` but not for per-user installs: Orbx Central lands in
/// `users/steamuser/AppData/Local/Programs/orbx-central/`, and everything under
/// `users/` would become one group. Skipping the containers finds the
/// directory that is actually the program.
const CONTAINERS: &[&str] = &[
    "users",
    "steamuser",
    "public",
    "appdata",
    "local",
    "locallow",
    "roaming",
    "programs",
    "program files",
    "program files (x86)",
    "programdata",
    "package cache",
    "temp",
    "windows",
];

/// The directory that is the installed program, for grouping.
///
/// Walks down from the root skipping [`CONTAINERS`] and stops at the first
/// segment that names something. Falls back to the executable's own parent
/// when every segment is a container, which is never worse than the whole path.
fn group_root(exe: &Path) -> PathBuf {
    let mut walked = PathBuf::new();
    let components: Vec<_> = exe.components().collect();
    // The last component is the file itself and is never a group.
    for component in components.iter().take(components.len().saturating_sub(1)) {
        walked.push(component.as_os_str());
        let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        if !CONTAINERS.contains(&name.as_str()) {
            return walked;
        }
    }
    exe.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Group new executables by the program directory they landed in, and pick one
/// per group.
fn companion_apps(added: &BTreeSet<PathBuf>) -> Vec<Landed> {
    let mut groups: std::collections::BTreeMap<PathBuf, Vec<PathBuf>> = Default::default();
    for exe in added {
        groups.entry(group_root(exe)).or_default().push(exe.clone());
    }

    groups
        .into_iter()
        .filter_map(|(root, mut members)| {
            // Sorted so the pick is deterministic whatever order the diff
            // produced, and so the tie-break below is stable.
            members.sort();
            // **Highest score, then shallowest, then first alphabetically.**
            // Not `max_by_key`: it breaks ties by taking the *last* element,
            // which after the sort means alphabetically last.
            let best = members
                .iter()
                .min_by_key(|exe| (-score(exe, &root), depth_within(exe, &root), (*exe).clone()))?
                .clone();
            // Every candidate scored below zero is an uninstaller, a
            // redistributable or a runtime. An install that produced only
            // those has produced no application, and saying so beats naming
            // one.
            if score(&best, &root) < 0 {
                return None;
            }
            let also = members.into_iter().filter(|e| *e != best).collect();
            Some(Landed::CompanionApp { exe: best, also })
        })
        .collect()
}

/// How far below its program directory an executable sits.
fn depth_within(exe: &Path, root: &Path) -> usize {
    exe.strip_prefix(root).unwrap_or(exe).components().count()
}

/// `Orbx Central` and `orbx-central` are the same name.
///
/// An exact string compare would miss the strongest signal in the scoring
/// over a hyphen: `Orbx Central.exe` inside `orbx-central/`. Case, spaces,
/// hyphens, underscores and dots all vary between a product's display name and
/// the directory an installer picks for it, and none of them carries meaning.
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// How likely an executable is to be the thing that was installed.
///
/// Scored against FSUIPC7's real output, which is six executables where
/// exactly one is the application:
///
/// ```text
/// FSUIPC7/FSUIPC7.exe                       ← the app: shallow, named for its directory
/// FSUIPC7/Utils/FSInterrogate2std.exe
/// FSUIPC7/Utils/FSUIPCWebSocketServer.exe
/// FSUIPC7/Utils/HidScanner.exe
/// FSUIPC7/Utils/unzip.exe
/// FSUIPC7/uninstallFSUIPC7.exe              ← an uninstaller
/// ```
///
/// Two rules earn their place. **An uninstaller is rejected, not
/// deprioritised** — naming one as the installed app would put a Stop button
/// next to something that removes the install. And **`unzip` must not read as
/// an uninstaller**: the test is `unins`, not the `un` prefix, or every
/// unpacking helper an installer ships gets thrown away with it.
fn score(exe: &Path, root: &Path) -> i32 {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = stem.to_ascii_lowercase();

    if is_uninstaller(&lower) || is_redistributable(&lower) {
        return -1;
    }

    let mut score = 0;
    let normalised = normalise(&stem);

    // Named for the directory it sits in — the strongest signal there is.
    if let Some(dir) = exe.parent().and_then(|p| p.file_name()) {
        if normalise(&dir.to_string_lossy()) == normalised {
            score += 8;
        }
    }

    // Or named for the program directory itself, which is the same signal one
    // level up: `orbx-central/Orbx Central.exe` has both, `FSUIPC7/FSUIPC7.exe`
    // has both, and an app in its own `bin/` has only this one.
    if let Some(dir) = root.file_name() {
        if normalise(&dir.to_string_lossy()) == normalised {
            score += 8;
        }
    }

    // Shallower is more likely to be the app; `Utils/` and `bin/` are where
    // the helpers go. Measured **within the program directory**, so an app
    // installed six levels down under AppData is not scored as though every
    // one of its executables were a helper.
    score += 4 - (depth_within(exe, root) as i32).min(4);

    score
}

/// Whether a file stem names an uninstaller. Public because a caller that is
/// about to record an app's uninstaller has to use *this* rule and not its own
/// — see the note on the `unins` test inside.
pub fn is_uninstaller(stem: &str) -> bool {
    // `unins`, not `uninst`: Inno names its uninstaller `unins000.exe`, which
    // has no `t` in it. `unins` still covers `uninstall` and `uninstaller`,
    // and `unzip` contains neither: a prefix test on `un` would throw away
    // every unpacking helper an installer ships.
    stem.contains("unins") || stem == "un"
}

/// A prerequisite rather than the thing that was installed.
///
/// The prefix tests catch an installer that was *fetched*; the two exact names
/// catch a runtime that was **unpacked**: Orbx Central's install leaves
/// `Program Files/dotnet/dotnet.exe`, which sits in a directory called
/// `dotnet` — the strongest signal in the scoring — and would otherwise be
/// reported as a companion app with a Stop button.
///
/// Exact matches, not prefixes: `dotnetfx` needs the prefix test and an
/// application legitimately named `dotnet-something` must not be thrown away
/// with it.
fn is_redistributable(stem: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "vcredist",
        "vc_redist",
        "dotnetfx",
        "windowsdesktop-runtime",
    ];
    const EXACT: &[&str] = &["dotnet", "createdump"];
    PREFIXES.iter().any(|k| stem.starts_with(k)) || EXACT.contains(&stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::ConsoleHit;
    use crate::snapshot::{Addon, Package};
    use crate::snapshot::{Seen, Snapshot};
    use std::time::Duration;

    /// The exact set FSUIPC7 put in the companion prefix.
    fn fsuipc7_exes() -> BTreeSet<PathBuf> {
        [
            "FSUIPC7/FSUIPC7.exe",
            "FSUIPC7/Utils/FSInterrogate2std.exe",
            "FSUIPC7/Utils/FSUIPCWebSocketServer.exe",
            "FSUIPC7/Utils/HidScanner.exe",
            "FSUIPC7/Utils/unzip.exe",
            "FSUIPC7/uninstallFSUIPC7.exe",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }

    fn snap(
        community: BTreeSet<Package>,
        executables: BTreeSet<PathBuf>,
        addons: BTreeSet<Addon>,
    ) -> Snapshot {
        Snapshot {
            community: Seen::Read(community),
            executables: Seen::Read(executables),
            addons: Seen::Read(addons),
            notes: Vec::new(),
        }
    }

    fn empty() -> Snapshot {
        snap(BTreeSet::new(), BTreeSet::new(), BTreeSet::new())
    }

    fn diff_of(after: Snapshot) -> Diff {
        Diff::between(&empty(), &after)
    }

    #[test]
    fn the_app_is_picked_out_of_its_own_helpers_and_uninstaller() {
        let report = classify(
            &diff_of(snap(BTreeSet::new(), fsuipc7_exes(), BTreeSet::new())),
            &Verdict::NoComplaint,
            Produces::CompanionApp,
            Path::new("/nonexistent"),
        );

        let apps: Vec<_> = report
            .landed
            .iter()
            .filter(|l| matches!(l, Landed::CompanionApp { .. }))
            .collect();
        assert_eq!(apps.len(), 1, "one directory, one app: {apps:?}");

        let Landed::CompanionApp { exe, also } = apps[0] else {
            unreachable!()
        };
        assert_eq!(exe, Path::new("FSUIPC7/FSUIPC7.exe"));
        assert_eq!(also.len(), 5, "and it keeps what it chose between");
    }

    /// The ten executables Orbx Central's install actually left behind.
    fn orbx_exes() -> BTreeSet<PathBuf> {
        [
            "Program Files/dotnet/dotnet.exe",
            "Program Files/dotnet/shared/Microsoft.NETCore.App/10.0.11/createdump.exe",
            "ProgramData/Package Cache/{96749152}/windowsdesktop-runtime-10.0.11-win-x64.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/Orbx Central.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/Uninstall Orbx Central.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/resources/elevate.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/saturn-win/FontReg.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/saturn-win/OrbxCentralWorker.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/saturn-win/createdump.exe",
            "users/steamuser/AppData/Local/Programs/orbx-central/saturn-win/texconv.exe",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }

    /// One app, correctly named, and the .NET runtime not reported as one.
    ///
    /// Covers three traps at once:
    ///
    /// * grouping on the **first** path component lumps everything under
    ///   `users/` into one group;
    /// * an exact directory-name compare misses `Orbx Central.exe` in
    ///   `orbx-central/` over a hyphen;
    /// * `max_by_key` breaks ties to the *last* maximum — after the sort,
    ///   alphabetically last, which is `texconv`.
    #[test]
    fn orbx_central_is_named_rather_than_its_texture_tool() {
        let report = classify(
            &diff_of(snap(BTreeSet::new(), orbx_exes(), BTreeSet::new())),
            &Verdict::NoComplaint,
            Produces::CompanionApp,
            Path::new("/nonexistent"),
        );

        let apps: Vec<_> = report
            .landed
            .iter()
            .filter_map(|l| match l {
                Landed::CompanionApp { exe, also } => Some((exe, also)),
                _ => None,
            })
            .collect();

        assert_eq!(
            apps.len(),
            1,
            "the .NET runtime and the package cache are not applications: {apps:?}"
        );
        let (exe, also) = apps[0];
        assert_eq!(
            exe,
            Path::new("users/steamuser/AppData/Local/Programs/orbx-central/Orbx Central.exe")
        );
        assert_eq!(also.len(), 6, "and it keeps what it chose between");
        assert_eq!(report.landed[0].name(), "Orbx Central");
    }

    /// The grouping rule on its own, because it is the part that generalises:
    /// a program installed under `AppData` is found at its own directory, and
    /// one installed at the root of `drive_c` still is.
    #[test]
    fn a_program_directory_is_found_under_any_amount_of_windows_scaffolding() {
        assert_eq!(
            group_root(Path::new(
                "users/steamuser/AppData/Local/Programs/orbx-central/saturn-win/texconv.exe"
            )),
            Path::new("users/steamuser/AppData/Local/Programs/orbx-central")
        );
        assert_eq!(
            group_root(Path::new("FSUIPC7/Utils/HidScanner.exe")),
            Path::new("FSUIPC7")
        );
        assert_eq!(
            group_root(Path::new("Program Files/dotnet/dotnet.exe")),
            Path::new("Program Files/dotnet")
        );
    }

    /// A display name and the directory an installer picks for it differ by
    /// case and separators, and none of that carries meaning.
    #[test]
    fn a_display_name_and_its_directory_are_the_same_name() {
        assert_eq!(normalise("Orbx Central"), normalise("orbx-central"));
        assert_eq!(normalise("FSUIPC7"), normalise("fsuipc7"));
        assert_eq!(normalise("Pilot_Client"), normalise("pilot client"));
        assert_ne!(normalise("Orbx Central"), normalise("orbx centre"));
    }

    /// An unpacked runtime is a prerequisite, not the thing installed. Exact
    /// names, so an application called `dotnet-tool-something` survives.
    #[test]
    fn an_unpacked_runtime_is_not_an_application() {
        assert!(is_redistributable("dotnet"));
        assert!(is_redistributable("createdump"));
        assert!(is_redistributable("windowsdesktop-runtime-10.0.11-win-x64"));
        assert!(!is_redistributable("dotnet-counters"));
        assert!(!is_redistributable("orbx central"));
    }

    /// `unzip.exe` starts with `un` and is not an uninstaller. A prefix test
    /// would throw away every unpacking helper an installer ships.
    #[test]
    fn unzip_is_not_mistaken_for_an_uninstaller() {
        assert!(!is_uninstaller("unzip"));
        assert!(is_uninstaller("uninstallfsuipc7"));
        assert!(is_uninstaller("unins000"));
        assert!(is_uninstaller("un"));
    }

    /// Naming an uninstaller as the installed app would put a Stop button
    /// next to the thing that removes the install.
    #[test]
    fn an_install_of_only_uninstallers_produces_no_app() {
        let only = ["Thing/unins000.exe", "Thing/uninstall.exe"]
            .into_iter()
            .map(PathBuf::from)
            .collect();

        let report = classify(
            &diff_of(snap(BTreeSet::new(), only, BTreeSet::new())),
            &Verdict::NoComplaint,
            Produces::CompanionApp,
            Path::new("/nonexistent"),
        );
        assert!(report
            .landed
            .iter()
            .all(|l| !matches!(l, Landed::CompanionApp { .. })));
        assert!(report
            .concerns
            .iter()
            .any(|c| matches!(c, Concern::NothingArrived { .. })));
    }

    #[test]
    fn two_directories_are_two_apps() {
        let two = ["AppOne/AppOne.exe", "AppTwo/AppTwo.exe"]
            .into_iter()
            .map(PathBuf::from)
            .collect();

        let report = classify(
            &diff_of(snap(BTreeSet::new(), two, BTreeSet::new())),
            &Verdict::NoComplaint,
            Produces::CompanionApp,
            Path::new("/nonexistent"),
        );
        assert_eq!(report.landed.len(), 2);
    }

    #[test]
    fn a_community_directory_is_a_package_only_with_a_manifest() {
        let with = [Package {
            name: "bksq-aircraft-commander114".into(),
            has_manifest: true,
        }]
        .into_iter()
        .collect();

        let report = classify(
            &diff_of(snap(with, BTreeSet::new(), BTreeSet::new())),
            &Verdict::NoComplaint,
            Produces::Package,
            Path::new("/nonexistent"),
        );
        assert_eq!(
            report.landed,
            vec![Landed::Package {
                name: "bksq-aircraft-commander114".into()
            }]
        );
        assert_eq!(report.landed[0].kind(), ItemKind::Package);
        assert!(report.is_clean());

        let without = [Package {
            name: "half-unpacked".into(),
            has_manifest: false,
        }]
        .into_iter()
        .collect();

        let report = classify(
            &diff_of(snap(without, BTreeSet::new(), BTreeSet::new())),
            &Verdict::NoComplaint,
            Produces::Package,
            Path::new("/nonexistent"),
        );
        assert!(report.landed.is_empty(), "a directory is not a package");
        assert!(!report.concerns.is_empty());
    }

    /// What FSUIPC7 did: registered `C:\FSUIPC7\FSUIPC7.exe` in the sim's
    /// EXE.xml while installing into the companion prefix's drive_c.
    #[test]
    fn an_addon_the_sim_cannot_follow_is_a_concern_as_well_as_an_item() {
        let addons = [Addon {
            name: "FSUIPC7".into(),
            path: "C:\\FSUIPC7\\FSUIPC7.exe".into(),
        }]
        .into_iter()
        .collect();

        let report = classify(
            &diff_of(snap(BTreeSet::new(), BTreeSet::new(), addons)),
            &Verdict::NoComplaint,
            Produces::SimAddon,
            Path::new("/nonexistent-prefix"),
        );

        assert_eq!(report.landed.len(), 1, "it did register, and that is real");
        assert_eq!(report.landed[0].kind(), ItemKind::SimAddonExe);
        assert!(matches!(
            report.landed[0],
            Landed::SimAddon { resolved: None, .. }
        ));
        assert!(
            report
                .concerns
                .iter()
                .any(|c| matches!(c, Concern::AddonPathUnreachable { .. })),
            "the sim will try to launch something that is not there"
        );
        assert!(!report.is_clean());
    }

    #[test]
    fn nothing_arriving_is_a_concern_unless_a_runtime_was_expected() {
        let report = classify(
            &diff_of(empty()),
            &Verdict::NoComplaint,
            Produces::Package,
            Path::new("/nonexistent"),
        );
        assert!(report
            .concerns
            .iter()
            .any(|c| matches!(c, Concern::NothingArrived { .. })));

        // A runtime installs into places none of the three watch.
        let report = classify(
            &diff_of(empty()),
            &Verdict::NoComplaint,
            Produces::Runtime,
            Path::new("/nonexistent"),
        );
        assert!(report.concerns.is_empty(), "{:?}", report.concerns);
    }

    #[test]
    fn a_bad_console_line_is_carried_through_verbatim() {
        let report = classify(
            &diff_of(empty()),
            &Verdict::BadConsole(vec![ConsoleHit {
                signature: "valid purchase".into(),
                line: "Setup could not detect a valid purchase.".into(),
            }]),
            Produces::Package,
            Path::new("/nonexistent"),
        );
        assert!(report.concerns.iter().any(|c| matches!(
            c,
            Concern::RunObjected { detail } if detail.contains("Setup could not detect")
        )));
    }

    #[test]
    fn a_vanishing_is_reported_even_when_something_landed() {
        let report = classify(
            &diff_of(snap(BTreeSet::new(), fsuipc7_exes(), BTreeSet::new())),
            &Verdict::Vanished {
                after: Duration::from_millis(400),
            },
            Produces::CompanionApp,
            Path::new("/nonexistent"),
        );
        assert!(!report.landed.is_empty());
        assert!(report
            .concerns
            .iter()
            .any(|c| matches!(c, Concern::RunObjected { .. })));
    }

    /// A place nobody could read must not read as "nothing landed there".
    #[test]
    fn an_unobserved_place_becomes_a_concern() {
        let mut before = empty();
        before.community = Seen::Unreadable("permission denied".into());
        let diff = Diff::between(&before, &empty());

        let report = classify(
            &diff,
            &Verdict::NoComplaint,
            Produces::Package,
            Path::new("/nonexistent"),
        );
        assert!(report.concerns.iter().any(|c| matches!(
            c,
            Concern::Unobserved { why, .. } if why.contains("permission denied")
        )));
    }

    #[test]
    fn something_disappearing_is_reported_rather_than_assumed_to_be_an_upgrade() {
        let before = snap(
            [Package {
                name: "old-thing".into(),
                has_manifest: true,
            }]
            .into_iter()
            .collect(),
            BTreeSet::new(),
            BTreeSet::new(),
        );
        let report = classify(
            &Diff::between(&before, &empty()),
            &Verdict::NoComplaint,
            Produces::Package,
            Path::new("/nonexistent"),
        );
        assert!(report.concerns.iter().any(|c| matches!(
            c,
            Concern::Removed { what, .. } if what.iter().any(|n| n == "old-thing")
        )));
    }
}
