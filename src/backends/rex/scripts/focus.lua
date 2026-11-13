-- canopy:focus
-- Focuses `id` (kind "block", "window" or "session") inside its own session,
-- ending another block's zoom, and reports where it is and which apps are connected.
local attached, err = rex.call("session.attach_all", { event_scope = "session" })
if not attached then error(err, 0) end
local a = rex.args
local function must(result, message)
  if result == nil then error(message, 0) end
  return result
end

local function locate()
  for _, s in ipairs((rex.call("session.list") or {}).sessions or {}) do
    local view = rex.call("session.view", { session_id = s.session_id })
    if view then
      if a.kind == "session" and s.session_id == a.id then
        return s.session_id, view.active_window_id, nil
      end
      for _, w in ipairs(view.windows or {}) do
        if a.kind == "window" and w.window_id == a.id then return s.session_id, w.window_id, w end
        if a.kind == "block" then
          for _, layer in ipairs(w.layers or {}) do
            for _, b in ipairs(layer.blocks or {}) do
              if b.block_id == a.id then return s.session_id, w.window_id, w end
            end
          end
        end
      end
    end
  end
end

local session, window, w = locate()
if not session then error("Canopy: " .. tostring(a.kind) .. " " .. tostring(a.id) .. " no longer exists", 0) end
if a.kind == "block" then
  if w.zoomed_block_id and w.zoomed_block_id ~= a.id then
    must(rex.call("session.zoom_block", { session_id = session, window_id = window, zoom = false }))
  end
  must(rex.call("session.focus_block", { session_id = session, block_id = a.id }))
elseif a.kind == "window" then
  must(rex.call("session.focus_window", { session_id = session, window_id = window }))
end

local apps = {}
for _, c in ipairs((rex.call("client.list", { kinds = { "app" } }) or {}).clients or {}) do
  if c.connection_state ~= "lost" then table.insert(apps, c.client_id) end
end
print(rex.json.encode({ session_id = session, window_id = window, apps = apps }))
