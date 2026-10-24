use clap::{Parser, Subcommand};

/// Fuzzy tree navigation for Herdr workspaces, tabs, named panes, and agent panes.
#[derive(Debug, Parser)]
#[command(name = "herdr-canopy", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Open the navigator popup in Herdr.
    #[arg(long)]
    pub pane_open: bool,

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
    fn exposes_a_single_mode_without_view_categories() {
        assert!(Cli::try_parse_from(["herdr-canopy", "--pane-open"]).is_ok());
        assert!(Cli::try_parse_from(["herdr-canopy", "--view", "agents"]).is_err());
        assert!(Cli::try_parse_from(["herdr-canopy", "track"]).is_ok());
        assert!(Cli::try_parse_from(["herdr-canopy", "track-work"]).is_ok());
        assert!(Cli::try_parse_from(["herdr-canopy", "toggle"]).is_ok());
    }
}
