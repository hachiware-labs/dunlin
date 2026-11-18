//! `use` (spec 12 and 28.11 step 4c): adopts a build in one transaction with the configuration
//! file it was built with. The adoption history, the serving build and its generation are written
//! here; JavaScript writes the configuration file, since YAML stays there until stage 6.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use rusqlite::{Connection, TransactionBehavior};
use serde::Deserialize;

use crate::config_value::ConfigValue;
use crate::eval_report::{StatusRequest, evaluation_status};
use crate::index_stage::{object, sql, text};
use crate::transform_stage::StageError;

/// A build to adopt and the configuration it brings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptRequest {
    pub state: String,
    pub build_id: String,
    /// The search configuration ID of the adopted configuration.
    pub search_id: String,
    /// The configuration before and after, without secrets, as JSON text.
    pub previous: String,
    pub adopted: String,
    /// `eval-data.use` and the `eval` settings of the adopted configuration.
    #[serde(default)]
    pub dataset: Option<String>,
    #[serde(default)]
    pub settings: ConfigValue,
}

/// What adoption asks of JavaScript.
pub trait AdoptHost {
    /// Writes the adopted configuration file, unless it changed since the command started.
    fn write_config(&self) -> Result<(), String>;
    /// Puts the previous configuration file back after a failure.
    fn restore_config(&self) -> Result<(), String>;
}

/// Adopts the build: `{ build_id, search_config_id, evaluation_status, restart_mcp }`.
pub fn adopt(request: &AdoptRequest, host: &dyn AdoptHost) -> Result<ConfigValue, StageError> {
    let mut connection = Connection::open(&request.state).map_err(sql)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(sql)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    host.write_config().map_err(StageError::plain)?;
    let recorded = (|| -> Result<ConfigValue, StageError> {
        transaction
            .execute(
                "INSERT INTO adoption_history (previous, adopted) VALUES (?1, ?2)",
                (&request.previous, &request.adopted),
            )
            .map_err(sql)?;
        let generation = transaction.last_insert_rowid();
        transaction
            .execute(
                "UPDATE project SET build_id = ?1 WHERE id = 1",
                [&request.build_id],
            )
            .map_err(sql)?;
        transaction
            .execute(
                "INSERT INTO serving_generations VALUES (?1, ?2)",
                (generation, &request.build_id),
            )
            .map_err(sql)?;
        let status = evaluation_status(&StatusRequest {
            state: request.state.clone(),
            build_id: request.build_id.clone(),
            search_id: request.search_id.clone(),
            dataset: request.dataset.clone(),
            settings: request.settings.clone(),
        })?;
        Ok(status.get("state").cloned().unwrap_or_default())
    })()
    .and_then(|state| transaction.commit().map_err(sql).map(|()| state));
    match recorded {
        Ok(state) => Ok(object(vec![
            ("build_id", text(&request.build_id)),
            ("search_config_id", text(&request.search_id)),
            ("evaluation_status", state),
            ("restart_mcp", ConfigValue::Bool(true)),
        ])),
        Err(error) => {
            // The transaction rolled back when it was dropped.
            host.restore_config().map_err(StageError::plain)?;
            Err(error)
        }
    }
}
