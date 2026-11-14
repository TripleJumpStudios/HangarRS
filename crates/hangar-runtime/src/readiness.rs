//! Is an app finished doing its startup work?
//!
//! Some companion apps have to run **before** the sim because they prepare
//! content it reads at startup. Project Airports syncs a scenery database; the
//! sim only picks up what was synced before it loaded. So the useful question
//! is not "did the app start" but "has it finished".
//!
//! # It is knowable, because the app says so
//!
//! Project Airports writes newline-delimited JSON to
//! `…/@project-airports/desktop/logs/companion-<date>.log`:
//!
//! ```text
//! {"time":"…","level":"info","message":"sync.start","data":{"reason":"manual"}}
//! {"time":"…","level":"info","message":"sync.shared_plan","data":{"install":1,"update":48,"downloadBytes":4428154}}
//! {"time":"…","level":"info","message":"sync.done","data":{"installedCount":893,"installed":18,"updated":55,"failed":0,"outcome":"ok"}}
//! ```
//!
//! That is a start marker, a plan worth showing someone, and a terminal event.
//!
//! # Two things the real logs insist on
//!
//! **Only read what happened after launch.** The log persists across runs, so a
//! `sync.done` from yesterday would satisfy the gate instantly. A [`Watch`]
//! records the file's length when the app is started and reads only past it.
//!
//! **A sync does not always finish.** Across the shipped history there are 90
//! `sync.start` lines and 74 `sync.done` — the rest are crashes, respawns and
//! apps closed mid-sync. So a probe reports what it sees and never becomes the
//! only way forward: whatever waits on this keeps a manual override.

use hangar_core::Utc;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long to wait for a sync to appear before moving on and starting the sim.
///
/// This is **a person's decision window**, and it is measured from the moment
/// the app shows signs of life — not from when it was launched.
///
/// That distinction is the whole point. Electron under Wine takes the better
/// part of twenty seconds to come up, and a window that starts at launch is
/// mostly spent watching a splash screen, so it has expired by the time anyone
/// can press Sync.
pub const SYNC_GRACE: Duration = Duration::from_secs(30);

/// How long to wait for the app to write *anything* before giving up on it.
///
/// A window anchored to signs of life needs a floor for the case where none
/// come: a crash on startup, a prefix problem, an app that simply does not log.
/// Without this the gate would wait for ever on a process that is never going
/// to speak.
pub const BOOT_TIMEOUT: Duration = Duration::from_secs(120);

/// What an app is doing, as far as its own log says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// Nothing is known — no probe, or no log yet.
    Unknown,
    /// Started, and still inside the window where a sync might begin.
    Starting { grace_left: Duration },
    /// Working, with whatever the log says about it.
    Working { detail: String },
    /// Finished.
    Ready { detail: String },
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Readiness::Ready { .. })
    }

    /// A line for the UI.
    pub fn describe(&self) -> String {
        match self {
            Readiness::Unknown => "no way to tell when this is done".to_string(),
            Readiness::Starting { grace_left } => format!(
                "no sync yet \u{2014} moving on in {}s if none starts",
                grace_left.as_secs()
            ),
            Readiness::Working { detail } | Readiness::Ready { detail } => detail.clone(),
        }
    }
}

/// How to read one app's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// Matched against an app's name, case-insensitively, as a substring.
    pub matches: &'static str,
    /// Where the logs are, relative to `drive_c/users/steamuser/AppData/Roaming`
    /// inside the prefix.
    pub log_dir: &'static str,
    /// Log file names start with this; the newest by modification time wins.
    pub log_prefix: &'static str,
    /// The message that means work has begun.
    pub start: &'static str,
    /// Messages that mean the work is over, however it went.
    pub done: &'static [&'static str],
    /// Messages carrying a plan worth showing while it runs.
    pub plan: &'static [&'static str],
    /// The message that means the results are actually on disk.
    ///
    /// Project Airports logs `sync.done` and then writes the scenery groups,
    /// emitting `sync.groups_finalized` up to thirteen seconds later. Treating
    /// `sync.done` as the end starts the sim while the content it just fetched
    /// is still being written.
    ///
    /// Only expected when the sync actually changed something: across the whole
    /// shipped history, **74 of 74** syncs that installed, updated or removed
    /// anything were followed by it, and the two that changed nothing were not.
    pub finalize: &'static str,
}

/// The probes shipped with HangarRS.
///
/// A table rather than a guess, and deliberately small.
pub const PROBES: &[Probe] = &[Probe {
    matches: "project airports",
    log_dir: "@project-airports/desktop/logs",
    log_prefix: "companion-",
    start: "sync.start",
    done: &["sync.done", "sync.skip_unchanged"],
    plan: &["sync.plan", "sync.shared_plan"],
    finalize: "sync.groups_finalized",
}];

/// The probe for an app, if one is shipped.
pub fn probe_for(app_name: &str) -> Option<&'static Probe> {
    let lowered = app_name.to_ascii_lowercase();
    PROBES.iter().find(|p| lowered.contains(p.matches))
}

/// One app being watched from the moment it was started.
#[derive(Debug, Clone)]
pub struct Watch {
    pub label: String,
    probe: &'static Probe,
    /// Where the logs live, so a log that did not exist at launch is still found.
    dir: PathBuf,
    log: Option<PathBuf>,
    /// Where the log had got to when the app was launched. Everything before
    /// this belongs to a previous run.
    from: u64,
    /// When watching began, for the boot timeout.
    since: Instant,
    /// When the app first wrote anything after launch.
    ///
    /// The grace window runs from here, not from `since` — see [`SYNC_GRACE`].
    /// `Cell` because polling is a read as far as callers are concerned.
    alive_since: std::cell::Cell<Option<Instant>>,
    /// The last completed sync **before** launch, if the log records one.
    ///
    /// Answers "is this out of date?" without waiting for anything.
    pub last_sync: Option<Utc>,
}

impl Watch {
    /// Start watching. `roaming` is the prefix's
    /// `drive_c/users/steamuser/AppData/Roaming`.
    pub fn begin(label: impl Into<String>, probe: &'static Probe, roaming: &Path) -> Self {
        let dir = roaming.join(probe.log_dir);
        let log = newest_log(&dir, probe.log_prefix);
        let from = log
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        let last_sync = log.as_ref().and_then(|p| last_sync_in(p, probe));

        Watch {
            label: label.into(),
            probe,
            dir,
            log,
            from,
            since: Instant::now(),
            alive_since: std::cell::Cell::new(None),
            last_sync,
        }
    }

    /// How long since the app last finished a sync, before this launch.
    pub fn since_last_sync(&self) -> Option<Duration> {
        self.last_sync.and_then(|t| t.ago())
    }

    /// Read what has been written since the app started.
    pub fn poll(&self) -> Readiness {
        // The app may open a *new* log file after starting — the name carries
        // the date, so a launch just before midnight UTC rolls over. Re-finding
        // the newest each time costs a directory read and avoids watching a
        // file nothing writes to any more.
        let Some(path) = self.current_log() else {
            return self.nothing_yet();
        };
        let Ok(text) = read_from(&path, self.offset_for(&path)) else {
            return self.nothing_yet();
        };
        if text.trim().is_empty() {
            return self.nothing_yet();
        }

        // Anything at all means the app is up, which is when the person's
        // window starts.
        if self.alive_since.get().is_none() {
            self.alive_since.set(Some(Instant::now()));
        }

        let mut syncing: Option<String> = None;
        let mut finished: Option<Finished> = None;

        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Entry>(line) else {
                continue;
            };
            let message = entry.message.as_str();

            if message == self.probe.start {
                // A sync has begun, so the window no longer applies: from here
                // it waits for the sync itself, however long that takes. The
                // observed range runs to gigabytes.
                syncing = Some("syncing".to_string());
                finished = None;
            } else if self.probe.plan.contains(&message) {
                if syncing.is_some() {
                    syncing = Some(describe_plan(&entry));
                }
            } else if self.probe.done.contains(&message) {
                finished = Some(Finished {
                    detail: describe_done(&entry),
                    // Only a sync that changed something goes on to write the
                    // scenery groups; one that changed nothing is over here.
                    awaits_finalize: changed_something(&entry),
                });
            } else if message == self.probe.finalize {
                if let Some(done) = &mut finished {
                    done.awaits_finalize = false;
                }
            }
        }

        match finished {
            Some(done) if !done.awaits_finalize => Readiness::Ready {
                detail: done.detail,
            },
            Some(done) => Readiness::Working {
                detail: format!("{} \u{2014} writing scenery groups", done.detail),
            },
            None => match syncing {
                Some(detail) => Readiness::Working { detail },
                None => self.nothing_yet(),
            },
        }
    }

    /// No sync has been seen. Inside the grace window that is `Starting`;
    /// past it, the app is taken to be up to date and the sequence moves on.
    ///
    /// **This is the difference between a gate and a hang.** The app syncs at
    /// its own cadence — `catch-up` on launch, `nightly`, `build-complete` when
    /// the server pushes — and it is perfectly normal for none to fire because
    /// there is nothing to do. Waiting for a `sync.start` that is never coming
    /// would stall the preflight indefinitely.
    fn nothing_yet(&self) -> Readiness {
        // Nothing has been written at all: the app is still coming up. Only
        // the boot timeout applies here, not the person's window.
        let Some(alive) = self.alive_since.get() else {
            return if self.since.elapsed() >= BOOT_TIMEOUT {
                Readiness::Ready {
                    detail: "no sign of life \u{2014} moving on".to_string(),
                }
            } else {
                Readiness::Starting {
                    grace_left: SYNC_GRACE,
                }
            };
        };

        let elapsed = alive.elapsed();
        if elapsed >= SYNC_GRACE {
            let detail = match self.since_last_sync() {
                Some(ago) => format!("no sync started \u{2014} last one {}", describe_ago(ago)),
                None => "no sync started".to_string(),
            };
            return Readiness::Ready { detail };
        }
        Readiness::Starting {
            grace_left: SYNC_GRACE - elapsed,
        }
    }

    fn current_log(&self) -> Option<PathBuf> {
        newest_log(&self.dir, self.probe.log_prefix)
    }

    /// The app exited and is expected back — a self-update.
    ///
    /// Everything written so far belongs to the instance that left, so it
    /// cannot open the gate, and both clocks start over. Otherwise the grace
    /// window keeps running from the old instance's first line, can expire
    /// during the restart before the new instance begins its sync, and the sim
    /// is started into it.
    pub fn rearm(&mut self) {
        if let Some(path) = self.current_log() {
            self.from = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.log = Some(path);
        }
        self.restart_clock();
    }

    /// The restarted instance has appeared: its boot window starts now.
    ///
    /// The log offset is left where [`Watch::rearm`] put it, so nothing the new
    /// instance wrote before it was noticed is lost.
    pub fn restart_clock(&mut self) {
        self.alive_since.set(None);
        self.since = Instant::now();
    }

    /// The offset only applies to the file that existed at launch. A log that
    /// rolled over since is entirely ours, so it is read from the beginning.
    fn offset_for(&self, path: &Path) -> u64 {
        match &self.log {
            Some(original) if original == path => self.from,
            _ => 0,
        }
    }
}

/// A finished sync, and whether its results are on disk yet.
struct Finished {
    detail: String,
    awaits_finalize: bool,
}

/// Did this sync change anything? Only then are the groups rewritten.
fn changed_something(entry: &Entry) -> bool {
    ["installed", "updated", "removed"]
        .iter()
        .filter_map(|k| entry.data.get(*k).and_then(|v| v.as_u64()))
        .sum::<u64>()
        > 0
}

/// One log line, of the parts that matter.
#[derive(Debug, Deserialize)]
struct Entry {
    message: String,
    #[serde(default)]
    time: String,
    #[serde(default)]
    data: serde_json::Value,
}

fn describe_plan(entry: &Entry) -> String {
    let get = |key: &str| entry.data.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let (install, update, remove) = (get("install"), get("update"), get("remove"));
    let bytes = get("downloadBytes");

    let mut parts = Vec::new();
    if install > 0 {
        parts.push(format!("{install} to install"));
    }
    if update > 0 {
        parts.push(format!("{update} to update"));
    }
    if remove > 0 {
        parts.push(format!("{remove} to remove"));
    }
    if parts.is_empty() {
        return "syncing".to_string();
    }
    if bytes > 0 {
        parts.push(format!("{:.0} MB to download", bytes as f64 / 1_048_576.0));
    }
    format!("syncing \u{2014} {}", parts.join(", "))
}

fn describe_done(entry: &Entry) -> String {
    let get = |key: &str| entry.data.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let (installed, updated, removed, failed) = (
        get("installed"),
        get("updated"),
        get("removed"),
        get("failed"),
    );

    if installed + updated + removed + failed == 0 {
        return "up to date".to_string();
    }
    let mut parts = Vec::new();
    if installed > 0 {
        parts.push(format!("{installed} installed"));
    }
    if updated > 0 {
        parts.push(format!("{updated} updated"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    if failed > 0 {
        parts.push(format!("{failed} FAILED"));
    }
    format!("done \u{2014} {}", parts.join(", "))
}

/// When the log last recorded a finished sync, wherever in the file.
fn last_sync_in(path: &Path, probe: &Probe) -> Option<Utc> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .find(|entry| probe.done.contains(&entry.message.as_str()))
        .and_then(|entry| Utc::parse_iso8601(&entry.time))
}

/// `2 hours ago`, `just now`.
fn describe_ago(ago: Duration) -> String {
    let seconds = ago.as_secs();
    match seconds {
        0..=90 => "just now".to_string(),
        91..=5400 => format!("{} minutes ago", seconds / 60),
        5401..=172_800 => format!("{} hours ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// The most recently modified log in `dir` whose name starts with `prefix`.
fn newest_log(dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let is_match = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(prefix));
        if !is_match {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// Read a file from a byte offset.
///
/// A log that is *shorter* than the offset was rotated or truncated, so it is
/// read whole rather than not at all.
fn read_from(path: &Path, from: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if from > 0 && from <= len {
        file.seek(SeekFrom::Start(from))?;
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines copied from the real log.
    const START: &str = r#"{"time":"2026-08-25T03:46:41.260Z","level":"info","scope":"main","message":"sync.start","data":{"reason":"manual","deepVerify":false}}"#;
    const PLAN: &str = r#"{"time":"2026-08-25T03:46:41.821Z","level":"info","scope":"main","message":"sync.shared_plan","data":{"install":1,"update":48,"remove":2,"downloadBytes":4428154}}"#;
    const DONE: &str = r#"{"time":"2026-08-25T03:46:25.053Z","level":"info","scope":"main","message":"sync.done","data":{"installedCount":893,"installed":18,"updated":55,"removed":24,"failed":0,"outcome":"ok"}}"#;
    const SKIP: &str = r#"{"time":"2026-08-25T03:46:42.825Z","level":"info","scope":"main","message":"sync.skip_unchanged"}"#;
    const FINAL: &str = r#"{"time":"2026-08-25T04:32:37.502Z","level":"info","scope":"main","message":"sync.groups_finalized","data":{"groups":["projectairports-airports"]}}"#;
    const DONE_IDLE: &str = r#"{"time":"2026-08-25T03:55:56.614Z","level":"info","scope":"main","message":"sync.done","data":{"installedCount":893,"installed":0,"updated":0,"removed":0,"failed":0,"outcome":"ok"}}"#;
    const NOISE: &str = r#"{"time":"2026-08-25T03:46:24.186Z","level":"debug","scope":"main","message":"discord.connect_failed","data":{"error":"Error: connect ENOENT"}}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hangar-readiness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("@project-airports/desktop/logs")).unwrap();
        dir
    }

    fn write_log(roaming: &Path, lines: &[&str]) {
        let path = roaming.join("@project-airports/desktop/logs/companion-2026-08-25.log");
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    fn probe() -> &'static Probe {
        probe_for("Project Airports").expect("a shipped probe")
    }

    #[test]
    fn an_app_with_no_probe_is_simply_unknown() {
        assert!(probe_for("BeyondATC").is_none());
        assert!(probe_for("Pilot Client").is_none());
        assert!(probe_for("Project Airports").is_some());
        // Matched case-insensitively, since the catalogue's title casing is
        // not guaranteed.
        assert!(probe_for("PROJECT AIRPORTS.exe").is_some());
    }

    #[test]
    fn a_sync_that_has_started_reads_as_working() {
        let roaming = scratch("working");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        write_log(&roaming, &[NOISE, START, PLAN]);
        match watch.poll() {
            Readiness::Working { detail } => {
                assert!(detail.contains("48 to update"), "{detail}");
                assert!(detail.contains("MB to download"), "{detail}");
            }
            other => panic!("expected working, got {other:?}"),
        }

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_sync_that_changed_something_waits_for_the_groups_to_be_written() {
        // `sync.done` is not the end. The app then writes the scenery groups,
        // and logged `sync.groups_finalized` up to thirteen seconds later.
        // Starting the sim in that gap loads content mid-write.
        let roaming = scratch("finalize");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        write_log(&roaming, &[NOISE, START, PLAN, DONE]);
        let mid = watch.poll();
        assert!(!mid.is_ready(), "done is not finished: {mid:?}");
        assert!(mid.describe().contains("writing scenery groups"), "{mid:?}");

        write_log(&roaming, &[NOISE, START, PLAN, DONE, FINAL]);
        let end = watch.poll();
        assert!(end.is_ready(), "{end:?}");
        assert!(end.describe().contains("55 updated"), "{end:?}");

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_sync_that_changed_nothing_is_over_at_done() {
        // 74 of 74 syncs that changed something were followed by
        // groups_finalized; the two that changed nothing were not. Waiting for
        // one that is never coming would hang the gate.
        let roaming = scratch("idle-done");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        write_log(&roaming, &[NOISE, START, DONE_IDLE]);
        assert!(
            watch.poll().is_ready(),
            "nothing changed, so nothing to write"
        );

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn nothing_to_do_still_counts_as_finished() {
        let roaming = scratch("skip");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        write_log(&roaming, &[NOISE, START, SKIP]);
        assert!(watch.poll().is_ready());

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn yesterdays_sync_does_not_satisfy_todays_gate() {
        // The whole reason an offset is recorded. Without it the log's history
        // makes the app look finished the instant it starts.
        let roaming = scratch("offset");
        write_log(&roaming, &[START, PLAN, DONE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        assert!(
            matches!(watch.poll(), Readiness::Starting { .. }),
            "a completed sync from before launch must not count"
        );
        // But it is still reported, because "last synced 2 hours ago" is what
        // answers "is this out of date?".
        assert!(watch.last_sync.is_some());

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn the_grace_window_is_a_human_decision_window() {
        // Long enough to notice and press Sync, short enough not to be a tax on
        // every launch. Both halves are the point.
        assert!(SYNC_GRACE >= Duration::from_secs(15));
        assert!(SYNC_GRACE <= Duration::from_secs(45));
        // And a floor for an app that never says anything at all.
        assert!(BOOT_TIMEOUT > SYNC_GRACE);
    }

    #[test]
    fn the_window_starts_when_the_app_speaks_not_when_it_was_launched() {
        // The bug this fixes: Electron under Wine takes ~20s to come up, so a
        // window measured from launch is mostly spent on a splash screen and
        // has expired by the time anyone can press Sync.
        let roaming = scratch("anchor");
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        // No log at all yet: the full window is still ahead.
        match watch.poll() {
            Readiness::Starting { grace_left } => assert_eq!(grace_left, SYNC_GRACE),
            other => panic!("expected the full window, got {other:?}"),
        }

        // The app speaks. Only now does the clock start.
        write_log(&roaming, &[NOISE]);
        match watch.poll() {
            Readiness::Starting { grace_left } => {
                assert!(
                    grace_left > SYNC_GRACE - Duration::from_secs(2),
                    "{grace_left:?}"
                );
            }
            other => panic!("expected the window to have just started, got {other:?}"),
        }

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_grace_window_is_reported_while_it_lasts() {
        // The window is the person's chance to start a sync by hand, so it has
        // to say how much of it is left.
        let roaming = scratch("grace");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        match watch.poll() {
            Readiness::Starting { grace_left } => {
                assert!(grace_left <= SYNC_GRACE);
                assert!(grace_left > SYNC_GRACE / 2, "{grace_left:?}");
            }
            other => panic!("expected to be in the grace window, got {other:?}"),
        }
        assert!(watch.poll().describe().contains("moving on in"));

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn the_last_sync_before_launch_is_read_so_staleness_can_be_shown() {
        let roaming = scratch("laststamp");
        write_log(&roaming, &[NOISE, START, DONE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        let last = watch.last_sync.expect("a recorded sync");
        assert_eq!(last.stamp(), "2026-08-25T03-46-25Z");

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn how_long_ago_reads_like_a_person_would_say_it() {
        assert_eq!(describe_ago(Duration::from_secs(5)), "just now");
        assert_eq!(describe_ago(Duration::from_secs(600)), "10 minutes ago");
        assert_eq!(describe_ago(Duration::from_secs(7200)), "2 hours ago");
        assert_eq!(describe_ago(Duration::from_secs(300_000)), "3 days ago");
    }

    #[test]
    fn a_second_sync_after_a_finished_one_reads_as_working_again() {
        // Real behaviour: the app syncs on start and again on demand.
        let roaming = scratch("second");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        write_log(&roaming, &[NOISE, START, DONE, FINAL, START, PLAN]);
        assert!(matches!(watch.poll(), Readiness::Working { .. }));

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_restart_discards_the_old_instance_and_starts_the_window_again() {
        // The instance that exits for an update said things (and may have
        // begun a sync); none of it speaks for the instance that returns.
        let roaming = scratch("rearm");
        write_log(&roaming, &[NOISE]);
        let mut watch = Watch::begin("Project Airports", probe(), &roaming);
        write_log(&roaming, &[NOISE, NOISE, START, PLAN]);
        assert!(matches!(watch.poll(), Readiness::Working { .. }));

        watch.rearm();
        match watch.poll() {
            Readiness::Starting { grace_left } => assert_eq!(grace_left, SYNC_GRACE),
            other => panic!("the old sync must not count: {other:?}"),
        }
        watch.restart_clock();
        write_log(&roaming, &[NOISE, NOISE, START, PLAN, NOISE, START, DONE]);
        let mid = watch.poll();
        assert!(
            !mid.is_ready() && mid.describe().contains("writing scenery groups"),
            "{mid:?}"
        );
        write_log(
            &roaming,
            &[NOISE, NOISE, START, PLAN, NOISE, START, DONE, FINAL],
        );
        assert!(watch.poll().is_ready());

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_log_created_after_launch_is_still_found_after_a_restart() {
        let roaming = scratch("late-log");
        let mut watch = Watch::begin("Project Airports", probe(), &roaming);
        write_log(&roaming, &[NOISE]);
        watch.rearm();
        write_log(&roaming, &[NOISE, START, DONE_IDLE]);
        assert!(watch.poll().is_ready());
        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_missing_log_is_starting_rather_than_ready() {
        // Never report ready on absence of evidence — that would launch the sim
        // into a half-synced scenery library.
        let roaming = scratch("missing");
        let watch = Watch::begin("Project Airports", probe(), &roaming);
        assert!(matches!(watch.poll(), Readiness::Starting { .. }));
        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn a_truncated_log_is_read_whole_rather_than_skipped() {
        let roaming = scratch("truncated");
        write_log(&roaming, &[NOISE, NOISE, NOISE, NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        // Rotated: shorter than the recorded offset.
        write_log(&roaming, &[START, DONE, FINAL]);
        assert!(watch.poll().is_ready());

        std::fs::remove_dir_all(&roaming).unwrap();
    }

    #[test]
    fn unparseable_lines_are_stepped_over() {
        let roaming = scratch("garbage");
        write_log(&roaming, &[NOISE]);
        let watch = Watch::begin("Project Airports", probe(), &roaming);

        write_log(
            &roaming,
            &[NOISE, "not json at all", START, "{oops", DONE, FINAL],
        );
        assert!(watch.poll().is_ready());

        std::fs::remove_dir_all(&roaming).unwrap();
    }
}
