//! Which blocks wait on which agents, so that `wait` and `prompt --wait` refuse a
//! wait that would close a cycle: the waiting agent stays busy until its wait
//! ends, so an agent that waits on it, directly or through others, would wait
//! until a timeout.
//!
//! Each wait has an entry in `<state>/waits/`, a file naming the waiting block
//! and its target, locked for as long as the wait runs. An unlocked entry was
//! left by a wait whose process was killed; it is ignored and removed.
//! `waits.lock` makes each check and registration one step, so two blocks
//! cannot start waiting on each other at once.

use crate::history::lock_state;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, TryLockError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Numbers this process's entries, which are named `<pid>-<n>.json`.
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct Entry {
    waiter: String,
    target: String,
}

/// A registered wait; dropping it removes its entry.
pub struct Wait {
    path: PathBuf,
    file: File,
}

impl Drop for Wait {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Registers block `waiter` as waiting on block `target`, unless `target`
/// already waits on `waiter`, directly or through other blocks. Then the inner
/// error is that chain of blocks, from `target` to `waiter` (only `waiter` when
/// they are the same block).
pub fn register(state_dir: &Path, waiter: &str, target: &str) -> Result<Result<Wait, Vec<String>>> {
    let dir = state_dir.join("waits");
    let _registering = lock_state(&dir)?;
    if let Some(chain) = chain(&live(&dir)?, target, waiter) {
        return Ok(Err(chain));
    }
    fs::create_dir_all(&dir)?;
    let name = format!(
        "{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = dir.join(name);
    let file = File::create(&path)?;
    let wait = Wait { path, file };
    wait.file.try_lock()?;
    let entry = Entry {
        waiter: waiter.to_owned(),
        target: target.to_owned(),
    };
    serde_json::to_writer(&wait.file, &entry)?;
    Ok(Ok(wait))
}

/// The running waits in `dir`, as waiter → targets. Removes the entries of
/// waits that were killed.
fn live(dir: &Path) -> Result<HashMap<String, Vec<String>>> {
    let mut waits: HashMap<String, Vec<String>> = HashMap::new();
    let entries = match fs::read_dir(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(waits),
        entries => entries?,
    };
    for entry in entries {
        let path = entry?.path();
        // Gone meanwhile: its wait ended.
        let Ok(file) = File::open(&path) else {
            continue;
        };
        match file.try_lock_shared() {
            // Nobody holds it: its wait was killed.
            Ok(()) => {
                let _ = fs::remove_file(&path);
                continue;
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
            Err(TryLockError::WouldBlock) => {}
        }
        if let Ok(Entry { waiter, target }) = serde_json::from_reader(&file) {
            waits.entry(waiter).or_default().push(target);
        }
    }
    Ok(waits)
}

/// The shortest chain of blocks through which `from` waits on `to`, from `from`
/// to `to`.
fn chain(waits: &HashMap<String, Vec<String>>, from: &str, to: &str) -> Option<Vec<String>> {
    let mut reached_from: HashMap<&str, &str> = HashMap::new();
    let mut queue = VecDeque::from([from]);
    while let Some(block) = queue.pop_front() {
        if block == to {
            let mut chain = vec![to.to_owned()];
            let mut at = to;
            while let Some(&before) = reached_from.get(at) {
                chain.push(before.to_owned());
                at = before;
            }
            chain.reverse();
            return Some(chain);
        }
        for next in waits.get(block).into_iter().flatten() {
            if next != from && !reached_from.contains_key(next.as_str()) {
                reached_from.insert(next, block);
                queue.push_back(next);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn wait(dir: &Path, waiter: &str, target: &str) -> Wait {
        register(dir, waiter, target).unwrap().unwrap()
    }

    fn refused(dir: &Path, waiter: &str, target: &str) -> Vec<String> {
        match register(dir, waiter, target).unwrap() {
            Ok(_) => panic!("{waiter} waiting on {target} was registered"),
            Err(chain) => chain,
        }
    }

    fn entries(dir: &Path) -> usize {
        fs::read_dir(dir.join("waits")).map_or(0, |entries| entries.count())
    }

    #[test]
    fn a_wait_on_a_block_that_waits_on_the_waiter_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let _a = wait(dir.path(), "a", "me");
        assert_eq!(refused(dir.path(), "me", "a"), ["a", "me"]);
    }

    #[test]
    fn the_refusal_gives_the_chain_through_which_the_target_waits() {
        let dir = tempfile::tempdir().unwrap();
        let _a = wait(dir.path(), "a", "b");
        let _b = wait(dir.path(), "b", "me");
        assert_eq!(refused(dir.path(), "me", "a"), ["a", "b", "me"]);
    }

    #[test]
    fn a_block_cannot_wait_on_itself() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(refused(dir.path(), "me", "me"), ["me"]);
    }

    #[test]
    fn waits_that_close_no_cycle_are_registered() {
        let dir = tempfile::tempdir().unwrap();
        let _held = [
            wait(dir.path(), "a", "me"),
            wait(dir.path(), "b", "me"),
            wait(dir.path(), "me", "c"),
            wait(dir.path(), "me", "c"),
            wait(dir.path(), "c", "d"),
        ];
        assert_eq!(entries(dir.path()), 5, "one entry per wait");
    }

    #[test]
    fn a_wait_counts_until_it_ends_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let held = wait(dir.path(), "a", "me");
        assert_eq!(refused(dir.path(), "me", "a"), ["a", "me"]);
        drop(held);
        assert_eq!(entries(dir.path()), 0);
        drop(wait(dir.path(), "me", "a"));
        assert_eq!(entries(dir.path()), 0);
    }

    #[test]
    fn an_entry_left_by_a_killed_wait_is_ignored_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let waits = dir.path().join("waits");
        fs::create_dir_all(&waits).unwrap();
        fs::write(waits.join("1-0.json"), r#"{"waiter": "a", "target": "me"}"#).unwrap();
        let _me = wait(dir.path(), "me", "a");
        assert_eq!(entries(dir.path()), 1, "only the new entry is left");
    }

    #[test]
    fn of_two_blocks_starting_to_wait_on_each_other_only_one_does() {
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let barrier = std::sync::Barrier::new(2);
            let results = std::thread::scope(|scope| {
                let [one, two] = [("a", "b"), ("b", "a")].map(|(waiter, target)| {
                    let (dir, barrier) = (dir.path(), &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        register(dir, waiter, target).unwrap()
                    })
                });
                [one.join().unwrap(), two.join().unwrap()]
            });
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        }
    }
}
