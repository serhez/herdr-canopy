//! Reads worktree-sync's session bindings (`bindings.json`, schema v1) to nest
//! linked-worktree sessions under their main checkout. Canopy never writes it.

use crate::models::{Snapshot, Worktree};
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// `$XDG_STATE_HOME/worktree-sync/rex/bindings.json` when `XDG_STATE_HOME` is
/// absolute, else `$HOME/.local/state/worktree-sync/rex/bindings.json`.
pub fn path(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let state = xdg_state_home
        .map(Path::new)
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| Some(Path::new(home?).join(".local/state")))?;
    Some(state.join("worktree-sync/rex/bindings.json"))
}

/// One session's binding to a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub worktree: Worktree,
    pub checkout_path: Option<String>,
    /// `None` on a detached HEAD.
    pub branch: Option<String>,
    bound_at: u64,
}

/// One host's bindings, plus a notice when the file or some entries were unusable.
#[derive(Debug, Default)]
pub struct Bindings {
    sessions: HashMap<String, Binding>,
    pub notice: Option<String>,
}

impl Bindings {
    pub fn parse(text: &str, host_id: &str) -> Self {
        let ignored = |reason: &str| Self {
            sessions: HashMap::new(),
            notice: Some(format!("Worktree bindings ignored: {reason}")),
        };
        let document: Value = match serde_json::from_str(text) {
            Ok(document) => document,
            Err(error) => return ignored(&format!("invalid JSON ({error})")),
        };
        match document.get("version").and_then(Value::as_u64) {
            Some(1) => {}
            Some(version) => return ignored(&format!("unknown version {version}")),
            None => return ignored("missing version"),
        }
        let Some(hosts) = document.get("hosts").and_then(Value::as_object) else {
            return ignored("hosts is not an object");
        };
        let Some(host) = hosts.get(host_id) else {
            return Self::default();
        };
        let Some(entries) = host.get("sessions").and_then(Value::as_object) else {
            return ignored("sessions is not an object");
        };
        let mut skipped = 0;
        let mut sessions = HashMap::new();
        for (id, entry) in entries {
            let field = |key: &str| entry.get(key);
            let binding = (|| {
                Some(Binding {
                    worktree: Worktree {
                        is_linked_worktree: field("is_linked_worktree")?.as_bool()?,
                        repo_key: field("repo_key")?.as_str()?.to_owned(),
                        repo_name: field("repo_name")?.as_str()?.to_owned(),
                    },
                    checkout_path: field("checkout_path")
                        .and_then(Value::as_str)
                        .map(String::from),
                    branch: field("branch").and_then(Value::as_str).map(String::from),
                    bound_at: field("bound_at")?.as_u64()?,
                })
            })();
            match binding {
                Some(binding) => {
                    sessions.insert(id.clone(), binding);
                }
                None => skipped += 1,
            }
        }
        let notice = (skipped > 0)
            .then(|| format!("Worktree bindings: skipped {skipped} malformed entries"));
        Self { sessions, notice }
    }

    /// Session `id`'s binding, if it has one.
    pub fn session(&self, id: &str) -> Option<&Binding> {
        self.sessions.get(id)
    }

    /// A missing file means no bindings; an unreadable one is ignored with a notice.
    pub fn read(path: &Path, host_id: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, host_id),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => Self {
                sessions: HashMap::new(),
                notice: Some(format!("Worktree bindings ignored: {error}")),
            },
        }
    }

    /// Sets each bound session's worktree (one main checkout per repository) and
    /// adds the notice, if any, to the snapshot's.
    pub fn apply(&self, snapshot: &mut Snapshot) {
        // The parent of a repository's linked sessions: its earliest-bound main
        // checkout, ties broken by the smallest session id (worktree-sync's rule).
        let mut parents: HashMap<String, (u64, String)> = HashMap::new();
        for workspace in &snapshot.workspaces {
            if let Some(binding) = self.sessions.get(&workspace.id)
                && !binding.worktree.is_linked_worktree
            {
                let candidate = (binding.bound_at, workspace.id.clone());
                parents
                    .entry(binding.worktree.repo_key.clone())
                    .and_modify(|best| *best = best.clone().min(candidate.clone()))
                    .or_insert(candidate);
            }
        }
        for workspace in &mut snapshot.workspaces {
            workspace.worktree = self.sessions.get(&workspace.id).and_then(|binding| {
                let worktree = &binding.worktree;
                let is_parent = parents
                    .get(&worktree.repo_key)
                    .is_some_and(|(_, id)| *id == workspace.id);
                (worktree.is_linked_worktree || is_parent).then(|| worktree.clone())
            });
        }
        if let Some(notice) = &self.notice {
            snapshot.add_notice(notice);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::models::{
        ActiveLocation, FocusTarget, History, Keybindings, View, Workspace, Worktree,
    };
    use serde_json::{Value, json};

    /// worktree-sync's published example (`tests/fixtures/bindings-v1.json`), verbatim.
    const CONTRACT_EXAMPLE: &str = r#"{
  "version": 1,
  "hosts": {
    "fcf58739-fba5-4b01-b6cf-022cd6ea6b06": {
      "sessions": {
        "session:01a1d1dc-1bdf-7111-bc71-452288dfffa8": {
          "repo_key": "/Users/ser/dev/app/.git",
          "repo_name": "app",
          "checkout_path": "/Users/ser/dev/.worktrees/app/feat-login",
          "branch": "feat/login",
          "is_linked_worktree": true,
          "bound_at": 1794522240
        },
        "session:01a1d1dc-2c4e-7a10-9d3b-1f0e5a6c7b8d": {
          "repo_key": "/Users/ser/dev/app/.git",
          "repo_name": "app",
          "checkout_path": "/Users/ser/dev/app",
          "branch": null,
          "is_linked_worktree": false,
          "bound_at": 1794520000
        }
      }
    }
  }
}
"#;
    const HOST: &str = "fcf58739-fba5-4b01-b6cf-022cd6ea6b06";
    const LINKED: &str = "session:01a1d1dc-1bdf-7111-bc71-452288dfffa8";
    const MAIN: &str = "session:01a1d1dc-2c4e-7a10-9d3b-1f0e5a6c7b8d";
    const REPO: &str = "/Users/ser/dev/app/.git";

    fn sessions(ids: &[&str]) -> Snapshot {
        Snapshot {
            workspaces: ids
                .iter()
                .map(|id| Workspace {
                    id: (*id).into(),
                    name: format!("name of {id}"),
                    focused: false,
                    active_tab_id: None,
                    worktree: None,
                    tabs: Vec::new(),
                })
                .collect(),
            notice: None,
        }
    }

    fn applied(text: &str, ids: &[&str]) -> Snapshot {
        let mut snapshot = sessions(ids);
        Bindings::parse(text, HOST).apply(&mut snapshot);
        snapshot
    }

    #[test]
    fn every_bound_session_reports_its_checkout_and_branch() {
        let bindings = Bindings::parse(CONTRACT_EXAMPLE, HOST);
        let linked = bindings.session(LINKED).unwrap();
        assert_eq!(
            (
                linked.worktree.repo_name.as_str(),
                linked.checkout_path.as_deref(),
                linked.branch.as_deref(),
                linked.worktree.is_linked_worktree,
            ),
            (
                "app",
                Some("/Users/ser/dev/.worktrees/app/feat-login"),
                Some("feat/login"),
                true
            )
        );
        let main = bindings.session(MAIN).unwrap();
        assert_eq!(
            (main.checkout_path.as_deref(), main.branch.as_deref()),
            (Some("/Users/ser/dev/app"), None),
            "a detached HEAD has no branch"
        );
        assert!(bindings.session("session:unbound").is_none());
        let older = r#"{"version": 1, "hosts": {"fcf58739-fba5-4b01-b6cf-022cd6ea6b06": {"sessions": {
            "session:x": {"repo_key": "/r/.git", "repo_name": "r", "is_linked_worktree": false, "bound_at": 1}}}}}"#;
        let older = Bindings::parse(older, HOST);
        let entry = older.session("session:x").unwrap();
        assert_eq!(
            (entry.checkout_path.as_deref(), entry.branch.as_deref()),
            (None, None)
        );
    }

    fn worktree(snapshot: &Snapshot, id: &str) -> Option<Worktree> {
        snapshot
            .workspaces
            .iter()
            .find(|w| w.id == id)?
            .worktree
            .clone()
    }

    fn entry(repo: &str, linked: bool, bound_at: u64) -> Value {
        json!({"repo_key": repo, "repo_name": "app", "checkout_path": "/c", "branch": "main",
               "is_linked_worktree": linked, "bound_at": bound_at})
    }

    fn file(sessions: Value) -> String {
        json!({"version": 1, "hosts": {HOST: {"sessions": sessions}}}).to_string()
    }

    fn tree(snapshot: Snapshot) -> AppState {
        AppState::new(
            snapshot,
            &History::default(),
            ActiveLocation::default(),
            Keybindings::default(),
            View::Tree,
        )
    }

    #[test]
    fn the_bindings_path_follows_the_worktree_sync_contract() {
        let home = Some(OsStr::new("/home/me"));
        let fallback = PathBuf::from("/home/me/.local/state/worktree-sync/rex/bindings.json");
        assert_eq!(
            path(Some(OsStr::new("/state")), home),
            Some(PathBuf::from("/state/worktree-sync/rex/bindings.json"))
        );
        assert_eq!(
            path(Some(OsStr::new("relative/state")), home),
            Some(fallback.clone())
        );
        assert_eq!(path(Some(OsStr::new("")), home), Some(fallback.clone()));
        assert_eq!(path(None, home), Some(fallback));
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn the_contract_example_nests_the_linked_session_under_its_main_checkout() {
        let snapshot = applied(CONTRACT_EXAMPLE, &[MAIN, LINKED]);
        assert_eq!(snapshot.notice, None);
        let expected = |linked| Worktree {
            is_linked_worktree: linked,
            repo_key: REPO.into(),
            repo_name: "app".into(),
        };
        assert_eq!(worktree(&snapshot, MAIN), Some(expected(false)));
        assert_eq!(worktree(&snapshot, LINKED), Some(expected(true)));
        let app = tree(snapshot);
        let main = app
            .rows
            .iter()
            .position(|r| r.key == FocusTarget::Workspace(MAIN.into()).key())
            .unwrap();
        let linked = app
            .rows
            .iter()
            .position(|r| r.key == FocusTarget::Workspace(LINKED.into()).key())
            .unwrap();
        assert!(app.rows[main].has_children);
        assert_eq!(app.rows[linked].depth, app.rows[main].depth + 1);
    }

    #[test]
    fn a_linked_session_without_a_bound_main_checkout_is_grouped_by_repository() {
        let snapshot = applied(CONTRACT_EXAMPLE, &[LINKED, "session:unbound"]);
        assert_eq!(worktree(&snapshot, "session:unbound"), None);
        let app = tree(snapshot);
        let repo = app
            .rows
            .iter()
            .find(|r| r.key == format!("repo:{REPO}"))
            .expect("repository row");
        assert_eq!(repo.name, "app");
        assert!(repo.has_children);
    }

    #[test]
    fn missing_files_and_hosts_mean_no_nesting_and_no_notice() {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = sessions(&[MAIN, LINKED]);
        Bindings::read(&dir.path().join("missing.json"), HOST).apply(&mut snapshot);
        assert_eq!(snapshot, sessions(&[MAIN, LINKED]));
        let other_host = applied(
            &CONTRACT_EXAMPLE.replace(HOST, "another-host"),
            &[MAIN, LINKED],
        );
        assert_eq!(other_host, sessions(&[MAIN, LINKED]));
    }

    #[test]
    fn the_file_is_read_without_writing_or_locking_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bindings.json");
        std::fs::write(&file, CONTRACT_EXAMPLE).unwrap();
        let mut snapshot = sessions(&[MAIN, LINKED]);
        Bindings::read(&file, HOST).apply(&mut snapshot);
        assert!(worktree(&snapshot, LINKED).is_some());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["bindings.json"]);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), CONTRACT_EXAMPLE);
    }

    #[test]
    fn unreadable_or_unknown_files_are_ignored_with_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let mut unreadable = sessions(&[MAIN, LINKED]);
        Bindings::read(dir.path(), HOST).apply(&mut unreadable);
        let cases = [
            "{ not json".to_string(),
            CONTRACT_EXAMPLE.replace("\"version\": 1", "\"version\": 2"),
            CONTRACT_EXAMPLE.replace("\"version\": 1,", ""),
            json!({"version": 1, "hosts": []}).to_string(),
        ];
        let parsed = cases.iter().map(|text| applied(text, &[MAIN, LINKED]));
        for snapshot in std::iter::once(unreadable).chain(parsed) {
            assert!(snapshot.workspaces.iter().all(|w| w.worktree.is_none()));
            let notice = snapshot.notice.expect("notice");
            assert!(notice.starts_with("Worktree bindings ignored"), "{notice}");
        }
    }

    #[test]
    fn malformed_entries_are_skipped_and_counted() {
        let mut no_repo = entry(REPO, true, 1);
        no_repo.as_object_mut().unwrap().remove("repo_key");
        let text = file(json!({
            "session:ok": entry(REPO, false, 1),
            "session:no-repo": no_repo,
            "session:bad-time": {"repo_key": REPO, "repo_name": "app", "is_linked_worktree": true, "bound_at": "noon"},
            "session:bad-flag": {"repo_key": REPO, "repo_name": "app", "is_linked_worktree": "yes", "bound_at": 1},
            "session:not-object": 7
        }));
        let ids = [
            "session:ok",
            "session:no-repo",
            "session:bad-time",
            "session:bad-flag",
            "session:not-object",
        ];
        let snapshot = applied(&text, &ids);
        assert!(worktree(&snapshot, "session:ok").is_some());
        for id in &ids[1..] {
            assert_eq!(worktree(&snapshot, id), None, "{id}");
        }
        assert!(snapshot.notice.expect("notice").contains('4'));
    }

    #[test]
    fn optional_and_unknown_fields_do_not_matter() {
        let mut minimal = entry(REPO, true, 1);
        let map = minimal.as_object_mut().unwrap();
        map.remove("checkout_path");
        map.insert("branch".into(), Value::Null);
        map.insert("future_field".into(), json!({"any": "thing"}));
        let text = json!({"version": 1, "writer": "next", "hosts": {HOST: {"sessions": {"session:x": minimal}, "extra": 1}}});
        let snapshot = applied(&text.to_string(), &["session:x"]);
        assert!(worktree(&snapshot, "session:x").is_some_and(|w| w.is_linked_worktree));
        assert_eq!(snapshot.notice, None);
    }

    #[test]
    fn sessions_that_no_longer_exist_are_ignored() {
        let snapshot = applied(CONTRACT_EXAMPLE, &["session:other"]);
        assert_eq!(snapshot, sessions(&["session:other"]));
    }

    #[test]
    fn the_earliest_bound_main_checkout_is_the_only_parent() {
        let text = file(json!({
            "session:late": entry(REPO, false, 200),
            "session:early": entry(REPO, false, 100),
            "session:linked": entry(REPO, true, 300),
        }));
        let snapshot = applied(&text, &["session:late", "session:early", "session:linked"]);
        assert!(worktree(&snapshot, "session:early").is_some_and(|w| !w.is_linked_worktree));
        assert_eq!(
            worktree(&snapshot, "session:late"),
            None,
            "shown as a plain session"
        );
        let app = tree(snapshot);
        let row = |id: &str| {
            app.rows
                .iter()
                .find(|r| r.key == FocusTarget::Workspace(id.into()).key())
                .unwrap()
        };
        assert!(row("session:early").has_children);
        assert!(!row("session:late").has_children);

        let tie = file(json!({
            "session:first": entry(REPO, false, 100),
            "session:second": entry(REPO, false, 100),
        }));
        let snapshot = applied(&tie, &["session:second", "session:first"]);
        assert!(
            worktree(&snapshot, "session:first").is_some(),
            "the smallest session id breaks ties, whatever the session order"
        );
        assert_eq!(worktree(&snapshot, "session:second"), None);
    }
}
