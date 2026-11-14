//! Watching which apps are running, without asking `/proc` sixty times a second.
//!
//! The GUI repaints continuously; a `/proc` scan is a `read_dir` plus a read per
//! process. Polling on a timer in the background and publishing a snapshot keeps
//! the cost fixed and off the UI thread — the same reason the catalog is an
//! `Arc` swap rather than a per-frame rebuild.

use crate::proc::{self, AppProcesses};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How often to look. Fast enough that a launch or a kill shows up promptly,
/// slow enough to be invisible.
pub const POLL: Duration = Duration::from_secs(2);

/// What was running as of the last poll, keyed by match key.
#[derive(Debug, Default)]
pub struct RunningSet {
    apps: BTreeMap<String, AppProcesses>,
}

impl RunningSet {
    pub fn get(&self, key: &str) -> Option<&AppProcesses> {
        self.apps.get(key).filter(|a| a.is_running())
    }

    pub fn is_running(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn running_count(&self) -> usize {
        self.apps.values().filter(|a| a.is_running()).count()
    }
}

/// Polls `/proc` for a set of apps and publishes what it finds.
pub struct AppMonitor {
    keys: watch::Sender<Vec<String>>,
    running: watch::Receiver<Arc<RunningSet>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for AppMonitor {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl AppMonitor {
    /// Start polling. `wake` is called whenever the answer changes.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (keys_tx, keys_rx) = watch::channel(Vec::new());
        let (running_tx, running) = watch::channel(Arc::new(RunningSet::default()));
        let task = tokio::spawn(poll_loop(keys_rx, running_tx, Box::new(wake)));
        Self {
            keys: keys_tx,
            running,
            task: Some(task),
        }
    }

    /// A fixed snapshot for renders and tests; never scans processes.
    pub fn inert(apps: BTreeMap<String, AppProcesses>) -> Self {
        let (keys, _) = watch::channel(Vec::new());
        let (_, running) = watch::channel(Arc::new(RunningSet { apps }));
        Self {
            keys,
            running,
            task: None,
        }
    }

    /// Replace the set of apps being watched.
    pub fn watch(&self, keys: Vec<String>) {
        let _ = self.keys.send(keys);
    }

    /// The most recent snapshot.
    pub fn running(&self) -> Arc<RunningSet> {
        self.running.borrow().clone()
    }
}

async fn poll_loop(
    mut keys: watch::Receiver<Vec<String>>,
    running: watch::Sender<Arc<RunningSet>>,
    wake: Box<dyn Fn() + Send + Sync>,
) {
    let mut ticker = tokio::time::interval(POLL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let needles = keys.borrow().clone();
        let apps = if needles.is_empty() {
            BTreeMap::new()
        } else {
            // The scan is blocking file I/O; keeping it off the async worker
            // matters once the poll runs alongside a telemetry stream.
            tokio::task::spawn_blocking(move || proc::find_apps(&needles))
                .await
                .unwrap_or_default()
        };

        let next = RunningSet { apps };
        // Only publish when something actually changed, so a UI woken by this
        // is woken for a reason.
        let changed = {
            let current = running.borrow();
            current.running_count() != next.running_count()
                || current.apps.keys().ne(next.apps.keys())
                || current
                    .apps
                    .iter()
                    .any(|(k, v)| next.apps.get(k).map(|n| n.main_pid()) != Some(v.main_pid()))
        };
        if changed {
            let _ = running.send(Arc::new(next));
            wake();
        }

        tokio::select! {
            _ = ticker.tick() => {}
            // A change to the watch list takes effect at once rather than
            // waiting out the poll interval.
            Ok(()) = keys.changed() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fake_app(dir: &Path) -> PathBuf {
        let exe = dir.join(format!("hangar-monitor-app-{}.exe", std::process::id()));
        let target = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .unwrap();
        std::os::unix::fs::symlink(target, &exe).unwrap();
        exe
    }

    #[test]
    fn an_empty_set_reports_nothing_running() {
        let set = RunningSet::default();
        assert!(!set.is_running("anything"));
        assert_eq!(set.running_count(), 0);
        assert!(set.get("anything").is_none());
    }

    #[tokio::test]
    async fn the_monitor_notices_a_process_appear_and_disappear() {
        let dir = std::env::temp_dir().join(format!("hangar-monitor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = fake_app(&dir);
        let key = proc::match_key(&exe);

        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let monitor = AppMonitor::spawn(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        monitor.watch(vec![key.clone()]);

        // Nothing running yet.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!monitor.running().is_running(&key));

        let mut child = std::process::Command::new(&exe).arg("60").spawn().unwrap();

        // The poll interval is two seconds; give it room.
        let mut seen = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if monitor.running().is_running(&key) {
                seen = true;
                break;
            }
        }
        assert!(seen, "the monitor should have noticed it start");
        assert_eq!(
            monitor.running().get(&key).unwrap().main_pid(),
            Some(child.id())
        );
        assert!(wakes.load(Ordering::Relaxed) > 0, "a change wakes the UI");

        let _ = child.kill();
        let _ = child.wait();

        let mut gone = false;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if !monitor.running().is_running(&key) {
                gone = true;
                break;
            }
        }
        assert!(gone, "and noticed it stop");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing changing must not keep waking the UI.
    #[tokio::test]
    async fn a_quiet_system_does_not_wake_the_ui_repeatedly() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let monitor = AppMonitor::spawn(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        monitor.watch(vec!["hangar-nothing-like-this-exists".into()]);

        tokio::time::sleep(Duration::from_millis(600)).await;
        let early = wakes.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(5)).await;
        let late = wakes.load(Ordering::Relaxed);

        assert!(
            late <= early + 1,
            "polls with no change should not wake the UI ({early} then {late})"
        );
    }
}
