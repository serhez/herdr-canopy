-- canopy:agent
-- One look at an agent's block for `herdr-canopy rex agent`. With `submit`, first
-- pastes `submit.text` and presses Enter, unless process `submit.pid` no longer
-- runs in the block: as its foreground process, or, unless `submit.at_prompt`,
-- an ancestor of it ({refused = true}, nothing sent). Then waits up to `wait_s`
-- seconds for the block's next status change and reports the block's process,
-- title and status, and whether that change was to working or blocked.
-- {gone = true} when the block or its session no longer exists.
local T = "com.superlogical.terminal"
local a = rex.args
local block = { session_id = a.session_id, block_id = a.block_id }

-- Rex's answer, nil when what it asks about no longer exists (a 404), or an error.
local function ask(method, args)
  local answer, err = rex.call(method, args)
  if answer then return answer end
  if tostring(err):find("status=404", 1, true) then return nil end
  error(err, 0)
end

local function reply(answer)
  print(rex.json.encode(answer))
end

local function runs(process, pid, at_prompt)
  local foreground = process.foreground
  if not foreground then return false end
  if foreground.pid == pid then return true end
  if at_prompt then return false end
  for _, p in ipairs(process.ancestors or {}) do
    if p.pid == pid then return true end
  end
  return false
end

local function send(event)
  local sent, err = rex.call("block.event", { session_id = a.session_id, block_id = a.block_id, event = event })
  if not sent then error(err, 0) end
end

local function enter(action)
  return { type = "key", payload = { key = "enter", mods = "", consumed_mods = "", text = "",
    unshifted_codepoint = 0, composing = false, action = action } }
end

-- Input events need the session attached.
if not ask("session.attach", { session_id = a.session_id }) then return reply({ gone = true }) end

if a.submit then
  local process = ask(T .. ".process", block)
  if not process then return reply({ gone = true }) end
  if not runs(process, a.submit.pid, a.submit.at_prompt) then return reply({ refused = true }) end
  send({ type = "paste", payload = { text = a.submit.text } })
  send(enter("press"))
  send(enter("release"))
end

local active = false
if (a.wait_s or 0) > 0 then
  local event = rex.wait(T .. ".program_status_changed", function(_, ctx)
    return ctx.block_id == a.block_id
  end, a.wait_s)
  local state = event and event.record and event.record.state
  active = state == "working" or state == "blocked"
end

local process = ask(T .. ".process", block)
if not process then return reply({ gone = true }) end
reply({
  process = process,
  title = (rex.call(T .. ".title", block)),
  status = ask(T .. ".program_status", block),
  active = active,
})
