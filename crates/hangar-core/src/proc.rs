//! Reading `/proc` to find out what is running.
//!
//! # Always match on `cmdline`
//!
//! Never `comm`, never `exe`. Both lie about Wine processes:
//!
//! | PID | `comm` | `/proc/PID/exe` |
//! |---|---|---|
//! | 53513 | `CrBrowserMain` | `…/wine64-preloader` |
//! | 53536 | `CrGpuMain` | `…/wine64-preloader` |
//! | 53591 | `CrRendererMain` | `…/wine64-preloader` |
//!
//! Chromium overwrites process names with thread names, and `exe` resolves to
//! the Wine preloader for *every* Wine process. Only `cmdline` carries the real
//! identity. The same applies to the sim itself, which `comm` truncates to
//! `FlightSimulator2`.

use std::io;
use std::path::Path;

/// One process, as far as `/proc` will say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    /// The full command line, NUL separators replaced with spaces. For display.
    pub cmdline: String,
    /// The individual arguments, still separated as the kernel stored them.
    ///
    /// Kept apart from [`Self::cmdline`] because matching needs them: an
    /// executable name can contain spaces ("Project Airports.exe"), so a
    /// space-split of the joined string is not the same thing as the real
    /// argument list.
    pub args: Vec<String>,
}

impl ProcInfo {
    /// Whether this process **is** the named executable.
    ///
    /// Matched on `argv[0]` alone, which is the only argument that says what a
    /// process is running. Anything looser produces false positives that are
    /// easy to demonstrate and unpleasant to debug:
    ///
    /// * a plain substring search over the whole command line matches a shell
    ///   whose script mentions the app, an editor with a note about it open, or
    ///   a `grep` for it;
    /// * matching *any* argument additionally matches the tool being run to
    ///   look for the app, since the name is its argument.
    ///
    /// Every genuine case has the executable in `argv[0]`: launched by full
    /// path, relaunched by an app that updated itself and carries only its bare
    /// name, or a Chromium helper, which repeats `argv[0]` and adds `--type=`.
    pub fn runs(&self, needle: &str) -> bool {
        self.args
            .first()
            .is_some_and(|argv0| argv0.ends_with(needle))
    }

    /// Whether this is a Chromium helper rather than the process in charge.
    ///
    /// An Electron app is four or more processes — browser, GPU, network,
    /// renderer — and only the one without a `--type=` flag is the real
    /// instance. Adopting a renderer would track the wrong thing, and killing
    /// one achieves nothing because the browser process restarts it.
    pub fn is_helper(&self) -> bool {
        self.args.iter().any(|a| a.starts_with("--type="))
    }
}

/// Read `/proc/{pid}/cmdline`.
pub fn read_cmdline(pid: u32) -> io::Result<String> {
    Ok(parse_cmdline(&std::fs::read(format!(
        "/proc/{pid}/cmdline"
    ))?))
}

/// Read `/proc/{pid}/cmdline` as its separate arguments.
pub fn read_args(pid: u32) -> io::Result<Vec<String>> {
    Ok(split_cmdline(&std::fs::read(format!(
        "/proc/{pid}/cmdline"
    ))?))
}

/// Split raw `cmdline` bytes into arguments.
pub fn split_cmdline(raw: &[u8]) -> Vec<String> {
    raw.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Turn raw `cmdline` bytes into a searchable string.
///
/// The file is NUL-separated with a trailing NUL. Splitting and rejoining with
/// spaces is what makes `contains` work the way a person expects.
pub fn parse_cmdline(raw: &[u8]) -> String {
    raw.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read the 15-character-truncated name from `/proc/{pid}/comm`.
///
/// Provided for diagnostics only. **Do not match on it** — see the module note.
pub fn read_comm(pid: u32) -> io::Result<String> {
    Ok(std::fs::read_to_string(format!("/proc/{pid}/comm"))?
        .trim_end()
        .to_owned())
}

/// Read the parent PID from `/proc/{pid}/status`.
pub fn read_ppid(pid: u32) -> io::Result<u32> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    parse_ppid(&content)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no PPid line in /proc status"))
}

/// Pull `PPid:` out of a `/proc/[pid]/status` body.
pub fn parse_ppid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()
}

/// Whether `/proc/{pid}` exists at all.
///
/// **Not the same as running.** A process that has exited but not yet been
/// reaped stays a zombie, and its `/proc` entry stays with it. Use
/// [`is_alive`] for "is this still doing anything".
pub fn pid_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Whether a process exists and has not already exited.
///
/// A zombie counts as gone. This matters for anything we spawned ourselves: a
/// child that has been signalled stays in `/proc` until it is reaped, so
/// waiting on [`pid_exists`] to go false after a `SIGTERM` waits forever and
/// then escalates to `SIGKILL` on a process that already exited cleanly.
pub fn is_alive(pid: u32) -> bool {
    match read_state(pid) {
        Some(state) => state != 'Z',
        // No status file means no process.
        None => false,
    }
}

/// The single-letter state from `/proc/{pid}/status`.
pub fn read_state(pid: u32) -> Option<char> {
    parse_state(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// Pull the state letter out of a `/proc/[pid]/status` body.
pub fn parse_state(status: &str) -> Option<char> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("State:"))?
        .trim()
        .chars()
        .next()
}

/// Every process whose command line contains `needle`, lowest PID first.
///
/// The scan is a `read_dir` over `/proc` and one small read per entry —
/// a few milliseconds on a normal system, and it needs no privileges.
pub fn find_by_cmdline(needle: &str) -> Vec<ProcInfo> {
    let mut found = scan(|info| info.runs(needle));
    found.sort_by_key(|p| p.pid);
    found
}

/// Every process matching a predicate.
pub fn scan(mut predicate: impl FnMut(&ProcInfo) -> bool) -> Vec<ProcInfo> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let own = std::process::id();

    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        // Never match ourselves. Our own command line can easily contain an
        // app's name — a launch argument, a search box — and adopting or
        // killing our own process would be a memorable bug.
        .filter(|pid| *pid != own)
        .filter_map(|pid| {
            let args = read_args(pid).ok()?;
            Some(ProcInfo {
                pid,
                cmdline: args.join(" "),
                args,
            })
        })
        .filter(|info| predicate(info))
        .collect()
}

/// The processes belonging to one application.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppProcesses {
    /// The process in charge, if one is running.
    pub main: Option<ProcInfo>,
    /// Chromium helpers and other children.
    pub helpers: Vec<ProcInfo>,
}

impl AppProcesses {
    pub fn is_running(&self) -> bool {
        self.main.is_some()
    }

    pub fn main_pid(&self) -> Option<u32> {
        self.main.as_ref().map(|p| p.pid)
    }

    /// Total processes found, main included.
    pub fn count(&self) -> usize {
        self.helpers.len() + usize::from(self.main.is_some())
    }
}

/// Find the processes for an executable, separating the main one from helpers.
///
/// `needle` should be the executable's **file name**, not its full path. This is
/// not a shortcut — it is the only thing that works. An app that self-updates
/// relaunches itself, and the replacement carries its own name and its own
/// arguments, with no trace of the path the launcher used. Project Airports
/// re-executed as `Project Airports.exe --updated`; matching the full launch
/// path would have missed it.
pub fn find_app(needle: &str) -> AppProcesses {
    classify(find_by_cmdline(needle))
}

/// Find several apps in **one** pass over `/proc`.
///
/// One scan for all of them rather than one per app: a scan is a `read_dir`
/// plus a read per process, and repeating it per key multiplies that by the
/// number of apps for no benefit. With a poll running every couple of seconds
/// that difference is worth having.
pub fn find_apps(needles: &[String]) -> std::collections::BTreeMap<String, AppProcesses> {
    let mut hits: std::collections::BTreeMap<String, Vec<ProcInfo>> =
        needles.iter().map(|n| (n.clone(), Vec::new())).collect();

    for info in scan(|info| needles.iter().any(|n| info.runs(n))) {
        for needle in needles {
            if info.runs(needle) {
                hits.entry(needle.clone()).or_default().push(info.clone());
            }
        }
    }

    hits.into_iter()
        .map(|(needle, mut matches)| {
            matches.sort_by_key(|p| p.pid);
            (needle, classify(matches))
        })
        .collect()
}

/// Split a set of matches into the main process and its helpers.
pub fn classify(matches: Vec<ProcInfo>) -> AppProcesses {
    let (helpers, mut mains): (Vec<_>, Vec<_>) = matches.into_iter().partition(ProcInfo::is_helper);
    // Lowest PID among the non-helpers: the browser process is started first,
    // and its children follow.
    mains.sort_by_key(|p| p.pid);
    AppProcesses {
        main: mains.into_iter().next(),
        helpers,
    }
}

/// The match key for an executable path.
///
/// Just the file name, for the reason in [`find_app`].
pub fn match_key(exe_path: &Path) -> String {
    exe_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| exe_path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `ProcInfo` from arguments, as the kernel would store them.
    fn info(pid: u32, args: &[&str]) -> ProcInfo {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        ProcInfo {
            pid,
            cmdline: args.join(" "),
            args,
        }
    }

    #[test]
    fn cmdline_bytes_become_a_searchable_string() {
        let raw = b"/usr/bin/app\0--flag\0value\0";
        assert_eq!(parse_cmdline(raw), "/usr/bin/app --flag value");
    }

    #[test]
    fn an_empty_cmdline_is_empty_not_a_panic() {
        assert_eq!(parse_cmdline(b""), "");
        assert_eq!(parse_cmdline(b"\0\0\0"), "");
    }

    /// Kernel threads have an empty cmdline; invalid UTF-8 must not blow up.
    #[test]
    fn invalid_utf8_is_lossy_not_fatal() {
        let raw = b"/bin/\xff\xfeapp\0";
        assert!(parse_cmdline(raw).contains("app"));
    }

    #[test]
    fn ppid_parses_out_of_a_status_body() {
        let status = "Name:\tCrBrowserMain\nUmask:\t0022\nState:\tS (sleeping)\n\
                      Tgid:\t53513\nPid:\t53513\nPPid:\t2228\nTracerPid:\t0\n";
        assert_eq!(parse_ppid(status), Some(2228));
        assert_eq!(parse_ppid("Name:\tx\n"), None);
        assert_eq!(parse_ppid(""), None);
    }

    /// Real Wine processes from this machine, verbatim.
    #[test]
    fn chromium_helpers_are_told_apart_from_the_main_process() {
        let matches = vec![
            info(53513, &["Project Airports.exe", "--updated"]),
            info(53536, &["Project Airports.exe", "--type=gpu-process"]),
            info(
                53566,
                &[
                    "Project Airports.exe",
                    "--type=utility",
                    "--utility-sub-type=network.mojom.NetworkService",
                ],
            ),
            info(
                53591,
                &[
                    "Project Airports.exe",
                    "--type=renderer",
                    "--enable-sandbox",
                ],
            ),
        ];
        let app = classify(matches);

        assert_eq!(
            app.main_pid(),
            Some(53513),
            "the browser process is in charge"
        );
        assert_eq!(app.helpers.len(), 3);
        assert_eq!(app.count(), 4);
        assert!(app.is_running());
    }

    /// Killing or adopting a renderer would be wrong: the browser process
    /// restarts its children, so only the main PID matters.
    #[test]
    fn a_lone_helper_does_not_count_as_a_running_app() {
        let app = classify(vec![info(53591, &["app.exe", "--type=renderer"])]);
        assert!(!app.is_running());
        assert_eq!(app.main_pid(), None);
        assert_eq!(app.helpers.len(), 1);
    }

    #[test]
    fn nothing_running_is_an_empty_result() {
        let app = classify(Vec::new());
        assert!(!app.is_running());
        assert_eq!(app.count(), 0);
    }

    /// The main process is the lowest-numbered non-helper: an app relaunched
    /// after a self-update leaves the newer PID, and picking the highest would
    /// track a child.
    #[test]
    fn the_lowest_non_helper_pid_wins() {
        let app = classify(vec![
            info(53600, &["app.exe", "--second-instance"]),
            info(53513, &["app.exe", "--updated"]),
        ]);
        assert_eq!(app.main_pid(), Some(53513));
    }

    /// **The reason matching uses the file name.** The self-relaunched process
    /// carries none of the launcher's path.
    #[test]
    fn the_match_key_is_the_file_name_not_the_launch_path() {
        let launched = Path::new(
            "/home/user/.local/share/msfs-manager/companion-prefix/pfx/drive_c/users/steamuser/\
             AppData/Local/Programs/@project-airportsdesktop/Project Airports.exe",
        );
        let key = match_key(launched);
        assert_eq!(key, "Project Airports.exe");

        // What the process actually looked like after it updated itself.
        let running = "Project Airports.exe --updated";
        assert!(running.contains(&key), "the file name matches");
        assert!(
            !running.contains(&launched.display().to_string()),
            "the full launch path does not — matching on it is what missed the orphan"
        );
    }

    /// **The false positive.** A shell running a
    /// script that mentions an app by name is not that app, and neither is a
    /// search command. Only a token that *ends with* the name counts.
    #[test]
    fn a_command_line_that_merely_mentions_the_name_is_not_a_match() {
        let needle = "Project Airports.exe";

        let real = info(53513, &["Project Airports.exe", "--updated"]);
        let launched = info(
            100,
            &[
                "/prefix/drive_c/Programs/@pa/Project Airports.exe",
                "--no-sandbox",
            ],
        );
        assert!(real.runs(needle), "the self-relaunched process");
        assert!(
            launched.runs(needle),
            "the process we launched by full path"
        );

        // A shell whose whole script is one argument containing the name.
        let shell = info(
            200,
            &["/bin/bash", "-c", "echo 'Project Airports.exe' && ls /proc"],
        );
        assert!(
            !shell.runs(needle),
            "a script mentioning the name is not the app"
        );
        assert!(
            shell.cmdline.contains(needle),
            "and it would have matched a plain substring search"
        );

        // The tool being run to look for the app carries the name as its own
        // argument.
        let searcher = info(
            201,
            &[
                "timeout",
                "300",
                "cargo",
                "run",
                "--example",
                "watch",
                "Project Airports.exe",
            ],
        );
        assert!(
            !searcher.runs(needle),
            "the name as a trailing argument is not the app running"
        );

        // An editor with the file open, and an unrelated program.
        assert!(!info(300, &["nvim", "notes-about-Project Airports.exe.md"]).runs(needle));
        assert!(!info(301, &["/usr/bin/something-else"]).runs(needle));
        assert!(!info(302, &[]).runs(needle), "a kernel thread has no argv");
    }

    /// The helper flag is an argument in its own right, not a substring.
    #[test]
    fn helper_detection_looks_at_arguments_not_the_joined_string() {
        assert!(info(1, &["app.exe", "--type=renderer"]).is_helper());
        assert!(
            !info(2, &["app.exe", "--config=/x/--type=renderer.conf"]).is_helper(),
            "a path that happens to contain the flag is not a helper"
        );
    }

    #[test]
    fn arguments_split_on_nul_not_whitespace() {
        let raw = b"/path/Project Airports.exe\0--no-sandbox\0";
        let args = split_cmdline(raw);
        assert_eq!(args, ["/path/Project Airports.exe", "--no-sandbox"]);
        assert_eq!(
            args[0], "/path/Project Airports.exe",
            "the space is inside one argument"
        );
    }

    #[test]
    fn match_key_survives_a_path_with_no_file_name() {
        assert_eq!(match_key(Path::new("/")), "/");
    }

    // ── Against the real /proc ────────────────────────────────────────────

    #[test]
    fn reads_this_process_from_proc() {
        let pid = std::process::id();
        assert!(pid_exists(pid));
        assert!(!read_cmdline(pid).unwrap().is_empty());
        assert!(read_ppid(pid).unwrap() > 0);
        assert!(!read_comm(pid).unwrap().is_empty());
    }

    /// A scan must never return the scanning process, or an app could adopt or
    /// kill itself.
    #[test]
    fn a_scan_never_returns_our_own_process() {
        let own = std::process::id();
        let own_cmdline = read_cmdline(own).unwrap();
        // Search for something guaranteed to be in our own command line.
        let needle = own_cmdline.split_whitespace().next().unwrap();
        assert!(!find_by_cmdline(needle).iter().any(|p| p.pid == own));
    }

    #[test]
    fn process_state_parses_out_of_a_status_body() {
        assert_eq!(parse_state("Name:\tx\nState:\tS (sleeping)\n"), Some('S'));
        assert_eq!(parse_state("State:\tZ (zombie)\n"), Some('Z'));
        assert_eq!(parse_state("State:\tR (running)\n"), Some('R'));
        assert_eq!(parse_state("Name:\tx\n"), None);
    }

    /// The distinction that made `kill` escalate needlessly: a zombie still
    /// has a `/proc` entry, so "the directory is there" is not "it is running".
    #[test]
    fn a_zombie_exists_but_is_not_alive() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("true is available");
        let pid = child.id();

        // Wait for it to become a zombie: exited, but not yet reaped.
        for _ in 0..200 {
            if read_state(pid) == Some('Z') {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(
            read_state(pid),
            Some('Z'),
            "expected a zombie to test against"
        );
        assert!(pid_exists(pid), "a zombie keeps its /proc entry");
        assert!(!is_alive(pid), "but it is not running");

        let _ = child.wait();
        assert!(!is_alive(pid));
    }

    #[test]
    fn this_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn a_missing_pid_reads_as_gone() {
        // PID 0 never exists as a /proc entry.
        assert!(!pid_exists(0));
        assert!(!is_alive(0));
        assert!(read_cmdline(0).is_err());
    }

    /// One pass must give the same answer as one scan per app.
    #[test]
    fn a_batch_scan_matches_individual_scans() {
        let own = read_cmdline(std::process::id()).unwrap();
        let present = own.split_whitespace().next().unwrap().to_owned();
        let absent = "hangar-no-such-process-zzzz".to_owned();

        let needles = vec![present.clone(), absent.clone()];
        let batch = find_apps(&needles);

        assert_eq!(batch.len(), 2, "every needle gets an entry, hit or not");
        assert_eq!(batch[&present].main_pid(), find_app(&present).main_pid());
        assert!(!batch[&absent].is_running());
    }

    #[test]
    fn a_batch_scan_of_nothing_is_empty() {
        assert!(find_apps(&[]).is_empty());
    }

    #[test]
    fn scanning_for_nothing_finds_nothing() {
        assert!(find_by_cmdline("hangar-rs-no-such-process-abcdef123456").is_empty());
    }
}
