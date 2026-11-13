-- canopy:spawn
-- Opens the popup: a floating layer in `window_id` running `command`, focused.
local attached, err = rex.call("session.attach_all", { event_scope = "session" })
if not attached then error(err, 0) end
local a = rex.args
-- Disowning below needs the session attached.
attached, err = rex.call("session.attach", { session_id = a.session_id })
if not attached then error(err, 0) end
local layer, failure = rex.call("session.new_layer", {
  session_id = a.session_id,
  window_id = a.window_id,
  bounds = a.bounds,
  focus = true,
  layout = { block = {
    flavor = "com.superlogical.terminal.shell",
    label = a.label,
    options = { command = a.command, shell = "none", exit = { on_completion = true } },
  } },
})
if not layer then error(failure, 0) end
-- This short-lived client owns the new block, and a block takes its owner's
-- size, so the app would draw it only once it takes the block over, seconds
-- later. Giving it up hands it to the app at once. Best effort.
rex.call("session.disown_block", { session_id = a.session_id, block_ids = layer.block_ids })
print(rex.json.encode({ layer_id = layer.layer_id, block_id = (layer.block_ids or {})[1] }))
