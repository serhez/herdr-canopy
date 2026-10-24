use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

use crate::models::{ActiveLocation, AgentActivity, FocusTarget, Pane, Snapshot, Tab, Workspace};
use crate::tracker::WorkingTracker;
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub fn herdr_bin() -> String {
    std::env::var("HERDR_BIN_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "herdr".into())
}

pub fn response_result(text: &str) -> Result<Value> {
    let response: Value = serde_json::from_str(text).context("Invalid Herdr response")?;
    if let Some(error) = response.get("error") {
        anyhow::bail!("Herdr: {error}");
    }
    response
        .get("result")
        .cloned()
        .context("Herdr response is missing its result")
}

/// Read-only requests may fall back to the CLI when the socket transport is unavailable.
fn read_request(method: &str, args: &[&str]) -> Result<Value> {
    if let Ok(path) = std::env::var("HERDR_SOCKET_PATH")
        && let Ok(stream) = UnixStream::connect(path)
    {
        return socket_request(stream, method, json!({}));
    }
    cli_request(args)
}

fn socket_request(mut stream: UnixStream, method: &str, params: Value) -> Result<Value> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    serde_json::to_writer(
        &mut stream,
        &json!({"id":"herdr-canopy","method":method,"params":params}),
    )?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    response_result(&line)
}

pub fn cli_request(args: &[&str]) -> Result<Value> {
    let output = Command::new(herdr_bin())
        .args(args)
        .output()
        .context("Cannot run Herdr")?;
    if !output.status.success() {
        anyhow::bail!(
            "Herdr {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    response_result(std::str::from_utf8(&output.stdout)?)
}

pub fn fetch_snapshot() -> Result<Snapshot> {
    let workspaces = read_request("workspace.list", &["workspace", "list"])?;
    let tabs = read_request("tab.list", &["tab", "list"])?;
    let panes = read_request("pane.list", &["pane", "list"])?;
    let mut snapshot = decode_snapshot(workspaces, tabs, panes)?;
    let working = WorkingTracker::for_session().load()?;
    if !working.is_empty() {
        working.apply(&mut snapshot, &fetch_agent_activity()?);
    }
    if let Ok(pane_id) = std::env::var("HERDR_PANE_ID") {
        snapshot.exclude_pane(&pane_id);
    }
    Ok(snapshot)
}

pub fn fetch_agent_activity() -> Result<Vec<AgentActivity>> {
    let result = read_request("agent.list", &["agent", "list"])?;
    serde_json::from_value(result.get("agents").cloned().context("Missing agents")?)
        .context("Invalid agent activity")
}

pub fn fetch_focused_location() -> Result<ActiveLocation> {
    let result = read_request("pane.list", &["pane", "list"])?;
    let panes: Vec<Pane> =
        serde_json::from_value(result.get("panes").cloned().context("Missing panes")?)?;
    let pane = panes
        .into_iter()
        .find(|pane| pane.focused)
        .context("No focused pane")?;
    Ok(ActiveLocation {
        workspace_id: Some(pane.workspace_id),
        tab_id: Some(pane.tab_id),
        pane_id: Some(pane.id),
    })
}

pub fn decode_snapshot(workspaces: Value, tabs: Value, panes: Value) -> Result<Snapshot> {
    let mut workspaces: Vec<Workspace> = serde_json::from_value(
        workspaces
            .get("workspaces")
            .cloned()
            .context("Missing workspaces")?,
    )?;
    let mut tabs: Vec<Tab> =
        serde_json::from_value(tabs.get("tabs").cloned().context("Missing tabs")?)?;
    let panes: Vec<Pane> =
        serde_json::from_value(panes.get("panes").cloned().context("Missing panes")?)?;

    let tab_indices: HashMap<_, _> = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| ((t.workspace_id.clone(), t.id.clone()), i))
        .collect();
    for pane in panes {
        if let Some(&i) = tab_indices.get(&(pane.workspace_id.clone(), pane.tab_id.clone())) {
            tabs[i].panes.push(pane);
        }
    }
    let ws_indices: HashMap<_, _> = workspaces
        .iter()
        .enumerate()
        .map(|(i, w)| (w.id.clone(), i))
        .collect();
    for mut tab in tabs {
        if tab.name.trim().is_empty() {
            tab.name = tab.id.clone();
        }
        if let Some(&i) = ws_indices.get(&tab.workspace_id) {
            workspaces[i].tabs.push(tab);
        }
    }
    Ok(Snapshot { workspaces })
}

pub fn focus(target: &FocusTarget) -> Result<()> {
    let path = std::env::var("HERDR_SOCKET_PATH").context("HERDR_SOCKET_PATH is missing")?;
    focus_at(std::path::Path::new(&path), target)
}

fn focus_at(path: &std::path::Path, target: &FocusTarget) -> Result<()> {
    let request =
        |method: &str, params: Value| socket_request(UnixStream::connect(path)?, method, params);
    if let FocusTarget::Pane(id) = target {
        request("pane.zoom", json!({"pane_id":id,"mode":"off"}))?;
    }
    let (method, params) = match target {
        FocusTarget::Workspace(id) => ("workspace.focus", json!({"workspace_id":id})),
        FocusTarget::Tab(id) => ("tab.focus", json!({"tab_id":id})),
        FocusTarget::Pane(id) => ("pane.focus", json!({"pane_id":id})),
    };
    // A mutating request is sent once; a transport error must not trigger a retry.
    request(method, params)?;
    Ok(())
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selecting_a_pane_unzooms_then_explicitly_changes_focus() {
        use std::os::unix::net::UnixListener;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let mut focused = "original".to_string();
            let mut methods = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(2);
            while methods.len() < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut line = String::new();
                        BufReader::new(&stream).read_line(&mut line).unwrap();
                        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let method = request["method"].as_str().unwrap().to_string();
                        if method == "pane.focus" {
                            focused = request["params"]["pane_id"].as_str().unwrap().to_string();
                        }
                        if method == "pane.zoom" {
                            assert_eq!(request["params"]["mode"], "off");
                        }
                        methods.push(method);
                        writeln!(stream, "{}", json!({"id":"test","result":{"ok":true}})).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            (focused, methods)
        });
        focus_at(&path, &FocusTarget::Pane("target".into())).unwrap();
        let (focused, methods) = server.join().unwrap();
        assert_eq!(focused, "target");
        assert_eq!(methods, vec!["pane.zoom", "pane.focus"]);
    }

    #[test]
    fn navigator_overlay_excludes_itself_without_hiding_its_tab() {
        let mut snapshot = crate::test_helpers::snapshot();
        snapshot.exclude_pane("named-notes");
        assert_eq!(snapshot.workspaces[0].tabs[0].name, "Editor");
        assert_eq!(snapshot.workspaces[0].tabs[0].panes.len(), 1);
        assert_eq!(snapshot.workspaces[0].tabs[0].panes[0].id, "unnamed");
        assert_eq!(snapshot.workspaces[1].tabs[0].panes[0].id, "named-review");
    }

    #[test]
    fn decodes_manual_labels_without_promoting_titles_or_agent_metadata() {
        let snapshot = crate::test_helpers::snapshot();
        let panes = &snapshot.workspaces[0].tabs[0].panes;
        assert_eq!(panes[0].name.as_deref(), Some("notes"));
        assert!(panes[1].name.is_none());
    }

    #[test]
    fn preserves_workspace_and_tab_order_even_without_named_panes() {
        let snapshot = crate::test_helpers::snapshot();
        assert_eq!(snapshot.workspaces[2].name, "Other");
        assert!(snapshot.workspaces[2].tabs.is_empty());
        assert_eq!(snapshot.workspaces[0].tabs[1].name, "Tests");
    }

    #[test]
    fn malformed_or_missing_response_lists_are_errors() {
        for data in [
            json!({}),
            json!({"workspaces":null}),
            json!({"workspaces":[{}]}),
        ] {
            assert!(decode_snapshot(data, json!({"tabs":[]}), json!({"panes":[]})).is_err());
        }
    }

    #[test]
    fn ignores_stale_panes_and_tabs_during_nonatomic_refresh() {
        let data = decode_snapshot(
            json!({"workspaces":[{"workspace_id":"w","label":"workspace"}]}),
            json!({"tabs":[{"workspace_id":"closed","tab_id":"orphan","label":"orphan"}]}),
            json!({"panes":[{"workspace_id":"w","tab_id":"closed","pane_id":"orphan","label":"orphan"}]}),
        ).unwrap();
        assert!(data.workspaces[0].tabs.is_empty());
    }

    #[test]
    fn rpc_errors_are_not_treated_as_successful_data() {
        assert!(
            response_result(r#"{"id":"1","error":{"code":"not_found","message":"missing"}}"#)
                .is_err()
        );
        assert!(response_result("invalid").is_err());
        assert!(response_result(r#"{"id":"1"}"#).is_err());
        assert_eq!(
            response_result(r#"{"id":"1","result":{"ok":true}}"#).unwrap(),
            json!({"ok":true})
        );
    }

    #[test]
    fn tab_names_fall_back_to_ids_but_pane_names_do_not() {
        let data = decode_snapshot(
            json!({"workspaces":[{"workspace_id":"w","label":"workspace"}]}),
            json!({"tabs":[{"workspace_id":"w","tab_id":"w:t"}]}),
            json!({"panes":[{"workspace_id":"w","tab_id":"w:t","pane_id":"w:p","terminal_title":"automatic"}]}),
        ).unwrap();
        assert_eq!(data.workspaces[0].tabs[0].name, "w:t");
        assert!(data.workspaces[0].tabs[0].panes[0].name.is_none());
    }
}
