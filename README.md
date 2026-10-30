# Herdr Canopy

Navigate [Herdr](https://herdr.dev/) workspaces, worktrees, tabs, and panes from a
searchable tree or a flat agents-only list, with live agent activity.

Open the popup and press Enter to reach the pane that needs your attention most.
Blocked agents come first, followed by unseen completions, working agents, and
recent panes. Type to find a destination, or browse the tree with the arrow keys.
Agent status bubbles and working times update while the navigator is open.

```text
 Project                                ● ● 
├─  Editor                                ● 
│  ├─ ✳ notes                         1m 5s  ●
│  └─ ֎ review                               
└─  fix-tests                               ●
   └─  Tests                                ●
      └─ ֎ runner                       12s  ●
```

- **Workspace tree:** workspaces contain tabs and panes; linked worktrees add a branch
  beneath their main workspace.
- **Attention-aware switching:** blocked, completed, and working agents take
  priority, with recent focus breaking ties.
- **Compact by default:** see workspace groups, agent panes, and the path to the
  preferred destination. Press Tab for the full tree.
- **Agent activity:** provider icons, status bubbles, and working times for Codex
  and Claude help you find work that needs attention.
- **Agents-only mode:** one row per agent with workspace and optional worktree
  context, ordered globally by attention and recency.
- **Terminal colors:** the popup follows your terminal palette and Herdr theme.

## Installation

Requires macOS or Linux, Herdr **0.9.5 or later**, and a Rust toolchain. Use a
[Nerd Font](https://www.nerdfonts.com/) in your terminal for the workspace, tab,
and pane glyphs.

Install from GitHub with Herdr's plugin manager:

```sh
herdr plugin install serhez/herdr-canopy
```

Or clone, build, and link a local checkout:

```sh
git clone https://github.com/serhez/herdr-canopy.git
cd herdr-canopy
cargo build --release --locked
herdr plugin link "$PWD"
```

### Bind a shortcut

Add a command binding to Herdr's `config.toml`, replacing any existing binding
for the same key:

```toml
[[keys.command]]
key = "cmd+s"
type = "plugin_action"
command = "serhez.herdr-canopy.toggle"
description = "Toggle workspace tree"

[[keys.command]]
key = "cmd+a"
type = "plugin_action"
command = "serhez.herdr-canopy.toggle-agents"
description = "Toggle agents"
```

Reload Herdr's configuration:

```sh
herdr server reload-config
```

Press Cmd+S for the tree or Cmd+A for agents. Press the same shortcut again to
close its popup; the other shortcut switches modes. For Alt shortcuts, add the
same bindings with `key = "alt+s"` and `key = "alt+a"`. Choose other keys in
Herdr's configuration if you prefer. The plugin does not reserve shortcuts
internally.

You can also invoke it from a Herdr terminal:

```sh
herdr plugin action invoke serhez.herdr-canopy.toggle
```

| Plugin action | Behavior |
| --- | --- |
| `serhez.herdr-canopy.toggle` | Toggle the workspace tree |
| `serhez.herdr-canopy.open` | Open the workspace tree |
| `serhez.herdr-canopy.toggle-agents` | Toggle the agents-only list |
| `serhez.herdr-canopy.open-agents` | Open the agents-only list |

Canopy keeps one popup per session. Opening a different mode replaces the active
popup; opening the same mode keeps it open. Both modes share focus history and
working-time tracking, and use the same 65-column, 30-row popup size.

## Navigation

The tree popup starts in **compact view**: workspaces and linked worktrees, every
pane containing an agent, and the preselected preferred destination, with their
ancestors. Agent panes appear regardless of provider or status, including unnamed
panes. Press **Tab** for the full tree, which also includes other tabs and
manually named panes. `[all]` beside the search input indicates full view.

### Agents-only mode

Every row represents one pane containing an agent, including unnamed agents and
every provider/status. The workspace and optional worktree appear inline, with
middle-dot separators; tab names and tree guides are omitted:

```text
 Project · ✳ notes                                    1m 5s  ●
 Project ·  fix-tests · ֎ runner                       12s  ●
```

Agents follow the same attention-aware MRU priority as the tree, sorted globally
across all tabs, worktrees, and workspaces. The current agent remains visible as
disabled gray context, with its bubble retaining its status color. The first
selectable agent is preselected; there is no tab/workspace fallback when the list
is empty.

Search retains the tree's matching rules: workspace, worktree, or tab matches
reveal their agent panes, and pane-name matches reveal that agent. Tab names
remain searchable even though they are omitted from rows. Search selects the
first eligible agent by attention and recency. Live refreshes preserve the
selected agent when it remains visible. Tab and Left/Right have no effect in this
flat mode; the other navigation keys work as in the tree.

Icons, status colors, timers, and name fallbacks are shared with the tree. Long
workspace, worktree, and pane names are shortened individually with `…`, keeping
their icons visible and reserving space for the complete running time and bubble.

### Attention-aware MRU

Selectable panes follow this priority:

1. **Blocked agents (red):** agents waiting for attention.
2. **Unseen completions (blue):** agents Herdr reports as done, before you visit them.
3. **Working agents (yellow):** agents actively working.
4. **Other panes:** idle agents, unknown states, and named panes without agents.

Within each group, the most recently focused pane comes first. This priority
applies across tabs, worktrees, and workspaces: each branch is ordered by its
highest-priority eligible pane and that pane's recency. A recent parent focus
cannot override a pane's priority. Workspace and worktree groups stay together in
the tree.

The highest-priority eligible pane is preselected on opening. When no agent needs
attention or is working, this returns you to the most recent eligible pane. If
there are no selectable panes, tree mode falls back to a recent tab or workspace.
Herdr supplies the agent states; a completed agent loses its blue
priority when Herdr marks it seen/idle after a visit.

### Search

In the tree, type to fuzzy-search workspace, worktree, tab, and pane names, case insensitively.
A workspace, worktree, or tab name match reveals its eligible panes and their
ancestors. A pane-name match keeps its ancestors for context without revealing
unrelated branches. Unnamed panes without agents remain excluded.

Search preselects the **first selectable pane** in the ordered results, even when
a workspace, worktree, tab, or another pane has a better fuzzy-name score. Panes
and branches retain the attention-aware MRU order. If only the current pane
matches, its dimmed context row is selected; if no pane matches, a matching
container remains available.

Search temporarily opens matching branches. Clearing it restores your collapsed
branches and the preferred destination selection. Live refreshes and full-view
toggling preserve your selected destination when it remains visible.

In the tree, the current workspace and tab are context rows. The current agent pane also
stays visible as context: its text and icon are dimmed, while its bubble keeps its
status color. These rows cannot switch focus to the current location. The current
pane without an agent stays hidden. When a main workspace is closed, its linked
worktrees use a repository grouping row; Enter on that row moves into its children.

| Key | Action |
| --- | --- |
| Tab | Toggle compact/full view (tree only) |
| Up / Ctrl+P / Cmd+K / Alt+K | Previous row |
| Down / Ctrl+N / Cmd+J / Alt+J | Next row |
| Right | Expand a branch, or move to its first child (tree only) |
| Left | Collapse a branch, or move to its parent (tree only) |
| Enter | Focus the selected destination |
| Esc | Clear search; close when search is empty |
| Ctrl+C | Close |
| Backspace | Delete the last search character |

Collapsed branches show `[+]`. Selecting a pane focuses it with zoom disabled.

### Names and icons

Workspace, worktree, and tab rows use ``, ``, and ``. Panes use ``, replaced
by `֎` for Codex, `✳` for Claude, or `◇` for another agent provider.

Manual pane names take precedence. An unnamed agent pane uses its cleaned
terminal title, then its raw title, then the agent type and pane ID. These are
display labels; the navigator does not rename panes. Unnamed panes without agents
are excluded in compact, full, and agents-only views.

Long names end with `…`, preserving whole Unicode characters and emoji. Search
still uses the full name. Working times stay complete while names shorten.

## Agent activity

In the tree, the right side of each row shows one bubble per nested agent,
including agents in hidden, unnamed, or current panes. Workspace totals include
linked worktrees. Agents-only rows each show their own agent's bubble.
Bubbles are ordered by status:

| Status | Color | Marker |
| --- | --- | --- |
| Blocked | Red | `●` |
| Working | Yellow | `●` |
| Done | Blue | `●` |
| Idle / ready | Green | `` |
| Unknown | Muted gray | `●` |

Agent detection, tree contents, and bubble counts and colors refresh every two
seconds. Selection follows the same destination through name and status changes.

### Working times

Working Codex and Claude panes show elapsed time immediately before their bubble,
with two spaces between them: `5s  ●`. Timers tick every second and show only
nonzero units, without padding: `5s`, `1m 5s`, `1h 5s`, or `1d 2h 3m 4s`.
Other providers, nonworking agents, and parent rows have no timer.

Timing starts when the plugin receives a Herdr working-status event, continues
while the popup is closed, and resets for each new working period. Tracking is
registered when you link the plugin. An agent already working before tracking
begins has no known start, so its timer stays blank until the next observed
working period. Zero elapsed seconds are also blank.

Time and bubbles share the existing status area. The complete timer takes
priority over name space; if the terminal cannot fit it with its bubble and at
least one name cell, the timer is omitted entirely.

## Configuration

### Keybindings

Find the plugin's configuration directory:

```sh
herdr plugin config-dir serhez.herdr-canopy
```

Create `config.toml` there to override navigation keys. Unspecified bindings keep
their defaults. The defaults are:

```toml
[keybindings]
toggle_all = ["Tab"]
move_up = ["Up", "C-p", "D-k", "M-k"]
move_down = ["Down", "C-n", "D-j", "M-j"]
expand = ["Right"]
collapse = ["Left"]
select = ["Enter"]
dismiss = ["Esc"]
force_quit = ["C-c"]
backspace = ["Backspace"]
```

Bindings accept named keys, literal characters, and `C-` (Control), `S-` (Shift),
`M-`/`A-` (Alt), and `D-` (Command/Super) modifiers. Settings are read when the
popup opens. Configure the shortcut that opens the popup separately in Herdr.

Cmd/Alt+J moves down and Cmd/Alt+K moves up inside Canopy by default. These
bindings apply to Canopy's terminal UI. Canopy requests enhanced keyboard
reporting to receive Command/Super keys and restores the previous mode on exit.

### Appearance

The popup is **65 columns × 30 rows**. To change its dimensions, edit `width` and
`height` in the `navigator` pane entry of `herdr-plugin.toml`, then relink the
plugin. The new size applies the next time you open it.

Colors follow the terminal's ANSI palette and default foreground, with overrides
from Herdr's `[theme.custom]` settings. The plugin reads `HERDR_CONFIG_PATH`, then
`$XDG_CONFIG_HOME/herdr/config.toml`, then `~/.config/herdr/config.toml`.

The background uses `sidebar_bg`, then `surface_dim`, then a darker shade of RGB
`panel_bg`, falling back to ANSI black. Selection uses `active_row_bg` or
`selection_bg`, falling back to ANSI dark gray. Gray tree guides and horizontal
rules separate workspace groups and the search input.

Icons use blue for workspaces, mauve/purple for worktrees, yellow for tabs,
peach/orange for Claude, and teal for other agents. Codex icons follow ANSI
palette index 6, which is Kitty's `color6`. Corresponding Herdr custom tokens
override the other icon and status colors. Search highlights use `ui.accent`,
then the custom accent, then ANSI cyan.

Working times use the lighter `subtext0` gray, falling back to ANSI gray. Disabled
text and tree guides use the darker `overlay0` gray, falling back to ANSI dark
gray. The current pane's timer keeps that disabled text color.

## Development

```sh
cargo test --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo fmt --check
cargo run --locked -- --mock
cargo run --locked -- --mock --view agents
```

The mock preview uses sample data without connecting to Herdr. Live connection
failures are reported; a failed refresh keeps the last complete view and shows
an error.

## Credits and license

Originally derived from [Herdr Recent Navigator](https://github.com/beyondlex/herdr-recent-navigator)
by [beyondlex](https://github.com/beyondlex). Herdr Canopy is independently
maintained with its own tree interface, navigation behavior, and agent activity
tracking.

Licensed under the [MIT License](LICENSE).
