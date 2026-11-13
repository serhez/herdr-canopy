-- canopy:snapshot
-- Every session's view plus each terminal block's process, title and program status.
local attached, err = rex.call("session.attach_all", { event_scope = "session" })
if not attached then error(err, 0) end
local T = "com.superlogical.terminal"
local status = rex.call("server.status") or {}
local out = { host_id = status.host_id, sessions = {} }
for _, s in ipairs((rex.call("session.list") or {}).sessions or {}) do
  local view = rex.call("session.view", { session_id = s.session_id })
  if view then
    local blocks = {}
    for _, w in ipairs(view.windows or {}) do
      for _, layer in ipairs(w.layers or {}) do
        for _, b in ipairs(layer.blocks or {}) do
          if b.creator_name == T then
            local args = { session_id = s.session_id, block_id = b.block_id }
            blocks[b.block_id] = {
              process = rex.call(T .. ".process", args),
              title = rex.call(T .. ".title", args),
              status = rex.call(T .. ".program_status", args),
            }
          end
        end
      end
    end
    table.insert(out.sessions, { view = view, blocks = blocks })
  end
end
print(rex.json.encode(out))
