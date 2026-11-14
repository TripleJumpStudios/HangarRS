//! Stage 4's other half — planning the invocation, and running it while
//! watching what it says.
//!
//! # Nothing here decides that an install worked
//!
//! **The exit code is never success.** Four of the five known failure modes
//! return 0, so the most this module ever concludes is [`Verdict::NoComplaint`]
//! — nothing in the run itself said it failed. What actually arrived is
//! [`crate::snapshot::Diff`]'s answer, and what it means is stage 5's.
//!
//! # Nothing here answers a dialog
//!
//! InstallShield's MAX_PATH prompt offers Abort, Retry and Ignore, and **Abort
//! rolls back the whole four-gigabyte install**. The right answer is Ignore
//! followed by an out-of-band repair, and it is a person's to give. This
//! module reads the console and never writes to it.
//!
//! # The working directory is a parameter, not a policy
//!
//! Just Flight's installer passes its purchase check only with the working
//! directory HangarRS inherits and fails from its own; the Garmin Trainer
//! launcher requires its own and fails without it. Both are reproducible, and
//! no single policy satisfies both — so [`Cwd`] comes from the vendor table
//! and this module resolves whatever it is told.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hangar_runtime::env::ProtonEnv;
use hangar_runtime::run_state::{is_app_output, IMMEDIATE_EXIT};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::vendor::{Cwd, Profile, Verb};

/// Console strings that mean failure whatever the exit code says.
///
/// Every one of them has been observed on a run that exited 0:
///
/// * `Error Creating process` — the stub could not spawn its own engine.
/// * `valid purchase` — Just Flight's purchase check could not run its helper.
/// * `File name is too long` — the MAX_PATH prompt, which must never be
///   auto-answered.
/// * `0xce` — InstallShield's engine failure code.
pub const UNIVERSAL_BAD: &[&str] = &[
    "Error Creating process",
    "valid purchase",
    "File name is too long",
    "0xce",
];

/// A line that matched a known-bad signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleHit {
    /// The signature that matched.
    pub signature: String,
    /// The line it matched in, kept whole — the surrounding text is usually
    /// the only detail about *which* file or process was involved.
    pub line: String,
}

/// Everything needed to start an installer, all of it resolved.
///
/// Built once and inspected before anything runs, so the exact command is
/// visible in a dry run rather than assembled inside the call that executes
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

impl Invocation {
    /// What would be run, as a person would type it. For logs and dry runs.
    pub fn command_line(&self) -> String {
        std::iter::once(&self.program)
            .chain(self.args.iter())
            .map(|part| {
                let s = part.to_string_lossy();
                if s.contains(' ') {
                    format!("\"{s}\"")
                } else {
                    s.into_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Resolve a vendor profile into a command.
    ///
    /// `app_dir` is only consulted for [`Cwd::AppDir`], and is absent until
    /// something has been installed — which is why asking for it before that
    /// is a typed error rather than a fallback to somewhere plausible.
    pub fn plan(
        profile: &Profile,
        proton: &ProtonEnv,
        installer: &Path,
        app_dir: Option<&Path>,
    ) -> Result<Self, PlanError> {
        let (program, mut args) = program_and_verb(profile.verb, proton)?;
        args.push(installer.as_os_str().to_owned());
        // The installer's own switches go after its path, because everything
        // before it belongs to Proton.
        args.extend(profile.args.iter().map(OsString::from));

        let cwd = match profile.cwd {
            Cwd::Inherit => std::env::current_dir().map_err(|e| PlanError::NoInheritedCwd {
                detail: e.to_string(),
            })?,
            Cwd::InstallerDir => installer.parent().map(Path::to_path_buf).ok_or_else(|| {
                PlanError::NoInstallerDir {
                    installer: installer.to_path_buf(),
                }
            })?,
            Cwd::AppDir => app_dir.map(Path::to_path_buf).ok_or(PlanError::NoAppDir)?,
        };

        // The profile's environment goes on last, because that is what it is
        // for: `DOTNET_SYSTEM_GLOBALIZATION_USENLS=1` exists to override what
        // the prefix would otherwise do.
        let mut env = proton.vars();
        for (k, v) in &profile.env {
            env.push((OsString::from(k), OsString::from(v)));
        }

        Ok(Invocation {
            program,
            args,
            cwd,
            env,
        })
    }

    /// Every known-bad string for this run: the universal ones and the
    /// vendor's own.
    ///
    /// Deduplicated, because a vendor entry can repeat a universal string
    /// (Just Flight's profile lists `valid purchase`); otherwise one line
    /// would report as two hits.
    pub fn signatures(profile: &Profile) -> Vec<String> {
        let mut seen = Vec::new();
        for sig in UNIVERSAL_BAD
            .iter()
            .map(|s| (*s).to_owned())
            .chain(profile.bad_console.iter().cloned())
        {
            if !seen.iter().any(|s: &String| s.eq_ignore_ascii_case(&sig)) {
                seen.push(sig);
            }
        }
        seen
    }
}

fn program_and_verb(
    verb: Verb,
    proton: &ProtonEnv,
) -> Result<(OsString, Vec<OsString>), PlanError> {
    if proton.proton_script.is_file() {
        return Ok((
            proton.proton_script.as_os_str().to_owned(),
            vec![OsString::from(verb.as_str())],
        ));
    }

    // Bare Wine has no verbs. Downgrading `runinprefix` to a plain Wine call
    // silently is the failure the verb exists to prevent: `proton run` wraps
    // the process in a session that tears the prefix down when the stub exits,
    // killing the engine it just spawned.
    if verb == Verb::RunInPrefix {
        return Err(PlanError::VerbNeedsProtonScript {
            verb: verb.as_str(),
            looked_for: proton.proton_script.clone(),
        });
    }

    Ok((proton.wine_binary.as_os_str().to_owned(), Vec::new()))
}

/// Why an invocation could not even be described.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("the vendor profile asks for `{verb}`, which needs the proton script; none at {}", looked_for.display())]
    VerbNeedsProtonScript {
        verb: &'static str,
        looked_for: PathBuf,
    },

    #[error("this profile runs from the installed application's directory, and nothing is installed yet")]
    NoAppDir,

    #[error("{} has no parent directory to run from", installer.display())]
    NoInstallerDir { installer: PathBuf },

    #[error("cannot read the current working directory: {detail}")]
    NoInheritedCwd { detail: String },
}

/// What a run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// `None` when it was killed by a signal rather than exiting.
    pub code: Option<i32>,
    pub after: Duration,
    /// Every line, both streams, in arrival order.
    pub log: Vec<String>,
    pub hits: Vec<ConsoleHit>,
    /// Whether anything that was not Wine's own chatter reached the console.
    pub spoke: bool,
}

/// What the run itself says. Never that it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A known-bad string appeared. This outranks the exit code in both
    /// directions: a run that printed one and exited 0 has failed.
    BadConsole(Vec<ConsoleHit>),
    /// Gone in under two seconds with nothing to say — what a stub torn down
    /// mid-unpack looks like from outside.
    Vanished { after: Duration },
    /// Killed by a signal rather than exiting.
    Killed { after: Duration },
    /// A non-zero exit, with nothing more specific to say about it.
    ExitCode(i32),
    /// Nothing in the run said it failed.
    ///
    /// **Not success.** Whether anything was installed is [`crate::snapshot::Diff`]'s
    /// answer, and this is only the absence of a complaint.
    NoComplaint,
}

impl Verdict {
    /// Whether the run itself objected. A `false` here still needs the diffs.
    pub fn objected(&self) -> bool {
        !matches!(self, Verdict::NoComplaint)
    }
}

/// Read a completed run. Pure, so every branch is testable without starting
/// anything.
pub fn judge(ran: &Ran) -> Verdict {
    if !ran.hits.is_empty() {
        return Verdict::BadConsole(ran.hits.clone());
    }
    if ran.after < IMMEDIATE_EXIT && !ran.spoke {
        return Verdict::Vanished { after: ran.after };
    }
    match ran.code {
        None => Verdict::Killed { after: ran.after },
        Some(0) => Verdict::NoComplaint,
        Some(code) => Verdict::ExitCode(code),
    }
}

/// Test one line against the known-bad signatures.
pub fn scan_line(line: &str, signatures: &[String]) -> Option<ConsoleHit> {
    signatures.iter().find_map(|sig| {
        matches_signature(line, sig).then(|| ConsoleHit {
            signature: sig.clone(),
            line: line.to_owned(),
        })
    })
}

/// Case-insensitive substring, with one tightening for hex codes.
///
/// `0xce` as a plain substring matches `0xceff0080`, and Wine prints addresses
/// constantly — so a signature that is itself a hex literal has to end where
/// it says it does. Everything else stays a substring, because `valid
/// purchase` has to match inside a sentence.
fn matches_signature(line: &str, signature: &str) -> bool {
    let haystack = line.to_ascii_lowercase();
    let needle = signature.to_ascii_lowercase();
    let Some(at) = haystack.find(&needle) else {
        return false;
    };

    if !is_hex_literal(&needle) {
        return true;
    }

    haystack[at + needle.len()..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphanumeric())
}

fn is_hex_literal(s: &str) -> bool {
    s.starts_with("0x") && s.len() > 2 && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Something to hand each console line to as it arrives.
pub type OnLine = Arc<dyn Fn(&str) + Send + Sync>;

/// Could not be started at all.
#[derive(Debug, Error)]
#[error("could not start {program}: {source}")]
pub struct SpawnError {
    pub program: String,
    #[source]
    pub source: std::io::Error,
}

/// Run an installer to completion, watching what it says on the way.
///
/// There is **no timeout**. A four-gigabyte InstallShield title takes as long
/// as it takes, and a wrapper that gave up partway would leave a half-written
/// install with nobody watching. Cancellation belongs to the caller, which can
/// drop the future.
///
/// The reader tasks are joined **after** the process exits and before the log
/// is read. `wait()` returns the moment the process dies, and the tasks
/// draining its pipes have not necessarily finished — so judging the run
/// straight after `wait()` races them, and the loser is the diagnostic. An
/// installer that failed *and printed why* would otherwise be recorded as
/// having said nothing, which is exactly the case this stage exists to catch.
pub async fn run(
    invocation: &Invocation,
    signatures: &[String],
    on_line: Option<OnLine>,
) -> Result<Ran, SpawnError> {
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .envs(invocation.env.iter().cloned())
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let started = Instant::now();
    let mut child = command.spawn().map_err(|source| SpawnError {
        program: invocation.program.to_string_lossy().into_owned(),
        source,
    })?;

    let log = Arc::new(Mutex::new(Vec::new()));
    let hits = Arc::new(Mutex::new(Vec::new()));
    let spoke = Arc::new(Mutex::new(false));

    let mut pumps = Vec::new();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    for stream in [
        stdout.map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        stderr.map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let log = Arc::clone(&log);
        let hits = Arc::clone(&hits);
        let spoke = Arc::clone(&spoke);
        let signatures = signatures.to_vec();
        let on_line = on_line.clone();

        pumps.push(tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(hit) = scan_line(&line, &signatures) {
                    tracing::warn!("installer console: {}", hit.line);
                    hits.lock().unwrap_or_else(|e| e.into_inner()).push(hit);
                }
                if is_app_output(&line) {
                    *spoke.lock().unwrap_or_else(|e| e.into_inner()) = true;
                }
                if let Some(on_line) = &on_line {
                    on_line(&line);
                }
                log.lock().unwrap_or_else(|e| e.into_inner()).push(line);
            }
        }));
    }

    let status = child.wait().await;
    // The pipes are closed now, so these finish in microseconds. The cap is
    // for a grandchild that inherited one and is holding it open after its
    // parent is gone: there is nothing left to wait for, and a run must never
    // hang on a stranger's file descriptor.
    for pump in pumps {
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, pump).await;
    }

    let after = started.elapsed();
    let code = status.ok().and_then(|s| s.code());

    let log = std::mem::take(&mut *log.lock().unwrap_or_else(|e| e.into_inner()));
    let hits = std::mem::take(&mut *hits.lock().unwrap_or_else(|e| e.into_inner()));
    let spoke = *spoke.lock().unwrap_or_else(|e| e.into_inner());

    Ok(Ran {
        code,
        after,
        log,
        hits,
        spoke,
    })
}

/// How long to let the readers finish once the process has gone.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::vendor::Table;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hangar-run-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Scratch(dir)
        }

        fn file(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(&p, b"#!/bin/sh\n").expect("write");
            p
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A Proton environment with a real script on disk, so `plan` takes the
    /// branch the machine actually takes.
    fn proton_with_script(s: &Scratch) -> ProtonEnv {
        ProtonEnv {
            wineprefix: s.0.join("pfx"),
            proton_script: s.file("proton"),
            wine_binary: s.0.join("wine"),
            compat_data: s.0.join("compatdata"),
            compat_install: s.0.join("install"),
            steam_client: s.0.join("steam"),
            is_flatpak: false,
        }
    }

    /// No script — the bare-Wine fallback.
    fn proton_without_script(s: &Scratch) -> ProtonEnv {
        ProtonEnv {
            proton_script: s.0.join("no-such-proton"),
            ..proton_with_script(s)
        }
    }

    fn just_flight() -> Profile {
        Table::shipped()
            .match_on(
                Engine::InstallShield,
                "Just Flight (MSFS 2024)",
                "Black Square - Commander 114 MSFS2024",
            )
            .expect("a profile")
            .profile
            .clone()
    }

    #[test]
    fn the_verb_comes_from_the_profile_not_from_proton_env() {
        let s = Scratch::new("verb");
        let proton = proton_with_script(&s);
        let installer = s.file("setup.exe");

        let mut profile = just_flight();
        profile.verb = Verb::RunInPrefix;

        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        assert_eq!(plan.args[0], OsString::from("runinprefix"));

        profile.verb = Verb::Run;
        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        assert_eq!(plan.args[0], OsString::from("run"));
    }

    /// Bare Wine has no verbs, and quietly dropping `runinprefix` is the
    /// failure the verb exists to prevent.
    #[test]
    fn runinprefix_without_the_proton_script_is_refused_rather_than_downgraded() {
        let s = Scratch::new("noscript");
        let proton = proton_without_script(&s);
        let installer = s.file("setup.exe");

        let mut profile = just_flight();
        profile.verb = Verb::RunInPrefix;

        assert!(matches!(
            Invocation::plan(&profile, &proton, &installer, None),
            Err(PlanError::VerbNeedsProtonScript { .. })
        ));
    }

    #[test]
    fn plain_run_falls_back_to_the_wine_binary_with_no_verb() {
        let s = Scratch::new("winefallback");
        let proton = proton_without_script(&s);
        let installer = s.file("setup.exe");

        let mut profile = just_flight();
        profile.verb = Verb::Run;

        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        assert_eq!(plan.program, proton.wine_binary.as_os_str());
        assert_eq!(plan.args, vec![installer.as_os_str().to_owned()]);
    }

    #[test]
    fn installer_switches_go_after_the_installer_path() {
        let s = Scratch::new("args");
        let proton = proton_with_script(&s);
        let installer = s.file("setup.exe");

        let mut profile = just_flight();
        profile.args = vec!["/passive".into(), "/norestart".into()];

        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        assert_eq!(
            plan.args,
            vec![
                OsString::from("run"),
                installer.as_os_str().to_owned(),
                OsString::from("/passive"),
                OsString::from("/norestart"),
            ],
            "everything before the installer belongs to Proton"
        );
    }

    /// Nothing in the shipped table sets a switch, because none has been
    /// observed working — and a wrong one looks exactly like a failed install.
    #[test]
    fn the_shipped_table_passes_no_switches() {
        let table = Table::shipped();
        for profile in &table.profiles {
            assert!(
                profile.args.is_empty(),
                "{} ships switches that nobody has watched work: {:?}",
                profile.name,
                profile.args
            );
        }
    }

    #[test]
    fn cwd_inherit_is_hangarrs_own_directory() {
        let s = Scratch::new("inherit");
        let proton = proton_with_script(&s);
        let installer = s.file("sub/setup.exe");

        let mut profile = just_flight();
        profile.cwd = Cwd::Inherit;

        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        assert_eq!(plan.cwd, std::env::current_dir().expect("cwd"));
    }

    #[test]
    fn cwd_installer_dir_is_the_directory_the_installer_sits_in() {
        let s = Scratch::new("installerdir");
        let proton = proton_with_script(&s);
        let installer = s.file("sub/setup.exe");

        let mut profile = just_flight();
        profile.cwd = Cwd::InstallerDir;

        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        assert_eq!(plan.cwd, installer.parent().expect("parent"));
    }

    /// Nothing is installed yet, so there is no application directory to run
    /// from. Saying so beats defaulting to somewhere plausible.
    #[test]
    fn cwd_app_dir_without_an_app_dir_is_a_typed_error() {
        let s = Scratch::new("appdir");
        let proton = proton_with_script(&s);
        let installer = s.file("setup.exe");

        let mut profile = just_flight();
        profile.cwd = Cwd::AppDir;

        assert_eq!(
            Invocation::plan(&profile, &proton, &installer, None),
            Err(PlanError::NoAppDir)
        );

        let app = s.0.join("app");
        let plan = Invocation::plan(&profile, &proton, &installer, Some(&app)).expect("planned");
        assert_eq!(plan.cwd, app);
    }

    #[test]
    fn the_profiles_environment_goes_on_after_protons() {
        let s = Scratch::new("env");
        let proton = proton_with_script(&s);
        let installer = s.file("setup.exe");

        let profile = Table::shipped()
            .match_on(Engine::WixBurn, "Garmin", "Garmin Aviation Trainers")
            .expect("a profile")
            .profile
            .clone();

        let plan = Invocation::plan(&profile, &proton, &installer, None).expect("planned");
        let names: Vec<_> = plan
            .env
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"WINEPREFIX".to_string()));
        let nls = plan
            .env
            .iter()
            .position(|(k, _)| k == "DOTNET_SYSTEM_GLOBALIZATION_USENLS")
            .expect("the variable without which .NET 6 x86 dies before a window appears");
        let wineprefix = plan
            .env
            .iter()
            .position(|(k, _)| k == "WINEPREFIX")
            .expect("WINEPREFIX");
        assert!(nls > wineprefix, "the vendor's environment has to win");
    }

    #[test]
    fn a_command_line_quotes_the_paths_with_spaces_in_them() {
        let s = Scratch::new("cmdline");
        let proton = proton_with_script(&s);
        let installer = s.file("Program Files/setup.exe");

        let plan = Invocation::plan(&just_flight(), &proton, &installer, None).expect("planned");
        assert!(plan.command_line().contains("\"") && plan.command_line().contains("setup.exe"));
    }

    #[test]
    fn the_signatures_are_the_universal_ones_plus_the_vendors() {
        let signatures = Invocation::signatures(&just_flight());
        for universal in UNIVERSAL_BAD {
            assert!(signatures.iter().any(|s| s == universal), "{universal}");
        }
        assert_eq!(
            signatures
                .iter()
                .filter(|s| s.eq_ignore_ascii_case("valid purchase"))
                .count(),
            1,
            "Just Flight's own entry names it too, and scanning twice reports \
             one line as two hits"
        );
    }

    fn sigs() -> Vec<String> {
        UNIVERSAL_BAD.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_bad_string_matches_inside_a_sentence_and_ignores_case() {
        let hit = scan_line(
            "Setup could not detect a VALID PURCHASE of this product.",
            &sigs(),
        )
        .expect("a hit");
        assert_eq!(hit.signature, "valid purchase");
        assert!(
            hit.line.contains("Setup could not"),
            "the line is kept whole"
        );
    }

    /// Wine prints addresses constantly, and `0xce` as a plain substring
    /// matches inside every one of them that happens to contain it.
    #[test]
    fn a_hex_signature_does_not_match_a_longer_address() {
        assert!(scan_line("err: something at 0xceff0080 failed", &sigs()).is_none());
        assert!(scan_line("Setup failed with 0xCE", &sigs()).is_some());
        assert!(scan_line("exit code 0xce.", &sigs()).is_some());
    }

    #[test]
    fn an_ordinary_line_matches_nothing() {
        assert!(scan_line("Extracting: aircraft.cfg", &sigs()).is_none());
        assert!(scan_line("", &sigs()).is_none());
    }

    fn ran(after: Duration, code: Option<i32>, spoke: bool, hits: Vec<ConsoleHit>) -> Ran {
        Ran {
            code,
            after,
            log: Vec::new(),
            hits,
            spoke,
        }
    }

    /// The whole reason this stage exists: a run that printed a known failure
    /// and exited 0 has failed.
    #[test]
    fn a_bad_console_string_outranks_a_zero_exit() {
        let verdict = judge(&ran(
            Duration::from_secs(30),
            Some(0),
            true,
            vec![ConsoleHit {
                signature: "valid purchase".into(),
                line: "no valid purchase found".into(),
            }],
        ));
        assert!(matches!(verdict, Verdict::BadConsole(_)));
        assert!(verdict.objected());
    }

    #[test]
    fn a_silent_disappearance_inside_two_seconds_is_a_vanishing() {
        let verdict = judge(&ran(Duration::from_millis(300), Some(0), false, Vec::new()));
        assert!(matches!(verdict, Verdict::Vanished { .. }));
    }

    /// It exited quickly, but it said something on the way — that is a run
    /// with a diagnostic in it, not a stub torn down before it started.
    #[test]
    fn a_quick_exit_that_said_something_is_not_a_vanishing() {
        let verdict = judge(&ran(Duration::from_millis(300), Some(0), true, Vec::new()));
        assert_eq!(verdict, Verdict::NoComplaint);
    }

    #[test]
    fn a_long_quiet_run_that_exited_zero_draws_no_complaint() {
        let verdict = judge(&ran(Duration::from_secs(600), Some(0), false, Vec::new()));
        assert_eq!(verdict, Verdict::NoComplaint);
        assert!(
            !verdict.objected(),
            "and it still says nothing about whether anything was installed"
        );
    }

    #[test]
    fn a_non_zero_exit_is_reported_as_itself() {
        assert_eq!(
            judge(&ran(Duration::from_secs(10), Some(3), true, Vec::new())),
            Verdict::ExitCode(3)
        );
    }

    #[test]
    fn a_process_killed_by_a_signal_is_not_an_exit_code() {
        assert!(matches!(
            judge(&ran(Duration::from_secs(10), None, true, Vec::new())),
            Verdict::Killed { .. }
        ));
    }

    /// A real spawn, of something that is not an installer, so the plumbing —
    /// pipes, drain, timing, judgement — is exercised without Proton.
    #[tokio::test]
    async fn a_run_collects_both_streams_and_finds_a_signature() {
        let invocation = Invocation {
            program: OsString::from("/bin/sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from("echo Extracting: thing.cfg; echo 'no valid purchase' >&2; exit 0"),
            ],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
        };

        let ran = run(&invocation, &sigs(), None).await.expect("spawned");
        assert_eq!(ran.code, Some(0));
        assert_eq!(ran.log.len(), 2, "both streams: {:?}", ran.log);
        assert_eq!(ran.hits.len(), 1);
        assert_eq!(ran.hits[0].signature, "valid purchase");
        assert!(matches!(judge(&ran), Verdict::BadConsole(_)));
    }

    #[tokio::test]
    async fn a_program_that_does_not_exist_is_a_spawn_error() {
        let invocation = Invocation {
            program: OsString::from("/nonexistent/proton"),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            env: Vec::new(),
        };

        assert!(run(&invocation, &sigs(), None).await.is_err());
    }

    #[tokio::test]
    async fn every_line_reaches_the_callback() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let on_line: OnLine = Arc::new(move |line: &str| {
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_owned())
        });

        let invocation = Invocation {
            program: OsString::from("/bin/sh"),
            args: vec![OsString::from("-c"), OsString::from("echo one; echo two")],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
        };

        let ran = run(&invocation, &sigs(), Some(on_line))
            .await
            .expect("spawned");
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(seen.len(), ran.log.len());
        assert_eq!(seen.len(), 2);
    }
}
