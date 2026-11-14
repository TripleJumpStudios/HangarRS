//! Stage 2 — the vendor table: what this installer needs, from experience.
//!
//! Stage 1 says what a file *is*. This says how its family has to be run, and
//! every entry in it was paid for by an install that went wrong first. The
//! table is TOML, shipped compiled-in, and a user file at
//! `$XDG_CONFIG_HOME/hangar-rs/vendors.toml` adds to it or overrides an entry
//! — the same file, the same parser, so a broken shipped table fails a test
//! rather than a person's install.
//!
//! # Why a table rather than rules
//!
//! Because the requirements contradict each other. Just Flight's installer
//! works with the working directory it inherits and fails from the installer's
//! own directory; the Garmin Trainer launcher requires its own directory and
//! fails without it. Both are reproducible, and no single policy satisfies
//! them — so the policy is per vendor and the code carries
//! none of its own.
//!
//! # What is deliberately not automated
//!
//! `bad_console` lists the strings that mean a failure even when the process
//! exits 0, and that is all it does: it never answers a dialog. InstallShield's
//! MAX_PATH prompt offers Abort, Retry and Ignore, and **Abort rolls back the
//! whole install** — four gigabytes of it. The right answer is Ignore followed
//! by an out-of-band repair, and nothing here presses it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::engine::Engine;
use crate::inspect::Inspection;

/// The table HangarRS ships. Parsed by the same code as the user's file.
pub const SHIPPED: &str = include_str!("vendors.toml");

/// Which prefix an installer belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Prefix {
    /// The MSFS prefix. Anything that probes for `UserCfg.opt` or writes into
    /// `Community` has to run here or it will not find the sim.
    Msfs,
    /// The companion prefix, where BeyondATC, the PilotEdge client and Project
    /// Airports already live.
    Companion,
}

impl Prefix {
    pub fn label(self) -> &'static str {
        match self {
            Prefix::Msfs => "MSFS",
            Prefix::Companion => "companion",
        }
    }
}

/// Which Proton verb to spawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verb {
    /// `proton run` — a full Steam session wrapper.
    Run,
    /// `proton runinprefix` — no session wrapper.
    ///
    /// Correct in principle for a self-extracting stub, because the wrapper
    /// tears the prefix down when the stub exits and kills the engine it just
    /// spawned. Not universally required in practice: both Just Flight updates
    /// succeeded through `run`.
    ///
    /// Spelled the way Proton spells it. A kebab-case rename would have made
    /// this `run-in-prefix`, so a table entry naming the real verb would have
    /// been dropped as unparseable.
    #[serde(rename = "runinprefix", alias = "run-in-prefix")]
    RunInPrefix,
}

impl Verb {
    /// The word Proton is actually invoked with.
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Run => "run",
            Verb::RunInPrefix => "runinprefix",
        }
    }
}

/// What working directory the installer is spawned with.
///
/// The two ends of this are both reproducible and both non-negotiable, which
/// is why it is a per-vendor field rather than a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cwd {
    /// Inherit HangarRS's own working directory.
    ///
    /// The only condition under which Just Flight's purchase check has been
    /// seen to pass. Why is **still unidentified** — protontricks from the
    /// installer's own directory failed the check — so this reproduces the
    /// launch conditions rather than reasoning about them.
    Inherit,
    /// The directory the installer sits in.
    InstallerDir,
    /// The application's own directory, once it is installed.
    ///
    /// The Garmin Trainer launcher resolves `Interop.IWshRuntimeLibrary.dll`
    /// relative to the working directory, and gets it wrong at *power-on*
    /// rather than at startup — so a wrong CWD here looks like a broken
    /// avionics stack rather than a launch problem.
    AppDir,
}

impl Cwd {
    pub fn label(self) -> &'static str {
        match self {
            Cwd::Inherit => "inherited",
            Cwd::InstallerDir => "the installer's own directory",
            Cwd::AppDir => "the application's own directory",
        }
    }
}

/// What a successful install is expected to leave behind.
///
/// Stage 5 classifies from the diffs regardless; this says what to expect, so
/// "nothing landed" can be reported against something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Produces {
    /// A new directory in `Community` with a `manifest.json`.
    Package,
    /// A new `<Launch.Addon>` in `EXE.xml`. Never an app-runner entry: MSFS
    /// spawns these itself, and launching one separately starts a second copy.
    SimAddon,
    /// A Windows application with a lifecycle in a prefix.
    CompanionApp,
    /// A prerequisite rather than a library item — the .NET Desktop Runtime is
    /// installed *for* something else and never appears in the Hangar.
    Runtime,
    /// Not known in advance.
    Unknown,
}

impl Produces {
    pub fn label(self) -> &'static str {
        match self {
            Produces::Package => "a Community package",
            Produces::SimAddon => "an EXE.xml add-on",
            Produces::CompanionApp => "a companion app",
            Produces::Runtime => "a runtime (not a library item)",
            Produces::Unknown => "not known in advance",
        }
    }
}

/// A case-insensitive pattern with `*` wildcards.
///
/// Not a regex. The names being matched are vendor and product strings from a
/// version resource, the patterns are written by hand, and a regex dependency
/// buys nothing here but a new way for a user's table to fail to compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    raw: String,
    parts: Vec<String>,
    leading: bool,
    trailing: bool,
}

impl Pattern {
    pub fn new(raw: &str) -> Pattern {
        let lowered = raw.to_ascii_lowercase();
        let parts: Vec<String> = lowered
            .split('*')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        Pattern {
            leading: lowered.starts_with('*'),
            trailing: lowered.ends_with('*'),
            parts,
            raw: raw.to_string(),
        }
    }

    /// Does this pattern match anything at all?
    pub fn is_any(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// How specific this pattern is, for choosing between two that both match.
    /// A literal beats a wildcard because it says more.
    pub fn specificity(&self) -> usize {
        self.parts.iter().map(String::len).sum()
    }

    pub fn matches(&self, text: &str) -> bool {
        if self.is_any() {
            return true;
        }
        let hay = text.to_ascii_lowercase();
        let mut at = 0usize;
        for (i, part) in self.parts.iter().enumerate() {
            let first = i == 0;
            let Some(found) = hay[at..].find(part.as_str()) else {
                return false;
            };
            // A pattern with no leading `*` is anchored at the start.
            if first && !self.leading && found != 0 {
                return false;
            }
            at += found + part.len();
        }
        // …and one with no trailing `*` is anchored at the end.
        self.trailing || at == hay.len()
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Pattern, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Pattern::new(&raw))
    }
}

impl<'de> Deserialize<'de> for Engine {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Engine, D::Error> {
        let raw = String::deserialize(d)?;
        match raw
            .to_ascii_lowercase()
            .replace(['-', '_', ' '], "")
            .as_str()
        {
            "installshield" => Ok(Engine::InstallShield),
            "wixburn" | "burn" | "wix" => Ok(Engine::WixBurn),
            "nsis" => Ok(Engine::Nsis),
            "innosetup" | "inno" => Ok(Engine::InnoSetup),
            "msi" => Ok(Engine::Msi),
            "squirrel" => Ok(Engine::Squirrel),
            other => Err(serde::de::Error::custom(format!(
                "unknown engine {other:?}"
            ))),
        }
    }
}

/// What stage 4 does about Wine's PowerShell stub before it starts.
///
/// The default is [`PowerShell::Park`], and unusually for this table that is a
/// policy rather than a per-vendor observation. It is a policy because the
/// thing it works around is not a vendor's behaviour: Wine's `powershell.exe`
/// exits 0 for every command it is given, so *any* installer that asks it a
/// question gets an answer that is wrong whenever the truth is "no". There is
/// no title for which the stub's answer is better than no answer.
///
/// [`PowerShell::Keep`] exists for the case this reasoning misses: an
/// installer that checks `powershell.exe` is *present* and refuses outright
/// when it is not. None has been seen; the escape hatch costs one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PowerShell {
    /// Move the stub aside for the run and put it back afterwards.
    #[default]
    #[serde(alias = "neutralise", alias = "neutralize", alias = "disable")]
    Park,
    /// Leave it where it is, and accept that anything it answers is wrong.
    Keep,
}

impl PowerShell {
    pub fn label(self) -> &'static str {
        match self {
            PowerShell::Park => "parked for the run",
            PowerShell::Keep => "left in place",
        }
    }
}

/// Where a profile came from, which is also how ties are broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Shipped,
    User,
}

/// Whether the installer contains its payload or downloads it later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Payload {
    #[default]
    Bundled,
    Downloads,
}

/// One vendor's rules.
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    /// What to call this in a log or a UI.
    pub name: String,
    /// The engine this applies to. Absent means any engine.
    #[serde(default)]
    pub engine: Option<Engine>,
    /// Matched against the vendor from stage 1's [`Inspection::identity`].
    #[serde(default = "any")]
    pub company: Pattern,
    /// Matched against the product from stage 1's identity.
    #[serde(default = "any")]
    pub product: Pattern,
    pub prefix: Prefix,
    pub verb: Verb,
    pub cwd: Cwd,
    #[serde(default = "unknown_outcome")]
    pub produces: Produces,
    #[serde(default)]
    pub payload: Payload,
    /// Requirement ids stage 3 knows how to check. An id it does not know is
    /// reported as unchecked rather than silently passed — a user's table is
    /// allowed to name a requirement HangarRS cannot verify yet.
    #[serde(default)]
    pub runtimes: Vec<String>,
    /// Switches to pass the installer itself, in order, after its path.
    ///
    /// **Empty in the shipped table, deliberately.** Every engine documents a
    /// silent-install vocabulary — WiX Burn takes `/quiet`, `/passive`,
    /// `/norestart` and `/repair`; NSIS takes `/S`; Inno takes `/SILENT`;
    /// InstallShield takes `/s` with a response file — but not one of them has
    /// been observed working here, and a switch that is wrong for a particular
    /// title is indistinguishable from an installer that simply failed.
    ///
    /// Two reasons the default stays empty beyond that:
    ///
    /// * A silent run hides the dialogs, and one of the dialogs is
    ///   InstallShield's MAX_PATH prompt. Suppressing it does not make the
    ///   long-path problem go away — it makes it happen where nobody can see
    ///   it, and Abort is what a suppressed prompt may well pick.
    /// * The first run of anything through stage 4 wants watching, not
    ///   automating.
    ///
    /// Set them per title in `$XDG_CONFIG_HOME/hangar-rs/vendors.toml`, which
    /// needs no rebuild.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment the installer needs. `DOTNET_SYSTEM_GLOBALIZATION_USENLS=1`
    /// is the load-bearing one: without it any .NET 6 x86 component dies
    /// before a window appears, because Wine's i386 `icu.dll` forwards to
    /// `icuuc68` and Fedora ships ICU 77.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Console strings that mean failure whatever the exit code says.
    #[serde(default)]
    pub bad_console: Vec<String>,
    /// How the *installed* application has to be launched, when that differs
    /// from how its installer does.
    ///
    /// The Garmin Trainer launcher is the case: its installer is indifferent
    /// to the working directory and the launcher it installs is not.
    #[serde(default)]
    pub app_cwd: Option<Cwd>,
    /// What to do about Wine's PowerShell stub for the duration of the run.
    ///
    /// Defaults to [`PowerShell::Park`] for every entry, including entries
    /// written before this field existed, because a stub that exits 0 for
    /// every query is never useful — see [`crate::shim`]. Set `keep` for a
    /// title that turns out to need `powershell.exe` to exist.
    #[serde(default)]
    pub powershell: PowerShell,
    /// Anything a person would need to know before running it by hand.
    #[serde(default)]
    pub notes: Option<String>,
    /// Filled in by the loader, not by the file.
    #[serde(skip, default = "shipped_origin")]
    pub origin: Origin,
}

fn any() -> Pattern {
    Pattern::new("*")
}

fn unknown_outcome() -> Produces {
    Produces::Unknown
}

fn shipped_origin() -> Origin {
    Origin::Shipped
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    vendor: Vec<Profile>,
}

/// The shipped table plus whatever the user added.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub profiles: Vec<Profile>,
    /// Problems with the user's file, reported rather than thrown.
    pub warnings: Vec<String>,
}

impl Table {
    /// The compiled-in table only.
    ///
    /// Panics if the shipped table does not parse, which is a build-time
    /// mistake and is covered by a test.
    pub fn shipped() -> Table {
        let file: File = toml::from_str(SHIPPED).expect("the shipped vendor table must parse");
        Table {
            profiles: file.vendor,
            warnings: Vec::new(),
        }
    }

    /// The shipped table, plus the user's file if it is there and readable.
    ///
    /// A malformed user file is a warning and not a failure: losing the whole
    /// table because one hand-edited entry has a typo would take away every
    /// rule that was fine.
    pub fn load() -> Table {
        let mut table = Table::shipped();
        let Some(path) = hangar_core::paths::vendors_file() else {
            return table;
        };
        table.merge_user(&path);
        table
    }

    /// Read one user file. Public so a test can point it somewhere harmless.
    pub fn merge_user(&mut self, path: &Path) {
        if !path.exists() {
            return;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                self.warnings.push(format!("{}: {e}", path.display()));
                return;
            }
        };
        match toml::from_str::<File>(&text) {
            Ok(file) => {
                for mut profile in file.vendor {
                    profile.origin = Origin::User;
                    self.profiles.push(profile);
                }
            }
            Err(e) => self.warnings.push(format!("{}: {e}", path.display())),
        }
    }

    /// The best profile for an inspected file, with why it was chosen.
    pub fn lookup(&self, report: &Inspection) -> Option<Match<'_>> {
        let vendor = report.identity.vendor.as_deref().unwrap_or_default();
        let product = report.identity.product.as_deref().unwrap_or_default();
        self.match_on(report.engine, vendor, product)
    }

    /// The lookup without an [`Inspection`], for tests and for a file that has
    /// already been identified by hand.
    pub fn match_on(&self, engine: Engine, vendor: &str, product: &str) -> Option<Match<'_>> {
        let mut best: Option<(usize, &Profile)> = None;
        for profile in &self.profiles {
            let Some(score) = profile.score(engine, vendor, product) else {
                continue;
            };
            let better = match best {
                None => true,
                // A user entry wins a tie, because it was written to override.
                Some((current, chosen)) => {
                    score > current
                        || (score == current
                            && profile.origin == Origin::User
                            && chosen.origin == Origin::Shipped)
                }
            };
            if better {
                best = Some((score, profile));
            }
        }
        best.map(|(score, profile)| Match {
            profile,
            score,
            why: profile.why(engine, vendor, product),
        })
    }
}

impl Profile {
    /// A downloader's file size says nothing about the installed payload.
    pub fn payload_estimate(&self, inspection: &Inspection) -> Option<u64> {
        match self.payload {
            Payload::Bundled => Some(inspection.payload_estimate()),
            Payload::Downloads => None,
        }
    }

    /// How well this profile fits, or `None` if it does not.
    ///
    /// An engine named in the profile must match exactly. Everything else is a
    /// pattern, and a literal scores above a wildcard so the specific entry
    /// beats the family default.
    fn score(&self, engine: Engine, vendor: &str, product: &str) -> Option<usize> {
        if let Some(wanted) = self.engine {
            if wanted != engine {
                return None;
            }
        }
        if !self.company.matches(vendor) || !self.product.matches(product) {
            return None;
        }
        let mut score = self.engine.is_some() as usize * 4;
        score += self.company.specificity() + self.product.specificity();
        Some(score)
    }

    fn why(&self, engine: Engine, vendor: &str, product: &str) -> Vec<String> {
        let mut why = Vec::new();
        if self.engine.is_some() {
            why.push(format!("engine {}", engine.label()));
        }
        if !self.company.is_any() {
            why.push(format!("company {:?} ~ {}", vendor, self.company.as_str()));
        }
        if !self.product.is_any() {
            why.push(format!("product {:?} ~ {}", product, self.product.as_str()));
        }
        if why.is_empty() {
            why.push("family default".to_string());
        }
        why
    }
}

/// A chosen profile and the reasons it was chosen.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub profile: &'a Profile,
    pub score: usize,
    pub why: Vec<String>,
}

impl Match<'_> {
    /// Is this the family default rather than a profile for this product?
    pub fn is_generic(&self) -> bool {
        self.profile.company.is_any() && self.profile.product.is_any()
    }
}

/// Where the user's own table goes.
pub fn user_table() -> Option<PathBuf> {
    hangar_core::paths::vendors_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_policy_is_explicit_and_existing_profiles_keep_their_default() {
        let table = table();
        let orbx = table
            .match_on(Engine::Nsis, "Orbx", "Orbx Central")
            .unwrap();
        assert_eq!(orbx.profile.payload, Payload::Downloads);
        let other = table
            .match_on(Engine::Nsis, "Another vendor", "Another app")
            .unwrap();
        assert_eq!(other.profile.payload, Payload::Bundled);
    }

    fn table() -> Table {
        Table::shipped()
    }

    /// If the shipped table stops parsing, that is a build mistake and this is
    /// where it should be found — not in someone's install.
    #[test]
    fn the_shipped_table_parses() {
        let table = table();
        assert!(
            table.profiles.len() >= 10,
            "seeded from the investigation docs"
        );
        assert!(table.warnings.is_empty());
        assert!(table.profiles.iter().all(|p| p.origin == Origin::Shipped));
    }

    #[test]
    fn just_flight_gets_its_own_entry_rather_than_the_family_default() {
        let table = table();
        let found = table
            .match_on(
                Engine::InstallShield,
                "Just Flight (MSFS 2024)",
                "Black Square - Commander 114 MSFS2024",
            )
            .expect("a profile");
        assert_eq!(found.profile.name, "Just Flight / Black Square");
        assert!(!found.is_generic());
        assert_eq!(found.profile.cwd, Cwd::Inherit);
        assert_eq!(found.profile.produces, Produces::Package);
        assert!(found
            .profile
            .bad_console
            .iter()
            .any(|s| s == "valid purchase"));
    }

    /// An InstallShield installer from someone else still gets the family's
    /// rules — the MAX_PATH behaviour is a property of the engine.
    #[test]
    fn an_unknown_vendor_falls_back_to_the_family() {
        let table = table();
        let found = table
            .match_on(Engine::InstallShield, "Carenado", "Some Aeroplane")
            .expect("a profile");
        assert_eq!(found.profile.name, "InstallShield (any)");
        assert!(found.is_generic());
    }

    #[test]
    fn garmin_carries_the_environment_without_which_it_dies_silently() {
        let table = table();
        let found = table
            .match_on(Engine::WixBurn, "Garmin", "Garmin Aviation Trainers")
            .expect("a profile");
        assert_eq!(
            found.profile.env.get("DOTNET_SYSTEM_GLOBALIZATION_USENLS"),
            Some(&"1".to_string())
        );
        assert!(found
            .profile
            .runtimes
            .iter()
            .any(|r| r == "dotnet-desktop-6-x86"));
        assert_eq!(found.profile.app_cwd, Some(Cwd::AppDir));
    }

    /// A runtime is installed for something else and never becomes a library
    /// item, so it has to be distinguishable before it runs.
    #[test]
    fn a_runtime_is_not_a_library_item() {
        let table = table();
        let found = table
            .match_on(
                Engine::WixBurn,
                "Microsoft Corporation",
                "Microsoft Windows Desktop Runtime - 8.0.29 (x64)",
            )
            .expect("a profile");
        assert_eq!(found.profile.produces, Produces::Runtime);
    }

    /// TDS GPS Manager is Themida-packed, so stage 1 cannot name its engine.
    /// An entry that insisted on one would never match it.
    #[test]
    fn a_packed_installer_is_matched_on_its_vendor_alone() {
        let table = table();
        let found = table
            .match_on(Engine::Unknown, "TDS", "TDSGPSManager")
            .expect("a profile");
        assert_eq!(found.profile.name, "TDS GPS Manager");
        assert_eq!(found.profile.produces, Produces::SimAddon);
    }

    #[test]
    fn a_file_nothing_knows_about_has_no_profile() {
        let table = table();
        assert!(table
            .match_on(Engine::Unknown, "Some Vendor", "Some Product")
            .is_none());
    }

    #[test]
    fn project_airports_installs_into_the_companion_prefix() {
        let table = table();
        let found = table
            .match_on(Engine::Nsis, "Project Airports", "Project Airports")
            .expect("a profile");
        assert_eq!(found.profile.name, "Project Airports");
        assert_eq!(found.profile.prefix, Prefix::Companion);
    }

    #[test]
    fn patterns_are_anchored_unless_they_say_otherwise() {
        assert!(Pattern::new("just flight*").matches("Just Flight (MSFS 2024)"));
        assert!(!Pattern::new("flight*").matches("Just Flight (MSFS 2024)"));
        assert!(Pattern::new("*flight*").matches("Just Flight (MSFS 2024)"));
        assert!(Pattern::new("garmin").matches("Garmin"));
        assert!(!Pattern::new("garmin").matches("Garmin Ltd"));
        assert!(Pattern::new("*").matches(""));
        assert!(Pattern::new("*desktop runtime*")
            .matches("Microsoft Windows Desktop Runtime - 8.0.29 (x64)"));
    }

    #[test]
    fn a_literal_outscores_a_wildcard() {
        assert!(Pattern::new("just flight*").specificity() > Pattern::new("*").specificity());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hangar-vendor-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("vendors.toml")
    }

    /// The point of the user file: override a shipped entry without editing
    /// the binary.
    #[test]
    fn a_user_entry_wins_a_tie() {
        let path = scratch("override");
        std::fs::write(
            &path,
            r#"
[[vendor]]
name = "Just Flight, but mine"
engine = "installshield"
company = "just flight*"
prefix = "msfs"
verb = "runinprefix"
cwd = "installer-dir"
produces = "package"
"#,
        )
        .unwrap();

        let mut table = Table::shipped();
        table.merge_user(&path);
        let found = table
            .match_on(Engine::InstallShield, "Just Flight (MSFS 2024)", "Anything")
            .expect("a profile");
        assert_eq!(found.profile.name, "Just Flight, but mine");
        assert_eq!(found.profile.verb, Verb::RunInPrefix);
        assert_eq!(found.profile.origin, Origin::User);
        let _ = std::fs::remove_file(&path);
    }

    /// One typo in a hand-edited file must not take the other rules with it.
    #[test]
    fn a_malformed_user_file_is_a_warning_not_a_failure() {
        let path = scratch("broken");
        std::fs::write(&path, "[[vendor]]\nname = \"half an entry\"\n").unwrap();

        let mut table = Table::shipped();
        let shipped = table.profiles.len();
        table.merge_user(&path);

        assert_eq!(table.profiles.len(), shipped, "the shipped table survives");
        assert_eq!(table.warnings.len(), 1);
        assert!(table.warnings[0].contains("vendors.toml"));
        let _ = std::fs::remove_file(&path);
    }

    /// Proton's verb is `runinprefix`, one word. A table entry naming it must
    /// parse, or the only way to ask for it is a spelling Proton does not use.
    #[test]
    fn the_proton_verb_is_spelled_the_way_proton_spells_it() {
        let file: File = toml::from_str(
            "[[vendor]]\nname = \"x\"\nprefix = \"msfs\"\nverb = \"runinprefix\"\ncwd = \"inherit\"\n",
        )
        .expect("runinprefix parses");
        assert_eq!(file.vendor[0].verb, Verb::RunInPrefix);
    }

    #[test]
    fn a_missing_user_file_is_the_normal_case() {
        let mut table = Table::shipped();
        table.merge_user(Path::new("/nonexistent/vendors.toml"));
        assert!(table.warnings.is_empty());
    }

    #[test]
    fn an_unknown_engine_name_in_a_user_file_is_reported() {
        let path = scratch("engine");
        std::fs::write(
            &path,
            "[[vendor]]\nname = \"x\"\nengine = \"instalshield\"\nprefix = \"msfs\"\nverb = \"run\"\ncwd = \"inherit\"\n",
        )
        .unwrap();
        let mut table = Table::shipped();
        table.merge_user(&path);
        assert_eq!(table.warnings.len(), 1);
        assert!(table.warnings[0].contains("unknown engine"));
        let _ = std::fs::remove_file(&path);
    }
}
