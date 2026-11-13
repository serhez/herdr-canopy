-- canopy:layer
-- action = "find": the layer holding `block_id` and its bounds.
-- action = "close": close `layer_id`, or the layer holding `block_id`.
-- action = "resize": move `layer_id` to `bounds`.
local attached, err = rex.call("session.attach_all", { event_scope = "session" })
if not attached then error(err, 0) end
local a = rex.args
local function must(result, message)
  if result == nil then error(message, 0) end
  return result
end

local function layer_of(block_id)
  local view = must(rex.call("session.view", { session_id = a.session_id }))
  for _, w in ipairs(view.windows or {}) do
    for _, layer in ipairs(w.layers or {}) do
      for _, b in ipairs(layer.blocks or {}) do
        if b.block_id == block_id then return layer end
      end
    end
  end
  error("Canopy: block " .. tostring(block_id) .. " is not in a layer", 0)
end

if a.action == "find" then
  local layer = layer_of(a.block_id)
  print(rex.json.encode({ layer_id = layer.layer_id, bounds = layer.bounds }))
elseif a.action == "close" then
  local layer_id = a.layer_id or layer_of(a.block_id).layer_id
  must(rex.call("session.close_layer", { session_id = a.session_id, layer_id = layer_id }))
  print("{}")
elseif a.action == "resize" then
  must(rex.call("session.resize_layer",
    { session_id = a.session_id, layer_id = a.layer_id, bounds = a.bounds }))
  print("{}")
else
  error("Canopy: unknown layer action " .. tostring(a.action), 0)
end
