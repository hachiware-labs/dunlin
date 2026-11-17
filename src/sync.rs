use std::collections::HashSet;

use serde::Serialize;

use crate::{
    Cancellation, Error, IndexChanges, KnowledgeBase, Result, WikiStatus,
    source::{self, SourceInput},
};

#[derive(Debug, Serialize)]
pub struct DeltaSyncOutcome {
    #[serde(flatten)]
    pub changes: IndexChanges,
    pub wiki: WikiSyncOutcome,
}

#[derive(Debug, Serialize)]
pub struct WikiSyncOutcome {
    pub status: &'static str,
    pub regenerated: Vec<RegeneratedPage>,
    pub failures: Vec<WikiSyncFailure>,
}

#[derive(Debug, Serialize)]
pub struct RegeneratedPage {
    pub id: String,
    pub revision: u32,
    pub status: WikiStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiSyncFailure {
    pub page_id: Option<String>,
    pub code: &'static str,
    pub message: String,
}

impl WikiSyncOutcome {
    fn failure(&mut self, page_id: Option<String>, error: Error) {
        self.status = "partial";
        self.failures.push(WikiSyncFailure {
            page_id,
            code: error.code(),
            message: error.to_string(),
        });
    }
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
    let mut wiki = WikiSyncOutcome {
        status: "complete",
        regenerated: Vec::new(),
        failures: Vec::new(),
    };
    // Recheck even for a no-op index sync so failed or interrupted Wiki work can be retried.
    match database
        .wiki
        .affected(&database.index.snapshot(), Some(source_id), cancel)
    {
        Ok(impacts) => {
            for impact in impacts {
                match database.wiki.regenerate_affected(
                    &database.index,
                    &impact.id,
                    source_id,
                    cancel,
                ) {
                    Ok(Some(page)) => wiki.regenerated.push(RegeneratedPage {
                        id: page.id,
                        revision: page.revision,
                        status: page.status,
                    }),
                    Ok(None) => {}
                    Err(error) => {
                        let interrupted = matches!(error, Error::Aborted | Error::Timeout);
                        wiki.failure(Some(impact.id), error);
                        if interrupted {
                            break;
                        }
                    }
                }
            }
        }
        Err(error) => wiki.failure(None, error),
    }
    Ok(DeltaSyncOutcome { changes, wiki })
}
