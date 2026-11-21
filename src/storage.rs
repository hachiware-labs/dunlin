use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use tantivy::directory::error::{
    DeleteError, LockError, OpenDirectoryError, OpenReadError, OpenWriteError,
};
use tantivy::directory::{
    Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, WatchCallback, WatchHandle, WritePtr,
};

/// A folder of index files that retries the replacing write and the read of a small file such as
/// `meta.json` while another handle has it open.
///
/// On Windows, replacing a file fails with "access denied" while any process holds it open, such
/// as a virus scanner that opens new files or another reader. A commit that met such a moment
/// failed with "Could not open, update, or read the index". (The index readers no longer watch
/// `meta.json`, which once caused most of these failures; see `KeywordIndex::open`.)
#[derive(Clone, Debug)]
pub(crate) struct RetryDirectory(MmapDirectory);

impl RetryDirectory {
    pub(crate) fn open(path: &Path) -> Result<Self, OpenDirectoryError> {
        MmapDirectory::open(path).map(Self)
    }
}

/// Whether an error is the brief "file in use" of Windows rather than a lasting refusal.
#[cfg(windows)]
fn in_use(error: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION.
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(not(windows))]
fn in_use(_error: &io::Error) -> bool {
    false
}

/// Runs `operation` again for up to about two seconds while the file is in use.
fn retry<T, E>(
    mut operation: impl FnMut() -> Result<T, E>,
    io_error: fn(&E) -> Option<&io::Error>,
) -> Result<T, E> {
    let mut attempt = 0;
    loop {
        match operation() {
            Err(error) if attempt < 40 && io_error(&error).is_some_and(in_use) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 + 2 * attempt));
            }
            other => return other,
        }
    }
}

impl Directory for RetryDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.0.get_file_handle(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.0.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.0.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        self.0.open_write(path)
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        retry(
            || self.0.atomic_read(path),
            |error| match error {
                OpenReadError::IoError { io_error, .. } => Some(io_error.as_ref()),
                _ => None,
            },
        )
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        retry(|| self.0.atomic_write(path, data), |error| Some(error))
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.0.sync_directory()
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.0.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.0.watch(watch_callback)
    }
}

pub(crate) fn read_json<T: DeserializeOwned>(
    directory: &impl Directory,
    path: &Path,
) -> io::Result<Option<T>> {
    match directory.atomic_read(path) {
        Ok(data) => serde_json::from_slice(&data)
            .map(Some)
            .map_err(io::Error::other),
        Err(OpenReadError::FileDoesNotExist(_)) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

pub(crate) fn write_json<T: Serialize + ?Sized>(
    directory: &impl Directory,
    path: &Path,
    value: &T,
) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    directory.atomic_write(path, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_a_file_that_another_handle_has_open() {
        let folder = tempfile::tempdir().unwrap();
        let directory = RetryDirectory::open(folder.path()).unwrap();
        let name = Path::new("meta.json");
        directory.atomic_write(name, b"1").unwrap();
        // A reader, like the index's watcher, holds the file open for a moment.
        let reader = std::fs::File::open(folder.path().join(name)).unwrap();
        #[cfg(windows)]
        assert!(
            directory
                .0
                .atomic_write(name, b"x")
                .is_err_and(|error| in_use(&error))
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(reader);
        });
        directory.atomic_write(name, b"2").unwrap();
        release.join().unwrap();
        assert_eq!(directory.atomic_read(name).unwrap(), b"2");
    }
}
