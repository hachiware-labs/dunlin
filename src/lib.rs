//! Local, single-tenant knowledge indexing and evidence retrieval.

pub mod assess;
pub mod chunk;
pub mod config_check;
pub mod config_value;
mod corpus;
mod engine;
mod evaluate;
mod evidence_match;
mod excerpt;
mod explore;
pub mod extract;
pub mod glob;
pub mod index;
mod japanese;
mod legacy_delete;
#[cfg(any(feature = "node", test))]
mod markdown;
mod model;
mod node_path;
pub mod optimize;
pub mod pipeline;
mod plans;
pub mod records;
pub mod registry;
pub mod settings;
mod source;
mod storage;
pub mod store;
mod sync;
pub mod tpe;
pub mod transform;
pub mod yaml_config;
#[cfg(any(feature = "node", test))]
mod yaml_emit;

#[cfg(any(feature = "node", test))]
mod adoption;
#[cfg(any(feature = "node", test))]
mod build_stage;
#[cfg(any(feature = "node", test))]
mod chunk_context;
#[cfg(any(feature = "node", test))]
mod cli_args;
#[cfg(all(feature = "node", not(test)))]
mod cli_dictionary;
#[cfg(all(feature = "node", not(test)))]
mod cli_models;
#[cfg(all(feature = "node", not(test)))]
mod cli_project;
#[cfg(all(feature = "node", not(test)))]
mod cli_run;
#[cfg(all(feature = "node", not(test)))]
mod cli_store;
#[cfg(any(feature = "node", test))]
mod cli_text;
#[cfg(all(feature = "node", not(test)))]
mod cli_tools;
#[cfg(any(feature = "node", test))]
mod command_parser;
#[cfg(any(feature = "node", test))]
mod embed;
#[cfg(any(feature = "node", test))]
mod eval_data;
#[cfg(any(feature = "node", test))]
mod eval_generate;
#[cfg(any(feature = "node", test))]
mod eval_report;
#[cfg(any(feature = "node", test))]
mod eval_stage;
#[cfg(any(feature = "node", test))]
mod generate;
#[cfg(any(feature = "node", test))]
mod html_text;
#[cfg(any(feature = "node", test))]
mod http_agent;
#[cfg(any(feature = "node", test))]
mod index_stage;
#[cfg(any(feature = "node", test))]
mod lighton;
#[cfg(all(feature = "node", not(test)))]
mod mcp;
#[cfg(all(feature = "node", not(test)))]
mod node;
#[cfg(any(feature = "node", test))]
mod ocr;
#[cfg(any(feature = "node", test))]
mod office;
#[cfg(any(feature = "node", test))]
mod pdf_text;
#[cfg(any(feature = "node", test))]
mod search_stage;
#[cfg(all(feature = "node", not(test)))]
mod serve;
#[cfg(any(feature = "node", test))]
mod source_files;
#[cfg(any(feature = "node", test))]
mod source_load;
#[cfg(any(feature = "node", test))]
mod source_scan;
#[cfg(any(feature = "node", test))]
mod source_stage;
#[cfg(any(feature = "node", test))]
mod stage_store;
#[cfg(any(feature = "node", test))]
mod transform_stage;
#[cfg(any(feature = "node", test))]
mod trial;
#[cfg(any(feature = "node", test))]
mod vector;
#[cfg(any(feature = "node", test))]
mod wiki_browser;
#[cfg(any(feature = "node", test))]
mod wiki_x;
#[cfg(any(feature = "node", test))]
mod wiki_commands;
#[cfg(any(feature = "node", test))]
mod wiki_fetch;
#[cfg(any(feature = "node", test))]
mod wiki_folder;
#[cfg(any(feature = "node", test))]
mod wiki_html;
#[cfg(any(feature = "node", test))]
mod wiki_ingress;
#[cfg(any(feature = "node", test))]
mod wiki_inspect;
#[cfg(any(feature = "node", test))]
mod wiki_project;
#[cfg(all(feature = "node", not(test)))]
mod wiki_serve;
#[cfg(any(feature = "node", test))]
mod wiki_work;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use corpus::{CorpusBackend, CorpusDocument, CorpusMetadata, ParserInfo, SourceLocation};
use engine::{KeywordIndex, KeywordOptions};
pub use evaluate::{
    CandidateMetrics, EvaluationCase, EvaluationOptions, EvaluationReport, EvaluationSet,
    EvidenceQuote, OptimizationReport, OptimizeOptions,
};
pub use excerpt::EvidenceExcerpt;
pub use explore::{
    ExploreEvidence, ExploreIssue, ExploreOptions, ExploreOutcome, ExploreStop, ExploreVia,
};
pub use model::{
    ByteRange, Cancellation, DocumentFormat, DocumentRef, DocumentSnapshot, Error, Evidence,
    IndexChanges, IndexStatus, LineRange, ReadOutcome, Result, SearchHit, SearchOutcome,
    SourceDocument, SourceRef, SyncOutcome,
};
use plans::{DEFAULT_LIMIT, PlanStore};
pub use plans::{PlanEvaluation, RetrievalMethod, RetrievalPlan};
use source::{DocumentInput, SourceInput};
pub use sync::DeltaSyncOutcome;

pub struct KnowledgeBase {
    index: KeywordIndex,
    plans: PlanStore,
}

impl KnowledgeBase {
    /// Inspects the current keyword index without creating a knowledge base.
    pub fn inspect_index(path: &Path, cancel: &Cancellation) -> Result<Option<IndexStatus>> {
        KeywordIndex::inspect(&path.join(".knowledge-nest-index-v1"), cancel)
    }

    pub fn open(path: &Path, cancel: &Cancellation) -> Result<Self> {
        Self::open_with_corpus(path, CorpusBackend::default(), cancel)
    }

    pub fn open_with_corpus(
        path: &Path,
        backend: CorpusBackend,
        cancel: &Cancellation,
    ) -> Result<Self> {
        Self::open_with_options(path, backend, None, KeywordOptions::default(), cancel)
    }

    pub(crate) fn open_with_options(
        path: &Path,
        backend: CorpusBackend,
        corpus_path: Option<&Path>,
        keyword: KeywordOptions<'_>,
        cancel: &Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        let index_path = path.join(".knowledge-nest-index-v1");
        std::fs::create_dir_all(&index_path)
            .map_err(|_| Error::Io("Could not create the index directory"))?;
        let index = KeywordIndex::open(
            &index_path,
            corpus_path.unwrap_or(&path.join(".knowledge-nest-corpus-v1")),
            backend,
            keyword,
            cancel,
        )?;
        let plans = PlanStore::open(&path.join(".knowledge-nest-plans-v1"))?;
        cancel.check()?;
        Ok(Self { index, plans })
    }

    pub fn add_source(
        &self,
        id: &str,
        path: PathBuf,
        cancel: &Cancellation,
    ) -> Result<SyncOutcome> {
        source::validate_id(id)?;
        self.index
            .replace_source(id, SourceInput::Filesystem(&path), cancel)
    }

    /// Replaces the source with the supplied documents in a single commit.
    pub fn replace_documents(
        &self,
        id: &str,
        documents: Vec<SourceDocument>,
        cancel: &Cancellation,
    ) -> Result<SyncOutcome> {
        self.replace_document_inputs(id, documents.into_iter().map(Into::into).collect(), cancel)
    }

    pub(crate) fn replace_document_inputs(
        &self,
        id: &str,
        documents: Vec<DocumentInput>,
        cancel: &Cancellation,
    ) -> Result<SyncOutcome> {
        source::validate_id(id)?;
        self.index
            .replace_source(id, SourceInput::Documents(documents), cancel)
    }

    pub fn remove_source(&self, id: &str, cancel: &Cancellation) -> Result<()> {
        self.replace_documents(id, Vec::new(), cancel)?;
        Ok(())
    }

    pub fn sync_source(
        &self,
        id: &str,
        path: PathBuf,
        cancel: &Cancellation,
    ) -> Result<DeltaSyncOutcome> {
        sync::sync(
            self,
            id,
            SourceInput::Filesystem(&path),
            &Default::default(),
            cancel,
        )
    }

    pub fn sync_documents(
        &self,
        id: &str,
        documents: Vec<SourceDocument>,
        cancel: &Cancellation,
    ) -> Result<DeltaSyncOutcome> {
        self.sync_document_inputs(
            id,
            documents.into_iter().map(Into::into).collect(),
            &Default::default(),
            cancel,
        )
    }

    /// Syncs a source to `documents`. Objects in `keep` are left as indexed.
    pub(crate) fn sync_document_inputs(
        &self,
        id: &str,
        documents: Vec<DocumentInput>,
        keep: &std::collections::HashSet<String>,
        cancel: &Cancellation,
    ) -> Result<DeltaSyncOutcome> {
        sync::sync(self, id, SourceInput::Documents(documents), keep, cancel)
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<SearchOutcome> {
        self.search_with_method(query, Some(limit), None, cancel)
    }

    pub fn search_with_method(
        &self,
        query: &str,
        limit: Option<usize>,
        method: Option<RetrievalMethod>,
        cancel: &Cancellation,
    ) -> Result<SearchOutcome> {
        self.search_with_sources(query, limit, method, &[], cancel)
    }

    pub fn search_with_sources(
        &self,
        query: &str,
        limit: Option<usize>,
        method: Option<RetrievalMethod>,
        source_ids: &[String],
        cancel: &Cancellation,
    ) -> Result<SearchOutcome> {
        let start = Instant::now();
        cancel.check()?;
        source::validate_scope(source_ids)?;
        let (plan_id, method, default_limit) = match method {
            Some(method) => (method.plan_id().into(), method, DEFAULT_LIMIT),
            None => self.plans.active_route()?,
        };
        let limit = limit.unwrap_or(default_limit);
        if !(1..=100).contains(&limit) {
            return Err(Error::Invalid("Limit must be 1–100"));
        }
        let (hits, total_hits) = self
            .index
            .snapshot()
            .search_scoped(method, query, limit, source_ids, cancel)?;
        cancel.check()?;
        Ok(SearchOutcome {
            hits,
            total_hits,
            status: if total_hits < limit {
                "fewer_matches"
            } else {
                "complete"
            },
            engine: method.engine(),
            method,
            plan_id,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }

    pub fn explore(
        &self,
        query: &str,
        options: ExploreOptions,
        cancel: &Cancellation,
    ) -> Result<ExploreOutcome> {
        let start = Instant::now();
        options.validate()?;
        let deadline = start + Duration::from_millis(options.timeout_ms.into());
        let cancel = cancel.clone().with_deadline(Some(deadline));
        let route = self.plans.active_route()?;
        explore::explore(
            &self.index.snapshot(),
            query,
            route,
            options,
            &cancel,
            start,
        )
    }

    pub fn evaluate(
        &self,
        options: EvaluationOptions,
        cancel: &Cancellation,
    ) -> Result<EvaluationReport> {
        cancel.check()?;
        let locations = |document: &DocumentRef, version: &str| -> Result<Vec<SourceLocation>> {
            Ok(self
                .index
                .corpus
                .get(document, version)?
                .map(|document| document.metadata.locations)
                .unwrap_or_default())
        };
        evaluate::evaluate(&self.index.snapshot(), &locations, options, cancel)
    }

    pub fn optimize(
        &self,
        options: OptimizeOptions,
        cancel: &Cancellation,
    ) -> Result<OptimizationReport> {
        let budget = options.p95_latency_budget_ms;
        if !budget.is_finite() || budget <= 0.0 {
            return Err(Error::Invalid(
                "P95 latency budget must be finite and positive",
            ));
        }
        let report = self.evaluate(options.evaluation, cancel)?;
        let best = report
            .candidates
            .iter()
            .filter(|candidate| candidate.p95_latency_ms <= budget)
            .max_by(|left, right| {
                left.evidence_recall_at_k
                    .total_cmp(&right.evidence_recall_at_k)
                    .then_with(|| right.p95_latency_ms.total_cmp(&left.p95_latency_ms))
                    .then_with(|| right.plan_id.cmp(&left.plan_id))
            });
        let selected_plan_id = best
            .map(|candidate| {
                self.plans.save(
                    candidate.method,
                    PlanEvaluation {
                        p95_latency_budget_ms: budget,
                        report: serde_json::to_value(&report).map_err(|_| Error::Plan)?,
                    },
                    cancel,
                )
            })
            .transpose()?;
        Ok(OptimizationReport {
            status: if selected_plan_id.is_some() {
                "selected"
            } else {
                "budget_unmet"
            },
            selected_plan_id,
            p95_latency_budget_ms: budget,
            evaluation: report,
        })
    }

    pub fn get_active_plan(&self, cancel: &Cancellation) -> Result<RetrievalPlan> {
        cancel.check()?;
        self.plans.active()
    }

    pub fn activate_plan(&self, id: &str, cancel: &Cancellation) -> Result<RetrievalPlan> {
        self.plans.activate(id, cancel)
    }

    /// Exports stored documents and their line evidence from one index revision.
    pub fn snapshot_documents(&self, cancel: &Cancellation) -> Result<DocumentSnapshot> {
        cancel.check()?;
        let snapshot = self.index.snapshot();
        let documents = snapshot.documents(cancel)?;
        cancel.check()?;
        Ok(DocumentSnapshot {
            revision: snapshot.revision(),
            documents,
        })
    }

    pub fn get_index_revision(&self, cancel: &Cancellation) -> Result<String> {
        cancel.check()?;
        Ok(self.index.snapshot().revision())
    }

    /// Looks up a stored document by its stable source and object IDs.
    pub fn get_document(
        &self,
        document: &DocumentRef,
        cancel: &Cancellation,
    ) -> Result<Option<ReadOutcome>> {
        cancel.check()?;
        source::validate_id(&document.source_id)?;
        let result = self
            .index
            .snapshot()
            .object(&document.source_id, &document.object_id)?;
        cancel.check()?;
        Ok(result)
    }

    /// Reads the parsed document and its metadata for a saved revision, including old revisions.
    pub fn get_corpus_document(
        &self,
        document: &DocumentRef,
        version: &str,
        cancel: &Cancellation,
    ) -> Result<Option<CorpusDocument>> {
        cancel.check()?;
        source::validate_id(&document.source_id)?;
        let result = self.index.corpus.get(document, version)?;
        cancel.check()?;
        Ok(result)
    }

    /// Reads the text, character offsets and overlapping locations of evidence ranges from their
    /// saved revisions. Each document is loaded once per call. Missing revisions give `None`.
    pub fn describe_evidence(
        &self,
        items: &[Evidence],
        cancel: &Cancellation,
    ) -> Result<Vec<Option<EvidenceExcerpt>>> {
        fn key(evidence: &Evidence) -> (&str, &str, &str) {
            let source = &evidence.source;
            (&source.source_id, &source.object_id, &source.version)
        }
        let mut keys = Vec::new();
        for evidence in items {
            source::validate_id(&evidence.source.source_id)?;
            if !keys.contains(&key(evidence)) {
                keys.push(key(evidence));
            }
        }
        let load = |&(source_id, object_id, version): &(&str, &str, &str)| {
            cancel.check()?;
            let document = DocumentRef {
                source_id: source_id.to_owned(),
                object_id: object_id.to_owned(),
            };
            self.index.corpus.get(&document, version)
        };
        // Documents can be large; the addon parses them on the Rayon pool.
        #[cfg(all(feature = "node", not(test)))]
        let loaded: Vec<_> = {
            use rayon::prelude::*;
            keys.par_iter().map(load).collect::<Result<_>>()?
        };
        #[cfg(not(all(feature = "node", not(test))))]
        let loaded: Vec<_> = keys.iter().map(load).collect::<Result<_>>()?;
        items
            .iter()
            .map(|evidence| {
                cancel.check()?;
                let position = keys.iter().position(|item| *item == key(evidence));
                position
                    .and_then(|position| loaded[position].as_ref())
                    .map(|document| {
                        excerpt::excerpt(&document.content, &document.metadata.locations, evidence)
                    })
                    .transpose()
            })
            .collect()
    }

    pub fn read(&self, evidence: &Evidence, cancel: &Cancellation) -> Result<ReadOutcome> {
        cancel.check()?;
        source::validate_id(&evidence.source.source_id)?;
        let result = self.index.read(evidence)?;
        cancel.check()?;
        Ok(result)
    }
}
