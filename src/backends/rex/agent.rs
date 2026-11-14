//! `herdr-canopy rex agent …`: Herdr's agent commands for the agents on a Rex
//! server, with Herdr's replies: `{"id": "cli:agent:<command>", "result": …}` on
//! stdout, or `{"error": {"code", "message"}, "id": …}` on stderr and exit status 1.
//!
//! An agent is a terminal block whose processes or status reports name a known
//! agent (see [`decode`]); its name is the block's label. Prompts, keys and starts
//! hold a per-block lock in the server's state directory while they write, so two
//! senders never mix their input in one prompt box. Waits from a block record
//! themselves there too (see [`waits`]), so a wait that would never end is refused.

use super::bindings::{self, Bindings};
use super::claude_jobs::{self, Job};
use super::cli::Rex;
use super::decode::{self, AgentState, BlockInfo, DecodeContext, TerminalBlock};
use super::waits;
use super::watch::Period;
use super::{FOCUS, Focused, SNAPSHOT, choose_client, state_dir, tracker};
use crate::cli::{
    AgentArgs, AgentCommand, PromptArgs, ReadArgs, ReadSource, RenameArgs, StartArgs, WaitArgs,
};
use crate::history::lock_state;
use crate::models::{ActiveLocation, AgentStatus, History};
use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const AGENT: &str = include_str!("scripts/agent.lua");

/// The longest one look at an agent waits for its status to change.
const POLL: Duration = Duration::from_secs(1);
/// How soon a prompt must set a resting agent working or blocked (Herdr's rule).
const STALL: Duration = Duration::from_secs(5);
/// What a wait ends on unless told otherwise: the agent wants input.
const SETTLED: [AgentStatus; 3] = [AgentStatus::Idle, AgentStatus::Done, AgentStatus::Blocked];
/// Lines a read of recent output keeps unless told otherwise.
const RECENT_LINES: usize = 200;
/// Agents that report their state to Rex once ready, so silence means not ready.
/// `start` runs only these: it could not tell when another is ready.
const REPORTING: [&str; 2] = ["claude", "codex"];
/// Shells `start` can type a command into: it quotes for these.
const SHELLS: [&str; 7] = ["sh", "bash", "zsh", "dash", "ksh", "mksh", "fish"];
const START_TIMEOUT_MS: u64 = 30_000;
const START_TIMEOUTS_MS: std::ops::RangeInclusive<u64> = 3_001..=300_000;
/// Lines of text from the block's screen that a failed `start` shows.
const START_SCREEN_LINES: usize = 8;
const NAME_RULE: &str = "agent name must start with a lowercase letter and contain only \
                         lowercase letters, digits, '-' or '_' (1-32 characters)";

/// Agent commands time no working periods.
static NO_PERIODS: LazyLock<HashMap<String, Period>> = LazyLock::new(HashMap::new);

/// A failed command: Herdr's error code and a message.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Anything else that fails, mostly a call to Rex.
impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::new("rex_error", format!("{error:#}"))
    }
}

type Reply = Result<Value, Failure>;

/// How the commands reach a Rex server, and Canopy's files about it.
#[derive(Clone)]
struct Server {
    rex: Rex,
    /// Canopy's state root; each server has a directory under it.
    state_root: PathBuf,
    /// worktree-sync's bindings file.
    bindings: Option<PathBuf>,
    /// The background agent a silent Claude process shows.
    viewed_job: fn(u32, Option<&str>) -> Option<Job>,
    /// The block the command runs in (`REX_BLOCK`), if any.
    caller: Option<String>,
}

/// Runs an agent command and prints its reply.
pub fn run(args: AgentArgs, state_root: anyhow::Result<PathBuf>) -> ExitCode {
    let command = name(&args.command);
    let started = Instant::now();
    let reply = state_root.map_err(Failure::from).and_then(|state_root| {
        let env = std::env::var_os;
        let server = Server {
            rex: super::resolve_rex(args.rex),
            state_root,
            bindings: bindings::path(env("XDG_STATE_HOME").as_deref(), env("HOME").as_deref()),
            viewed_job: claude_jobs::viewed_job,
            caller: std::env::var("REX_BLOCK").ok().filter(|b| !b.is_empty()),
        };
        execute(&server, args.command, &|| started.elapsed())
    });
    match envelope(command, reply) {
        Ok(reply) => {
            println!("{reply}");
            ExitCode::SUCCESS
        }
        Err(reply) => {
            eprintln!("{reply}");
            ExitCode::FAILURE
        }
    }
}

/// Herdr's reply for `command`: the result, or the error.
fn envelope(command: &str, reply: Reply) -> Result<Value, Value> {
    let id = format!("cli:agent:{command}");
    match reply {
        Ok(result) => Ok(json!({"id": id, "result": result})),
        Err(Failure { code, message }) => {
            Err(json!({"error": {"code": code, "message": message}, "id": id}))
        }
    }
}

fn name(command: &AgentCommand) -> &'static str {
    match command {
        AgentCommand::List => "list",
        AgentCommand::Get { .. } => "get",
        AgentCommand::Read(_) => "read",
        AgentCommand::SendKeys { .. } => "send-keys",
        AgentCommand::Prompt(_) => "prompt",
        AgentCommand::Rename(_) => "rename",
        AgentCommand::Focus { .. } => "focus",
        AgentCommand::Wait(_) => "wait",
        AgentCommand::Start(_) => "start",
    }
}

/// Runs `command`; `elapsed` is the time since it was given.
fn execute(server: &Server, command: AgentCommand, elapsed: &dyn Fn() -> Duration) -> Reply {
    match command {
        AgentCommand::List => {
            let view = server.load()?;
            let agents: Vec<Value> = view.agents().map(|(block, _)| view.info(block)).collect();
            Ok(json!({"type": "agent_list", "agents": agents}))
        }
        AgentCommand::Get { target } => {
            let view = server.load()?;
            let (block, _) = view.resolve(&target)?;
            Ok(json!({"type": "agent_info", "agent": view.info(block)}))
        }
        AgentCommand::Read(args) => read(server, args),
        AgentCommand::SendKeys { target, keys } => send_keys(server, &target, &keys),
        AgentCommand::Prompt(args) => prompt(server, args, elapsed),
        AgentCommand::Rename(args) => rename(server, args),
        AgentCommand::Focus { target } => focus(server, &target),
        AgentCommand::Wait(args) => wait(server, args, elapsed),
        AgentCommand::Start(args) => start(server, args, elapsed),
    }
}

/// The server, Canopy's history and worktree-sync's bindings at one moment.
struct View {
    /// The snapshot script's answer.
    raw: Value,
    state_dir: PathBuf,
    history: History,
    bindings: Bindings,
    blocks: Vec<TerminalBlock>,
}

impl Server {
    fn load(&self) -> Result<View, Failure> {
        let raw = self.rex.script(SNAPSHOT, &json!({}))?;
        let host_id = raw["host_id"]
            .as_str()
            .context("The Rex server did not report its host id")?;
        let state_dir = state_dir(&self.state_root, host_id)?;
        let history = tracker(&state_dir).load()?;
        let bindings = self
            .bindings
            .as_deref()
            .map(|path| Bindings::read(path, host_id))
            .unwrap_or_default();
        let blocks = decode::terminal_blocks(&raw, &self.context(&history))?;
        Ok(View {
            raw,
            state_dir,
            history,
            bindings,
            blocks,
        })
    }

    fn context<'a>(&'a self, history: &'a History) -> DecodeContext<'a> {
        DecodeContext {
            history,
            periods: &NO_PERIODS,
            exclude_block: None,
            viewed_job: &self.viewed_job,
        }
    }
}

/// Text to submit to a block: pasted, then Enter.
struct Input<'a> {
    text: &'a str,
    /// The process that must still run in the block: its foreground process,
    /// or an ancestor of it unless `at_prompt`.
    pid: u32,
    at_prompt: bool,
}

/// What one look at a block found.
enum Found {
    Look(Box<Look>),
    /// The input's process no longer runs in the block; nothing was sent.
    Refused,
    /// The block is gone.
    Gone,
}

/// A block as one look found it.
#[derive(Debug, Clone)]
struct Look {
    block: TerminalBlock,
    /// Whether it reported working or blocked during the look.
    active: bool,
}

impl Look {
    fn status(&self) -> AgentStatus {
        self.block
            .agent
            .as_ref()
            .map_or(AgentStatus::Unknown, |agent| agent.status)
    }
}

impl View {
    fn agents(&self) -> impl Iterator<Item = (&TerminalBlock, &AgentState)> {
        self.blocks
            .iter()
            .filter_map(|block| Some((block, block.agent.as_ref()?)))
    }

    /// The agent `target` names: a block id, a name, or SESSION/NAME.
    fn resolve(&self, target: &str) -> Result<(&TerminalBlock, &AgentState), Failure> {
        let in_session = target.rsplit_once('/');
        let matches: Vec<_> = self
            .agents()
            .filter(|(block, _)| {
                let name = block.name.as_deref();
                block.block_id == target
                    || name == Some(target)
                    || in_session
                        .is_some_and(|(session, n)| block.session == session && name == Some(n))
            })
            .collect();
        match matches[..] {
            [agent] => Ok(agent),
            [] => Err(Failure::new(
                "agent_not_found",
                format!("agent target {target} not found"),
            )),
            _ => {
                let found: Vec<String> = matches
                    .iter()
                    .map(|(block, _)| format!("{} in session {}", block.block_id, block.session))
                    .collect();
                Err(Failure::new(
                    "agent_target_ambiguous",
                    format!(
                        "agent target {target} matches {}; use a block id or SESSION/NAME",
                        found.join(", ")
                    ),
                ))
            }
        }
    }

    /// Fails when an agent other than block `except` has `name`.
    fn name_free(&self, name: &str, except: Option<&str>) -> Result<(), Failure> {
        let taken = self.agents().find(|(block, _)| {
            block.name.as_deref() == Some(name) && Some(block.block_id.as_str()) != except
        });
        match taken {
            Some((block, _)) => Err(Failure::new(
                "agent_name_taken",
                format!("agent name {name} is taken by {}", block.block_id),
            )),
            None => Ok(()),
        }
    }

    /// Herdr's agent fields for `block`, plus its session's worktree binding.
    fn info(&self, block: &TerminalBlock) -> Value {
        let agent = block.agent.as_ref();
        let worktree = self.bindings.session(&block.session_id).map(|binding| {
            json!({
                "repo_name": binding.worktree.repo_name,
                "repo_key": binding.worktree.repo_key,
                "checkout_path": binding.checkout_path,
                "branch": binding.branch,
                "is_linked_worktree": binding.worktree.is_linked_worktree,
            })
        });
        json!({
            "pane_id": block.block_id,
            "workspace_id": block.session_id,
            "tab_id": block.window_id,
            "workspace": block.session,
            "tab": block.window,
            "name": block.name,
            "agent": agent.map(|a| a.agent),
            "agent_status": agent.map_or(AgentStatus::Unknown, |a| a.status),
            "message": agent.and_then(|a| a.message.as_deref()),
            "blocked_kind": agent.and_then(|a| a.blocked_kind.as_deref()),
            "cwd": block.cwd,
            "terminal_title": block.title,
            "terminal_title_stripped": block
                .title
                .as_deref()
                .map(decode::strip_title)
                .filter(|title| !title.is_empty()),
            "worktree": worktree,
        })
    }

    /// One look at `block`: waits up to `wait` for its status to change, then
    /// reads it. With `input`, first submits it.
    fn look(
        &self,
        server: &Server,
        block: &TerminalBlock,
        wait: Duration,
        input: Option<&Input>,
    ) -> Result<Found, Failure> {
        let mut args = json!({
            "session_id": block.session_id,
            "block_id": block.block_id,
            "wait_s": wait.as_secs_f64(),
        });
        if let Some(input) = input {
            args["submit"] =
                json!({"text": input.text, "pid": input.pid, "at_prompt": input.at_prompt});
        }
        let answer = server.rex.script(AGENT, &args)?;
        if answer["gone"] == true {
            return Ok(Found::Gone);
        }
        if answer["refused"] == true {
            return Ok(Found::Refused);
        }
        let info =
            BlockInfo::deserialize(&answer).context("Invalid answer from rex script agent")?;
        let mut block = block.clone();
        block.update(&info, &server.context(&self.history));
        Ok(Found::Look(Box::new(Look {
            block,
            active: answer["active"] == true,
        })))
    }

    /// A look without input: `None` once the block is gone.
    fn observe(
        &self,
        server: &Server,
        block: &TerminalBlock,
        wait: Duration,
    ) -> Result<Option<Look>, Failure> {
        Ok(match self.look(server, block, wait, None)? {
            Found::Look(look) => Some(*look),
            Found::Refused | Found::Gone => None,
        })
    }

    /// Records that the calling block waits on `block` until the result is
    /// dropped, unless `block` waits on it: that wait would never end.
    fn wait_on(
        &self,
        server: &Server,
        block: &TerminalBlock,
    ) -> Result<Option<waits::Wait>, Failure> {
        let Some(caller) = &server.caller else {
            return Ok(None);
        };
        let chain = match waits::register(&self.state_dir, caller, &block.block_id)? {
            Ok(wait) => return Ok(Some(wait)),
            Err(chain) => chain,
        };
        let name = |id: &str| {
            self.blocks
                .iter()
                .find(|b| b.block_id == id)
                .map_or(id.to_owned(), |b| label(b).to_owned())
        };
        let message = match chain.as_slice() {
            [] | [_] => "an agent cannot wait on itself: it works until the wait ends".to_owned(),
            [target, between @ .., _] => {
                let mut message = format!("agent {} waits on you", name(target));
                if !between.is_empty() {
                    let names: Vec<String> =
                        chain[..chain.len() - 1].iter().map(|id| name(id)).collect();
                    message += &format!(" ({} → you)", names.join(" → "));
                }
                message
                    + ", so this wait would last until a timeout: end your turn with your answer or question, or prompt it without --wait"
            }
        };
        Err(Failure::new("agent_waiting_on_you", message))
    }
}

/// How messages name an agent: its name, else its block id.
fn label(block: &TerminalBlock) -> &str {
    block.name.as_deref().unwrap_or(&block.block_id)
}

fn not_running(agent: &str) -> Failure {
    Failure::new(
        "agent_not_running",
        format!("agent {agent} is no longer running"),
    )
}

/// Herdr's rule for agent names.
fn valid_name(name: &str) -> Result<(), Failure> {
    let mut bytes = name.bytes();
    let valid = name.len() <= 32
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(Failure::new("invalid_agent_name", NAME_RULE))
    }
}

/// Holds `block_id`'s input lock until dropped.
fn input_lock(state_dir: &Path, block_id: &str) -> Result<File, Failure> {
    // Rex block ids are `block:<uuid>`, a plain file name.
    if block_id.is_empty() || block_id.starts_with('.') || block_id.contains(['/', '\0']) {
        return Err(Failure::new(
            "rex_error",
            format!("Invalid block id {block_id:?}"),
        ));
    }
    Ok(lock_state(&state_dir.join("input").join(block_id))?)
}

/// The lines of a capture, without the spaces Rex pads rendered rows with; for
/// `recent` output, also without the blank rows after its last text.
fn lines_of(capture: &str, recent: bool) -> Vec<&str> {
    let mut lines: Vec<&str> = capture.lines().map(str::trim_end).collect();
    while recent && lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// The last `count` of `lines` as text, and whether any were dropped.
fn last_lines(lines: &[&str], count: usize) -> (String, bool) {
    let first = lines.len().saturating_sub(count);
    (lines[first..].join("\n"), first > 0)
}

fn read(server: &Server, args: ReadArgs) -> Reply {
    let view = server.load()?;
    let (block, _) = view.resolve(&args.target)?;
    let place = [
        "-s",
        block.session_id.as_str(),
        "-b",
        block.block_id.as_str(),
    ];
    let lines = args.lines.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
    let (source, (text, truncated)) = match args.source {
        ReadSource::Visible => {
            let size = server
                .rex
                .output(&[&["block", "call"][..], &place, &["size"]].concat())?;
            let size: Value = serde_json::from_str(&size).context("Invalid block size from rex")?;
            let rows = size["rows"]
                .as_u64()
                .and_then(|rows| usize::try_from(rows).ok())
                .context("Rex did not report the block's rows")?;
            // The screen is the capture's last rows; only fewer than those is truncated.
            let output = server.rex.output(&[&["capture"][..], &place].concat())?;
            let keep = lines.map_or(rows, |lines| lines.min(rows));
            let (text, _) = last_lines(&lines_of(&output, false), keep);
            ("visible", (text, keep < rows))
        }
        ReadSource::Recent | ReadSource::RecentUnwrapped => {
            let mut capture = [&["capture"][..], &place].concat();
            let source = if args.source == ReadSource::Recent {
                "recent"
            } else {
                capture.push("--unwrap");
                "recent-unwrapped"
            };
            let output = server.rex.output(&capture)?;
            let count = lines.unwrap_or(RECENT_LINES);
            (source, last_lines(&lines_of(&output, true), count))
        }
    };
    Ok(json!({"type": "pane_read", "read": {
        "pane_id": block.block_id,
        "workspace_id": block.session_id,
        "tab_id": block.window_id,
        "source": source,
        "format": "text",
        "text": text,
        "truncated": truncated,
    }}))
}

fn send_keys(server: &Server, target: &str, keys: &[String]) -> Reply {
    let view = server.load()?;
    let (block, _) = view.resolve(target)?;
    let _lock = input_lock(&view.state_dir, &block.block_id)?;
    let mut args = vec![
        "send-key",
        "-s",
        &block.session_id,
        "-b",
        &block.block_id,
        "--",
    ];
    args.extend(keys.iter().map(String::as_str));
    // Rex checks every key before it sends any.
    match server.rex.output(&args) {
        Ok(_) => Ok(json!({"type": "ok"})),
        Err(error) => {
            let message = format!("{error:#}");
            Err(match message.find("invalid key") {
                Some(at) => Failure::new(
                    "invalid_key",
                    message[at..].lines().next().unwrap_or_default(),
                ),
                None => error.into(),
            })
        }
    }
}

fn prompt(server: &Server, args: PromptArgs, elapsed: &dyn Fn() -> Duration) -> Reply {
    let view = server.load()?;
    let (block, state) = view.resolve(&args.target)?;
    let agent = label(block);
    match state.status {
        AgentStatus::Blocked => {
            let waiting = state.blocked_kind.as_deref().unwrap_or("input");
            return Err(Failure::new(
                "agent_blocked",
                format!(
                    "agent {agent} is blocked waiting for {waiting}: read its screen and answer that first"
                ),
            ));
        }
        AgentStatus::Unknown if REPORTING.contains(&state.agent) => {
            return Err(Failure::new(
                "agent_not_ready",
                format!(
                    "agent {agent} has not reported its state: it may be starting or at a startup dialog; read its screen"
                ),
            ));
        }
        _ => {}
    }
    let pid = state.pid.ok_or_else(|| not_running(agent))?;
    let _waiting = if args.wait {
        view.wait_on(server, block)?
    } else {
        None
    };
    let input = Input {
        text: &args.text,
        pid,
        at_prompt: false,
    };
    let mut submitted = false;
    let mut observe = |wait: Duration| -> Result<Option<Look>, Failure> {
        if submitted {
            return view.observe(server, block, wait);
        }
        submitted = true;
        let _lock = input_lock(&view.state_dir, &block.block_id)?;
        match view.look(server, block, wait, Some(&input))? {
            Found::Look(look) => Ok(Some(*look)),
            Found::Refused | Found::Gone => Err(not_running(agent)),
        }
    };
    let look = if args.wait {
        let goal = Goal {
            until: until_or_settled(args.until),
            needs_activity: state.status != AgentStatus::Working,
            timeout: args.timeout.map(Duration::from_millis),
        };
        wait_until(&mut observe, elapsed, pid, &goal, agent)?
    } else {
        observe(Duration::ZERO)?.ok_or_else(|| not_running(agent))?
    };
    Ok(json!({"type": "agent_prompted", "agent": view.info(&look.block)}))
}

fn wait(server: &Server, args: WaitArgs, elapsed: &dyn Fn() -> Duration) -> Reply {
    let view = server.load()?;
    let (block, state) = view.resolve(&args.target)?;
    let until = until_or_settled(args.until);
    if until.contains(&state.status) {
        return Ok(json!({"type": "wait_matched", "agent": view.info(block)}));
    }
    let agent = label(block);
    let pid = state.pid.ok_or_else(|| not_running(agent))?;
    let _waiting = view.wait_on(server, block)?;
    let goal = Goal {
        until,
        needs_activity: false,
        timeout: args.timeout.map(Duration::from_millis),
    };
    let mut observe = |wait: Duration| view.observe(server, block, wait);
    let look = wait_until(&mut observe, elapsed, pid, &goal, agent)?;
    Ok(json!({"type": "wait_matched", "agent": view.info(&look.block)}))
}

fn until_or_settled(until: Vec<AgentStatus>) -> Vec<AgentStatus> {
    if until.is_empty() {
        SETTLED.to_vec()
    } else {
        until
    }
}

/// What a wait ends on.
struct Goal {
    until: Vec<AgentStatus>,
    /// Whether the agent must first work or block, within [`STALL`].
    needs_activity: bool,
    /// Since the command was given.
    timeout: Option<Duration>,
}

/// Looks at agent process `pid` until it is in a state of `goal.until`. Each
/// look waits at most [`POLL`] and never past the timeout or the stall limit,
/// which counts from the first look.
fn wait_until(
    observe: &mut dyn FnMut(Duration) -> Result<Option<Look>, Failure>,
    elapsed: &dyn Fn() -> Duration,
    pid: u32,
    goal: &Goal,
    agent: &str,
) -> Result<Look, Failure> {
    let first_look = elapsed();
    let mut active = !goal.needs_activity;
    loop {
        let now = elapsed();
        let mut wait = POLL;
        if let Some(timeout) = goal.timeout {
            if now >= timeout {
                return Err(Failure::new(
                    "timeout",
                    format!(
                        "agent {agent} did not reach a requested state within {} ms",
                        timeout.as_millis()
                    ),
                ));
            }
            wait = wait.min(timeout - now);
        }
        if !active {
            let stall = first_look + STALL;
            if now >= stall {
                return Err(Failure::new(
                    "agent_prompt_stalled",
                    format!(
                        "agent {agent} did not start working within {} ms of the prompt; read its screen before sending it again",
                        STALL.as_millis()
                    ),
                ));
            }
            wait = wait.min(stall - now);
        }
        let look = observe(wait)?
            .filter(|look| look.block.pids.contains(&pid))
            .ok_or_else(|| not_running(agent))?;
        let status = look.status();
        active |= look.active || matches!(status, AgentStatus::Working | AgentStatus::Blocked);
        if active && goal.until.contains(&status) {
            return Ok(look);
        }
    }
}

/// Looks at a block where `kind` was just typed at the shell `shell_pid` until
/// the agent reports it is ready.
fn wait_ready(
    observe: &mut dyn FnMut(Duration) -> Result<Option<Look>, Failure>,
    elapsed: &dyn Fn() -> Duration,
    kind: &str,
    shell_pid: u32,
    timeout: Duration,
) -> Result<Look, Failure> {
    let mut started = false;
    loop {
        let now = elapsed();
        if now >= timeout {
            let ms = timeout.as_millis();
            return Err(if started {
                Failure::new(
                    "agent_not_ready",
                    format!(
                        "{kind} runs but has not reported ready within {ms} ms; it may be waiting at a startup dialog: read its screen"
                    ),
                )
            } else {
                Failure::new("timeout", format!("{kind} did not start within {ms} ms"))
            });
        }
        let look = observe(POLL.min(timeout - now))?.ok_or_else(|| {
            Failure::new(
                "agent_not_running",
                format!("the block closed before {kind} was ready"),
            )
        })?;
        match &look.block.agent {
            Some(state) if state.agent != kind => {
                return Err(Failure::new(
                    "agent_kind_mismatch",
                    format!("the block runs {}, not {kind}", state.agent),
                ));
            }
            Some(state) => {
                started = true;
                match (state.record.is_some(), state.status) {
                    (true, AgentStatus::Idle | AgentStatus::Done) => return Ok(look),
                    (true, AgentStatus::Blocked) => {
                        let waiting = state.blocked_kind.as_deref().unwrap_or("input");
                        return Err(Failure::new(
                            "agent_not_ready",
                            format!(
                                "{kind} is blocked at startup, waiting for {waiting}: read its screen"
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            None if started
                && look
                    .block
                    .foreground
                    .as_ref()
                    .is_some_and(|(_, pid)| *pid == shell_pid) =>
            {
                return Err(Failure::new(
                    "agent_not_running",
                    format!("{kind} exited before it was ready"),
                ));
            }
            None => {}
        }
    }
}

fn rename(server: &Server, args: RenameArgs) -> Reply {
    let view = server.load()?;
    let (block, _) = view.resolve(&args.target)?;
    let label = match &args.name {
        Some(name) => {
            valid_name(name)?;
            view.name_free(name, Some(&block.block_id))?;
            name.clone()
        }
        None => block.default_label.clone(),
    };
    server.rex.output(&[
        "block",
        "rename",
        "-s",
        &block.session_id,
        &block.block_id,
        &label,
    ])?;
    let mut renamed = block.clone();
    renamed.name = args.name;
    Ok(json!({"type": "agent_info", "agent": view.info(&renamed)}))
}

fn focus(server: &Server, target: &str) -> Reply {
    let view = server.load()?;
    let (block, _) = view.resolve(target)?;
    let answer = server
        .rex
        .script(FOCUS, &json!({"kind": "block", "id": block.block_id}))?;
    let found: Focused = serde_json::from_value(answer).context("Invalid focus answer")?;
    let Some(client) = choose_client(None, &found.apps) else {
        let why = if found.apps.is_empty() {
            "no Rex app is connected"
        } else {
            "several Rex apps are connected, so which one should show it is unclear"
        };
        return Err(Failure::new(
            "focus_failed",
            format!("Cannot show agent {}: {why}", label(block)),
        ));
    };
    let select = [
        ("session_id", found.session_id.as_str()),
        ("window_id", found.window_id.as_str()),
    ];
    server
        .rex
        .client_action(&client, "session.select", &select)
        .map_err(|error| Failure::new("focus_failed", format!("{error:#}")))?;
    let history = tracker(&view.state_dir);
    history.record(&ActiveLocation {
        workspace_id: Some(found.session_id.clone()),
        tab_id: Some(found.window_id.clone()),
        pane_id: Some(block.block_id.clone()),
    })?;
    // The user has now seen the agent, so a finished one reads idle.
    let seen = history.load()?;
    let blocks = decode::terminal_blocks(&view.raw, &server.context(&seen))?;
    let shown = blocks
        .iter()
        .find(|shown| shown.block_id == block.block_id)
        .unwrap_or(block);
    Ok(json!({"type": "agent_info", "agent": view.info(shown)}))
}

fn start(server: &Server, args: StartArgs, elapsed: &dyn Fn() -> Duration) -> Reply {
    valid_name(&args.name)?;
    let kind = args.kind.as_str();
    if !REPORTING.contains(&kind) {
        return Err(Failure::new(
            "unsupported_agent_kind",
            format!(
                "agent kind {kind} cannot be started in Rex: only claude and codex report when they are ready"
            ),
        ));
    }
    let timeout_ms = args.timeout.unwrap_or(START_TIMEOUT_MS);
    if !START_TIMEOUTS_MS.contains(&timeout_ms) {
        return Err(Failure::new(
            "invalid_agent_timeout",
            "agent start timeout must be greater than 3000ms and at most 300000ms",
        ));
    }
    if args
        .args
        .iter()
        .any(|arg| arg.chars().any(char::is_control))
    {
        return Err(Failure::new(
            "invalid_agent_argument",
            "agent arguments cannot be encoded safely for the target shell",
        ));
    }
    let view = server.load()?;
    let pane = view
        .blocks
        .iter()
        .find(|block| block.block_id == args.pane)
        .ok_or_else(|| {
            Failure::new(
                "agent_pane_not_found",
                format!("agent target pane {} not found", args.pane),
            )
        })?;
    let shell = pane
        .foreground
        .as_ref()
        .map(|(name, pid)| (name.trim_start_matches('-'), *pid))
        .filter(|(name, _)| SHELLS.contains(name));
    let (Some((shell, shell_pid)), None) = (shell, &pane.agent) else {
        let running = pane
            .foreground
            .as_ref()
            .map_or("nothing", |(name, _)| name.as_str());
        return Err(Failure::new(
            "agent_pane_busy",
            format!(
                "pane {} is not at a shell prompt Canopy can type into: it runs {running}",
                pane.block_id
            ),
        ));
    };
    view.name_free(&args.name, None)?;
    server.rex.output(&[
        "block",
        "rename",
        "-s",
        &pane.session_id,
        &pane.block_id,
        &args.name,
    ])?;
    let mut named = pane.clone();
    named.name = Some(args.name.clone());
    let argv: Vec<String> = std::iter::once(args.kind.clone())
        .chain(args.args)
        .collect();
    let command: Vec<String> = argv
        .iter()
        .map(|arg| shell_quote(arg, shell == "fish"))
        .collect();
    let input = Input {
        text: &command.join(" "),
        pid: shell_pid,
        at_prompt: true,
    };
    let mut typed = false;
    let mut observe = |wait: Duration| -> Result<Option<Look>, Failure> {
        if typed {
            return view.observe(server, &named, wait);
        }
        typed = true;
        let _lock = input_lock(&view.state_dir, &named.block_id)?;
        match view.look(server, &named, wait, Some(&input))? {
            Found::Look(look) => Ok(Some(*look)),
            Found::Refused => Err(Failure::new(
                "agent_pane_busy",
                format!("pane {} left its shell prompt", named.block_id),
            )),
            Found::Gone => Ok(None),
        }
    };
    let ready = wait_ready(
        &mut observe,
        elapsed,
        kind,
        shell_pid,
        Duration::from_millis(timeout_ms),
    )
    .map_err(|failure| with_screen(server, &named, failure))?;
    Ok(json!({"type": "agent_started", "agent": view.info(&ready.block), "argv": argv}))
}

/// `failure` followed by the last lines of `block`'s screen, which show why the
/// typed command started no agent: a dialog, a shell error, or a question the
/// shell asked when it started.
fn with_screen(server: &Server, block: &TerminalBlock, mut failure: Failure) -> Failure {
    let capture = ["capture", "-s", &block.session_id, "-b", &block.block_id];
    if let Ok(output) = server.rex.output(&capture) {
        let lines: Vec<&str> = lines_of(&output, false)
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect();
        if !lines.is_empty() {
            let (text, _) = last_lines(&lines, START_SCREEN_LINES);
            failure.message += &format!("\n\nIts screen ends with:\n{text}");
        }
    }
    failure
}

/// `arg` as one word for a POSIX shell, or for fish, where backslashes in single
/// quotes escape.
fn shell_quote(arg: &str, fish: bool) -> String {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"%+,-./:=@_".contains(&b));
    if plain {
        arg.to_owned()
    } else if fish {
        format!("'{}'", arg.replace('\\', r"\\").replace('\'', r"\'"))
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::rex::decode::LiveRecord;
    use crate::backends::rex::testing::{Call, FakeRex};
    use crate::backends::rex::waits;
    use crate::cli::{Cli, Command, RexCommand};
    use crate::test_helpers::rerun_alone;
    use AgentStatus::{Blocked, Done, Idle, Unknown, Working};
    use clap::Parser;
    use fs2::FileExt;
    use std::cell::Cell;
    use std::fs;

    /// When the records below were updated: long before any test runs, so a
    /// focus now comes after it.
    const AT: &str = "2025-06-01T12:00:00Z";

    fn view_block(id: &str, label: &str) -> Value {
        json!({"block_id": id, "creator_name": "com.superlogical.terminal",
               "flavor": "com.superlogical.terminal.shell", "label": label})
    }

    /// A block's processes, title and status, as the snapshot and agent scripts
    /// report them.
    fn info(foreground: (&str, u32), ancestors: &[(&str, u32)], records: Value) -> Value {
        let ancestors: Vec<Value> = ancestors
            .iter()
            .map(|(name, pid)| json!({"name": name, "pid": pid}))
            .collect();
        json!({
            "process": {"foreground": {"name": foreground.0, "argv0": foreground.0,
                                       "pid": foreground.1, "cwd": "/work"},
                        "ancestors": ancestors},
            "title": {"title": format!("✳ {} title", foreground.0)},
            "status": {"records": records},
        })
    }

    fn record(state: &str, app: &str, owner: u32) -> Value {
        json!({"state": state, "app": app, "effective_app": app, "owner": {"pid": owner}, "updated_at": AT})
    }

    /// Session `pad`, bound to a worktree: Claude named `reviewer` (working), a
    /// zsh shell, and Gemini named `helper`, which reports nothing. Session
    /// `api`: an unnamed Codex blocked on a permission.
    fn snapshot() -> Value {
        let mut working = record("working", "claude-code", 30);
        working["msg"] = json!("Reading files");
        let mut blocked = record("blocked", "codex", 60);
        blocked["kind"] = json!("permission");
        blocked["msg"] = json!("Run cargo test?");
        json!({"host_id": "host", "sessions": [
            {"view": {"session_id": "session:pad", "label": "pad", "windows": [
                {"window_id": "window:main", "label": "main", "layers": [{"blocks": [
                    view_block("block:claude", "reviewer"),
                    view_block("block:shell", "shell"),
                    view_block("block:gemini", "helper"),
                ]}]}]},
             "blocks": {
                "block:claude": info(("claude", 30), &[("zsh", 3)], json!([working])),
                "block:shell": info(("zsh", 4), &[("login", 1)], json!({})),
                "block:gemini": info(("gemini", 50), &[("zsh", 5)], json!({})),
             }},
            {"view": {"session_id": "session:api", "label": "api", "windows": [
                {"window_id": "window:api", "label": "", "layers": [{"blocks": [
                    view_block("block:codex", "shell"),
                ]}]}]},
             "blocks": {"block:codex": info(("codex", 60), &[("zsh", 6)], json!([blocked]))}}
        ]})
    }

    /// `data` with block `id`, labeled `label`, added to session `session`.
    fn with_block(mut data: Value, session: usize, id: &str, label: &str, info: Value) -> Value {
        let session = &mut data["sessions"][session];
        session["view"]["windows"][0]["layers"][0]["blocks"]
            .as_array_mut()
            .unwrap()
            .push(view_block(id, label));
        session["blocks"][id] = info;
        data
    }

    const BINDINGS: &str = r#"{"version": 1, "hosts": {"host": {"sessions": {"session:pad": {
        "repo_key": "/src/pad/.git", "repo_name": "pad", "is_linked_worktree": true,
        "checkout_path": "/src/.worktrees/pad/review", "branch": "review", "bound_at": 1}}}}}"#;

    struct Fixture {
        fake: FakeRex,
        dir: tempfile::TempDir,
        server: Server,
    }

    impl Fixture {
        /// A server whose snapshot is `data`, with the bindings above.
        fn new(data: &Value) -> Self {
            let fake = FakeRex::new();
            fake.answer("snapshot", &data.to_string());
            let dir = tempfile::tempdir().unwrap();
            let bindings = dir.path().join("bindings.json");
            fs::write(&bindings, BINDINGS).unwrap();
            let server = Server {
                rex: Rex::new(fake.path.clone()),
                state_root: dir.path().join("state"),
                bindings: Some(bindings),
                viewed_job: |_, _| None,
                caller: None,
            };
            Self { fake, dir, server }
        }

        /// The commands run from block `caller`.
        fn called_from(mut self, caller: &str) -> Self {
            self.server.caller = Some(caller.to_owned());
            self
        }

        /// Runs `herdr-canopy rex agent ARGS…` at the moment it was given.
        fn run(&self, args: &[&str]) -> Reply {
            execute(&self.server, parse(args), &|| Duration::ZERO)
        }

        /// [`Self::run`] from block `caller`.
        fn run_as(&self, caller: &str, args: &[&str]) -> Reply {
            let server = Server {
                caller: Some(caller.to_owned()),
                ..self.server.clone()
            };
            execute(&server, parse(args), &|| Duration::ZERO)
        }

        fn state_dir(&self) -> PathBuf {
            self.dir.path().join("state/rex-host")
        }

        /// The arguments of every agent script run.
        fn agent_calls(&self) -> Vec<Value> {
            self.fake
                .calls_to("agent")
                .iter()
                .map(script_args)
                .collect()
        }
    }

    fn parse(args: &[&str]) -> AgentCommand {
        let cli = Cli::try_parse_from([&["herdr-canopy", "rex", "agent"], args].concat()).unwrap();
        let Some(Command::Rex(RexCommand::Agent(agent))) = cli.command else {
            panic!("not an agent command");
        };
        agent.command
    }

    fn script_args(call: &Call) -> Value {
        let at = call.argv.iter().position(|a| a == "--args").unwrap();
        serde_json::from_str(&call.argv[at + 1]).unwrap()
    }

    /// A call's arguments after `--autostart=false`.
    fn argv(call: &Call) -> Vec<&str> {
        call.argv[1..].iter().map(String::as_str).collect()
    }

    /// The agent script's answer: a look at a block reporting `info`.
    fn looked(info: Value, active: bool) -> String {
        let mut answer = info;
        answer["active"] = json!(active);
        answer.to_string()
    }

    #[test]
    fn replies_and_errors_come_in_herdrs_envelope() {
        assert_eq!(
            envelope("list", Ok(json!({"type": "agent_list", "agents": []}))),
            Ok(json!({"id": "cli:agent:list", "result": {"type": "agent_list", "agents": []}}))
        );
        assert_eq!(
            envelope(
                "send-keys",
                Err(Failure::new("invalid_key", "invalid key \"x\""))
            ),
            Err(
                json!({"error": {"code": "invalid_key", "message": "invalid key \"x\""},
                       "id": "cli:agent:send-keys"})
            )
        );
        let rex_failed = Failure::from(anyhow::anyhow!(
            "rex snapshot failed: server is not running"
        ));
        assert_eq!(rex_failed.code, "rex_error");
        assert!(rex_failed.message.contains("server is not running"));
    }

    #[test]
    fn the_list_shows_every_agent_with_its_place_state_and_worktree() {
        let fixture = Fixture::new(&snapshot());
        let list = fixture.run(&["list"]).unwrap();
        assert_eq!(list["type"], "agent_list");
        let agents = list["agents"].as_array().unwrap();
        let ids: Vec<&str> = agents
            .iter()
            .map(|a| a["pane_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["block:claude", "block:gemini", "block:codex"],
            "a shell is no agent"
        );
        assert_eq!(
            agents[0],
            json!({
                "pane_id": "block:claude", "workspace_id": "session:pad", "tab_id": "window:main",
                "workspace": "pad", "tab": "main", "name": "reviewer",
                "agent": "claude", "agent_status": "working", "message": "Reading files",
                "blocked_kind": null, "cwd": "/work",
                "terminal_title": "✳ claude title", "terminal_title_stripped": "claude title",
                "worktree": {"repo_name": "pad", "repo_key": "/src/pad/.git",
                             "checkout_path": "/src/.worktrees/pad/review", "branch": "review",
                             "is_linked_worktree": true},
            })
        );
        let gemini = &agents[1];
        assert_eq!(
            (&gemini["name"], &gemini["agent_status"]),
            (&json!("helper"), &json!("unknown")),
            "an agent that reports nothing is unknown"
        );
        let codex = &agents[2];
        assert_eq!(
            [
                &codex["name"],
                &codex["tab"],
                &codex["agent_status"],
                &codex["blocked_kind"],
                &codex["message"],
                &codex["worktree"]
            ],
            [
                &json!(null),
                &json!("window:api"),
                &json!("blocked"),
                &json!("permission"),
                &json!("Run cargo test?"),
                &json!(null)
            ],
            "unnamed, in an unlabeled window, in an unbound session"
        );
        let get = fixture.run(&["get", "reviewer"]).unwrap();
        assert_eq!(get, json!({"type": "agent_info", "agent": agents[0]}));
    }

    #[test]
    fn targets_are_block_ids_names_or_session_slash_name() {
        let mut data = snapshot();
        data["sessions"][1]["view"]["label"] = json!("web/api");
        data["sessions"][1]["view"]["windows"][0]["layers"][0]["blocks"][0]["label"] =
            json!("reviewer");
        let fixture = Fixture::new(&data);
        let get = |target: &str| {
            fixture
                .run(&["get", target])
                .map(|reply| reply["agent"]["pane_id"].as_str().unwrap().to_owned())
        };
        assert_eq!(get("block:claude"), Ok("block:claude".into()));
        assert_eq!(get("helper"), Ok("block:gemini".into()));
        assert_eq!(get("pad/reviewer"), Ok("block:claude".into()));
        assert_eq!(
            get("web/api/reviewer"),
            Ok("block:codex".into()),
            "session labels may hold a slash"
        );
        let ambiguous = get("reviewer").unwrap_err();
        assert_eq!(ambiguous.code, "agent_target_ambiguous");
        assert!(
            ambiguous.message.contains("block:claude") && ambiguous.message.contains("block:codex"),
            "{}",
            ambiguous.message
        );
        for missing in [
            "block:shell",
            "shell",
            "pad/shell",
            "api/reviewer",
            "pad/helper/x",
            "nobody",
            "block:nope",
        ] {
            assert_eq!(
                get(missing),
                Err(Failure::new(
                    "agent_not_found",
                    format!("agent target {missing} not found")
                )),
                "{missing}"
            );
        }
    }

    #[test]
    fn reads_capture_the_agents_block_and_keep_the_last_lines() {
        let fixture = Fixture::new(&snapshot());
        // Rex pads rendered rows with spaces and keeps the blank rows under a prompt.
        let mut output: String = (1..=300).map(|n| format!("line {n}  \n")).collect();
        output.push_str("   \n\n \n");
        fixture.fake.answer("capture", &output);
        let reply = fixture.run(&["read", "reviewer"]).unwrap();
        assert_eq!(reply["type"], "pane_read");
        let read = &reply["read"];
        assert_eq!(
            [
                &read["pane_id"],
                &read["workspace_id"],
                &read["tab_id"],
                &read["source"],
                &read["format"],
                &read["truncated"]
            ],
            [
                &json!("block:claude"),
                &json!("session:pad"),
                &json!("window:main"),
                &json!("recent"),
                &json!("text"),
                &json!(true)
            ]
        );
        let text = read["text"].as_str().unwrap();
        assert_eq!(text.lines().count(), 200, "200 lines unless told otherwise");
        assert!(text.starts_with("line 101\n") && text.ends_with("\nline 300"));
        let reply = fixture
            .run(&[
                "read",
                "reviewer",
                "--source",
                "recent-unwrapped",
                "--lines",
                "500",
            ])
            .unwrap();
        assert_eq!(
            (&reply["read"]["source"], &reply["read"]["truncated"]),
            (&json!("recent-unwrapped"), &json!(false))
        );
        assert_eq!(reply["read"]["text"].as_str().unwrap().lines().count(), 300);
        let captures = fixture.fake.calls_to("capture");
        assert_eq!(
            argv(&captures[0]),
            ["capture", "-s", "session:pad", "-b", "block:claude"]
        );
        assert_eq!(
            argv(&captures[1]),
            [
                "capture",
                "-s",
                "session:pad",
                "-b",
                "block:claude",
                "--unwrap"
            ]
        );
    }

    #[test]
    fn a_visible_read_is_the_screen_at_the_end_of_the_capture() {
        let fixture = Fixture::new(&snapshot());
        fixture
            .fake
            .answer("capture", "old 1\nold 2\nrow 1   \nrow 2\n   \n");
        fixture
            .fake
            .answer("block", r#"{"columns": 80, "rows": 3, "cell_width_px": 9}"#);
        let read = |args: &[&str]| {
            let reply = fixture
                .run(&[&["read", "reviewer", "--source", "visible"][..], args].concat())
                .unwrap();
            (
                reply["read"]["text"].clone(),
                reply["read"]["truncated"].clone(),
            )
        };
        assert_eq!(
            read(&[]),
            (json!("row 1\nrow 2\n"), json!(false)),
            "the screen's blank rows stay, without their spaces"
        );
        assert_eq!(read(&["--lines", "2"]), (json!("row 2\n"), json!(true)));
        assert_eq!(
            argv(&fixture.fake.calls_to("block")[0]),
            [
                "block",
                "call",
                "-s",
                "session:pad",
                "-b",
                "block:claude",
                "size"
            ]
        );
        assert_eq!(
            argv(&fixture.fake.calls_to("capture")[0]),
            ["capture", "-s", "session:pad", "-b", "block:claude"]
        );
    }

    #[test]
    fn read_text_drops_trailing_spaces_and_keeps_the_last_lines() {
        assert_eq!(
            lines_of("a  \n b\n  \n\n", true),
            ["a", " b"],
            "recent output ends at its last text"
        );
        assert_eq!(lines_of("a  \n b\n  \n\n", false), ["a", " b", "", ""]);
        assert_eq!(lines_of("", true), Vec::<&str>::new());
        assert_eq!(last_lines(&["a", "b", "c"], 2), ("b\nc".into(), true));
        assert_eq!(last_lines(&["a", "b", "c"], 3), ("a\nb\nc".into(), false));
        assert_eq!(last_lines(&["a", "b", "c"], 9), ("a\nb\nc".into(), false));
        assert_eq!(last_lines(&[], 5), (String::new(), false));
    }

    #[test]
    fn keys_go_to_the_agents_block_and_rex_checks_them_all_first() {
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer("send-key", "");
        assert_eq!(
            fixture.run(&["send-keys", "reviewer", "esc", "ctrl+c"]),
            Ok(json!({"type": "ok"}))
        );
        assert_eq!(
            argv(&fixture.fake.calls_to("send-key")[0]),
            [
                "send-key",
                "-s",
                "session:pad",
                "-b",
                "block:claude",
                "--",
                "esc",
                "ctrl+c"
            ]
        );
        fixture.fake.answer_with(
            "send-key",
            "",
            "Error: invalid key \"bogus\": unknown key \"bogus\"\n\nUsage:\n  rex send-key KEY... [flags]\n",
            2,
        );
        assert_eq!(
            fixture.run(&["send-keys", "reviewer", "esc", "bogus"]),
            Err(Failure::new(
                "invalid_key",
                "invalid key \"bogus\": unknown key \"bogus\""
            ))
        );
        fixture
            .fake
            .answer_with("send-key", "", "Error: block not found", 1);
        assert_eq!(
            fixture
                .run(&["send-keys", "reviewer", "esc"])
                .unwrap_err()
                .code,
            "rex_error"
        );
    }

    #[test]
    fn a_prompt_is_submitted_to_the_agents_process_and_replies_at_once() {
        let fixture = Fixture::new(&snapshot());
        let working = info(
            ("claude", 30),
            &[("zsh", 3)],
            json!([record("working", "claude-code", 30)]),
        );
        fixture.fake.answer("agent", &looked(working, true));
        let reply = fixture
            .run(&["prompt", "pad/reviewer", "Review the diff"])
            .unwrap();
        assert_eq!(
            (
                &reply["type"],
                &reply["agent"]["pane_id"],
                &reply["agent"]["agent_status"]
            ),
            (
                &json!("agent_prompted"),
                &json!("block:claude"),
                &json!("working")
            )
        );
        assert_eq!(
            fixture.agent_calls(),
            [
                json!({"session_id": "session:pad", "block_id": "block:claude", "wait_s": 0.0,
                    "submit": {"text": "Review the diff", "pid": 30, "at_prompt": false}})
            ]
        );
        assert!(
            fixture.state_dir().join("input/block:claude.lock").exists(),
            "input is locked per block in the server's state directory"
        );
    }

    #[test]
    fn prompts_are_refused_while_the_agent_cannot_take_input() {
        let fixture = Fixture::new(&snapshot());
        let blocked = fixture.run(&["prompt", "block:codex", "go"]).unwrap_err();
        assert_eq!(blocked.code, "agent_blocked");
        assert!(
            blocked.message.contains("permission"),
            "{}",
            blocked.message
        );
        let mut silent = snapshot();
        silent["sessions"][0]["blocks"]["block:claude"]["status"]["records"] = json!({});
        let starting = Fixture::new(&silent);
        assert_eq!(
            starting
                .run(&["prompt", "reviewer", "go"])
                .unwrap_err()
                .code,
            "agent_not_ready",
            "a silent Claude may be at a startup dialog"
        );
        assert!(
            fixture.agent_calls().is_empty() && starting.agent_calls().is_empty(),
            "nothing is sent"
        );
    }

    #[test]
    fn agents_that_never_report_take_prompts_while_unknown() {
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer(
            "agent",
            &looked(info(("gemini", 50), &[("zsh", 5)], json!({})), false),
        );
        let reply = fixture.run(&["prompt", "helper", "hi"]).unwrap();
        assert_eq!(reply["agent"]["agent_status"], "unknown");
        assert_eq!(fixture.agent_calls()[0]["submit"]["pid"], 50);
    }

    #[test]
    fn a_prompt_to_an_agent_that_left_its_block_is_not_sent() {
        let fixture = Fixture::new(&snapshot());
        for answer in [json!({"refused": true}), json!({"gone": true})] {
            fixture.fake.answer("agent", &answer.to_string());
            assert_eq!(
                fixture.run(&["prompt", "reviewer", "go"]).unwrap_err().code,
                "agent_not_running",
                "{answer}"
            );
        }
    }

    #[test]
    fn a_prompt_with_wait_replies_once_the_agent_settles() {
        let mut data = snapshot();
        data["sessions"][0]["blocks"]["block:claude"]["status"]["records"] =
            json!([record("idle", "claude-code", 30)]);
        let fixture = Fixture::new(&data);
        let idle = info(
            ("claude", 30),
            &[("zsh", 3)],
            json!([record("idle", "claude-code", 30)]),
        );
        fixture.fake.answer("agent", &looked(idle, true));
        let reply = fixture
            .run(&["prompt", "reviewer", "go", "--wait", "--timeout", "60000"])
            .unwrap();
        assert_eq!(
            (&reply["type"], &reply["agent"]["agent_status"]),
            (&json!("agent_prompted"), &json!("idle")),
            "it worked during the submission's look, then settled"
        );
        let calls = fixture.agent_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]["wait_s"], 1.0,
            "the submission's look waits for the agent to react"
        );
    }

    #[test]
    fn a_wait_replies_at_once_when_the_agent_is_already_there() {
        let fixture = Fixture::new(&snapshot());
        let reply = fixture.run(&["wait", "block:codex"]).unwrap();
        assert_eq!(
            (&reply["type"], &reply["agent"]["agent_status"]),
            (&json!("wait_matched"), &json!("blocked"))
        );
        let reply = fixture
            .run(&["wait", "reviewer", "--until", "working"])
            .unwrap();
        assert_eq!(reply["agent"]["agent_status"], "working");
        assert!(fixture.agent_calls().is_empty());
    }

    #[test]
    fn a_wait_looks_at_the_agent_until_it_settles_and_sends_nothing() {
        let fixture = Fixture::new(&snapshot());
        let idle = info(
            ("claude", 30),
            &[("zsh", 3)],
            json!([record("idle", "claude-code", 30)]),
        );
        fixture.fake.answer("agent", &looked(idle, false));
        let reply = fixture.run(&["wait", "reviewer"]).unwrap();
        assert_eq!(reply["agent"]["agent_status"], "idle");
        assert_eq!(
            fixture.agent_calls(),
            [json!({"session_id": "session:pad", "block_id": "block:claude", "wait_s": 1.0})]
        );
    }

    /// Records that block `waiter` waits on block `target`, until dropped.
    fn waiting(fixture: &Fixture, waiter: &str, target: &str) -> waits::Wait {
        waits::register(&fixture.state_dir(), waiter, target)
            .unwrap()
            .unwrap()
    }

    fn reviewer_look(state: &str) -> String {
        let records = json!([record(state, "claude-code", 30)]);
        looked(info(("claude", 30), &[("zsh", 3)], records), false)
    }

    #[test]
    fn waiting_on_an_agent_that_waits_on_you_is_refused_before_anything_is_sent() {
        let fixture = Fixture::new(&snapshot()).called_from("block:me");
        let _reviewer = waiting(&fixture, "block:claude", "block:me");
        for args in [
            &["wait", "reviewer"][..],
            &["prompt", "reviewer", "go", "--wait"],
        ] {
            let error = fixture.run(args).unwrap_err();
            assert_eq!(error.code, "agent_waiting_on_you", "{args:?}");
            assert!(
                error.message.contains("agent reviewer waits on you"),
                "{}",
                error.message
            );
        }
        assert!(
            fixture.agent_calls().is_empty(),
            "nothing is sent or looked at"
        );
    }

    #[test]
    fn the_refusal_names_the_agents_through_which_the_target_waits_on_you() {
        let fixture = Fixture::new(&snapshot()).called_from("block:me");
        let _held = [
            waiting(&fixture, "block:claude", "block:gemini"),
            waiting(&fixture, "block:gemini", "block:me"),
        ];
        let error = fixture.run(&["wait", "reviewer"]).unwrap_err();
        assert_eq!(error.code, "agent_waiting_on_you");
        assert!(
            error.message.contains("reviewer → helper → you"),
            "{}",
            error.message
        );
    }

    #[test]
    fn an_agent_cannot_wait_on_itself() {
        let fixture = Fixture::new(&snapshot()).called_from("block:claude");
        for args in [
            &["wait", "reviewer"][..],
            &["prompt", "reviewer", "go", "--wait"],
        ] {
            assert_eq!(
                fixture.run(args).unwrap_err().code,
                "agent_waiting_on_you",
                "{args:?}"
            );
        }
        assert!(fixture.agent_calls().is_empty());
    }

    #[test]
    fn a_prompt_without_wait_reaches_an_agent_that_waits_on_you() {
        let fixture = Fixture::new(&snapshot()).called_from("block:me");
        let _reviewer = waiting(&fixture, "block:claude", "block:me");
        fixture.fake.answer("agent", &reviewer_look("working"));
        fixture.run(&["prompt", "reviewer", "my answer"]).unwrap();
        assert_eq!(fixture.agent_calls()[0]["submit"]["text"], "my answer");
    }

    #[test]
    fn a_wait_that_is_already_met_is_not_refused() {
        let fixture = Fixture::new(&snapshot()).called_from("block:me");
        let _reviewer = waiting(&fixture, "block:claude", "block:me");
        let reply = fixture
            .run(&["wait", "reviewer", "--until", "working"])
            .unwrap();
        assert_eq!(reply["agent"]["agent_status"], "working");
    }

    #[test]
    fn waits_from_outside_a_block_are_not_checked() {
        let fixture = Fixture::new(&snapshot());
        let _reviewer = waiting(&fixture, "block:claude", "block:me");
        fixture.fake.answer("agent", &reviewer_look("idle"));
        let reply = fixture.run(&["wait", "reviewer"]).unwrap();
        assert_eq!(reply["agent"]["agent_status"], "idle");
    }

    #[test]
    fn a_running_wait_is_seen_by_the_agent_it_waits_on_and_gone_once_it_ends() {
        let planner = info(
            ("claude", 70),
            &[("zsh", 7)],
            json!([record("working", "claude-code", 70)]),
        );
        let data = with_block(snapshot(), 0, "block:me", "planner", planner);
        let fixture = Fixture::new(&data).called_from("block:me");
        fixture.fake.answer("agent", &reviewer_look("working"));
        let waits = fixture.state_dir().join("waits");
        let entries = || fs::read_dir(&waits).map_or(0, |entries| entries.count());
        let started = Instant::now();
        std::thread::scope(|scope| {
            let planner_wait = scope.spawn(|| {
                let args = ["wait", "reviewer", "--timeout", "15000"];
                execute(&fixture.server, parse(&args), &|| started.elapsed())
            });
            while entries() == 0 {
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "no wait recorded"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let error = fixture
                .run_as("block:claude", &["wait", "planner"])
                .unwrap_err();
            assert_eq!(error.code, "agent_waiting_on_you", "{}", error.message);
            fixture.fake.answer("agent", &reviewer_look("idle"));
            let reply = planner_wait.join().unwrap().unwrap();
            assert_eq!(reply["agent"]["agent_status"], "idle");
        });
        assert_eq!(entries(), 0, "the wait that ended left nothing behind");
    }

    #[test]
    fn renaming_sets_the_label_and_clearing_restores_the_default() {
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer("block", "");
        let reply = fixture.run(&["rename", "block:codex", "tester"]).unwrap();
        assert_eq!(
            (&reply["type"], &reply["agent"]["name"]),
            (&json!("agent_info"), &json!("tester"))
        );
        let reply = fixture.run(&["rename", "reviewer", "--clear"]).unwrap();
        assert_eq!(reply["agent"]["name"], Value::Null);
        assert!(
            fixture.run(&["rename", "helper", "helper"]).is_ok(),
            "an agent keeps its own name"
        );
        let calls = fixture.fake.calls_to("block");
        let renames: Vec<Vec<&str>> = calls.iter().map(argv).collect();
        assert_eq!(
            renames,
            [
                [
                    "block",
                    "rename",
                    "-s",
                    "session:api",
                    "block:codex",
                    "tester"
                ],
                [
                    "block",
                    "rename",
                    "-s",
                    "session:pad",
                    "block:claude",
                    "shell"
                ],
                [
                    "block",
                    "rename",
                    "-s",
                    "session:pad",
                    "block:gemini",
                    "helper"
                ],
            ]
        );
    }

    #[test]
    fn names_follow_herdrs_rule_and_belong_to_one_agent() {
        let longest = "a".repeat(32);
        for name in ["a", "r2-d2_x", longest.as_str()] {
            assert_eq!(valid_name(name), Ok(()), "{name}");
        }
        let too_long = "a".repeat(33);
        for name in [
            "",
            "Reviewer",
            "1st",
            "-x",
            "_x",
            "re viewer",
            "rëviewer",
            "a/b",
            "a.b",
            too_long.as_str(),
        ] {
            assert_eq!(
                valid_name(name),
                Err(Failure::new("invalid_agent_name", NAME_RULE)),
                "{name}"
            );
        }
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer("block", "");
        assert_eq!(
            fixture
                .run(&["rename", "block:codex", "helper"])
                .unwrap_err()
                .code,
            "agent_name_taken"
        );
        assert_eq!(
            fixture
                .run(&["rename", "block:codex", "Helper"])
                .unwrap_err()
                .code,
            "invalid_agent_name"
        );
        assert!(
            fixture.fake.calls_to("block").is_empty(),
            "a refused name renames nothing"
        );
    }

    fn focus_answer(fixture: &Fixture, apps: &[&str]) {
        let answer = json!({"session_id": "session:api", "window_id": "window:api", "apps": apps});
        fixture.fake.answer("focus", &answer.to_string());
    }

    #[test]
    fn focus_shows_the_agent_in_the_app_and_marks_it_seen() {
        let mut data = snapshot();
        data["sessions"][1]["blocks"]["block:codex"]["status"]["records"] =
            json!([record("done", "codex", 60)]);
        let fixture = Fixture::new(&data);
        focus_answer(&fixture, &["client:mac"]);
        fixture.fake.answer("session.select", "");
        assert_eq!(
            fixture.run(&["get", "block:codex"]).unwrap()["agent"]["agent_status"],
            "done"
        );
        let reply = fixture.run(&["focus", "block:codex"]).unwrap();
        assert_eq!(
            (&reply["type"], &reply["agent"]["agent_status"]),
            (&json!("agent_info"), &json!("idle")),
            "a finished agent the user has seen is idle"
        );
        assert_eq!(
            script_args(&fixture.fake.calls_to("focus")[0]),
            json!({"kind": "block", "id": "block:codex"})
        );
        assert_eq!(
            argv(&fixture.fake.calls_to("session.select")[0]),
            [
                "-C",
                "client:mac",
                "do",
                "session.select",
                "session_id=session:api",
                "window_id=window:api"
            ]
        );
        assert_eq!(
            fixture.run(&["get", "block:codex"]).unwrap()["agent"]["agent_status"],
            "idle",
            "it stays seen"
        );
    }

    #[test]
    fn focus_fails_when_no_app_can_show_the_agent() {
        let fixture = Fixture::new(&snapshot());
        fixture
            .fake
            .answer_with("session.select", "", "403 Remote Control is off", 1);
        for apps in [
            vec![],
            vec!["client:mac", "client:ipad"],
            vec!["client:mac"],
        ] {
            focus_answer(&fixture, &apps);
            assert_eq!(
                fixture.run(&["focus", "reviewer"]).unwrap_err().code,
                "focus_failed",
                "{apps:?}"
            );
        }
        let refused = fixture.run(&["focus", "reviewer"]).unwrap_err();
        assert!(
            refused.message.contains("Remote Control"),
            "{}",
            refused.message
        );
        assert!(
            !fixture.state_dir().join("mru.json").exists(),
            "an agent not shown is not seen"
        );
    }

    #[test]
    fn start_checks_its_arguments_before_asking_rex_anything() {
        let fixture = Fixture::new(&snapshot());
        let start = |name: &str, kind: &str, extra: &[&str]| {
            let args = [
                &["start", name, "--kind", kind, "--pane", "block:shell"][..],
                extra,
            ]
            .concat();
            fixture.run(&args).unwrap_err().code
        };
        assert_eq!(start("Coder", "claude", &[]), "invalid_agent_name");
        assert_eq!(
            start("coder", "gemini", &[]),
            "unsupported_agent_kind",
            "Gemini cannot say it is ready"
        );
        assert_eq!(start("coder", "vim", &[]), "unsupported_agent_kind");
        assert_eq!(
            start("coder", "claude", &["--timeout", "3000"]),
            "invalid_agent_timeout"
        );
        assert_eq!(
            start("coder", "claude", &["--timeout", "300001"]),
            "invalid_agent_timeout"
        );
        assert_eq!(
            start("coder", "claude", &["--", "--model", "a\nb"]),
            "invalid_agent_argument"
        );
        assert_eq!(
            start("coder", "claude", &["--", "\u{1b}[31m"]),
            "invalid_agent_argument"
        );
        assert!(fixture.fake.calls().is_empty());
    }

    #[test]
    fn start_needs_a_free_name_and_a_block_at_a_shell_prompt() {
        let data = with_block(
            snapshot(),
            0,
            "block:server",
            "server",
            info(("node", 7), &[("zsh", 8)], json!({})),
        );
        let fixture = Fixture::new(&data);
        fixture.fake.answer("block", "");
        for (pane, name, code) in [
            ("block:nope", "coder", "agent_pane_not_found"),
            ("block:claude", "coder", "agent_pane_busy"),
            ("block:server", "coder", "agent_pane_busy"),
            ("block:shell", "reviewer", "agent_name_taken"),
        ] {
            let failure = fixture
                .run(&["start", name, "--kind", "claude", "--pane", pane])
                .unwrap_err();
            assert_eq!(failure.code, code, "{pane} {name}: {}", failure.message);
        }
        assert!(
            fixture.fake.calls_to("block").is_empty() && fixture.agent_calls().is_empty(),
            "nothing is renamed or typed"
        );
    }

    #[test]
    fn start_names_the_block_types_the_command_and_waits_until_ready() {
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer("block", "");
        let ready = info(
            ("codex", 70),
            &[("zsh", 4)],
            json!([record("idle", "codex", 70)]),
        );
        fixture.fake.answer("agent", &looked(ready, false));
        let reply = fixture
            .run(&[
                "start",
                "coder",
                "--kind",
                "codex",
                "--pane",
                "block:shell",
                "--",
                "--model",
                "o3 mini",
                "it's",
            ])
            .unwrap();
        assert_eq!(reply["type"], "agent_started");
        assert_eq!(
            reply["argv"],
            json!(["codex", "--model", "o3 mini", "it's"])
        );
        assert_eq!(
            [
                &reply["agent"]["pane_id"],
                &reply["agent"]["name"],
                &reply["agent"]["agent"],
                &reply["agent"]["agent_status"]
            ],
            [
                &json!("block:shell"),
                &json!("coder"),
                &json!("codex"),
                &json!("idle")
            ]
        );
        let keys: Vec<String> = fixture.fake.calls().iter().map(Call::key).collect();
        assert_eq!(
            keys,
            ["snapshot", "block", "agent"],
            "named before the command is typed"
        );
        assert_eq!(
            argv(&fixture.fake.calls_to("block")[0]),
            [
                "block",
                "rename",
                "-s",
                "session:pad",
                "block:shell",
                "coder"
            ]
        );
        assert_eq!(
            fixture.agent_calls()[0]["submit"],
            json!({"text": r"codex --model 'o3 mini' 'it'\''s'", "pid": 4, "at_prompt": true})
        );
    }

    #[test]
    fn a_start_whose_shell_left_its_prompt_is_busy() {
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer("block", "");
        fixture.fake.answer("agent", r#"{"refused": true}"#);
        let failure = fixture
            .run(&[
                "start",
                "coder",
                "--kind",
                "claude",
                "--pane",
                "block:shell",
            ])
            .unwrap_err();
        assert_eq!(failure.code, "agent_pane_busy");
    }

    /// A start of Claude in the shell block that ends with Claude blocked at a
    /// question, while the block's screen is `capture`.
    fn failed_start(capture: Result<&str, &str>) -> (Fixture, Failure) {
        let fixture = Fixture::new(&snapshot());
        fixture.fake.answer("block", "");
        let mut blocked = record("blocked", "claude-code", 70);
        blocked["kind"] = json!("question");
        let look = info(("claude", 70), &[("zsh", 4)], json!([blocked]));
        fixture.fake.answer("agent", &looked(look, false));
        match capture {
            Ok(screen) => fixture.fake.answer("capture", screen),
            Err(error) => fixture.fake.answer_with("capture", "", error, 1),
        }
        let failure = fixture
            .run(&[
                "start",
                "coder",
                "--kind",
                "claude",
                "--pane",
                "block:shell",
            ])
            .unwrap_err();
        (fixture, failure)
    }

    #[test]
    fn a_failed_start_ends_its_message_with_the_end_of_the_blocks_screen() {
        let screen: Vec<String> = (1..=10).map(|n| format!("line {n}   ")).collect();
        let (fixture, failure) = failed_start(Ok(&format!("{}\n\n\n", screen.join("\n\n"))));
        assert_eq!(failure.code, "agent_not_ready");
        let (reason, screen) = failure.message.split_once("\n\n").unwrap();
        assert!(reason.contains("blocked at startup"), "{reason}");
        assert_eq!(
            screen,
            "Its screen ends with:\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10",
            "its last 8 lines with text, without padding"
        );
        assert_eq!(
            argv(&fixture.fake.calls_to("capture")[0]),
            ["capture", "-s", "session:pad", "-b", "block:shell"]
        );
    }

    #[test]
    fn a_failed_start_whose_screen_cannot_be_read_keeps_its_message() {
        let (_, failure) = failed_start(Err("no such block"));
        assert_eq!(failure.code, "agent_not_ready");
        assert!(!failure.message.contains('\n'), "{}", failure.message);
    }

    #[test]
    fn start_commands_quote_each_argument_for_the_shell() {
        for (arg, posix, fish) in [
            ("claude", "claude", "claude"),
            ("--model=opus-4.5", "--model=opus-4.5", "--model=opus-4.5"),
            ("o3 mini", "'o3 mini'", "'o3 mini'"),
            ("it's", r"'it'\''s'", r"'it\'s'"),
            (r"C:\dir\", r"'C:\dir\'", r"'C:\\dir\\'"),
            ("$HOME;`id`|*&", "'$HOME;`id`|*&'", "'$HOME;`id`|*&'"),
            ("", "''", "''"),
        ] {
            assert_eq!(
                (shell_quote(arg, false), shell_quote(arg, true)),
                (posix.to_owned(), fish.to_owned()),
                "{arg}"
            );
        }
    }

    #[test]
    fn input_to_a_block_waits_for_its_last_sender_only() {
        if rerun_alone() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let held = input_lock(dir.path(), "block:a").unwrap();
        let other = File::open(dir.path().join("input/block:a.lock")).unwrap();
        assert!(other.try_lock_exclusive().is_err(), "held by the sender");
        drop(input_lock(dir.path(), "block:b").unwrap());
        drop(held);
        assert!(
            other.try_lock_exclusive().is_ok(),
            "free once the sender is done"
        );
        for id in ["", "../x", "a/b", ".hidden"] {
            assert!(input_lock(dir.path(), id).is_err(), "{id}");
        }
    }

    /// The pids of the shell in the blocks below.
    const SHELL: u32 = 4;
    const CLAUDE: u32 = 30;

    /// The block `reviewer` with `(name, pid)` in front of the shell.
    fn running(name: &str, pid: u32) -> TerminalBlock {
        TerminalBlock {
            session_id: "session:pad".into(),
            session: "pad".into(),
            window_id: "window:main".into(),
            window: "main".into(),
            block_id: "block:claude".into(),
            name: Some("reviewer".into()),
            default_label: "shell".into(),
            title: None,
            cwd: None,
            foreground: Some((name.into(), pid)),
            pids: if pid == SHELL {
                vec![SHELL]
            } else {
                vec![pid, SHELL]
            },
            agent: None,
        }
    }

    /// `block` hosting `agent` in `status`, from its live report when `reported`.
    fn hosting(
        mut block: TerminalBlock,
        agent: &'static str,
        status: AgentStatus,
        reported: bool,
    ) -> TerminalBlock {
        let pid = block.pids[0];
        block.agent = Some(AgentState {
            agent,
            status,
            record: reported.then_some(LiveRecord {
                owner_pid: pid,
                updated_ms: None,
                clears_when_seen: false,
            }),
            pid: Some(pid),
            message: None,
            blocked_kind: None,
        });
        block
    }

    fn look(block: TerminalBlock, active: bool) -> Option<Look> {
        Some(Look { block, active })
    }

    fn claude(status: AgentStatus, active: bool) -> Option<Look> {
        look(
            hosting(running("claude", CLAUDE), "claude", status, true),
            active,
        )
    }

    type Observe<'a> = &'a mut dyn FnMut(Duration) -> Result<Option<Look>, Failure>;

    /// Runs `waiting` against `looks`, one per look and then the last one again,
    /// each taking the whole wait it is given. Returns the outcome, each look's
    /// wait, and the time it all took.
    fn play(
        looks: &[Option<Look>],
        waiting: impl FnOnce(Observe, &dyn Fn() -> Duration) -> Result<Look, Failure>,
    ) -> (Result<Look, Failure>, Vec<Duration>, Duration) {
        let now = Cell::new(Duration::ZERO);
        let mut waits = Vec::new();
        let mut observe = |wait: Duration| {
            waits.push(wait);
            now.set(now.get() + wait);
            Ok(looks[(waits.len() - 1).min(looks.len() - 1)].clone())
        };
        let outcome = waiting(&mut observe, &|| now.get());
        (outcome, waits, now.get())
    }

    fn goal(until: &[AgentStatus], needs_activity: bool, timeout_ms: Option<u64>) -> Goal {
        Goal {
            until: until.to_vec(),
            needs_activity,
            timeout: timeout_ms.map(Duration::from_millis),
        }
    }

    fn wait_for(
        looks: &[Option<Look>],
        goal: &Goal,
    ) -> (Result<Look, Failure>, Vec<Duration>, Duration) {
        play(looks, |observe, elapsed| {
            wait_until(observe, elapsed, CLAUDE, goal, "reviewer")
        })
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn a_prompted_agent_must_work_before_a_settled_state_counts() {
        let looks = [
            claude(Idle, false),
            claude(Working, false),
            claude(Done, false),
        ];
        let (outcome, waits, _) = wait_for(&looks, &goal(&SETTLED, true, None));
        assert_eq!(outcome.unwrap().status(), Done);
        assert_eq!(waits.len(), 3, "the idle before it worked does not count");
        let (outcome, waits, _) = wait_for(&[claude(Blocked, false)], &goal(&SETTLED, true, None));
        assert_eq!(
            (outcome.unwrap().status(), waits.len()),
            (Blocked, 1),
            "blocking is a reaction too"
        );
    }

    #[test]
    fn work_reported_during_a_look_counts_even_when_it_is_over() {
        let (outcome, waits, _) = wait_for(&[claude(Idle, true)], &goal(&SETTLED, true, None));
        assert_eq!((outcome.unwrap().status(), waits.len()), (Idle, 1));
    }

    #[test]
    fn a_prompt_that_never_sets_the_agent_working_stalls_after_five_seconds() {
        let (outcome, waits, took) = wait_for(&[claude(Idle, false)], &goal(&SETTLED, true, None));
        assert_eq!(outcome.unwrap_err().code, "agent_prompt_stalled");
        assert_eq!((waits, took), (vec![POLL; 5], STALL));
        let (outcome, _, took) =
            wait_for(&[claude(Idle, false)], &goal(&SETTLED, true, Some(60_000)));
        assert_eq!(
            (outcome.unwrap_err().code, took),
            ("agent_prompt_stalled", STALL)
        );
    }

    #[test]
    fn the_callers_timeout_ends_a_wait_first_and_bounds_every_look() {
        let (outcome, waits, took) =
            wait_for(&[claude(Idle, false)], &goal(&SETTLED, true, Some(2_500)));
        assert_eq!(outcome.unwrap_err().code, "timeout");
        assert_eq!(
            (waits, took),
            (vec![secs(1.0), secs(1.0), secs(0.5)], secs(2.5))
        );
        let (outcome, _, _) = wait_for(&[claude(Idle, false)], &goal(&SETTLED, true, Some(5_000)));
        assert_eq!(
            outcome.unwrap_err().code,
            "timeout",
            "at the stall limit, the timeout wins"
        );
        let (outcome, waits, _) = wait_for(&[claude(Idle, false)], &goal(&SETTLED, true, Some(0)));
        assert_eq!(
            (outcome.unwrap_err().code, waits.len()),
            ("timeout", 0),
            "nothing is looked at"
        );
        let (outcome, _, took) = wait_for(
            &[claude(Working, false)],
            &goal(&SETTLED, false, Some(4_200)),
        );
        assert_eq!((outcome.unwrap_err().code, took), ("timeout", secs(4.2)));
    }

    #[test]
    fn without_a_timeout_a_working_agent_is_waited_for_until_it_settles() {
        let mut looks = vec![claude(Working, false); 600];
        looks.push(claude(Idle, false));
        let (outcome, _, took) = wait_for(&looks, &goal(&SETTLED, true, None));
        assert_eq!(
            (outcome.unwrap().status(), took),
            (Idle, Duration::from_secs(601))
        );
    }

    #[test]
    fn a_wait_ends_on_the_requested_states_only() {
        let looks = [
            claude(Working, false),
            claude(Idle, false),
            claude(Blocked, false),
        ];
        let (outcome, waits, _) = wait_for(&looks, &goal(&[Blocked], false, None));
        assert_eq!((outcome.unwrap().status(), waits.len()), (Blocked, 3));
        let (outcome, waits, _) = wait_for(&[claude(Idle, false)], &goal(&SETTLED, false, None));
        assert_eq!(
            (outcome.unwrap().status(), waits.len()),
            (Idle, 1),
            "a wait needs no work first"
        );
        let (outcome, _, _) = wait_for(
            &[claude(Done, false)],
            &goal(&[Unknown], false, Some(3_000)),
        );
        assert_eq!(outcome.unwrap_err().code, "timeout");
    }

    #[test]
    fn an_agent_that_left_its_block_ends_the_wait() {
        let replaced = hosting(running("claude", 31), "claude", Working, true);
        for looks in [
            vec![claude(Working, false), None],
            vec![look(running("zsh", SHELL), false)],
            vec![look(replaced, false)],
        ] {
            let (outcome, _, _) = wait_for(&looks, &goal(&SETTLED, false, None));
            assert_eq!(outcome.unwrap_err().code, "agent_not_running");
        }
        let tool = TerminalBlock {
            foreground: Some(("cargo".into(), 80)),
            pids: vec![80, CLAUDE, SHELL],
            ..running("claude", CLAUDE)
        };
        let (outcome, waits, _) = wait_for(
            &[look(tool, false), claude(Idle, false)],
            &goal(&SETTLED, false, None),
        );
        assert_eq!(
            (outcome.unwrap().status(), waits.len()),
            (Idle, 2),
            "an agent running a program in front of it still runs"
        );
    }

    fn ready_for(
        looks: &[Option<Look>],
        timeout_ms: u64,
    ) -> (Result<Look, Failure>, Vec<Duration>, Duration) {
        play(looks, |observe, elapsed| {
            wait_ready(
                observe,
                elapsed,
                "claude",
                SHELL,
                Duration::from_millis(timeout_ms),
            )
        })
    }

    fn at_prompt() -> Option<Look> {
        look(running("zsh", SHELL), false)
    }

    fn starting() -> Option<Look> {
        look(
            hosting(running("claude", CLAUDE), "claude", Unknown, false),
            false,
        )
    }

    #[test]
    fn a_started_agent_is_ready_once_it_reports_idle() {
        let (outcome, waits, _) =
            ready_for(&[at_prompt(), starting(), claude(Idle, false)], 30_000);
        assert_eq!((outcome.unwrap().status(), waits.len()), (Idle, 3));
        let (outcome, _, _) = ready_for(&[starting(), claude(Done, false)], 30_000);
        assert_eq!(outcome.unwrap().status(), Done);
        let (_, waits, _) = ready_for(&[at_prompt(), claude(Idle, false)], 4_500);
        assert!(waits.iter().all(|wait| *wait <= POLL));
    }

    #[test]
    fn an_agent_blocked_at_startup_is_not_ready() {
        let (outcome, _, _) = ready_for(&[at_prompt(), claude(Blocked, false)], 30_000);
        assert_eq!(outcome.unwrap_err().code, "agent_not_ready");
    }

    #[test]
    fn a_silent_agent_is_not_ready_by_the_deadline_and_one_never_started_times_out() {
        let (outcome, _, took) = ready_for(&[at_prompt(), starting()], 5_000);
        assert_eq!(
            (outcome.unwrap_err().code, took),
            ("agent_not_ready", secs(5.0))
        );
        let (outcome, waits, took) = ready_for(&[at_prompt()], 4_500);
        assert_eq!(outcome.unwrap_err().code, "timeout");
        assert_eq!((waits.last().copied(), took), (Some(secs(0.5)), secs(4.5)));
    }

    #[test]
    fn an_agent_that_exits_or_whose_block_closes_during_startup_is_not_running() {
        for looks in [
            vec![at_prompt(), starting(), at_prompt()],
            vec![at_prompt(), None],
        ] {
            let (outcome, _, _) = ready_for(&looks, 30_000);
            assert_eq!(outcome.unwrap_err().code, "agent_not_running");
        }
    }

    #[test]
    fn another_agent_starting_is_a_kind_mismatch() {
        let codex = look(hosting(running("codex", 70), "codex", Idle, true), false);
        let (outcome, _, _) = ready_for(&[at_prompt(), codex], 30_000);
        assert_eq!(outcome.unwrap_err().code, "agent_kind_mismatch");
    }
}
