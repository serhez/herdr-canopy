//! Turns the snapshot script's JSON into Canopy's models: sessions → workspaces,
//! windows → tabs, blocks → panes, with agent provider, status, "seen" and timers.

use super::claude_jobs::Job;
use super::watch::Period;
use crate::models::{AgentProvider, AgentStatus, History, Pane, Snapshot, Tab, Workspace};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, Error as _};
use serde_json::Value;
use std::collections::HashMap;

/// What decoding needs besides the raw server data.
pub struct DecodeContext<'a> {
    /// Recent locations; a block focused after its agent finished counts as seen.
    pub history: &'a History,
    /// The watcher's working periods by block id.
    pub periods: &'a HashMap<String, Period>,
    /// The navigator's own block, never listed.
    pub exclude_block: Option<&'a str>,
    /// The background agent a Claude process without status records shows, by
    /// pid and the block's title, if that process set it.
    pub viewed_job: &'a dyn Fn(u32, Option<&str>) -> Option<Job>,
}

/// Lua encodes an empty array as `{}`; accept that, `null` and arrays.
pub(super) fn list<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    match Value::deserialize(d)? {
        Value::Null => Ok(Vec::new()),
        Value::Object(map) if map.is_empty() => Ok(Vec::new()),
        Value::Array(items) => items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(D::Error::custom))
            .collect(),
        other => Err(D::Error::custom(format!("expected a list, got {other}"))),
    }
}

#[derive(Deserialize)]
struct RawSnapshot {
    sessions: Vec<RawSession>,
}

#[derive(Deserialize)]
struct RawSession {
    view: RawView,
    #[serde(default)]
    blocks: HashMap<String, BlockInfo>,
}

#[derive(Deserialize)]
struct RawView {
    session_id: String,
    #[serde(default)]
    label: String,
    active_window_id: Option<String>,
    #[serde(default, deserialize_with = "list")]
    windows: Vec<RawWindow>,
}

#[derive(Deserialize)]
struct RawWindow {
    window_id: String,
    #[serde(default)]
    label: String,
    #[serde(default, deserialize_with = "list")]
    layers: Vec<RawLayer>,
}

#[derive(Deserialize)]
struct RawLayer {
    #[serde(default, deserialize_with = "list")]
    blocks: Vec<RawBlock>,
}

#[derive(Deserialize)]
struct RawBlock {
    block_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    flavor: String,
}

/// A terminal block's process, title and program status, as the snapshot and
/// watch scripts report them.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct BlockInfo {
    #[serde(default)]
    process: Processes,
    title: Option<Title>,
    #[serde(default)]
    status: Status,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Processes {
    foreground: Option<Process>,
    #[serde(default, deserialize_with = "list")]
    ancestors: Vec<Process>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Process {
    pid: Option<u32>,
    #[serde(default)]
    name: String,
    argv0: Option<String>,
    path: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Title {
    title: Option<String>,
    /// The process that set the title.
    owner: Option<Owner>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Status {
    #[serde(default, deserialize_with = "list")]
    records: Vec<Record>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Record {
    #[serde(default)]
    state: String,
    app: Option<String>,
    effective_app: Option<String>,
    owner: Option<Owner>,
    updated_at: Option<String>,
    /// What the program says it is doing or asking.
    msg: Option<String>,
    /// What a blocked program waits for: `permission`, `question` or `auth`.
    kind: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Owner {
    pid: Option<u32>,
}

/// Agents Canopy recognizes, as Herdr's name table; the first name is canonical.
const AGENTS: &[&[&str]] = &[
    &["pi"],
    &["claude", "claude-code"],
    &["codex"],
    &["gemini"],
    &["cursor", "cursor-agent"],
    &["devin", "devin-cli", "devin cli"],
    &["agy", "antigravity", "antigravity-cli"],
    &["cline", ".cline"],
    &["omp"],
    &["mastracode", "mastra-code", "mastra code"],
    &["opencode", "opencode2", "open-code"],
    &["copilot", "github-copilot", "ghcs"],
    &["kimi", "kimi-code", "kimi code"],
    &["kiro", "kiro-cli"],
    &["droid"],
    &["amp", "amp-local"],
    &["grok", "grok-build"],
    &["hermes", "hermes-agent"],
    &["kilo", "kilo-code", "kilo code"],
    &["qodercli", "qoderclicn", "qoder", "qodercn"],
    &["qwen", "qwen-code", "qwen code"],
    &["letta", "letta-code", "letta code"],
    &["maki"],
    &["muse", "muse-code", "muse-cli"],
];

/// The canonical agent name for a program or app name, Herdr's normalization:
/// basename, lower case, without a Windows or script suffix.
fn agent_name(name: &str) -> Option<&'static str> {
    let base = name.rsplit(['/', '\\']).next()?.trim().to_lowercase();
    let base = [".exe", ".cmd", ".bat", ".ps1", ".js"]
        .iter()
        .find_map(|suffix| base.strip_suffix(suffix))
        .unwrap_or(&base);
    AGENTS
        .iter()
        .find(|names| names.contains(&base))
        .map(|names| names[0])
}

/// A block's agent and its status before the "seen" rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentState {
    pub agent: &'static str,
    pub status: AgentStatus,
    /// The live record that decided `status`, if any.
    pub record: Option<LiveRecord>,
    /// The agent's process: the deciding record's owner, else the foreground process.
    pub pid: Option<u32>,
    /// What the agent says it is doing or asking.
    pub message: Option<String>,
    /// What a blocked agent waits for (`permission`, `question`, `auth`; for a
    /// Claude background agent, [`super::claude_jobs::REPLY`]).
    pub blocked_kind: Option<String>,
    /// When the record or background agent that decided `status` last changed.
    pub updated_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRecord {
    pub owner_pid: u32,
    /// `done` or `error`: cleared to idle once the user looked at the block.
    pub clears_when_seen: bool,
}

impl BlockInfo {
    /// The foreground process's pid, then its ancestors'; none without a
    /// foreground process.
    pub fn pids(&self) -> Vec<u32> {
        let Some(foreground) = &self.process.foreground else {
            return Vec::new();
        };
        std::iter::once(foreground)
            .chain(&self.process.ancestors)
            .filter_map(|p| p.pid)
            .collect()
    }

    /// The block's agent, from live status records or its foreground process, and
    /// its status. A record is live while its owner is the foreground process or
    /// one of its ancestors. `seen_ms` is when the user last focused the block;
    /// `viewed_job` gives the background agent a silent Claude process shows, by
    /// its pid and the block's title, if that process set it.
    pub fn agent_state(
        &self,
        seen_ms: Option<u64>,
        viewed_job: &dyn Fn(u32, Option<&str>) -> Option<Job>,
    ) -> Option<AgentState> {
        let foreground = self.process.foreground.as_ref();
        let live_pids = self.pids();
        let live: Vec<(&Record, u32)> = self
            .status
            .records
            .iter()
            .filter_map(|r| {
                let pid = r.owner.as_ref()?.pid?;
                live_pids.contains(&pid).then_some((r, pid))
            })
            .collect();
        let agent = live
            .iter()
            .find_map(|(r, _)| {
                r.effective_app
                    .as_deref()
                    .or(r.app.as_deref())
                    .and_then(agent_name)
            })
            .or_else(|| {
                let p = foreground?;
                [p.argv0.as_deref(), Some(p.name.as_str()), p.path.as_deref()]
                    .into_iter()
                    .flatten()
                    .find_map(agent_name)
            })?;
        let mut best: Option<(AgentStatus, LiveRecord, &Record)> = None;
        for (record, owner_pid) in live {
            let (status, clears_when_seen) = match record.state.as_str() {
                "blocked" => (AgentStatus::Blocked, false),
                "working" => (AgentStatus::Working, false),
                "done" => (AgentStatus::Done, true),
                "error" => (AgentStatus::Blocked, true),
                _ => (AgentStatus::Idle, false),
            };
            let updated_ms = record.updated_at.as_deref().and_then(parse_time_ms);
            let status = unless_seen(status, clears_when_seen, seen_ms, updated_ms);
            if best.is_none_or(|(b, _, _)| status.priority() > b.priority()) {
                let live = LiveRecord {
                    owner_pid,
                    clears_when_seen,
                };
                best = Some((status, live, record));
            }
        }
        let state = match best {
            Some((status, live, record)) => AgentState {
                agent,
                status,
                record: Some(live),
                pid: Some(live.owner_pid),
                message: record.msg.clone(),
                blocked_kind: record
                    .kind
                    .clone()
                    .filter(|_| status == AgentStatus::Blocked),
                updated_ms: record.updated_at.as_deref().and_then(parse_time_ms),
            },
            None => {
                // Agents that report at all report idle once ready, so silence means
                // starting up or a startup dialog, unless the process is a view of a
                // Claude background agent, which never reports here.
                let pid = foreground.and_then(|p| p.pid);
                let job = if agent == "claude" {
                    pid.and_then(|pid| {
                        let title = self
                            .title
                            .as_ref()
                            .filter(|t| t.owner.as_ref().and_then(|o| o.pid) == Some(pid))
                            .and_then(|t| t.title.as_deref());
                        viewed_job(pid, title)
                    })
                } else {
                    None
                };
                let status = job
                    .as_ref()
                    .and_then(|job| {
                        let (status, clears) = job.status()?;
                        Some(unless_seen(status, clears, seen_ms, job.updated_ms))
                    })
                    .unwrap_or(AgentStatus::Unknown);
                let job = job.as_ref();
                AgentState {
                    agent,
                    status,
                    record: None,
                    pid,
                    message: job.and_then(|job| job.message.clone()),
                    blocked_kind: job
                        .and_then(|job| job.blocked_kind)
                        .filter(|_| status == AgentStatus::Blocked)
                        .map(String::from),
                    updated_ms: job.and_then(|job| job.updated_ms),
                }
            }
        };
        Some(state)
    }
}

/// Idle once the user focused the block at or after the update, for a status
/// that `clears` when seen (done, error); otherwise `status`.
fn unless_seen(
    status: AgentStatus,
    clears: bool,
    seen_ms: Option<u64>,
    updated_ms: Option<u64>,
) -> AgentStatus {
    let seen = clears && matches!((seen_ms, updated_ms), (Some(seen), Some(at)) if seen >= at);
    if seen { AgentStatus::Idle } else { status }
}

/// Decodes the snapshot script output: `{host_id, sessions: [{view, blocks}]}`,
/// where `view` is `session.view` and `blocks` maps block ids to their
/// `process`, `title` and `program_status` results.
pub fn decode(raw: &Value, ctx: &DecodeContext) -> Result<Snapshot> {
    let raw = RawSnapshot::deserialize(raw).context("Invalid Rex snapshot")?;
    let workspaces = raw
        .sessions
        .into_iter()
        .map(|session| decode_session(session, ctx))
        .collect();
    Ok(Snapshot {
        workspaces,
        notice: None,
    })
}

fn decode_session(session: RawSession, ctx: &DecodeContext) -> Workspace {
    let view = session.view;
    let tabs = view
        .windows
        .into_iter()
        .map(|window| {
            let name = window_name(&window);
            let panes = window
                .layers
                .into_iter()
                .flat_map(|layer| layer.blocks)
                .filter(|block| Some(block.block_id.as_str()) != ctx.exclude_block)
                .map(|block| {
                    let info = session.blocks.get(&block.block_id);
                    decode_block(block, info, &view.session_id, &window.window_id, ctx)
                })
                .collect();
            Tab {
                name,
                id: window.window_id,
                workspace_id: view.session_id.clone(),
                panes,
            }
        })
        .collect();
    Workspace {
        id: view.session_id,
        name: view.label,
        focused: false,
        active_tab_id: view.active_window_id,
        worktree: None,
        tabs,
    }
}

fn decode_block(
    block: RawBlock,
    info: Option<&BlockInfo>,
    session: &str,
    window: &str,
    ctx: &DecodeContext,
) -> Pane {
    let name = pane_name(&block);
    let title = info
        .and_then(|i| i.title.as_ref())
        .and_then(|t| t.title.clone());
    let seen_ms = ctx.history.panes.get(&block.block_id).copied();
    let state = info.and_then(|i| i.agent_state(seen_ms, ctx.viewed_job));
    let working_since_ms = state.as_ref().and_then(|state| {
        let record = state.record?;
        let period = ctx.periods.get(&block.block_id)?;
        let provider = AgentProvider::from_name(state.agent)?;
        (state.status == AgentStatus::Working
            && provider.supports_timing()
            && period.provider == provider
            && period.owner_pid == record.owner_pid)
            .then_some(period.started_ms)
    });
    Pane {
        id: block.block_id,
        workspace_id: session.into(),
        tab_id: window.into(),
        focused: false,
        name,
        terminal_title_stripped: title.as_deref().map(strip_title).filter(|t| !t.is_empty()),
        terminal_title: title,
        agent: state.as_ref().map(|s| s.agent.to_owned()),
        terminal_id: None,
        working_since_ms,
        status: state.map(|s| s.status).unwrap_or_default(),
    }
}

/// The label Rex gives a block of its kind (`shell` for a terminal shell).
fn default_label(block: &RawBlock) -> &str {
    block.flavor.rsplit('.').next().unwrap_or_default()
}

/// A block's label when the user chose it.
fn pane_name(block: &RawBlock) -> Option<String> {
    let label = &block.label;
    (!label.trim().is_empty() && label != default_label(block)).then(|| label.clone())
}

/// A window's label, else its id.
fn window_name(window: &RawWindow) -> String {
    if window.label.trim().is_empty() {
        window.window_id.clone()
    } else {
        window.label.clone()
    }
}

/// A terminal block: where it is, what runs in it and the agent it hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalBlock {
    pub session_id: String,
    /// The session's label.
    pub session: String,
    pub window_id: String,
    /// The window's label, else its id.
    pub window: String,
    pub block_id: String,
    /// The label, when the user chose it.
    pub name: Option<String>,
    /// The label Rex gives a block of this kind.
    pub default_label: String,
    pub title: Option<String>,
    /// The foreground process's working directory.
    pub cwd: Option<String>,
    /// The foreground process's name and pid.
    pub foreground: Option<(String, u32)>,
    /// The foreground process's pid, then its ancestors'.
    pub pids: Vec<u32>,
    pub agent: Option<AgentState>,
}

impl TerminalBlock {
    /// Takes the title, processes and agent that `info`, a newer report on this
    /// block, gives.
    pub fn update(&mut self, info: &BlockInfo, ctx: &DecodeContext) {
        let foreground = info.process.foreground.as_ref();
        let seen_ms = ctx.history.panes.get(&self.block_id).copied();
        self.title = info.title.as_ref().and_then(|t| t.title.clone());
        self.cwd = foreground.and_then(|p| p.cwd.clone());
        self.foreground = foreground.and_then(|p| Some((p.name.clone(), p.pid?)));
        self.pids = info.pids();
        self.agent = info.agent_state(seen_ms, ctx.viewed_job);
    }
}

/// Every terminal block of the snapshot script output, in server order.
pub fn terminal_blocks(raw: &Value, ctx: &DecodeContext) -> Result<Vec<TerminalBlock>> {
    let raw = RawSnapshot::deserialize(raw).context("Invalid Rex snapshot")?;
    let mut blocks = Vec::new();
    for session in raw.sessions {
        let view = session.view;
        for window in &view.windows {
            for block in window.layers.iter().flat_map(|layer| &layer.blocks) {
                let Some(info) = session.blocks.get(&block.block_id) else {
                    continue;
                };
                if Some(block.block_id.as_str()) == ctx.exclude_block {
                    continue;
                }
                let mut terminal = TerminalBlock {
                    session_id: view.session_id.clone(),
                    session: view.label.clone(),
                    window_id: window.window_id.clone(),
                    window: window_name(window),
                    block_id: block.block_id.clone(),
                    name: pane_name(block),
                    default_label: default_label(block).to_owned(),
                    title: None,
                    cwd: None,
                    foreground: None,
                    pids: Vec::new(),
                    agent: None,
                };
                terminal.update(info, ctx);
                blocks.push(terminal);
            }
        }
    }
    Ok(blocks)
}

/// Herdr's title rule: drop one leading spinner (braille) or Claude activity glyph
/// when whitespace or the end follows it.
pub fn strip_title(title: &str) -> String {
    const CLAUDE_ACTIVITY_GLYPHS: &str = "·✢✳✶✻✽◐◓◑◒";
    let title = title.trim();
    let mut chars = title.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest = chars.as_str();
    let recognized =
        matches!(first, '\u{2800}'..='\u{28ff}') || CLAUDE_ACTIVITY_GLYPHS.contains(first);
    if recognized && rest.chars().next().is_none_or(char::is_whitespace) {
        rest.trim().to_owned()
    } else {
        title.to_owned()
    }
}

/// Unix milliseconds of an RFC 3339 timestamp, as Rex writes `updated_at`.
pub fn parse_time_ms(text: &str) -> Option<u64> {
    let digits = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    let (date, time) = text.split_once(['T', 't'])?;
    let mut date_parts = date.splitn(3, '-');
    let (year, month, day) = (
        digits(date_parts.next()?)?,
        digits(date_parts.next()?)?,
        digits(date_parts.next()?)?,
    );
    let (clock, offset_s) = if let Some(clock) = time.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let at = time.rfind(['+', '-'])?;
        let (clock, offset) = time.split_at(at);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (h, m) = offset[1..].split_once(':')?;
        (clock, sign * (digits(h)? * 3600 + digits(m)? * 60))
    };
    let (hms, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut hms = hms.splitn(3, ':');
    let (hour, minute, second) = (
        digits(hms.next()?)?,
        digits(hms.next()?)?,
        digits(hms.next()?)?,
    );
    let millis = if fraction.is_empty() {
        0
    } else {
        digits(fraction)?;
        format!("{fraction:0<3}")[..3].parse().ok()?
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's civil-from-days inverse).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_s;
    u64::try_from(seconds * 1000 + millis).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::rex::claude_jobs::{Job, REPLY};
    use crate::models::{AgentProvider, AgentStatus, Pane};
    use serde_json::json;
    use std::cell::RefCell;

    const TERMINAL: &str = "com.superlogical.terminal";
    /// Rex's `updated_at` for the records below, and the same instant in ms.
    const AT: &str = "2026-11-12T14:25:55.256139-08:00";
    const AT_MS: u64 = 1_794_522_355_256;

    fn view_block(id: &str, label: &str) -> Value {
        json!({"block_id": id, "creator_name": TERMINAL, "flavor": "com.superlogical.terminal.shell",
               "host_id": "host", "label": label, "owner": "client:app"})
    }

    /// A block's process, title and status as the snapshot script reports them.
    fn info(foreground: (&str, u32), ancestors: &[(&str, u32)], records: Value) -> Value {
        let ancestors: Vec<Value> = ancestors
            .iter()
            .map(|(name, pid)| json!({"name": name, "pid": pid, "path": format!("/bin/{name}")}))
            .collect();
        json!({
            "process": {"foreground": {"name": foreground.0, "argv0": foreground.0, "pid": foreground.1,
                                        "path": format!("/usr/local/bin/{}", foreground.0)},
                        "ancestors": ancestors, "child": {"name": "login", "pid": 1}},
            "title": {"title": format!("{} title", foreground.0), "owner": {"name": foreground.0, "pid": foreground.1}},
            "status": {"records": records}
        })
    }

    fn record(state: &str, app: &str, owner: u32) -> Value {
        json!({"state": state, "app": app, "effective_app": app,
               "owner": {"name": "agent", "pid": owner}, "updated_at": AT})
    }

    fn shell() -> Value {
        info(("zsh", 10), &[("login", 1)], json!({}))
    }

    /// Session `app` (two windows, the second with a floating note and the popup)
    /// and session `docs`.
    fn raw() -> Value {
        json!({"host_id": "host", "sessions": [
            {"view": {"session_id": "session:app", "label": "app", "active_window_id": "window:a2", "revision": 3,
                      "windows": [
                {"window_id": "window:a1", "label": "Window 5976c218", "focused_block_id": "block:a1",
                 "layers": [{"kind": "tiled", "layer_id": "layer:a1", "bounds": [0, 0, 1, 1],
                             "blocks": [view_block("block:a1", "shell"), view_block("block:a2", "server")]}]},
                {"window_id": "window:a2", "label": "editor", "focused_block_id": "block:a3",
                 "layers": [
                    {"kind": "tiled", "layer_id": "layer:a2", "blocks": [view_block("block:a3", "shell")]},
                    {"kind": "floating", "layer_id": "layer:a3", "blocks": [view_block("block:note", "notes")]},
                    {"kind": "floating", "layer_id": "layer:a4", "blocks": [view_block("block:popup", "Canopy")]}
                 ]}
             ]},
             "blocks": {
                "block:a1": shell(),
                "block:a2": info(("node", 20), &[("zsh", 11), ("login", 1)], json!({})),
                "block:a3": info(("claude", 30), &[("zsh", 12)], json!([record("working", "claude-code", 30)])),
                "block:note": shell(),
                "block:popup": info(("herdr-canopy", 40), &[], json!({}))
             }},
            {"view": {"session_id": "session:docs", "label": "docs", "active_window_id": "window:d1",
                      "windows": [{"window_id": "window:d1", "label": "main", "focused_block_id": "block:d1",
                                   "layers": [{"kind": "tiled", "blocks": [view_block("block:d1", "shell")]}]}]},
             "blocks": {"block:d1": shell()}}
        ]})
    }

    /// No Claude pane shows a background agent unless a test says otherwise.
    fn no_job(_pid: u32, _title: Option<&str>) -> Option<Job> {
        None
    }

    fn decode_with(raw: &Value, history: &History, periods: &HashMap<String, Period>) -> Snapshot {
        decode(
            raw,
            &DecodeContext {
                history,
                periods,
                exclude_block: Some("block:popup"),
                viewed_job: &no_job,
            },
        )
        .unwrap()
    }

    fn decode_viewing(
        raw: &Value,
        history: &History,
        viewed_job: &dyn Fn(u32, Option<&str>) -> Option<Job>,
    ) -> Snapshot {
        decode(
            raw,
            &DecodeContext {
                history,
                periods: &HashMap::new(),
                exclude_block: Some("block:popup"),
                viewed_job,
            },
        )
        .unwrap()
    }

    fn decoded(raw: &Value) -> Snapshot {
        decode_with(raw, &History::default(), &HashMap::new())
    }

    /// The raw fixture with one block's info replaced.
    fn with_block(id: &str, block: Value) -> Value {
        let mut data = raw();
        data["sessions"][0]["blocks"][id] = block;
        data
    }

    fn pane<'a>(snapshot: &'a Snapshot, id: &str) -> &'a Pane {
        snapshot
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("pane {id} is missing"))
    }

    fn agent_of(block: Value) -> (Option<String>, AgentStatus) {
        let data = decoded(&with_block("block:a2", block));
        let pane = pane(&data, "block:a2");
        (pane.agent.clone(), pane.status)
    }

    #[test]
    fn sessions_windows_and_blocks_become_workspaces_tabs_and_panes_in_server_order() {
        let data = decoded(&raw());
        let ids = |w: usize| -> Vec<Vec<&str>> {
            data.workspaces[w]
                .tabs
                .iter()
                .map(|t| t.panes.iter().map(|p| p.id.as_str()).collect())
                .collect()
        };
        assert_eq!(data.workspaces.len(), 2);
        let app = &data.workspaces[0];
        assert_eq!((app.id.as_str(), app.name.as_str()), ("session:app", "app"));
        assert_eq!(app.active_tab_id.as_deref(), Some("window:a2"));
        assert!(!app.focused && app.worktree.is_none());
        let tabs: Vec<_> = app
            .tabs
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str()))
            .collect();
        assert_eq!(
            tabs,
            [("window:a1", "Window 5976c218"), ("window:a2", "editor")]
        );
        assert_eq!(
            ids(0),
            [vec!["block:a1", "block:a2"], vec!["block:a3", "block:note"]]
        );
        assert_eq!(ids(1), [vec!["block:d1"]]);
        let note = pane(&data, "block:note");
        assert_eq!(
            (note.workspace_id.as_str(), note.tab_id.as_str()),
            ("session:app", "window:a2")
        );
        assert!(!note.focused && note.terminal_id.is_none());
        assert_eq!(data.notice, None);
    }

    #[test]
    fn only_labels_the_user_chose_become_pane_names() {
        let mut data = raw();
        data["sessions"][0]["view"]["windows"][0]["layers"][0]["blocks"][1]["flavor"] =
            json!("com.superlogical.terminal.server");
        data["sessions"][1]["view"]["windows"][0]["layers"][0]["blocks"][0]["label"] = json!("");
        let data = decoded(&data);
        assert_eq!(pane(&data, "block:a1").name, None, "default label");
        assert_eq!(
            pane(&data, "block:a2").name,
            None,
            "label equal to its own flavor"
        );
        assert_eq!(pane(&data, "block:note").name.as_deref(), Some("notes"));
        assert_eq!(pane(&data, "block:d1").name, None, "empty label");
        let named = decoded(&raw());
        assert_eq!(pane(&named, "block:a2").name.as_deref(), Some("server"));
    }

    #[test]
    fn titles_are_kept_raw_and_stripped_of_one_leading_activity_glyph() {
        for (title, stripped) in [
            ("✳ Fix the parser", "Fix the parser"),
            ("⠙ Thinking", "Thinking"),
            ("· Compacting", "Compacting"),
            ("✳", ""),
            ("⠋ ⠙ task", "⠙ task"),
            ("✳✳ twice", "✳✳ twice"),
            ("✳glued", "✳glued"),
            ("plain title", "plain title"),
        ] {
            assert_eq!(strip_title(title), stripped, "{title:?}");
        }
        let mut block = shell();
        block["title"]["title"] = json!("✳ Review");
        let data = decoded(&with_block("block:a1", block));
        let shown = pane(&data, "block:a1");
        assert_eq!(shown.terminal_title.as_deref(), Some("✳ Review"));
        assert_eq!(shown.terminal_title_stripped.as_deref(), Some("Review"));
    }

    #[test]
    fn the_navigator_block_is_hidden_but_other_floating_blocks_are_listed() {
        let data = decoded(&raw());
        let all: Vec<_> = data
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|p| p.id.as_str())
            .collect();
        assert!(!all.contains(&"block:popup"));
        assert!(all.contains(&"block:note"));
    }

    #[test]
    fn agents_are_known_names_from_live_status_apps_or_the_foreground_process() {
        let cases = [
            (
                record("working", "claude-code", 20),
                ("node", 20),
                Some("claude"),
            ),
            (record("working", "codex", 20), ("node", 20), Some("codex")),
            (record("working", "wts", 20), ("wts", 20), None),
        ];
        for (rec, foreground, expected) in cases {
            let (agent, _) = agent_of(info(foreground, &[("zsh", 11)], json!([rec])));
            assert_eq!(agent.as_deref(), expected);
        }
        for (process, expected) in [
            ("codex", Some("codex")),
            ("/Users/me/.local/bin/claude", Some("claude")),
            ("Claude", Some("claude")),
            ("codex.exe", Some("codex")),
            ("cursor-agent", Some("cursor")),
            ("gemini", Some("gemini")),
            ("zsh", None),
            ("vim", None),
        ] {
            let (agent, _) = agent_of(info((process, 20), &[("zsh", 11)], json!({})));
            assert_eq!(agent.as_deref(), expected, "{process}");
        }
        let data = decoded(&raw());
        let claude = pane(&data, "block:a3");
        assert_eq!(
            claude.agent.as_deref().and_then(AgentProvider::from_name),
            Some(AgentProvider::Claude)
        );
    }

    #[test]
    fn status_left_behind_by_an_exited_program_is_ignored() {
        // The owner (pid 30) is gone: the shell is back in the foreground.
        let stale = info(
            ("zsh", 11),
            &[("login", 1)],
            json!([record("done", "claude-code", 30)]),
        );
        assert_eq!(agent_of(stale), (None, AgentStatus::Unknown));
        // The owner is an ancestor of the foreground process (the agent runs a tool).
        let tool = info(
            ("cargo", 31),
            &[("claude", 30), ("zsh", 11)],
            json!([record("working", "claude-code", 30)]),
        );
        assert_eq!(
            agent_of(tool),
            (Some("claude".into()), AgentStatus::Working)
        );
    }

    #[test]
    fn rex_states_map_to_canopy_states_and_the_most_urgent_live_record_wins() {
        for (state, expected) in [
            ("blocked", AgentStatus::Blocked),
            ("working", AgentStatus::Working),
            ("done", AgentStatus::Done),
            ("error", AgentStatus::Blocked),
            ("idle", AgentStatus::Idle),
            ("something-new", AgentStatus::Idle),
        ] {
            let block = info(("codex", 20), &[], json!([record(state, "codex", 20)]));
            assert_eq!(agent_of(block).1, expected, "{state}");
        }
        for (states, expected) in [
            (["idle", "working"], AgentStatus::Working),
            (["working", "blocked"], AgentStatus::Blocked),
            (["done", "idle"], AgentStatus::Done),
        ] {
            let records: Vec<Value> = states
                .iter()
                .enumerate()
                .map(|(n, state)| {
                    let mut rec = record(state, "codex", 20);
                    rec["id"] = json!(format!("child-{n}"));
                    rec
                })
                .collect();
            let block = info(("codex", 20), &[], json!(records));
            assert_eq!(agent_of(block).1, expected, "{states:?}");
        }
    }

    #[test]
    fn agents_that_have_not_reported_are_unknown() {
        // Claude and Codex report idle once ready, so silence means they are
        // starting or waiting at a startup dialog.
        for process in ["claude", "codex", "gemini"] {
            let block = info((process, 20), &[("zsh", 11)], json!({}));
            assert_eq!(agent_of(block).1, AgentStatus::Unknown, "{process}");
        }
    }

    fn job(state: &str, updated_ms: Option<u64>, message: Option<&str>) -> Job {
        Job {
            state: state.into(),
            tempo: None,
            updated_ms,
            message: message.map(String::from),
            blocked_kind: None,
        }
    }

    #[test]
    fn only_a_title_the_claude_process_set_names_the_job_it_shows() {
        // Before Claude sets its title, the block keeps the one the shell set.
        let mut by_shell = info(("claude.exe", 20), &[("zsh", 11)], json!({}));
        by_shell["title"] = json!({"title": "Pilot plan", "owner": {"name": "zsh", "pid": 11}});
        let mut unowned = by_shell.clone();
        unowned["title"] = json!({"title": "Pilot plan"});
        for block in [by_shell, unowned] {
            let asked = RefCell::new(None);
            decode_viewing(
                &with_block("block:a2", block),
                &History::default(),
                &|pid, title| {
                    *asked.borrow_mut() = Some((pid, title.map(String::from)));
                    None
                },
            );
            assert_eq!(asked.take(), Some((20, None)));
        }
    }

    #[test]
    fn a_background_agent_waiting_on_its_reply_says_so() {
        let state_of = |job: Job| {
            block_info(info(("claude", 20), &[("zsh", 11)], json!({})))
                .agent_state(None, &|_, _| Some(job.clone()))
                .unwrap()
        };
        let mut question = job("blocked", Some(AT_MS), Some("Go ahead?"));
        question.blocked_kind = Some(REPLY);
        let waiting = state_of(question.clone());
        assert_eq!(
            (
                waiting.status,
                waiting.blocked_kind.as_deref(),
                waiting.updated_ms
            ),
            (AgentStatus::Blocked, Some(REPLY), Some(AT_MS))
        );
        question.tempo = Some("active".into());
        let answered = state_of(question);
        assert_eq!(
            (answered.status, answered.blocked_kind),
            (AgentStatus::Working, None),
            "only a blocked agent has a blocked kind"
        );
    }

    #[test]
    fn a_silent_claude_viewing_a_background_agent_shows_that_agents_job_state() {
        let silent = with_block(
            "block:a2",
            info(("claude.exe", 20), &[("zsh", 11)], json!({})),
        );
        for (shown, expected) in [
            (
                Some(job("working", Some(AT_MS), None)),
                AgentStatus::Working,
            ),
            (
                Some(job("blocked", Some(AT_MS), None)),
                AgentStatus::Blocked,
            ),
            (Some(job("done", Some(AT_MS), None)), AgentStatus::Done),
            (Some(job("failed", Some(AT_MS), None)), AgentStatus::Blocked),
            (Some(job("stopped", Some(AT_MS), None)), AgentStatus::Idle),
            (
                Some(job("something-new", Some(AT_MS), None)),
                AgentStatus::Unknown,
            ),
            (None, AgentStatus::Unknown),
        ] {
            let asked = RefCell::new(None);
            let data = decode_viewing(&silent, &History::default(), &|pid, title| {
                *asked.borrow_mut() = Some((pid, title.map(String::from)));
                shown.clone()
            });
            let shown_pane = pane(&data, "block:a2");
            assert_eq!(
                (shown_pane.agent.as_deref(), shown_pane.status),
                (Some("claude"), expected),
                "{shown:?}"
            );
            assert_eq!(
                asked.take(),
                Some((20, Some("claude.exe title".to_string()))),
                "asks about the foreground process and the block's title"
            );
        }
    }

    #[test]
    fn only_claude_processes_without_live_records_are_looked_up() {
        let never =
            |pid: u32, _title: Option<&str>| -> Option<Job> { panic!("looked up pid {pid}") };
        // block:a3 is Claude with a live record; block:a2 a silent Codex.
        let data = with_block("block:a2", info(("codex", 20), &[], json!({})));
        let snapshot = decode_viewing(&data, &History::default(), &never);
        assert_eq!(pane(&snapshot, "block:a3").status, AgentStatus::Working);
        assert_eq!(pane(&snapshot, "block:a2").status, AgentStatus::Unknown);
    }

    #[test]
    fn a_finished_background_agent_turns_idle_once_focused_after_its_last_update() {
        let silent = with_block("block:a2", info(("claude", 20), &[], json!({})));
        let focused_at = |ms: u64| History {
            panes: HashMap::from([("block:a2".to_string(), ms)]),
            ..History::default()
        };
        let status = |state: &str, updated_ms: Option<u64>, history: &History| {
            let shown = job(state, updated_ms, None);
            pane(
                &decode_viewing(&silent, history, &|_, _| Some(shown.clone())),
                "block:a2",
            )
            .status
        };
        let done = Some(AT_MS);
        assert_eq!(status("done", done, &History::default()), AgentStatus::Done);
        assert_eq!(
            status("done", done, &focused_at(AT_MS - 1)),
            AgentStatus::Done
        );
        assert_eq!(status("done", done, &focused_at(AT_MS)), AgentStatus::Idle);
        assert_eq!(
            status("failed", done, &focused_at(AT_MS + 5)),
            AgentStatus::Idle
        );
        assert_eq!(
            status("done", None, &focused_at(AT_MS + 5)),
            AgentStatus::Done,
            "an undated job is kept"
        );
        assert_eq!(
            status("blocked", done, &focused_at(AT_MS + 5)),
            AgentStatus::Blocked,
            "a question stays until answered"
        );
    }

    fn block_info(block: Value) -> BlockInfo {
        serde_json::from_value(block).unwrap()
    }

    #[test]
    fn an_agents_state_names_its_process_and_what_it_reports() {
        let state_of = |block: Value, job: Option<Job>| {
            block_info(block)
                .agent_state(None, &|_, _| job.clone())
                .unwrap()
        };
        let reported = |state: &str| {
            let mut rec = record(state, "claude-code", 30);
            rec["msg"] = json!("Allow cargo test?");
            rec["kind"] = json!("permission");
            // The agent runs a tool: its process is an ancestor of the foreground one.
            info(("cargo", 31), &[("claude", 30), ("zsh", 11)], json!([rec]))
        };
        let blocked = state_of(reported("blocked"), None);
        assert_eq!(
            (
                blocked.pid,
                blocked.message.as_deref(),
                blocked.blocked_kind.as_deref(),
                blocked.updated_ms
            ),
            (
                Some(30),
                Some("Allow cargo test?"),
                Some("permission"),
                Some(AT_MS)
            )
        );
        let working = state_of(reported("working"), None);
        assert_eq!(
            (working.status, working.pid, working.blocked_kind),
            (AgentStatus::Working, Some(30), None),
            "only a blocked agent has a blocked kind"
        );
        let silent = || info(("claude", 20), &[("zsh", 11)], json!({}));
        let viewer = state_of(silent(), Some(job("blocked", None, Some("Go ahead?"))));
        assert_eq!(
            (viewer.status, viewer.pid, viewer.message.as_deref()),
            (AgentStatus::Blocked, Some(20), Some("Go ahead?")),
            "a view of a background agent: its own process, the job's message"
        );
        let unknown = state_of(silent(), None);
        assert_eq!(
            (unknown.status, unknown.pid, unknown.message),
            (AgentStatus::Unknown, Some(20), None)
        );
    }

    #[test]
    fn terminal_blocks_carry_their_place_process_and_agent() {
        let mut data = raw();
        data["sessions"][0]["blocks"]["block:a3"]["process"]["foreground"]["cwd"] = json!("/repo");
        data["sessions"][0]["view"]["windows"][1]["label"] = json!("");
        let blocks = terminal_blocks(
            &data,
            &DecodeContext {
                history: &History::default(),
                periods: &HashMap::new(),
                exclude_block: Some("block:popup"),
                viewed_job: &no_job,
            },
        )
        .unwrap();
        let ids: Vec<&str> = blocks.iter().map(|b| b.block_id.as_str()).collect();
        assert_eq!(
            ids,
            ["block:a1", "block:a2", "block:a3", "block:note", "block:d1"]
        );
        let find = |id: &str| blocks.iter().find(|b| b.block_id == id).unwrap();
        let claude = find("block:a3");
        assert_eq!(
            (
                claude.session_id.as_str(),
                claude.session.as_str(),
                claude.window_id.as_str(),
                claude.window.as_str(),
            ),
            ("session:app", "app", "window:a2", "window:a2"),
            "an unlabeled window is named by its id"
        );
        assert_eq!(
            (claude.name.as_deref(), claude.default_label.as_str()),
            (None, "shell")
        );
        assert_eq!(claude.cwd.as_deref(), Some("/repo"));
        assert_eq!(claude.title.as_deref(), Some("claude title"));
        assert_eq!(claude.foreground, Some(("claude".to_string(), 30)));
        assert_eq!(
            claude.pids,
            [30, 12],
            "the foreground process, then its ancestors"
        );
        let agent = claude.agent.as_ref().unwrap();
        assert_eq!(
            (agent.agent, agent.status, agent.pid),
            ("claude", AgentStatus::Working, Some(30))
        );
        let server = find("block:a2");
        assert_eq!(server.name.as_deref(), Some("server"));
        assert_eq!(server.agent, None);
        assert_eq!(server.foreground, Some(("node".to_string(), 20)));
        assert_eq!(server.pids, [20, 11, 1]);
    }

    #[test]
    fn a_block_takes_what_its_latest_report_says_and_keeps_its_place() {
        let ctx = DecodeContext {
            history: &History::default(),
            periods: &HashMap::new(),
            exclude_block: None,
            viewed_job: &no_job,
        };
        let blocks = terminal_blocks(&raw(), &ctx).unwrap();
        let before = blocks.iter().find(|b| b.block_id == "block:a3").unwrap();
        let mut after = before.clone();
        let mut report = info(
            ("codex", 31),
            &[("zsh", 12)],
            json!([record("blocked", "codex", 31)]),
        );
        report["process"]["foreground"]["cwd"] = json!("/elsewhere");
        after.update(&block_info(report), &ctx);
        assert_eq!(
            (
                &after.session_id,
                &after.window,
                &after.block_id,
                &after.name
            ),
            (
                &before.session_id,
                &before.window,
                &before.block_id,
                &before.name
            )
        );
        assert_eq!(after.cwd.as_deref(), Some("/elsewhere"));
        assert_eq!(after.title.as_deref(), Some("codex title"));
        assert_eq!(after.foreground, Some(("codex".to_string(), 31)));
        assert_eq!(after.pids, [31, 12]);
        let agent = after.agent.unwrap();
        assert_eq!(
            (agent.agent, agent.status, agent.pid),
            ("codex", AgentStatus::Blocked, Some(31))
        );
        let mut closed = before.clone();
        closed.update(&block_info(json!({})), &ctx);
        assert_eq!(
            (closed.foreground, closed.pids, closed.agent),
            (None, vec![], None),
            "a report without processes leaves no process or agent"
        );
    }

    #[test]
    fn finished_or_failed_agents_turn_idle_once_focused_after_finishing() {
        let focused_at = |ms: u64| History {
            panes: HashMap::from([("block:a2".to_string(), ms)]),
            ..History::default()
        };
        for (state, unseen) in [("done", AgentStatus::Done), ("error", AgentStatus::Blocked)] {
            let data = with_block(
                "block:a2",
                info(("codex", 20), &[], json!([record(state, "codex", 20)])),
            );
            let status = |history: &History| {
                pane(&decode_with(&data, history, &HashMap::new()), "block:a2").status
            };
            assert_eq!(
                status(&History::default()),
                unseen,
                "{state}: never focused"
            );
            assert_eq!(
                status(&focused_at(AT_MS - 1)),
                unseen,
                "{state}: focused before"
            );
            assert_eq!(
                status(&focused_at(AT_MS)),
                AgentStatus::Idle,
                "{state}: focused at"
            );
            assert_eq!(
                status(&focused_at(AT_MS + 5)),
                AgentStatus::Idle,
                "{state}: focused after"
            );
        }
        let waiting = with_block(
            "block:a2",
            info(("codex", 20), &[], json!([record("blocked", "codex", 20)])),
        );
        let status = pane(
            &decode_with(&waiting, &focused_at(AT_MS + 5), &HashMap::new()),
            "block:a2",
        )
        .status;
        assert_eq!(
            status,
            AgentStatus::Blocked,
            "a question stays until answered"
        );
    }

    #[test]
    fn working_timers_come_only_from_a_matching_watcher_period() {
        let period = |provider, owner_pid| Period {
            provider,
            owner_pid,
            started_ms: 1_000,
        };
        let since = |block: Value, period: Period| {
            let data = with_block("block:a2", block);
            let periods = HashMap::from([("block:a2".to_string(), period)]);
            pane(
                &decode_with(&data, &History::default(), &periods),
                "block:a2",
            )
            .working_since_ms
        };
        let working = || {
            info(
                ("claude", 20),
                &[],
                json!([record("working", "claude-code", 20)]),
            )
        };
        assert_eq!(
            since(working(), period(AgentProvider::Claude, 20)),
            Some(1_000)
        );
        assert_eq!(
            since(working(), period(AgentProvider::Claude, 99)),
            None,
            "another process"
        );
        assert_eq!(
            since(working(), period(AgentProvider::Codex, 20)),
            None,
            "another provider"
        );
        let idle = info(
            ("claude", 20),
            &[],
            json!([record("idle", "claude-code", 20)]),
        );
        assert_eq!(
            since(idle, period(AgentProvider::Claude, 20)),
            None,
            "not working"
        );
        let gemini = info(
            ("gemini", 20),
            &[],
            json!([record("working", "gemini", 20)]),
        );
        assert_eq!(
            since(gemini, period(AgentProvider::Other, 20)),
            None,
            "untimed agent"
        );
        assert_eq!(
            pane(&decoded(&raw()), "block:a3").working_since_ms,
            None,
            "no period"
        );
    }

    #[test]
    fn lua_empty_tables_and_missing_fields_decode_as_empty() {
        let data = json!({"host_id": "host", "sessions": [
            {"view": {"session_id": "session:e", "label": "empty", "windows": {}}, "blocks": {}},
            {"view": {"session_id": "session:f", "label": "f", "active_window_id": "window:f1",
                      "windows": [{"window_id": "window:f1", "label": "w", "layers": [
                          {"kind": "tiled", "blocks": {}},
                          {"kind": "tiled", "blocks": [view_block("block:new", "shell"), view_block("block:bare", "shell")]}]}]},
             "blocks": {"block:bare": {"process": {"foreground": {"name": "zsh", "pid": 5}, "ancestors": {}},
                                       "status": {"records": {}}}}}
        ]});
        let data = decode(
            &data,
            &DecodeContext {
                history: &History::default(),
                periods: &HashMap::new(),
                exclude_block: None,
                viewed_job: &no_job,
            },
        )
        .unwrap();
        assert!(data.workspaces[0].tabs.is_empty());
        assert_eq!(data.workspaces[0].active_tab_id, None);
        for id in ["block:new", "block:bare"] {
            let shown = pane(&data, id);
            assert_eq!(
                (shown.agent.as_deref(), shown.status),
                (None, AgentStatus::Unknown),
                "{id}"
            );
            assert_eq!(shown.terminal_title, None, "{id}");
        }
    }

    #[test]
    fn malformed_snapshots_are_errors() {
        let ctx = DecodeContext {
            history: &History::default(),
            periods: &HashMap::new(),
            exclude_block: None,
            viewed_job: &no_job,
        };
        for data in [
            json!({}),
            json!({"sessions": "none"}),
            json!({"sessions": [{"view": {"label": "no id"}, "blocks": {}}]}),
        ] {
            assert!(decode(&data, &ctx).is_err(), "{data}");
        }
    }

    #[test]
    fn rfc3339_times_convert_to_unix_milliseconds() {
        for (text, ms) in [
            (AT, Some(AT_MS)),
            ("2026-11-12T22:25:55Z", Some(1_794_522_355_000)),
            ("2026-11-12T22:25:55.5+00:00", Some(1_794_522_355_500)),
            ("2024-02-29T23:59:59.999+05:30", Some(1_709_231_399_999)),
            ("1970-01-01T00:00:00Z", Some(0)),
            ("yesterday", None),
            ("2026-13-12T22:25:55Z", None),
            ("", None),
        ] {
            assert_eq!(parse_time_ms(text), ms, "{text}");
        }
    }
}
