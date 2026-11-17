//! Which process builds a project (#1970). A run holds an OS lock on `inbox/build.lock` for its
//! whole life, and the system releases it however the process ends (finished, stopped, killed or
//! crashed). So a project whose lock nobody holds has no build running, whatever its saved status
//! says, and one whose lock is held is building somewhere: in this process, another window or the
//! command line.

use super::Workspace;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The lock's file name, in the project's inbox/ (one of Craft Anything's own records, which the
/// builder leaves alone).
pub const BUILD_LOCK: &str = "build.lock";

/// How often, and how long apart, [`BuildLock::take`] tries a lock that's busy: [`held`] takes it
/// for a moment to look, and a run starting at that moment must not be refused for it.
const TRIES: u32 = 10;
const PAUSE: Duration = Duration::from_millis(20);

/// The project's build lock, held until dropped.
#[derive(Debug)]
pub struct BuildLock {
    _file: File,
}

impl BuildLock {
    /// Take `ws`'s lock for a build. `Ok(None)`: a build holds it already, here or in another
    /// process. The file is never read or removed; it only carries the lock.
    pub fn take(ws: &Workspace) -> std::io::Result<Option<BuildLock>> {
        let path = path(&ws.root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        for i in 0..TRIES {
            match file.try_lock() {
                Ok(()) => return Ok(Some(BuildLock { _file: file })),
                Err(TryLockError::WouldBlock) if i + 1 < TRIES => std::thread::sleep(PAUSE),
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
        Ok(None)
    }
}

/// Where the project at `root` keeps its build lock.
pub fn path(root: &Path) -> PathBuf {
    root.join("inbox").join(BUILD_LOCK)
}

/// A build holds the project's lock now, in this process or another. It reads the disk, so call it
/// on a worker, never while drawing. A project with no lock file, or one that can't be opened, has
/// no build holding it.
pub fn held(root: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).open(path(root)) else { return false };
    // Shared, so two looks at once don't count each other as a build.
    matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::TargetRef;

    fn project() -> (tempfile::TempDir, Workspace) {
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Notes", "A notes app", TargetRef::None).unwrap();
        (d, ws)
    }

    #[test]
    fn a_build_holds_its_project_until_it_ends() {
        let (_d, ws) = project();
        assert!(!held(&ws.root), "no build yet");
        let lock = BuildLock::take(&ws).unwrap().expect("free");
        assert!(held(&ws.root));
        // A second build of the same project is refused while the first holds it, even in the
        // same process (each open file has its own lock).
        assert!(BuildLock::take(&ws).unwrap().is_none());
        drop(lock);
        assert!(!held(&ws.root), "released when the build ends");
        assert!(BuildLock::take(&ws).unwrap().is_some(), "and the next build can take it");
    }

    #[test]
    fn looking_doesnt_refuse_a_build_or_count_as_one() {
        let (_d, ws) = project();
        drop(BuildLock::take(&ws).unwrap());
        // Someone looking holds the lock for a moment: a build that starts then still gets it.
        let look = OpenOptions::new().read(true).open(path(&ws.root)).unwrap();
        look.try_lock_shared().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(look);
        });
        assert!(BuildLock::take(&ws).unwrap().is_some(), "a look isn't a build");
        release.join().unwrap();
        // And two looks at once don't see each other.
        let other = OpenOptions::new().read(true).open(path(&ws.root)).unwrap();
        other.try_lock_shared().unwrap();
        assert!(!held(&ws.root));
    }

    #[test]
    fn a_project_without_a_lock_file_has_no_build() {
        let (_d, ws) = project();
        assert!(!path(&ws.root).exists());
        assert!(!held(&ws.root));
        assert!(!path(&ws.root).exists(), "looking doesn't create one");
    }
}
