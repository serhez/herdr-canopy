-- Canopy for Rex: the navigator actions and the watcher.
--
--   local canopy = require("canopy")
--   canopy.setup{}          -- options below
--   rex.bind("cmd+s", "canopy_toggle")
--
-- Run as a script (`rex do canopy.lua --action canopy_toggle`), it sets itself
-- up from the script's arguments instead, to try it without reloading the config.

local canopy = {}

-- The popup labels, as `herdr-canopy rex ui` titles them.
local LABELS = { ["Canopy"] = true, ["Canopy Agents"] = true }

local ACTIONS = {
  { "canopy_toggle", "Toggle workspace tree", "toggle", "tree" },
  { "canopy_open", "Open workspace tree", "open", "tree" },
  { "canopy_toggle_agents", "Toggle agents", "toggle", "agents" },
  { "canopy_open_agents", "Open agents", "open", "agents" },
}

local options

local function default_rex()
  local dir = os.getenv("REX_BIN_DIR")
  if dir and dir ~= "" then return dir .. "/rex" end
  return "rex"
end

local function popup(command, view)
  return function(ctx)
    local argv = { options.bin, "rex", command, "--view", view,
      "--session", ctx.session_id, "--block", ctx.block_id, "--rex", options.rex }
    local function add(...) for _, v in ipairs({ ... }) do argv[#argv + 1] = v end end
    if ctx.client_id then add("--client", ctx.client_id) end
    if options.config then add("--config", options.config) end
    local size = options.sizes and options.sizes[view]
    if size then add("--size", string.format("%dx%d", size[1], size[2])) end
    -- Returns once the popup has started; it never waits for a selection.
    local r, err = rex.exec(argv, { timeout = 10 })
    if not r then error("Canopy: " .. tostring(err), 0) end
    if not r.ok then
      local reason = r.stderr ~= "" and r.stderr or ("exit status " .. tostring(r.code))
      error("Canopy: " .. reason, 0)
    end
  end
end

--- Defines the actions and starts this server's watcher.
-- opts.bin     herdr-canopy binary (default: ~/.cargo/bin/herdr-canopy)
-- opts.rex     rex binary (default: $REX_BIN_DIR/rex, else rex on PATH)
-- opts.config  Canopy's config file (default: ~/.config/herdr-canopy/config.toml)
-- opts.sizes   popup sizes in cells, e.g. { tree = { 65, 30 }, agents = { 70, 18 } }
-- opts.watch   false to not start the watcher here (the first popup starts it)
function canopy.setup(opts)
  opts = opts or {}
  options = {
    bin = opts.bin or ((os.getenv("HOME") or "") .. "/.cargo/bin/herdr-canopy"),
    rex = opts.rex or default_rex(),
    config = opts.config,
    sizes = opts.sizes,
  }
  for _, a in ipairs(ACTIONS) do
    rex.action{ name = a[1], title = a[2], category = "Canopy", run = popup(a[3], a[4]) }
  end
  if opts.watch ~= false then
    local h, err = rex.exec.start({ options.bin, "rex", "watch", "--ensure", "--rex", options.rex },
      { detach = true, stdout = false, stderr = false })
    if not h then rex.log("Canopy: cannot start the watcher: " .. tostring(err)) end
  end
end

--- Whether `ctx.block_id` is a Canopy popup, for key bindings that should reach it.
function canopy.popup_focused(ctx)
  local view = ctx.block_id and rex.session.view{ session_id = ctx.session_id }
  for _, w in ipairs((view and view.windows) or {}) do
    for _, layer in ipairs(w.layers or {}) do
      for _, b in ipairs(layer.blocks or {}) do
        if b.block_id == ctx.block_id then
          return layer.kind == "floating" and LABELS[b.label] == true
        end
      end
    end
  end
  return false
end

if rex.args then
  canopy.setup(rex.args)
else
  return canopy
end
