# Herdr Canopy

Navigate [Herdr](https://herdr.dev/) workspaces, worktrees, tabs, and panes from a
searchable tree or a flat agents-only list, with live agent activity. Canopy also
runs on [Rex](https://www.superlogical.com/rex/) servers; see [Rex](#rex).

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
working-time tracking. The tree popup is 65 columns × 30 rows; the agents popup
is 70 columns × 18 rows.

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

The tree popup is **65 columns × 30 rows**, and the agents popup is **70 columns
× 18 rows**. To change their dimensions, edit `width` and `height` in the
`navigator` or `agents` pane entry of `herdr-plugin.toml`, then relink the plugin.
The new size applies the next time you open it.

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

## Rex

On a Rex server, Rex sessions are Canopy's workspaces, windows are its tabs, and
blocks are its panes. The views, ordering, search, bubbles, working times, keys,
and colors are the same as on Herdr. Canopy opens as a floating layer centred in
the window you open it from, with one popup per server.

### Install for Rex

Build the binary and put the Lua module beside your Rex configuration:

```sh
cargo install --path . --locked
ln -s "$PWD/rex/canopy.lua" ~/.config/rex/canopy.lua
```

Add these lines to `~/.config/rex/init.lua`, choosing your own keys and
replacing any existing bindings for them:

```lua
local canopy = require("canopy")
canopy.setup{}
rex.bind("cmd+s", "canopy_toggle")
rex.bind("cmd+a", "canopy_toggle_agents")
```

Then run `rex config reload`. Press the same key again to close the popup.

| Rex action | Behavior |
| --- | --- |
| `canopy_toggle` | Toggle the workspace tree |
| `canopy_open` | Open the workspace tree |
| `canopy_toggle_agents` | Toggle the agents-only list |
| `canopy_open_agents` | Open the agents-only list |

A toggle from another window or session moves the popup there. `canopy.setup`
takes these options, all optional:

```lua
canopy.setup{
  bin = "/path/to/herdr-canopy",    -- default: ~/.cargo/bin/herdr-canopy
  rex = "/path/to/rex",             -- default: $REX_BIN_DIR/rex, else rex on PATH
  config = "/path/to/config.toml",  -- default: see Rex configuration below
  sizes = { tree = { 65, 30 }, agents = { 70, 18 } },  -- popup cells
  watch = true,                     -- start the watcher when the config loads
}
```

To try the module without changing your configuration, run an action from a Rex
terminal: `rex do rex/canopy.lua --action canopy_toggle`.

### Watcher

`canopy.setup` starts one background watcher per Rex server
(`herdr-canopy rex watch`). It records focus for recent-location ordering and
Claude and Codex working periods for timers. Opening the popup starts it again
if it is not running, and a rebuilt binary replaces the old watcher. Without a
watcher the popup still works, timers stay blank, and the popup says
`Canopy watcher is not running`.

### Jumping to another session

Jumps inside the session you opened Canopy from always work. To show another
session, Canopy asks the Rex app to switch, which needs **Remote Control**: turn
it on in the Rex app under Settings > Rex Server. Without it, Canopy
stays open and explains why. With several Rex apps connected, Canopy switches the
app that opened it; if it cannot tell which app that is, it stays open with a
notice.

### Keys inside the popup

Rex app bindings take precedence over the focused block, so keys that Canopy
uses, such as Cmd/Alt+J/K, never reach it if your configuration binds them. Use
`canopy.popup_focused(ctx)` to forward them. For example, in an action bound to
Cmd/Alt+J/K that moves focus by `args.direction`:

```lua
run = function(ctx, args)
  if canopy.popup_focused(ctx) and (args.direction == "down" or args.direction == "up") then
    rex.client.queue("pane.send_key", { key = args.direction == "down" and "alt+j" or "alt+k" })
    return
  end
  -- the action's usual behavior
end,
```

### Rex configuration

Canopy reads `$XDG_CONFIG_HOME/herdr-canopy/config.toml`, falling back to
`~/.config/herdr-canopy/config.toml` (or the `config` option). It takes the same
`[keybindings]` table as on Herdr and the Herdr `[theme.custom]` color tokens.
Without a background token, the popup keeps the Rex theme's background, like any
pane, and without `active_row_bg` the focused row is that background lifted
slightly toward the theme's text color, as the terminal reports them. Without
`peach`, the Claude icon takes palette color 16 when the theme sets it, as
themes such as Teide and Tokyo Night do for orange, and ANSI bright red
otherwise. Popup sizes
come from the `sizes` option. State lives in
`$XDG_STATE_HOME/herdr-canopy/rex-<server id>/`, falling back to
`~/.local/state/herdr-canopy/rex-<server id>/`.

Linked worktrees nest under their main checkout when
[worktree-sync](https://github.com/serhez/worktree-sync) has bound the
sessions. Canopy reads its bindings file read-only.

### Agent commands

`herdr-canopy rex agent` gives coding agents, and scripts, Herdr's `agent`
commands for every agent on the Rex server, with Herdr's arguments, JSON replies
and error codes:

| Command | Does |
| --- | --- |
| `list` | Every agent: block, session, window, name, kind, state, message, what a blocked agent waits for, working directory, and its session's worktree binding |
| `get TARGET` | One agent, with the same fields |
| `read TARGET [--source visible\|recent\|recent-unwrapped] [--lines N]` | Its screen, or screen and scrollback, as text |
| `prompt TARGET TEXT [--wait] [--until STATE]… [--timeout MS]` | Pastes the text and presses Enter; with `--wait`, returns once the agent settles |
| `wait TARGET [--until STATE]… [--timeout MS]` | Waits for a state without sending anything |
| `send-keys TARGET KEY…` | Presses keys such as `esc`, `enter`, `ctrl+c` |
| `rename TARGET NAME\|--clear` | Sets or removes its name |
| `focus TARGET` | Shows it in the Rex app (another session needs Remote Control) |
| `start NAME --kind claude\|codex --pane BLOCK [--timeout MS] [-- ARGS…]` | Starts an agent in a block at a shell prompt and returns once it is ready |

An agent's name is its block's label; a block with Rex's default label, such as
`shell`, has none. A TARGET is a block id, a name, or `SESSION/NAME` with the
session's label. Prompts, keys and starts to one block take turns through a lock
in Canopy's state directory, so two senders never mix their input. Once `prompt`
holds the lock, it checks the agent again right before typing, since another
sender's prompt may have changed it.

Differences from Herdr:

- States come from status reports (see the limitations below). Waits need an
  agent that reports, so `start` runs only Claude Code and Codex, and
  `prompt --wait` on another agent ends with `agent_prompt_stalled`.
- `wait` and `prompt --wait` from an agent (`REX_BLOCK` set) refuse, before
  sending anything, to wait on itself or on an agent that waits on it, directly
  or through others (`agent_waiting_on_you`): each would stay busy until a
  timeout. A plain `prompt` is never refused.
- `prompt` refuses a Claude or Codex in the unknown state (`agent_not_ready`),
  since its input could answer a startup dialog. `start` returns
  `agent_not_ready` when the agent runs but stays silent until the timeout:
  read its screen, and answer the dialog with `send-keys`. A failed `start` ends
  its message with the last lines of the block's screen.
- `read` gives text only, with no `ansi` format or `detection` source, and does
  not recover an agent's own scrolled-off history. There is no `attach` or
  `explain`.

The skill in `rex/skills/rex-agents` teaches agents to use these commands. Link it
into each agent's skills folder:

```sh
ln -s "$PWD/rex/skills/rex-agents" ~/.agents/skills/rex-agents
ln -s "$PWD/rex/skills/rex-agents" ~/.claude/skills/rex-agents
```

### Rex limitations

- Session switches made in the Rex app itself, such as from the sidebar or the
  session switcher, are not seen by the server, so they do not update recent
  locations. Focus changes inside a session and Canopy's own jumps do.
- Agent states come from Rex program status reports. An agent that reports
  nothing shows as unknown. Claude and Codex report once they are ready, so for
  them this means starting up or waiting at a startup dialog, such as a folder
  trust or hooks prompt.
- A Claude background agent runs in Claude's own daemon, so a pane viewing it
  reports nothing: `claude attach`, the `claude agents` view, or a Claude
  session moved to the background, whose process stays to show it. Canopy
  reads its state from Claude's files (`$CLAUDE_CONFIG_DIR`, by default
  `~/.claude`): the job file `jobs/ID/state.json` of the one job whose name is
  the title Claude set on the pane, else of the job the process opened (`claude
  attach ID`, or the job its file in `sessions/` names). Claude counts such an
  agent as working while it waits on its background tasks, and as blocked both
  at a dialog and when its last reply ends on a question or request for the
  user. In the second case, its `blocked_kind` is `reply` and `prompt` answers
  it when every Claude process running it is idle, no message waits to start a
  turn, and it has no background work (shells, subagents, monitors, scheduled
  wake-ups), which could open a dialog without a turn of its own; otherwise
  `prompt` refuses it until that changes. While a prompt's answer starts, the
  agent still shows its question, so `prompt --wait` counts only a newer block
  as a reaction. It shows as unknown in the `claude agents` list and when its
  files cannot be read. Such agents have no working timer.
- Rex reports neither the window's size nor its pane frame (header and
  padding), so Canopy estimates the window from the pane it opens from and
  learns the frame from each popup. The first popup on a server, or after a
  change of font size or pane density, resizes itself once before it draws;
  the next one or two may be a row or column off, and later ones open at the
  right size from any pane.
- The pane header and padding are Rex app settings, which apply to every pane,
  the popup included.

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
