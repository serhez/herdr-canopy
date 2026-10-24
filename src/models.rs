use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveLocation {
    pub workspace_id: Option<String>,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
}

impl ActiveLocation {
    pub fn complete_from(&mut self, other: &Self) {
        if [
            (&self.workspace_id, &other.workspace_id),
            (&self.tab_id, &other.tab_id),
            (&self.pane_id, &other.pane_id),
        ]
        .iter()
        .any(|(a, b)| a.is_some() && a != b)
        {
            return;
        }
        if self.workspace_id.is_none() {
            self.workspace_id.clone_from(&other.workspace_id);
        }
        if self.tab_id.is_none() {
            self.tab_id.clone_from(&other.tab_id);
        }
        if self.pane_id.is_none() {
            self.pane_id.clone_from(&other.pane_id);
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct History {
    pub workspaces: HashMap<String, u64>,
    pub tabs: HashMap<String, u64>,
    pub panes: HashMap<String, u64>,
}

impl History {
    pub fn timestamp(&self, target: &FocusTarget) -> u64 {
        let (map, id) = match target {
            FocusTarget::Workspace(id) => (&self.workspaces, id),
            FocusTarget::Tab(id) => (&self.tabs, id),
            FocusTarget::Pane(id) => (&self.panes, id),
        };
        map.get(id).copied().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub workspaces: Vec<Workspace>,
}

impl Snapshot {
    pub fn active_location(&self, context: Option<&serde_json::Value>) -> ActiveLocation {
        let mut location = ActiveLocation::default();
        if let Some(value) = context {
            let field = |a: &str, b: &str| {
                value
                    .get(a)
                    .or_else(|| value.get(b))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            };
            location = ActiveLocation {
                workspace_id: field("workspace_id", "focused_workspace_id"),
                tab_id: field("tab_id", "focused_tab_id"),
                pane_id: field("focused_pane_id", "pane_id"),
            };
        }
        if let Some(pane) = self
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .find(|p| location.pane_id.as_ref() == Some(&p.id))
        {
            location
                .workspace_id
                .get_or_insert_with(|| pane.workspace_id.clone());
            location.tab_id.get_or_insert_with(|| pane.tab_id.clone());
        }
        if location == ActiveLocation::default() {
            if let Some(pane) = self
                .workspaces
                .iter()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes)
                .find(|p| p.focused)
            {
                location = ActiveLocation {
                    workspace_id: Some(pane.workspace_id.clone()),
                    tab_id: Some(pane.tab_id.clone()),
                    pane_id: Some(pane.id.clone()),
                };
            } else if let Some(workspace) = self.workspaces.iter().find(|w| w.focused) {
                location.workspace_id = Some(workspace.id.clone());
                location.tab_id.clone_from(&workspace.active_tab_id);
            }
        }
        location
    }

    pub fn exclude_pane(&mut self, pane_id: &str) {
        for workspace in &mut self.workspaces {
            for tab in &mut workspace.tabs {
                tab.panes.retain(|pane| pane.id != pane_id);
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    #[serde(rename = "workspace_id")]
    pub id: String,
    #[serde(rename = "label")]
    pub name: String,
    #[serde(default)]
    pub focused: bool,
    pub active_tab_id: Option<String>,
    pub worktree: Option<Worktree>,
    #[serde(skip)]
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Worktree {
    pub is_linked_worktree: bool,
    pub repo_key: String,
    pub repo_name: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Tab {
    #[serde(rename = "tab_id")]
    pub id: String,
    pub workspace_id: String,
    #[serde(rename = "label", default)]
    pub name: String,
    #[serde(skip)]
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Pane {
    #[serde(rename = "pane_id")]
    pub id: String,
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(rename = "label")]
    pub name: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub terminal_title: Option<String>,
    pub agent: Option<String>,
    pub terminal_id: Option<String>,
    #[serde(skip)]
    pub working_since_ms: Option<u64>,
    #[serde(rename = "agent_status", default)]
    pub status: AgentStatus,
}

impl Pane {
    pub fn display_name(&self) -> Option<String> {
        if let Some(label) = self.name.as_ref().filter(|name| !name.trim().is_empty()) {
            return Some(label.clone());
        }
        let provider = self.agent.as_deref().and_then(AgentProvider::from_name)?;
        if let Some(title) = self
            .terminal_title_stripped
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| {
                self.terminal_title
                    .as_deref()
                    .filter(|title| !title.trim().is_empty())
            })
        {
            return Some(title.trim().to_owned());
        }
        let name = match provider {
            AgentProvider::Codex => "Codex",
            AgentProvider::Claude => "Claude",
            AgentProvider::Other => self.agent.as_deref()?.trim(),
        };
        Some(format!("{name} ({})", self.id))
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProvider {
    Codex,
    Claude,
    Other,
}

impl AgentProvider {
    pub fn supports_timing(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.is_empty() {
            None
        } else if name.eq_ignore_ascii_case("codex") {
            Some(Self::Codex)
        } else if name.eq_ignore_ascii_case("claude") {
            Some(Self::Claude)
        } else {
            Some(Self::Other)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Blocked,
    Working,
    Done,
    Idle,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentActivity {
    pub pane_id: String,
    pub terminal_id: String,
    pub agent: Option<String>,
    #[serde(rename = "agent_status")]
    pub status: AgentStatus,
    #[serde(default)]
    pub state_change_seq: u64,
}

impl AgentActivity {
    pub fn provider(&self) -> Option<AgentProvider> {
        self.agent.as_deref().and_then(AgentProvider::from_name)
    }
}

impl AgentStatus {
    pub fn priority(self) -> u8 {
        match self {
            AgentStatus::Blocked => 4,
            AgentStatus::Working => 3,
            AgentStatus::Done => 2,
            AgentStatus::Idle => 1,
            AgentStatus::Unknown => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FocusTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
}

impl FocusTarget {
    pub fn key(&self) -> String {
        match self {
            Self::Workspace(id) => format!("workspace:{id}"),
            Self::Tab(id) => format!("tab:{id}"),
            Self::Pane(id) => format!("pane:{id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Continue,
    Select,
    Dismiss,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    pub toggle_all: Vec<String>,
    pub move_up: Vec<String>,
    pub move_down: Vec<String>,
    pub expand: Vec<String>,
    pub collapse: Vec<String>,
    pub select: Vec<String>,
    pub dismiss: Vec<String>,
    pub force_quit: Vec<String>,
    pub backspace: Vec<String>,
}

impl Default for Keybindings {
    fn default() -> Self {
        fn keys(values: &[&str]) -> Vec<String> {
            values.iter().map(|s| (*s).into()).collect()
        }
        Self {
            toggle_all: keys(&["Tab"]),
            move_up: keys(&["Up", "C-p", "D-k", "M-k"]),
            move_down: keys(&["Down", "C-n", "D-j", "M-j"]),
            expand: keys(&["Right"]),
            collapse: keys(&["Left"]),
            select: keys(&["Enter"]),
            dismiss: keys(&["Esc"]),
            force_quit: keys(&["C-c"]),
            backspace: keys(&["Backspace"]),
        }
    }
}

impl Keybindings {
    pub fn matches(bindings: &[String], event: KeyEvent) -> bool {
        bindings.iter().any(|binding| {
            parse_key(binding).is_some_and(|parsed| {
                parsed.code == event.code && parsed.modifiers == event.modifiers
            })
        })
    }
}

fn parse_key(text: &str) -> Option<KeyEvent> {
    let mut rest = text;
    let mut modifiers = KeyModifiers::NONE;
    loop {
        let Some((prefix, modifier)) = [
            ("C-", KeyModifiers::CONTROL),
            ("S-", KeyModifiers::SHIFT),
            ("M-", KeyModifiers::ALT),
            ("A-", KeyModifiers::ALT),
            ("D-", KeyModifiers::SUPER),
        ]
        .into_iter()
        .find(|(prefix, _)| rest.starts_with(prefix)) else {
            break;
        };
        modifiers |= modifier;
        rest = rest.strip_prefix(prefix)?;
    }
    parse_key_code(rest, modifiers)
}

fn parse_key_code(rest: &str, modifiers: KeyModifiers) -> Option<KeyEvent> {
    let code = match rest {
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Space" => KeyCode::Char(' '),
        "Tab" if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        _ => {
            let mut chars = rest.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    Some(KeyEvent::new(code, modifiers))
}

#[cfg(test)]
mod location_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_key_bindings_match_super_without_matching_plain_or_alt_keys() {
        let bindings = vec!["D-j".into()];
        assert!(Keybindings::matches(
            &bindings,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SUPER)
        ));
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ] {
            assert!(!Keybindings::matches(
                &bindings,
                KeyEvent::new(KeyCode::Char('j'), modifiers)
            ));
        }
        assert_eq!(
            parse_key("C-D-k"),
            Some(KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL | KeyModifiers::SUPER
            ))
        );
        assert_eq!(
            parse_key("D-C-k"),
            Some(KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL | KeyModifiers::SUPER
            ))
        );
    }

    #[test]
    fn invocation_context_wins_over_another_clients_focus() {
        let mut data = crate::test_helpers::snapshot();
        data.workspaces[1].tabs[0].panes[0].focused = true;
        let context =
            json!({"workspace_id":"project","tab_id":"editor","focused_pane_id":"named-notes"});
        let location = data.active_location(Some(&context));
        assert_eq!(location.workspace_id.as_deref(), Some("project"));
        assert_eq!(location.tab_id.as_deref(), Some("editor"));
        assert_eq!(location.pane_id.as_deref(), Some("named-notes"));
    }

    #[test]
    fn missing_tab_and_workspace_are_derived_from_the_invoking_pane() {
        let data = crate::test_helpers::snapshot();
        let location = data.active_location(Some(&json!({"focused_pane_id":"named-notes"})));
        assert_eq!(location.workspace_id.as_deref(), Some("project"));
        assert_eq!(location.tab_id.as_deref(), Some("editor"));
    }

    #[test]
    fn direct_launch_uses_focused_pane_when_context_is_absent() {
        let mut data = crate::test_helpers::snapshot();
        data.workspaces[0].tabs[0].panes[0].focused = true;
        let location = data.active_location(None);
        assert_eq!(location.pane_id.as_deref(), Some("named-notes"));
        assert_eq!(location.tab_id.as_deref(), Some("editor"));
    }
}
