//! Working periods recorded from Herdr agent events, shown as Claude/Codex timers.

use crate::history::{lock_state, save_state};
use crate::models::{AgentActivity, AgentProvider, AgentStatus, Snapshot};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

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
        Self::new(super::session_path("working", "json"))
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
