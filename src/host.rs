use crate::history::Tracker;
use crate::models::{ActiveLocation, FocusTarget, Keybindings, Snapshot};
use crate::theme::Palette;
use anyhow::Result;

/// What a focus request left to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusOutcome {
    /// Focus moved; the navigator exits.
    Done,
    /// Focus could not fully move; the navigator stays open showing the notice.
    Stay(String),
}

/// What the navigator needs from a terminal multiplexer.
pub trait Host: Send + Sync {
    /// Topology, names, agent status and known working starts, without the
    /// navigator's own pane.
    fn snapshot(&self) -> Result<Snapshot>;
    /// Where the navigator was opened from.
    fn current(&self, snapshot: &Snapshot) -> ActiveLocation;
    /// Moves focus to `target`, unzoomed.
    fn focus(&self, target: &FocusTarget) -> Result<FocusOutcome>;
    /// Recent-location store for this host.
    fn history(&self) -> &Tracker;
    fn keybindings(&self) -> Result<Keybindings>;
    fn palette(&self) -> Palette;
}
