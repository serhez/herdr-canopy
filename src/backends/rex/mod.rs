//! Rex backend: Canopy as a floating popup in a Rex server, fed by one batched
//! `rex do` script per refresh and a watcher process for MRU and timers.

pub mod agent;
pub mod bindings;
pub mod claude_jobs;
pub mod cli;
pub mod decode;
pub mod layer;
pub mod watch;

#[cfg(test)]
mod script_tests;
#[cfg(test)]
pub mod testing;

use crate::cli::{LaunchArgs, RexCommand, UiArgs, WatchArgs};
use crate::history::Tracker;
use crate::host::{FocusOutcome, Host};
use crate::models::{ActiveLocation, FocusTarget, Keybindings, Snapshot, View};
use crate::navigator;
use crate::popup::{self, PopupState};
use crate::theme::{self, Palette, Rgb};
use anyhow::{Context, Result};
use clap::ValueEnum;
use cli::Rex;
use decode::DecodeContext;
use serde::Deserialize;
use serde_json::json;
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SNAPSHOT: &str = include_str!("scripts/snapshot.lua");
const FOCUS: &str = include_str!("scripts/focus.lua");
const PREPARE: &str = include_str!("scripts/prepare.lua");
const SPAWN: &str = include_str!("scripts/spawn.lua");
const LAYER: &str = include_str!("scripts/layer.lua");
const STATUS: &str = include_str!("scripts/status.lua");

const REMOTE_CONTROL_NOTICE: &str = "Jumps need Remote Control: Rex Settings > Rex Server";
const WHICH_APP_NOTICE: &str = "Cannot tell which Rex app to switch; jump from the app itself";
const NO_WATCHER_NOTICE: &str = "Canopy watcher is not running";

/// How far the focused row's background moves from the terminal's background
/// toward its text color.
const ROW_LIFT: f64 = 0.08;

/// How long the popup waits for the app to size its terminal.
const APP_SIZE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long the popup waits for the app to resize its terminal to a new layer.
const RESIZE_TIMEOUT: Duration = Duration::from_millis(300);

/// How long the popup waits for its terminal to report its colors.
const COLOR_QUERY_TIMEOUT: Duration = Duration::from_millis(150);

/// The size a new terminal block reports until an app renders it.
const PLACEHOLDER_SIZE: (u16, u16) = (80, 24);

/// Runs a `herdr-canopy rex …` command.
pub fn run(command: RexCommand, view: View) -> Result<ExitCode> {
    let env = std::env::var_os;
    let root = state_root(env("XDG_STATE_HOME").as_deref(), env("HOME").as_deref())
        .context("Cannot find Canopy's state directory: HOME is not set");
    match command {
        RexCommand::Toggle(args) => launch(&launch_request(args, view, root?)?, true)?,
        RexCommand::Open(args) => launch(&launch_request(args, view, root?)?, false)?,
        RexCommand::Ui(args) => {
            let state_dir = popup_state_dir(args.state_dir.clone(), root, &args.host_id)?;
            ui(*args, view, state_dir)?
        }
        RexCommand::Watch(args) => watch_server(args, &root?)?,
        // Agent commands reply in JSON, errors included.
        RexCommand::Agent(args) => return Ok(agent::run(args, root)),
    }
    Ok(ExitCode::SUCCESS)
}

fn resolve_rex(explicit: Option<PathBuf>) -> Rex {
    Rex::resolve(explicit, std::env::var_os("REX_BIN_DIR").as_deref())
}

fn launch_request(args: LaunchArgs, view: View, state_root: PathBuf) -> Result<Launch> {
    Ok(Launch {
        rex: resolve_rex(args.common.rex),
        bin: std::env::current_exe().context("Cannot find the herdr-canopy binary")?,
        view,
        session: args.session,
        block: args.block,
        client: args.client,
        config: args.common.config,
        size: args.common.size,
        state_root,
        claim_timeout: popup::TIMEOUT,
    })
}

/// The popup process: claims the popup lock, then runs the navigator.
fn ui(args: UiArgs, view: View, state_dir: PathBuf) -> Result<()> {
    let env = std::env::var_os;
    let popup = PopupState::claim(
        &popup_path(&state_dir),
        &view_key(view, &args.origin_window),
    )?;
    let rex = resolve_rex(args.common.rex);
    let own_block = std::env::var("REX_BLOCK").ok().filter(|b| !b.is_empty());
    let session = args.origin_session.clone();
    // The app sizes the terminal, and gives it its theme, once it owns the new
    // block; drawing or asking for colors before would use the placeholders.
    if let Some(block) = &own_block
        && let Some(got) = wait_for(
            || {
                crossterm::terminal::window_size()
                    .ok()
                    .and_then(|s| app_size(&s))
            },
            APP_SIZE_TIMEOUT,
        )
        && let (Some(window), Some(share)) = (args.window_px, args.share)
    {
        let want = args.common.size.unwrap_or(view.default_size());
        let placement = Placement { window, share };
        if fit(&rex, &session, block, want, got, placement, &state_dir) {
            wait_for(
                || {
                    crossterm::terminal::size()
                        .ok()
                        .filter(|size| *size != got.cells)
                },
                RESIZE_TIMEOUT,
            );
        }
    }
    let host = RexHost::new(Settings {
        rex: rex.clone(),
        host_id: args.host_id,
        state_dir,
        origin: Origin {
            session: args.origin_session,
            window: args.origin_window,
            block: args.origin_block,
        },
        client: args.client,
        config: config_path(
            args.common.config,
            env("XDG_CONFIG_HOME").as_deref(),
            env("HOME").as_deref(),
        )
        .context("Cannot find Canopy's config file: HOME is not set")?,
        bindings: bindings::path(env("XDG_STATE_HOME").as_deref(), env("HOME").as_deref()),
        own_block: own_block.clone(),
        terminal_colors: terminal_colors(COLOR_QUERY_TIMEOUT),
    });
    let snapshot = host.snapshot()?;
    navigator::run(Arc::new(host), snapshot, view, Some(&popup), true)?;
    // Rex keeps a terminal whose program exits right after starting, so a
    // quickly closed popup would stay on screen: close its layer explicitly.
    // That ends this process too, so the lock is released first.
    if let Some(block) = own_block {
        drop(popup);
        let close = json!({"action": "close", "session_id": session, "block_id": block});
        rex.script(LAYER, &close)?;
    }
    Ok(())
}

/// The first answer of `probe`, polled until `timeout`.
fn wait_for<T>(mut probe: impl FnMut() -> Option<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        match probe() {
            Some(answer) => return Some(answer),
            None if Instant::now() >= deadline => return None,
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

/// A terminal's size as an app set it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AppSize {
    cells: (u16, u16),
    /// A cell's size in pixels, when the terminal reports it.
    cell: Option<layer::Px>,
}

/// The terminal's size if an app has set it: until then a terminal reports
/// the placeholder size and no pixels.
fn app_size(size: &crossterm::terminal::WindowSize) -> Option<AppSize> {
    let cells = (size.columns, size.rows);
    let pixels = size.width > 0 && size.height > 0 && cells.0 > 0 && cells.1 > 0;
    let cell = pixels.then(|| {
        (
            f64::from(size.width) / f64::from(cells.0),
            f64::from(size.height) / f64::from(cells.1),
        )
    });
    (cell.is_some() || cells != PLACEHOLDER_SIZE).then_some(AppSize { cells, cell })
}

/// Where the launcher put the popup: its estimate of the window in pixels and
/// the layer's share of it.
#[derive(Debug, Clone, Copy)]
struct Placement {
    window: layer::Px,
    share: (f64, f64),
}

/// Fits the popup's layer, which the app sized to `got`, to `want` cells
/// before its first draw, and learns the frame it measured for the next popup.
/// A cell off is left alone, as a resize shows more. Returns whether it
/// resized the layer. Best effort: a failure leaves the popup as it is.
fn fit(
    rex: &Rex,
    session: &str,
    block: &str,
    want: (u16, u16),
    got: AppSize,
    placement: Placement,
    state_dir: &Path,
) -> bool {
    let Some(measured) = got.cell.and_then(|cell| {
        layer::Frame::measured(placement.share, placement.window, got.cells, cell)
    }) else {
        return false;
    };
    let stored = layer::Frame::load(state_dir);
    let updated = layer::Frame::updated(stored, measured, want, got.cells);
    if let Some(frame) = updated {
        let _ = frame.save(state_dir);
    }
    let frame = updated.unwrap_or(measured);
    if !layer::needs_resize(want, got.cells) {
        return false;
    }
    let find = json!({"action": "find", "session_id": session, "block_id": block});
    let Some(layer_id) = rex
        .script(LAYER, &find)
        .ok()
        .and_then(|found| found["layer_id"].as_str().map(String::from))
    else {
        return false;
    };
    let resize = json!({"action": "resize", "session_id": session, "layer_id": layer_id,
                        "bounds": layer::centered(want, placement.window, frame)});
    rex.script(LAYER, &resize).is_ok()
}

/// The terminal's text, background and palette color 16, asked with OSC 10,
/// 11 and 4 before the navigator takes the terminal; those it does not answer
/// in `timeout` are `None`.
fn terminal_colors(timeout: Duration) -> TerminalColors {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    let none = TerminalColors::default();
    let Ok(mut tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    else {
        return none;
    };
    if crossterm::terminal::enable_raw_mode().is_err() {
        return none;
    }
    let mut reply = Vec::new();
    // Device attributes last: every terminal answers them, so their reply ends
    // the wait without the timeout.
    if tty
        .write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]4;16;?\x1b\\\x1b[c")
        .and_then(|()| tty.flush())
        .is_ok()
    {
        let deadline = Instant::now() + timeout;
        while !device_attributes_answered(&reply) {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut ready = libc::pollfd {
                fd: tty.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: `ready` is one valid pollfd for an open descriptor.
            if left.is_zero() || unsafe { libc::poll(&mut ready, 1, millis) } <= 0 {
                break;
            }
            let mut chunk = [0u8; 256];
            match tty.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => reply.extend_from_slice(&chunk[..n]),
            }
        }
    }
    let _ = crossterm::terminal::disable_raw_mode();
    let reply = String::from_utf8_lossy(&reply);
    TerminalColors {
        text: theme::osc_color(&reply, "10"),
        background: theme::osc_color(&reply, "11"),
        color16: theme::osc_color(&reply, "4;16"),
    }
}

/// Whether `reply` holds a whole primary device attributes reply, `ESC [ ? … c`.
fn device_attributes_answered(reply: &[u8]) -> bool {
    reply.windows(3).enumerate().any(|(at, start)| {
        start == b"\x1b[?"
            && reply[at + 3..]
                .iter()
                .find(|b| !(b.is_ascii_digit() || **b == b';'))
                == Some(&b'c')
    })
}

/// `herdr-canopy rex watch`: the watcher for the server `rex` reaches.
fn watch_server(args: WatchArgs, root: &Path) -> Result<()> {
    let rex = resolve_rex(args.rex);
    let status = rex.script(STATUS, &json!({}))?;
    let host_id = status["host_id"]
        .as_str()
        .context("The Rex server did not report its host id")?;
    let dir = state_dir(root, host_id)?;
    anyhow::ensure!(
        args.ensure || !watch::is_running(&dir)?,
        "A Canopy watcher already runs for this server (pass --ensure to replace one from another build)"
    );
    watch::run(&rex, &dir)
}

/// `${XDG_STATE_HOME:-$HOME/.local/state}/herdr-canopy`, using `XDG_STATE_HOME`
/// only when it is absolute.
pub fn state_root(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    xdg_home(xdg_state_home, home, ".local/state").map(|dir| dir.join("herdr-canopy"))
}

/// An XDG base directory: the variable when absolute, else `$HOME/<fallback>`.
fn xdg_home(xdg: Option<&OsStr>, home: Option<&OsStr>, fallback: &str) -> Option<PathBuf> {
    match xdg.map(Path::new) {
        Some(dir) if dir.is_absolute() => Some(dir.to_path_buf()),
        _ => home.map(|home| Path::new(home).join(fallback)),
    }
}

/// One Rex server's state directory (`rex-<host_id>`) under `root`. Refuses host
/// ids that are not a plain file name.
pub fn state_dir(root: &Path, host_id: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !matches!(host_id, "" | "." | "..") && !host_id.contains(['/', '\\', '\0']),
        "Invalid Rex host id {host_id:?}"
    );
    Ok(root.join(format!("rex-{host_id}")))
}

/// The popup's state directory: the one its launcher passed, else this
/// server's directory under `root`.
fn popup_state_dir(
    given: Option<PathBuf>,
    root: Result<PathBuf>,
    host_id: &str,
) -> Result<PathBuf> {
    match given {
        Some(dir) => Ok(dir),
        None => state_dir(&root?, host_id),
    }
}

/// The recent-location history (MRU) inside a server's state directory.
pub fn tracker(state_dir: &Path) -> Tracker {
    Tracker::new(state_dir.join("mru.json"))
}

/// The popup lock path inside a server's state directory.
pub fn popup_path(state_dir: &Path) -> PathBuf {
    state_dir.join("popup.state")
}

/// Canopy's Rex config file: `explicit`, else
/// `${XDG_CONFIG_HOME:-$HOME/.config}/herdr-canopy/config.toml` (absolute
/// `XDG_CONFIG_HOME` only).
pub fn config_path(
    explicit: Option<PathBuf>,
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    explicit.or_else(|| {
        xdg_home(xdg_config_home, home, ".config").map(|dir| dir.join("herdr-canopy/config.toml"))
    })
}

/// The popup identity on a server: a toggle in another window replaces it.
pub fn view_key(view: View, window_id: &str) -> String {
    format!("{}@{window_id}", view.entrypoint())
}

/// The app to switch sessions in: `given` when it is a connected app, else the
/// only connected app; `None` when that is ambiguous.
pub fn choose_client(given: Option<&str>, apps: &[String]) -> Option<String> {
    match (given, apps) {
        (Some(given), _) if apps.iter().any(|app| app == given) => Some(given.to_string()),
        (_, [only]) => Some(only.clone()),
        _ => None,
    }
}

/// Where the navigator was opened from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub session: String,
    pub window: String,
    pub block: String,
}

/// Everything a popup needs to talk to its server.
pub struct Settings {
    pub rex: Rex,
    pub host_id: String,
    pub state_dir: PathBuf,
    pub origin: Origin,
    /// The app client that opened the popup (`ctx.client_id`).
    pub client: Option<String>,
    pub config: PathBuf,
    pub bindings: Option<PathBuf>,
    /// The popup's own block, never listed.
    pub own_block: Option<String>,
    /// The colors the popup's terminal reported.
    pub terminal_colors: TerminalColors,
}

/// The colors a terminal reports, each when it does.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TerminalColors {
    pub text: Option<Rgb>,
    pub background: Option<Rgb>,
    /// Palette color 16, which some themes (Teide, Tokyo Night) set to orange.
    pub color16: Option<Rgb>,
}

/// Navigator data, focus and settings from one Rex server.
pub struct RexHost {
    settings: Settings,
    history: Tracker,
}

impl RexHost {
    pub fn new(settings: Settings) -> Self {
        let history = tracker(&settings.state_dir);
        Self { settings, history }
    }

    fn record(&self, location: &ActiveLocation) -> Result<FocusOutcome> {
        self.history.record(location)?;
        Ok(FocusOutcome::Done)
    }
}

/// The focus script's answer.
#[derive(Deserialize)]
struct Focused {
    session_id: String,
    window_id: String,
    /// Connected app clients.
    #[serde(default, deserialize_with = "decode::list")]
    apps: Vec<String>,
}

impl Host for RexHost {
    fn snapshot(&self) -> Result<Snapshot> {
        let settings = &self.settings;
        let raw = settings.rex.script(SNAPSHOT, &json!({}))?;
        let history = self.history.load()?;
        // The watcher rewrites a damaged file; until then there are no timers.
        let working =
            watch::WorkingState::load(&settings.state_dir.join("working.json")).unwrap_or_default();
        let mut snapshot = decode::decode(
            &raw,
            &DecodeContext {
                history: &history,
                periods: &working.periods,
                exclude_block: settings.own_block.as_deref(),
                viewed_job: &claude_jobs::viewed_job,
            },
        )?;
        if let Some(path) = &settings.bindings {
            bindings::Bindings::read(path, &settings.host_id).apply(&mut snapshot);
        }
        if !watch::is_running(&settings.state_dir)? {
            snapshot.add_notice(NO_WATCHER_NOTICE);
        }
        Ok(snapshot)
    }

    fn current(&self, _snapshot: &Snapshot) -> ActiveLocation {
        let origin = &self.settings.origin;
        ActiveLocation {
            workspace_id: Some(origin.session.clone()),
            tab_id: Some(origin.window.clone()),
            pane_id: Some(origin.block.clone()),
        }
    }

    fn focus(&self, target: &FocusTarget) -> Result<FocusOutcome> {
        let (kind, id, block) = match target {
            FocusTarget::Workspace(id) => ("session", id, None),
            FocusTarget::Tab(id) => ("window", id, None),
            FocusTarget::Pane(id) => ("block", id, Some(id.clone())),
        };
        let answer = self
            .settings
            .rex
            .script(FOCUS, &json!({"kind": kind, "id": id}))?;
        let found: Focused = serde_json::from_value(answer).context("Invalid focus answer")?;
        let destination = ActiveLocation {
            workspace_id: Some(found.session_id.clone()),
            tab_id: Some(found.window_id.clone()),
            pane_id: block,
        };
        // The app follows server focus inside the session it shows.
        if found.session_id == self.settings.origin.session {
            return self.record(&destination);
        }
        let Some(client) = choose_client(self.settings.client.as_deref(), &found.apps) else {
            return Ok(FocusOutcome::Stay(WHICH_APP_NOTICE.into()));
        };
        let select = [
            ("session_id", found.session_id.as_str()),
            ("window_id", found.window_id.as_str()),
        ];
        match self
            .settings
            .rex
            .client_action(&client, "session.select", &select)
        {
            Ok(()) => self.record(&destination),
            Err(error) => {
                let error = format!("{error:#}");
                // Rex's own message is too long for the popup's notice line.
                let notice = if error.contains("Remote Control") {
                    REMOTE_CONTROL_NOTICE.into()
                } else {
                    error
                };
                Ok(FocusOutcome::Stay(notice))
            }
        }
    }

    fn history(&self) -> &Tracker {
        &self.history
    }

    fn keybindings(&self) -> Result<Keybindings> {
        Keybindings::load(&self.settings.config)
    }

    fn palette(&self) -> Palette {
        // Unlike Herdr's, Rex's popup is a terminal like any pane: by default it
        // keeps the theme's background, so it matches them, and the focused row
        // lifts that background slightly.
        let mut base = Palette {
            background: ratatui::style::Color::Reset,
            ..Palette::default()
        };
        let colors = self.settings.terminal_colors;
        if let (Some(text), Some(background)) = (colors.text, colors.background) {
            base.selection = theme::lift(background, text, ROW_LIFT);
        }
        // Themes that set color 16, xterm's black, mean it as their orange.
        if colors.color16.is_some_and(|color| color != (0, 0, 0)) {
            base.peach = ratatui::style::Color::Indexed(16);
        }
        Palette::from_config_with(
            &std::fs::read_to_string(&self.settings.config).unwrap_or_default(),
            base,
        )
    }
}

/// A toggle/open request from a Rex action.
pub struct Launch {
    pub rex: Rex,
    /// This binary, started as the popup and the watcher.
    pub bin: PathBuf,
    pub view: View,
    pub session: String,
    pub block: String,
    pub client: Option<String>,
    pub config: Option<PathBuf>,
    pub size: Option<(u16, u16)>,
    /// Canopy's state root (see [`state_root`]).
    pub state_root: PathBuf,
    /// How long the popup may take to start (and an old one to close).
    pub claim_timeout: Duration,
}

/// The prepare script's answer.
#[derive(Deserialize)]
struct Prepared {
    host_id: String,
    window_id: String,
    size: CellSize,
    rect: layer::Bounds,
}

#[derive(Deserialize)]
struct CellSize {
    columns: u16,
    rows: u16,
    #[serde(default)]
    cell_width_px: f64,
    #[serde(default)]
    cell_height_px: f64,
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("Path is not UTF-8: {}", path.display()))
}

/// Toggles (or opens) the popup in the origin window and returns once it has
/// started; it never waits for a selection.
pub fn launch(launch: &Launch, toggle: bool) -> Result<()> {
    let rex = &launch.rex;
    let prepared: Prepared = serde_json::from_value(rex.script(
        PREPARE,
        &json!({"session_id": launch.session, "block_id": launch.block}),
    )?)
    .context("Invalid prepare answer")?;
    let state_dir = state_dir(&launch.state_root, &prepared.host_id)?;
    if let Err(error) = start_watcher(launch, &state_dir) {
        eprintln!("Cannot start the Canopy watcher: {error:#}");
    }
    let want = launch.size.unwrap_or(launch.view.default_size());
    let size = &prepared.size;
    let origin_cells = (size.columns, size.rows);
    // In pixels, panes of any font size agree on the window; a server that
    // reports no pixels gets a popup sized in the origin's cells.
    let origin_cell = (size.cell_width_px, size.cell_height_px);
    let pixels = origin_cell.0 > 0.0 && origin_cell.1 > 0.0;
    let (window, frame) = if pixels {
        let frame = layer::Frame::load(&state_dir).unwrap_or(layer::Frame::unmeasured(origin_cell));
        (
            layer::window_px(origin_cells, origin_cell, prepared.rect),
            frame,
        )
    } else {
        let unit = (1.0, 1.0);
        (
            layer::window_px(origin_cells, unit, prepared.rect),
            layer::Frame::unmeasured(unit),
        )
    };
    let bounds = layer::centered(want, window, frame);
    let view_name = launch
        .view
        .to_possible_value()
        .context("View has no name")?;
    let mut command = vec![
        utf8(&launch.bin)?.to_string(),
        "rex".into(),
        "ui".into(),
        "--view".into(),
        view_name.get_name().into(),
        "--host-id".into(),
        prepared.host_id.clone(),
        "--origin-session".into(),
        launch.session.clone(),
        "--origin-window".into(),
        prepared.window_id.clone(),
        "--origin-block".into(),
        launch.block.clone(),
        // The popup runs in a Rex block, whose environment may differ from ours.
        "--state-dir".into(),
        utf8(&state_dir)?.to_string(),
        "--rex".into(),
        utf8(&rex.bin)?.to_string(),
        "--size".into(),
        format!("{}x{}", want.0, want.1),
    ];
    if pixels {
        command.extend([
            "--window-px".into(),
            format!("{}x{}", window.0, window.1),
            "--share".into(),
            format!("{}x{}", bounds[2], bounds[3]),
        ]);
    }
    if let Some(client) = &launch.client {
        command.extend(["--client".into(), client.clone()]);
    }
    if let Some(config) = &launch.config {
        command.extend(["--config".into(), utf8(config)?.to_string()]);
    }
    let spawn = json!({
        "session_id": launch.session,
        "window_id": prepared.window_id,
        "bounds": bounds,
        "label": launch.view.title(),
        "command": command,
    });
    let mut layer_id = None;
    let changed = popup::change(
        &popup_path(&state_dir),
        &view_key(launch.view, &prepared.window_id),
        toggle,
        launch.claim_timeout,
        || {
            let spawned = rex.script(SPAWN, &spawn)?;
            layer_id = spawned["layer_id"].as_str().map(String::from);
            Ok(())
        },
    );
    // A popup that never claimed the lock would be left behind as an empty layer.
    if changed.is_err()
        && let Some(layer_id) = layer_id
    {
        let close = json!({"action": "close", "session_id": launch.session, "layer_id": layer_id});
        if let Err(error) = rex.script(LAYER, &close) {
            eprintln!("Cannot close the popup layer {layer_id}: {error:#}");
        }
    }
    changed.map(|_| ())
}

/// Starts this server's watcher, detached, unless one already runs.
fn start_watcher(launch: &Launch, state_dir: &Path) -> Result<()> {
    if watch::is_running(state_dir)? {
        return Ok(());
    }
    let mut child = Command::new(&launch.bin)
        .args(["rex", "watch", "--ensure", "--rex"])
        .arg(&launch.rex.bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group: the action's timeout must not stop it.
        .process_group(0)
        .spawn()?;
    // Reaps it if this process outlives it; otherwise it is reparented.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::popup::{self, PopupState};
    use ratatui::style::Color;
    use serde_json::{Value, json};
    use std::thread;
    use std::time::Instant;
    use testing::FakeRex;

    const HOST: &str = "fcf58739-fba5-4b01-b6cf-022cd6ea6b06";
    const REMOTE_CONTROL: &str = "Remote Control";
    const WHICH_APP: &str = "which Rex app";

    fn origin() -> Origin {
        Origin {
            session: "session:a".into(),
            window: "window:a1".into(),
            block: "block:a1".into(),
        }
    }

    fn settings(fake: &FakeRex, dir: &Path, client: Option<&str>) -> Settings {
        Settings {
            rex: Rex::new(fake.path.clone()),
            host_id: HOST.into(),
            state_dir: dir.join(format!("rex-{HOST}")),
            origin: origin(),
            client: client.map(String::from),
            config: dir.join("config.toml"),
            bindings: None,
            own_block: Some("block:popup".into()),
            terminal_colors: TerminalColors::default(),
        }
    }

    /// The focus script's answer: where the target lives and which apps are connected.
    fn focus_answer(fake: &FakeRex, session: &str, window: &str, apps: &[&str]) {
        let answer = json!({"session_id": session, "window_id": window, "apps": apps});
        fake.answer("focus", &answer.to_string());
    }

    fn args_of(call: &testing::Call) -> Value {
        let at = call
            .argv
            .iter()
            .position(|a| a == "--args")
            .expect("--args");
        serde_json::from_str(&call.argv[at + 1]).unwrap()
    }

    fn assert_never_starts_a_server(fake: &FakeRex) {
        for call in fake.calls() {
            assert_eq!(
                call.argv.first().map(String::as_str),
                Some("--autostart=false"),
                "{call:?}"
            );
        }
    }

    #[test]
    fn state_lives_under_xdg_state_home_per_server() {
        let home = Some(OsStr::new("/home/me"));
        assert_eq!(
            state_root(Some(OsStr::new("/state")), home),
            Some(PathBuf::from("/state/herdr-canopy"))
        );
        let fallback = Some(PathBuf::from("/home/me/.local/state/herdr-canopy"));
        assert_eq!(state_root(Some(OsStr::new("relative")), home), fallback);
        assert_eq!(state_root(Some(OsStr::new("")), home), fallback);
        assert_eq!(state_root(None, None), None);
        let root = Path::new("/state/herdr-canopy");
        assert_eq!(
            state_dir(root, HOST).unwrap(),
            root.join(format!("rex-{HOST}"))
        );
        for bad in ["", ".", "..", "../escape", "a/b", "a\\b", "a\0b"] {
            assert!(state_dir(root, bad).is_err(), "{bad:?}");
        }
        assert_ne!(
            popup_path(&root.join("rex-a")),
            popup_path(&root.join("rex-b"))
        );
    }

    #[test]
    fn the_config_file_is_explicit_or_under_xdg_config_home() {
        let home = Some(OsStr::new("/home/me"));
        let explicit = PathBuf::from("/etc/canopy.toml");
        assert_eq!(
            config_path(Some(explicit.clone()), Some(OsStr::new("/cfg")), home),
            Some(explicit)
        );
        assert_eq!(
            config_path(None, Some(OsStr::new("/cfg")), home),
            Some(PathBuf::from("/cfg/herdr-canopy/config.toml"))
        );
        let fallback = Some(PathBuf::from("/home/me/.config/herdr-canopy/config.toml"));
        assert_eq!(config_path(None, Some(OsStr::new("rel")), home), fallback);
        assert_eq!(config_path(None, None, home), fallback);
        assert_eq!(config_path(None, None, None), None);
    }

    #[test]
    fn popup_keys_name_the_view_and_its_window() {
        assert_eq!(view_key(View::Tree, "window:abc"), "navigator@window:abc");
        assert_eq!(view_key(View::Agents, "window:abc"), "agents@window:abc");
    }

    #[test]
    fn the_session_switch_goes_to_the_requesting_app_or_the_only_app() {
        let apps = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let one = apps(&["client:mac"]);
        let two = apps(&["client:mac", "client:ipad"]);
        assert_eq!(
            choose_client(Some("client:ipad"), &two).as_deref(),
            Some("client:ipad")
        );
        assert_eq!(choose_client(None, &one).as_deref(), Some("client:mac"));
        assert_eq!(
            choose_client(Some("client:cli"), &one).as_deref(),
            Some("client:mac")
        );
        assert_eq!(choose_client(None, &two), None);
        assert_eq!(choose_client(Some("client:cli"), &two), None);
        assert_eq!(choose_client(None, &[]), None);
        assert_eq!(
            choose_client(Some("client:mac"), &[]),
            None,
            "not connected"
        );
    }

    #[test]
    fn the_navigator_starts_from_the_block_it_was_opened_from() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), None));
        assert_eq!(
            host.current(&Snapshot::default()),
            ActiveLocation {
                workspace_id: Some("session:a".into()),
                tab_id: Some("window:a1".into()),
                pane_id: Some("block:a1".into()),
            }
        );
    }

    #[test]
    fn jumps_inside_the_origin_session_never_need_remote_control() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), Some("client:mac")));
        focus_answer(&fake, "session:a", "window:a2", &["client:mac"]);
        let target = FocusTarget::Pane("block:a3".into());
        assert_eq!(host.focus(&target).unwrap(), FocusOutcome::Done);
        let focus = fake.calls_to("focus");
        assert_eq!(focus.len(), 1);
        assert!(focus[0].argv.iter().any(|a| a.contains("block:a3")));
        assert!(fake.calls_to("session.select").is_empty());
        let saved = host.history().load().unwrap();
        assert!(saved.panes.contains_key("block:a3"));
        assert!(saved.tabs.contains_key("window:a2"));
        assert!(saved.workspaces.contains_key("session:a"));
        assert_never_starts_a_server(&fake);
    }

    #[test]
    fn jumps_to_another_session_ask_the_app_to_show_it() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), Some("client:ipad")));
        focus_answer(
            &fake,
            "session:b",
            "window:b1",
            &["client:mac", "client:ipad"],
        );
        fake.answer("session.select", "");
        let target = FocusTarget::Pane("block:b2".into());
        assert_eq!(host.focus(&target).unwrap(), FocusOutcome::Done);
        let select = fake.calls_to("session.select");
        assert_eq!(select.len(), 1);
        let argv = &select[0].argv;
        let client = argv.iter().position(|a| a == "-C").unwrap();
        assert_eq!(argv[client + 1], "client:ipad");
        assert!(argv.contains(&"session_id=session:b".to_string()));
        assert!(argv.contains(&"window_id=window:b1".to_string()));
        let saved = host.history().load().unwrap();
        assert!(saved.panes.contains_key("block:b2"));
        assert!(saved.workspaces.contains_key("session:b"));
        assert_never_starts_a_server(&fake);
    }

    #[test]
    fn a_refused_session_switch_keeps_the_navigator_open_and_says_why() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), Some("client:mac")));
        focus_answer(&fake, "session:b", "window:b1", &["client:mac"]);
        fake.answer_with(
            "session.select",
            "",
            "Error: Rex.app is not accepting actions. Turn on Remote Control in its Settings under Rex Server.",
            1,
        );
        let outcome = host.focus(&FocusTarget::Tab("window:b1".into())).unwrap();
        let FocusOutcome::Stay(notice) = outcome else {
            panic!("expected the navigator to stay open, got {outcome:?}");
        };
        assert!(notice.contains(REMOTE_CONTROL), "{notice}");
        assert!(notice.chars().count() <= 60, "fits a popup line: {notice}");
        assert_eq!(fake.calls_to("session.select").len(), 1, "sent once");
        assert_never_arrived(&host);
    }

    #[test]
    fn a_session_switch_failing_otherwise_says_what_rex_said() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), Some("client:mac")));
        focus_answer(&fake, "session:b", "window:b1", &["client:mac"]);
        fake.answer_with("session.select", "", "Error: window not found", 1);
        let outcome = host.focus(&FocusTarget::Tab("window:b1".into())).unwrap();
        let FocusOutcome::Stay(notice) = outcome else {
            panic!("expected the navigator to stay open, got {outcome:?}");
        };
        assert!(notice.contains("window not found"), "{notice}");
        assert!(!notice.contains(REMOTE_CONTROL), "{notice}");
        assert_never_arrived(&host);
    }

    /// A jump that did not reach another session leaves recent locations alone.
    fn assert_never_arrived(host: &RexHost) {
        let saved = host.history().load().unwrap();
        assert!(!saved.workspaces.contains_key("session:b"), "{saved:?}");
        assert!(!saved.tabs.contains_key("window:b1"), "{saved:?}");
        assert!(saved.panes.is_empty(), "{saved:?}");
    }

    #[test]
    fn without_a_clear_app_the_session_switch_is_skipped_with_a_notice() {
        for (client, apps) in [
            (None, vec![]),
            (None, vec!["client:mac", "client:ipad"]),
            (Some("client:cli"), vec!["client:mac", "client:ipad"]),
        ] {
            let fake = FakeRex::new();
            let dir = tempfile::tempdir().unwrap();
            let host = RexHost::new(settings(&fake, dir.path(), client));
            focus_answer(&fake, "session:b", "window:b1", &apps);
            fake.answer("session.select", "");
            let outcome = host
                .focus(&FocusTarget::Workspace("session:b".into()))
                .unwrap();
            let FocusOutcome::Stay(notice) = outcome else {
                panic!("{client:?} {apps:?}: expected a notice, got {outcome:?}");
            };
            assert!(notice.contains(WHICH_APP), "{notice}");
            assert!(fake.calls_to("session.select").is_empty());
            assert_never_arrived(&host);
        }
    }

    #[test]
    fn focus_failures_are_errors_not_notices() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), None));
        fake.answer_with("focus", "", "block not found", 1);
        let error = host
            .focus(&FocusTarget::Pane("block:gone".into()))
            .unwrap_err();
        assert!(format!("{error:#}").contains("block not found"));
        assert!(fake.calls_to("session.select").is_empty());
    }

    /// A snapshot answer with session `a` (origin, plus the popup) and session `b`.
    fn snapshot_answer() -> Value {
        let block = |id: &str, label: &str| {
            json!({"block_id": id, "creator_name": "com.superlogical.terminal",
                   "flavor": "com.superlogical.terminal.shell", "label": label})
        };
        let shell = json!({"process": {"foreground": {"name": "zsh", "pid": 9}, "ancestors": {}},
                           "status": {"records": {}}});
        json!({"host_id": HOST, "sessions": [
            {"view": {"session_id": "session:a", "label": "app", "active_window_id": "window:a1",
                      "windows": [{"window_id": "window:a1", "label": "main", "layers": [
                          {"kind": "tiled", "blocks": [block("block:a1", "shell")]},
                          {"kind": "floating", "blocks": [block("block:popup", "Canopy")]}]}]},
             "blocks": {"block:a1": shell, "block:popup": shell}},
            {"view": {"session_id": "session:b", "label": "app-feature", "active_window_id": "window:b1",
                      "windows": [{"window_id": "window:b1", "label": "main", "layers": [
                          {"kind": "tiled", "blocks": [block("block:b1", "shell")]}]}]},
             "blocks": {"block:b1": shell}}
        ]})
    }

    #[test]
    fn snapshots_hide_the_popup_nest_bound_worktrees_and_report_a_missing_watcher() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let bindings = dir.path().join("bindings.json");
        let entry = |linked: bool| {
            json!({"repo_key": "/r/.git", "repo_name": "app", "branch": null,
                   "is_linked_worktree": linked, "bound_at": 1})
        };
        let text = json!({"version": 1, "hosts": {HOST: {"sessions": {
            "session:a": entry(false), "session:b": entry(true)}}}});
        std::fs::write(&bindings, text.to_string()).unwrap();
        let mut settings = settings(&fake, dir.path(), None);
        settings.bindings = Some(bindings);
        let state = settings.state_dir.clone();
        let host = RexHost::new(settings);
        fake.answer("snapshot", &snapshot_answer().to_string());

        let data = host.snapshot().unwrap();
        let panes: Vec<_> = data.workspaces[0].tabs[0]
            .panes
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(panes, ["block:a1"]);
        assert!(
            data.workspaces[1]
                .worktree
                .as_ref()
                .is_some_and(|w| w.is_linked_worktree)
        );
        let notice = data.notice.expect("watcher notice");
        assert!(notice.contains("watcher is not running"), "{notice}");

        std::fs::create_dir_all(&state).unwrap();
        let _watcher = watch::ensure(&state, "build", Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(host.snapshot().unwrap().notice, None);
        assert_never_starts_a_server(&fake);
    }

    #[test]
    fn keys_and_colors_come_from_canopys_rex_config_file() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(&fake, dir.path(), None);
        let config = settings.config.clone();
        let host = RexHost::new(settings);
        assert_eq!(
            host.keybindings().unwrap().move_up,
            Keybindings::default().move_up
        );
        assert_eq!(host.palette().accent, Palette::default().accent);
        std::fs::write(
            &config,
            "[keybindings]\nmove_up = ['C-k']\n[theme.custom]\naccent = \"#5CCEFF\"\n",
        )
        .unwrap();
        assert_eq!(host.keybindings().unwrap().move_up, ["C-k"]);
        assert_eq!(host.palette().accent, Color::Rgb(92, 206, 255));
        std::fs::write(&config, "[keybindings\n").unwrap();
        assert!(host.keybindings().is_err());
    }

    /// A stand-in for this binary that logs how it was started.
    fn fake_bin(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("canopy-bin");
        let log = dir.join("canopy-bin.log");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn launch_request(fake: &FakeRex, dir: &Path, view: View) -> Launch {
        Launch {
            rex: Rex::new(fake.path.clone()),
            bin: fake_bin(dir),
            view,
            session: "session:a".into(),
            block: "block:a1".into(),
            client: Some("client:mac".into()),
            config: None,
            size: None,
            state_root: dir.join("state"),
            claim_timeout: Duration::from_secs(5),
        }
    }

    /// The origin block fills a window of 200x50 cells of 10x20 px.
    fn prepare_answer(fake: &FakeRex, window: &str) {
        let answer = json!({"host_id": HOST, "window_id": window, "rect": [0, 0, 1, 1],
                            "size": {"columns": 200, "rows": 50,
                                     "cell_width_px": 10, "cell_height_px": 20}});
        fake.answer("prepare", &answer.to_string());
    }

    fn popup_args(spawn: &Value) -> UiArgs {
        let command: Vec<String> = serde_json::from_value(spawn["command"].clone()).unwrap();
        let parsed = <crate::cli::Cli as clap::Parser>::try_parse_from(&command).unwrap();
        let Some(crate::cli::Command::Rex(RexCommand::Ui(ui))) = parsed.command else {
            panic!("not a popup command: {command:?}");
        };
        *ui
    }

    fn spawn_answer(fake: &FakeRex) {
        fake.answer(
            "spawn",
            r#"{"layer_id":"layer:p","block_id":"block:popup"}"#,
        );
        fake.answer("layer", "{}");
    }

    /// Plays the popup process: after the n-th spawn call it claims the lock with
    /// `keys[n]` and holds it until asked to close.
    fn play_popups(fake: &FakeRex, path: &Path, keys: &[String]) {
        for (n, key) in keys.iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while fake.calls_to("spawn").len() <= n {
                assert!(Instant::now() < deadline, "popup {n} was never spawned");
                thread::sleep(Duration::from_millis(5));
            }
            let popup = PopupState::claim(path, key).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !popup.close_requested() {
                assert!(Instant::now() < deadline, "popup {n} was never closed");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn popup_lock(request: &Launch) -> PathBuf {
        popup_path(&request.state_root.join(format!("rex-{HOST}")))
    }

    fn assert_bounds(args: &Value, expected: [f64; 4]) {
        let bounds: Vec<f64> = serde_json::from_value(args["bounds"].clone()).unwrap();
        assert_eq!(bounds.len(), 4);
        for (got, want) in bounds.iter().zip(expected) {
            assert!((got - want).abs() < 1e-9, "{bounds:?} != {expected:?}");
        }
    }

    #[test]
    fn a_toggle_opens_a_centred_popup_in_the_origin_window_then_closes_it() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let request = launch_request(&fake, dir.path(), View::Tree);
        let path = popup_lock(&request);
        let keys = [view_key(View::Tree, "window:a1")];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            assert!(
                popup::is_open(&path).unwrap(),
                "returns while the popup runs"
            );
            launch(&request, true).unwrap();
        });
        assert!(!popup::is_open(&path).unwrap());
        let prepare = fake.calls_to("prepare");
        assert!(prepare[0].argv.iter().any(|a| a.contains("block:a1")));
        let spawns = fake.calls_to("spawn");
        assert_eq!(spawns.len(), 1);
        let args = args_of(&spawns[0]);
        assert_eq!(args["window_id"], "window:a1");
        assert_eq!(args["label"], "Canopy");
        assert_bounds(&args, [0.33625, 0.195, 0.3275, 0.61]);
        let command: Vec<String> = serde_json::from_value(args["command"].clone()).unwrap();
        assert_eq!(command[0], request.bin.to_str().unwrap());
        assert_eq!(command[1..3], ["rex", "ui"]);
        for expected in [
            HOST,
            "session:a",
            "window:a1",
            "block:a1",
            "client:mac",
            "tree",
        ] {
            assert!(
                command.iter().any(|a| a == expected),
                "{expected} in {command:?}"
            );
        }
        assert_never_starts_a_server(&fake);
    }

    #[test]
    fn a_measured_frame_shapes_the_popup_which_learns_against_the_same_window() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let request = launch_request(&fake, dir.path(), View::Tree);
        let state_dir = request.state_root.join(format!("rex-{HOST}"));
        std::fs::create_dir_all(&state_dir).unwrap();
        let frame = layer::Frame {
            cell: (10.0, 20.0),
            frame: (5.0, 40.0),
        };
        frame.save(&state_dir).unwrap();
        let path = popup_lock(&request);
        let keys = [view_key(View::Tree, "window:a1")];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            launch(&request, true).unwrap();
        });
        let args = args_of(&fake.calls_to("spawn")[0]);
        assert_bounds(&args, [0.335, 0.175, 0.33, 0.65]);
        let ui = popup_args(&args);
        assert_eq!(ui.window_px, Some((2000.0, 1000.0)));
        let share = ui.share.unwrap();
        assert!(
            (share.0 - 0.33).abs() < 1e-9 && (share.1 - 0.65).abs() < 1e-9,
            "{share:?}"
        );
    }

    #[test]
    fn an_origin_without_pixel_sizes_gets_a_popup_sized_in_cells_that_learns_nothing() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let answer = json!({"host_id": HOST, "window_id": "window:a1", "rect": [0, 0, 1, 1],
                            "size": {"columns": 200, "rows": 50}});
        fake.answer("prepare", &answer.to_string());
        spawn_answer(&fake);
        let request = launch_request(&fake, dir.path(), View::Tree);
        let path = popup_lock(&request);
        let keys = [view_key(View::Tree, "window:a1")];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            launch(&request, true).unwrap();
        });
        let args = args_of(&fake.calls_to("spawn")[0]);
        assert_bounds(&args, [0.33625, 0.195, 0.3275, 0.61]);
        let ui = popup_args(&args);
        assert_eq!((ui.window_px, ui.share), (None, None));
    }

    /// A popup launched for 65x30 cells of 10x20 px in a 2000x1000 px window.
    fn placement() -> Placement {
        Placement {
            window: (2000.0, 1000.0),
            share: (0.3275, 0.61),
        }
    }

    fn sized(cells: (u16, u16)) -> AppSize {
        AppSize {
            cells,
            cell: Some((10.0, 20.0)),
        }
    }

    #[test]
    fn a_popup_more_than_a_cell_off_is_resized_and_its_frame_kept() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        fake.answer("layer", r#"{"layer_id":"layer:p","bounds":[0,0,1,1]}"#);
        let fitted = fit(
            &Rex::new(fake.path.clone()),
            "session:a",
            "block:popup",
            (65, 30),
            sized((60, 28)),
            placement(),
            dir.path(),
        );
        assert!(fitted);
        let calls = fake.calls_to("layer");
        assert_eq!(calls.len(), 2);
        assert_eq!(args_of(&calls[0])["action"], "find");
        let resize = args_of(&calls[1]);
        assert_eq!(
            (&resize["action"], &resize["layer_id"]),
            (&json!("resize"), &json!("layer:p"))
        );
        assert_bounds(&resize, [0.32375, 0.175, 0.3525, 0.65]);
        let kept = layer::Frame::load(dir.path()).unwrap();
        assert_eq!(kept.cell, (10.0, 20.0));
        assert!(
            (kept.frame.0 - 50.0).abs() < 1e-9 && (kept.frame.1 - 40.0).abs() < 1e-9,
            "{kept:?}"
        );
    }

    #[test]
    fn a_popup_a_cell_off_is_left_as_it_is_but_its_frame_kept() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let fitted = fit(
            &Rex::new(fake.path.clone()),
            "session:a",
            "block:popup",
            (65, 30),
            sized((64, 30)),
            placement(),
            dir.path(),
        );
        assert!(!fitted);
        assert!(fake.calls_to("layer").is_empty());
        assert!(layer::Frame::load(dir.path()).is_some());
    }

    #[test]
    fn a_popup_at_its_size_is_left_as_it_is_and_only_a_first_frame_kept() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let rex = Rex::new(fake.path.clone());
        let fit_at_size = || {
            fit(
                &rex,
                "session:a",
                "block:popup",
                (65, 30),
                sized((65, 30)),
                placement(),
                dir.path(),
            )
        };
        assert!(!fit_at_size());
        let first = layer::Frame::load(dir.path()).expect("a first frame is kept");
        let stored = layer::Frame {
            cell: (10.0, 20.0),
            frame: (3.0, 7.0),
        };
        stored.save(dir.path()).unwrap();
        assert!(!fit_at_size());
        assert_eq!(
            layer::Frame::load(dir.path()),
            Some(stored),
            "a stored frame stays"
        );
        assert_ne!(first, stored);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_popup_without_pixel_sizes_is_not_fitted() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let cells_only = AppSize {
            cells: (40, 10),
            cell: None,
        };
        let fitted = fit(
            &Rex::new(fake.path.clone()),
            "session:a",
            "block:popup",
            (65, 30),
            cells_only,
            placement(),
            dir.path(),
        );
        assert!(!fitted);
        assert!(fake.calls().is_empty());
        assert_eq!(layer::Frame::load(dir.path()), None);
    }

    #[test]
    fn the_focused_row_is_a_slight_lift_of_the_terminal_background() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let mut settings = settings(&fake, dir.path(), None);
        settings.terminal_colors = TerminalColors {
            text: Some((231, 234, 238)),
            background: Some((29, 34, 40)),
            color16: None,
        };
        let host = RexHost::new(settings);
        assert_eq!(
            host.palette().selection,
            crate::theme::lift((29, 34, 40), (231, 234, 238), ROW_LIFT)
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[theme.custom]\nactive_row_bg = \"#2C313A\"",
        )
        .unwrap();
        assert_eq!(
            host.palette().selection,
            Color::Rgb(44, 49, 58),
            "configured"
        );
    }

    #[test]
    fn the_claude_logo_is_the_themes_orange_when_it_sets_palette_color_16() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let peach = |color16| {
            let mut settings = settings(&fake, dir.path(), None);
            settings.terminal_colors.color16 = color16;
            RexHost::new(settings).palette().peach
        };
        assert_eq!(
            peach(Some((255, 152, 88))),
            Color::Indexed(16),
            "the theme's orange"
        );
        // Themes that leave color 16 alone keep xterm's black there.
        assert_eq!(peach(Some((0, 0, 0))), Palette::default().peach);
        assert_eq!(peach(None), Palette::default().peach, "not reported");
        std::fs::write(
            dir.path().join("config.toml"),
            "[theme.custom]\npeach = \"#556677\"",
        )
        .unwrap();
        assert_eq!(
            peach(Some((255, 152, 88))),
            Color::Rgb(85, 102, 119),
            "configured"
        );
    }

    #[test]
    fn a_terminal_that_reports_no_colors_keeps_the_ansi_row_color() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), None));
        assert_eq!(host.palette().selection, Palette::default().selection);
    }

    #[test]
    fn a_terminal_is_sized_once_it_reports_pixels() {
        // Until an app owns the new block, it has the placeholder size and no
        // pixels; drawing then would show a frame of the wrong size first.
        let size = |columns, rows, width, height| crossterm::terminal::WindowSize {
            columns,
            rows,
            width,
            height,
        };
        assert_eq!(app_size(&size(80, 24, 0, 0)), None);
        assert_eq!(
            app_size(&size(80, 24, 1360, 888)),
            Some(AppSize {
                cells: (80, 24),
                cell: Some((17.0, 37.0)),
            })
        );
        // A terminal that never reports pixels is sized once it leaves the placeholder.
        assert_eq!(
            app_size(&size(65, 30, 0, 0)),
            Some(AppSize {
                cells: (65, 30),
                cell: None,
            })
        );
    }

    #[test]
    fn a_wait_ends_at_the_first_answer_or_the_timeout() {
        let mut probes = vec![Some(3), None, None];
        assert_eq!(
            wait_for(|| probes.pop().unwrap(), Duration::from_secs(5)),
            Some(3)
        );
        assert!(probes.is_empty());
        let started = Instant::now();
        assert_eq!(wait_for(|| None::<()>, Duration::from_millis(50)), None);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_color_query_ends_at_the_terminals_device_attributes() {
        assert!(device_attributes_answered(
            b"\x1b]11;rgb:0e/11/17\x07\x1b[?62;22c"
        ));
        assert!(device_attributes_answered(b"\x1b[?1;2c"));
        assert!(!device_attributes_answered(b"\x1b]11;rgb:0e/11/17\x07"));
        assert!(!device_attributes_answered(b"\x1b[?62;2"));
    }

    #[test]
    fn the_popup_background_is_the_terminals_own_unless_configured() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        let host = RexHost::new(settings(&fake, dir.path(), None));
        assert_eq!(host.palette().background, Color::Reset);
        std::fs::write(
            dir.path().join("config.toml"),
            "[theme.custom]\nsidebar_bg = \"#171B20\"",
        )
        .unwrap();
        assert_eq!(host.palette().background, Color::Rgb(23, 27, 32));
    }

    #[test]
    fn the_popup_uses_the_state_dir_its_launcher_chose() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let request = launch_request(&fake, dir.path(), View::Tree);
        let path = popup_lock(&request);
        let keys = [view_key(View::Tree, "window:a1")];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            launch(&request, true).unwrap();
        });
        let command: Vec<String> =
            serde_json::from_value(args_of(&fake.calls_to("spawn")[0])["command"].clone()).unwrap();
        let parsed = <crate::cli::Cli as clap::Parser>::try_parse_from(&command).unwrap();
        let Some(crate::cli::Command::Rex(RexCommand::Ui(args))) = parsed.command else {
            panic!("not a popup command: {command:?}");
        };
        let launcher_dir = request.state_root.join(format!("rex-{HOST}"));
        // The popup's own environment points elsewhere, or nowhere.
        let elsewhere = Ok(dir.path().join("other-root"));
        assert_eq!(
            popup_state_dir(args.state_dir.clone(), elsewhere, &args.host_id).unwrap(),
            launcher_dir
        );
        let nowhere = Err(anyhow::anyhow!("HOME is not set"));
        assert_eq!(
            popup_state_dir(args.state_dir, nowhere, &args.host_id).unwrap(),
            launcher_dir
        );
        // Without a hand-off, the popup falls back to its own environment.
        let root = dir.path().join("own-root");
        assert_eq!(
            popup_state_dir(None, Ok(root.clone()), HOST).unwrap(),
            root.join(format!("rex-{HOST}"))
        );
    }

    #[test]
    fn a_toggle_from_another_window_or_session_moves_the_popup_there() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let mut request = launch_request(&fake, dir.path(), View::Tree);
        let path = popup_lock(&request);
        let keys = [
            view_key(View::Tree, "window:a1"),
            view_key(View::Tree, "window:b1"),
        ];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            request.session = "session:b".into();
            request.block = "block:b1".into();
            prepare_answer(&fake, "window:b1");
            launch(&request, true).unwrap();
            assert!(popup::is_open(&path).unwrap(), "replaced, not just closed");
            launch(&request, true).unwrap();
        });
        let spawns = fake.calls_to("spawn");
        assert_eq!(spawns.len(), 2);
        assert_eq!(args_of(&spawns[1])["window_id"], "window:b1");
        assert!(!popup::is_open(&path).unwrap());
    }

    #[test]
    fn open_keeps_the_popup_already_open_in_the_same_window() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let request = launch_request(&fake, dir.path(), View::Agents);
        let path = popup_lock(&request);
        let keys = [view_key(View::Agents, "window:a1")];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, false).unwrap();
            launch(&request, false).unwrap();
            assert_eq!(fake.calls_to("spawn").len(), 1);
            launch(&request, true).unwrap();
        });
        assert!(!popup::is_open(&path).unwrap());
    }

    #[test]
    fn the_agents_view_and_custom_sizes_shape_the_popup() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let mut request = launch_request(&fake, dir.path(), View::Agents);
        request.size = Some((100, 25));
        let path = popup_lock(&request);
        let keys = [view_key(View::Agents, "window:a1")];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            launch(&request, true).unwrap();
        });
        let args = args_of(&fake.calls_to("spawn")[0]);
        assert_eq!(args["label"], "Canopy Agents");
        assert_bounds(&args, [0.24875, 0.245, 0.5025, 0.51]);
    }

    #[test]
    fn a_popup_that_never_starts_has_its_layer_closed() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let mut request = launch_request(&fake, dir.path(), View::Tree);
        request.claim_timeout = Duration::from_millis(200);
        assert!(launch(&request, true).is_err());
        let closes = fake.calls_to("layer");
        assert_eq!(closes.len(), 1);
        assert!(closes[0].argv.iter().any(|a| a.contains("layer:p")));
        assert!(!popup::is_open(&popup_lock(&request)).unwrap());
    }

    #[test]
    fn opening_a_popup_starts_a_missing_watcher() {
        let fake = FakeRex::new();
        let dir = tempfile::tempdir().unwrap();
        prepare_answer(&fake, "window:a1");
        spawn_answer(&fake);
        let request = launch_request(&fake, dir.path(), View::Tree);
        let path = popup_lock(&request);
        let log = dir.path().join("canopy-bin.log");
        let watcher_starts = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|line| line.starts_with("rex watch --ensure"))
                .count()
        };
        let keys = [
            view_key(View::Tree, "window:a1"),
            view_key(View::Tree, "window:a1"),
        ];
        thread::scope(|s| {
            s.spawn(|| play_popups(&fake, &path, &keys));
            launch(&request, true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while watcher_starts() == 0 {
                assert!(Instant::now() < deadline, "the watcher was not started");
                thread::sleep(Duration::from_millis(10));
            }
            let state = request.state_root.join(format!("rex-{HOST}"));
            let _running = watch::ensure(&state, "build", Duration::from_secs(1))
                .unwrap()
                .unwrap();
            for _ in 0..3 {
                launch(&request, true).unwrap();
            }
            thread::sleep(Duration::from_millis(300));
            assert_eq!(watcher_starts(), 1, "a running watcher is left alone");
        });
    }
}
