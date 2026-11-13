-- canopy:watch
-- Event adapter for the watcher: one JSON line per focus change, terminal block
-- status/process change, closed block and destroyed session, after a baseline.
local T = "com.superlogical.terminal"
local function emit(line)
  io.stdout:write(rex.json.encode(line), "\n")
  io.stdout:flush()
end

-- block id -> { session, window }, refreshed from each view.
local where = {}

local function index(session_id, view)
  local blocks = {}
  for _, w in ipairs(view.windows or {}) do
    for _, layer in ipairs(w.layers or {}) do
      for _, b in ipairs(layer.blocks or {}) do
        where[b.block_id] = { session = session_id, window = w.window_id }
        if b.creator_name == T then table.insert(blocks, { id = b.block_id, window = w.window_id }) end
      end
    end
  end
  return blocks
end

local function focus_line(session_id, view, baseline)
  for _, w in ipairs(view.windows or {}) do
    if w.window_id == view.active_window_id and w.focused_block_id then
      local label, floating = nil, false
      for _, layer in ipairs(w.layers or {}) do
        for _, b in ipairs(layer.blocks or {}) do
          if b.block_id == w.focused_block_id then
            label, floating = b.label, layer.kind == "floating"
          end
        end
      end
      emit({ kind = "focus", session = session_id, window = w.window_id, block = w.focused_block_id,
             label = label, floating = floating, baseline = baseline })
    end
  end
end

local function block_line(session_id, window_id, block_id, baseline)
  local args = { session_id = session_id, block_id = block_id }
  emit({ kind = "block", session = session_id, window = window_id, block = block_id,
         process = rex.call(T .. ".process", args), status = rex.call(T .. ".program_status", args),
         baseline = baseline })
end

local function changed_block(ctx)
  local at = where[ctx.block_id]
  if not at and ctx.session_id then
    local view = rex.call("session.view", { session_id = ctx.session_id })
    if view then index(ctx.session_id, view) end
    at = where[ctx.block_id]
  end
  if at then block_line(at.session, at.window, ctx.block_id, false) end
end

local status = rex.call("server.status") or {}
emit({ kind = "hello", host_id = status.host_id })
for _, s in ipairs((rex.call("session.list") or {}).sessions or {}) do
  local view = rex.call("session.view", { session_id = s.session_id })
  if view then
    focus_line(s.session_id, view, true)
    for _, b in ipairs(index(s.session_id, view)) do block_line(s.session_id, b.window, b.id, true) end
  end
end
emit({ kind = "ready" })

rex.on("session_view_changed", function(ctx)
  local view = rex.call("session.view", { session_id = ctx.session_id })
  if view then
    index(ctx.session_id, view)
    focus_line(ctx.session_id, view, false)
  end
end)
rex.on("terminal.program_status_changed", changed_block)
rex.on("terminal.program_status_removed", changed_block)
rex.on("terminal.process_changed", changed_block)
rex.on("block_closed", function(_, ev)
  where[ev.block_id] = nil
  emit({ kind = "closed", block = ev.block_id })
end)
rex.on("session_destroyed", function(_, ev)
  emit({ kind = "session_gone", session = ev.session_id })
end)
