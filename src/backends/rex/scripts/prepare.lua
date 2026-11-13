-- canopy:prepare
-- Where a popup opened from block `block_id` of session `session_id` goes:
-- the server, the block's window, its size in cells (and its cells' in pixels)
-- and its share of the window.
local attached, err = rex.call("session.attach_all", { event_scope = "session" })
if not attached then error(err, 0) end
local a = rex.args
local function must(result, message)
  if result == nil then error(message, 0) end
  return result
end
local status = must(rex.call("server.status"))
local view = must(rex.call("session.view", { session_id = a.session_id }))

local function rect_of(node, id)
  if not node then return nil end
  if node.block_id == id then return node.rect end
  if node.split then return rect_of(node.split.before, id) or rect_of(node.split.after, id) end
end

for _, w in ipairs(view.windows or {}) do
  for _, layer in ipairs(w.layers or {}) do
    for _, b in ipairs(layer.blocks or {}) do
      if b.block_id == a.block_id then
        local r = rect_of(layer.layout, a.block_id) or { 0, 0, 1, 1 }
        local lb = layer.bounds or { 0, 0, 1, 1 }
        local size = rex.call("com.superlogical.terminal.size",
          { session_id = a.session_id, block_id = a.block_id }) or {}
        print(rex.json.encode({
          host_id = status.host_id,
          window_id = w.window_id,
          size = { columns = size.columns or 0, rows = size.rows or 0,
                   cell_width_px = size.cell_width_px or 0, cell_height_px = size.cell_height_px or 0 },
          rect = { lb[1] + r[1] * lb[3], lb[2] + r[2] * lb[4], r[3] * lb[3], r[4] * lb[4] },
        }))
        return
      end
    end
  end
end
error("Canopy: block " .. tostring(a.block_id) .. " is not in session " .. tostring(a.session_id), 0)
