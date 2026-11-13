//! A fake `rex` executable for tests: logs every call and answers from canned files.
//!
//! Embedded scripts start with `-- canopy:<name>`; the fake answers a script by
//! that name, a one-shot call by its method (`do <method> ...`), and anything
//! else by its first non-flag argument (`whoami`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct FakeRex {
    dir: tempfile::TempDir,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
}

impl Call {
    /// The answer key this call was routed by.
    pub fn key(&self) -> String {
        if let Some(stdin) = &self.stdin {
            return stdin
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("-- canopy:"))
                .map(|name| name.trim().to_owned())
                .unwrap_or_default();
        }
        let after_do = self.argv.iter().skip_while(|a| *a != "do").nth(1);
        after_do
            .or_else(|| self.argv.iter().find(|a| !a.starts_with('-')))
            .cloned()
            .unwrap_or_default()
    }
}

const SCRIPT: &str = r#"#!/bin/sh
d="$FAKE_DIR"
i=0
while ! mkdir "$d/calls/$i" 2>/dev/null; do i=$((i+1)); done
c="$d/calls/$i"
for a in "$@"; do printf '%s\n' "$a"; done > "$c/argv"
key=""
prev=""
for a in "$@"; do
  if [ -z "$key" ] && [ "$prev" = "do" ]; then key="$a"; fi
  prev="$a"
done
if [ "$key" = "-" ]; then
  cat > "$c/stdin"
  key=$(head -n 1 "$c/stdin" | sed -n 's/^-- canopy:\([a-z_]*\).*/\1/p')
elif [ -z "$key" ]; then
  for a in "$@"; do case "$a" in -*) ;; *) key="$a"; break;; esac; done
fi
if [ -f "$d/answers/$key.status" ]; then
  if [ -f "$d/answers/$key.out" ]; then cat "$d/answers/$key.out"; fi
  if [ -f "$d/answers/$key.err" ]; then cat "$d/answers/$key.err" >&2; fi
  exit "$(cat "$d/answers/$key.status")"
fi
echo "fake rex: no answer for '$key'" >&2
exit 97
"#;

impl FakeRex {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["calls", "answers"] {
            fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let path = dir.path().join("rex");
        let script = SCRIPT.replacen("\"$FAKE_DIR\"", &format!("'{}'", dir.path().display()), 1);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, path }
    }

    /// Answers calls routed to `key` with `stdout` and exit status 0.
    pub fn answer(&self, key: &str, stdout: &str) {
        self.answer_with(key, stdout, "", 0);
    }

    /// Answers calls routed to `key` with the given output and exit status.
    pub fn answer_with(&self, key: &str, stdout: &str, stderr: &str, status: i32) {
        // Keys such as `session.select` contain dots, so append rather than replace extensions.
        let file = |ext: &str| self.dir.path().join("answers").join(format!("{key}.{ext}"));
        fs::write(file("out"), stdout).unwrap();
        fs::write(file("err"), stderr).unwrap();
        fs::write(file("status"), status.to_string()).unwrap();
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        let calls = self.dir.path().join("calls");
        let mut ids: Vec<usize> = fs::read_dir(&calls)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(|id| read_call(&calls.join(id.to_string())))
            .collect()
    }

    /// The calls routed to `key`.
    pub fn calls_to(&self, key: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.key() == key)
            .collect()
    }
}

fn read_call(dir: &Path) -> Call {
    let argv = fs::read_to_string(dir.join("argv")).unwrap_or_default();
    Call {
        argv: argv.lines().map(String::from).collect(),
        stdin: fs::read_to_string(dir.join("stdin")).ok(),
    }
}
