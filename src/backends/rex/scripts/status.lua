-- canopy:status
-- The server's identity.
local status, err = rex.call("server.status")
if not status then error(err, 0) end
print(rex.json.encode({ host_id = status.host_id }))
