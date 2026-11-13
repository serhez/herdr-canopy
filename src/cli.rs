use crate::models::{AgentStatus, View};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Attention-aware tree and agent navigation for Herdr and Rex.
#[derive(Debug, Parser)]
#[command(name = "herdr-canopy", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Open the navigator popup in Herdr.
    #[arg(long)]
    pub pane_open: bool,

    /// Choose the workspace tree or flat agent list.
    #[arg(long, value_enum, default_value_t = View::Tree, global = true)]
    pub view: View,

    /// Use sample data without connecting to Herdr.
    #[cfg(feature = "mock")]
    #[arg(long)]
    pub mock: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Toggle this session's navigator popup.
    Toggle,
    /// Record workspace, tab, and pane focus for recent location ordering.
    Track,
    /// Record working periods from Herdr agent lifecycle events.
    TrackWork,
    /// Run Canopy on a Rex server.
    #[command(subcommand)]
    Rex(RexCommand),
}

#[derive(Debug, Subcommand)]
pub enum RexCommand {
    /// Toggle the popup in the origin block's window.
    Toggle(LaunchArgs),
    /// Open the popup in the origin block's window, keeping one already there.
    Open(LaunchArgs),
    /// Run the popup itself (started by toggle/open inside a floating layer).
    Ui(Box<UiArgs>),
    /// Track focus and agent working periods for one Rex server.
    Watch(WatchArgs),
    /// Inspect and drive the agents on a Rex server, as `herdr agent` does.
    Agent(AgentArgs),
}

#[derive(Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
    /// The `rex` binary (default: `$REX_BIN_DIR/rex`, else `rex` on `PATH`).
    #[arg(long, global = true)]
    pub rex: Option<PathBuf>,
}

/// Herdr's agent commands. A TARGET is a block id, an agent's name (its
/// block's label) or SESSION/NAME, where SESSION is a session's label.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// List the agents on the server.
    List,
    /// Show an agent.
    Get { target: String },
    /// Read an agent's terminal output as text.
    Read(ReadArgs),
    /// Send key presses (esc, enter, ctrl+c…); all are checked before any is sent.
    SendKeys {
        target: String,
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
    },
    /// Submit a prompt to an agent. A blocked agent is refused before any input.
    Prompt(PromptArgs),
    /// Rename an agent (its block's label), or restore the default label.
    Rename(RenameArgs),
    /// Show an agent in the Rex app; this marks it seen.
    Focus { target: String },
    /// Wait until an agent reaches one of the requested states.
    Wait(WaitArgs),
    /// Start Claude Code or Codex in a block at a shell prompt, and wait until it is ready.
    Start(StartArgs),
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    pub target: String,
    #[arg(long, value_enum, default_value_t = ReadSource::Recent)]
    pub source: ReadSource,
    /// Keep the last N lines (default: the whole screen for visible, else 200).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub lines: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ReadSource {
    /// The screen.
    Visible,
    /// The screen and scrollback, as rendered.
    Recent,
    /// The screen and scrollback with soft-wrapped lines joined.
    RecentUnwrapped,
}

#[derive(Debug, Args)]
pub struct PromptArgs {
    pub target: String,
    pub text: String,
    /// Wait for a settled state after submitting. From a state other than
    /// working, the agent must work or block within 5 s (else
    /// agent_prompt_stalled).
    #[arg(long)]
    pub wait: bool,
    /// State to match after --wait; repeat for more (default: idle, done, blocked).
    #[arg(long, value_enum, value_name = "STATUS", requires = "wait")]
    pub until: Vec<AgentStatus>,
    /// Fail after this many milliseconds, submission included.
    #[arg(long, value_name = "MS", requires = "wait")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Args)]
#[command(group = clap::ArgGroup::new("label").required(true).args(["name", "clear"]))]
pub struct RenameArgs {
    pub target: String,
    pub name: Option<String>,
    /// Restore the block's default label, so the agent has no name.
    #[arg(long)]
    pub clear: bool,
}

#[derive(Debug, Args)]
pub struct WaitArgs {
    pub target: String,
    /// State to match; repeat for more (default: idle, done, blocked).
    #[arg(long, value_enum, value_name = "STATUS")]
    pub until: Vec<AgentStatus>,
    /// Fail after this many milliseconds (default: wait indefinitely).
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    pub name: String,
    /// The agent: claude or codex, the agents that report when they are ready.
    #[arg(long)]
    pub kind: String,
    /// The block to start it in, at an interactive shell prompt.
    #[arg(long, value_name = "BLOCK")]
    pub pane: String,
    /// How long to wait until it is ready, in milliseconds (3001 to 300000; default 30000).
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,
    /// Arguments for the agent.
    #[arg(last = true, value_name = "AGENT_ARG")]
    pub args: Vec<String>,
}

/// Where a Rex action was triggered and how to reach the server.
#[derive(Debug, Args)]
pub struct LaunchArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub block: String,
    /// The app client that triggered the action.
    #[arg(long)]
    pub client: Option<String>,
    #[command(flatten)]
    pub common: RexArgs,
}

#[derive(Debug, Args)]
pub struct UiArgs {
    #[arg(long)]
    pub host_id: String,
    #[arg(long)]
    pub origin_session: String,
    #[arg(long)]
    pub origin_window: String,
    #[arg(long)]
    pub origin_block: String,
    #[arg(long)]
    pub client: Option<String>,
    /// The server's state directory, as the launcher chose it (default:
    /// `${XDG_STATE_HOME:-~/.local/state}/herdr-canopy/rex-<host-id>`).
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
    /// The launcher's estimate of the window in pixels, as WIDTHxHEIGHT, against
    /// which the popup measures the app's frame.
    #[arg(long, value_parser = parse_dimensions, requires = "share")]
    pub window_px: Option<(f64, f64)>,
    /// The popup layer's share of the window, as WIDTHxHEIGHT fractions.
    #[arg(long, value_parser = parse_dimensions, requires = "window_px")]
    pub share: Option<(f64, f64)>,
    #[command(flatten)]
    pub common: RexArgs,
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Exit if this binary's watcher already runs; replace one from another build.
    #[arg(long)]
    pub ensure: bool,
    /// The `rex` binary (default: `$REX_BIN_DIR/rex`, else `rex` on `PATH`).
    #[arg(long)]
    pub rex: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RexArgs {
    /// The `rex` binary (default: `$REX_BIN_DIR/rex`, else `rex` on `PATH`).
    #[arg(long)]
    pub rex: Option<PathBuf>,
    /// Canopy's config file (default: `${XDG_CONFIG_HOME:-~/.config}/herdr-canopy/config.toml`).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Popup size in cells, as COLUMNSxROWS (default: the view's Herdr size).
    #[arg(long, value_parser = parse_size)]
    pub size: Option<(u16, u16)>,
}

fn parse_size(text: &str) -> Result<(u16, u16), String> {
    let invalid = || format!("expected COLUMNSxROWS such as 65x30, got {text:?}");
    let (columns, rows) = text.split_once('x').ok_or_else(invalid)?;
    let cells = |part: &str| part.parse::<u16>().ok().filter(|n| *n > 0);
    Ok((
        cells(columns).ok_or_else(invalid)?,
        cells(rows).ok_or_else(invalid)?,
    ))
}

fn parse_dimensions(text: &str) -> Result<(f64, f64), String> {
    let invalid = || format!("expected WIDTHxHEIGHT such as 2890x1702.5, got {text:?}");
    let (columns, rows) = text.split_once('x').ok_or_else(invalid)?;
    let cells = |part: &str| {
        part.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    Ok((
        cells(columns).ok_or_else(invalid)?,
        cells(rows).ok_or_else(invalid)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_is_default_and_agents_can_be_opened_or_toggled_explicitly() {
        assert_eq!(
            Cli::try_parse_from(["herdr-canopy"]).unwrap().view,
            View::Tree
        );
        assert!(Cli::try_parse_from(["herdr-canopy", "--pane-open"]).is_ok());
        for args in [
            vec!["herdr-canopy", "--view", "agents"],
            vec!["herdr-canopy", "--pane-open", "--view", "agents"],
            vec!["herdr-canopy", "--view", "agents", "toggle"],
            vec!["herdr-canopy", "toggle", "--view", "agents"],
        ] {
            assert_eq!(Cli::try_parse_from(args).unwrap().view, View::Agents);
        }
        assert!(Cli::try_parse_from(["herdr-canopy", "--view", "unknown"]).is_err());
        assert!(Cli::try_parse_from(["herdr-canopy", "track"]).is_ok());
        assert!(Cli::try_parse_from(["herdr-canopy", "track-work"]).is_ok());
        assert!(Cli::try_parse_from(["herdr-canopy", "toggle"]).is_ok());
    }

    #[test]
    fn rex_commands_take_the_origin_and_optional_settings() {
        let parse = |args: &[&str]| Cli::try_parse_from([&["herdr-canopy", "rex"], args].concat());
        let cli = parse(&[
            "toggle",
            "--view",
            "agents",
            "--session",
            "session:a",
            "--block",
            "block:a1",
            "--client",
            "client:mac",
            "--size",
            "80x20",
        ])
        .unwrap();
        assert_eq!(cli.view, View::Agents);
        let Some(Command::Rex(RexCommand::Toggle(args))) = cli.command else {
            panic!("not a rex toggle");
        };
        assert_eq!(
            (args.session.as_str(), args.block.as_str()),
            ("session:a", "block:a1")
        );
        assert_eq!(args.client.as_deref(), Some("client:mac"));
        assert_eq!(args.common.size, Some((80, 20)));
        assert!(parse(&["open", "--session", "s", "--block", "b"]).is_ok());
        assert!(
            parse(&["open", "--session", "s"]).is_err(),
            "origin block is required"
        );
        for size in [
            "80", "80x", "x20", "0x20", "80x0", "80x20x1", "-1x20", "99999x20",
        ] {
            assert!(
                parse(&["open", "--session", "s", "--block", "b", "--size", size]).is_err(),
                "{size}"
            );
        }
        assert!(parse(&["watch", "--ensure"]).is_ok());
        assert!(
            parse(&[
                "ui",
                "--host-id",
                "h",
                "--origin-session",
                "s",
                "--origin-window",
                "w",
                "--origin-block",
                "b"
            ])
            .is_ok()
        );
        assert!(parse(&["ui", "--host-id", "h"]).is_err());
    }

    fn agent(args: &[&str]) -> Result<AgentCommand, clap::Error> {
        let cli = Cli::try_parse_from([&["herdr-canopy", "rex", "agent"], args].concat())?;
        let Some(Command::Rex(RexCommand::Agent(agent))) = cli.command else {
            panic!("not a rex agent command");
        };
        Ok(agent.command)
    }

    #[test]
    fn agent_commands_take_herdrs_arguments() {
        assert!(matches!(agent(&["list"]), Ok(AgentCommand::List)));
        assert!(
            matches!(agent(&["get", "reviewer"]), Ok(AgentCommand::Get { target }) if target == "reviewer")
        );
        assert!(
            matches!(agent(&["focus", "pad/reviewer"]), Ok(AgentCommand::Focus { target }) if target == "pad/reviewer")
        );
        let Ok(AgentCommand::SendKeys { target, keys }) =
            agent(&["send-keys", "r", "esc", "ctrl+c"])
        else {
            panic!("send-keys");
        };
        assert_eq!(
            (target.as_str(), keys),
            ("r", vec!["esc".to_owned(), "ctrl+c".to_owned()])
        );
        assert!(agent(&["send-keys", "r"]).is_err(), "at least one key");
        assert!(agent(&["get"]).is_err(), "a target is required");
        assert!(
            agent(&["attach", "r"]).is_err(),
            "Herdr-only commands are absent"
        );
    }

    #[test]
    fn agent_reads_default_to_recent_output_and_take_positive_line_counts() {
        let Ok(AgentCommand::Read(read)) = agent(&["read", "r"]) else {
            panic!("read");
        };
        assert_eq!((read.source, read.lines), (ReadSource::Recent, None));
        let Ok(AgentCommand::Read(read)) = agent(&[
            "read",
            "r",
            "--source",
            "recent-unwrapped",
            "--lines",
            "120",
        ]) else {
            panic!("read");
        };
        assert_eq!(
            (read.source, read.lines),
            (ReadSource::RecentUnwrapped, Some(120))
        );
        assert!(agent(&["read", "r", "--source", "visible"]).is_ok());
        for bad in [
            ["--lines", "0"],
            ["--lines", "-3"],
            ["--source", "detection"],
        ] {
            assert!(
                agent(&[&["read", "r"][..], &bad].concat()).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn agent_prompts_take_wait_options_only_with_wait() {
        let Ok(AgentCommand::Prompt(prompt)) = agent(&["prompt", "r", "Review the diff"]) else {
            panic!("prompt");
        };
        assert_eq!(prompt.text, "Review the diff");
        assert!(!prompt.wait && prompt.until.is_empty() && prompt.timeout.is_none());
        let Ok(AgentCommand::Prompt(prompt)) = agent(&[
            "prompt",
            "r",
            "go",
            "--wait",
            "--until",
            "blocked",
            "--until",
            "idle",
            "--timeout",
            "120000",
        ]) else {
            panic!("prompt --wait");
        };
        assert!(prompt.wait);
        assert_eq!(prompt.until, [AgentStatus::Blocked, AgentStatus::Idle]);
        assert_eq!(prompt.timeout, Some(120_000));
        assert!(
            agent(&["prompt", "r", "go", "--until", "idle"]).is_err(),
            "--until requires --wait"
        );
        assert!(
            agent(&["prompt", "r", "go", "--timeout", "100"]).is_err(),
            "--timeout requires --wait"
        );
        assert!(agent(&["prompt", "r", "go", "--wait", "--until", "finished"]).is_err());
        assert!(agent(&["prompt", "r"]).is_err(), "the text is required");
    }

    #[test]
    fn agent_waits_take_any_states_and_an_optional_timeout() {
        let Ok(AgentCommand::Wait(wait)) = agent(&["wait", "r"]) else {
            panic!("wait");
        };
        assert!(wait.until.is_empty() && wait.timeout.is_none());
        let Ok(AgentCommand::Wait(wait)) = agent(&[
            "wait",
            "r",
            "--until",
            "unknown",
            "--until",
            "working",
            "--timeout",
            "5",
        ]) else {
            panic!("wait --until");
        };
        assert_eq!(wait.until, [AgentStatus::Unknown, AgentStatus::Working]);
        assert_eq!(wait.timeout, Some(5));
    }

    #[test]
    fn agent_renames_take_a_name_or_clear_but_not_both() {
        let Ok(AgentCommand::Rename(rename)) = agent(&["rename", "r", "reviewer"]) else {
            panic!("rename");
        };
        assert_eq!(
            (rename.name.as_deref(), rename.clear),
            (Some("reviewer"), false)
        );
        let Ok(AgentCommand::Rename(rename)) = agent(&["rename", "r", "--clear"]) else {
            panic!("rename --clear");
        };
        assert_eq!((rename.name, rename.clear), (None, true));
        assert!(agent(&["rename", "r"]).is_err());
        assert!(agent(&["rename", "r", "reviewer", "--clear"]).is_err());
    }

    #[test]
    fn agent_starts_take_a_kind_a_pane_and_agent_arguments_after_a_separator() {
        let Ok(AgentCommand::Start(start)) = agent(&[
            "start",
            "reviewer",
            "--kind",
            "codex",
            "--pane",
            "block:1",
            "--timeout",
            "60000",
            "--",
            "--model",
            "o3",
            "--",
        ]) else {
            panic!("start");
        };
        assert_eq!(
            (start.name.as_str(), start.kind.as_str()),
            ("reviewer", "codex")
        );
        assert_eq!(
            (start.pane.as_str(), start.timeout),
            ("block:1", Some(60_000))
        );
        assert_eq!(start.args, ["--model", "o3", "--"]);
        let Ok(AgentCommand::Start(start)) =
            agent(&["start", "r", "--kind", "claude", "--pane", "b"])
        else {
            panic!("start without agent arguments");
        };
        assert!(start.args.is_empty() && start.timeout.is_none());
        assert!(
            agent(&["start", "r", "--pane", "b"]).is_err(),
            "--kind is required"
        );
        assert!(
            agent(&["start", "r", "--kind", "claude"]).is_err(),
            "--pane is required"
        );
    }

    #[test]
    fn the_rex_binary_can_follow_any_agent_command() {
        let cli =
            Cli::try_parse_from(["herdr-canopy", "rex", "agent", "list", "--rex", "/opt/rex"])
                .unwrap();
        let Some(Command::Rex(RexCommand::Agent(agent))) = cli.command else {
            panic!("not a rex agent command");
        };
        assert_eq!(agent.rex, Some(PathBuf::from("/opt/rex")));
    }
}
