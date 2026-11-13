//! Herdr backend: the plugin entrypoints and `HerdrHost`.

pub mod ipc;
pub mod working;

use crate::cli::{Cli, Command};
use crate::history::{Tracker, unix_millis};
use crate::host::{FocusOutcome, Host};
use crate::models::{ActiveLocation, FocusTarget, Keybindings, Snapshot, View};
use crate::navigator;
use crate::popup;
use crate::theme::Palette;
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

const PLUGIN_ID: &str = "serhez.herdr-canopy";

/// Navigator data, focus and settings from the Herdr session this plugin runs in.
struct HerdrHost {
    history: Tracker,
}

impl Host for HerdrHost {
    fn snapshot(&self) -> Result<Snapshot> {
        ipc::fetch_snapshot()
    }

    fn current(&self, snapshot: &Snapshot) -> ActiveLocation {
        let context: Option<serde_json::Value> = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        snapshot.active_location(context.as_ref())
    }

    fn focus(&self, target: &FocusTarget) -> Result<FocusOutcome> {
        ipc::focus(target)?;
        Ok(FocusOutcome::Done)
    }

    fn history(&self) -> &Tracker {
        &self.history
    }

    fn keybindings(&self) -> Result<Keybindings> {
        load_keybindings()
    }

    fn palette(&self) -> Palette {
        Palette::from_config(
            &herdr_config_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default(),
        )
    }
}

/// Runs a Herdr plugin command or the navigator.
pub fn run(cli: Cli) -> Result<()> {
    if matches!(cli.command, Some(Command::TrackWork)) {
        let received_ms = unix_millis()?;
        let event: serde_json::Value = serde_json::from_str(
            &std::env::var("HERDR_PLUGIN_EVENT_JSON")
                .context("HERDR_PLUGIN_EVENT_JSON is missing")?,
        )?;
        let agents = if event["event"] == "pane_agent_status_changed" {
            ipc::fetch_agent_activity()?
        } else {
            Vec::new()
        };
        return working::WorkingTracker::for_session().record_event(&event, &agents, received_ms);
    }
    if matches!(cli.command, Some(Command::Track)) {
        let event = std::env::var("HERDR_PLUGIN_EVENT_JSON")
            .context("HERDR_PLUGIN_EVENT_JSON is missing")?;
        return track_event(
            &mru_tracker(),
            &event,
            ipc::fetch_focused_location().ok().as_ref(),
        );
    }
    anyhow::ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1") || mock_enabled(&cli),
        "Run this plugin inside Herdr, or use --mock to preview it"
    );
    if cli.pane_open {
        return popup::open(
            &session_path("popup", "state"),
            cli.view.entrypoint(),
            || open_navigator(cli.view),
        )
        .map(|_| ());
    }
    if matches!(cli.command, Some(Command::Toggle)) {
        return popup::toggle(
            &session_path("popup", "state"),
            cli.view.entrypoint(),
            || open_navigator(cli.view),
        )
        .map(|_| ());
    }

    let popup =
        if std::env::var("HERDR_PLUGIN_ENTRYPOINT_ID").as_deref() == Ok(cli.view.entrypoint()) {
            Some(popup::PopupState::claim(
                &session_path("popup", "state"),
                cli.view.entrypoint(),
            )?)
        } else {
            None
        };

    let host = Arc::new(HerdrHost {
        history: mru_tracker(),
    });
    let mock = mock_enabled(&cli);
    let snapshot = if mock {
        sample_snapshot()?
    } else {
        host.snapshot()?
    };
    navigator::run(host, snapshot, cli.view, popup.as_ref(), !mock)
}

fn mru_tracker() -> Tracker {
    Tracker::new(session_path("mru", "json"))
}

fn session_path(name: &str, extension: &str) -> PathBuf {
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

fn herdr_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HERDR_CONFIG_PATH")
        && !path.is_empty()
    {
        return Some(path.into());
    }
    if let Ok(path) = std::env::var("XDG_CONFIG_HOME")
        && !path.is_empty()
    {
        return Some(PathBuf::from(path).join("herdr/config.toml"));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".config/herdr/config.toml"))
}

fn mock_enabled(cli: &Cli) -> bool {
    #[cfg(feature = "mock")]
    {
        cli.mock
    }
    #[cfg(not(feature = "mock"))]
    {
        let _ = cli;
        false
    }
}

fn open_navigator(view: View) -> Result<()> {
    let plugin_id = std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.into());
    ipc::cli_request(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        &plugin_id,
        "--entrypoint",
        view.entrypoint(),
        "--focus",
    ])?;
    Ok(())
}

fn track_event(tracker: &Tracker, text: &str, live: Option<&ActiveLocation>) -> Result<()> {
    let event: serde_json::Value = serde_json::from_str(text)?;
    let kind = event.get("event").and_then(|v| v.as_str());
    if !matches!(
        kind,
        Some("workspace_focused" | "tab_focused" | "pane_focused")
    ) {
        return Ok(());
    }
    let data = event.get("data").context("Focus event is missing data")?;
    let field = |key: &str| {
        data.get(key)
            .and_then(|v| v.as_str())
            .map(String::from)
            .with_context(|| format!("Focus event is missing {key}"))
    };
    let mut location = ActiveLocation {
        workspace_id: Some(field("workspace_id")?),
        ..ActiveLocation::default()
    };
    match kind {
        Some("tab_focused") => location.tab_id = Some(field("tab_id")?),
        Some("pane_focused") => location.pane_id = Some(field("pane_id")?),
        _ => {}
    }
    if let Some(live) = live {
        location.complete_from(live);
    }
    tracker.record(&location)
}

fn load_keybindings() -> Result<Keybindings> {
    match std::env::var_os("HERDR_PLUGIN_CONFIG_DIR") {
        Some(dir) => Keybindings::load(&PathBuf::from(dir).join("config.toml")),
        None => Ok(Keybindings::default()),
    }
}

#[cfg(any(feature = "mock", test))]
fn sample_snapshot() -> Result<Snapshot> {
    use serde_json::json;
    ipc::decode_snapshot(
        json!({"workspaces":[
            {"workspace_id":"project","label":"Project","worktree":{"is_linked_worktree":false,"repo_key":"repo","repo_name":"project"}},
            {"workspace_id":"feature","label":"feature","worktree":{"is_linked_worktree":true,"repo_key":"repo","repo_name":"project"}},
            {"workspace_id":"notes","label":"Notes"}
        ]}),
        json!({"tabs":[
            {"workspace_id":"project","tab_id":"editor","label":"Editor"},
            {"workspace_id":"project","tab_id":"tests","label":"Tests"},
            {"workspace_id":"feature","tab_id":"review","label":"Review"}
        ]}),
        json!({"panes":[
            {"workspace_id":"project","tab_id":"editor","pane_id":"implementation","label":"implementation","agent":"codex","agent_status":"working"},
            {"workspace_id":"project","tab_id":"editor","pane_id":"shell","terminal_title":"not a manual label"},
            {"workspace_id":"feature","tab_id":"review","pane_id":"reviewer","label":"reviewer"}
        ]}),
    )
}
#[cfg(not(any(feature = "mock", test)))]
fn sample_snapshot() -> Result<Snapshot> {
    unreachable!("mock mode is disabled")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_accepts_focus_events_and_rejects_missing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(dir.path().join("mru.json"));
        track_event(
            &tracker,
            r#"{"event":"workspace_focused","data":{"workspace_id":"w1"}}"#,
            None,
        )
        .unwrap();
        assert!(tracker.load().unwrap().workspaces.contains_key("w1"));
        track_event(&tracker, r#"{"event":"unrelated","data":{}}"#, None).unwrap();
        for kind in ["pane_focused", "tab_focused", "workspace_focused"] {
            let text = serde_json::json!({"event":kind,"data":{}}).to_string();
            assert!(track_event(&tracker, &text, None).is_err());
        }
    }

    #[test]
    fn pane_and_tab_focus_capture_the_full_matching_location() {
        for event in [
            r#"{"event":"pane_focused","data":{"workspace_id":"w","pane_id":"p"}}"#,
            r#"{"event":"tab_focused","data":{"workspace_id":"w","tab_id":"t"}}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let tracker = Tracker::new(dir.path().join("mru.json"));
            let live = ActiveLocation {
                workspace_id: Some("w".into()),
                tab_id: Some("t".into()),
                pane_id: Some("p".into()),
            };
            track_event(&tracker, event, Some(&live)).unwrap();
            let saved = tracker.load().unwrap();
            assert_eq!(saved.workspaces["w"], saved.tabs["t"]);
            assert_eq!(saved.tabs["t"], saved.panes["p"]);
        }
    }

    #[test]
    fn delayed_focus_events_do_not_record_an_unrelated_live_location() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = Tracker::new(dir.path().join("mru.json"));
        let live = ActiveLocation {
            workspace_id: Some("w".into()),
            tab_id: Some("new-tab".into()),
            pane_id: Some("new-pane".into()),
        };
        track_event(
            &tracker,
            r#"{"event":"pane_focused","data":{"workspace_id":"w","pane_id":"old-pane"}}"#,
            Some(&live),
        )
        .unwrap();
        let saved = tracker.load().unwrap();
        assert!(saved.tabs.is_empty());
        assert!(saved.panes.contains_key("old-pane"));
        assert!(!saved.panes.contains_key("new-pane"));
    }

    #[test]
    fn mock_sample_contains_a_worktree_and_an_unnamed_pane() {
        let snapshot = sample_snapshot().unwrap();
        assert!(
            snapshot.workspaces[1]
                .worktree
                .as_ref()
                .unwrap()
                .is_linked_worktree
        );
        assert!(snapshot.workspaces[0].tabs[0].panes[1].name.is_none());
    }

    #[test]
    fn custom_keybindings_keep_unspecified_defaults() {
        let bindings: Keybindings = toml::from_str("move_up = ['C-k']").unwrap();
        assert_eq!(bindings.move_up, vec!["C-k"]);
        assert_eq!(bindings.move_down, vec!["Down", "C-n", "D-j", "M-j"]);
    }
}
