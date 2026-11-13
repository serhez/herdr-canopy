//! The background agent a Claude Code pane shows. Claude's background agents run
//! in its own daemon, outside any terminal, so a pane viewing one (`claude
//! attach ID`) reports no program status. Claude keeps each agent's state in
//! `<config>/jobs/<id>/state.json` (config: `$CLAUDE_CONFIG_DIR`, else
//! `~/.claude`). That file is Claude's internal format: when it is missing or
//! unreadable, the pane's state is unknown.

use super::decode::parse_time_ms;
use crate::models::AgentStatus;
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A background agent's state, as Claude's job file records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Claude's job state: `working`, `blocked`, `done`, `failed`, `crashed`
    /// or `stopped`.
    pub state: String,
    pub updated_ms: Option<u64>,
    /// What the agent needs from the user, else what it is doing.
    pub message: Option<String>,
}

impl Job {
    /// Canopy's status for the job, and whether it clears to idle once the
    /// user looked at the pane; `None` for a state Canopy does not know.
    pub fn status(&self) -> Option<(AgentStatus, bool)> {
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

/// The job Claude process `pid` shows, from its arguments and Claude's job files.
pub fn viewed_job(pid: u32) -> Option<Job> {
    let jobs = jobs_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    attached_job(&process_args(pid)?, &jobs)
}

/// Claude's jobs folder for its config directory setting and the home folder.
fn jobs_dir(config_dir: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let config = match config_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home?.join(".claude"),
    };
    Some(config.join("jobs"))
}

/// The job a `claude attach ID` process shows. Claude also accepts part of a
/// session name in place of the id; Canopy reads ids only.
fn attached_job(args: &[String], jobs: &Path) -> Option<Job> {
    let id = match args {
        [_, attach, id, ..] if attach == "attach" => id,
        _ => return None,
    };
    let is_id = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    is_id.then(|| read_job(&jobs.join(id).join("state.json")))?
}

/// A job's state file, if it can be read.
fn read_job(path: &Path) -> Option<Job> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StateFile {
        state: String,
        updated_at: Option<String>,
        needs: Option<String>,
        detail: Option<String>,
    }
    let file: StateFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(Job {
        state: file.state,
        updated_ms: file.updated_at.as_deref().and_then(parse_time_ms),
        message: file.needs.or(file.detail),
    })
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

    /// A jobs folder holding one job, `8bb03a8b`, with the given state file.
    fn jobs_with(state_file: &str) -> tempfile::TempDir {
        let jobs = tempfile::tempdir().unwrap();
        fs::create_dir(jobs.path().join("8bb03a8b")).unwrap();
        fs::write(jobs.path().join("8bb03a8b/state.json"), state_file).unwrap();
        jobs
    }

    const WORKING: &str = r#"{"state": "working", "detail": "awaiting a subagent", "tempo": "idle",
        "updatedAt": "2026-11-12T22:14:16.335Z", "fan": [], "cwd": "/repo"}"#;

    fn job(state: &str, updated_ms: Option<u64>, message: Option<&str>) -> Job {
        Job {
            state: state.into(),
            updated_ms,
            message: message.map(String::from),
        }
    }

    #[test]
    fn an_attach_pane_shows_the_job_its_argument_names() {
        let jobs = jobs_with(WORKING);
        let working = job(
            "working",
            Some(1_794_521_656_335),
            Some("awaiting a subagent"),
        );
        for list in [
            ["claude", "attach", "8bb03a8b"],
            ["/opt/homebrew/bin/claude.exe", "attach", "8bb03a8b"],
        ] {
            assert_eq!(
                attached_job(&args(&list), jobs.path()),
                Some(working.clone()),
                "{list:?}"
            );
        }
        let attach = args(&["claude", "attach", "8bb03a8b"]);
        for (file, expected) in [
            (r#"{"state": "blocked"}"#, job("blocked", None, None)),
            (
                r#"{"state": "blocked", "detail": "the plan", "needs": "Should I go ahead?"}"#,
                job("blocked", None, Some("Should I go ahead?")),
            ),
        ] {
            let jobs = jobs_with(file);
            assert_eq!(attached_job(&attach, jobs.path()), Some(expected), "{file}");
        }
    }

    #[test]
    fn an_attach_pane_whose_job_cannot_be_read_shows_no_job() {
        let jobs = jobs_with(WORKING);
        for list in [
            vec!["claude", "attach"],
            // Part of a session name works in place of the id; Canopy only reads ids.
            vec!["claude", "attach", "Ranking"],
            vec!["claude", "attach", "Ranking of experiment variants"],
            vec!["claude", "attach", "../8bb03a8b"],
            vec!["claude", "attach", "8bb03a8b/.."],
            vec!["claude", "attach", ""],
        ] {
            assert_eq!(attached_job(&args(&list), jobs.path()), None, "{list:?}");
        }
        for broken in [
            "",
            "not json",
            r#"{"detail": "no state"}"#,
            r#"{"state": 3}"#,
        ] {
            let jobs = jobs_with(broken);
            assert_eq!(
                attached_job(&args(&["claude", "attach", "8bb03a8b"]), jobs.path()),
                None,
                "{broken:?}"
            );
        }
    }

    #[test]
    fn only_attach_panes_show_a_job() {
        let jobs = jobs_with(WORKING);
        for list in [
            vec!["claude", "agents"],
            vec!["claude"],
            vec!["claude", "--resume", "8bb03a8b"],
            vec!["claude", "-p", "attach", "8bb03a8b"],
            vec!["claude", "--model", "opus", "fix the attach command"],
        ] {
            assert_eq!(attached_job(&args(&list), jobs.path()), None, "{list:?}");
        }
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
            assert_eq!(job(state, None, None).status(), expected, "{state:?}");
        }
    }

    #[test]
    fn the_jobs_folder_is_in_claudes_config_directory() {
        let home = Some(PathBuf::from("/Users/me"));
        assert_eq!(
            jobs_dir(None, home.clone()),
            Some(PathBuf::from("/Users/me/.claude/jobs"))
        );
        assert_eq!(
            jobs_dir(Some("/etc/claude".into()), home.clone()),
            Some(PathBuf::from("/etc/claude/jobs"))
        );
        assert_eq!(
            jobs_dir(Some("".into()), home),
            Some(PathBuf::from("/Users/me/.claude/jobs")),
            "an empty setting is unset"
        );
        assert_eq!(jobs_dir(None, None), None);
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
