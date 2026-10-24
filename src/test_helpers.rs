use crate::models::Snapshot;
use serde_json::json;

pub fn snapshot() -> Snapshot {
    crate::ipc::decode_snapshot(
        json!({"workspaces": [
            {"workspace_id":"project","label":"Project","worktree":{"is_linked_worktree":false,"repo_key":"repo","repo_name":"project-repo"}},
            {"workspace_id":"worktree","label":"feature","worktree":{"is_linked_worktree":true,"repo_key":"repo","repo_name":"project-repo"}},
            {"workspace_id":"other","label":"Other"}
        ]}),
        json!({"tabs": [
            {"workspace_id":"project","tab_id":"editor","label":"Editor"},
            {"workspace_id":"project","tab_id":"tests","label":"Tests"},
            {"workspace_id":"worktree","tab_id":"review","label":"Review"}
        ]}),
        json!({"panes": [
            {"workspace_id":"project","tab_id":"editor","pane_id":"named-notes","label":"notes","agent":"claude","agent_status":"working"},
            {"workspace_id":"project","tab_id":"editor","pane_id":"unnamed","terminal_title":"hidden-title","title":"hidden-title","agent":"claude","agent_status":"idle"},
            {"workspace_id":"project","tab_id":"tests","pane_id":"blank","label":"","agent_status":"unknown"},
            {"workspace_id":"worktree","tab_id":"review","pane_id":"named-review","label":"reviewer","agent":"codex","agent_status":"done"}
        ]}),
    ).unwrap()
}
