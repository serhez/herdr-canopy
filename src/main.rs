mod app;
mod cli;
mod ipc;
mod models;
mod popup;
mod theme;
mod tracker;
mod ui;

#[cfg(test)]
mod test_helpers;

use anyhow::{Context, Result};
use app::AppState;
use clap::Parser;
use cli::{Cli, Command};
use crossterm::{
    cursor,
    event::{
        self, Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    style::ResetColor,
    terminal,
};
use models::{ActiveLocation, FocusTarget, History, KeyAction, Keybindings, Snapshot};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use theme::Palette;
use tracker::Tracker;

const PLUGIN_ID: &str = "serhez.herdr-canopy";

struct RawTerminal {
    enhanced_keys: bool,
}
impl RawTerminal {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut raw = Self {
            enhanced_keys: false,
        };
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        raw.enhanced_keys = true;
        Ok(raw)
    }
}
impl Drop for RawTerminal {
    fn drop(&mut self) {
        if self.enhanced_keys {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), cursor::Show, ResetColor);
        let _ = io::stdout().flush();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::TrackWork)) {
        let received_ms = tracker::unix_millis()?;
        let event: serde_json::Value = serde_json::from_str(
            &std::env::var("HERDR_PLUGIN_EVENT_JSON")
                .context("HERDR_PLUGIN_EVENT_JSON is missing")?,
        )?;
        let agents = if event["event"] == "pane_agent_status_changed" {
            ipc::fetch_agent_activity()?
        } else {
            Vec::new()
        };
        return tracker::WorkingTracker::for_session().record_event(&event, &agents, received_ms);
    }
    if matches!(cli.command, Some(Command::Track)) {
        let event = std::env::var("HERDR_PLUGIN_EVENT_JSON")
            .context("HERDR_PLUGIN_EVENT_JSON is missing")?;
        return track_event(
            &Tracker::for_session(),
            &event,
            ipc::fetch_focused_location().ok().as_ref(),
        );
    }
    anyhow::ensure!(
        std::env::var("HERDR_ENV").as_deref() == Ok("1") || mock_enabled(&cli),
        "Run this plugin inside Herdr, or use --mock to preview it"
    );
    if cli.pane_open {
        return popup::open(&tracker::session_path("popup", "state"), open_navigator).map(|_| ());
    }
    if matches!(cli.command, Some(Command::Toggle)) {
        return popup::toggle(&tracker::session_path("popup", "state"), open_navigator).map(|_| ());
    }

    let _popup = if std::env::var("HERDR_PLUGIN_ENTRYPOINT_ID").as_deref() == Ok("navigator") {
        Some(popup::PopupState::claim(&tracker::session_path(
            "popup", "state",
        ))?)
    } else {
        None
    };

    let tracker = Tracker::for_session();
    let mock = mock_enabled(&cli);
    let snapshot = if mock {
        sample_snapshot()?
    } else {
        ipc::fetch_snapshot()?
    };
    let context: Option<serde_json::Value> = std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let current = snapshot.active_location(context.as_ref());
    if !mock && current != ActiveLocation::default() {
        tracker.record(&current)?;
    }
    let recency = if mock {
        History::default()
    } else {
        tracker.load()?
    };
    let mut app = AppState::new(snapshot, &recency, current, load_keybindings()?);
    let selected = run_tui(&mut app, &tracker, !mock, _popup.as_ref())?;
    if let Some(target) = selected
        && !mock
    {
        ipc::focus(&target)?;
    }
    Ok(())
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

fn open_navigator() -> Result<()> {
    let plugin_id = std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.into());
    ipc::cli_request(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        &plugin_id,
        "--entrypoint",
        "navigator",
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
    let path =
        std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").map(|p| PathBuf::from(p).join("config.toml"));
    let Some(path) = path else {
        return Ok(Keybindings::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let settings: toml::Value = text
                .parse()
                .with_context(|| format!("Invalid settings: {}", path.display()))?;
            settings
                .get("keybindings")
                .map(|value| value.clone().try_into().context("Invalid keybindings"))
                .unwrap_or_else(|| Ok(Keybindings::default()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Keybindings::default()),
        Err(e) => Err(e.into()),
    }
}

fn run_tui(
    app: &mut AppState,
    tracker: &Tracker,
    connected: bool,
    popup: Option<&popup::PopupState>,
) -> Result<Option<FocusTarget>> {
    let _raw = RawTerminal::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut palette = Palette::load();
    let (updates_tx, updates_rx) = mpsc::sync_channel(1);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    if connected {
        std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stop_rx.recv_timeout(Duration::from_secs(2))
            {
                if let Err(mpsc::TrySendError::Disconnected(_)) =
                    updates_tx.try_send(ipc::fetch_snapshot())
                {
                    break;
                }
            }
        });
    }
    let result = (|| -> Result<Option<FocusTarget>> {
        let mut redraw = true;
        let mut last_tick = Instant::now();
        loop {
            if popup.is_some_and(|popup| popup.close_requested()) {
                return Ok(None);
            }
            if last_tick.elapsed() >= Duration::from_secs(1) {
                redraw |= app.rows.iter().any(|row| row.working_since_ms.is_some());
                last_tick = Instant::now();
            }
            if let Ok(update) = updates_rx.try_recv() {
                match update.and_then(|snapshot| Ok((snapshot, tracker.load()?))) {
                    Ok((snapshot, recency)) => {
                        app.update_snapshot(snapshot, &recency);
                        app.error = None;
                    }
                    Err(error) => app.error = Some(format!("Refresh failed: {error}")),
                }
                palette = Palette::load();
                redraw = true;
            }
            if redraw {
                terminal.draw(|frame| ui::render(frame, app, &palette))?;
                redraw = false;
            }
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    redraw = true;
                    match app.handle_key(key) {
                        KeyAction::Select => return Ok(app.selected_target()),
                        KeyAction::Dismiss => return Ok(None),
                        KeyAction::Continue => {}
                    }
                }
                Event::Resize(_, _) => redraw = true,
                _ => {}
            }
        }
    })();
    drop(stop_tx);
    result
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
