//! The store of the data management system (docs/data-management.md, chapter 4).
//!
//! Three tables. `objects` holds content by the hash of its bytes; `snapshots` holds lists of
//! objects with their parents and tags; `runs` records what happened. A new row says everything
//! about itself when it is made: a snapshot names its parents and its items, and nothing is
//! written to the rows it points at. Only tags and notes change later.
//!
//! Objects are in `objects.sqlite`, the two other tables in `records.sqlite`. Objects are written
//! first, so a record never points at something that is missing.
//!
//! Not here yet: the project lock, materializations, large content as files, and collection.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::records::{
    InfoId, ItemId, ItemName, ListDigest, ObjectId, Part, Process, ProcessId, SnapshotKey,
    canonical, info_id, item_id, item_value, list_digest, list_from_value, list_value, object_id,
    snapshot_key,
};

const OBJECTS: &str = "
    CREATE TABLE IF NOT EXISTS objects (id TEXT PRIMARY KEY, data BLOB NOT NULL,
        size INTEGER NOT NULL, created_at INTEGER NOT NULL,
        results TEXT NOT NULL DEFAULT '{}', meta TEXT NOT NULL DEFAULT '{}');";

const RECORDS: &str = "
    CREATE TABLE IF NOT EXISTS snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT,
        type TEXT NOT NULL, items TEXT NOT NULL, count INTEGER NOT NULL,
        parents TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '[]', created_at INTEGER NOT NULL,
        meta TEXT NOT NULL DEFAULT '{}');
    CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, stage TEXT NOT NULL, key TEXT NOT NULL,
        inputs TEXT NOT NULL, state TEXT NOT NULL, started_at INTEGER NOT NULL,
        completed_at INTEGER, snapshot INTEGER, outcome TEXT, degraded INTEGER NOT NULL DEFAULT 0,
        tag TEXT, added INTEGER, updated INTEGER, deleted INTEGER, unchanged INTEGER,
        config TEXT, info TEXT, stats TEXT, cause TEXT);
    CREATE INDEX IF NOT EXISTS runs_key ON runs (key);
    CREATE INDEX IF NOT EXISTS runs_tag ON runs (tag);";

/// Objects written between two saves.
const BATCH: usize = 256;

#[derive(Debug)]
pub struct StoreError(String);

impl StoreError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Milliseconds since 1970-01-01 UTC.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The serial number of a snapshot. It says nothing about what the snapshot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotId(pub i64);

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A list of objects, with where it came from and what it is called.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub id: SnapshotId,
    /// `files`, `extracted`, `chunks`, `index`, or a type of data across documents.
    pub kind: String,
    /// Names with the ID of what each holds, in the order of the names.
    pub items: Vec<(ItemName, String)>,
    /// The snapshots this one was first made from.
    pub parents: Vec<SnapshotId>,
    /// The names that point at this snapshot now.
    pub tags: Vec<String>,
    pub created_at: u64,
    /// Notes added later. Nothing here is part of an identifier, and only what can be worked
    /// out again belongs here.
    pub meta: Value,
}

impl Snapshot {
    /// What the snapshot holds, for comparing and for the keys of later stages.
    pub fn digest(&self) -> ListDigest {
        list_digest(&self.kind, &self.items)
    }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A new snapshot was made.
    Created,
    /// The key led to a snapshot, so nothing ran.
    Reused,
    /// The stage ran and made what was already there.
    Unchanged,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Reused => "reused",
            Self::Unchanged => "unchanged",
        }
    }
}

/// The result of a finished run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed {
    pub run: String,
    pub snapshot: SnapshotId,
    pub outcome: Outcome,
}

/// How many items a run found added, updated, deleted and unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
}

/// One run of a stage, as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecord {
    pub id: String,
    pub stage: String,
    /// `building`, `complete` or `failed`.
    pub state: String,
    pub outcome: Option<String>,
    /// A failure made an earlier item stand in. Such a result is not found by its key.
    pub degraded: bool,
    pub inputs: Vec<SnapshotId>,
    pub snapshot: Option<SnapshotId>,
    /// The name this run pointed at its result.
    pub tag: Option<String>,
    pub counts: Option<Counts>,
    /// The configuration file as it was when the run started, as an object.
    pub config: Option<ObjectId>,
    /// How the result was made: its type, what its inputs held and the modules with their
    /// arguments. It is the key of the run in a form that can be read.
    pub info: Option<InfoId>,
    /// The run that started this one, such as an optimization. Only the run that was started
    /// says so.
    pub cause: Option<String>,
    /// Lists and other details that are not part of the result.
    pub stats: Value,
    pub started_at: u64,
    pub completed_at: Option<u64>,
}

/// The difference between the items of two snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub added: Vec<ItemName>,
    pub updated: Vec<ItemName>,
    pub removed: Vec<ItemName>,
    pub unchanged: usize,
}

pub struct Store {
    objects: Connection,
    records: Connection,
}

/// Runs `work` in one transaction of `connection`.
fn transact<T>(connection: &Connection, work: impl FnOnce() -> StoreResult<T>) -> StoreResult<T> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    match work() {
        Ok(value) => {
            connection.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn corrupt(what: &str) -> StoreError {
    StoreError::new(format!("The saved {what} is not valid."))
}

fn ids(value: &str) -> Option<Vec<SnapshotId>> {
    serde_json::from_str::<Vec<i64>>(value)
        .ok()
        .map(|ids| ids.into_iter().map(SnapshotId).collect())
}

fn ids_json(ids: &[SnapshotId]) -> String {
    Value::Array(ids.iter().map(|id| json!(id.0)).collect()).to_string()
}

/// `$."key"` for a key without quotes or backslashes.
fn path(key: &str) -> StoreResult<String> {
    if key.contains(['"', '\\']) {
        return Err(StoreError::new(
            "A note's name has a character it cannot have.",
        ));
    }
    Ok(format!("$.\"{key}\""))
}

impl Store {
    /// Opens the store in `directory`, creating it when it is not there.
    pub fn open(directory: &Path) -> StoreResult<Self> {
        std::fs::create_dir_all(directory)?;
        let objects = Connection::open(directory.join("objects.sqlite"))?;
        let records = Connection::open(directory.join("records.sqlite"))?;
        for connection in [&objects, &records] {
            connection.busy_timeout(std::time::Duration::from_secs(10))?;
        }
        records.pragma_update(None, "journal_mode", "WAL")?;
        objects.execute_batch(OBJECTS)?;
        records.execute_batch(RECORDS)?;
        Ok(Self { objects, records })
    }

    /// Starts a run of `stage` that makes a `kind` snapshot from `inputs` with `process`.
    pub fn begin(
        &self,
        stage: &str,
        kind: &str,
        inputs: &[SnapshotId],
        process: &Process,
    ) -> StoreResult<Draft<'_>> {
        let digests = inputs
            .iter()
            .map(|input| self.snapshot(*input).map(|snapshot| snapshot.digest()))
            .collect::<StoreResult<Vec<_>>>()?;
        let key = snapshot_key(kind, &digests, process);
        // What the key is the hash of, kept so that the run can say how its result was made.
        let info = json!({
            "type": kind,
            "inputs": digests.iter().map(ListDigest::as_str).collect::<Vec<_>>(),
            "modules": process.to_value(),
        });
        let id = info_id(&info);
        self.objects.execute(
            "INSERT OR IGNORE INTO objects (id, data, size, created_at) VALUES (?, ?, ?, ?)",
            params![
                id.as_str(),
                canonical(&info).as_bytes(),
                canonical(&info).len() as i64,
                now_ms() as i64
            ],
        )?;
        self.start(stage, kind, inputs, key, Some(id))
    }

    /// Starts a run whose input is outside the store, such as a folder. It runs every time: its
    /// result is not looked up by a key.
    pub fn begin_scan(&self, stage: &str, kind: &str) -> StoreResult<Draft<'_>> {
        let key = snapshot_key(kind, &[], &Process::default());
        self.start(stage, kind, &[], key, None)
    }

    fn start(
        &self,
        stage: &str,
        kind: &str,
        inputs: &[SnapshotId],
        key: SnapshotKey,
        info: Option<InfoId>,
    ) -> StoreResult<Draft<'_>> {
        let run = format!("run-{}", uuid::Uuid::new_v4());
        self.records.execute(
            "INSERT INTO runs (id, stage, key, inputs, info, state, started_at)
             VALUES (?, ?, ?, ?, ?, 'building', ?)",
            params![
                run,
                stage,
                key.as_str(),
                ids_json(inputs),
                info.as_ref().map(InfoId::as_str),
                now_ms() as i64
            ],
        )?;
        Ok(Draft {
            store: self,
            run,
            kind: kind.to_owned(),
            inputs: inputs.to_vec(),
            key,
            keyed: info.is_some(),
            info,
            config: None,
            cause: None,
            items: Vec::new(),
            degraded: false,
            pending: 0,
            writing: false,
            counts: None,
            stats: Value::Null,
            done: false,
        })
    }

    /// The result of the latest complete run under a key. A degraded run is passed over, so that
    /// the next run tries again what failed.
    pub fn find(&self, key: &SnapshotKey) -> StoreResult<Option<SnapshotId>> {
        Ok(self
            .records
            .query_row(
                "SELECT snapshot FROM runs WHERE key = ? AND state = 'complete' AND degraded = 0
                    AND snapshot IS NOT NULL
                 ORDER BY rowid DESC LIMIT 1",
                [key.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(SnapshotId))
    }

    /// The snapshot a run of `process` on `inputs` would be given without running, if there is
    /// one. It is how a plan says what will run.
    pub fn lookup(
        &self,
        kind: &str,
        inputs: &[SnapshotId],
        process: &Process,
    ) -> StoreResult<Option<SnapshotId>> {
        let digests = inputs
            .iter()
            .map(|input| self.snapshot(*input).map(|snapshot| snapshot.digest()))
            .collect::<StoreResult<Vec<_>>>()?;
        self.find(&snapshot_key(kind, &digests, process))
    }

    /// The snapshot that has a tag now.
    fn tagged(&self, tag: &str) -> StoreResult<Option<SnapshotId>> {
        Ok(self
            .records
            .query_row(
                "SELECT snapshots.id FROM snapshots, json_each(snapshots.tags)
                 WHERE json_each.value = ? LIMIT 1",
                [tag],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(SnapshotId))
    }

    /// The snapshot a tag points at, or the snapshot with this serial number.
    pub fn resolve(&self, tag_or_id: &str) -> StoreResult<Option<SnapshotId>> {
        if let Some(id) = self.tagged(tag_or_id)? {
            return Ok(Some(id));
        }
        let Ok(id) = tag_or_id.parse::<i64>() else {
            return Ok(None);
        };
        Ok(self
            .records
            .query_row("SELECT id FROM snapshots WHERE id = ?", [id], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
            .map(SnapshotId))
    }

    pub fn snapshot(&self, id: SnapshotId) -> StoreResult<Snapshot> {
        let row: Option<(String, String, String, String, i64, String)> = self
            .records
            .query_row(
                "SELECT type, items, parents, tags, created_at, meta FROM snapshots WHERE id = ?",
                [id.0],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let (kind, items, parents, tags, created_at, meta) =
            row.ok_or_else(|| StoreError::new(format!("There is no snapshot {id}.")))?;
        let invalid = || corrupt("snapshot");
        Ok(Snapshot {
            id,
            kind,
            items: serde_json::from_str::<Value>(&items)
                .ok()
                .and_then(|value| list_from_value(&value))
                .ok_or_else(invalid)?,
            parents: ids(&parents).ok_or_else(invalid)?,
            tags: serde_json::from_str(&tags).map_err(|_| invalid())?,
            created_at: created_at as u64,
            meta: serde_json::from_str(&meta).map_err(|_| invalid())?,
        })
    }

    /// Writes a note on a snapshot. Notes are not part of what the snapshot is.
    pub fn note(&self, id: SnapshotId, name: &str, value: &Value) -> StoreResult<()> {
        self.records.execute(
            "UPDATE snapshots SET meta = json_set(meta, ?, json(?)) WHERE id = ?",
            params![path(name)?, value.to_string(), id.0],
        )?;
        Ok(())
    }

    /// The bytes of an object.
    pub fn bytes(&self, id: &ObjectId) -> StoreResult<Vec<u8>> {
        self.data(id.as_str())
    }

    fn data(&self, id: &str) -> StoreResult<Vec<u8>> {
        self.objects
            .query_row("SELECT data FROM objects WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or_else(|| StoreError::new(format!("There is no object {id}.")))
    }

    /// The parts of a document's item, in order.
    pub fn item(&self, id: &ItemId) -> StoreResult<Vec<Part>> {
        match serde_json::from_slice::<Value>(&self.data(id.as_str())?) {
            Ok(Value::Array(parts)) => parts
                .iter()
                .map(|part| Part::from_value(part).ok_or_else(|| corrupt("item")))
                .collect(),
            _ => Err(corrupt("item")),
        }
    }

    /// An information object: the description of something made from a whole snapshot.
    pub fn info(&self, id: &InfoId) -> StoreResult<Value> {
        serde_json::from_slice(&self.data(id.as_str())?).map_err(|_| corrupt("information object"))
    }

    /// Writes a note on an object, an item or an information object, by its ID. A note is not
    /// part of what the object is: its ID stays, and so does every snapshot that lists it.
    pub fn note_object(&self, id: &str, name: &str, value: &Value) -> StoreResult<()> {
        let noted = self.objects.execute(
            "UPDATE objects SET meta = json_set(meta, ?, json(?)) WHERE id = ?",
            params![path(name)?, value.to_string(), id],
        )?;
        if noted == 0 {
            return Err(StoreError::new(format!("There is no object {id}.")));
        }
        Ok(())
    }

    /// The notes on an object, as a JSON object.
    pub fn object_notes(&self, id: &str) -> StoreResult<Value> {
        let meta: Option<String> = self
            .objects
            .query_row("SELECT meta FROM objects WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let meta = meta.ok_or_else(|| StoreError::new(format!("There is no object {id}.")))?;
        serde_json::from_str(&meta).map_err(|_| corrupt("notes of an object"))
    }

    /// What `process` made of an object or an item, if it remembers. `input` is its ID.
    pub fn result(&self, input: &str, process: &ProcessId) -> StoreResult<Option<ItemId>> {
        let found: Option<Option<String>> = self
            .objects
            .query_row(
                "SELECT json_extract(results, ?) FROM objects WHERE id = ?",
                params![path(process.as_str())?, input],
                |row| row.get(0),
            )
            .optional()?;
        match found.flatten() {
            Some(item) => ItemId::parse(&item)
                .map(Some)
                .ok_or_else(|| corrupt("result of an object")),
            None => Ok(None),
        }
    }

    /// What changed from snapshot `from` to snapshot `to`, by the names of their items.
    pub fn diff(&self, from: SnapshotId, to: SnapshotId) -> StoreResult<Diff> {
        Ok(compare(
            &self.snapshot(from)?.items,
            &self.snapshot(to)?.items,
        ))
    }

    /// The snapshots a tag has pointed at, oldest first.
    pub fn history(&self, tag: &str) -> StoreResult<Vec<SnapshotId>> {
        let mut statement = self.records.prepare(
            "SELECT snapshot FROM runs WHERE tag = ? AND state = 'complete' ORDER BY rowid",
        )?;
        let rows = statement.query_map([tag], |row| row.get::<_, i64>(0))?;
        let mut history: Vec<SnapshotId> = Vec::new();
        for id in rows {
            let id = SnapshotId(id?);
            if history.last() != Some(&id) {
                history.push(id);
            }
        }
        Ok(history)
    }

    /// The inputs of the latest complete run that pointed `tag` at its result.
    pub fn last_inputs(&self, tag: &str) -> StoreResult<Vec<SnapshotId>> {
        let inputs: Option<String> = self
            .records
            .query_row(
                "SELECT inputs FROM runs WHERE tag = ? AND state = 'complete'
                 ORDER BY rowid DESC LIMIT 1",
                [tag],
                |row| row.get(0),
            )
            .optional()?;
        match inputs {
            Some(inputs) => ids(&inputs).ok_or_else(|| corrupt("run")),
            None => Ok(Vec::new()),
        }
    }

    /// Every run, oldest first.
    pub fn runs(&self) -> StoreResult<Vec<RunRecord>> {
        let mut statement = self.records.prepare(
            "SELECT id, stage, state, outcome, degraded, inputs, snapshot, tag, added, updated,
                deleted, unchanged, started_at, completed_at, config, info, cause, stats FROM runs
             ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            let count = |index: usize| row.get::<_, Option<i64>>(index);
            let counts = match (count(8)?, count(9)?, count(10)?, count(11)?) {
                (Some(added), Some(updated), Some(deleted), Some(unchanged)) => Some(Counts {
                    added: added as usize,
                    updated: updated as usize,
                    deleted: deleted as usize,
                    unchanged: unchanged as usize,
                }),
                _ => None,
            };
            Ok(RunRecord {
                id: row.get(0)?,
                stage: row.get(1)?,
                state: row.get(2)?,
                outcome: row.get(3)?,
                degraded: row.get(4)?,
                inputs: ids(&row.get::<_, String>(5)?).unwrap_or_default(),
                snapshot: row.get::<_, Option<i64>>(6)?.map(SnapshotId),
                tag: row.get(7)?,
                counts,
                config: row
                    .get::<_, Option<String>>(14)?
                    .and_then(|id| ObjectId::parse(&id)),
                info: row
                    .get::<_, Option<String>>(15)?
                    .and_then(|id| InfoId::parse(&id)),
                cause: row.get(16)?,
                stats: row
                    .get::<_, Option<String>>(17)?
                    .and_then(|stats| serde_json::from_str(&stats).ok())
                    .unwrap_or(Value::Null),
                started_at: row.get::<_, i64>(12)? as u64,
                completed_at: row.get::<_, Option<i64>>(13)?.map(|at| at as u64),
            })
        })?;
        rows.map(|row| row.map_err(StoreError::from)).collect()
    }

    /// Points a name at a snapshot, as `dunlin tag` and a release do. It is recorded as a run of
    /// `stage`, so the history of the name shows it.
    pub fn tag(&self, stage: &str, name: &str, snapshot: SnapshotId) -> StoreResult<()> {
        self.snapshot(snapshot)?;
        transact(&self.records, || {
            self.records.execute(
                "INSERT INTO runs (id, stage, key, inputs, state, started_at, completed_at,
                    snapshot, tag) VALUES (?, ?, '', '[]', 'complete', ?, ?, ?, ?)",
                params![
                    format!("run-{}", uuid::Uuid::new_v4()),
                    stage,
                    now_ms() as i64,
                    now_ms() as i64,
                    snapshot.0,
                    name
                ],
            )?;
            self.move_tag(name, snapshot)
        })
    }

    /// Takes `tag` from the snapshot that has it and gives it to `snapshot`. Inside a
    /// transaction of `records`. These two rows are the only existing ones a run changes.
    fn move_tag(&self, tag: &str, snapshot: SnapshotId) -> StoreResult<()> {
        let holder = self.tagged(tag)?;
        if holder == Some(snapshot) {
            return Ok(());
        }
        if let Some(holder) = holder {
            let mut tags = self.snapshot(holder)?.tags;
            tags.retain(|name| name != tag);
            self.records.execute(
                "UPDATE snapshots SET tags = ? WHERE id = ?",
                params![json!(tags).to_string(), holder.0],
            )?;
        }
        let mut tags = self.snapshot(snapshot)?.tags;
        tags.push(tag.to_owned());
        self.records.execute(
            "UPDATE snapshots SET tags = ? WHERE id = ?",
            params![json!(tags).to_string(), snapshot.0],
        )?;
        Ok(())
    }
}

/// What changed from one list of items to another, by their names.
pub fn compare(from: &[(ItemName, String)], to: &[(ItemName, String)]) -> Diff {
    let before: HashMap<&ItemName, &String> = from.iter().map(|(name, id)| (name, id)).collect();
    let after: HashMap<&ItemName, &String> = to.iter().map(|(name, id)| (name, id)).collect();
    let mut result = Diff::default();
    for (name, id) in to {
        match before.get(name) {
            None => result.added.push(name.clone()),
            Some(earlier) if *earlier == id => result.unchanged += 1,
            Some(_) => result.updated.push(name.clone()),
        }
    }
    result.removed = from
        .iter()
        .filter(|(name, _)| !after.contains_key(name))
        .map(|(name, _)| name.clone())
        .collect();
    for names in [&mut result.added, &mut result.updated, &mut result.removed] {
        names.sort();
    }
    result
}

/// A snapshot being made by a run. Dropping it without a commit fails the run; the objects
/// already written stay, with what they remember.
pub struct Draft<'a> {
    store: &'a Store,
    run: String,
    kind: String,
    inputs: Vec<SnapshotId>,
    key: SnapshotKey,
    /// The result is looked up and found by the key.
    keyed: bool,
    info: Option<InfoId>,
    config: Option<ObjectId>,
    cause: Option<String>,
    items: Vec<(ItemName, String)>,
    degraded: bool,
    /// Objects written since the last save.
    pending: usize,
    /// A transaction of `objects` is open.
    writing: bool,
    counts: Option<Counts>,
    stats: Value,
    done: bool,
}

impl Draft<'_> {
    pub fn run(&self) -> &str {
        &self.run
    }

    pub fn key(&self) -> &SnapshotKey {
        &self.key
    }

    /// The snapshot this run would make again, when a run under its key made one.
    pub fn existing(&self) -> StoreResult<Option<SnapshotId>> {
        if self.keyed {
            self.store.find(&self.key)
        } else {
            Ok(None)
        }
    }

    /// How many items were added, updated, deleted and unchanged, for the run record.
    pub fn set_counts(&mut self, counts: Counts) {
        self.counts = Some(counts);
    }

    /// How the result is made, as an information object. A run with a key has one.
    pub fn info(&self) -> Option<&InfoId> {
        self.info.as_ref()
    }

    /// Keeps the configuration file as it is now with the run. The same file is one object,
    /// however many runs keep it.
    pub fn set_config(&mut self, file: &[u8]) -> StoreResult<()> {
        self.config = Some(self.save(file)?);
        Ok(())
    }

    /// Names the run that started this one.
    pub fn set_cause(&mut self, run: &str) {
        self.cause = Some(run.to_owned());
    }

    /// Lists and other details for the run record. They are not part of the snapshot.
    pub fn set_stats(&mut self, stats: Value) {
        self.stats = stats;
    }

    /// Writes details on the run while it is going, for whoever watches it from elsewhere. The
    /// details the run ends with take their place.
    pub fn report_progress(&self, stats: &Value) -> StoreResult<()> {
        self.store.records.execute(
            "UPDATE runs SET stats = ? WHERE id = ?",
            params![canonical(stats), self.run],
        )?;
        Ok(())
    }

    fn write(&mut self, id: &str, data: &[u8]) -> StoreResult<()> {
        if !self.writing {
            self.store.objects.execute_batch("BEGIN IMMEDIATE")?;
            self.writing = true;
        }
        self.store
            .objects
            .prepare(
                "INSERT OR IGNORE INTO objects (id, data, size, created_at) VALUES (?, ?, ?, ?)",
            )?
            .execute(params![id, data, data.len() as i64, now_ms() as i64])?;
        self.pending += 1;
        Ok(())
    }

    /// Saves content as an object. Content that is there already is not written again.
    pub fn save(&mut self, bytes: &[u8]) -> StoreResult<ObjectId> {
        let id = object_id(bytes);
        self.write(id.as_str(), bytes)?;
        if self.pending >= BATCH {
            self.flush()?;
        }
        Ok(id)
    }

    /// Saves an information object. List it under a name of its own, such as the kind of index:
    /// a snapshot's list may hold such objects as well as documents.
    pub fn save_info(&mut self, value: &Value) -> StoreResult<InfoId> {
        let id = info_id(value);
        self.write(id.as_str(), canonical(value).as_bytes())?;
        Ok(id)
    }

    /// Lists a name with the ID of what it holds.
    pub fn list(&mut self, name: ItemName, id: String) {
        self.items.push((name, id));
    }

    /// What `process` made of an object, if the object remembers.
    pub fn result(&self, input: &str, process: &ProcessId) -> StoreResult<Option<ItemId>> {
        self.store.result(input, process)
    }

    /// Saves the item `process` made of the object `input`, lists it, and has `input` remember
    /// it. The objects of `parts` are saved before.
    pub fn put(
        &mut self,
        name: ItemName,
        input: &str,
        process: &ProcessId,
        parts: &[Part],
    ) -> StoreResult<ItemId> {
        let item = item_id(parts);
        self.write(item.as_str(), canonical(&item_value(parts)).as_bytes())?;
        let remembered = self.store.objects.execute(
            "UPDATE objects SET results = json_set(results, ?, ?) WHERE id = ?",
            params![path(process.as_str())?, item.as_str(), input],
        )?;
        if remembered == 0 {
            return Err(StoreError::new(format!(
                "The object {input} was not saved before its result."
            )));
        }
        self.items.push((name, item.to_string()));
        if self.pending >= BATCH {
            self.flush()?;
        }
        Ok(item)
    }

    /// Lists the earlier item for one whose processing failed, or nothing when there is none.
    /// The run is recorded as degraded, so its result is not found by its key and the next run
    /// processes the item again.
    pub fn substitute(&mut self, name: ItemName, earlier: Option<String>) {
        self.degraded = true;
        if let Some(earlier) = earlier {
            self.items.push((name, earlier));
        }
    }

    /// Saves the written objects. A run that starts other runs does this before it starts one.
    pub fn flush(&mut self) -> StoreResult<()> {
        self.pending = 0;
        if !self.writing {
            return Ok(());
        }
        self.writing = false;
        if let Err(error) = self.store.objects.execute_batch("COMMIT") {
            let _ = self.store.objects.execute_batch("ROLLBACK");
            return Err(error.into());
        }
        Ok(())
    }

    /// Completes the run with `snapshot` as its result and points `tag` at it. Inside a
    /// transaction of `records`.
    fn complete(
        &self,
        snapshot: Option<SnapshotId>,
        outcome: Option<Outcome>,
        tag: Option<&str>,
    ) -> StoreResult<()> {
        let counts = self.counts.map(|counts| {
            [
                counts.added as i64,
                counts.updated as i64,
                counts.deleted as i64,
                counts.unchanged as i64,
            ]
        });
        let count = |index: usize| counts.map(|counts| counts[index]);
        self.store.records.execute(
            "UPDATE runs SET state = 'complete', completed_at = ?, snapshot = ?, outcome = ?,
                degraded = ?, tag = ?, added = ?, updated = ?, deleted = ?, unchanged = ?,
                config = ?, stats = ?, cause = ? WHERE id = ?",
            params![
                now_ms() as i64,
                snapshot.map(|snapshot| snapshot.0),
                outcome.map(Outcome::as_str),
                self.degraded,
                tag,
                count(0),
                count(1),
                count(2),
                count(3),
                self.config.as_ref().map(ObjectId::as_str),
                canonical(&self.stats),
                self.cause,
                self.run
            ],
        )?;
        match (tag, snapshot) {
            (Some(tag), Some(snapshot)) => self.store.move_tag(tag, snapshot),
            _ => Ok(()),
        }
    }

    /// Ends a run that makes no snapshot, such as an optimization: what it did is in its
    /// configuration, its details and the runs that name it as their cause.
    pub fn finish(mut self) -> StoreResult<String> {
        self.flush()?;
        transact(&self.store.records, || self.complete(None, None, None))?;
        self.done = true;
        Ok(self.run.clone())
    }

    /// Ends the run with a snapshot that `existing` found, and points `tag` at it.
    pub fn finish_reused(
        mut self,
        snapshot: SnapshotId,
        tag: Option<&str>,
    ) -> StoreResult<Committed> {
        self.flush()?;
        transact(&self.store.records, || {
            self.complete(Some(snapshot), Some(Outcome::Reused), tag)
        })?;
        self.done = true;
        Ok(Committed {
            run: self.run.clone(),
            snapshot,
            outcome: Outcome::Reused,
        })
    }

    /// Saves the snapshot, completes the run and points `tag` at the result, all at once.
    ///
    /// When the snapshot `tag` points at, or `previous`, holds the same items, no snapshot is
    /// made and that one is the result.
    pub fn commit(
        mut self,
        tag: Option<&str>,
        previous: Option<SnapshotId>,
    ) -> StoreResult<Committed> {
        self.flush()?;
        let store = self.store;
        let items = canonical(&list_value(&self.items));
        let (snapshot, outcome) = transact(&store.records, || {
            let tagged = match tag {
                Some(tag) => store.tagged(tag)?,
                None => None,
            };
            let mut same = None;
            for candidate in [tagged, previous].into_iter().flatten() {
                let holds: Option<(String, String)> = store
                    .records
                    .query_row(
                        "SELECT type, items FROM snapshots WHERE id = ?",
                        [candidate.0],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if holds.is_some_and(|(kind, saved)| kind == self.kind && saved == items) {
                    same = Some(candidate);
                    break;
                }
            }
            let result = match same {
                Some(snapshot) => (snapshot, Outcome::Unchanged),
                None => {
                    store.records.execute(
                        "INSERT INTO snapshots (type, items, count, parents, created_at)
                         VALUES (?, ?, ?, ?, ?)",
                        params![
                            self.kind,
                            items,
                            self.items.len() as i64,
                            ids_json(&self.inputs),
                            now_ms() as i64
                        ],
                    )?;
                    (
                        SnapshotId(store.records.last_insert_rowid()),
                        Outcome::Created,
                    )
                }
            };
            self.complete(Some(result.0), Some(result.1), tag)?;
            Ok(result)
        })?;
        self.done = true;
        Ok(Committed {
            run: self.run.clone(),
            snapshot,
            outcome,
        })
    }
}

impl Drop for Draft<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = self.flush();
        // A run that failed still says what started it and with which configuration.
        let _ = self.store.records.execute(
            "UPDATE runs SET state = 'failed', completed_at = ?, config = ?, cause = ?
             WHERE id = ? AND state = 'building'",
            params![
                now_ms() as i64,
                self.config.as_ref().map(ObjectId::as_str),
                self.cause,
                self.run
            ],
        );
    }
}
