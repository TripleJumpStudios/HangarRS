//! Whether Steam is running.
//!
//! This exists for one reason: **Steam Cloud will overwrite a file edited
//! underneath it.** MSFS keeps its input profiles in `userdata/…/remote/`,
//! Steam syncs that directory, and a client that is running holds its own idea
//! of what those files contain. Write to them with Steam up and the next sync
//! either reverts the edit or, worse, uploads the stale copy and reverts it on
//! every machine.
//!
//! So anything that writes to `remote/` asks here first, and refuses if the
//! answer is yes. The check errs towards refusing: a false "Steam is running"
//! is an annoyance, a false "Steam is closed" costs someone their bindings.

use hangar_core::proc::{self, ProcInfo};

/// Executables that mean the client is up, matched as exact file names.
///
/// Exact rather than [`ProcInfo::runs`]' `ends_with`, because that would also
/// match a path ending `…/notsteam`. Being precise matters more here than in
/// app adoption: this decides whether a write is allowed.
///
/// `steamwebhelper` is included because it is the client's UI process and does
/// not meaningfully run without it — and a second signal costs nothing when
/// the failure mode of missing one is losing an edit.
const CLIENT_EXECUTABLES: &[&str] = &["steam", "steamwebhelper"];

/// What was found, so a refusal can name it rather than just saying no.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteamProcesses {
    pub processes: Vec<ProcInfo>,
}

impl SteamProcesses {
    pub fn is_running(&self) -> bool {
        !self.processes.is_empty()
    }

    pub fn pids(&self) -> Vec<u32> {
        self.processes.iter().map(|p| p.pid).collect()
    }

    /// `steam (4697), steamwebhelper (5269), and 8 more` — for an error
    /// message. Capped, because a running client has ten web helpers and a
    /// refusal that scrolls is a refusal nobody reads.
    pub fn summary(&self) -> String {
        const SHOWN: usize = 3;
        let mut parts: Vec<String> = self
            .processes
            .iter()
            .take(SHOWN)
            .map(|p| format!("{} ({})", file_name_of(p).unwrap_or("?"), p.pid))
            .collect();
        if self.processes.len() > SHOWN {
            parts.push(format!("and {} more", self.processes.len() - SHOWN));
        }
        parts.join(", ")
    }
}

/// Find the running Steam client processes, if any.
pub fn steam_processes() -> SteamProcesses {
    SteamProcesses {
        processes: proc::scan(is_steam_client),
    }
}

/// Whether the Steam client is running at all.
pub fn is_running() -> bool {
    steam_processes().is_running()
}

fn is_steam_client(info: &ProcInfo) -> bool {
    file_name_of(info).is_some_and(|name| CLIENT_EXECUTABLES.contains(&name))
}

/// The file name of `argv[0]`, and nothing looser.
///
/// `bash /home/…/Steam/steam.sh` is a shell running a script, not Steam — its
/// `argv[0]` is `bash`, and treating it as the client is exactly the
/// false positive the `argv[0]` rule exists to prevent.
fn file_name_of(info: &ProcInfo) -> Option<&str> {
    let argv0 = info.args.first()?;
    Some(match argv0.rsplit_once('/') {
        Some((_, name)) => name,
        None => argv0.as_str(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(args: &[&str]) -> ProcInfo {
        ProcInfo {
            pid: 1,
            cmdline: args.join(" "),
            args: args.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn the_real_client_is_recognised() {
        assert!(is_steam_client(&info(&[
            "/home/user/.local/share/Steam/ubuntu12_32/steam",
            "-srt-logger-opened",
        ])));
        assert!(is_steam_client(&info(&["./steamwebhelper", "-lang=en_US"])));
        // Flatpak and any other install path, since only the name is matched.
        assert!(is_steam_client(&info(&[
            "/var/lib/flatpak/app/com.valvesoftware.Steam/x86_64/stable/active/files/bin/steam"
        ])));
    }

    #[test]
    fn a_shell_running_steams_launcher_script_is_not_steam() {
        // argv[0] is bash. This is the exact shape the argv[0] rule exists for.
        assert!(!is_steam_client(&info(&[
            "bash",
            "/home/user/.local/share/Steam/steam.sh",
            "-srt-logger-opened",
        ])));
    }

    #[test]
    fn a_path_that_merely_ends_in_steam_is_not_steam() {
        // `ends_with` would accept both of these.
        assert!(!is_steam_client(&info(&["/usr/local/bin/notsteam"])));
        assert!(!is_steam_client(&info(&["/opt/mysteam"])));
    }

    #[test]
    fn steams_helper_services_do_not_count() {
        // These linger after the client exits, so counting them would make the
        // gate impossible to satisfy.
        assert!(!is_steam_client(&info(&[
            "steam-runtime-launcher-service",
            "--alongside-steam",
        ])));
        assert!(!is_steam_client(&info(&[
            "/usr/lib/pressure-vessel/from-host/libexec/steam-runtime-tools-0/pv-adverb"
        ])));
    }

    #[test]
    fn a_refusal_can_name_what_it_found() {
        let found = SteamProcesses {
            processes: vec![
                ProcInfo {
                    pid: 4697,
                    cmdline: String::new(),
                    args: vec!["/home/j/.local/share/Steam/ubuntu12_32/steam".into()],
                },
                ProcInfo {
                    pid: 5269,
                    cmdline: String::new(),
                    args: vec!["./steamwebhelper".into()],
                },
            ],
        };
        assert!(found.is_running());
        assert_eq!(found.summary(), "steam (4697), steamwebhelper (5269)");
        assert_eq!(found.pids(), vec![4697, 5269]);
    }

    #[test]
    fn a_long_list_of_helpers_does_not_scroll_the_refusal() {
        let found = SteamProcesses {
            processes: (0..10)
                .map(|i| ProcInfo {
                    pid: 100 + i,
                    cmdline: String::new(),
                    args: vec!["./steamwebhelper".into()],
                })
                .collect(),
        };
        assert_eq!(
            found.summary(),
            "steamwebhelper (100), steamwebhelper (101), steamwebhelper (102), and 7 more"
        );
    }

    #[test]
    fn nothing_running_reads_as_not_running() {
        assert!(!SteamProcesses::default().is_running());
    }
}
