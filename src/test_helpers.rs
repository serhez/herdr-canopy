use crate::models::Snapshot;
use serde_json::json;

pub fn snapshot() -> Snapshot {
    crate::backends::herdr::ipc::decode_snapshot(
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

/// Reruns the calling test alone in a new process of this test binary and
/// returns true once it passed there; returns false inside that process, which
/// then runs the test. For tests that release a file lock and then need it
/// free: a `flock` belongs to the open file, and a process that another test
/// thread starts holds a copy of every open file until it runs its program.
pub fn rerun_alone() -> bool {
    const CHILD: &str = "CANOPY_TEST_RERUN_ALONE";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let thread = std::thread::current();
    let test = thread
        .name()
        .expect("libtest names the thread after its test");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{test} alone:\n{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
