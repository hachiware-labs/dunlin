use std::collections::HashSet;

use serde::Serialize;

use crate::{
    Cancellation, IndexChanges, KnowledgeBase, Result,
    source::{self, SourceInput},
};

#[derive(Debug, Serialize)]
pub struct DeltaSyncOutcome {
    #[serde(flatten)]
    pub changes: IndexChanges,
}

pub(crate) fn sync(
    database: &KnowledgeBase,
    source_id: &str,
    input: SourceInput<'_>,
    keep: &HashSet<String>,
    cancel: &Cancellation,
) -> Result<DeltaSyncOutcome> {
    source::validate_id(source_id)?;
    let changes = database.index.sync_source(source_id, input, keep, cancel)?;
    Ok(DeltaSyncOutcome { changes })
}
