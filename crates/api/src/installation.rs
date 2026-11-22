//! The one process that may open an installation's data directory.
//!
//! Much of what an installation shares is guarded by locks that live in one
//! process: the workspace registry and manifests, the global settings file,
//! the Python environment's build sweep, the model records, and the residency
//! bound that keeps two workers of one kind from holding their models at once.
//! Those are correct only while a single process has the directory open. The
//! desktop app's single-instance plugin used to be what made that true, but it
//! is keyed by the bundle identifier, not by the directory: a `wilkes-server`
//! pointed at the desktop app's data, or a build with another identifier,
//! passed it and wrote the same files with none of those locks shared.
//!
//! So the directory itself is locked, by whichever process opens it, for as
//! long as that process has it open. The operating system releases the lock
//! when the process exits, however it exits, so a crash never leaves a lock
//! that refuses the next start.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const LOCK_FILENAME: &str = ".installation.lock";

/// Another process holds the installation's data directory.
///
/// Its own type so a shell can tell this from every other startup failure and
/// say what to do about it: the remedy is to close the other Wilkes, not to
/// report a bug.
#[derive(Debug)]
pub struct InstallationInUse {
    pub data_dir: PathBuf,
    /// The holder's process id as it recorded it, when it could be read.
    pub holder_pid: Option<u32>,
}

impl std::fmt::Display for InstallationInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Another Wilkes process{} is using the library at {}",
            self.holder_pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default(),
            self.data_dir.display()
        )
    }
}

impl std::error::Error for InstallationInUse {}

/// Held for as long as this process has the data directory open.
#[derive(Debug)]
pub struct InstallationLock {
    _file: File,
}

impl InstallationLock {
    /// Locks `data_dir` for this process, or fails with [`InstallationInUse`]
    /// when another process (or another opener in this one) holds it.
    pub fn acquire(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(LOCK_FILENAME);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut recorded = String::new();
                let holder_pid = match file.read_to_string(&mut recorded) {
                    Ok(_) => recorded.trim().parse().ok(),
                    Err(error) => {
                        tracing::warn!("could not read the holder of {}: {error}", path.display());
                        None
                    }
                };
                return Err(InstallationInUse {
                    data_dir: data_dir.to_path_buf(),
                    holder_pid,
                }
                .into());
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        // The pid is for the message another process shows, not for the lock:
        // the lock is the flock, and a stale pid in the file means nothing.
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        write!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_until_the_first_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstallationLock::acquire(dir.path()).unwrap();

        let error = InstallationLock::acquire(dir.path()).unwrap_err();
        let in_use = error.downcast_ref::<InstallationInUse>().unwrap();
        assert_eq!(in_use.holder_pid, Some(std::process::id()));
        assert_eq!(in_use.data_dir, dir.path());

        drop(first);
        InstallationLock::acquire(dir.path()).unwrap();
    }

    #[test]
    fn two_directories_are_two_installations() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let _a = InstallationLock::acquire(a.path()).unwrap();
        InstallationLock::acquire(b.path()).unwrap();
    }
}
