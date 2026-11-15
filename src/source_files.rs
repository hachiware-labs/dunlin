//! Source file reads and shared original storage for the project CLI.
//!
//! The CLI parses files in JavaScript, but reading many small files and storing their originals
//! one asynchronous call at a time spent most of the source stage waiting. These functions do the
//! file work for a batch at once.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The bytes of a source file and its modification time in milliseconds since 1970.
#[derive(Debug)]
pub struct SourceFile {
    pub bytes: Vec<u8>,
    pub modified_at_ms: Option<u64>,
}

/// Why a source file could not be read. The JavaScript caller turns these into its messages.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadFailure {
    NotFile,
    TooLarge,
    Changed,
    Io(String),
}

/// Reads a regular file of at most `limit` bytes and checks that it did not change meanwhile.
pub fn read_source_file(path: &Path, limit: u64) -> Result<SourceFile, ReadFailure> {
    let io = |error: std::io::Error| ReadFailure::Io(io_message(&error, path));
    let mut file = File::open(path).map_err(io)?;
    let before = file.metadata().map_err(io)?;
    if !before.is_file() {
        return Err(ReadFailure::NotFile);
    }
    let mut bytes = Vec::with_capacity(before.len().min(limit) as usize);
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > limit {
        return Err(ReadFailure::TooLarge);
    }
    drop(file);
    let after = fs::metadata(path).map_err(io)?;
    if before.modified().ok() != after.modified().ok() || before.len() != after.len() {
        return Err(ReadFailure::Changed);
    }
    let modified_at_ms = before
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|time| time.as_millis() as u64);
    Ok(SourceFile {
        bytes,
        modified_at_ms,
    })
}

/// A Node.js style message, which does not depend on the system language.
fn io_message(error: &std::io::Error, path: &Path) -> String {
    use std::io::ErrorKind;
    let (code, text) = match error.kind() {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        ErrorKind::ResourceBusy => ("EBUSY", "resource busy or locked"),
        _ => ("EIO", "i/o error"),
    };
    format!("{code}: {text}, open '{}'", path.display())
}

/// Stores `bytes` as `<directory>/<sha256>.bin` unless an identical file is already there.
/// The caller computes the digest. A different existing file is an error.
pub fn store_original(directory: &Path, sha256: &str, bytes: &[u8]) -> Result<(), String> {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("Invalid original digest".into());
    }
    let path = directory.join(format!("{sha256}.bin"));
    match fs::symlink_metadata(&path) {
        Ok(entry) if !entry.is_file() => {
            return Err(format!("Expected a regular file at {}", path.display()));
        }
        Ok(entry) => {
            if entry.len() == bytes.len() as u64
                && fs::read(&path).map_err(|error| error.to_string())? == bytes
            {
                return Ok(());
            }
            return Err("Stored original file does not match its SHA-256 digest".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let temporary = temporary_path(&path);
    let written = File::create_new(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written.map_err(|error| error.to_string())
}

/// Creates `directory` and its parents, and checks that it is a regular directory.
pub fn original_directory(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let entry = fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if !entry.is_dir() {
        return Err(format!(
            "Expected a regular directory at {}",
            directory.display()
        ));
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .unwrap_or_default();
    let mut name = path.as_os_str().to_owned();
    name.push(format!(
        ".{}-{nanos}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bounded_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.md");
        fs::write(&path, "手順\n").unwrap();
        let file = read_source_file(&path, 64).unwrap();
        assert_eq!(file.bytes, "手順\n".as_bytes());
        assert!(file.modified_at_ms.is_some());
        assert_eq!(
            read_source_file(&path, 3).unwrap_err(),
            ReadFailure::TooLarge
        );
        assert!(matches!(
            read_source_file(directory.path(), 64),
            Err(ReadFailure::NotFile | ReadFailure::Io(_))
        ));
        assert!(matches!(
            read_source_file(&directory.path().join("missing"), 64),
            Err(ReadFailure::Io(_))
        ));
    }

    #[test]
    fn stores_originals_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("originals").join("shared");
        original_directory(&directory).unwrap();
        original_directory(&directory).unwrap();
        let directory = tempfile::TempDir::new_in(&directory).unwrap();
        let digest = "a".repeat(64);
        store_original(directory.path(), &digest, b"one").unwrap();
        store_original(directory.path(), &digest, b"one").unwrap();
        assert_eq!(
            fs::read(directory.path().join(format!("{digest}.bin"))).unwrap(),
            b"one"
        );
        assert!(store_original(directory.path(), &digest, b"two").is_err());
        assert!(store_original(directory.path(), "../x", b"one").is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
