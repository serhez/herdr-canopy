//! Calls to the `rex` CLI. Every call passes `--autostart=false`, so Canopy never
//! starts a Rex server.

use anyhow::{Context, Result};
use serde_json::Value;
use std::ffi::OsStr;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rex {
    pub bin: PathBuf,
}

impl Rex {
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    /// The binary to run: `explicit`, else `$REX_BIN_DIR/rex`, else `rex` on `PATH`.
    pub fn resolve(explicit: Option<PathBuf>, rex_bin_dir: Option<&OsStr>) -> Self {
        let bin = explicit
            .or_else(|| {
                rex_bin_dir
                    .filter(|dir| !dir.is_empty())
                    .map(|dir| PathBuf::from(dir).join("rex"))
            })
            .unwrap_or_else(|| PathBuf::from("rex"));
        Self::new(bin)
    }

    /// A `rex` command that never starts a server.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        command.arg("--autostart=false");
        command
    }

    /// Runs an embedded Lua `script` with `args` as its `rex.args` and returns the
    /// JSON it prints.
    pub fn script(&self, script: &str, args: &Value) -> Result<Value> {
        let mut child = self
            .command()
            .args(["do", "-", "--args", &args.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Cannot run {}", self.bin.display()))?;
        // Dropping stdin after the write closes it, so `rex` sees the whole script.
        let written = child
            .stdin
            .take()
            .context("rex stdin is missing")?
            .write_all(script.as_bytes());
        let output = child.wait_with_output()?;
        let output = checked(output, script_name(script))?;
        written.context("Cannot send the script to rex")?;
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("Invalid output from rex script {}", script_name(script)))
    }

    /// Runs `rex ARGS…` and returns what it prints.
    pub fn output(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("Cannot run {}", self.bin.display()))?;
        let output = checked(output, args.first().copied().unwrap_or_default())?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Asks connected app `client` to perform `action` once; never retried.
    pub fn client_action(&self, client: &str, action: &str, args: &[(&str, &str)]) -> Result<()> {
        let output = self
            .command()
            .args(["-C", client, "do", action])
            .args(args.iter().map(|(key, value)| format!("{key}={value}")))
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("Cannot run {}", self.bin.display()))?;
        checked(output, action).map(|_| ())
    }
}

/// The `-- canopy:<name>` header of an embedded script, for error messages.
fn script_name(script: &str) -> &str {
    script
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("-- canopy:"))
        .unwrap_or("script")
        .trim()
}

fn checked(output: Output, what: &str) -> Result<Output> {
    anyhow::ensure!(
        output.status.success(),
        "rex {what} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::rex::testing::FakeRex;
    use serde_json::json;

    const SCRIPT: &str = "-- canopy:probe\nprint(rex.json.encode(rex.args))\n";

    #[test]
    fn scripts_are_piped_with_json_args_and_never_start_a_server() {
        let fake = FakeRex::new();
        fake.answer("probe", r#"{"ok":true,"items":{}}"#);
        let rex = Rex::new(fake.path.clone());
        let args = json!({"target": "block:1", "n": 2});
        assert_eq!(
            rex.script(SCRIPT, &args).unwrap(),
            json!({"ok": true, "items": {}})
        );
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.argv[0], "--autostart=false");
        assert_eq!(call.stdin.as_deref(), Some(SCRIPT));
        let at = call.argv.iter().position(|a| a == "--args").unwrap();
        let passed: Value = serde_json::from_str(&call.argv[at + 1]).unwrap();
        assert_eq!(passed, args);
    }

    #[test]
    fn failed_or_garbled_scripts_are_errors_that_carry_rex_stderr() {
        let fake = FakeRex::new();
        let rex = Rex::new(fake.path.clone());
        fake.answer_with("probe", "", "server is not running", 1);
        let error = rex.script(SCRIPT, &json!({})).unwrap_err();
        assert!(format!("{error:#}").contains("server is not running"));
        fake.answer("probe", "not json");
        assert!(rex.script(SCRIPT, &json!({})).is_err());
        let missing = Rex::new(fake.path.with_file_name("missing-rex"));
        assert!(missing.script(SCRIPT, &json!({})).is_err());
    }

    #[test]
    fn client_actions_name_the_client_and_are_sent_once() {
        let fake = FakeRex::new();
        let rex = Rex::new(fake.path.clone());
        fake.answer("session.select", "");
        rex.client_action(
            "client:app",
            "session.select",
            &[("session_id", "session:b"), ("window_id", "window:b1")],
        )
        .unwrap();
        fake.answer_with("session.select", "", "403 Remote Control is off", 1);
        let error = rex
            .client_action(
                "client:app",
                "session.select",
                &[("session_id", "session:b")],
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("403"));
        let calls = fake.calls_to("session.select");
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].argv,
            [
                "--autostart=false",
                "-C",
                "client:app",
                "do",
                "session.select",
                "session_id=session:b",
                "window_id=window:b1",
            ]
        );
    }

    #[test]
    fn commands_return_what_rex_prints_or_an_error_with_its_stderr() {
        let fake = FakeRex::new();
        let rex = Rex::new(fake.path.clone());
        fake.answer("capture", "line 1\nline 2\n");
        assert_eq!(
            rex.output(&["capture", "-s", "session:a", "-b", "block:a1"])
                .unwrap(),
            "line 1\nline 2\n"
        );
        assert_eq!(
            fake.calls()[0].argv,
            [
                "--autostart=false",
                "capture",
                "-s",
                "session:a",
                "-b",
                "block:a1"
            ]
        );
        fake.answer_with("capture", "", "Error: block not found", 1);
        let error = rex.output(&["capture", "-b", "block:gone"]).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "rex capture failed: Error: block not found"
        );
    }

    #[test]
    fn the_rex_binary_is_explicit_then_rex_bin_dir_then_path() {
        let explicit = PathBuf::from("/opt/rex/bin/rex");
        assert_eq!(
            Rex::resolve(Some(explicit.clone()), Some(OsStr::new("/apps/rex"))).bin,
            explicit
        );
        assert_eq!(
            Rex::resolve(None, Some(OsStr::new("/apps/rex"))).bin,
            PathBuf::from("/apps/rex/rex")
        );
        assert_eq!(
            Rex::resolve(None, Some(OsStr::new(""))).bin,
            PathBuf::from("rex")
        );
        assert_eq!(Rex::resolve(None, None).bin, PathBuf::from("rex"));
    }
}
