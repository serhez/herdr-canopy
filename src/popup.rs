use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The popup process holds this lock for its entire lifetime.
pub struct PopupState {
    _active: File,
    close_path: std::path::PathBuf,
    token: String,
}

fn lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?)
}

impl PopupState {
    pub fn claim(path: &Path) -> Result<Self> {
        let mut active = lock_file(&path.with_extension("active"))?;
        active
            .try_lock_exclusive()
            .context("Navigator is already open")?;
        let token = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        active.set_len(0)?;
        active.seek(SeekFrom::Start(0))?;
        active.write_all(token.as_bytes())?;
        active.flush()?;
        Ok(Self {
            _active: active,
            close_path: path.with_extension("close"),
            token,
        })
    }

    pub fn close_requested(&self) -> bool {
        fs::read_to_string(&self.close_path).ok().as_deref() == Some(self.token.as_str())
    }
}

pub fn is_open(path: &Path) -> Result<bool> {
    let active = lock_file(&path.with_extension("active"))?;
    match active.try_lock_exclusive() {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn owner(path: &Path) -> Result<String> {
    let mut text = String::new();
    File::open(path.with_extension("active"))?
        .take(128)
        .read_to_string(&mut text)?;
    Ok(text)
}

fn wait_until(mut ready: impl FnMut() -> Result<bool>, message: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready()? {
        anyhow::ensure!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

pub fn toggle(path: &Path, open: impl FnOnce() -> Result<()>) -> Result<bool> {
    change(path, true, open)
}

pub fn open(path: &Path, spawn: impl FnOnce() -> Result<()>) -> Result<bool> {
    change(path, false, spawn)
}

fn change(path: &Path, toggle: bool, spawn: impl FnOnce() -> Result<()>) -> Result<bool> {
    let action = lock_file(&path.with_extension("action.lock"))?;
    action.lock_exclusive()?;
    if is_open(path)? {
        if !toggle {
            return Ok(true);
        }
        let token = owner(path)?;
        anyhow::ensure!(!token.is_empty(), "Navigator is still starting");
        fs::write(path.with_extension("close"), token)?;
        wait_until(|| Ok(!is_open(path)?), "Navigator did not close")?;
        return Ok(false);
    }
    let previous = owner(path).unwrap_or_default();
    spawn()?;
    wait_until(
        || {
            let token = owner(path)?;
            // A new token proves the popup has claimed the lock; probing sooner can block startup.
            Ok(!token.is_empty() && token != previous && is_open(path)?)
        },
        "Navigator did not start",
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    fn fake_ui(path: &Path, handles: &Mutex<Vec<JoinHandle<()>>>) -> Result<()> {
        let path = path.to_owned();
        handles.lock().unwrap().push(thread::spawn(move || {
            let popup = PopupState::claim(&path).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !popup.close_requested() {
                assert!(
                    Instant::now() < deadline,
                    "popup never received close request"
                );
                thread::sleep(Duration::from_millis(2));
            }
        }));
        Ok(())
    }

    #[test]
    fn toggle_opens_once_then_closes_the_same_popup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let handles = Mutex::new(Vec::new());
        assert!(toggle(&path, || fake_ui(&path, &handles)).unwrap());
        assert!(!toggle(&path, || panic!("must not open a second popup")).unwrap());
        for handle in handles.into_inner().unwrap() {
            handle.join().unwrap();
        }
        assert!(!is_open(&path).unwrap());
    }

    #[test]
    fn simultaneous_toggles_do_not_spawn_two_popups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let handles = Arc::new(Mutex::new(Vec::new()));
        let opens = Arc::new(AtomicUsize::new(0));
        for round in 0..20 {
            thread::scope(|scope| {
                for _ in 0..2 {
                    scope.spawn(|| {
                        toggle(&path, || {
                            opens.fetch_add(1, Ordering::SeqCst);
                            fake_ui(&path, &handles)
                        })
                        .unwrap()
                    });
                }
            });
            assert_eq!(opens.load(Ordering::SeqCst), round + 1);
            for handle in handles.lock().unwrap().drain(..) {
                handle.join().unwrap();
            }
            assert!(!is_open(&path).unwrap());
        }
    }

    #[test]
    fn stale_close_request_cannot_close_a_new_popup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let first = PopupState::claim(&path).unwrap();
        std::fs::write(path.with_extension("close"), &first.token).unwrap();
        drop(first);
        let second = PopupState::claim(&path).unwrap();
        assert!(!second.close_requested());
    }

    #[test]
    fn duplicate_ui_cannot_claim_the_active_popup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let _first = PopupState::claim(&path).unwrap();
        assert!(PopupState::claim(&path).is_err());
    }

    #[test]
    fn opening_failure_does_not_leave_a_false_open_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        assert!(toggle(&path, || anyhow::bail!("cannot open")).is_err());
        assert!(!is_open(&path).unwrap());
    }

    #[test]
    fn reopening_waits_until_the_new_owner_token_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        fs::write(path.with_extension("active"), "previous").unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let handle = Mutex::new(None);
        toggle(&path, || {
            let path = path.clone();
            *handle.lock().unwrap() = Some(thread::spawn(move || {
                let mut active = lock_file(&path.with_extension("active")).unwrap();
                active.lock_exclusive().unwrap();
                active.set_len(0).unwrap();
                ready_tx.send(()).unwrap();
                thread::sleep(Duration::from_millis(40));
                active.write_all(b"next").unwrap();
                let popup = PopupState {
                    _active: active,
                    close_path: path.with_extension("close"),
                    token: "next".into(),
                };
                let deadline = Instant::now() + Duration::from_secs(2);
                while !popup.close_requested() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(2));
                }
            }));
            ready_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
        let token = owner(&path).unwrap();
        // Always let the fixture exit, including on a regression.
        fs::write(path.with_extension("close"), "next").unwrap();
        handle.into_inner().unwrap().unwrap().join().unwrap();
        assert_eq!(token, "next");
    }
}
