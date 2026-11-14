//! One scrollback for everything HangarRS runs.
//!
//! # Where the lines come from
//!
//! | Source | Path into the journal |
//! |---|---|
//! | An install | `hangar_install::run::run`'s `OnLine` callback |
//! | A **failed** launch | the log `Launched::Failed` collected |
//! | A **running** launch | [`hangar_runtime::Sink`] |
//!
//! `runner::spawn` detaches its readers on purpose, so without a sink a
//! running app's output is not kept anywhere.
//!
//! # Recording is not a side effect
//!
//! No live/inert injection point: appending to a buffer touches nothing
//! outside the process. The repaint callback defaults to a no-op.
//!
//! # Bounded, and oldest-first
//!
//! Wine chatter and InstallShield logs run to tens of thousands of lines; the
//! oldest go first because the interesting lines are the last ones.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use hangar_ui::{ConsoleLevel, ConsoleLine};

/// How many lines the journal keeps.
pub const LIMIT: usize = 20_000;

/// One recorded line, and what produced it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The app, the installer, or the subsystem this came from.
    pub source: String,
    pub level: ConsoleLevel,
    pub text: String,
    pub at: Instant,
}

/// The shared scrollback. Cheap to clone; every clone is the same buffer.
#[derive(Clone)]
pub struct Journal {
    lines: Arc<Mutex<VecDeque<Entry>>>,
    /// Repaints the UI when a background task writes a line. A no-op by
    /// default.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::new())),
            wake: Arc::new(|| {}),
        }
    }
}

impl Journal {
    /// A journal that repaints the window when something writes to it.
    pub fn with_wake(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
            ..Self::default()
        }
    }

    /// Record one line.
    pub fn push(&self, source: &str, level: ConsoleLevel, text: impl Into<String>) {
        {
            let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
            if lines.len() >= LIMIT {
                lines.pop_front();
            }
            lines.push_back(Entry {
                source: source.to_owned(),
                level,
                text: text.into(),
                at: Instant::now(),
            });
        }
        (self.wake)();
    }

    /// HangarRS narrating what it is about to do.
    pub fn note(&self, source: &str, text: impl Into<String>) {
        self.push(source, ConsoleLevel::Note, text);
    }

    /// A step that completed.
    pub fn done(&self, source: &str, text: impl Into<String>) {
        self.push(source, ConsoleLevel::Done, text);
    }

    /// A recognised failure, called out by name.
    pub fn fail(&self, source: &str, text: impl Into<String>) {
        self.push(source, ConsoleLevel::Fail, text);
    }

    /// A line a child process wrote.
    ///
    /// Levelled by content, not by pipe: Wine writes harmless `fixme:` noise
    /// to stderr on every launch, and colouring it all as an error hides the
    /// lines that matter.
    pub fn output(&self, source: &str, line: &str) {
        self.push(source, level_of(line), line.to_owned());
    }

    /// A sink for [`hangar_runtime::launch_with`] and the installer.
    pub fn sink(&self, source: &str) -> Arc<dyn Fn(&str) + Send + Sync> {
        let journal = self.clone();
        let source = source.to_owned();
        Arc::new(move |line: &str| journal.output(&source, line))
    }

    /// Every distinct source seen, in first-seen order.
    pub fn sources(&self) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen: Vec<String> = Vec::new();
        for entry in lines.iter() {
            if !seen.iter().any(|s| s == &entry.source) {
                seen.push(entry.source.clone());
            }
        }
        seen
    }

    /// When the newest line arrived.
    ///
    /// Separates a console still receiving from the tail of something that
    /// finished long ago.
    pub fn last_at(&self) -> Option<Instant> {
        self.lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .back()
            .map(|e| e.at)
    }

    pub fn len(&self) -> usize {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The lines to draw, filtered.
    ///
    /// The source is prefixed onto the text, not drawn as a column:
    /// `console_view` lays out fixed-height rows so a long install does not
    /// stutter the UI.
    pub fn view(&self, source: Option<&str>, search: &str) -> Vec<ConsoleLine> {
        let needle = search.trim().to_lowercase();
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        lines
            .iter()
            .filter(|e| source.is_none_or(|s| e.source == s))
            .filter(|e| {
                needle.is_empty()
                    || e.text.to_lowercase().contains(&needle)
                    || e.source.to_lowercase().contains(&needle)
            })
            .map(|e| {
                ConsoleLine::new(
                    e.level,
                    if source.is_some() {
                        e.text.clone()
                    } else {
                        format!("[{}] {}", e.source, e.text)
                    },
                )
            })
            .collect()
    }
}

/// What a child process's line should be coloured as.
///
/// Wine's `fixme:`/`err:` chatter is incidental. Only a line that reads like
/// a real failure is called one; the list is short on purpose, because a
/// console that flags everything gets ignored.
fn level_of(line: &str) -> ConsoleLevel {
    let lower = line.to_lowercase();
    if lower.starts_with("fixme:") || lower.starts_with("err:") || lower.starts_with("warn:") {
        return ConsoleLevel::Err;
    }
    const REAL_TROUBLE: &[&str] = &[
        "unhandled page fault",
        "unhandled exception",
        "no such file or directory",
        "permission denied",
        "cannot open display",
        "segmentation fault",
    ];
    if REAL_TROUBLE.iter().any(|needle| lower.contains(needle)) {
        ConsoleLevel::Fail
    } else {
        ConsoleLevel::Out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_kept_with_their_source() {
        let journal = Journal::default();
        journal.note("Project Airports", "starting");
        journal.output("Project Airports", "sync.done");
        journal.output("BeyondATC", "connected");

        assert_eq!(journal.len(), 3);
        assert_eq!(
            journal.sources(),
            vec!["Project Airports".to_string(), "BeyondATC".to_string()]
        );
        assert_eq!(journal.view(Some("BeyondATC"), "").len(), 1);
        assert_eq!(journal.view(None, "").len(), 3);
    }

    /// Filtering to one app must not show a prefix that is now redundant, and
    /// the unfiltered view must show one or a mixed console is unreadable.
    #[test]
    fn the_source_prefix_appears_only_when_it_is_needed() {
        let journal = Journal::default();
        journal.output("BeyondATC", "connected");
        assert_eq!(journal.view(Some("BeyondATC"), "")[0].text, "connected");
        assert_eq!(journal.view(None, "")[0].text, "[BeyondATC] connected");
    }

    #[test]
    fn search_matches_the_text_and_the_source() {
        let journal = Journal::default();
        journal.output("Project Airports", "sync.done");
        journal.output("BeyondATC", "connected");

        assert_eq!(journal.view(None, "sync").len(), 1);
        assert_eq!(journal.view(None, "beyond").len(), 1);
        assert_eq!(journal.view(None, "SYNC").len(), 1, "case-insensitive");
        assert_eq!(journal.view(None, "nothing matches this").len(), 0);
    }

    /// Wine's noise must not be styled as failure: `fixme:` appears on every
    /// launch.
    #[test]
    fn wine_chatter_is_incidental_and_a_real_fault_is_not() {
        assert_eq!(
            level_of("fixme:ntdll:NtQuerySystemInformation stub"),
            ConsoleLevel::Err
        );
        assert_eq!(
            level_of("err:module:import_dll Library not found"),
            ConsoleLevel::Err
        );
        assert_eq!(
            level_of("wine: Unhandled page fault on read access"),
            ConsoleLevel::Fail
        );
        assert_eq!(level_of("Starting up"), ConsoleLevel::Out);
    }

    #[test]
    fn the_buffer_is_bounded_and_the_newest_survive() {
        let journal = Journal::default();
        for n in 0..(LIMIT + 100) {
            journal.output("app", &n.to_string());
        }
        assert_eq!(journal.len(), LIMIT);
        let view = journal.view(Some("app"), "");
        assert_eq!(view.last().unwrap().text, (LIMIT + 99).to_string());
    }

    /// A sink is what makes a *running* app's output visible at all, so it has
    /// to reach the same buffer the UI reads.
    #[test]
    fn a_sink_writes_into_the_same_journal() {
        let journal = Journal::default();
        let sink = journal.sink("Project Airports");
        sink("sync.start");
        sink("sync.done");
        assert_eq!(journal.view(Some("Project Airports"), "").len(), 2);
    }

    #[test]
    fn a_written_line_wakes_the_ui() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let woken = Arc::new(AtomicUsize::new(0));
        let journal = Journal::with_wake({
            let woken = Arc::clone(&woken);
            move || {
                woken.fetch_add(1, Ordering::Relaxed);
            }
        });
        journal.note("x", "one");
        journal.note("x", "two");
        assert_eq!(woken.load(Ordering::Relaxed), 2);
    }

    /// A console showing the tail of something that finished is not a console
    /// still receiving, and only the timestamp separates them.
    #[test]
    fn the_newest_line_carries_when_it_arrived() {
        let journal = Journal::default();
        assert!(journal.last_at().is_none());
        journal.note("x", "one");
        let at = journal.last_at().expect("a line was written");
        assert!(at.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn clearing_empties_it() {
        let journal = Journal::default();
        journal.note("x", "one");
        assert!(!journal.is_empty());
        journal.clear();
        assert!(journal.is_empty());
        assert!(journal.sources().is_empty());
    }
}
