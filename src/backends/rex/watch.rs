//! The watcher: one process per Rex server that turns server events into recent
//! locations (MRU) and Claude/Codex working periods.

use super::cli::Rex;
use super::decode::BlockInfo;
use crate::history::{Tracker, save_state, unix_millis};
use crate::models::{ActiveLocation, AgentProvider, AgentStatus, View};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A Claude/Codex working period of one block, as shown by its timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Period {
    pub provider: AgentProvider,
    /// The agent process that reported working.
    pub owner_pid: u32,
    /// When the agent reported it started working (Rex `updated_at`).
    pub started_ms: u64,
}

/// `working.json`: periods by block id, and when the watcher was last alive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct WorkingState {
    pub alive_ms: u64,
    pub periods: HashMap<String, Period>,
}

impl WorkingState {
    /// The saved state; missing means empty.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid working-time state"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Periods survive a watcher restart only if it was down for less than this.
const MAX_OUTAGE_MS: u64 = 15_000;
const HEARTBEAT_MS: u64 = 10_000;

/// One event line from the watch script.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    Hello {
        host_id: String,
    },
    /// The startup baseline is complete.
    Ready,
    Focus {
        session: String,
        window: String,
        block: Option<String>,
        #[serde(default)]
        label: String,
        #[serde(default)]
        floating: bool,
        #[serde(default)]
        baseline: bool,
    },
    Block {
        session: String,
        window: String,
        block: String,
        #[serde(flatten)]
        info: BlockInfo,
        #[serde(default)]
        baseline: bool,
    },
    Closed {
        block: String,
    },
    SessionGone {
        session: String,
    },
}

/// Parses one NDJSON line; unknown kinds yield `None`.
pub fn parse_line(text: &str) -> Result<Option<Line>> {
    const KINDS: [&str; 7] = [
        "hello",
        "ready",
        "focus",
        "block",
        "closed",
        "session_gone",
        "error",
    ];
    let value: serde_json::Value = serde_json::from_str(text).context("Invalid watch line")?;
    match value.get("kind").and_then(|k| k.as_str()) {
        Some("error") => anyhow::bail!("Watch script: {}", value["message"]),
        Some(kind) if KINDS.contains(&kind) => Ok(Some(
            serde_json::from_value(value).context("Invalid watch line")?,
        )),
        _ => Ok(None),
    }
}

/// What the watcher must do after a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Record a focus change in the MRU history.
    Record(ActiveLocation),
    /// Record this location again only if it is still the most recent one, so a
    /// completion in front of the user counts as seen.
    RecordIfNewest(ActiveLocation),
    /// Write `working.json`.
    SavePeriods,
}

/// What the watcher last saw of a block.
struct Tracked {
    session: String,
    status: AgentStatus,
    owner_pid: Option<u32>,
    /// The finished/failed report last announced, so it is announced once.
    finished: Option<(AgentStatus, Option<u64>)>,
}

/// The watcher's state machine; time comes from the caller.
pub struct Watch {
    /// Each session's `(window, block)` focus.
    focus: HashMap<String, (String, String)>,
    blocks: HashMap<String, Tracked>,
    periods: HashMap<String, Period>,
    /// Saved periods waiting for the baseline to confirm them.
    restored: HashMap<String, Period>,
    last_heartbeat: u64,
}

impl Watch {
    /// Starts from the saved state; periods survive only a short outage, and only
    /// for blocks the baseline still shows working with the same process.
    pub fn new(saved: WorkingState, now_ms: u64) -> Self {
        let fresh = now_ms.saturating_sub(saved.alive_ms) <= MAX_OUTAGE_MS;
        Self {
            focus: HashMap::new(),
            blocks: HashMap::new(),
            periods: HashMap::new(),
            restored: if fresh { saved.periods } else { HashMap::new() },
            last_heartbeat: now_ms,
        }
    }

    pub fn apply(&mut self, line: Line, now_ms: u64) -> Vec<Effect> {
        match line {
            Line::Hello { .. } => Vec::new(),
            Line::Ready => {
                self.restored.clear();
                vec![Effect::SavePeriods]
            }
            Line::Focus {
                session,
                window,
                block,
                label,
                floating,
                baseline,
            } => {
                let is_popup = floating
                    && [View::Tree, View::Agents]
                        .iter()
                        .any(|view| view.title() == label);
                let Some(block) = block.filter(|_| !is_popup) else {
                    return Vec::new();
                };
                let pair = (window, block);
                if self.focus.get(&session) == Some(&pair) {
                    return Vec::new();
                }
                self.focus.insert(session.clone(), pair.clone());
                if baseline {
                    return Vec::new();
                }
                vec![Effect::Record(location(session, pair.0, pair.1))]
            }
            Line::Block {
                session,
                window,
                block,
                info,
                baseline,
            } => self.block(session, window, block, &info, baseline, now_ms),
            Line::Closed { block } => {
                self.blocks.remove(&block);
                self.forget(&[block])
            }
            Line::SessionGone { session } => {
                self.focus.remove(&session);
                let gone: Vec<String> = self
                    .blocks
                    .iter()
                    .filter(|(_, tracked)| tracked.session == session)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in &gone {
                    self.blocks.remove(id);
                }
                self.forget(&gone)
            }
        }
    }

    fn forget(&mut self, blocks: &[String]) -> Vec<Effect> {
        let removed = blocks
            .iter()
            .filter(|id| self.periods.remove(*id).is_some())
            .count();
        if removed > 0 {
            vec![Effect::SavePeriods]
        } else {
            Vec::new()
        }
    }

    fn block(
        &mut self,
        session: String,
        window: String,
        block: String,
        info: &BlockInfo,
        baseline: bool,
        now_ms: u64,
    ) -> Vec<Effect> {
        // Only live records are timed, so Claude's job files are not read here.
        let state = info.agent_state(None, &|_, _| None);
        let status = state.as_ref().map_or(AgentStatus::Unknown, |s| s.status);
        let record = state.as_ref().and_then(|s| s.record);
        let owner_pid = record.map(|r| r.owner_pid);
        let timed = state
            .as_ref()
            .and_then(|s| AgentProvider::from_name(s.agent))
            .filter(|p| p.supports_timing());
        let working = status == AgentStatus::Working;
        let finished = record
            .filter(|r| r.clears_when_seen)
            .map(|r| (status, r.updated_ms));
        let previous = self.blocks.insert(
            block.clone(),
            Tracked {
                session: session.clone(),
                status,
                owner_pid,
                finished,
            },
        );
        if baseline {
            if let (Some(provider), Some(pid), Some(period)) =
                (timed, owner_pid, self.restored.remove(&block))
                && working
                && period.provider == provider
                && period.owner_pid == pid
            {
                self.periods.insert(block, period);
            }
            return Vec::new();
        }
        let mut effects = Vec::new();
        if let Some(finished) = finished
            && previous.as_ref().and_then(|p| p.finished) != Some(finished)
        {
            effects.push(Effect::RecordIfNewest(location(
                session,
                window,
                block.clone(),
            )));
        }
        match (working, timed, owner_pid) {
            (true, Some(provider), Some(pid)) => {
                let same = |p: &Period| p.provider == provider && p.owner_pid == pid;
                let continues = previous
                    .as_ref()
                    .is_some_and(|p| p.status == AgentStatus::Working && p.owner_pid == Some(pid));
                if self.periods.get(&block).is_some_and(same) || continues {
                    return effects;
                }
                let started_ms = record.and_then(|r| r.updated_ms).unwrap_or(now_ms);
                self.periods.insert(
                    block,
                    Period {
                        provider,
                        owner_pid: pid,
                        started_ms,
                    },
                );
                effects.push(Effect::SavePeriods);
            }
            _ => effects.extend(self.forget(&[block])),
        }
        effects
    }

    /// Heartbeat: asks for a save when the last one is ten seconds old.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Effect> {
        if now_ms.saturating_sub(self.last_heartbeat) < HEARTBEAT_MS {
            return Vec::new();
        }
        self.last_heartbeat = now_ms;
        vec![Effect::SavePeriods]
    }

    /// What to write to `working.json` now.
    pub fn state(&self, now_ms: u64) -> WorkingState {
        WorkingState {
            alive_ms: now_ms,
            periods: self.periods.clone(),
        }
    }
}

fn location(session: String, window: String, block: String) -> ActiveLocation {
    ActiveLocation {
        workspace_id: Some(session),
        tab_id: Some(window),
        pane_id: Some(block),
    }
}

/// The single-instance lock of a server's watcher (`watch.lock` in its state dir).
pub struct WatchLock {
    /// Held with an exclusive `flock` for the watcher's lifetime.
    _file: File,
    stop_path: std::path::PathBuf,
    token: String,
}

impl WatchLock {
    /// True once another starter asked this watcher to stop (a newer binary).
    pub fn stop_requested(&self) -> bool {
        fs::read_to_string(&self.stop_path).ok().as_deref() == Some(self.token.as_str())
    }
}

fn open_lock(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir)?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("watch.lock"))?)
}

/// The lock owner's token: `<pid>-<nanos> <identity>`.
fn owner(file: &mut File) -> Result<String> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0))?;
    file.take(512).read_to_string(&mut text)?;
    Ok(text)
}

fn try_take(mut file: File, dir: &Path, identity: &str) -> Result<Option<WatchLock>> {
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let token = format!("{}-{nanos} {identity}", std::process::id());
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(token.as_bytes())?;
    file.flush()?;
    Ok(Some(WatchLock {
        _file: file,
        stop_path: dir.join("watch.stop"),
        token,
    }))
}

/// Takes the watcher lock in `dir` for a binary with `identity`. `None` means a
/// watcher with the same identity already runs. A watcher with another identity
/// is asked to stop and given `wait` to release the lock.
pub fn ensure(dir: &Path, identity: &str, wait: Duration) -> Result<Option<WatchLock>> {
    let mut file = open_lock(dir)?;
    if let Some(lock) = try_take(file.try_clone()?, dir, identity)? {
        return Ok(Some(lock));
    }
    let token = owner(&mut file)?;
    // An empty token is a watcher still starting: treat it as running.
    if token.is_empty() || token.split_once(' ').is_some_and(|(_, id)| id == identity) {
        return Ok(None);
    }
    fs::write(dir.join("watch.stop"), &token)?;
    let deadline = Instant::now() + wait;
    loop {
        if let Some(lock) = try_take(file.try_clone()?, dir, identity)? {
            return Ok(Some(lock));
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "The running Canopy watcher did not stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a watcher holds the lock in `dir`.
pub fn is_running(dir: &Path) -> Result<bool> {
    let file = match File::open(dir.join("watch.lock")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// This binary's identity: a rebuilt binary has a different one.
pub fn identity() -> Result<String> {
    let meta = fs::metadata(std::env::current_exe()?)?;
    let mtime = meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!("{}-{mtime}", meta.len()))
}

const WATCH_SCRIPT: &str = include_str!("scripts/watch.lua");

/// Runs the watcher for the server in `dir` until the server goes away or a
/// newer binary asks it to stop. Returns at once if one already runs.
pub fn run(rex: &Rex, dir: &Path) -> Result<()> {
    let Some(lock) = ensure(dir, &identity()?, Duration::from_secs(5))? else {
        return Ok(());
    };
    let working = dir.join("working.json");
    let tracker = Tracker::new(dir.join("mru.json"));
    let saved = WorkingState::load(&working).unwrap_or_else(|error| {
        eprintln!("canopy watch: starting without saved periods: {error:#}");
        WorkingState::default()
    });
    let mut watch = Watch::new(saved, unix_millis()?);
    let mut child = rex
        .command()
        .args(["do", "--all", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Cannot start the Rex event stream")?;
    child
        .stdin
        .take()
        .context("rex stdin is missing")?
        .write_all(WATCH_SCRIPT.as_bytes())?;
    let stdout = child.stdout.take().context("rex stdout is missing")?;
    let (lines_tx, lines_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if line.is_err() || lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let result = (|| -> Result<()> {
        loop {
            let effects = match lines_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(line) => match parse_line(&line?) {
                    Ok(Some(line)) => watch.apply(line, unix_millis()?),
                    Ok(None) => Vec::new(),
                    Err(error) => {
                        eprintln!("canopy watch: {error:#}");
                        Vec::new()
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => Vec::new(),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            };
            let now = unix_millis()?;
            for effect in effects.into_iter().chain(watch.tick(now)) {
                let done = match &effect {
                    Effect::Record(location) => tracker.record(location),
                    Effect::RecordIfNewest(location) => {
                        tracker.record_if_newest(location).map(|_| ())
                    }
                    Effect::SavePeriods => save_state(&working, &watch.state(now)),
                };
                if let Err(error) = done {
                    eprintln!("canopy watch: {error:#}");
                }
            }
            if lock.stop_requested() {
                return Ok(());
            }
        }
    })();
    stop(&mut child);
    save_state(&working, &watch.state(unix_millis()?))?;
    result
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::rerun_alone;
    use serde_json::{Value, json};
    use std::thread;
    use std::time::Instant;

    const NOW: u64 = 1_800_000_000_000;
    const T1: &str = "2026-11-12T22:25:55Z";
    const T1_MS: u64 = 1_794_522_355_000;
    const T2: &str = "2026-11-12T22:26:10Z";
    const T2_MS: u64 = 1_794_522_370_000;

    fn line(value: Value) -> Line {
        parse_line(&value.to_string()).unwrap().unwrap()
    }

    fn focus(session: &str, window: &str, block: &str) -> Value {
        json!({"kind": "focus", "session": session, "window": window, "block": block,
               "label": "shell", "floating": false})
    }

    fn baseline(mut value: Value) -> Value {
        value["baseline"] = json!(true);
        value
    }

    /// A block line for `block` in session `s`/window `w` with one status record.
    fn block(block: &str, state: &str, app: &str, pid: u32, at: &str) -> Value {
        let name = if app == "claude-code" { "claude" } else { app };
        json!({"kind": "block", "session": "s", "window": "w", "block": block,
               "process": {"foreground": {"name": name, "argv0": name, "pid": pid}, "ancestors": {}},
               "status": {"records": [{"state": state, "app": app, "effective_app": app,
                                       "owner": {"name": name, "pid": pid}, "updated_at": at}]}})
    }

    fn location(session: &str, window: &str, block: &str) -> ActiveLocation {
        ActiveLocation {
            workspace_id: Some(session.into()),
            tab_id: Some(window.into()),
            pane_id: Some(block.into()),
        }
    }

    /// A watcher that has seen an empty baseline.
    fn started() -> Watch {
        let mut watch = Watch::new(WorkingState::default(), NOW);
        watch.apply(line(json!({"kind": "hello", "host_id": "host"})), NOW);
        watch.apply(line(json!({"kind": "ready"})), NOW);
        watch
    }

    fn records(effects: &[Effect]) -> Vec<&ActiveLocation> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Record(location) => Some(location),
                _ => None,
            })
            .collect()
    }

    fn start_of(watch: &Watch, block: &str) -> Option<u64> {
        watch.state(NOW).periods.get(block).map(|p| p.started_ms)
    }

    #[test]
    fn each_focus_change_is_recorded_once_and_the_startup_baseline_is_not() {
        let mut watch = Watch::new(WorkingState::default(), NOW);
        let effects = watch.apply(line(baseline(focus("s", "w1", "b1"))), NOW);
        assert!(records(&effects).is_empty());
        watch.apply(line(json!({"kind": "ready"})), NOW);
        assert!(records(&watch.apply(line(focus("s", "w1", "b1")), NOW + 1)).is_empty());
        let effects = watch.apply(line(focus("s", "w1", "b2")), NOW + 2);
        assert_eq!(records(&effects), [&location("s", "w1", "b2")]);
        assert!(records(&watch.apply(line(focus("s", "w1", "b2")), NOW + 3)).is_empty());
        let effects = watch.apply(line(focus("s", "w2", "b3")), NOW + 4);
        assert_eq!(records(&effects), [&location("s", "w2", "b3")]);
        let effects = watch.apply(line(focus("t", "w9", "b9")), NOW + 5);
        assert_eq!(records(&effects), [&location("t", "w9", "b9")]);
        let effects = watch.apply(line(focus("s", "w1", "b1")), NOW + 6);
        assert_eq!(records(&effects), [&location("s", "w1", "b1")]);
    }

    #[test]
    fn the_navigator_popup_never_becomes_a_recent_location() {
        let mut watch = started();
        watch.apply(line(focus("s", "w", "b1")), NOW);
        for label in ["Canopy", "Canopy Agents"] {
            let mut popup = focus("s", "w", "block:popup");
            popup["label"] = json!(label);
            popup["floating"] = json!(true);
            assert!(
                records(&watch.apply(line(popup), NOW + 1)).is_empty(),
                "{label}"
            );
            // Closing the popup returns focus to where the user was: no change.
            assert!(records(&watch.apply(line(focus("s", "w", "b1")), NOW + 2)).is_empty());
        }
        let mut named = focus("s", "w", "b2");
        named["label"] = json!("Canopy");
        assert_eq!(
            records(&watch.apply(line(named), NOW + 3)),
            [&location("s", "w", "b2")],
            "a tiled block the user named Canopy"
        );
    }

    #[test]
    fn a_working_period_starts_when_the_agent_says_and_survives_repeated_reports() {
        let mut watch = started();
        let effects = watch.apply(line(block("b", "working", "claude-code", 7, T1)), NOW);
        assert!(effects.contains(&Effect::SavePeriods));
        assert_eq!(start_of(&watch, "b"), Some(T1_MS));
        assert_eq!(
            watch.state(NOW).periods["b"],
            Period {
                provider: AgentProvider::Claude,
                owner_pid: 7,
                started_ms: T1_MS
            }
        );
        watch.apply(line(block("b", "working", "claude-code", 7, T2)), NOW + 1);
        assert_eq!(start_of(&watch, "b"), Some(T1_MS));
    }

    #[test]
    fn leaving_work_closing_the_block_or_losing_the_session_clears_the_timer() {
        let leave = [
            block("b", "idle", "codex", 7, T2),
            json!({"kind": "closed", "block": "b"}),
            json!({"kind": "session_gone", "session": "s"}),
        ];
        for (n, next) in leave.into_iter().enumerate() {
            let mut watch = started();
            watch.apply(line(block("b", "working", "codex", 7, T1)), NOW);
            let effects = watch.apply(line(next), NOW + 1);
            assert_eq!(start_of(&watch, "b"), None, "case {n}");
            assert!(effects.contains(&Effect::SavePeriods), "case {n}");
        }
    }

    #[test]
    fn work_that_began_before_the_watcher_started_is_not_timed() {
        let mut watch = Watch::new(WorkingState::default(), NOW);
        watch.apply(line(baseline(block("b", "working", "codex", 7, T1))), NOW);
        watch.apply(line(json!({"kind": "ready"})), NOW);
        assert_eq!(start_of(&watch, "b"), None);
        watch.apply(line(block("b", "working", "codex", 7, T2)), NOW + 1);
        assert_eq!(start_of(&watch, "b"), None, "still the same unseen start");
        watch.apply(line(block("b", "idle", "codex", 7, T2)), NOW + 2);
        watch.apply(line(block("b", "working", "codex", 7, T2)), NOW + 3);
        assert_eq!(start_of(&watch, "b"), Some(T2_MS));
    }

    #[test]
    fn a_new_agent_process_restarts_the_timer() {
        let mut watch = started();
        watch.apply(line(block("b", "working", "claude-code", 7, T1)), NOW);
        watch.apply(line(block("b", "working", "claude-code", 8, T2)), NOW + 1);
        assert_eq!(start_of(&watch, "b"), Some(T2_MS));
        assert_eq!(watch.state(NOW).periods["b"].owner_pid, 8);
    }

    #[test]
    fn only_claude_and_codex_are_timed() {
        let mut watch = started();
        watch.apply(line(block("g", "working", "gemini", 7, T1)), NOW);
        watch.apply(line(block("u", "working", "wts", 8, T1)), NOW);
        assert!(watch.state(NOW).periods.is_empty());
    }

    #[test]
    fn periods_survive_a_restart_only_after_a_short_outage_and_while_still_working() {
        let period = |pid| Period {
            provider: AgentProvider::Codex,
            owner_pid: pid,
            started_ms: T1_MS,
        };
        let saved = |alive_ms| WorkingState {
            alive_ms,
            periods: HashMap::from([
                ("kept".to_string(), period(1)),
                ("idle".to_string(), period(2)),
                ("replaced".to_string(), period(3)),
                ("gone".to_string(), period(4)),
            ]),
        };
        let restart = |alive_ms| {
            let mut watch = Watch::new(saved(alive_ms), NOW);
            for value in [
                block("kept", "working", "codex", 1, T1),
                block("idle", "idle", "codex", 2, T2),
                block("replaced", "working", "codex", 33, T2),
            ] {
                watch.apply(line(baseline(value)), NOW);
            }
            watch.apply(line(json!({"kind": "ready"})), NOW);
            watch.state(NOW).periods
        };
        let periods = restart(NOW - 5_000);
        assert_eq!(periods.keys().collect::<Vec<_>>(), [&"kept".to_string()]);
        assert_eq!(periods["kept"], period(1));
        assert!(restart(NOW - 20_000).is_empty(), "long outage");
    }

    #[test]
    fn finishing_or_failing_in_front_of_the_user_counts_as_seen() {
        let mut watch = started();
        for state in ["done", "error"] {
            let effects = watch.apply(line(block("b", state, "codex", 7, T2)), NOW);
            assert!(
                effects.contains(&Effect::RecordIfNewest(location("s", "w", "b"))),
                "{state}"
            );
        }
        for state in ["working", "blocked", "idle"] {
            let effects = watch.apply(line(block("b", state, "codex", 7, T2)), NOW);
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::RecordIfNewest(_))),
                "{state}"
            );
        }
        let mut quiet = Watch::new(WorkingState::default(), NOW);
        let effects = quiet.apply(line(baseline(block("b", "done", "codex", 7, T2))), NOW);
        assert!(effects.is_empty(), "baseline done is old news");
    }

    #[test]
    fn the_heartbeat_is_saved_every_ten_seconds() {
        let mut watch = started();
        assert!(watch.tick(NOW + 5_000).is_empty());
        assert_eq!(watch.tick(NOW + 10_000), [Effect::SavePeriods]);
        assert!(watch.tick(NOW + 15_000).is_empty());
        assert_eq!(watch.tick(NOW + 20_000), [Effect::SavePeriods]);
        assert_eq!(watch.state(NOW + 20_000).alive_ms, NOW + 20_000);
    }

    #[test]
    fn lines_accept_lua_empty_tables_and_skip_unknown_kinds() {
        assert_eq!(parse_line(r#"{"kind":"client_connected"}"#).unwrap(), None);
        assert!(parse_line("not json").is_err());
        assert!(
            parse_line(r#"{"kind":"focus"}"#).is_err(),
            "focus without ids"
        );
        let mut watch = started();
        let empty = json!({"kind": "block", "session": "s", "window": "w", "block": "b",
                           "process": {"foreground": {"name": "zsh", "pid": 1}, "ancestors": {}},
                           "status": {"records": {}}});
        watch.apply(line(empty), NOW);
        assert!(watch.state(NOW).periods.is_empty());
    }

    #[test]
    fn one_watcher_per_server_and_a_rebuilt_binary_takes_over() {
        if rerun_alone() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let wait = Duration::from_secs(2);
        assert!(!is_running(dir.path()).unwrap());
        let first = ensure(dir.path(), "build-a", wait).unwrap().unwrap();
        assert!(is_running(dir.path()).unwrap());
        assert!(
            ensure(dir.path(), "build-a", wait).unwrap().is_none(),
            "same binary"
        );
        let holder = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !first.stop_requested() {
                assert!(Instant::now() < deadline, "never asked to stop");
                thread::sleep(Duration::from_millis(5));
            }
        });
        let second = ensure(dir.path(), "build-b", wait).unwrap().unwrap();
        holder.join().unwrap();
        assert!(!second.stop_requested());
        drop(second);
        assert!(!is_running(dir.path()).unwrap());
    }

    #[test]
    fn a_watcher_that_ignores_the_stop_request_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let _stuck = ensure(dir.path(), "build-a", Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(ensure(dir.path(), "build-b", Duration::from_millis(200)).is_err());
        assert!(is_running(dir.path()).unwrap());
    }
}
