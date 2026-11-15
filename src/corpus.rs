use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tantivy::directory::{Directory, MmapDirectory};

use crate::{
    Cancellation, DocumentFormat, DocumentRef, Error, Evidence, LineRange, Result,
    source::SourceObject, storage,
};

const SQLITE_FILE: &str = "corpus.sqlite";
const SCHEMA_VERSION: u32 = 1;
const INSERT: &str = "INSERT INTO revisions (source_id, object_id, version, record)
    VALUES (?1, ?2, ?3, ?4) ON CONFLICT (source_id, object_id, version) DO NOTHING";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CorpusBackend {
    #[default]
    Sqlite,
    Json,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<SourceLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parser: Option<ParserInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub range: LineRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[u16; 4]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ParserInfo {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Source module that extracted the document, such as `loader.command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Version reported for an external parser tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusDocument {
    pub evidence: Evidence,
    pub content: String,
    pub format: DocumentFormat,
    pub parsed_version: String,
    #[serde(flatten)]
    pub metadata: CorpusMetadata,
}

pub(crate) struct Corpus {
    json: MmapDirectory,
    sqlite_path: PathBuf,
    sqlite: Option<Mutex<Connection>>,
}

impl Corpus {
    pub fn open(path: &Path, backend: CorpusBackend, cancel: &Cancellation) -> Result<Self> {
        std::fs::create_dir_all(path)
            .map_err(|_| Error::Io("Could not create the corpus directory"))?;
        let json = MmapDirectory::open(path)
            .map_err(|_| Error::Io("Could not open the corpus directory"))?;
        let sqlite_path = path.join(SQLITE_FILE);
        let sqlite = match backend {
            CorpusBackend::Sqlite => Some(Mutex::new(open_sqlite(&sqlite_path, path, cancel)?)),
            CorpusBackend::Json => None,
        };
        Ok(Self {
            json,
            sqlite_path,
            sqlite,
        })
    }

    pub fn save(&self, object: &SourceObject, end_line: usize) -> Result<()> {
        let source = &object.source;
        let path = record_path(&source.source_id, &source.object_id, &source.version);
        let has_json = self
            .json
            .exists(&path)
            .map_err(|_| Error::Io("Could not check the corpus revision"))?;
        if self.sqlite.is_none() {
            if has_json {
                return Ok(());
            }
            let document = DocumentRef {
                source_id: source.source_id.clone(),
                object_id: source.object_id.clone(),
            };
            if self
                .read_optional_sqlite(&document, &source.version)?
                .is_some()
            {
                return Ok(());
            }
        }
        let record = || CorpusDocument {
            evidence: object.evidence(LineRange {
                start_line: 1,
                end_line,
            }),
            content: object.content.clone(),
            format: object.format(),
            parsed_version: object.parsed_version.clone(),
            metadata: object.metadata.clone(),
        };
        if let Some(connection) = &self.sqlite {
            let bytes = if has_json {
                self.json
                    .atomic_read(&path)
                    .map_err(|_| Error::Io("Could not read the legacy corpus revision"))?
            } else {
                serde_json::to_vec(&record())
                    .map_err(|_| Error::Io("Could not encode the corpus revision"))?
            };
            let connection = connection
                .lock()
                .map_err(|_| Error::Io("Could not lock the corpus database"))?;
            connection
                .execute(
                    INSERT,
                    params![source.source_id, source.object_id, source.version, bytes],
                )
                .map_err(|error| sqlite_error(error, "Could not save the corpus revision"))?;
            return Ok(());
        }
        storage::write_json(&self.json, &path, &record())
            .map_err(|_| Error::Io("Could not save the corpus revision"))
    }

    pub fn get(&self, document: &DocumentRef, version: &str) -> Result<Option<CorpusDocument>> {
        if let Some(connection) = &self.sqlite {
            let connection = connection
                .lock()
                .map_err(|_| Error::Io("Could not lock the corpus database"))?;
            if let Some(record) = read_sqlite(&connection, document, version)? {
                return Ok(Some(record));
            }
        }
        let path = record_path(&document.source_id, &document.object_id, version);
        let record = storage::read_json(&self.json, &path)
            .map_err(|_| Error::Io("Could not read the corpus revision"))?;
        if record.is_none() && self.sqlite.is_none() {
            return self.read_optional_sqlite(document, version);
        }
        Ok(record)
    }

    fn read_optional_sqlite(
        &self,
        document: &DocumentRef,
        version: &str,
    ) -> Result<Option<CorpusDocument>> {
        if !self
            .sqlite_path
            .try_exists()
            .map_err(|_| Error::Io("Could not check the corpus database"))?
        {
            return Ok(None);
        }
        let connection =
            Connection::open_with_flags(&self.sqlite_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| sqlite_error(error, "Could not open the corpus database"))?;
        if schema_version(&connection)? == 0 {
            return Ok(None);
        }
        read_sqlite(&connection, document, version)
    }
}

fn open_sqlite(path: &Path, legacy: &Path, cancel: &Cancellation) -> Result<Connection> {
    let mut connection = Connection::open(path)
        .map_err(|error| sqlite_error(error, "Could not open the corpus database"))?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(|error| sqlite_error(error, "Could not configure the corpus database"))?;
    if schema_version(&connection)? == 0 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_error(error, "Could not start corpus migration"))?;
        if schema_version(&transaction)? == 0 {
            transaction
                .execute_batch(
                    "CREATE TABLE revisions (
                source_id TEXT NOT NULL, object_id TEXT NOT NULL, version TEXT NOT NULL,
                record BLOB NOT NULL, PRIMARY KEY (source_id, object_id, version)
            ) WITHOUT ROWID;",
                )
                .map_err(|error| sqlite_error(error, "Could not initialize the corpus database"))?;
            migrate_json(&transaction, legacy, cancel)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|error| sqlite_error(error, "Could not record corpus migration"))?;
        }
        cancel.check()?;
        transaction
            .commit()
            .map_err(|error| sqlite_error(error, "Could not finish corpus migration"))?;
    }
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<u32> {
    let version = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_error(error, "Could not read the corpus schema version"))?;
    match version {
        0 | SCHEMA_VERSION => Ok(version),
        _ => Err(Error::Io("Unsupported corpus database version")),
    }
}

fn migrate_json(connection: &Connection, legacy: &Path, cancel: &Cancellation) -> Result<()> {
    let entries = std::fs::read_dir(legacy)
        .map_err(|_| Error::Io("Could not list legacy corpus revisions"))?;
    let mut insert = connection
        .prepare(INSERT)
        .map_err(|error| sqlite_error(error, "Could not prepare corpus migration"))?;
    for entry in entries {
        cancel.check()?;
        let path = entry
            .map_err(|_| Error::Io("Could not list legacy corpus revisions"))?
            .path();
        let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|ext| ext != "json")
            || stem.len() != 64
            || !stem
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|_| Error::Io("Could not read a legacy corpus revision"))?;
        let record: CorpusDocument = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Io("Invalid legacy corpus revision"))?;
        let source = &record.evidence.source;
        if path.file_name()
            != record_path(&source.source_id, &source.object_id, &source.version).file_name()
        {
            return Err(Error::Io(
                "Legacy corpus revision does not match its filename",
            ));
        }
        insert
            .execute(params![
                source.source_id,
                source.object_id,
                source.version,
                bytes
            ])
            .map_err(|error| sqlite_error(error, "Could not migrate a legacy corpus revision"))?;
    }
    Ok(())
}

fn read_sqlite(
    connection: &Connection,
    document: &DocumentRef,
    version: &str,
) -> Result<Option<CorpusDocument>> {
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record FROM revisions WHERE source_id = ?1 AND object_id = ?2 AND version = ?3",
            params![document.source_id, document.object_id, version],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| sqlite_error(error, "Could not read the corpus revision"))?;
    bytes
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| Error::Io("Invalid SQLite corpus revision"))
        })
        .transpose()
}

fn sqlite_error(error: rusqlite::Error, message: &'static str) -> Error {
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Error::Busy,
        _ => Error::Io(message),
    }
}

fn record_path(source_id: &str, object_id: &str, version: &str) -> PathBuf {
    let mut key = blake3::Hasher::new();
    key.update(source_id.as_bytes())
        .update(b"\0")
        .update(object_id.as_bytes())
        .update(b"\0")
        .update(version.as_bytes());
    PathBuf::from(format!("{}.json", key.finalize().to_hex()))
}
