use crate::models::{ActiveLocation, AgentActivity, AgentProvider, AgentStatus, History, Snapshot};
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

pub struct WorkingTracker {
    path: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
pub struct WorkingHistory {
    periods: HashMap<String, WorkingPeriod>,
}

#[derive(Deserialize, Serialize)]
struct WorkingPeriod {
    terminal_id: String,
    provider: AgentProvider,
    sequence: u64,
    started_ms: u64,
}

pub fn unix_millis() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
}

fn lock_state(path: &Path) -> Result<File> {
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

fn save_state(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

impl WorkingHistory {
    pub fn is_empty(&self) -> bool {
        self.periods.is_empty()
    }

    pub fn apply(&self, snapshot: &mut Snapshot, agents: &[AgentActivity]) {
        let agents: HashMap<_, _> = agents.iter().map(|a| (a.pane_id.as_str(), a)).collect();
        for pane in snapshot
            .workspaces
            .iter_mut()
            .flat_map(|w| &mut w.tabs)
            .flat_map(|t| &mut t.panes)
        {
            pane.working_since_ms = None;
            let Some(period) = self.periods.get(&pane.id) else {
                continue;
            };
            let Some(agent) = agents.get(pane.id.as_str()) else {
                continue;
            };
            if pane.status == AgentStatus::Working
                && agent.status == AgentStatus::Working
                && agent.state_change_seq > 0
                && period.sequence == agent.state_change_seq
                && period.terminal_id == agent.terminal_id
                && pane.terminal_id.as_deref() == Some(agent.terminal_id.as_str())
                && Some(period.provider) == agent.provider()
                && pane.agent.as_deref().and_then(AgentProvider::from_name) == Some(period.provider)
                && period.provider.supports_timing()
            {
                pane.working_since_ms = Some(period.started_ms);
            }
        }
    }
}

impl WorkingTracker {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn for_session() -> Self {
        Self::new(session_path("working", "json"))
    }

    pub fn load(&self) -> Result<WorkingHistory> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid working-time history"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(WorkingHistory::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn record_event(
        &self,
        event: &serde_json::Value,
        agents: &[AgentActivity],
        timestamp: u64,
    ) -> Result<()> {
        let kind = event.get("event").and_then(|v| v.as_str());
        if !matches!(
            kind,
            Some("pane_agent_status_changed" | "pane_closed" | "pane_exited")
        ) {
            return Ok(());
        }
        let data = &event["data"];
        let pane_id = data["pane_id"]
            .as_str()
            .context("Working-time event is missing pane_id")?;
        let current = if kind == Some("pane_agent_status_changed") {
            let status: AgentStatus = serde_json::from_value(data["agent_status"].clone())?;
            let current = agents.iter().find(|a| a.pane_id == pane_id);
            match current {
                Some(agent)
                    if agent.status != status
                        || agent.provider()
                            != data["agent"].as_str().and_then(AgentProvider::from_name) =>
                {
                    return Ok(());
                }
                None if status == AgentStatus::Working => return Ok(()),
                _ => current,
            }
        } else {
            None
        };
        let _lock = lock_state(&self.path)?;
        let mut saved = self.load()?;
        let previous = saved.periods.get(pane_id);
        if let (Some(previous), Some(current)) = (previous, current)
            && previous.terminal_id == current.terminal_id
            && previous.sequence > current.state_change_seq
        {
            return Ok(());
        }
        if let Some(agent) =
            current.filter(|a| a.status == AgentStatus::Working && a.state_change_seq > 0)
            && let Some(provider) = agent.provider().filter(|p| p.supports_timing())
        {
            if previous.is_some_and(|p| {
                p.terminal_id == agent.terminal_id
                    && p.sequence == agent.state_change_seq
                    && p.provider == provider
            }) {
                return Ok(());
            }
            saved.periods.insert(
                pane_id.into(),
                WorkingPeriod {
                    terminal_id: agent.terminal_id.clone(),
                    provider,
                    sequence: agent.state_change_seq,
                    started_ms: timestamp,
                },
            );
            if saved.periods.len() > 300 {
                let oldest = saved
                    .periods
                    .iter()
                    .min_by_key(|(_, p)| p.started_ms)
                    .map(|(id, _)| id.clone())
                    .unwrap();
                saved.periods.remove(&oldest);
            }
        } else if saved.periods.remove(pane_id).is_none() {
            return Ok(());
        }
        save_state(&self.path, &saved)
    }
}

pub fn session_path(name: &str, extension: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::env::var("HERDR_SOCKET_PATH")
        .unwrap_or_default()
        .hash(&mut hash);
    let root = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
                .unwrap_or_else(std::env::temp_dir)
                .join("herdr-canopy")
        });
    root.join(format!("{name}-{:x}.{extension}", hash.finish()))
}

impl Tracker {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn for_session() -> Self {
        Self::new(session_path("mru", "json"))
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

    fn record_at(&self, location: &ActiveLocation, timestamp: u64) -> Result<()> {
        let _lock = lock_state(&self.path)?;
        let mut saved = self.load()?;
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
    use crate::models::{AgentActivity, AgentStatus};
    use serde_json::json;

    fn activity(sequence: u64, status: AgentStatus) -> AgentActivity {
        AgentActivity {
            pane_id: "named-notes".into(),
            terminal_id: "term-notes".into(),
            agent: Some("claude".into()),
            status,
            state_change_seq: sequence,
        }
    }

    fn work_event(agent: &AgentActivity) -> serde_json::Value {
        json!({"event":"pane_agent_status_changed","data":{
            "pane_id":agent.pane_id,"agent":agent.agent,"agent_status":agent.status
        }})
    }

    fn known_start(tracker: &WorkingTracker, agent: &AgentActivity) -> Option<u64> {
        let mut data = crate::test_helpers::snapshot();
        let pane = &mut data.workspaces[0].tabs[0].panes[0];
        pane.terminal_id = Some(agent.terminal_id.clone());
        pane.agent.clone_from(&agent.agent);
        pane.status = agent.status;
        tracker
            .load()
            .unwrap()
            .apply(&mut data, std::slice::from_ref(agent));
        data.workspaces[0].tabs[0].panes[0].working_since_ms
    }

    #[test]
    fn work_start_survives_reopening_and_duplicate_working_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("working.json");
        let tracker = WorkingTracker::new(path.clone());
        let agent = activity(10, AgentStatus::Working);
        assert_eq!(known_start(&tracker, &agent), None);
        tracker
            .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
            .unwrap();
        WorkingTracker::new(path.clone())
            .record_event(&work_event(&agent), std::slice::from_ref(&agent), 20_000)
            .unwrap();
        assert_eq!(
            known_start(&WorkingTracker::new(path), &agent),
            Some(10_000)
        );
    }

    #[test]
    fn nonworking_states_clear_the_timer_and_a_new_working_period_resets_it() {
        for status in [
            AgentStatus::Idle,
            AgentStatus::Done,
            AgentStatus::Blocked,
            AgentStatus::Unknown,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let tracker = WorkingTracker::new(dir.path().join("working.json"));
            let mut agent = activity(10, AgentStatus::Working);
            tracker
                .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
                .unwrap();
            agent.status = status;
            agent.state_change_seq = 11;
            tracker
                .record_event(&work_event(&agent), std::slice::from_ref(&agent), 20_000)
                .unwrap();
            assert_eq!(known_start(&tracker, &agent), None);
            agent.status = AgentStatus::Working;
            agent.state_change_seq = 12;
            tracker
                .record_event(&work_event(&agent), std::slice::from_ref(&agent), 30_000)
                .unwrap();
            assert_eq!(known_start(&tracker, &agent), Some(30_000));
        }
    }

    #[test]
    fn old_work_records_cannot_time_a_new_terminal_provider_or_working_period() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = WorkingTracker::new(dir.path().join("working.json"));
        let agent = activity(10, AgentStatus::Working);
        tracker
            .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
            .unwrap();
        for changed in [
            AgentActivity {
                terminal_id: "replacement".into(),
                ..agent.clone()
            },
            AgentActivity {
                state_change_seq: 11,
                ..agent.clone()
            },
            AgentActivity {
                agent: Some("codex".into()),
                ..agent.clone()
            },
            AgentActivity {
                status: AgentStatus::Idle,
                ..agent.clone()
            },
        ] {
            assert_eq!(known_start(&tracker, &changed), None);
        }
        let stale = activity(9, AgentStatus::Idle);
        tracker
            .record_event(&work_event(&stale), std::slice::from_ref(&agent), 20_000)
            .unwrap();
        assert_eq!(known_start(&tracker, &agent), Some(10_000));
    }

    #[test]
    fn unsupported_agents_and_missing_transition_ids_do_not_get_timers() {
        for agent in [
            AgentActivity {
                agent: Some("other".into()),
                ..activity(10, AgentStatus::Working)
            },
            AgentActivity {
                agent: None,
                ..activity(10, AgentStatus::Working)
            },
            activity(0, AgentStatus::Working),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let tracker = WorkingTracker::new(dir.path().join("working.json"));
            tracker
                .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
                .unwrap();
            assert_eq!(known_start(&tracker, &agent), None);
        }
    }

    #[test]
    fn closed_or_exited_panes_lose_their_timer() {
        for event in ["pane_closed", "pane_exited"] {
            let dir = tempfile::tempdir().unwrap();
            let tracker = WorkingTracker::new(dir.path().join("working.json"));
            let agent = activity(10, AgentStatus::Working);
            tracker
                .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
                .unwrap();
            tracker
                .record_event(
                    &json!({"event":event,"data":{"pane_id":agent.pane_id}}),
                    &[],
                    20_000,
                )
                .unwrap();
            assert_eq!(known_start(&tracker, &agent), None);
        }
    }

    #[test]
    fn working_history_rejects_inconsistent_pane_snapshots_and_missing_agents() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = WorkingTracker::new(dir.path().join("working.json"));
        let agent = activity(10, AgentStatus::Working);
        tracker
            .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
            .unwrap();
        for mismatch in 0..4 {
            let mut data = crate::test_helpers::snapshot();
            let pane = &mut data.workspaces[0].tabs[0].panes[0];
            pane.terminal_id = Some(agent.terminal_id.clone());
            match mismatch {
                0 => pane.terminal_id = Some("replacement".into()),
                1 => pane.status = AgentStatus::Idle,
                2 => pane.agent = Some("codex".into()),
                _ => {}
            }
            let agents = if mismatch == 3 {
                &[][..]
            } else {
                std::slice::from_ref(&agent)
            };
            tracker.load().unwrap().apply(&mut data, agents);
            assert_eq!(data.workspaces[0].tabs[0].panes[0].working_since_ms, None);
        }
    }

    #[test]
    fn codex_work_events_are_supported_and_unrelated_or_malformed_events_do_not_overwrite_them() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = WorkingTracker::new(dir.path().join("working.json"));
        let mut agent = activity(10, AgentStatus::Working);
        agent.agent = Some("codex".into());
        tracker
            .record_event(&work_event(&agent), std::slice::from_ref(&agent), 10_000)
            .unwrap();
        tracker
            .record_event(&json!({"event":"unrelated"}), &[], 20_000)
            .unwrap();
        let mut wrong_provider = work_event(&agent);
        wrong_provider["data"]["agent"] = json!("claude");
        tracker
            .record_event(&wrong_provider, std::slice::from_ref(&agent), 20_000)
            .unwrap();
        assert!(
            tracker
                .record_event(
                    &json!({"event":"pane_agent_status_changed","data":{}}),
                    &[],
                    20_000
                )
                .is_err()
        );
        assert_eq!(known_start(&tracker, &agent), Some(10_000));
    }

    #[test]
    fn concurrent_work_events_preserve_all_panes_and_corrupt_state_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("working.json");
        std::thread::scope(|s| {
            for n in 0..10 {
                let path = path.clone();
                s.spawn(move || {
                    let mut agent = activity(10, AgentStatus::Working);
                    agent.pane_id = format!("pane-{n}");
                    WorkingTracker::new(path)
                        .record_event(&work_event(&agent), &[agent], 10_000)
                        .unwrap();
                });
            }
        });
        assert_eq!(
            WorkingTracker::new(path.clone())
                .load()
                .unwrap()
                .periods
                .len(),
            10
        );
        fs::write(&path, "invalid").unwrap();
        let agent = activity(10, AgentStatus::Working);
        assert!(
            WorkingTracker::new(path.clone())
                .record_event(&work_event(&agent), &[agent], 20_000)
                .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "invalid");
    }

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
}
