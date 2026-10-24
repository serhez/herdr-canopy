use crate::models::{
    ActiveLocation, AgentProvider, AgentStatus, FocusTarget, History, KeyAction, Keybindings,
    Snapshot, Workspace,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Workspace,
    Worktree,
    Tab,
    Pane,
}

impl RowKind {
    fn is_workspace(self) -> bool {
        matches!(self, Self::Workspace | Self::Worktree)
    }
}

#[derive(Clone)]
struct Node {
    key: String,
    name: String,
    target: Option<FocusTarget>,
    agents: Vec<AgentStatus>,
    provider: Option<AgentProvider>,
    working_since_ms: Option<u64>,
    kind: RowKind,
    children: Vec<Node>,
    timestamp: u64,
    rank: (u8, u64),
}

pub struct Row {
    pub key: String,
    pub name: String,
    pub target: Option<FocusTarget>,
    pub agents: Vec<AgentStatus>,
    pub provider: Option<AgentProvider>,
    pub working_since_ms: Option<u64>,
    pub kind: RowKind,
    pub depth: usize,
    pub prefix: String,
    pub has_children: bool,
    pub expanded: bool,
    pub matches: Vec<usize>,
    pub score: Option<u32>,
    timestamp: u64,
    parent: Option<usize>,
}

pub struct AppState {
    roots: Vec<Node>,
    collapsed: HashSet<String>,
    keybindings: Keybindings,
    pub query: String,
    pub selected: usize,
    pub rows: Vec<Row>,
    pub error: Option<String>,
    current: ActiveLocation,
    pub show_all: bool,
    preferred_key: Option<String>,
}

impl AppState {
    pub fn new(
        snapshot: Snapshot,
        history: &History,
        current: ActiveLocation,
        keybindings: Keybindings,
    ) -> Self {
        let roots = build_tree(snapshot, history, &current);
        let preferred_key = preferred_key(&roots);
        let mut state = Self {
            roots,
            collapsed: HashSet::new(),
            keybindings,
            query: String::new(),
            selected: 0,
            rows: Vec::new(),
            error: None,
            current,
            show_all: false,
            preferred_key,
        };
        state.rebuild();
        state.select_initial();
        state
    }

    fn rebuild(&mut self) {
        let query = self.query.trim();
        let pattern = (!query.is_empty())
            .then(|| Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart));
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut filtered: Vec<Node> = self
            .roots
            .iter()
            .filter_map(|node| {
                filter_node(
                    node,
                    pattern.as_ref(),
                    &mut matcher,
                    self.show_all,
                    self.preferred_key.as_deref(),
                    false,
                )
            })
            .collect();
        filtered.sort_by(|a, b| b.rank.cmp(&a.rank));
        self.rows.clear();
        flatten(
            &filtered,
            "",
            0,
            None,
            &self.collapsed,
            pattern.as_ref(),
            &mut matcher,
            &mut self.rows,
        );
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.into();
        self.selected = 0;
        self.rebuild();
        self.select_initial();
    }

    pub fn update_snapshot(&mut self, snapshot: Snapshot, history: &History) {
        let key = self.rows.get(self.selected).map(|r| r.key.clone());
        self.roots = build_tree(snapshot, history, &self.current);
        self.preferred_key = preferred_key(&self.roots);
        self.rebuild();
        if let Some(index) = self.rows.iter().position(|r| Some(&r.key) == key.as_ref()) {
            self.selected = index;
        } else {
            self.select_initial();
        }
    }

    pub fn toggle_all(&mut self) {
        let key = self.rows.get(self.selected).map(|row| row.key.clone());
        self.show_all = !self.show_all;
        self.rebuild();
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| Some(&row.key) == key.as_ref())
        {
            self.selected = index;
        } else {
            self.select_initial();
        }
    }

    fn select_initial(&mut self) {
        if !self.query.trim().is_empty() {
            self.selected = self
                .rows
                .iter()
                .position(|row| row.kind == RowKind::Pane && row.target.is_some())
                .or_else(|| self.rows.iter().position(|row| row.kind == RowKind::Pane))
                .or_else(|| {
                    self.rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| {
                            row.score.map(|score| (i, row.target.is_some(), score))
                        })
                        .max_by(|a, b| (a.1, a.2).cmp(&(b.1, b.2)).then_with(|| b.0.cmp(&a.0)))
                        .map(|(i, _, _)| i)
                })
                .unwrap_or(0);
            return;
        }
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| Some(&row.key) == self.preferred_key.as_ref() && row.target.is_some())
        {
            self.selected = index;
            return;
        }
        if let Some((index, _)) = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind == RowKind::Pane && row.target.is_some())
            .max_by(|(ia, a), (ib, b)| {
                (pane_priority(&a.agents), a.timestamp)
                    .cmp(&(pane_priority(&b.agents), b.timestamp))
                    .then_with(|| ib.cmp(ia))
            })
        {
            self.selected = index;
            return;
        }
        fn specificity(target: &FocusTarget) -> u8 {
            match target {
                FocusTarget::Pane(_) => 3,
                FocusTarget::Tab(_) => 2,
                FocusTarget::Workspace(_) => 1,
            }
        }
        let has_recent_location = self.rows.iter().any(|row| {
            row.timestamp > 0
                && matches!(row.target, Some(FocusTarget::Pane(_) | FocusTarget::Tab(_)))
        });
        self.selected = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                row.target
                    .as_ref()
                    .map(|target| (i, row.timestamp, specificity(target)))
            })
            .filter(|(_, timestamp, _)| *timestamp > 0)
            .filter(|(_, _, specificity)| !has_recent_location || *specificity > 1)
            .max_by(|a, b| (a.1, a.2).cmp(&(b.1, b.2)).then_with(|| b.0.cmp(&a.0)))
            .map(|(i, _, _)| i)
            .or_else(|| self.rows.iter().position(|row| row.target.is_some()))
            .unwrap_or(0);
    }

    pub fn selected_target(&self) -> Option<FocusTarget> {
        self.rows.get(self.selected).and_then(|r| r.target.clone())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> KeyAction {
        let bindings = &self.keybindings;
        if Keybindings::matches(&bindings.force_quit, key) {
            return KeyAction::Dismiss;
        }
        if Keybindings::matches(&bindings.dismiss, key) {
            if self.query.is_empty() {
                return KeyAction::Dismiss;
            }
            self.set_query("");
        } else if Keybindings::matches(&bindings.select, key) {
            if self.selected_target().is_some() {
                return KeyAction::Select;
            }
            self.expand();
        } else if Keybindings::matches(&bindings.toggle_all, key) {
            self.toggle_all();
        } else if Keybindings::matches(&bindings.move_up, key) {
            if !self.rows.is_empty() {
                self.selected = (self.selected + self.rows.len() - 1) % self.rows.len();
            }
        } else if Keybindings::matches(&bindings.move_down, key) {
            if !self.rows.is_empty() {
                self.selected = (self.selected + 1) % self.rows.len();
            }
        } else if Keybindings::matches(&bindings.expand, key) {
            self.expand();
        } else if Keybindings::matches(&bindings.collapse, key) {
            self.collapse();
        } else if Keybindings::matches(&bindings.backspace, key) {
            let mut query = self.query.clone();
            query.pop();
            self.set_query(&query);
        } else if let KeyCode::Char(c) = key.code
            && matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
        {
            let mut query = self.query.clone();
            query.push(c);
            self.set_query(&query);
        }
        KeyAction::Continue
    }

    fn expand(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if !row.has_children {
            return;
        }
        if row.expanded {
            self.selected += 1;
        } else {
            self.collapsed.remove(&row.key);
            self.rebuild();
        }
    }

    fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if self.query.trim().is_empty() && row.has_children && row.expanded {
            self.collapsed.insert(row.key.clone());
            self.rebuild();
        } else if let Some(parent) = row.parent {
            self.selected = parent;
        }
    }
}

fn pane_priority(agents: &[AgentStatus]) -> u8 {
    match agents.first() {
        Some(AgentStatus::Blocked) => 4,
        Some(AgentStatus::Done) => 3,
        Some(AgentStatus::Working) => 2,
        _ => 1,
    }
}

fn finish_node(node: &mut Node) {
    let own_rank = (
        if node.kind == RowKind::Pane && node.target.is_some() {
            pane_priority(&node.agents)
        } else {
            0
        },
        node.timestamp,
    );
    node.children.sort_by(|a, b| b.rank.cmp(&a.rank));
    node.agents
        .sort_by_key(|status| std::cmp::Reverse(status.priority()));
    node.rank = node
        .children
        .iter()
        .map(|n| n.rank)
        .max()
        .unwrap_or(own_rank)
        .max(own_rank);
}

fn workspace_node(workspace: Workspace, history: &History, current: &ActiveLocation) -> Node {
    let target = FocusTarget::Workspace(workspace.id.clone());
    let is_current = current.workspace_id.as_ref() == Some(&workspace.id);
    let timestamp = if is_current {
        0
    } else {
        history.timestamp(&target)
    };
    let mut node = Node {
        key: target.key(),
        name: workspace.name,
        target: (!is_current).then_some(target),
        agents: Vec::new(),
        provider: None,
        working_since_ms: None,
        kind: if workspace
            .worktree
            .as_ref()
            .is_some_and(|t| t.is_linked_worktree)
        {
            RowKind::Worktree
        } else {
            RowKind::Workspace
        },
        children: Vec::new(),
        timestamp,
        rank: (0, timestamp),
    };
    for tab in workspace.tabs {
        let target = FocusTarget::Tab(tab.id.clone());
        let is_current = current.tab_id.as_ref() == Some(&tab.id);
        let timestamp = if is_current {
            0
        } else {
            history.timestamp(&target).max(
                tab.panes
                    .iter()
                    .filter_map(|p| history.panes.get(&p.id))
                    .copied()
                    .max()
                    .unwrap_or(0),
            )
        };
        let mut child = Node {
            key: target.key(),
            name: tab.name,
            target: (!is_current).then_some(target),
            agents: Vec::new(),
            provider: None,
            working_since_ms: None,
            kind: RowKind::Tab,
            children: Vec::new(),
            timestamp,
            rank: (0, timestamp),
        };
        for pane in tab.panes {
            let provider = pane.agent.as_deref().and_then(AgentProvider::from_name);
            let agents = if provider.is_some() {
                vec![pane.status]
            } else {
                Vec::new()
            };
            child.agents.extend_from_slice(&agents);
            let is_current = current.pane_id.as_ref() == Some(&pane.id);
            if is_current && provider.is_none() {
                continue;
            }
            if let Some(name) = pane.display_name() {
                let target = FocusTarget::Pane(pane.id);
                let timestamp = if is_current {
                    0
                } else {
                    history.timestamp(&target)
                };
                let rank = (
                    if is_current {
                        0
                    } else {
                        pane_priority(&agents)
                    },
                    timestamp,
                );
                child.children.push(Node {
                    key: target.key(),
                    name,
                    target: (!is_current).then_some(target),
                    agents,
                    provider,
                    working_since_ms: if pane.status == AgentStatus::Working
                        && provider.is_some_and(AgentProvider::supports_timing)
                    {
                        pane.working_since_ms
                    } else {
                        None
                    },
                    kind: RowKind::Pane,
                    children: Vec::new(),
                    timestamp,
                    rank,
                });
            }
        }
        finish_node(&mut child);
        node.agents.extend_from_slice(&child.agents);
        if child.target.is_some() || !child.children.is_empty() {
            node.children.push(child);
        }
    }
    finish_node(&mut node);
    node
}

fn build_tree(snapshot: Snapshot, history: &History, current: &ActiveLocation) -> Vec<Node> {
    let main_ids: HashMap<_, _> = snapshot
        .workspaces
        .iter()
        .filter_map(|w| {
            w.worktree
                .as_ref()
                .filter(|t| !t.is_linked_worktree)
                .map(|t| (t.repo_key.clone(), w.id.clone()))
        })
        .collect();
    let mut roots: Vec<(Node, usize)> = Vec::new();
    let mut linked = Vec::new();
    for (order, workspace) in snapshot.workspaces.into_iter().enumerate() {
        if let Some(worktree) = workspace.worktree.as_ref().filter(|t| t.is_linked_worktree) {
            let parent = main_ids
                .get(&worktree.repo_key)
                .map(|id| FocusTarget::Workspace(id.clone()).key())
                .unwrap_or_else(|| format!("repo:{}", worktree.repo_key));
            linked.push((
                parent,
                worktree.repo_name.clone(),
                workspace_node(workspace, history, current),
                order,
            ));
        } else {
            roots.push((workspace_node(workspace, history, current), order));
        }
    }
    for (parent, repo_name, child, order) in linked {
        let index = match roots.iter().position(|(node, _)| node.key == parent) {
            Some(index) => index,
            None => {
                let index = roots.len();
                roots.push((
                    Node {
                        key: parent,
                        name: repo_name,
                        target: None,
                        agents: Vec::new(),
                        provider: None,
                        working_since_ms: None,
                        kind: RowKind::Workspace,
                        children: Vec::new(),
                        timestamp: 0,
                        rank: (0, 0),
                    },
                    order,
                ));
                index
            }
        };
        let root = &mut roots[index].0;
        root.agents.extend_from_slice(&child.agents);
        root.children.push(child);
    }
    for (root, _) in &mut roots {
        finish_node(root);
    }
    roots.sort_by(|a, b| b.0.rank.cmp(&a.0.rank).then_with(|| a.1.cmp(&b.1)));
    roots.into_iter().map(|(node, _)| node).collect()
}

fn score(pattern: Option<&Pattern>, node: &Node, matcher: &mut Matcher) -> Option<u32> {
    let mut buffer = Vec::new();
    pattern?.score(Utf32Str::new(&node.name, &mut buffer), matcher)
}

fn preferred_key(nodes: &[Node]) -> Option<String> {
    fn collect<'a>(nodes: &'a [Node], all: &mut Vec<&'a Node>) {
        for node in nodes {
            all.push(node);
            collect(&node.children, all);
        }
    }
    let mut all = Vec::new();
    collect(nodes, &mut all);
    let recent = |node: &&Node| node.timestamp > 0 && node.target.is_some();
    let location = |node: &&Node| matches!(node.kind, RowKind::Tab | RowKind::Pane);
    let choose = |a: &&Node, b: &&Node| {
        (a.timestamp, u8::from(a.kind == RowKind::Pane))
            .cmp(&(b.timestamp, u8::from(b.kind == RowKind::Pane)))
    };
    all.iter()
        .copied()
        .rev()
        .filter(|node| node.kind == RowKind::Pane && node.target.is_some())
        .max_by(|a, b| a.rank.cmp(&b.rank))
        .or_else(|| {
            all.iter()
                .copied()
                .filter(recent)
                .filter(location)
                .max_by(choose)
        })
        .or_else(|| all.iter().copied().filter(recent).max_by(choose))
        .or_else(|| all.iter().copied().find(|node| node.target.is_some()))
        .map(|node| node.key.clone())
}

fn filter_node(
    node: &Node,
    pattern: Option<&Pattern>,
    matcher: &mut Matcher,
    show_all: bool,
    preferred_key: Option<&str>,
    ancestor_matched: bool,
) -> Option<Node> {
    let matched = ancestor_matched || pattern.is_some() && score(pattern, node, matcher).is_some();
    let agent_pane = node.kind == RowKind::Pane && !node.agents.is_empty();
    let children: Vec<Node> = node
        .children
        .iter()
        .filter_map(|child| filter_node(child, pattern, matcher, show_all, preferred_key, matched))
        .collect();
    let include = if pattern.is_some() {
        matched && (node.target.is_some() || node.kind.is_workspace() || agent_pane)
            || !children.is_empty()
    } else {
        show_all
            || agent_pane
            || node.kind.is_workspace()
            || preferred_key == Some(node.key.as_str())
            || !children.is_empty()
    };
    include.then(|| {
        let mut filtered = Node {
            key: node.key.clone(),
            name: node.name.clone(),
            target: node.target.clone(),
            agents: node.agents.clone(),
            provider: node.provider,
            working_since_ms: node.working_since_ms,
            kind: node.kind,
            children,
            timestamp: node.timestamp,
            rank: node.rank,
        };
        finish_node(&mut filtered);
        filtered
    })
}

#[allow(clippy::too_many_arguments)]
fn flatten(
    nodes: &[Node],
    ancestor_prefix: &str,
    depth: usize,
    parent: Option<usize>,
    collapsed: &HashSet<String>,
    pattern: Option<&Pattern>,
    matcher: &mut Matcher,
    rows: &mut Vec<Row>,
) {
    for (i, node) in nodes.iter().enumerate() {
        let last = i == nodes.len() - 1;
        let prefix = if depth == 0 {
            String::new()
        } else {
            format!("{ancestor_prefix}{}", if last { "└─ " } else { "├─ " })
        };
        let mut matches = Vec::new();
        let mut buf = Vec::new();
        let score = pattern
            .and_then(|p| p.indices(Utf32Str::new(&node.name, &mut buf), matcher, &mut matches));
        matches.sort_unstable();
        matches.dedup();
        let expanded =
            !node.children.is_empty() && (pattern.is_some() || !collapsed.contains(&node.key));
        let row_index = rows.len();
        rows.push(Row {
            key: node.key.clone(),
            name: node.name.clone(),
            target: node.target.clone(),
            agents: node.agents.clone(),
            provider: node.provider,
            working_since_ms: node.working_since_ms,
            kind: node.kind,
            depth,
            prefix,
            has_children: !node.children.is_empty(),
            expanded,
            matches: matches.into_iter().map(|n| n as usize).collect(),
            score,
            timestamp: node.timestamp,
            parent,
        });
        if expanded {
            let next_prefix = if depth == 0 {
                String::new()
            } else {
                format!("{ancestor_prefix}{}", if last { "   " } else { "│  " })
            };
            flatten(
                &node.children,
                &next_prefix,
                depth + 1,
                Some(row_index),
                collapsed,
                pattern,
                matcher,
                rows,
            );
        }
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    use crate::models::FocusTarget;
    use crate::test_helpers::snapshot;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::HashMap;

    fn priority_fixture() -> (Snapshot, History) {
        let mut data = snapshot();
        data.workspaces.truncate(1);
        data.workspaces[0].worktree = None;
        data.workspaces[0].tabs.truncate(1);
        let template = data.workspaces[0].tabs[0].panes[0].clone();
        let mut history = History {
            workspaces: HashMap::from([("project".into(), 10_000)]),
            tabs: HashMap::from([("editor".into(), 20_000)]),
            ..History::default()
        };
        data.workspaces[0].tabs[0].panes = [
            ("red-old", AgentStatus::Blocked, 1, true),
            ("red-new", AgentStatus::Blocked, 2, true),
            ("blue-old", AgentStatus::Done, 3, true),
            ("blue-new", AgentStatus::Done, 4, true),
            ("yellow-old", AgentStatus::Working, 5, true),
            ("yellow-new", AgentStatus::Working, 6, true),
            ("plain", AgentStatus::Blocked, 100, false),
            ("idle", AgentStatus::Idle, 90, true),
            ("unknown", AgentStatus::Unknown, 80, true),
        ]
        .into_iter()
        .map(|(id, status, timestamp, agent)| {
            let mut pane = template.clone();
            pane.id = id.into();
            pane.name = Some(id.into());
            pane.agent = agent.then(|| "codex".into());
            pane.status = status;
            history.panes.insert(id.into(), timestamp);
            pane
        })
        .collect();
        (data, history)
    }

    #[test]
    fn enhanced_mru_orders_panes_by_attention_then_recency_and_preselects_the_best() {
        let (data, history) = priority_fixture();
        let app = full_app(
            data,
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        let panes: Vec<_> = app
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Pane)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            panes,
            [
                "red-new",
                "red-old",
                "blue-new",
                "blue-old",
                "yellow-new",
                "yellow-old",
                "plain",
                "idle",
                "unknown"
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("red-new".into()))
        );
    }

    #[test]
    fn enhanced_mru_promotes_attention_across_workspaces_worktrees_and_tabs_without_history() {
        let mut data = snapshot();
        let mut alerts = data.workspaces[0].tabs[0].clone();
        alerts.id = "alerts".into();
        alerts.name = "Alerts".into();
        alerts.panes.truncate(1);
        alerts.panes[0].id = "red-elsewhere".into();
        alerts.panes[0].name = Some("attention".into());
        alerts.panes[0].workspace_id = "other".into();
        alerts.panes[0].tab_id = "alerts".into();
        alerts.panes[0].status = AgentStatus::Blocked;
        data.workspaces[2].tabs.push(alerts);
        let history = History {
            workspaces: HashMap::from([("project".into(), 10_000)]),
            tabs: HashMap::from([("editor".into(), 10_000)]),
            panes: HashMap::from([("named-notes".into(), 1000), ("unnamed".into(), 2000)]),
        };
        let app = AppState::new(
            data.clone(),
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("red-elsewhere".into()))
        );
        assert_eq!(
            names(&app),
            vec![
                ("Other", 0),
                ("Alerts", 1),
                ("attention", 2),
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2)
            ]
        );
        let app = AppState::new(
            data,
            &history,
            ActiveLocation {
                workspace_id: Some("other".into()),
                tab_id: Some("alerts".into()),
                pane_id: Some("red-elsewhere".into()),
            },
            Keybindings::default(),
        );
        assert_eq!(app.rows[0].name, "Project");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        assert!(
            app.rows
                .iter()
                .any(|r| r.name == "attention" && r.target.is_none())
        );
    }

    #[test]
    fn ordinary_pane_recency_crosses_branches_without_parent_timestamps_overriding_it() {
        let mut data = snapshot();
        for pane in data
            .workspaces
            .iter_mut()
            .flat_map(|w| &mut w.tabs)
            .flat_map(|t| &mut t.panes)
        {
            pane.agent = None;
        }
        let history = History {
            workspaces: HashMap::from([("project".into(), 1000)]),
            tabs: HashMap::from([("editor".into(), 1000)]),
            panes: HashMap::from([("named-notes".into(), 1), ("named-review".into(), 2)]),
        };
        let app = full_app(
            data,
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(app.rows[1].name, "feature");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn a_completed_agent_loses_attention_priority_after_herdr_reports_it_idle() {
        let mut data = snapshot();
        let history = History {
            panes: HashMap::from([("named-review".into(), 100)]),
            ..History::default()
        };
        let app = AppState::new(
            data.clone(),
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        data.workspaces[1].tabs[0].panes[0].status = AgentStatus::Idle;
        let app = AppState::new(
            data,
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-notes".into()))
        );
    }

    #[test]
    fn search_selects_the_first_prioritized_pane_instead_of_the_best_fuzzy_name() {
        let (mut data, history) = priority_fixture();
        for pane in &mut data.workspaces[0].tabs[0].panes {
            match pane.id.as_str() {
                "yellow-old" => pane.name = Some("t---a---s---k".into()),
                "plain" => pane.name = Some("task".into()),
                _ => {}
            }
        }
        let mut app = AppState::new(
            data,
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("task");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("yellow-old".into()))
        );
        assert_eq!(app.rows[app.selected].kind, RowKind::Pane);
        assert_eq!(app.rows.last().unwrap().name, "task");
        app.set_query("");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("red-new".into()))
        );
    }

    #[test]
    fn parent_search_reveals_eligible_descendants_and_selects_the_first_pane() {
        let mut data = snapshot();
        let mut shell = data.workspaces[0].tabs[0].panes[0].clone();
        shell.id = "manual-shell".into();
        shell.name = Some("shell".into());
        shell.agent = None;
        data.workspaces[0].tabs[0].panes.push(shell);
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("Project");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2),
                ("shell", 2),
                ("Tests", 1)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        app.set_query("Editor");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2),
                ("shell", 2)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-notes".into()))
        );
        app.set_query("feature");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        assert!(
            !app.rows
                .iter()
                .any(|r| r.name == "Editor" || r.name == "blank")
        );
        app.set_query("Tests");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Tab("tests".into()))
        );
    }

    #[test]
    fn filtered_branch_priority_ignores_nonmatching_agent_panes() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].status = AgentStatus::Blocked;
        data.workspaces[0].tabs[0].panes[1].agent = None;
        data.workspaces[0].tabs[0].panes[1].name = Some("matching plain".into());
        let mut other = data.workspaces[1].tabs[0].clone();
        other.id = "elsewhere".into();
        other.name = "Elsewhere".into();
        other.panes[0].id = "matching-agent".into();
        other.panes[0].name = Some("matching agent".into());
        other.panes[0].workspace_id = "other".into();
        other.panes[0].tab_id = "elsewhere".into();
        data.workspaces[2].tabs.push(other);
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("matching");
        assert_eq!(app.rows[0].name, "Other");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("matching-agent".into()))
        );
        assert!(
            app.rows
                .iter()
                .find(|r| r.name == "Project")
                .unwrap()
                .agents
                .contains(&AgentStatus::Blocked)
        );
        assert!(!app.rows.iter().any(|r| r.name == "notes"));
    }

    #[test]
    fn refresh_preserves_manual_selection_and_reselects_within_search_when_it_disappears() {
        let (mut data, history) = priority_fixture();
        let mut app = AppState::new(
            data.clone(),
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("yellow");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("yellow-new".into()))
        );
        app.selected = app
            .rows
            .iter()
            .position(|r| r.name == "yellow-old")
            .unwrap();
        data.workspaces[0].tabs[0]
            .panes
            .iter_mut()
            .find(|p| p.id == "yellow-new")
            .unwrap()
            .status = AgentStatus::Blocked;
        app.update_snapshot(data.clone(), &history);
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("yellow-old".into()))
        );
        data.workspaces[0].tabs[0]
            .panes
            .retain(|p| p.id != "yellow-old");
        app.update_snapshot(data, &history);
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("yellow-new".into()))
        );
    }

    #[test]
    fn compact_view_includes_all_agent_states_and_providers_with_their_parents() {
        for provider in ["codex", "claude", "other"] {
            for status in [
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown,
            ] {
                let mut data = snapshot();
                data.workspaces[0].tabs[0].panes[0].agent = Some(provider.into());
                data.workspaces[0].tabs[0].panes[0].status = status;
                let app = AppState::new(
                    data,
                    &History::default(),
                    ActiveLocation::default(),
                    Keybindings::default(),
                );
                assert!(!app.show_all);
                assert_eq!(
                    app.rows.iter().find(|r| r.name == "notes").unwrap().agents,
                    vec![status]
                );
                for name in [
                    "Project",
                    "Editor",
                    "hidden-title",
                    "feature",
                    "Review",
                    "reviewer",
                    "Other",
                ] {
                    assert!(app.rows.iter().any(|r| r.name == name), "missing {name}");
                }
                assert!(!app.rows.iter().any(|r| r.name == "Tests"));
            }
        }
    }

    #[test]
    fn agent_pane_names_prefer_manual_labels_then_titles_then_provider_and_id() {
        for (label, clean_title, title, expected) in [
            (Some("manual"), Some("clean"), Some("raw"), "manual"),
            (None, Some("clean"), Some("raw"), "clean"),
            (Some("  "), Some("  "), Some("raw"), "raw"),
            (None, None, None, "Claude (named-notes)"),
        ] {
            let mut data = snapshot();
            let pane = &mut data.workspaces[0].tabs[0].panes[0];
            pane.name = label.map(str::to_owned);
            pane.terminal_title_stripped = clean_title.map(str::to_owned);
            pane.terminal_title = title.map(str::to_owned);
            let app = AppState::new(
                data,
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
            );
            let row = app
                .rows
                .iter()
                .find(|r| r.target == Some(FocusTarget::Pane("named-notes".into())))
                .unwrap();
            assert_eq!(row.name, expected);
        }
    }

    #[test]
    fn refresh_adds_agent_panes_updates_states_and_times_and_removes_non_mru_plain_panes() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].agent = None;
        let mut app = AppState::new(
            data.clone(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert!(!app.rows.iter().any(|r| r.name == "notes"));
        let previous = app.selected_target();
        data.workspaces[0].tabs[0].panes[0].agent = Some("codex".into());
        data.workspaces[0].tabs[0].panes[0].working_since_ms = Some(10_000);
        app.update_snapshot(data.clone(), &History::default());
        let row = app.rows.iter().find(|r| r.name == "notes").unwrap();
        assert_eq!(row.agents, vec![AgentStatus::Working]);
        assert_eq!(row.working_since_ms, Some(10_000));
        assert_eq!(app.selected_target(), previous);
        data.workspaces[0].tabs[0].panes[0].status = AgentStatus::Done;
        app.update_snapshot(data.clone(), &History::default());
        let row = app.rows.iter().find(|r| r.name == "notes").unwrap();
        assert_eq!(row.agents, vec![AgentStatus::Done]);
        assert_eq!(row.working_since_ms, None);
        data.workspaces[0].tabs[0].panes[0].agent = None;
        app.update_snapshot(data, &History::default());
        assert!(!app.rows.iter().any(|r| r.name == "notes"));
        assert_eq!(app.selected_target(), previous);
    }

    #[test]
    fn current_agent_is_context_and_does_not_replace_the_previous_mru_destination() {
        let history = History {
            panes: HashMap::from([("named-notes".into(), 100), ("named-review".into(), 50)]),
            ..History::default()
        };
        let mut app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        let index = app.rows.iter().position(|r| r.name == "notes").unwrap();
        assert!(app.rows[index].target.is_none());
        assert_eq!(app.rows[index].agents, vec![AgentStatus::Working]);
        app.selected = index;
        assert_eq!(key(&mut app, KeyCode::Enter), KeyAction::Continue);
        app.set_query("notes");
        assert!(
            app.rows
                .iter()
                .any(|r| r.name == "notes" && r.target.is_none())
        );
    }

    #[test]
    fn searching_for_the_current_agent_selects_its_context_instead_of_an_unmatched_focusable_parent()
     {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation {
                workspace_id: Some("worktree".into()),
                tab_id: Some("review".into()),
                pane_id: Some("named-review".into()),
            },
            Keybindings::default(),
        );
        app.set_query("reviewer");
        assert_eq!(app.rows[app.selected].name, "reviewer");
        assert!(app.selected_target().is_none());
        assert_eq!(key(&mut app, KeyCode::Enter), KeyAction::Continue);
    }

    #[test]
    fn compact_view_starts_with_workspaces_worktrees_and_agent_panes() {
        let app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2),
                ("Other", 0)
            ]
        );
        assert!(!app.show_all);
    }

    #[test]
    fn compact_view_reveals_the_preselected_mru_pane_and_its_parents() {
        let history = History {
            panes: HashMap::from([("named-review".into(), 10)]),
            ..History::default()
        };
        let app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("hidden-title", 2),
                ("notes", 2),
                ("Other", 0)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn compact_parent_search_reveals_panes_without_switching_to_full_view() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("Editor");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-notes".into()))
        );
        assert!(!app.show_all);
        app.set_query("Project");
        assert!(app.rows.iter().any(|r| r.name == "Tests"));
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        app.set_query("");
        assert!(!app.rows.iter().any(|r| r.name == "Tests"));
    }

    #[test]
    fn pane_search_keeps_only_the_matching_pane_and_its_ancestry() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("notes");
        assert_eq!(
            names(&app),
            vec![("Project", 0), ("Editor", 1), ("notes", 2)]
        );
        app.set_query("");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2),
                ("Other", 0)
            ]
        );
    }

    #[test]
    fn tab_toggles_full_tree_without_losing_the_search_or_mru_selection() {
        let history = History {
            panes: HashMap::from([("named-review".into(), 10)]),
            ..History::default()
        };
        let mut app = AppState::new(
            snapshot(),
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        let previous = app.selected_target();
        key(&mut app, KeyCode::Tab);
        assert!(app.show_all);
        assert_eq!(app.rows.len(), 9);
        assert_eq!(app.selected_target(), previous);
        key(&mut app, KeyCode::Tab);
        assert!(!app.show_all);
        assert_eq!(app.selected_target(), previous);
        app.set_query("reviewer");
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "reviewer");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3)
            ]
        );
    }

    #[test]
    fn unnamed_panes_without_agents_stay_hidden_in_both_views_and_search() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[1].agent = None;
        let mut app = AppState::new(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        for _ in 0..2 {
            for query in ["unnamed", "hidden-title", "blank"] {
                app.set_query(query);
                assert!(app.rows.is_empty());
            }
            app.set_query("");
            assert!(!app.rows.iter().any(
                |r| matches!(&r.target,Some(FocusTarget::Pane(id)) if id=="unnamed" || id=="blank")
            ));
            key(&mut app, KeyCode::Tab);
        }
    }

    #[test]
    fn parent_bubbles_count_all_agents_and_sort_by_state_even_when_children_are_hidden() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[1].status = AgentStatus::Blocked;
        let app = AppState::new(
            data,
            &History::default(),
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        let row = app.rows.iter().find(|r| r.name == "Project").unwrap();
        assert_eq!(
            row.agents,
            vec![
                AgentStatus::Blocked,
                AgentStatus::Working,
                AgentStatus::Done
            ]
        );
        assert!(
            app.rows
                .iter()
                .any(|r| r.name == "notes" && r.target.is_none())
        );
        assert_eq!(
            app.rows
                .iter()
                .find(|r| r.name == "feature")
                .unwrap()
                .agents,
            vec![AgentStatus::Done]
        );
    }

    #[test]
    fn tab_bubbles_include_unnamed_agents_and_named_panes_have_at_most_one_bubble() {
        let mut app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        key(&mut app, KeyCode::Tab);
        assert_eq!(
            app.rows.iter().find(|r| r.name == "Editor").unwrap().agents,
            vec![AgentStatus::Working, AgentStatus::Idle]
        );
        assert_eq!(
            app.rows.iter().find(|r| r.name == "notes").unwrap().agents,
            vec![AgentStatus::Working]
        );
    }

    #[test]
    fn worktree_rows_are_typed_without_changing_searchable_names() {
        let app = AppState::new(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(
            app.rows.iter().find(|r| r.name == "feature").unwrap().kind,
            RowKind::Worktree
        );
        assert_eq!(app.rows[0].kind, RowKind::Workspace);
    }

    fn origin(pane_id: &str, tab_id: &str) -> ActiveLocation {
        ActiveLocation {
            workspace_id: Some("project".into()),
            tab_id: Some(tab_id.into()),
            pane_id: Some(pane_id.into()),
        }
    }

    #[test]
    fn previous_named_pane_is_selected_and_current_agent_search_is_context_only() {
        let history = History {
            panes: HashMap::from([("named-notes".into(), 30), ("named-review".into(), 20)]),
            ..History::default()
        };
        let mut app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        assert!(
            app.rows
                .iter()
                .any(|r| r.name == "notes" && r.target.is_none())
        );
        assert!(
            app.rows
                .iter()
                .any(|r| r.name == "Editor" && r.target.is_none())
        );
        app.set_query("notes");
        assert_eq!(
            names(&app),
            vec![("Project", 0), ("Editor", 1), ("notes", 2)]
        );
        assert!(app.selected_target().is_none());
    }

    #[test]
    fn current_tab_remains_context_for_another_named_pane_and_enter_returns_to_it() {
        let mut data = snapshot();
        data.workspaces.truncate(1);
        for pane in &mut data.workspaces[0].tabs[0].panes {
            pane.status = AgentStatus::Idle;
        }
        data.workspaces[0].tabs[0].panes[1].name = Some("previous pane".into());
        let history = History {
            panes: HashMap::from([("unnamed".into(), 20), ("named-notes".into(), 30)]),
            ..History::default()
        };
        let mut app = AppState::new(
            data,
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        let parent = app.rows.iter().find(|r| r.name == "Editor").unwrap();
        assert!(parent.target.is_none());
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("unnamed".into()))
        );
        assert_eq!(key(&mut app, KeyCode::Enter), KeyAction::Select);
    }

    #[test]
    fn attention_pane_precedes_a_more_recent_tab() {
        let history = History {
            tabs: HashMap::from([("review".into(), 25)]),
            panes: HashMap::from([("named-review".into(), 10)]),
            ..History::default()
        };
        let app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn workspace_parent_timestamp_does_not_override_an_attention_pane() {
        let history = History {
            workspaces: HashMap::from([("worktree".into(), 30)]),
            tabs: HashMap::from([("review".into(), 25)]),
            panes: HashMap::from([("named-review".into(), 20)]),
        };
        let app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn eligible_panes_order_branches_before_parent_recency() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].status = AgentStatus::Idle;
        data.workspaces[0].tabs[0].panes[1].name = Some("recent pane".into());
        let history = History {
            tabs: HashMap::from([("tests".into(), 50), ("editor".into(), 30)]),
            panes: HashMap::from([("unnamed".into(), 20), ("named-notes".into(), 10)]),
            ..History::default()
        };
        let app = full_app(
            data,
            &history,
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert!(
            app.rows.iter().position(|r| r.name == "Tests")
                > app.rows.iter().position(|r| r.name == "Editor")
        );
        assert!(
            app.rows.iter().position(|r| r.name == "recent pane")
                < app.rows.iter().position(|r| r.name == "notes")
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn named_pane_wins_the_tie_with_its_tab_and_workspace() {
        let history = History {
            workspaces: HashMap::from([("worktree".into(), 20)]),
            tabs: HashMap::from([("review".into(), 20)]),
            panes: HashMap::from([("named-review".into(), 20)]),
        };
        let app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn reopening_toggles_between_two_named_panes_in_the_same_tab() {
        let mut data = snapshot();
        data.workspaces.truncate(1);
        for pane in &mut data.workspaces[0].tabs[0].panes {
            pane.status = AgentStatus::Idle;
        }
        data.workspaces[0].tabs[0].panes[1].name = Some("second".into());
        let mut history = History {
            panes: HashMap::from([("named-notes".into(), 30), ("unnamed".into(), 20)]),
            ..History::default()
        };
        let app = AppState::new(
            data.clone(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("unnamed".into()))
        );
        history.panes.insert("unnamed".into(), 40);
        let app = AppState::new(
            data,
            &history,
            origin("unnamed", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-notes".into()))
        );
    }

    #[test]
    fn unnamed_pane_recency_supplies_tab_fallback_when_no_eligible_panes_exist() {
        let mut data = snapshot();
        for pane in data
            .workspaces
            .iter_mut()
            .flat_map(|w| &mut w.tabs)
            .flat_map(|t| &mut t.panes)
        {
            pane.agent = None;
            pane.name = None;
        }
        let history = History {
            panes: HashMap::from([("blank".into(), 20), ("named-review".into(), 10)]),
            ..History::default()
        };
        let app = AppState::new(
            data,
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Tab("tests".into()))
        );
        assert!(
            !app.rows
                .iter()
                .any(|r| r.target == Some(FocusTarget::Pane("blank".into())))
        );
    }

    #[test]
    fn clearing_search_restores_the_previous_destination() {
        let history = History {
            tabs: HashMap::from([("review".into(), 25)]),
            ..History::default()
        };
        let mut app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        app.set_query("tests");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Tab("tests".into()))
        );
        app.set_query("");
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn stale_history_and_the_current_workspace_cannot_steal_initial_selection() {
        let history = History {
            workspaces: HashMap::from([("project".into(), 100)]),
            panes: HashMap::from([("closed-pane".into(), 200), ("named-review".into(), 20)]),
            ..History::default()
        };
        let app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        assert!(
            app.rows
                .iter()
                .find(|r| r.name == "Project")
                .unwrap()
                .target
                .is_none()
        );
    }

    #[test]
    fn refresh_keeps_the_invocation_origin_and_manual_selection() {
        let history = History {
            tabs: HashMap::from([("review".into(), 25)]),
            ..History::default()
        };
        let mut app = AppState::new(
            snapshot(),
            &history,
            origin("named-notes", "editor"),
            Keybindings::default(),
        );
        app.set_query("tests");
        app.update_snapshot(snapshot(), &history);
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Tab("tests".into()))
        );
        assert!(!app.rows.iter().any(|r| r.name == "notes"));
    }

    fn full_app(
        data: Snapshot,
        history: &History,
        current: ActiveLocation,
        keys: Keybindings,
    ) -> AppState {
        let mut state = AppState::new(data, history, current, keys);
        state.toggle_all();
        state
    }

    fn app() -> AppState {
        full_app(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        )
    }

    fn names(app: &AppState) -> Vec<(&str, usize)> {
        app.rows
            .iter()
            .map(|r| (r.name.as_str(), r.depth))
            .collect()
    }

    fn key(app: &mut AppState, code: KeyCode) -> KeyAction {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn full_tree_has_workspaces_tabs_named_panes_and_unnamed_agent_panes() {
        assert_eq!(
            names(&app()),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2),
                ("Tests", 1),
                ("Other", 0),
            ]
        );
    }

    #[test]
    fn worktrees_group_by_repository_identity_even_when_labels_differ() {
        let mut data = snapshot();
        data.workspaces[0].name = "Renamed project".into();
        let app = full_app(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert!(names(&app).contains(&("feature", 1)));
    }

    #[test]
    fn worktree_without_open_main_workspace_keeps_a_repository_parent() {
        let mut data = snapshot();
        data.workspaces.remove(0);
        let app = full_app(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(
            names(&app),
            vec![
                ("project-repo", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Other", 0),
            ]
        );
        assert!(app.rows[0].target.is_none());
        assert!(
            matches!(&app.rows[1].target, Some(FocusTarget::Workspace(id)) if id == "worktree")
        );
    }

    #[test]
    fn recent_worktree_promotes_its_whole_workspace_group() {
        let mut data = snapshot();
        data.workspaces.swap(0, 2);
        let recency = HashMap::from([("worktree".into(), 30), ("other".into(), 20)]);
        let app = full_app(
            data,
            &History {
                workspaces: recency,
                ..History::default()
            },
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(app.rows[0].name, "Project");
        assert_eq!(app.rows.last().unwrap().name, "Other");
    }

    #[test]
    fn pane_search_keeps_ancestors_and_selects_the_matching_pane() {
        let mut app = app();
        app.set_query("rvwr");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn tab_search_keeps_matching_tab_and_ancestors() {
        let mut app = app();
        app.set_query("edtr");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-notes".into()))
        );
    }

    #[test]
    fn workspace_search_reveals_its_panes_and_selects_the_first() {
        let mut app = app();
        app.set_query("PRJCT");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3),
                ("Editor", 1),
                ("notes", 2),
                ("hidden-title", 2),
                ("Tests", 1)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
        assert!(!app.rows.iter().any(|r| r.name == "Other"));
    }

    #[test]
    fn worktree_search_keeps_parent_and_matching_worktree() {
        let mut app = app();
        app.set_query("ftr");
        assert_eq!(
            names(&app),
            vec![
                ("Project", 0),
                ("feature", 1),
                ("Review", 2),
                ("reviewer", 3)
            ]
        );
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-review".into()))
        );
    }

    #[test]
    fn manually_named_panes_only_match_their_manual_names() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].terminal_title = Some("metadata-title".into());
        data.workspaces[0].tabs[0].panes[1].agent = None;
        for query in [
            "metadata-title",
            "hidden-title",
            "claude",
            "unnamed",
            "named-review",
        ] {
            let mut app = full_app(
                data.clone(),
                &History::default(),
                ActiveLocation::default(),
                Keybindings::default(),
            );
            app.set_query(query);
            assert!(app.rows.is_empty(), "{query} must not match metadata");
        }
    }

    #[test]
    fn empty_and_whitespace_pane_labels_are_hidden_but_tabs_remain() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].name = Some("  ".into());
        let app = full_app(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert!(names(&app).contains(&("Editor", 1)));
        assert!(
            !app.rows
                .iter()
                .any(|r| r.name == "notes" || r.name.trim().is_empty())
        );
    }

    #[test]
    fn expand_collapse_and_left_to_parent_preserve_tree_navigation() {
        let mut app = app();
        app.selected = 0;
        key(&mut app, KeyCode::Left);
        assert_eq!(names(&app), vec![("Project", 0), ("Other", 0)]);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.rows.len(), 9);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.rows[app.selected].name, "feature");
        key(&mut app, KeyCode::Left);
        assert!(!app.rows.iter().any(|r| r.name == "reviewer"));
        key(&mut app, KeyCode::Left);
        assert_eq!(app.rows[app.selected].name, "Project");
    }

    #[test]
    fn search_opens_matching_paths_without_losing_collapse_state() {
        let mut app = app();
        app.selected = 0;
        key(&mut app, KeyCode::Left);
        app.set_query("notes");
        assert_eq!(
            names(&app),
            vec![("Project", 0), ("Editor", 1), ("notes", 2)]
        );
        app.set_query("");
        assert_eq!(names(&app), vec![("Project", 0), ("Other", 0)]);
    }

    #[test]
    fn unicode_fuzzy_search_records_character_positions() {
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].name = Some("模型 café".into());
        let mut app = full_app(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        app.set_query("模型");
        let row = app.rows.last().unwrap();
        assert_eq!(row.name, "模型 café");
        assert_eq!(row.matches, vec![0, 1]);
    }

    #[test]
    fn escape_clears_search_then_dismisses_and_digits_are_search_text() {
        let mut app = app();
        assert_eq!(key(&mut app, KeyCode::Char('3')), KeyAction::Continue);
        assert_eq!(app.query, "3");
        assert_eq!(key(&mut app, KeyCode::Esc), KeyAction::Continue);
        assert_eq!(app.query, "");
        assert_eq!(key(&mut app, KeyCode::Esc), KeyAction::Dismiss);
    }

    #[test]
    fn repeated_arrow_events_move_selection() {
        let mut app = app();
        let start = app.selected;
        let mut event = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        event.kind = crossterm::event::KeyEventKind::Repeat;
        app.handle_key(event);
        assert_eq!(app.selected, (start + 1) % app.rows.len());
    }

    #[test]
    fn default_command_and_alt_j_k_match_arrows_and_leave_plain_letters_as_search() {
        let bindings = Keybindings::default();
        for (chord, arrow, modifiers) in [
            ('j', KeyCode::Down, KeyModifiers::SUPER),
            ('k', KeyCode::Up, KeyModifiers::SUPER),
            ('j', KeyCode::Down, KeyModifiers::ALT),
            ('k', KeyCode::Up, KeyModifiers::ALT),
        ] {
            let mut command_app = full_app(
                snapshot(),
                &History::default(),
                ActiveLocation::default(),
                bindings.clone(),
            );
            let mut arrow_app = full_app(
                snapshot(),
                &History::default(),
                ActiveLocation::default(),
                bindings.clone(),
            );
            for kind in [
                crossterm::event::KeyEventKind::Press,
                crossterm::event::KeyEventKind::Repeat,
            ] {
                let mut event = KeyEvent::new(KeyCode::Char(chord), modifiers);
                event.kind = kind;
                assert_eq!(command_app.handle_key(event), KeyAction::Continue);
                key(&mut arrow_app, arrow);
                assert_eq!(command_app.selected, arrow_app.selected);
                assert!(command_app.query.is_empty());
            }
        }
        let mut app = full_app(
            snapshot(),
            &History::default(),
            ActiveLocation::default(),
            bindings,
        );
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.query, "j");
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.query, "jk");
    }

    #[test]
    fn distinct_repositories_with_the_same_name_keep_separate_worktree_groups() {
        let mut data = snapshot();
        let mut other_worktree = data.workspaces[1].clone();
        other_worktree.id = "different-worktree".into();
        other_worktree.name = "different-feature".into();
        other_worktree.worktree.as_mut().unwrap().repo_key = "different-repo".into();
        data.workspaces.push(other_worktree);
        let app = full_app(
            data,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
        );
        assert_eq!(app.rows.iter().filter(|r| r.depth == 0).count(), 3);
        assert_eq!(app.rows.iter().filter(|r| r.name == "feature").count(), 1);
        assert_eq!(
            app.rows
                .iter()
                .filter(|r| r.name == "different-feature")
                .count(),
            1
        );
    }

    #[test]
    fn refresh_preserves_selection_by_identity_after_rename_and_insertion() {
        let mut app = app();
        app.selected = app.rows.iter().position(|r| r.name == "notes").unwrap();
        let mut data = snapshot();
        data.workspaces[0].tabs[0].panes[0].name = Some("renamed notes".into());
        data.workspaces.swap(0, 2);
        app.update_snapshot(data, &History::default());
        assert_eq!(
            app.selected_target(),
            Some(FocusTarget::Pane("named-notes".into()))
        );
        assert_eq!(app.rows[app.selected].name, "renamed notes");
    }

    #[test]
    fn removing_selected_row_clamps_selection_and_empty_lists_are_safe() {
        let mut app = app();
        app.selected = 6;
        let mut data = snapshot();
        data.workspaces.retain(|w| w.id != "worktree");
        app.update_snapshot(data, &History::default());
        assert!(app.selected < app.rows.len());
        app.set_query("no-such-name");
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Enter,
        ] {
            key(&mut app, code);
        }
        assert!(app.selected_target().is_none());
    }

    #[test]
    fn up_down_wrap_and_ctrl_keys_do_not_become_query_text() {
        let mut app = app();
        app.selected = 0;
        key(&mut app, KeyCode::Up);
        assert_eq!(app.selected, app.rows.len() - 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.selected, 0);
        assert!(app.query.is_empty());
        assert_eq!(key(&mut app, KeyCode::Enter), KeyAction::Select);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            KeyAction::Dismiss
        );
    }

    #[test]
    fn sibling_connections_use_aligned_box_drawing_branches() {
        let app = app();
        let prefix = |name| {
            app.rows
                .iter()
                .find(|r| r.name == name)
                .unwrap()
                .prefix
                .as_str()
        };
        assert_eq!(prefix("Project"), "");
        assert_eq!(prefix("feature"), "├─ ");
        assert_eq!(prefix("Review"), "│  └─ ");
        assert_eq!(prefix("reviewer"), "│     └─ ");
        assert_eq!(prefix("Editor"), "├─ ");
        assert_eq!(prefix("notes"), "│  ├─ ");
        assert_eq!(prefix("hidden-title"), "│  └─ ");
        assert_eq!(prefix("Tests"), "└─ ");
    }
}
