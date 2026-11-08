use std::collections::HashSet;
use std::sync::Arc;

use tantivy::query::{
    BitSetDocSet, BooleanQuery, ConstScorer, EmptyScorer, EnableScoring, Explanation, Query,
    Scorer, Weight,
};
use tantivy::schema::{Field, Value};
use tantivy::{BitSet, DocId, Score, SegmentReader, TERMINATED, TantivyDocument, TantivyError};

use crate::Evidence;

/// Older search units only store their object ID in the evidence payload.
#[derive(Clone, Debug)]
pub(crate) struct LegacyDeleteQuery {
    pub candidates: BooleanQuery,
    pub evidence: Field,
    pub objects: Arc<HashSet<String>>,
}

impl Query for LegacyDeleteQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(LegacyDeleteWeight {
            candidates: self.candidates.weight(scoring)?,
            evidence: self.evidence,
            objects: Arc::clone(&self.objects),
        }))
    }
}

struct LegacyDeleteWeight {
    candidates: Box<dyn Weight>,
    evidence: Field,
    objects: Arc<HashSet<String>>,
}

impl Weight for LegacyDeleteWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let mut candidates = self.candidates.scorer(reader, 1.0)?;
        if candidates.doc() == TERMINATED {
            return Ok(Box::new(EmptyScorer));
        }
        let store = reader.get_store_reader(1)?;
        let mut matched = BitSet::with_max_value(reader.max_doc());
        while candidates.doc() != TERMINATED {
            let id = candidates.doc();
            if !reader.is_deleted(id) {
                let document: TantivyDocument = store.get(id)?;
                let evidence: Evidence = document
                    .get_first(self.evidence)
                    .and_then(|value| value.as_str())
                    .and_then(|value| serde_json::from_str(value).ok())
                    .ok_or_else(|| {
                        TantivyError::InvalidArgument("Invalid stored evidence".into())
                    })?;
                if self.objects.contains(&evidence.source.object_id) {
                    matched.insert(id);
                }
            }
            candidates.advance();
        }
        Ok(Box::new(ConstScorer::new(
            BitSetDocSet::from(matched),
            boost,
        )))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.doc() <= doc && scorer.seek(doc) == doc {
            Ok(Explanation::new("Matching legacy source object", 1.0))
        } else {
            Err(TantivyError::InvalidArgument(
                "Document does not match the legacy deletion".into(),
            ))
        }
    }
}
