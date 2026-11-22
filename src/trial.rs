//! Trial runs (spec 11 and 28.11 step 4c): the `trial_runs` record of `build --tag`/`--config`
//! and `eval --build`/`--config`, their limits on runs, elapsed time, model requests and input
//! bytes, and the counting of each model request before it is sent.
//!
//! Limits, messages, the error data and the recorded rows are the ones the JavaScript
//! implementation produced.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior};
use serde::Deserialize;

use crate::Cancellation;
use crate::config_value::ConfigValue;
use crate::index_stage::{object, sql, text};
use crate::transform_stage::StageError;

/// The trial a run belongs to.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrialRequest {
    /// `state.sqlite`.
    pub state: String,
    /// The run ID, which is the row ID.
    pub id: String,
    /// The command: `build`, `index` or `eval`.
    pub operation: String,
    /// `{ max_runs?, max_elapsed_ms?, max_api_requests?, max_api_input_bytes? }`.
    #[serde(default)]
    pub limits: ConfigValue,
}

const CREATE: &str = "CREATE TABLE IF NOT EXISTS trial_runs (
    id TEXT PRIMARY KEY, operation TEXT NOT NULL, started INTEGER NOT NULL,
    elapsed_ms INTEGER, api_requests INTEGER NOT NULL DEFAULT 0, api_input_bytes INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running'
  )";

#[derive(Clone, Copy, Debug, Default)]
struct Usage {
    runs: i64,
    elapsed_ms: i64,
    api_requests: i64,
    api_input_bytes: i64,
}

impl Usage {
    fn read(connection: &Connection) -> Result<Self, StageError> {
        connection
            .query_row(
                "SELECT count(*), coalesce(sum(elapsed_ms), 0), coalesce(sum(api_requests), 0), coalesce(sum(api_input_bytes), 0) FROM trial_runs",
                [],
                |row| {
                    Ok(Self {
                        runs: row.get(0)?,
                        elapsed_ms: row.get(1)?,
                        api_requests: row.get(2)?,
                        api_input_bytes: row.get(3)?,
                    })
                },
            )
            .map_err(sql)
    }

    fn json(self) -> ConfigValue {
        object(vec![
            ("runs", ConfigValue::Number(self.runs as f64)),
            ("elapsed_ms", ConfigValue::Number(self.elapsed_ms as f64)),
            (
                "api_requests",
                ConfigValue::Number(self.api_requests as f64),
            ),
            (
                "api_input_bytes",
                ConfigValue::Number(self.api_input_bytes as f64),
            ),
        ])
    }
}

fn open(path: &str, read_only: bool) -> Result<Connection, StageError> {
    let flags = if read_only {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let connection = Connection::open_with_flags(path, flags).map_err(sql)?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(sql)?;
    Ok(connection)
}

/// The usage of all trial runs of a project, for `status`.
pub fn usage(state: &str) -> Result<ConfigValue, StageError> {
    let connection = open(state, true)?;
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = 'trial_runs'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return Ok(Usage::default().json());
    }
    Ok(Usage::read(&connection)?.json())
}

/// A limit, or infinity when it is not set.
fn limit(limits: &ConfigValue, name: &str) -> f64 {
    match limits.get(name) {
        Some(ConfigValue::Number(value)) => *value,
        _ => f64::INFINITY,
    }
}

fn exhausted(limits: &ConfigValue, field: &str, used: i64) -> StageError {
    let mut error = StageError::coded(
        "TRIAL_LIMIT_REACHED",
        format!("Trial limit reached: {field}."),
        4,
    );
    error.data = Some(object(vec![
        ("limit", text(field)),
        ("maximum", limits.get(field).cloned().unwrap_or_default()),
        ("used", ConfigValue::Number(used as f64)),
    ]));
    error
}

/// A running trial: its row, the usage before it, and the limit it reached, if any.
pub struct Trial {
    connection: Mutex<Connection>,
    id: String,
    limits: ConfigValue,
    before: Usage,
    totals: Mutex<Usage>,
    started: Instant,
    /// The elapsed time left for this run, in milliseconds.
    available: f64,
    limited: Mutex<Option<StageError>>,
    cancel: Cancellation,
    finished: (Mutex<bool>, Condvar),
}

impl Trial {
    /// Starts a trial run within its limits: marks rows of interrupted runs, checks the run and
    /// elapsed time limits, and records the new run. A watchdog updates the elapsed time every
    /// second and aborts `cancel` when the time limit is reached.
    pub fn start(request: &TrialRequest, cancel: &Cancellation) -> Result<Arc<Self>, StageError> {
        let mut connection = open(&request.state, false)?;
        connection.execute(CREATE, []).map_err(sql)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        // The caller holds the execution lock. A prior unfinished row can only be from an
        // interrupted run.
        transaction
            .execute(
                "UPDATE trial_runs SET status = 'interrupted', elapsed_ms = coalesce(elapsed_ms, 0) WHERE status = 'running'",
                [],
            )
            .map_err(sql)?;
        let before = Usage::read(&transaction)?;
        for (key, used) in [
            ("max_runs", before.runs),
            ("max_elapsed_ms", before.elapsed_ms),
        ] {
            if used as f64 >= limit(&request.limits, key) {
                return Err(exhausted(&request.limits, key, used));
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        transaction
            .execute(
                "INSERT INTO trial_runs (id, operation, started) VALUES (?1, ?2, ?3)",
                (&request.id, &request.operation, now),
            )
            .map_err(sql)?;
        transaction.commit().map_err(sql)?;
        let trial = Arc::new(Self {
            connection: Mutex::new(connection),
            id: request.id.clone(),
            limits: request.limits.clone(),
            before,
            totals: Mutex::new(before),
            started: Instant::now(),
            available: limit(&request.limits, "max_elapsed_ms") - before.elapsed_ms as f64,
            limited: Mutex::new(None),
            cancel: cancel.clone(),
            finished: (Mutex::new(false), Condvar::new()),
        });
        let watched = trial.clone();
        std::thread::spawn(move || watched.watch());
        Ok(trial)
    }

    fn elapsed_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    /// Records that the time limit was reached and stops the run.
    fn time_up(&self, elapsed: f64) {
        let mut limited = self
            .limited
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if limited.is_none() {
            *limited = Some(exhausted(
                &self.limits,
                "max_elapsed_ms",
                self.before.elapsed_ms + elapsed as i64,
            ));
        }
        drop(limited);
        self.cancel.abort();
    }

    fn watch(&self) {
        let (lock, condvar) = &self.finished;
        let mut finished = lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut next_update = 1000.0;
        loop {
            if *finished {
                return;
            }
            let elapsed = self.elapsed_ms();
            if elapsed >= self.available {
                self.time_up(elapsed.ceil());
            }
            if elapsed >= next_update {
                let saved = self
                    .connection
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .execute(
                        "UPDATE trial_runs SET elapsed_ms = ?1 WHERE id = ?2",
                        (elapsed.ceil() as i64, &self.id),
                    );
                if let Err(error) = saved {
                    let mut limited = self
                        .limited
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    limited.get_or_insert_with(|| sql(error));
                    drop(limited);
                    self.cancel.abort();
                }
                next_update += 1000.0;
            }
            let until_limit = (self.available - elapsed).max(0.0);
            let wait = (next_update - elapsed).min(until_limit).clamp(1.0, 1000.0);
            finished = condvar
                .wait_timeout(finished, Duration::from_micros((wait * 1000.0) as u64))
                .map(|(guard, _)| guard)
                .unwrap_or_else(|error| error.into_inner().0);
        }
    }

    /// Counts a model request of `bytes` before it is sent, or refuses it at a limit.
    pub fn charge(&self, bytes: usize) -> Result<(), StageError> {
        let elapsed = self.elapsed_ms().floor();
        if elapsed >= self.available {
            self.time_up(elapsed);
        }
        if let Some(error) = self
            .limited
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            return Err(clone_error(error));
        }
        let mut totals = self
            .totals
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (key, used, needed) in [
            ("max_api_requests", totals.api_requests, 1),
            ("max_api_input_bytes", totals.api_input_bytes, bytes as i64),
        ] {
            if (used + needed) as f64 > limit(&self.limits, key) {
                let error = exhausted(&self.limits, key, used);
                self.limited
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get_or_insert_with(|| clone_error(&error));
                return Err(error);
            }
        }
        self.connection
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .execute(
                "UPDATE trial_runs SET api_requests = api_requests + 1, api_input_bytes = api_input_bytes + ?1 WHERE id = ?2",
                (bytes as i64, &self.id),
            )
            .map_err(sql)?;
        totals.api_requests += 1;
        totals.api_input_bytes += bytes as i64;
        Ok(())
    }

    /// Ends the run: records its elapsed time and status, and gives the error to report. A run
    /// stopped at a limit reports the limit.
    pub fn finish(&self, outcome: Result<(), &StageError>) -> Option<StageError> {
        {
            let (lock, condvar) = &self.finished;
            *lock.lock().unwrap_or_else(|error| error.into_inner()) = true;
            condvar.notify_all();
        }
        let limited = self
            .limited
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(clone_error);
        let (status, error) = match (outcome, limited) {
            (Ok(()), _) => ("succeeded", None),
            (Err(_), Some(limit)) => ("limited", Some(limit)),
            (Err(error), None) => (
                if error.code == Some("TRIAL_LIMIT_REACHED") {
                    "limited"
                } else {
                    "failed"
                },
                None,
            ),
        };
        let saved = self
            .connection
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .execute(
                "UPDATE trial_runs SET elapsed_ms = ?1, status = ?2 WHERE id = ?3",
                (self.elapsed_ms().ceil() as i64, status, &self.id),
            );
        error.or_else(|| saved.err().map(sql))
    }
}

fn clone_error(error: &StageError) -> StageError {
    StageError {
        code: error.code,
        message: error.message.clone(),
        exit_code: error.exit_code,
        aborted: false,
        data: error.data.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (std::path::PathBuf, String) {
        let directory = std::env::temp_dir().join(format!(
            "knowledge-nest-trial-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("state.sqlite");
        Connection::open(&path).unwrap();
        let text = path.to_string_lossy().into_owned();
        (directory, text)
    }

    fn request(state: &str, id: &str, limits: ConfigValue) -> TrialRequest {
        TrialRequest {
            state: state.into(),
            id: id.into(),
            operation: "build".into(),
            limits,
        }
    }

    #[test]
    fn refuses_runs_and_requests_beyond_the_limits() {
        let (directory, state) = state();
        let limits = object(vec![
            ("max_runs", ConfigValue::Number(1.0)),
            ("max_api_requests", ConfigValue::Number(1.0)),
        ]);
        let cancel = Cancellation::default();
        let trial = Trial::start(&request(&state, "run-1", limits.clone()), &cancel).unwrap();
        trial.charge(10).unwrap();
        let refused = trial.charge(10).unwrap_err();
        assert_eq!(refused.code, Some("TRIAL_LIMIT_REACHED"));
        assert_eq!(
            refused.data.unwrap().to_js_json(),
            r#"{"limit":"max_api_requests","maximum":1,"used":1}"#
        );
        let reported = trial.finish(Err(&StageError::plain("failed")));
        assert_eq!(reported.unwrap().code, Some("TRIAL_LIMIT_REACHED"));
        let again = Trial::start(&request(&state, "run-2", limits), &cancel)
            .err()
            .unwrap();
        assert_eq!(
            again.data.unwrap().to_js_json(),
            r#"{"limit":"max_runs","maximum":1,"used":1}"#
        );
        let used = usage(&state).unwrap().to_js_json();
        assert!(used.starts_with(r#"{"runs":1,"elapsed_ms":"#), "{used}");
        assert!(
            used.ends_with(r#""api_requests":1,"api_input_bytes":10}"#),
            "{used}"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn stops_a_run_at_the_time_limit() {
        let (directory, state) = state();
        let cancel = Cancellation::default();
        let trial = Trial::start(
            &request(
                &state,
                "run-1",
                object(vec![("max_elapsed_ms", ConfigValue::Number(30.0))]),
            ),
            &cancel,
        )
        .unwrap();
        let started = Instant::now();
        while cancel.check().is_ok() && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(cancel.check().is_err());
        let reported = trial.finish(Err(&StageError::aborted())).unwrap();
        assert_eq!(reported.code, Some("TRIAL_LIMIT_REACHED"));
        let status: String = Connection::open(&state)
            .unwrap()
            .query_row("SELECT status FROM trial_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "limited");
        let _ = std::fs::remove_dir_all(directory);
    }
}
