---
name: rex-agents
description: "See and talk to the coding agents running in Rex: list every agent on the Rex server with its session, repo, branch and state; prompt one and wait for its answer; read its screen; send it keys; start Claude Code or Codex in a block. Use when the user asks to check on, coordinate with, or hand work to other agents in Rex. Requires running inside Rex (REX_SESSION set) and herdr-canopy."
---

# Rex agents

`herdr-canopy rex agent` lists and drives the agents in every session of the Rex
server you run in. It mirrors `herdr agent`: the same commands, JSON replies and
error codes.

First check that you run inside Rex:

```bash
test -n "${REX_SESSION:-}"
```

If the check fails, say that you are not running inside Rex and stop.

## Find agents

```bash
herdr-canopy rex agent list
herdr-canopy rex agent get reviewer
```

Replies are JSON on stdout: `.result.agents[]` for `list`, `.result.agent` for
the others. Errors are JSON on stderr (`.error.code`, `.error.message`) with exit
status 1; usage errors exit with status 2. Each agent has:

- `pane_id` (its block id), `workspace` and `workspace_id` (its session),
  `tab` and `tab_id` (its window), `name` (its block's label, or null)
- `agent` (`claude`, `codex`, …), `agent_status`, `message` (what it says it is
  doing or asking), `blocked_kind` (`permission`, `question` or `auth`)
- `cwd`, `terminal_title`, and `worktree`: the session's checkout
  (`repo_name`, `checkout_path`, `branch`, `is_linked_worktree`), or null

States: `working`; `blocked`, waiting at an approval or a question; `idle` and
`done`, both ready for input (`done` means finished and not yet looked at);
`unknown`, when Canopy cannot tell. Claude Code and Codex report their state, so
for them `unknown` means starting up or waiting at a startup dialog. Other agents
report nothing and always show `unknown`.

Your own block is listed too: its `pane_id` is `$REX_BLOCK`.

A TARGET is a `pane_id`, a name, or `SESSION/NAME` (the session's label) when
names repeat; a name on several agents fails with `agent_target_ambiguous`. Prefer
the `pane_id` from a reply.

## Prompt and wait

```bash
herdr-canopy rex agent prompt reviewer "Review the current diff and report only actionable findings." --wait --timeout 600000
herdr-canopy rex agent read reviewer --source recent-unwrapped --lines 120
```

- `prompt` pastes the text and presses Enter. Before sending anything it refuses
  a `blocked` agent (`agent_blocked`) and a Claude or Codex in `unknown`
  (`agent_not_ready`).
- With `--wait`, it returns at the first `idle`, `done` or `blocked`. An agent
  that was not working must start working or block within 5 seconds, else
  `agent_prompt_stalled`. Add `--until STATE` (repeatable) only to end on other
  states. `--timeout MS` includes the submission; without it the wait has no
  limit. An agent that reports nothing never satisfies `--wait`.
- `wait TARGET [--until STATE]… [--timeout MS]` waits without sending; it returns
  at once if the agent is already in a requested state.
- A timeout or stall does not prove the prompt was lost: `read` the agent before
  sending it again.
- When a wait ends `blocked`, `get` and `read` the agent to see what it asks, and
  ask the user before answering an approval.

## Read and send keys

- `read TARGET [--source visible|recent|recent-unwrapped] [--lines N]` returns
  text in `.result.read.text`: the screen (`visible`), or screen and scrollback
  (`recent`, the default; `recent-unwrapped` joins wrapped lines, best for
  answers). Recent sources keep the last 200 lines unless `--lines` says
  otherwise; `.result.read.truncated` says whether lines were left out.
- `send-keys TARGET KEY…` presses keys such as `esc`, `enter`, `down`, `ctrl+c`.
  All are checked before any is sent (`invalid_key`).

## Rename, focus, start

- `rename TARGET NAME` sets the agent's name: a lowercase letter, then up to 31
  lowercase letters, digits, `-` or `_`, not used by another agent.
  `rename TARGET --clear` removes it.
- `focus TARGET` shows the agent in the Rex app and marks it seen. Use it only
  when the user asks to see an agent.
- `start NAME --kind claude|codex --pane BLOCK [--timeout MS] [-- AGENT_ARG…]`
  starts an agent in an existing block at a shell prompt, names it, and returns
  once it is ready (`.result.agent`). Create a block for it with
  `rex split -b "$REX_BLOCK" --focus=false --cwd "$PWD" --json`. If it returns
  `agent_not_ready`, the agent is blocked or silent at a startup dialog: `read`
  it, and ask the user before accepting a trust or hooks prompt.
