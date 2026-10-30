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
    pub fn claim(path: &Path, view: &str) -> Result<Self> {
        let mut active = lock_file(&path.with_extension("active"))?;
        active
            .try_lock_exclusive()
            .context("Navigator is already open")?;
        let token = format!(
            "{view}:{}-{}",
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

pub fn toggle(path: &Path, view: &str, open: impl FnOnce() -> Result<()>) -> Result<bool> {
    change(path, view, true, open)
}

pub fn open(path: &Path, view: &str, spawn: impl FnOnce() -> Result<()>) -> Result<bool> {
    change(path, view, false, spawn)
}

fn change(
    path: &Path,
    view: &str,
    toggle: bool,
    spawn: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    let action = lock_file(&path.with_extension("action.lock"))?;
    action.lock_exclusive()?;
    if is_open(path)? {
        let token = owner(path)?;
        let same_view = token
            .split_once(':')
            .is_none_or(|(active, _)| active == view);
        if !toggle && same_view {
            return Ok(true);
        }
        anyhow::ensure!(!token.is_empty(), "Navigator is still starting");
        fs::write(path.with_extension("close"), token)?;
        wait_until(|| Ok(!is_open(path)?), "Navigator did not close")?;
        if same_view {
            return Ok(false);
        }
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
        fake_view(path, handles, "navigator")
    }

    fn fake_view(
        path: &Path,
        handles: &Mutex<Vec<JoinHandle<()>>>,
        view: &'static str,
    ) -> Result<()> {
        let path = path.to_owned();
        handles.lock().unwrap().push(thread::spawn(move || {
            let popup = PopupState::claim(&path, view).unwrap();
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
    fn changing_modes_replaces_the_popup_and_same_mode_toggle_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let handles = Mutex::new(Vec::new());
        assert!(
            toggle(&path, "navigator", || fake_view(
                &path,
                &handles,
                "navigator"
            ))
            .unwrap()
        );
        assert!(toggle(&path, "agents", || fake_view(&path, &handles, "agents")).unwrap());
        assert!(open(&path, "agents", || panic!("same mode open keeps its owner")).unwrap());
        assert!(
            open(&path, "navigator", || fake_view(
                &path,
                &handles,
                "navigator"
            ))
            .unwrap()
        );
        assert!(!toggle(&path, "navigator", || panic!("same mode toggle closes")).unwrap());
        for handle in handles.into_inner().unwrap() {
            handle.join().unwrap();
        }
        assert!(!is_open(&path).unwrap());
    }

    #[test]
    fn simultaneous_different_modes_share_one_popup_and_serialize_the_switch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let handles = Mutex::new(Vec::new());
        thread::scope(|scope| {
            for view in ["navigator", "agents"] {
                let path = &path;
                let handles = &handles;
                scope.spawn(move || {
                    assert!(toggle(path, view, || fake_view(path, handles, view)).unwrap());
                });
            }
        });
        assert!(is_open(&path).unwrap());
        fs::write(path.with_extension("close"), owner(&path).unwrap()).unwrap();
        let handles = handles.into_inner().unwrap();
        assert_eq!(handles.len(), 2);
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(!is_open(&path).unwrap());
    }

    #[test]
    fn toggle_opens_once_then_closes_the_same_popup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let handles = Mutex::new(Vec::new());
        assert!(toggle(&path, "navigator", || fake_ui(&path, &handles)).unwrap());
        assert!(
            !toggle(&path, "navigator", || panic!(
                "must not open a second popup"
            ))
            .unwrap()
        );
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
                        toggle(&path, "navigator", || {
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
        let first = PopupState::claim(&path, "navigator").unwrap();
        std::fs::write(path.with_extension("close"), &first.token).unwrap();
        drop(first);
        let second = PopupState::claim(&path, "navigator").unwrap();
        assert!(!second.close_requested());
    }

    #[test]
    fn duplicate_ui_cannot_claim_the_active_popup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        let _first = PopupState::claim(&path, "navigator").unwrap();
        assert!(PopupState::claim(&path, "navigator").is_err());
    }

    #[test]
    fn opening_failure_does_not_leave_a_false_open_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        assert!(toggle(&path, "navigator", || anyhow::bail!("cannot open")).is_err());
        assert!(!is_open(&path).unwrap());
    }

    #[test]
    fn reopening_waits_until_the_new_owner_token_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("popup");
        fs::write(path.with_extension("active"), "previous").unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let handle = Mutex::new(None);
        toggle(&path, "navigator", || {
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
