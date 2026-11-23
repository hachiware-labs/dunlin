//! Work records of Wiki and PARA projects (spec 14.1 and 28.12): each instruction is recorded in
//! `wiki_work` with its stage, progress, result or error, as `project-wiki-work.mjs` recorded it.

// The entry points are called from `node`, which test builds leave out.
#![cfg_attr(test, allow(dead_code))]

use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};

use crate::config_value::ConfigValue;
use crate::eval_stage::parse;
use crate::index_stage::{assign, object, sql, string_field, text};
use crate::source_stage::iso_now;
use crate::transform_stage::StageError;

/// Whether a process runs, as `process.kill(pid, 0)` tells: only a missing process is stopped.
#[cfg(windows)]
fn running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: the handle is checked and closed; the exit code is written to a local.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code = 0u32;
        let known = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        !known || code == STILL_ACTIVE
    }
}

#[cfg(unix)]
fn running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(any(windows, unix)))]
fn running(_pid: u32) -> bool {
    true
}

/// `visibleWork`: queued or running work of a process that stopped is shown as interrupted.
pub(crate) fn visible(work: ConfigValue) -> ConfigValue {
    let status = string_field(&work, "status").unwrap_or_default();
    let pid = match work.get("pid") {
        Some(ConfigValue::Number(pid)) => *pid,
        _ => return work,
    };
    if !["queued", "running"].contains(&status)
        || pid.fract() != 0.0
        || pid <= 0.0
        || pid > 9_007_199_254_740_991.0
        || pid == f64::from(std::process::id())
        || pid > f64::from(u32::MAX)
        || running(pid as u32)
    {
        return work;
    }
    let ConfigValue::Object(mut entries) = work else {
        return work;
    };
    assign(&mut entries, "status", Some(text("failed")));
    assign(&mut entries, "stage", Some(text("failed")));
    assign(
        &mut entries,
        "error",
        Some(object(vec![
            ("code", text("INTERRUPTED")),
            (
                "message",
                text("The process stopped before completing this work."),
            ),
        ])),
    );
    ConfigValue::Object(entries)
}

/// `workStatus(db, id)`
pub(crate) fn status(connection: &Connection, id: &str) -> Result<Option<ConfigValue>, StageError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name = 'wiki_work'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql)?
        .is_some();
    if !exists {
        return Ok(None);
    }
    let row: Option<String> = connection
        .query_row("SELECT data FROM wiki_work WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql)?;
    row.map(|data| parse(&data).map(visible)).transpose()
}

/// `latestWork(db)`: the last 12 works, newest first.
pub(crate) fn latest(connection: &Connection) -> Result<Vec<ConfigValue>, StageError> {
    let mut statement = connection
        .prepare("SELECT data FROM wiki_work ORDER BY rowid DESC LIMIT 12")
        .map_err(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql)?;
    rows.map(|row| row.map_err(sql).and_then(|data| parse(&data)).map(visible))
        .collect()
}

/// `queueWork(db, id, command, page, connection)`
pub(crate) fn queue(
    connection: &Connection,
    id: &str,
    command: &str,
    page: Option<&str>,
    client: Option<ConfigValue>,
) -> Result<ConfigValue, StageError> {
    let mut work = vec![
        ("id", text(id)),
        ("command", text(command)),
        (
            "page",
            page.filter(|page| !page.is_empty())
                .map_or(ConfigValue::Null, text),
        ),
        ("status", text("queued")),
        ("stage", text("queued")),
        ("queued_at", text(&iso_now())),
        ("pid", ConfigValue::Number(f64::from(std::process::id()))),
    ];
    if let Some(client) = client {
        work.push(("connection", client));
    }
    let work = object(work);
    connection
        .execute(
            "INSERT INTO wiki_work VALUES (?1, ?2)",
            [id, &work.to_js_json()],
        )
        .map_err(sql)?;
    Ok(work)
}

/// The progress of one work: the stage and counts it records, and the lines the CLI prints.
pub(crate) struct Tracker {
    connection: Option<Mutex<Connection>>,
    id: String,
    work: Mutex<Vec<(String, ConfigValue)>>,
    print: bool,
}

impl Tracker {
    /// Progress that is not recorded, for commands that only read.
    pub(crate) fn none() -> Self {
        Self {
            connection: None,
            id: String::new(),
            work: Mutex::new(Vec::new()),
            print: false,
        }
    }

    /// `progress(stage, { completed, total })`
    pub(crate) fn progress(&self, stage: &str, counts: Option<(usize, usize)>) {
        let Some(connection) = &self.connection else {
            return;
        };
        let Ok(mut work) = self.work.lock() else {
            return;
        };
        assign(&mut work, "stage", Some(text(stage)));
        assign(
            &mut work,
            "progress",
            Some(match counts {
                Some((completed, total)) => object(vec![
                    ("completed", ConfigValue::Number(completed as f64)),
                    ("total", ConfigValue::Number(total as f64)),
                ]),
                None => ConfigValue::Object(Vec::new()),
            }),
        );
        if let Ok(connection) = connection.lock() {
            let _ = connection.execute(
                "UPDATE wiki_work SET data = ?1 WHERE id = ?2",
                [&ConfigValue::Object(work.clone()).to_js_json(), &self.id],
            );
        }
        if self.print {
            let counts = counts.map_or_else(String::new, |(completed, total)| {
                format!(" {completed}/{total}")
            });
            eprintln!("{} {stage}{counts}", self.id);
        }
    }
}

/// `trackWork(db, id, command, page, run)`: records the work around `run`, which receives the
/// tracker. The error code of a failed work is the error's code, or `EXECUTION_FAILED`.
pub(crate) fn track<T>(
    state: &str,
    id: &str,
    command: &str,
    page: Option<&str>,
    print: bool,
    run: impl FnOnce(&Tracker) -> Result<T, StageError>,
    result: impl Fn(&T) -> ConfigValue,
) -> Result<T, StageError> {
    let connection = crate::eval_stage::open_state(state, false)?;
    let work = match status(&connection, id)? {
        Some(work) => work,
        None => queue(&connection, id, command, page, None)?,
    };
    let ConfigValue::Object(mut entries) = work else {
        return Err(StageError::plain("Invalid work record"));
    };
    assign(&mut entries, "status", Some(text("running")));
    assign(&mut entries, "started_at", Some(text(&iso_now())));
    let tracker = Tracker {
        connection: Some(Mutex::new(connection)),
        id: id.to_owned(),
        work: Mutex::new(entries),
        print,
    };
    tracker.progress("starting", None);
    let outcome = run(&tracker);
    let (final_status, final_entry) = match &outcome {
        Ok(value) => ("succeeded", ("result", result(value))),
        Err(error) => (
            "failed",
            (
                "error",
                object(vec![
                    ("code", text(error.code.unwrap_or("EXECUTION_FAILED"))),
                    ("message", text(&error.message)),
                ]),
            ),
        ),
    };
    if let Ok(mut work) = tracker.work.lock() {
        assign(&mut work, "status", Some(text(final_status)));
        assign(&mut work, final_entry.0, Some(final_entry.1));
        assign(&mut work, "stage", Some(text(final_status)));
        assign(&mut work, "finished_at", Some(text(&iso_now())));
        if let Some(connection) = &tracker.connection
            && let Ok(connection) = connection.lock()
        {
            let saved = connection.execute(
                "UPDATE wiki_work SET data = ?1 WHERE id = ?2",
                [&ConfigValue::Object(work.clone()).to_js_json(), id],
            );
            if let Err(error) = saved
                && outcome.is_ok()
            {
                return Err(sql(error));
            }
        }
    }
    if print {
        eprintln!("{id} {final_status}");
    }
    outcome
}
