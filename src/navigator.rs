//! The navigator loop shared by every backend.

use crate::app::AppState;
use crate::host::{FocusOutcome, Host};
use crate::models::{ActiveLocation, FocusTarget, History, KeyAction, Snapshot, View};
use crate::popup::PopupState;
use crate::ui;
use anyhow::Result;
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
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Write};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

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

/// Runs the navigator over `initial` until the user leaves or focus moves.
/// Without `live`, it neither records history, refreshes, nor moves focus (preview).
pub fn run(
    host: Arc<dyn Host>,
    initial: Snapshot,
    view: View,
    popup: Option<&PopupState>,
    live: bool,
) -> Result<()> {
    let mut app = prepare(host.as_ref(), initial, view, live)?;
    let mut jump_notice = None;
    loop {
        let selected = run_tui(&mut app, &host, live, popup, jump_notice.as_deref())?;
        if !finish(host.as_ref(), &mut app, selected, live)? {
            return Ok(());
        }
        // `finish` left the reason the jump stopped short; keep it across refreshes.
        jump_notice.clone_from(&app.error);
    }
}

/// Records where the navigator was opened from and builds its state.
fn prepare(host: &dyn Host, snapshot: Snapshot, view: View, live: bool) -> Result<AppState> {
    let current = host.current(&snapshot);
    if live && current != ActiveLocation::default() {
        host.history().record(&current)?;
    }
    let recency = if live {
        host.history().load()?
    } else {
        History::default()
    };
    let notice = snapshot.notice.clone();
    let mut app = AppState::new(snapshot, &recency, current, host.keybindings()?, view);
    app.error = notice;
    Ok(app)
}

/// Applies the selection once the terminal is restored; true reopens the navigator.
fn finish(
    host: &dyn Host,
    app: &mut AppState,
    selected: Option<FocusTarget>,
    live: bool,
) -> Result<bool> {
    let Some(target) = selected.filter(|_| live) else {
        return Ok(false);
    };
    match host.focus(&target)? {
        FocusOutcome::Done => Ok(false),
        FocusOutcome::Stay(notice) => {
            app.error = Some(notice);
            Ok(true)
        }
    }
}

/// Shows a refresh result: a refresh error first, else the sticky jump notice,
/// else the snapshot's own notice.
fn apply_refresh(
    app: &mut AppState,
    update: Result<(Snapshot, History)>,
    jump_notice: Option<&str>,
) {
    match update {
        Ok((mut snapshot, recency)) => {
            let notice = snapshot.notice.take();
            app.update_snapshot(snapshot, &recency);
            app.error = jump_notice.map(String::from).or(notice);
        }
        Err(error) => app.error = Some(format!("Refresh failed: {error}")),
    }
}

fn run_tui(
    app: &mut AppState,
    host: &Arc<dyn Host>,
    connected: bool,
    popup: Option<&PopupState>,
    jump_notice: Option<&str>,
) -> Result<Option<FocusTarget>> {
    let _raw = RawTerminal::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut palette = host.palette();
    let (updates_tx, updates_rx) = mpsc::sync_channel(1);
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    if connected {
        let host = Arc::clone(host);
        std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stop_rx.recv_timeout(Duration::from_secs(2))
            {
                if let Err(mpsc::TrySendError::Disconnected(_)) =
                    updates_tx.try_send(host.snapshot())
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
                let update = update.and_then(|snapshot| Ok((snapshot, host.history().load()?)));
                apply_refresh(app, update, jump_notice);
                palette = host.palette();
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::history::Tracker;
    use crate::models::Keybindings;
    use crate::test_helpers::snapshot;
    use crate::theme::Palette;
    use std::sync::Mutex;

    /// A host that records focus requests and answers them with a fixed outcome.
    pub struct FakeHost {
        pub current: ActiveLocation,
        pub outcome: FocusOutcome,
        pub focused: Mutex<Vec<FocusTarget>>,
        pub history: Tracker,
    }

    impl FakeHost {
        pub fn new(dir: &std::path::Path, outcome: FocusOutcome) -> Self {
            Self {
                current: ActiveLocation {
                    workspace_id: Some("project".into()),
                    tab_id: Some("editor".into()),
                    pane_id: Some("named-notes".into()),
                },
                outcome,
                focused: Mutex::new(Vec::new()),
                history: Tracker::new(dir.join("mru.json")),
            }
        }
    }

    impl Host for FakeHost {
        fn snapshot(&self) -> Result<Snapshot> {
            Ok(snapshot())
        }
        fn current(&self, _: &Snapshot) -> ActiveLocation {
            self.current.clone()
        }
        fn focus(&self, target: &FocusTarget) -> Result<FocusOutcome> {
            self.focused.lock().unwrap().push(target.clone());
            Ok(self.outcome.clone())
        }
        fn history(&self) -> &Tracker {
            &self.history
        }
        fn keybindings(&self) -> Result<Keybindings> {
            Ok(Keybindings::default())
        }
        fn palette(&self) -> Palette {
            Palette::default()
        }
    }

    #[test]
    fn opening_live_records_the_origin_but_a_preview_leaves_history_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Done);
        prepare(&host, snapshot(), View::Tree, false).unwrap();
        assert_eq!(host.history.load().unwrap(), History::default());
        prepare(&host, snapshot(), View::Tree, true).unwrap();
        let saved = host.history.load().unwrap();
        assert!(saved.panes.contains_key("named-notes"));
        assert!(saved.workspaces.contains_key("project"));
    }

    #[test]
    fn a_preview_or_dismissal_never_moves_focus() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Done);
        let mut app = prepare(&host, snapshot(), View::Tree, false).unwrap();
        let target = Some(FocusTarget::Pane("named-review".into()));
        assert!(!finish(&host, &mut app, target, false).unwrap());
        assert!(!finish(&host, &mut app, None, true).unwrap());
        assert!(host.focused.lock().unwrap().is_empty());
    }

    #[test]
    fn a_completed_jump_closes_the_navigator() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Done);
        let mut app = prepare(&host, snapshot(), View::Tree, true).unwrap();
        let target = FocusTarget::Pane("named-review".into());
        assert!(!finish(&host, &mut app, Some(target.clone()), true).unwrap());
        assert_eq!(*host.focused.lock().unwrap(), vec![target]);
        assert_eq!(app.error, None);
    }

    #[test]
    fn an_incomplete_jump_keeps_the_navigator_open_with_its_notice() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Stay("Turn on X".into()));
        let mut app = prepare(&host, snapshot(), View::Tree, true).unwrap();
        let target = Some(FocusTarget::Pane("named-review".into()));
        assert!(finish(&host, &mut app, target, true).unwrap());
        assert_eq!(app.error.as_deref(), Some("Turn on X"));
    }

    #[test]
    fn the_snapshot_notice_is_shown_when_the_navigator_opens() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Done);
        let mut initial = snapshot();
        initial.notice = Some("Canopy watcher is not running".into());
        let app = prepare(&host, initial, View::Tree, true).unwrap();
        assert_eq!(app.error.as_deref(), Some("Canopy watcher is not running"));
    }

    #[test]
    fn refreshes_show_errors_first_then_the_jump_notice_then_the_snapshot_notice() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Done);
        let mut app = prepare(&host, snapshot(), View::Tree, true).unwrap();
        let with_notice = || {
            let mut data = snapshot();
            data.notice = Some("Worktree bindings ignored: bad JSON".into());
            Ok((data, History::default()))
        };

        apply_refresh(&mut app, Ok((snapshot(), History::default())), None);
        assert_eq!(app.error, None);

        apply_refresh(&mut app, with_notice(), None);
        assert_eq!(
            app.error.as_deref(),
            Some("Worktree bindings ignored: bad JSON")
        );

        apply_refresh(&mut app, with_notice(), Some("Turn on Remote Control"));
        assert_eq!(app.error.as_deref(), Some("Turn on Remote Control"));

        apply_refresh(&mut app, Err(anyhow::anyhow!("socket closed")), None);
        assert!(app.error.as_deref().unwrap().contains("socket closed"));

        apply_refresh(&mut app, Ok((snapshot(), History::default())), None);
        assert_eq!(app.error, None);
    }

    #[test]
    fn refreshed_data_replaces_the_rows() {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(dir.path(), FocusOutcome::Done);
        let mut app = prepare(&host, snapshot(), View::Tree, true).unwrap();
        let mut data = snapshot();
        data.workspaces[2].name = "Renamed".into();
        apply_refresh(&mut app, Ok((data, History::default())), None);
        assert!(app.rows.iter().any(|row| row.name == "Renamed"));
    }
}
