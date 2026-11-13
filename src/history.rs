//! Recent-location history (MRU) and the state-file helpers backends share.

use crate::models::{ActiveLocation, History};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Tracker {
    path: PathBuf,
}

pub fn unix_millis() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
}

pub fn lock_state(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

pub fn save_state(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

impl Tracker {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<History> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StoredHistory {
            Full(History),
            Legacy(HashMap<String, u64>),
        }
        match fs::read_to_string(&self.path) {
            Ok(data) => Ok(
                match serde_json::from_str(&data).context("Invalid navigation history")? {
                    StoredHistory::Full(history) => history,
                    StoredHistory::Legacy(workspaces) => History {
                        workspaces,
                        ..History::default()
                    },
                },
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(History::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn record(&self, location: &ActiveLocation) -> Result<()> {
        self.record_at(
            location,
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64,
        )
    }

    /// Records `location` again only while its pane is the most recent pane, so a
    /// completion that happens in front of the user counts as seen.
    pub fn record_if_newest(&self, location: &ActiveLocation) -> Result<bool> {
        let Some(pane) = &location.pane_id else {
            return Ok(false);
        };
        let _lock = lock_state(&self.path)?;
        let saved = self.load()?;
        let newest = saved.panes.iter().max_by_key(|(_, ts)| **ts);
        if newest.is_none_or(|(id, _)| id != pane) {
            return Ok(false);
        }
        self.record_locked(saved, location, unix_millis()?)?;
        Ok(true)
    }

    fn record_at(&self, location: &ActiveLocation, timestamp: u64) -> Result<()> {
        let _lock = lock_state(&self.path)?;
        self.record_locked(self.load()?, location, timestamp)
    }

    /// Records `location` into `saved`; the caller holds the state lock.
    fn record_locked(
        &self,
        mut saved: History,
        location: &ActiveLocation,
        timestamp: u64,
    ) -> Result<()> {
        let timestamp = saved
            .workspaces
            .values()
            .chain(saved.tabs.values())
            .chain(saved.panes.values())
            .max()
            .map(|last| timestamp.max(last.saturating_add(1)))
            .unwrap_or(timestamp);
        for (map, id) in [
            (&mut saved.workspaces, &location.workspace_id),
            (&mut saved.tabs, &location.tab_id),
            (&mut saved.panes, &location.pane_id),
        ] {
            if let Some(id) = id {
                map.insert(id.clone(), timestamp);
            }
            if map.len() > 300 {
                let mut entries: Vec<_> = map.iter().map(|(id, ts)| (id.clone(), *ts)).collect();
                entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                *map = entries.into_iter().take(300).collect();
            }
        }
        save_state(&self.path, &saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str) -> ActiveLocation {
        ActiveLocation {
            workspace_id: Some(id.into()),
            ..ActiveLocation::default()
        }
    }

    #[test]
    fn legacy_workspace_history_is_preserved_when_tab_and_pane_history_are_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mru.json");
        std::fs::write(&path, r#"{"old-workspace":10}"#).unwrap();
        let tracker = Tracker::new(path);
        tracker
            .record_at(
                &ActiveLocation {
                    workspace_id: Some("new-workspace".into()),
                    tab_id: Some("tab".into()),
                    pane_id: Some("pane".into()),
                },
                20,
            )
            .unwrap();
        let history = tracker.load().unwrap();
        assert_eq!(history.workspaces.get("old-workspace"), Some(&10));
        assert_eq!(history.workspaces.get("new-workspace"), Some(&20));
        assert_eq!(history.tabs.get("tab"), Some(&20));
        assert_eq!(history.panes.get("pane"), Some(&20));
    }

    #[test]
    fn rapid_focus_changes_have_a_total_order_across_location_levels() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(dir.path().join("mru.json"));
        tracker
            .record_at(
                &ActiveLocation {
                    workspace_id: Some("w".into()),
                    tab_id: Some("t1".into()),
                    pane_id: Some("p1".into()),
                },
                10,
            )
            .unwrap();
        tracker
            .record_at(
                &ActiveLocation {
                    workspace_id: Some("w".into()),
                    tab_id: Some("t2".into()),
                    pane_id: Some("p2".into()),
                },
                10,
            )
            .unwrap();
        let history = tracker.load().unwrap();
        assert!(history.panes["p2"] > history.panes["p1"]);
        assert_eq!(history.tabs["t2"], history.panes["p2"]);
        assert_eq!(history.workspaces["w"], history.panes["p2"]);
    }

    #[test]
    fn recording_updates_recency_and_keeps_other_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(dir.path().join("mru.json"));
        tracker.record_at(&workspace("a"), 10).unwrap();
        tracker.record_at(&workspace("b"), 20).unwrap();
        tracker.record_at(&workspace("a"), 30).unwrap();
        assert_eq!(
            tracker.load().unwrap().workspaces,
            HashMap::from([("a".into(), 30), ("b".into(), 20)])
        );
    }

    #[test]
    fn corrupt_state_is_preserved_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mru.json");
        std::fs::write(&path, "corrupt").unwrap();
        let tracker = Tracker::new(path.clone());
        assert!(tracker.record_at(&workspace("a"), 10).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "corrupt");
    }

    #[test]
    fn concurrent_records_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mru.json");
        std::thread::scope(|s| {
            for n in 0..10 {
                let path = path.clone();
                s.spawn(move || {
                    Tracker::new(path)
                        .record_at(&workspace(&format!("w{n}")), n)
                        .unwrap()
                });
            }
        });
        assert_eq!(Tracker::new(path).load().unwrap().workspaces.len(), 10);
    }

    #[test]
    fn bounded_history_retains_most_recent_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(dir.path().join("mru.json"));
        for n in 0..305 {
            tracker.record_at(&workspace(&format!("w{n}")), n).unwrap();
        }
        let saved = tracker.load().unwrap().workspaces;
        assert_eq!(saved.len(), 300);
        assert!(!saved.contains_key("w0"));
        assert_eq!(saved.get("w304"), Some(&304));
    }

    #[test]
    fn only_the_most_recent_pane_is_recorded_again() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(dir.path().join("mru.json"));
        let location = |pane: &str| ActiveLocation {
            workspace_id: Some("w".into()),
            tab_id: Some("t".into()),
            pane_id: Some(pane.into()),
        };
        assert!(!tracker.record_if_newest(&location("a")).unwrap());
        assert_eq!(tracker.load().unwrap(), History::default());
        tracker.record_at(&location("a"), 10).unwrap();
        tracker.record_at(&location("b"), 20).unwrap();
        assert!(!tracker.record_if_newest(&location("a")).unwrap());
        assert_eq!(tracker.load().unwrap().panes["a"], 10);
        assert!(tracker.record_if_newest(&location("b")).unwrap());
        assert!(tracker.load().unwrap().panes["b"] > 20);
    }
}
