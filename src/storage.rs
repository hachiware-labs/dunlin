use std::io;
use std::path::Path;

use serde::{Serialize, de::DeserializeOwned};
use tantivy::directory::error::OpenReadError;
use tantivy::directory::{Directory, MmapDirectory};

pub(crate) fn read_json<T: DeserializeOwned>(
    directory: &MmapDirectory,
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
    directory: &MmapDirectory,
    path: &Path,
    value: &T,
) -> io::Result<()> {
    let data = serde_json::to_vec(value).map_err(io::Error::other)?;
    directory.atomic_write(path, &data)
}
