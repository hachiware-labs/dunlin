use hannoy::{Database, Reader, Writer, distances::Cosine};
use heed::{EnvFlags, EnvOpenOptions};
use rand::{SeedableRng, rngs::StdRng};
use roaring::RoaringBitmap;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    error::Error,
    fs::{self, File},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Describe,
    Build {
        directory: PathBuf,
        vectors: PathBuf,
        dimensions: usize,
        rows: u64,
        inserts: Vec<Insert>,
        deletes: Vec<u32>,
        ef_construction: usize,
    },
    Search {
        directory: PathBuf,
        query: Vec<f32>,
        limit: usize,
        ef_search: usize,
        candidates: Option<Vec<u32>>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Insert {
    id: u32,
    row: u64,
}

fn main() {
    if let Err(error) = serve() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn serve() -> Result<()> {
    let request = serde_json::from_reader(io::stdin().lock())?;
    let response = match request {
        Request::Describe => json!({
            "protocol": 1, "name": "hannoy", "version": env!("CARGO_PKG_VERSION"),
            "modules": ["hannoy.index", "hannoy.search"],
            "capabilities": ["build", "search", "incremental", "filter_sources"]
        }),
        Request::Build {
            directory,
            vectors,
            dimensions,
            rows,
            inserts,
            deletes,
            ef_construction,
        } => build(
            &directory,
            &vectors,
            dimensions,
            rows,
            &inserts,
            &deletes,
            ef_construction,
        )?,
        Request::Search {
            directory,
            query,
            limit,
            ef_search,
            candidates,
        } => search(&directory, &query, limit, ef_search, candidates)?,
    };
    serde_json::to_writer(io::stdout().lock(), &response)?;
    Ok(())
}

fn build(
    directory: &Path,
    vectors: &Path,
    dimensions: usize,
    rows: u64,
    inserts: &[Insert],
    deletes: &[u32],
    ef_construction: usize,
) -> Result<Value> {
    if !(1..=65_536).contains(&dimensions) || ef_construction < 32 || rows == 0 {
        return Err("invalid vector index settings".into());
    }
    fs::create_dir_all(directory)?;
    let mut file = BufReader::new(File::open(vectors)?);
    let width = (dimensions * 4) as u64;
    let bytes = rows.checked_mul(width).ok_or("vector file is too large")?;
    if file.get_ref().metadata()?.len() != bytes {
        return Err("invalid vector file length".into());
    }
    let old_size = directory
        .join("data.mdb")
        .metadata()
        .map(|file| file.len())
        .unwrap_or(0);
    let map_size = old_size
        .saturating_mul(2)
        .saturating_add(bytes.saturating_mul(8))
        .saturating_add(rows.saturating_mul(4096))
        .max(64 * 1024 * 1024);
    let map_size = usize::try_from(map_size)?
        .checked_next_multiple_of(65_536)
        .ok_or("vector index is too large")?;
    // The owner passes a staging directory that only this process writes.
    let env = unsafe { EnvOpenOptions::new().map_size(map_size).open(directory)? };
    let mut txn = env.write_txn()?;
    let db: Database<Cosine> = env.create_database(&mut txn, None)?;
    let writer = Writer::new(db, 0, dimensions);
    for id in deletes {
        if !writer.del_item(&mut txn, *id)? {
            return Err("the previous index did not contain an item to delete".into());
        }
    }
    let mut buffer = vec![0; width as usize];
    let mut vector = vec![0.0; dimensions];
    for Insert { id, row } in inserts {
        if *row >= rows {
            return Err("invalid vector row".into());
        }
        file.seek(SeekFrom::Start(*row * width))?;
        file.read_exact(&mut buffer)?;
        for (value, bytes) in vector.iter_mut().zip(buffer.as_chunks::<4>().0) {
            *value = f32::from_le_bytes(*bytes);
        }
        writer.add_item(&mut txn, *id, &vector)?;
    }
    writer
        .builder(&mut StdRng::seed_from_u64(42))
        .ef_construction(ef_construction)
        .build::<16, 32>(&mut txn)?;
    txn.commit()?;
    Ok(json!({"inserted": inserts.len(), "deleted": deletes.len()}))
}

fn search(
    directory: &Path,
    query: &[f32],
    limit: usize,
    ef_search: usize,
    candidates: Option<Vec<u32>>,
) -> Result<Value> {
    let mut options = EnvOpenOptions::new();
    // The completed index is immutable and shared between read-only processes.
    let env = unsafe { options.flags(EnvFlags::READ_ONLY).open(directory)? };
    let txn = env.read_txn()?;
    let db: Database<Cosine> = env.open_database(&txn, None)?.ok_or("index is missing")?;
    let reader = Reader::open(&txn, 0, db)?;
    let allowed = candidates.map(RoaringBitmap::from_iter);
    let mut search = reader.nns(limit);
    search.ef_search(ef_search.max(limit)).linear_below(0);
    if let Some(allowed) = &allowed {
        search.candidates(allowed);
    }
    let result = search.by_vector(&txn, query)?.into_nns();
    Ok(json!({"hits": result}))
}
