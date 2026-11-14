//! The background agent a Claude Code pane shows. Claude's background agents run
//! in its own daemon, outside any terminal, so a pane viewing one reports no
//! program status: `claude attach ID`, the `claude agents` view, and a Claude
//! session the user moved to the background, whose process stays to show it.
//! Claude keeps each agent's state in `<config>/jobs/<id>/state.json` and each
//! of its processes' in `<config>/sessions/<pid>.json` (config:
//! `$CLAUDE_CONFIG_DIR`, else `~/.claude`). Those files are Claude's internal
//! format: when they are missing or unreadable, the pane's state is unknown.

use super::decode::{parse_time_ms, strip_title};
use crate::models::AgentStatus;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The blocked kind of a background agent waiting only on the question or
/// request its last reply ended on: an ordinary prompt answers it.
pub const REPLY: &str = "reply";

/// A background agent's state, as Claude's files record it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Claude's job state, set when a turn ends: `working` (it continues on
    /// its own, such as after background tasks), `blocked` (it asks the user
    /// something), `done`, `failed`, `crashed` or `stopped`.
    pub state: String,
    /// The turn: `active` (running), `blocked` (waiting for the user at a
    /// dialog, or on the question that ended its turn) or `idle`.
    pub tempo: Option<String>,
    pub updated_ms: Option<u64>,
    /// What the agent needs from the user, else what it is doing.
    pub message: Option<String>,
    /// What a blocked agent waits for: [`REPLY`], else unknown.
    pub blocked_kind: Option<&'static str>,
}

impl Job {
    /// Canopy's status for the job, and whether it clears to idle once the
    /// user looked at the pane; `None` for a state Canopy does not know. The
    /// tempo decides while a turn runs or waits for the user, since the state
    /// is the last turn's until the next one ends.
    pub fn status(&self) -> Option<(AgentStatus, bool)> {
        match self.tempo.as_deref() {
            Some("active") => return Some((AgentStatus::Working, false)),
            Some("blocked") => return Some((AgentStatus::Blocked, false)),
            _ => {}
        }
        Some(match self.state.as_str() {
            "working" => (AgentStatus::Working, false),
            "blocked" => (AgentStatus::Blocked, false),
            "done" => (AgentStatus::Done, true),
            "failed" | "crashed" => (AgentStatus::Blocked, true),
            "stopped" => (AgentStatus::Idle, false),
            _ => return None,
        })
    }
}

/// The job Claude process `pid` shows; `title` is its block's title, if the
/// process set it.
pub fn viewed_job(pid: u32, title: Option<&str>) -> Option<Job> {
    let config = config_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    let parked = || {
        let file = config.join("sessions").join(format!("{pid}.json"));
        read_json::<Process>(&file)?.parked_job_id
    };
    shown_job(&process_args(pid)?, parked, title, &config)
}

/// Claude's config directory for its setting and the home folder.
fn config_dir(setting: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    match setting.filter(|dir| !dir.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(home?.join(".claude")),
    }
}

/// The job a Claude process with arguments `args` shows: `claude attach ID`
/// opens job ID, `claude agents` the list of jobs, and a session moved to the
/// background `parked()`, the job its process file names (read for other
/// commands only). Each view can move between jobs and titles its block with
/// the shown job's name, so a `title` decides alone: the one job of that name.
/// Claude also accepts part of a session name in place of the id; Canopy
/// reads ids only.
fn shown_job(
    args: &[String],
    parked: impl FnOnce() -> Option<String>,
    title: Option<&str>,
    config: &Path,
) -> Option<Job> {
    let opened = match args {
        [_, command, rest @ ..] if command == "attach" => rest.first().cloned(),
        [_, command, ..] if command == "agents" => None,
        _ => Some(parked()?),
    };
    let jobs = config.join("jobs");
    let (id, file) = match title {
        Some(title) => named_job(&strip_title(title), &jobs)?,
        None => {
            let id = opened.filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })?;
            let file = read_json(&jobs.join(&id).join("state.json"))?;
            (id, file)
        }
    };
    Some(file.into_job(&id, &config.join("sessions")))
}

/// The only job named `name`, with its id, if exactly one is.
fn named_job(name: &str, jobs: &Path) -> Option<(String, StateFile)> {
    if name.is_empty() {
        return None;
    }
    let mut named = std::fs::read_dir(jobs).ok()?.filter_map(|entry| {
        let entry = entry.ok()?;
        let file: StateFile = read_json(&entry.path().join("state.json"))?;
        if file.name.as_deref() != Some(name) {
            return None;
        }
        Some((entry.file_name().into_string().ok()?, file))
    });
    let job = named.next()?;
    named.next().is_none().then_some(job)
}

/// Whether Claude processes run job `id` and all are idle: none shows a dialog,
/// runs a turn or has background shells. Not when a process file cannot be
/// read, since its process might.
fn at_rest(id: &str, sessions: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return false;
    };
    let mut found = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(process) = read_json::<Process>(&path) else {
            return false;
        };
        if process.job_id.as_deref() == Some(id) {
            if process.status.as_deref() != Some("idle") {
                return false;
            }
            found = true;
        }
    }
    found
}

/// The JSON file at `path`, if it can be read as a `T`.
fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The fields Canopy reads from a job's state file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateFile {
    state: String,
    tempo: Option<String>,
    name: Option<String>,
    updated_at: Option<String>,
    needs: Option<String>,
    detail: Option<String>,
    /// The agent's background work (`tasks`: shells, subagents, monitors,
    /// scheduled wake-ups) and messages waiting to start a turn (`queued`).
    /// Read loosely, since a status must not depend on it.
    in_flight: Option<serde_json::Value>,
}

impl StateFile {
    /// The job `id`, whose processes' files are in `sessions`.
    fn into_job(self, id: &str, sessions: &Path) -> Job {
        let mut job = Job {
            state: self.state,
            tempo: self.tempo,
            updated_ms: self.updated_at.as_deref().and_then(parse_time_ms),
            message: self.needs.or(self.detail),
            blocked_kind: None,
        };
        // Claude marks a job blocked both at a dialog, where typed text could
        // pick an option, and once its last reply ended on a question; only
        // then are its processes at rest. Background work could open a dialog
        // without a turn of the agent's own, and a queued message start one.
        let no_work = self.in_flight.is_some_and(|work| {
            ["tasks", "queued"]
                .iter()
                .all(|count| work[count].as_f64() == Some(0.0))
        });
        if job.state == "blocked"
            && job.status() == Some((AgentStatus::Blocked, false))
            && no_work
            && at_rest(id, sessions)
        {
            job.blocked_kind = Some(REPLY);
        }
        job
    }
}

/// The fields Canopy reads from a Claude process's file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Process {
    /// The job a background agent's process runs.
    job_id: Option<String>,
    /// The job a session moved to the background became.
    parked_job_id: Option<String>,
    /// `idle`, `busy` (a turn runs), `waiting` (a dialog is open), `shell`
    /// (idle with background shells), …
    status: Option<String>,
}

/// A process's arguments, `argv[0]` first.
#[cfg(target_os = "macos")]
fn process_args(pid: u32) -> Option<Vec<String>> {
    use std::ptr::null_mut;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query: no output buffer, and `size` is a valid size_t.
    let asked = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, null_mut(), &mut size, null_mut(), 0) };
    if asked != 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: `buffer` holds `size` writable bytes; the kernel sets `size` to what it wrote.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buffer.truncate(size);
    parse_procargs(&buffer)
}

/// Splits a `KERN_PROCARGS2` buffer: `argc` as a native int, the executable
/// path, NUL padding, then `argc` NUL-terminated arguments (then the environment).
#[cfg(any(target_os = "macos", test))]
fn parse_procargs(buffer: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    let rest = &buffer[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let mut at = path_end + rest[path_end..].iter().position(|&b| b != 0)?;
    (0..usize::try_from(argc).ok()?)
        .map(|_| {
            let end = at + rest[at..].iter().position(|&b| b == 0)?;
            let arg = String::from_utf8_lossy(&rest[at..end]).into_owned();
            at = end + 1;
            Some(arg)
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn process_args(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
    Some(
        raw.split(|&b| b == 0)
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_args(_pid: u32) -> Option<Vec<String>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    /// A Claude config directory holding the given jobs, as (id, state file)
    /// pairs, and process files, as (file name, contents) pairs.
    fn config(jobs: &[(&str, &str)], processes: &[(&str, &str)]) -> tempfile::TempDir {
        let config = tempfile::tempdir().unwrap();
        for (id, state_file) in jobs {
            let dir = config.path().join("jobs").join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state.json"), state_file).unwrap();
        }
        let sessions = config.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        for (name, contents) in processes {
            fs::write(sessions.join(name), contents).unwrap();
        }
        config
    }

    fn jobs(files: &[(&str, &str)]) -> tempfile::TempDir {
        config(files, &[])
    }

    /// A config directory holding one job, `8bb03a8b`, with the given state file.
    fn jobs_with(state_file: &str) -> tempfile::TempDir {
        jobs(&[("8bb03a8b", state_file)])
    }

    /// The job shown by a process with arguments `list`, which is not a
    /// session moved to the background.
    fn shown(list: &[&str], title: Option<&str>, config: &Path) -> Option<Job> {
        shown_job(&args(list), || None, title, config)
    }

    const WORKING: &str = r#"{"state": "working", "detail": "awaiting a subagent", "tempo": "idle",
        "name": "Ranking of experiment variants", "updatedAt": "2026-11-12T22:14:16.335Z",
        "fan": [], "cwd": "/repo"}"#;

    const PLAN: &str = r#"{"state": "blocked", "tempo": "blocked", "name": "Pilot plan",
        "needs": "Should I go ahead?", "detail": "Should I go ahead?"}"#;

    fn job(
        state: &str,
        tempo: Option<&str>,
        updated_ms: Option<u64>,
        message: Option<&str>,
    ) -> Job {
        Job {
            state: state.into(),
            tempo: tempo.map(String::from),
            updated_ms,
            message: message.map(String::from),
            blocked_kind: None,
        }
    }

    fn working() -> Job {
        job(
            "working",
            Some("idle"),
            Some(1_794_521_656_335),
            Some("awaiting a subagent"),
        )
    }

    fn plan() -> Job {
        job("blocked", Some("blocked"), None, Some("Should I go ahead?"))
    }

    /// The file of a background agent's process running `job`.
    fn worker(job: &str, status: &str) -> String {
        format!(
            r#"{{"pid": 60649, "sessionId": "1364", "kind": "bg", "jobId": "{job}",
                "status": "{status}", "name": "Pilot plan"}}"#
        )
    }

    #[test]
    fn an_attach_pane_shows_the_job_its_argument_names() {
        let config = jobs_with(WORKING);
        for list in [
            ["claude", "attach", "8bb03a8b"],
            ["/opt/homebrew/bin/claude.exe", "attach", "8bb03a8b"],
        ] {
            assert_eq!(
                shown(&list, None, config.path()),
                Some(working()),
                "{list:?}"
            );
        }
        let attach = ["claude", "attach", "8bb03a8b"];
        for (file, expected) in [
            (r#"{"state": "blocked"}"#, job("blocked", None, None, None)),
            (
                r#"{"state": "blocked", "detail": "the plan", "needs": "Should I go ahead?"}"#,
                job("blocked", None, None, Some("Should I go ahead?")),
            ),
        ] {
            let config = jobs_with(file);
            assert_eq!(
                shown(&attach, None, config.path()),
                Some(expected),
                "{file}"
            );
        }
    }

    #[test]
    fn an_attach_pane_whose_job_cannot_be_read_shows_no_job() {
        let config = jobs_with(WORKING);
        for list in [
            vec!["claude", "attach"],
            // Part of a session name works in place of the id; Canopy only reads ids.
            vec!["claude", "attach", "Ranking"],
            vec!["claude", "attach", "Ranking of experiment variants"],
            vec!["claude", "attach", "../8bb03a8b"],
            vec!["claude", "attach", "8bb03a8b/.."],
            vec!["claude", "attach", ""],
        ] {
            assert_eq!(shown(&list, None, config.path()), None, "{list:?}");
        }
        for broken in [
            "",
            "not json",
            r#"{"detail": "no state"}"#,
            r#"{"state": 3}"#,
        ] {
            let config = jobs_with(broken);
            assert_eq!(
                shown(&["claude", "attach", "8bb03a8b"], None, config.path()),
                None,
                "{broken:?}"
            );
        }
    }

    #[test]
    fn an_agents_pane_shows_the_job_its_title_names() {
        let config = jobs(&[("8bb03a8b", WORKING), ("17f1a30e", PLAN)]);
        let agents = ["claude", "agents"];
        for (title, expected) in [
            ("Pilot plan", Some(plan())),
            // Claude puts its activity glyph before the name while it shows a job.
            ("✳ Pilot plan", Some(plan())),
            ("◑ Ranking of experiment variants", Some(working())),
            // The agents list itself.
            ("1 awaiting input · claude agents", None),
            ("Pilot", None),
            ("", None),
        ] {
            assert_eq!(
                shown(&agents, Some(title), config.path()),
                expected,
                "{title:?}"
            );
        }
        assert_eq!(shown(&agents, None, config.path()), None, "no title");
        let with_options = ["claude", "agents", "--cwd", "/repo"];
        assert_eq!(
            shown(&with_options, Some("Pilot plan"), config.path()),
            Some(plan())
        );
    }

    #[test]
    fn a_title_decides_the_shown_job_alone() {
        // The view can move from the job it opened to another one, or to the list.
        let config = jobs(&[("8bb03a8b", WORKING), ("17f1a30e", PLAN)]);
        let attach = ["claude", "attach", "8bb03a8b"];
        assert_eq!(
            shown(&attach, Some("✳ Pilot plan"), config.path()),
            Some(plan())
        );
        assert_eq!(
            shown(
                &attach,
                Some("1 awaiting input · claude agents"),
                config.path()
            ),
            None
        );
        assert_eq!(
            shown(&attach, None, config.path()),
            Some(working()),
            "without a title, the job it opened"
        );
    }

    #[test]
    fn a_title_that_names_several_jobs_names_none() {
        let config = jobs(&[
            ("8bb03a8b", WORKING),
            ("17f1a30e", PLAN),
            ("3a235eae", PLAN),
        ]);
        for list in [&["claude", "agents"][..], &["claude", "attach", "8bb03a8b"]] {
            assert_eq!(
                shown(list, Some("Pilot plan"), config.path()),
                None,
                "{list:?}"
            );
        }
    }

    #[test]
    fn unreadable_jobs_do_not_hide_the_job_a_title_names() {
        let config = jobs(&[("8bb03a8b", "not json"), ("17f1a30e", PLAN)]);
        let jobs = config.path().join("jobs");
        fs::create_dir(jobs.join("2d72cc03")).unwrap();
        fs::write(jobs.join("pins.json"), "[]").unwrap();
        assert_eq!(
            shown(&["claude", "agents"], Some("Pilot plan"), config.path()),
            Some(plan())
        );
    }

    #[test]
    fn a_session_moved_to_the_background_shows_its_job() {
        // Its process stays, showing the job, and its file names the job.
        let config = jobs(&[("8bb03a8b", WORKING), ("17f1a30e", PLAN)]);
        let resumed = args(&["claude", "--resume", "849245af"]);
        let parked = |id: &str| {
            let id = id.to_string();
            move || Some(id)
        };
        assert_eq!(
            shown_job(&resumed, parked("8bb03a8b"), None, config.path()),
            Some(working())
        );
        assert_eq!(
            shown_job(
                &resumed,
                parked("8bb03a8b"),
                Some("◑ Pilot plan"),
                config.path()
            ),
            Some(plan()),
            "a title decides"
        );
        for id in ["../8bb03a8b", "", "2d72cc03"] {
            assert_eq!(
                shown_job(&resumed, parked(id), None, config.path()),
                None,
                "{id:?}"
            );
        }
    }

    #[test]
    fn only_job_views_and_sessions_moved_to_the_background_show_a_job() {
        let config = jobs(&[("8bb03a8b", WORKING), ("17f1a30e", PLAN)]);
        for list in [
            vec!["claude"],
            vec!["claude", "--resume", "8bb03a8b"],
            vec!["claude", "-p", "attach", "8bb03a8b"],
            vec!["claude", "-p", "agents"],
            vec!["claude", "--model", "opus", "fix the attach command"],
        ] {
            assert_eq!(
                shown(&list, Some("Pilot plan"), config.path()),
                None,
                "{list:?}"
            );
        }
        let unread = || -> Option<String> { panic!("a job view's process file was read") };
        for list in [&["claude", "agents"][..], &["claude", "attach", "8bb03a8b"]] {
            shown_job(&args(list), unread, Some("Pilot plan"), config.path());
        }
    }

    #[test]
    fn process_files_name_the_job_a_process_runs_or_moved_to() {
        let read = |contents: &str| {
            let config = config(&[], &[("65434.json", contents)]);
            read_json::<Process>(&config.path().join("sessions/65434.json"))
                .map(|p| (p.job_id, p.parked_job_id, p.status))
        };
        assert_eq!(
            read(
                r#"{"pid": 65434, "sessionId": "849245af", "kind": "interactive", "name": "config-af",
                    "status": "busy", "parkedJobId": "15e317d6"}"#
            ),
            Some((None, Some("15e317d6".into()), Some("busy".into())))
        );
        assert_eq!(
            read(&worker("15e317d6", "idle")),
            Some((Some("15e317d6".into()), None, Some("idle".into())))
        );
        assert_eq!(read("not json"), None);
    }

    /// The blocked kind of job `8bb03a8b`, with state file `state_file`, for
    /// the given process files.
    fn blocked_kind(state_file: &str, processes: &[(&str, &str)]) -> Option<&'static str> {
        let config = config(&[("8bb03a8b", state_file)], processes);
        shown(&["claude", "attach", "8bb03a8b"], None, config.path())
            .unwrap()
            .blocked_kind
    }

    /// A job whose last reply ended on a question, with no background work.
    const QUESTION: &str = r#"{"state": "blocked", "tempo": "blocked", "name": "Pilot plan",
        "needs": "Should I go ahead?", "inFlight": {"tasks": 0, "queued": 0, "kinds": []}}"#;

    #[test]
    fn a_job_whose_reply_ended_on_a_question_waits_for_a_prompt() {
        // Its turn ended and no background work runs, so nothing can open a
        // dialog before a prompt starts a turn.
        let idle = worker("8bb03a8b", "idle");
        let others = worker("17f1a30e", "waiting");
        let parked = r#"{"pid": 65434, "kind": "interactive", "status": "busy",
                         "parkedJobId": "8bb03a8b"}"#;
        for processes in [
            vec![("60649.json", idle.as_str())],
            vec![("60649.json", idle.as_str()), ("60650.json", idle.as_str())],
            vec![
                ("60649.json", idle.as_str()),
                ("94957.json", others.as_str()),
            ],
            vec![("60649.json", idle.as_str()), ("65434.json", parked)],
            vec![
                ("60649.json", idle.as_str()),
                ("60649.0da5f5ce.key", "not json"),
            ],
        ] {
            assert_eq!(
                blocked_kind(QUESTION, &processes),
                Some(REPLY),
                "{processes:?}"
            );
        }
        let untimed = r#"{"state": "blocked", "inFlight": {"tasks": 0, "queued": 0, "kinds": []}}"#;
        assert_eq!(
            blocked_kind(untimed, &[("60649.json", &idle)]),
            Some(REPLY),
            "without a tempo, the state decides"
        );
    }

    #[test]
    fn a_job_at_a_dialog_or_whose_processes_cannot_be_read_waits_for_something_unknown() {
        let idle = worker("8bb03a8b", "idle");
        let waiting = worker("8bb03a8b", "waiting");
        let busy = worker("8bb03a8b", "busy");
        let no_status = r#"{"pid": 60649, "kind": "bg", "jobId": "8bb03a8b"}"#;
        for processes in [
            vec![("60649.json", waiting.as_str())],
            vec![("60649.json", busy.as_str())],
            vec![("60649.json", no_status)],
            vec![],
            vec![
                ("60649.json", idle.as_str()),
                ("60650.json", waiting.as_str()),
            ],
            vec![("60649.json", idle.as_str()), ("60650.json", "{\"pid\": 6")],
        ] {
            assert_eq!(blocked_kind(QUESTION, &processes), None, "{processes:?}");
        }
        for state_file in [
            // A dialog during a turn that follows a working one.
            r#"{"state": "working", "tempo": "blocked", "inFlight": {"tasks": 0, "queued": 0}}"#,
            // A prompt arrived: the turn runs.
            r#"{"state": "blocked", "tempo": "active", "inFlight": {"tasks": 0, "queued": 0}}"#,
            r#"{"state": "failed", "tempo": "idle", "inFlight": {"tasks": 0, "queued": 0}}"#,
            r#"{"state": "done", "inFlight": {"tasks": 0, "queued": 0}}"#,
        ] {
            assert_eq!(
                blocked_kind(state_file, &[("60649.json", &idle)]),
                None,
                "{state_file}"
            );
        }
        let config = jobs_with(QUESTION);
        fs::remove_dir(config.path().join("sessions")).unwrap();
        let shown = shown(&["claude", "attach", "8bb03a8b"], None, config.path());
        assert_eq!(shown.unwrap().blocked_kind, None, "no process files");
    }

    #[test]
    fn a_job_with_background_work_waits_for_something_unknown() {
        // Background shells, subagents and monitors can open a dialog (network
        // access, a subagent's permission) with no turn of the agent's own.
        let with_in_flight = |in_flight: &str| {
            format!(r#"{{"state": "blocked", "tempo": "blocked", "inFlight": {in_flight}}}"#)
        };
        let idle = worker("8bb03a8b", "idle");
        for state_file in [
            with_in_flight(r#"{"tasks": 1, "queued": 0, "kinds": ["local_bash"]}"#),
            with_in_flight(r#"{"tasks": 2, "queued": 0, "kinds": ["local_agent", "monitor"]}"#),
            // A message waits to start a turn.
            with_in_flight(r#"{"tasks": 0, "queued": 1, "kinds": []}"#),
            with_in_flight(r#"{"tasks": "none", "queued": 0}"#),
            with_in_flight(r#"{"queued": 0}"#),
            with_in_flight("null"),
            r#"{"state": "blocked", "tempo": "blocked"}"#.to_string(),
        ] {
            let config = config(&[("8bb03a8b", &state_file)], &[("60649.json", &idle)]);
            let shown = shown(&["claude", "attach", "8bb03a8b"], None, config.path());
            assert_eq!(
                shown.map(|job| (job.status(), job.blocked_kind)),
                Some((Some((AgentStatus::Blocked, false)), None)),
                "{state_file}"
            );
        }
        let shell = worker("8bb03a8b", "shell");
        assert_eq!(
            blocked_kind(QUESTION, &[("60649.json", &shell)]),
            None,
            "idle with background shells"
        );
    }

    #[test]
    fn job_states_map_to_canopy_states() {
        for (state, expected) in [
            ("working", Some((AgentStatus::Working, false))),
            ("blocked", Some((AgentStatus::Blocked, false))),
            ("done", Some((AgentStatus::Done, true))),
            ("failed", Some((AgentStatus::Blocked, true))),
            ("crashed", Some((AgentStatus::Blocked, true))),
            ("stopped", Some((AgentStatus::Idle, false))),
            ("something-new", None),
            ("", None),
        ] {
            for tempo in [None, Some("idle"), Some("something-new")] {
                assert_eq!(
                    job(state, tempo, None, None).status(),
                    expected,
                    "{state:?} {tempo:?}"
                );
            }
        }
    }

    #[test]
    fn a_running_turn_or_a_wait_for_the_user_decides_over_the_job_state() {
        // Claude sets the job state when a turn ends and keeps it through the next
        // turn; the tempo follows the turn: a prompt makes it active, a dialog
        // (permission, question, plan approval) blocked.
        for state in [
            "working",
            "blocked",
            "done",
            "failed",
            "stopped",
            "something-new",
        ] {
            assert_eq!(
                job(state, Some("active"), None, None).status(),
                Some((AgentStatus::Working, false)),
                "{state:?}"
            );
            assert_eq!(
                job(state, Some("blocked"), None, None).status(),
                Some((AgentStatus::Blocked, false)),
                "{state:?}"
            );
        }
    }

    #[test]
    fn claudes_config_directory_is_its_setting_else_in_the_home_folder() {
        let home = Some(PathBuf::from("/Users/me"));
        assert_eq!(
            config_dir(None, home.clone()),
            Some(PathBuf::from("/Users/me/.claude"))
        );
        assert_eq!(
            config_dir(Some("/etc/claude".into()), home.clone()),
            Some(PathBuf::from("/etc/claude"))
        );
        assert_eq!(
            config_dir(Some("".into()), home),
            Some(PathBuf::from("/Users/me/.claude")),
            "an empty setting is unset"
        );
        assert_eq!(config_dir(None, None), None);
    }

    #[test]
    fn procargs_buffers_split_into_the_arguments() {
        let buffer = |argc: i32, body: &[u8]| [&argc.to_ne_bytes()[..], body].concat();
        let full = buffer(
            3,
            b"/opt/bin/claude.exe\0\0\0\0claude\0attach\0\08bb03a8b\0HOME=/Users/me\0",
        );
        assert_eq!(
            parse_procargs(&full),
            Some(args(&["claude", "attach", ""])),
            "an empty argument counts"
        );
        let plain = buffer(2, b"/bin/claude\0claude\0agents\0TERM=xterm\0");
        assert_eq!(parse_procargs(&plain), Some(args(&["claude", "agents"])));
        for broken in [
            buffer(3, b"/bin/claude\0claude\0agents\0"),
            buffer(1, b"/bin/claude"),
            buffer(-1, b"/bin/claude\0claude\0"),
            vec![1, 0],
        ] {
            assert_eq!(parse_procargs(&broken), None, "{broken:?}");
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_process_reads_its_own_arguments() {
        let own: Vec<String> = std::env::args().collect();
        assert_eq!(process_args(std::process::id()), Some(own));
        assert_eq!(process_args(u32::MAX), None, "no such process");
    }
}
