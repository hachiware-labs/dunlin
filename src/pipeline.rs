//! A build on the new store: `extract`, `transform` and `index` one after the other
//! (docs/data-management.md, 3.3).
//!
//! Each stage takes the snapshot the stage before it made. A stage whose input and settings are
//! the ones of an earlier run is not run again.
//!
//! `run` does what a configuration file says: the build, and the evaluation when the file names
//! evaluation data. A trial of an optimization is such a run of a file of its own.

use std::path::Path;

use crate::assess::{EvalRequest, Report, evaluate};
use crate::extract::{Apply, ExtractOutcome, ExtractRequest, extract};
use crate::index::{IndexOutcome, IndexRequest, index};
use crate::settings::{Settings, Stage};
use crate::store::{Outcome, SnapshotId, Store, StoreError, StoreResult};
use crate::transform::{Chunking, Context, TransformOutcome, TransformRequest, transform};

pub struct BuildRequest<'a> {
    /// The ID of the source the folder is.
    pub source: &'a str,
    /// The folder of documents.
    pub root: &'a Path,
    /// Where the directories of indexes are made.
    pub indexes: &'a Path,
    /// The configuration file as it is now. It is kept with the runs.
    pub config: Option<&'a [u8]>,
    /// Which changes of the folder are taken in.
    pub apply: Apply,
    /// The tags of the three sections.
    pub extract_tag: &'a str,
    pub transform_tag: &'a str,
    pub index_tag: &'a str,
    /// The name that is served. A build with the project's own configuration moves it to the
    /// index it made; a trial leaves it where it is.
    pub release: Option<&'a str>,
    /// Maximum characters per chunk.
    pub size: usize,
    /// Characters repeated from the end of the previous chunk.
    pub overlap: usize,
}

#[derive(Debug)]
pub struct BuildOutcome {
    pub extract: ExtractOutcome,
    pub transform: TransformOutcome,
    pub index: IndexOutcome,
}

/// Brings the documents, their chunks and the index up to date.
pub fn build(store: &Store, request: &BuildRequest<'_>) -> StoreResult<BuildOutcome> {
    let extract = extract(
        store,
        &ExtractRequest {
            source: request.source,
            root: request.root,
            tag: Some(request.extract_tag),
            config: request.config,
            apply: request.apply,
            cause: None,
        },
    )?;
    let transform = transform(
        store,
        &TransformRequest {
            input: extract.extracted.snapshot,
            chunking: Chunking::Length {
                size: request.size,
                overlap: request.overlap,
            },
            context: Context::default(),
            tag: Some(request.transform_tag),
            config: request.config,
            cause: None,
        },
    )?;
    let index = index(
        store,
        &IndexRequest {
            input: transform.chunks.snapshot,
            directory: request.indexes,
            tag: Some(request.index_tag),
            config: request.config,
            cause: None,
        },
    )?;
    if let Some(release) = request.release {
        store.release(release, index.index.snapshot)?;
    }
    Ok(BuildOutcome {
        extract,
        transform,
        index,
    })
}

/// A configuration file to run.
pub struct RunRequest<'a> {
    /// The folder that relative paths of the configuration start at.
    pub project: &'a Path,
    /// Where the directories of indexes are made.
    pub indexes: &'a Path,
    /// The configuration file, in the syntax of the new store.
    pub config: &'a str,
    /// Evaluate the index as the `eval` section says, as `dunlin eval` and a trial do. A build
    /// leaves the section alone.
    pub evaluate: bool,
    /// Move the name `serve` gives to the index. A trial leaves it where it is.
    pub release: bool,
    /// Point `default` of each kind at what this run makes, as a run by hand does: the latest
    /// documents, chunks, index and result are then found under that name. A trial leaves
    /// `default` where it is and names only what its configuration names.
    pub latest: bool,
    /// The run that started this one, such as an optimization.
    pub cause: Option<&'a str>,
    /// Hears each stage when it starts, and again with how it ended.
    pub progress: &'a dyn Fn(&str, Option<Outcome>),
}

/// What a run of a configuration file made.
#[derive(Debug)]
pub struct RunOutcome {
    pub extracted: Option<SnapshotId>,
    /// What the scan of the folder found, when the run scanned it: the documents that were
    /// added, updated and deleted, and the changes `apply` left out.
    pub scan: Option<ExtractOutcome>,
    pub chunks: Option<SnapshotId>,
    /// The documents that were chunked in this run, when the run made chunks.
    pub chunked: Option<usize>,
    pub index: SnapshotId,
    /// The report of the evaluation, when the configuration names evaluation data.
    pub report: Option<SnapshotId>,
    pub result: Option<Report>,
    /// The stages that did work: they neither found their result by its key nor started from a
    /// name.
    pub ran: Vec<String>,
    /// The tags the configuration gave to what it made. `default` is not among them.
    pub tags: Vec<String>,
    /// The name that is served, when this run moved it.
    pub released: Option<String>,
}

/// The name of what a run by hand made last, for each kind of snapshot.
pub const LATEST: &str = "default";

/// The snapshot a section starts from.
fn named(store: &Store, section: &str, name: &str, kind: &str) -> StoreResult<SnapshotId> {
    let id = store.resolve(kind, name)?.ok_or_else(|| {
        StoreError::new(format!(
            "{section} starts from {name}, and nothing has that name."
        ))
    })?;
    if store.snapshot(id)?.kind != kind {
        return Err(StoreError::new(format!(
            "{section} starts from {name}, which is not what {section} makes."
        )));
    }
    Ok(id)
}

/// Does what a configuration file says: builds what is not there yet, and evaluates the index
/// when the file names evaluation data.
pub fn run(store: &Store, request: &RunRequest<'_>) -> StoreResult<RunOutcome> {
    let settings = Settings::parse(request.config).map_err(StoreError::new)?;
    let plan = settings.plan().map_err(StoreError::new)?;
    let config = Some(request.config.as_bytes());
    let cause = request.cause;
    let mut ran = Vec::new();
    let mut tags = Vec::new();
    // The name a stage gives its result: the one of its section, or `default` in a run by hand.
    let given = |tag: &Option<String>| match tag {
        Some(tag) => Some(tag.clone()),
        None => request.latest.then(|| LATEST.to_owned()),
    };
    let mut done =
        |stage: &str, made: &crate::store::Committed, tag: Option<&String>| -> StoreResult<()> {
            (request.progress)(stage, Some(made.outcome));
            if made.outcome != Outcome::Reused {
                ran.push(stage.to_owned());
            }
            tags.extend(tag.cloned());
            if request.latest {
                store.tag(LATEST, made.snapshot)?;
            }
            Ok(())
        };

    // What the evaluation needs is looked up before anything is built.
    let judged_with = match plan.eval.as_ref().filter(|_| request.evaluate) {
        None => None,
        Some(judging) => {
            let data = store.resolve("eval-data", &judging.data)?.ok_or_else(|| {
                StoreError::new(format!("There is no evaluation data {}.", judging.data))
            })?;
            if store.snapshot(data)?.kind != "eval-data" {
                return Err(StoreError::new(format!(
                    "{} is not evaluation data.",
                    judging.data
                )));
            }
            Some((judging, data))
        }
    };

    let mut scan = None;
    let mut chunked = None;
    let extracted = match &plan.extract {
        None => None,
        Some(Stage::From(name)) => Some(named(store, "extract", name, "extracted")?),
        Some(Stage::Make { what, tag }) => {
            (request.progress)("extract", None);
            let made = extract(
                store,
                &ExtractRequest {
                    source: &what.source,
                    root: &request.project.join(&what.path),
                    tag: given(tag).as_deref(),
                    config,
                    apply: what.apply,
                    cause,
                },
            )?;
            done("extract", &made.extracted, tag.as_ref())?;
            let snapshot = made.extracted.snapshot;
            scan = Some(made);
            Some(snapshot)
        }
    };
    let chunks = match (&plan.transform, extracted) {
        (None, _) => None,
        (Some(Stage::From(name)), _) => Some(named(store, "transform", name, "chunks")?),
        (Some(Stage::Make { what, tag }), Some(input)) => {
            (request.progress)("transform", None);
            let made = transform(
                store,
                &TransformRequest {
                    input,
                    chunking: what.chunking,
                    context: what.context,
                    tag: given(tag).as_deref(),
                    config,
                    cause,
                },
            )?;
            done("transform", &made.chunks, tag.as_ref())?;
            chunked = Some(made.chunked);
            Some(made.chunks.snapshot)
        }
        (Some(Stage::Make { .. }), None) => {
            return Err(StoreError::new("transform has no documents to start from."));
        }
    };
    let indexed = match (&plan.index, chunks) {
        (Stage::From(name), _) => named(store, "index", name, "index")?,
        (Stage::Make { tag, .. }, Some(input)) => {
            (request.progress)("index", None);
            let made = index(
                store,
                &IndexRequest {
                    input,
                    directory: request.indexes,
                    tag: given(tag).as_deref(),
                    config,
                    cause,
                },
            )?;
            done("index", &made.index, tag.as_ref())?;
            made.index.snapshot
        }
        (Stage::Make { .. }, None) => {
            return Err(StoreError::new("index has no chunks to start from."));
        }
    };

    let mut report = None;
    let mut result = None;
    if let Some((judging, data)) = judged_with {
        (request.progress)("eval", None);
        let judged = evaluate(
            store,
            &EvalRequest {
                index: indexed,
                data,
                retrieval: &plan.retrieval(),
                top_k: plan.top_k,
                split: &judging.split,
                directory: request.indexes,
                tag: given(&judging.tag).as_deref(),
                config,
                cause,
            },
        )?;
        done("eval", &judged.report, judging.tag.as_ref())?;
        report = Some(judged.report.snapshot);
        result = Some(judged.result);
    }

    let released = match &plan.release {
        Some(name) if request.release => {
            store.release(name, indexed)?;
            Some(name.clone())
        }
        _ => None,
    };
    Ok(RunOutcome {
        extracted,
        scan,
        chunks,
        chunked,
        index: indexed,
        report,
        result,
        ran,
        tags,
        released,
    })
}
