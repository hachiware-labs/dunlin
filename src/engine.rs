use std::collections::{BTreeMap, HashSet};
use std::ops::Bound::Included;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tantivy::collector::{Count, TopDocs};
use tantivy::directory::{Directory, Lock, MmapDirectory};
use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, EnableScoring, Occur, PhraseQuery, Query,
    RangeQuery, RegexQuery, TermQuery, TermSetQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::WhitespaceTokenizer;
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TERMINATED,
    TantivyDocument, Term, doc,
};
use url::Url;

use crate::{
    Cancellation, CorpusBackend, DocumentFormat, Error, Evidence, IndexChanges, IndexStatus,
    LineRange, ReadOutcome, Result, RetrievalMethod, SearchHit, SyncOutcome,
    corpus::{Corpus, CorpusBatch},
    japanese::{GAP, JapaneseTokenizer},
    legacy_delete::LegacyDeleteQuery,
    source::{self, SourceInput, SourceObject},
};

const MEMBERSHIP_MARKER: &str = "@member-v1";

#[derive(Default)]
pub(crate) struct KeywordOptions<'a> {
    pub ngrams: bool,
    pub bigrams: bool,
    pub japanese: bool,
    pub user_dictionary: Option<&'a Path>,
}

enum KeywordTokenizer {
    Word,
    Trigram { bigrams: bool },
    Japanese(Box<JapaneseTokenizer>),
}

impl KeywordTokenizer {
    fn open(options: KeywordOptions<'_>) -> Result<Self> {
        if options.japanese {
            if options.ngrams || options.bigrams {
                return Err(Error::Invalid(
                    "Choose Japanese keyword tokenization or n-grams",
                ));
            }
            Ok(Self::Japanese(Box::new(JapaneseTokenizer::open(
                options.user_dictionary,
            )?)))
        } else if options.ngrams {
            Ok(Self::Trigram {
                bigrams: options.bigrams,
            })
        } else {
            Ok(Self::Word)
        }
    }

    fn indexed_terms(&self, text: &str) -> Result<String> {
        match self {
            Self::Japanese(tokenizer) => Ok(tokenizer.terms(text)?.join(" ")),
            Self::Trigram { bigrams } => Ok(indexed_terms(text, true, *bigrams)),
            Self::Word => Ok(indexed_terms(text, false, false)),
        }
    }

    fn ngrams(&self) -> bool {
        matches!(self, Self::Trigram { .. })
    }
}

pub(crate) struct KeywordIndex {
    pub(crate) corpus: Corpus,
    index: Index,
    reader: IndexReader,
    source_id: Field,
    object_key: Field,
    title: Field,
    title_terms: Field,
    body_terms: Field,
    evidence: Field,
    content: Field,
    tokenizer: KeywordTokenizer,
}

impl KeywordIndex {
    pub fn inspect(path: &Path, cancel: &Cancellation) -> Result<Option<IndexStatus>> {
        cancel.check()?;
        match std::fs::metadata(path) {
            Ok(entry) if entry.is_dir() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            _ => return Err(Error::Index),
        }
        let directory = MmapDirectory::open(path).map_err(|_| Error::Index)?;
        if !Index::exists(&directory).map_err(|_| Error::Index)? {
            return Ok(None);
        }
        let index = Index::open(directory)?;
        let schema = index.schema();
        let query = primary_documents(schema.get_field("object_key")?)?;
        let evidence = schema.get_field("evidence")?;
        let reader: IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        let mut sources = BTreeMap::new();
        let mut searchable_documents = 0;
        visit_documents(&searcher, &query, cancel, |document| {
            let object: Evidence =
                serde_json::from_str(stored(&document, evidence)?).map_err(|_| Error::Index)?;
            *sources.entry(object.source.source_id).or_insert(0) += 1;
            searchable_documents += usize::from(object.range.end_line > 0);
            Ok(())
        })?;
        Ok(Some(IndexStatus {
            revision: index_revision(&searcher),
            sources,
            searchable_documents,
        }))
    }

    pub fn open(
        path: &Path,
        corpus_path: &Path,
        backend: CorpusBackend,
        options: KeywordOptions<'_>,
        cancel: &Cancellation,
    ) -> Result<Self> {
        let tokenizer = KeywordTokenizer::open(options)?;
        let mut schema = Schema::builder();
        let terms = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("dunlin-words")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );
        let source_id = schema.add_text_field("source_id", STRING);
        let object_key = schema.add_text_field("object_key", STRING);
        let title = schema.add_text_field("title", STORED);
        let title_terms = schema.add_text_field("title_terms", terms.clone());
        let body_terms = schema.add_text_field("body_terms", terms);
        let evidence = schema.add_text_field("evidence", STORED);
        let content = schema.add_text_field("content", STORED);
        let directory = MmapDirectory::open(path).map_err(|_| Error::Index)?;
        let _initialize = if Index::exists(&directory).map_err(|_| Error::Index)? {
            None
        } else {
            Some(
                directory
                    .acquire_lock(&Lock {
                        filepath: ".dunlin-init.lock".into(),
                        is_blocking: true,
                    })
                    .map_err(|_| Error::Busy)?,
            )
        };
        let index = Index::open_or_create(directory, schema.build())?;
        index
            .tokenizers()
            .register("dunlin-words", WhitespaceTokenizer::default());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(Self {
            corpus: Corpus::open(corpus_path, backend, cancel)?,
            index,
            reader,
            source_id,
            object_key,
            title,
            title_terms,
            body_terms,
            evidence,
            content,
            tokenizer,
        })
    }

    pub fn replace_source(
        &self,
        id: &str,
        input: SourceInput<'_>,
        cancel: &Cancellation,
    ) -> Result<SyncOutcome> {
        cancel.check()?;
        let mut writer = self
            .index
            .writer_with_num_threads::<TantivyDocument>(1, 32_000_000)?;
        writer.delete_term(Term::from_field_text(self.source_id, id));
        let mut corpus = self.corpus.batch();
        let result = input.ingest(id, cancel, |object| {
            self.index_object(&writer, object, &mut corpus, cancel)
        });
        let report = finish_or_rollback(
            &mut writer,
            result.and_then(|report| {
                cancel.check()?;
                corpus.flush()?;
                Ok(report)
            }),
        )?;
        writer.commit()?;
        self.reader.reload()?;
        Ok(report)
    }

    /// Indexes the objects of `input` that changed and deletes the ones that are gone. Objects in
    /// `keep` stay as indexed without being read again.
    pub fn sync_source(
        &self,
        id: &str,
        input: SourceInput<'_>,
        keep: &HashSet<String>,
        cancel: &Cancellation,
    ) -> Result<IndexChanges> {
        cancel.check()?;
        // Take the writer lock before loading the comparison snapshot, including across handles.
        let mut writer = self
            .index
            .writer_with_num_threads::<TantivyDocument>(1, 32_000_000)?;
        self.reader.reload()?;
        let snapshot = self.snapshot();
        let mut previous = snapshot.source_objects(id, cancel)?;
        let legacy_layouts = if previous.values().any(|old| old.known_format().is_none()) {
            snapshot.legacy_layouts(id, cancel)?
        } else {
            BTreeMap::new()
        };
        let mut legacy = HashSet::new();
        let mut changes = IndexChanges {
            source_id: id.into(),
            ..IndexChanges::default()
        };
        let mut corpus = self.corpus.batch();
        let result = input
            .ingest(id, cancel, |object| {
                let object_id = &object.source.object_id;
                match previous.remove(object_id) {
                    Some(old) if old.matches(&object, legacy_layouts.get(object_id))? => {
                        changes.unchanged += 1;
                        return Ok(0);
                    }
                    Some(old) => {
                        self.delete_object(&writer, id, &old, &mut legacy);
                        changes.updated.push(object_id.clone());
                    }
                    None => changes.added.push(object_id.clone()),
                }
                let units = self.index_object(&writer, object, &mut corpus, cancel)?;
                changes.indexed_units += units;
                Ok(units)
            })
            .and_then(|_| {
                for (object_id, old) in previous {
                    cancel.check()?;
                    if keep.contains(&object_id) {
                        changes.unchanged += 1;
                        continue;
                    }
                    self.delete_object(&writer, id, &old, &mut legacy);
                    changes.deleted.push(object_id);
                }
                if !legacy.is_empty() {
                    writer.delete_query(Box::new(LegacyDeleteQuery {
                        candidates: self.legacy_candidates(id),
                        evidence: self.evidence,
                        objects: Arc::new(legacy),
                    }))?;
                }
                cancel.check()?;
                corpus.flush()
            });
        finish_or_rollback(&mut writer, result)?;
        if !changes.added.is_empty() || !changes.updated.is_empty() || !changes.deleted.is_empty() {
            writer.commit()?;
            self.reader.reload()?;
        }
        changes.added.sort_unstable();
        changes.updated.sort_unstable();
        Ok(changes)
    }

    fn legacy_candidates(&self, id: &str) -> BooleanQuery {
        BooleanQuery::new(vec![
            (Occur::Must, Box::new(exact(self.source_id, id))),
            (
                Occur::MustNot,
                Box::new(exact(self.object_key, MEMBERSHIP_MARKER)),
            ),
        ])
    }

    fn delete_object(
        &self,
        writer: &IndexWriter<TantivyDocument>,
        id: &str,
        old: &StoredObject,
        legacy: &mut HashSet<String>,
    ) {
        let object_id = &old.evidence.source.object_id;
        if old.format.is_some() {
            writer.delete_term(Term::from_field_text(
                self.object_key,
                &membership_key(id, object_id),
            ));
        } else {
            legacy.insert(object_id.clone());
        }
    }

    fn index_object(
        &self,
        writer: &IndexWriter<TantivyDocument>,
        object: SourceObject,
        corpus: &mut CorpusBatch<'_>,
        cancel: &Cancellation,
    ) -> Result<usize> {
        let id = &object.source.source_id;
        let key = membership_key(id, &object.source.object_id);
        let mut units = 0;
        let mut end_line = 0;
        for (heading, text, range, byte_range) in object.units() {
            cancel.check()?;
            end_line = range.end_line;
            let title = unit_title(&object.source.object_id, heading);
            writer.add_document(doc!(
                self.source_id => id.as_str(),
                self.object_key => key.as_str(),
                self.object_key => MEMBERSHIP_MARKER,
                self.title => title.as_str(),
                self.title_terms => self.tokenizer.indexed_terms(&title)?,
                self.body_terms => self.tokenizer.indexed_terms(text)?,
                self.evidence => encode(&Evidence { byte_range, ..object.evidence(range) })?,
            ))?;
            units += 1;
        }
        let metadata = StoredObject {
            evidence: object.evidence(crate::LineRange {
                start_line: 1,
                end_line,
            }),
            format: Some(object.format()),
            chunk_layout: object.chunk_layout()?,
        };
        corpus.save(&object, end_line)?;
        writer.add_document(doc!(
            self.source_id => id.as_str(),
            self.object_key => object_key(id, &object.source.object_id),
            self.object_key => key,
            self.object_key => MEMBERSHIP_MARKER,
            self.object_key => url_key(&object.source.url),
            self.evidence => encode(&metadata)?,
            self.content => object.content,
        ))?;
        Ok(units)
    }

    pub fn snapshot(&self) -> KeywordSnapshot<'_> {
        KeywordSnapshot {
            index: self,
            searcher: self.reader.searcher(),
        }
    }

    pub fn current_snapshot(&self) -> Result<KeywordSnapshot<'_>> {
        self.reader.reload()?;
        Ok(self.snapshot())
    }

    pub fn read(&self, evidence: &Evidence) -> Result<ReadOutcome> {
        self.snapshot().read(evidence)
    }

    fn evidence(&self, document: &TantivyDocument) -> Result<Evidence> {
        serde_json::from_str(stored(document, self.evidence)?).map_err(|_| Error::Index)
    }
}

pub(crate) struct KeywordSnapshot<'a> {
    index: &'a KeywordIndex,
    searcher: Searcher,
}

impl KeywordSnapshot<'_> {
    pub fn documents(&self, cancel: &Cancellation) -> Result<Vec<ReadOutcome>> {
        let query = primary_documents(self.index.object_key)?;
        let mut documents = Vec::new();
        visit_documents(&self.searcher, &query, cancel, |document| {
            documents.push(ReadOutcome {
                evidence: self.index.evidence(&document)?,
                content: stored(&document, self.index.content)?.to_owned(),
            });
            Ok(())
        })?;
        Ok(documents)
    }

    fn source_objects(
        &self,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<BTreeMap<String, StoredObject>> {
        // Only primary object keys have this range. URL aliases and search-unit keys use an @ prefix.
        let key_bound = |character: &str| {
            Included(Term::from_field_text(
                self.index.object_key,
                &format!("{id}:{}", character.repeat(64)),
            ))
        };
        let mut objects = BTreeMap::new();
        visit_documents(
            &self.searcher,
            &RangeQuery::new(key_bound("0"), key_bound("f")),
            cancel,
            |document| {
                let metadata: StoredObject =
                    serde_json::from_str(stored(&document, self.index.evidence)?)
                        .map_err(|_| Error::Index)?;
                let source = &metadata.evidence.source;
                if source.source_id != id
                    || objects.insert(source.object_id.clone(), metadata).is_some()
                {
                    return Err(Error::Index);
                }
                Ok(())
            },
        )?;
        Ok(objects)
    }

    fn legacy_layouts(
        &self,
        id: &str,
        cancel: &Cancellation,
    ) -> Result<BTreeMap<String, UnitLayout>> {
        let mut layouts: BTreeMap<String, UnitLayout> = BTreeMap::new();
        visit_documents(
            &self.searcher,
            &self.index.legacy_candidates(id),
            cancel,
            |document| {
                if let Some(title) = document
                    .get_first(self.index.title)
                    .and_then(|value| value.as_str())
                {
                    let evidence = self.index.evidence(&document)?;
                    layouts
                        .entry(evidence.source.object_id)
                        .or_default()
                        .push(unit_shape(title, evidence.range));
                }
                Ok(())
            },
        )?;
        for layout in layouts.values_mut() {
            layout.sort_unstable_by_key(|unit| (unit.0, unit.1));
        }
        Ok(layouts)
    }

    pub fn revision(&self) -> String {
        index_revision(&self.searcher)
    }

    pub fn search(
        &self,
        method: RetrievalMethod,
        input: &str,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<(Vec<SearchHit>, usize)> {
        self.search_scoped(method, input, limit, &[], cancel)
    }

    pub fn search_scoped(
        &self,
        method: RetrievalMethod,
        input: &str,
        limit: usize,
        sources: &[String],
        cancel: &Cancellation,
    ) -> Result<(Vec<SearchHit>, usize)> {
        let mut hits = Vec::with_capacity(limit);
        let count = self.visit_hits(method, input, limit, sources, cancel, |hit| hits.push(hit))?;
        Ok((hits, count))
    }

    pub fn visit_hits(
        &self,
        method: RetrievalMethod,
        input: &str,
        limit: usize,
        sources: &[String],
        cancel: &Cancellation,
        mut visit: impl FnMut(SearchHit),
    ) -> Result<usize> {
        cancel.check()?;
        let japanese_terms;
        let groups;
        let query = if let KeywordTokenizer::Japanese(tokenizer) = &self.index.tokenizer {
            validate_query_input(input)?;
            japanese_terms = tokenizer.terms(input)?;
            let mut seen = HashSet::new();
            groups = japanese_terms
                .iter()
                .map(String::as_str)
                .filter(|term| *term != GAP && seen.insert(*term))
                .map(|term| (term, false))
                .collect::<Vec<_>>();
            if groups.is_empty() {
                return Ok(0);
            }
            validate_query_size(&groups)?;
            self.japanese_query(&groups, &japanese_terms)
        } else {
            groups = query_groups(input)?;
            self.any_term(&groups)
        };
        let (top, count) = match method {
            RetrievalMethod::Coverage if groups.len() > 1 => {
                self.coverage(&groups, query, limit, sources, cancel)?
            }
            RetrievalMethod::Keyword | RetrievalMethod::Coverage => {
                self.keyword(query, limit, sources)?
            }
        };
        for (rank, (score, address)) in top.into_iter().enumerate() {
            cancel.check()?;
            let document: TantivyDocument = self.searcher.doc(address)?;
            visit(SearchHit {
                title: stored(&document, self.index.title)?.to_owned(),
                evidence: self.index.evidence(&document)?,
                // A combined result's score reflects its rank, including the strict-first tier.
                score: match method {
                    RetrievalMethod::Keyword => score,
                    RetrievalMethod::Coverage => 1.0 / (rank + 1) as f32,
                },
            });
        }
        Ok(count)
    }

    fn keyword(
        &self,
        query: BooleanQuery,
        limit: usize,
        sources: &[String],
    ) -> Result<(Vec<(f32, DocAddress)>, usize)> {
        let query = self.scoped(query, sources);
        Ok(self.searcher.search(
            query.as_ref(),
            &(TopDocs::with_limit(limit).order_by_score(), Count),
        )?)
    }

    fn coverage(
        &self,
        groups: &[(&str, bool)],
        any: BooleanQuery,
        limit: usize,
        sources: &[String],
        cancel: &Cancellation,
    ) -> Result<(Vec<(f32, DocAddress)>, usize)> {
        let all_terms = BooleanQuery::intersection(
            groups
                .iter()
                .map(|&(word, cjk)| {
                    Box::new(BooleanQuery::union(self.term_queries(word, cjk).into()))
                        as Box<dyn Query>
                })
                .collect(),
        );
        let all_terms = self.scoped(all_terms, sources);
        let mut top = self.searcher.search(
            all_terms.as_ref(),
            &TopDocs::with_limit(limit).order_by_score(),
        )?;
        cancel.check()?;
        let count = if top.len() == limit {
            self.searcher
                .search(self.scoped(any, sources).as_ref(), &Count)?
        } else {
            let (fallback, count) = self.keyword(any, limit, sources)?;
            let strict = top
                .iter()
                .map(|&(_, address)| address)
                .collect::<std::collections::HashSet<_>>();
            top.extend(
                fallback
                    .into_iter()
                    .filter(|&(_, address)| !strict.contains(&address))
                    .take(limit - top.len()),
            );
            count
        };
        Ok((top, count))
    }

    fn any_term(&self, groups: &[(&str, bool)]) -> BooleanQuery {
        BooleanQuery::union(
            groups
                .iter()
                .flat_map(|&(word, cjk)| self.term_queries(word, cjk))
                .collect(),
        )
    }

    fn japanese_query(&self, groups: &[(&str, bool)], terms: &[String]) -> BooleanQuery {
        let mut clauses: Vec<Box<dyn Query>> = vec![Box::new(self.any_term(groups))];
        let mut seen = HashSet::new();
        for pair in terms.windows(2) {
            if pair.iter().any(|term| term == GAP) || !seen.insert((&pair[0], &pair[1])) {
                continue;
            }
            for (field, boost) in [(self.index.title_terms, 1.0), (self.index.body_terms, 0.5)] {
                let phrase = PhraseQuery::new(
                    pair.iter()
                        .map(|term| Term::from_field_text(field, term))
                        .collect(),
                );
                clauses.push(Box::new(BoostQuery::new(Box::new(phrase), boost)));
            }
        }
        BooleanQuery::union(clauses)
    }

    fn term_queries(&self, word: &str, cjk: bool) -> [Box<dyn Query>; 2] {
        [
            Box::new(BoostQuery::new(
                field_query(
                    self.index.title_terms,
                    word,
                    cjk,
                    self.index.tokenizer.ngrams(),
                ),
                2.0,
            )),
            field_query(
                self.index.body_terms,
                word,
                cjk,
                self.index.tokenizer.ngrams(),
            ),
        ]
    }

    fn scoped(&self, query: impl Query, sources: &[String]) -> Box<dyn Query> {
        if sources.is_empty() {
            return Box::new(query);
        }
        let terms = sources
            .iter()
            .map(|id| Term::from_field_text(self.index.source_id, id));
        let filter = ConstScoreQuery::new(Box::new(TermSetQuery::new(terms)), 0.0);
        Box::new(BooleanQuery::intersection(vec![
            Box::new(query),
            Box::new(filter),
        ]))
    }

    pub fn object(&self, source_id: &str, object_id: &str) -> Result<Option<ReadOutcome>> {
        self.find_object(&object_key(source_id, object_id), &[])
    }

    pub fn object_by_url(
        &self,
        url: &str,
        sources: &[String],
        cancel: &Cancellation,
    ) -> Result<Option<ReadOutcome>> {
        if let Some(object) = self.find_object(&url_key(url), sources)? {
            return Ok(Some(object));
        }
        if matches!(self.index.tokenizer, KeywordTokenizer::Japanese(_)) {
            return Ok(None);
        }
        // Older indices lack URL keys. Narrow by file name and verify the stored URL exactly.
        let path = Url::parse(url)
            .map_err(|_| Error::Index)?
            .to_file_path()
            .map_err(|_| Error::Index)?;
        let Some(name) = path.file_name().and_then(|part| part.to_str()) else {
            return Ok(None);
        };
        let clauses: Vec<_> = word_groups(name)
            .filter(|(word, _)| word.len() <= 240)
            .take(64)
            .map(|(word, cjk)| {
                field_query(
                    self.index.title_terms,
                    word,
                    cjk,
                    self.index.tokenizer.ngrams(),
                )
            })
            .collect();
        if clauses.is_empty() {
            return Ok(None);
        }
        let query = self.scoped(BooleanQuery::intersection(clauses), sources);
        let weight = query.weight(EnableScoring::disabled_from_searcher(&self.searcher))?;
        for (ordinal, segment) in self.searcher.segment_readers().iter().enumerate() {
            cancel.check()?;
            let mut scorer = weight.scorer(segment, 1.0)?;
            while scorer.doc() != TERMINATED {
                cancel.check()?;
                let doc_id = scorer.doc();
                if !segment.is_deleted(doc_id) {
                    let document: TantivyDocument =
                        self.searcher.doc(DocAddress::new(ordinal as u32, doc_id))?;
                    let source = self.index.evidence(&document)?.source;
                    if source.url == url {
                        return self.object(&source.source_id, &source.object_id);
                    }
                }
                scorer.advance();
            }
        }
        Ok(None)
    }

    fn find_object(&self, key: &str, sources: &[String]) -> Result<Option<ReadOutcome>> {
        self.find_document(key, sources)?
            .map(|document| {
                Ok(ReadOutcome {
                    evidence: self.index.evidence(&document)?,
                    content: stored(&document, self.index.content)?.to_owned(),
                })
            })
            .transpose()
    }

    fn find_document(&self, key: &str, sources: &[String]) -> Result<Option<TantivyDocument>> {
        let query = TermQuery::new(
            Term::from_field_text(self.index.object_key, key),
            IndexRecordOption::Basic,
        );
        let query = self.scoped(query, sources);
        let hits = self
            .searcher
            .search(query.as_ref(), &TopDocs::with_limit(1).order_by_score())?;
        let Some(&(_, address)) = hits.first() else {
            return Ok(None);
        };
        Ok(Some(self.searcher.doc(address)?))
    }

    fn read(&self, evidence: &Evidence) -> Result<ReadOutcome> {
        let requested = &evidence.source;
        let key = object_key(&requested.source_id, &requested.object_id);
        let document = self.find_document(&key, &[])?.ok_or(Error::NotFound)?;
        let mut current = self.index.evidence(&document)?;
        if current.source.version != requested.version || current.source.url != requested.url {
            return Err(Error::Stale);
        }
        let range = evidence.range;
        if range.start_line == 0
            || range.start_line > range.end_line
            || range.end_line > current.range.end_line
        {
            return Err(Error::Invalid(
                "Evidence range is outside the source object",
            ));
        }
        let full = stored(&document, self.index.content)?;
        let content = if let Some(bytes) = evidence.byte_range {
            let excerpt = full
                .get(bytes.start..bytes.end)
                .filter(|text| !text.is_empty())
                .ok_or(Error::Invalid(
                    "Evidence byte range is outside the source object",
                ))?;
            let start_line = full.as_bytes()[..bytes.start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            if start_line != range.start_line
                || start_line + excerpt.split_inclusive('\n').count() - 1 != range.end_line
            {
                return Err(Error::Invalid(
                    "Evidence byte range does not match source lines",
                ));
            }
            excerpt.to_owned()
        } else {
            source::select_lines(full, range)
        };
        current.range = range;
        current.byte_range = evidence.byte_range;
        Ok(ReadOutcome {
            evidence: current,
            content,
        })
    }
}

fn primary_documents(object_key: Field) -> Result<RegexQuery> {
    // Exclude the @-prefixed aliases and search-unit keys.
    Ok(RegexQuery::from_pattern(
        "[A-Za-z0-9_.-]{1,64}:[0-9a-f]{64}",
        object_key,
    )?)
}

fn visit_documents(
    searcher: &Searcher,
    query: &dyn Query,
    cancel: &Cancellation,
    mut visit: impl FnMut(TantivyDocument) -> Result<()>,
) -> Result<()> {
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    for (ordinal, segment) in searcher.segment_readers().iter().enumerate() {
        cancel.check()?;
        let mut scorer = weight.scorer(segment, 1.0)?;
        while scorer.doc() != TERMINATED {
            cancel.check()?;
            let doc_id = scorer.doc();
            if !segment.is_deleted(doc_id) {
                visit(searcher.doc(DocAddress::new(ordinal as u32, doc_id))?)?;
            }
            scorer.advance();
        }
    }
    Ok(())
}

fn index_revision(searcher: &Searcher) -> String {
    // Preserve segment order, which also determines the ordering of tied scores.
    let mut hash = blake3::Hasher::new();
    for segment in searcher.segment_readers() {
        let delete_opstamp = segment.delete_opstamp();
        hash.update(segment.segment_id().uuid_string().as_bytes());
        hash.update(&[u8::from(delete_opstamp.is_some())]);
        hash.update(&delete_opstamp.unwrap_or_default().to_le_bytes());
    }
    hash.finalize().to_hex().to_string()
}

pub(crate) fn query_groups(input: &str) -> Result<Vec<(&str, bool)>> {
    validate_query_input(input)?;
    let groups: Vec<_> = word_groups(input).take(65).collect();
    validate_query_size(&groups)?;
    Ok(groups)
}

fn validate_query_input(input: &str) -> Result<()> {
    if input.len() > 4096 {
        return Err(Error::Invalid("Query must be at most 4096 UTF-8 bytes"));
    }
    if word_groups(input).next().is_none() {
        return Err(Error::Invalid(
            "Query must contain a letter, digit, or identifier",
        ));
    }
    Ok(())
}

fn validate_query_size(groups: &[(&str, bool)]) -> Result<()> {
    if groups.len() > 64 || groups.iter().any(|(word, _)| word.len() > 240) {
        return Err(Error::Invalid(
            "Query must contain at most 64 terms of up to 240 bytes each",
        ));
    }
    Ok(())
}

fn stored(document: &TantivyDocument, field: Field) -> Result<&str> {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .ok_or(Error::Index)
}

fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|_| Error::Index)
}

#[derive(Deserialize, Serialize)]
struct StoredObject {
    #[serde(flatten)]
    evidence: Evidence,
    format: Option<DocumentFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chunk_layout: Option<String>,
}

impl StoredObject {
    fn known_format(&self) -> Option<DocumentFormat> {
        self.format.or_else(|| {
            let old = &self.evidence.source;
            // The old filesystem importer derived format from the filename.
            old.modified_at_ms
                .map(|_| source::filesystem_format(Path::new(&old.object_id)))
        })
    }

    fn matches(&self, object: &SourceObject, legacy_layout: Option<&UnitLayout>) -> Result<bool> {
        let old = &self.evidence.source;
        if old.version != object.source.version || old.url != object.source.url {
            return Ok(false);
        }
        if self.chunk_layout != object.chunk_layout()? {
            return Ok(false);
        }
        Ok(match self.known_format() {
            Some(format) => format == object.format(),
            // Imported objects from older versions did not store their format. Compare the
            // indexing effect of the format using the existing titles and line ranges.
            None => object
                .units()
                .map(|(heading, _, range, _)| {
                    unit_shape(&unit_title(&old.object_id, heading), range)
                })
                .eq(legacy_layout.into_iter().flatten().copied()),
        })
    }
}

type UnitShape = (usize, usize, blake3::Hash);
type UnitLayout = Vec<UnitShape>;

fn unit_shape(title: &str, range: LineRange) -> UnitShape {
    (
        range.start_line,
        range.end_line,
        blake3::hash(title.as_bytes()),
    )
}

fn unit_title(object_id: &str, heading: &str) -> String {
    if heading.is_empty() {
        object_id.to_owned()
    } else {
        format!("{object_id} — {heading}")
    }
}

fn finish_or_rollback<T>(
    writer: &mut IndexWriter<TantivyDocument>,
    result: Result<T>,
) -> Result<T> {
    if result.is_err() {
        writer.rollback()?;
    }
    result
}

fn exact(field: Field, key: &str) -> TermQuery {
    TermQuery::new(Term::from_field_text(field, key), IndexRecordOption::Basic)
}

fn object_key(id: &str, object_id: &str) -> String {
    format!("{id}:{}", blake3::hash(object_id.as_bytes()))
}

fn membership_key(id: &str, object_id: &str) -> String {
    format!("@member:{}", object_key(id, object_id))
}

fn url_key(url: &str) -> String {
    format!("@url:{}", blake3::hash(url.as_bytes()))
}

fn field_query(field: Field, word: &str, cjk: bool, ngrams: bool) -> Box<dyn Query> {
    let lower = word.to_lowercase();
    let length = lower.chars().count();
    if cjk && ngrams && length > 1 {
        let terms = character_grams(&lower, length.min(3))
            .into_iter()
            .map(|gram| Term::from_field_text(field, &format!("@{gram}")))
            .collect::<Vec<_>>();
        if terms.len() == 1 {
            return Box::new(TermQuery::new(
                terms[0].clone(),
                IndexRecordOption::WithFreqs,
            ));
        }
        Box::new(PhraseQuery::new(terms))
    } else if cjk && !ngrams && length > 1 {
        let terms = lower
            .chars()
            .map(|ch| Term::from_field_text(field, ch.encode_utf8(&mut [0; 4])));
        Box::new(PhraseQuery::new(terms.collect()))
    } else {
        Box::new(TermQuery::new(
            Term::from_field_text(field, &lower),
            IndexRecordOption::WithFreqs,
        ))
    }
}

fn character_grams(word: &str, n: usize) -> Vec<String> {
    word.chars()
        .collect::<Vec<_>>()
        .windows(n)
        .map(|chars| chars.iter().collect())
        .collect()
}

fn indexed_terms(text: &str, ngrams: bool, bigrams: bool) -> String {
    let mut terms = String::with_capacity(text.len());
    for (word, cjk) in word_groups(text) {
        if cjk {
            for ch in word.chars() {
                terms.extend(ch.to_lowercase());
                terms.push(' ');
            }
            if ngrams {
                let lower = word.to_lowercase();
                for n in [2, 3] {
                    if n == 2 && !bigrams {
                        continue;
                    }
                    terms.push_str("! ");
                    for gram in character_grams(&lower, n) {
                        terms.push('@');
                        terms.push_str(&gram);
                        terms.push(' ');
                    }
                }
            }
        } else if word.len() <= 240 {
            terms.push_str(&word.to_lowercase());
            terms.push(' ');
        }
        // A position gap prevents phrases from crossing word or punctuation boundaries.
        terms.push_str("! ");
    }
    terms
}

fn word_groups(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        let (start, kind) = loop {
            let (offset, ch) = chars.next()?;
            if let Some(kind) = word_kind(ch) {
                break (offset, kind);
            }
        };
        while chars
            .next_if(|&(_, ch)| word_kind(ch) == Some(kind))
            .is_some()
        {}
        let end = chars.peek().map_or(text.len(), |&(offset, _)| offset);
        Some((&text[start..end], kind))
    })
}

fn word_kind(ch: char) -> Option<bool> {
    if matches!(ch,
        '\u{3041}'..='\u{309f}' | '\u{30a1}'..='\u{30fa}' | '\u{30fc}'..='\u{30ff}'
        | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{323af}' | '々' | '〇'
    ) {
        Some(true)
    } else if ch.is_alphanumeric() || ch == '_' {
        Some(false)
    } else {
        None
    }
}
