---
name: rex-agents
description: "Prompt, wait on, read and start coding agents in Rex with herdr-canopy. Use to coordinate with other agents in Rex or to run worker or reviewer agents. Requires Rex (REX_SESSION set)."
---

# Rex agents

`herdr-canopy rex agent COMMAND` drives the agents in all your Rex sessions. If
`$REX_SESSION` is empty, say you are not in Rex and stop. Replies are JSON on
stdout (`.result.agents[]` or `.result.agent`); errors go to stderr
(`.error.code`), exit 1.

`list`, `get TARGET`. A TARGET is a `pane_id` (best), a name, or
`SESSION/NAME`; yours is `$REX_BLOCK`. States: `working`, `blocked` (at an
approval or question), `idle` or `done` (ready), `unknown` (starting, at a
startup dialog, or reporting nothing).

## Prompt and wait

```bash
herdr-canopy rex agent prompt reviewer "From planner: review the diff; end your turn with the findings." --wait --timeout 600000
```

The agent sees your prompt as if the user typed it, and answers on its screen.
Say who you are (your name or `$REX_BLOCK`) and ask it to end its turn with
its answer or a question.

- `prompt TARGET TEXT` refuses `blocked` agents and a starting Claude or Codex.
  `--wait` returns at `idle`, `done` or `blocked` (or `--until STATE`); a
  resting agent must start working within 5 s.
- `wait TARGET [--until STATE]… [--timeout MS]` waits without sending.
- Waiting on yourself, or on an agent that waits on you, fails
  (`agent_waiting_on_you`). When an agent prompts you, end your turn with your
  answer, or reply with a plain `prompt`.
- A timeout or stall does not mean the prompt was lost: `read` before sending
  again. Set your shell tool's time limit above `--timeout`, or rerun `wait`.

## Read and keys

- `read TARGET --source recent-unwrapped [--lines N]` returns
  `.result.read.text`: the last 200 lines with scrollback (`visible`: the
  screen only).
- `send-keys TARGET KEY…` (`esc`, `enter`, `ctrl+c`, …).
- `rename TARGET NAME|--clear` (lowercase `a-z0-9-_`, unique). `focus TARGET`
  shows the agent in the Rex app: only if the user asks.

## Start agents

`start NAME --kind claude|codex --pane BLOCK [-- AGENT_ARG…]` starts an agent at
the shell prompt in BLOCK and returns once it is ready. Make the block without
moving the user's view:

- Your checkout (reviewer, tester): a pane,
  `rex split -b "$REX_BLOCK" --focus=false --cwd "$PWD" --json` (a tab:
  `rex window new … --focus=false`); the block is `.block_ids[0]`.
- Its own worktree (parallel workers):
  `worktree-sync rex new BRANCH --wait --no-switch`, whose `block_id` is a shell
  there (skill `rex-worktrees`).

Only one agent per checkout should change files. When done, close its pane
(`rex block close -s SESSION BLOCK`), or remove its worktree once the work is
merged or saved (`worktree-sync rex remove --session SESSION --yes
--no-switch`, which ends its agents). Leave other agents and worktrees alone
unless asked. If `start` fails, `read` the block (a startup dialog or shell
question). Skill `rex` covers the rest of Rex.

Ask the user before answering an approval, trust or hooks prompt, unless more
specific instructions (the user's or a skill's) say otherwise.
