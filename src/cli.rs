use crate::models::View;
use clap::{Parser, Subcommand};

/// Attention-aware tree and agent navigation for Herdr.
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
}
