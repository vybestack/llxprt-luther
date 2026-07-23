//! Row mapping, parameter binding, and list-query helpers for the `runs` table.
//!
//! Extracted from the main sqlite module to keep it under the source-size
//! budget. Centralises the SQLite ↔ [`RunMetadata`] (de)serialization so the
//! column order, typed parse-error sources, and chunked list-by-id queries
//! remain in lock step.
use rusqlite::{params, Connection, Result as SqliteResult};

use super::super::run_metadata::{
    deserialize_pid_list, deserialize_string_list, serialize_pid_list, serialize_string_list,
    RunMetadata, RunStatus,
};
use super::schema::RUN_SELECT_COLUMNS;

/// Bind a [`RunMetadata`] row into the ordered parameter vector used by both
/// the upsert ([`super::persist_run_with_conn`]) and the atomic launch insert
/// ([`super::insert_initial_run_with_conn`]).
///
/// Centralising the binding keeps the column order and serialization in lock
/// step across both write paths.
pub(super) fn bind_run_metadata_params(
    metadata: &RunMetadata,
) -> SqliteResult<Vec<Box<dyn rusqlite::ToSql>>> {
    Ok(vec![
        Box::new(metadata.run_id.clone()),
        Box::new(metadata.workflow_type_id.clone()),
        Box::new(metadata.config_id.clone()),
        Box::new(metadata.status.to_string()),
        Box::new(metadata.created_at.to_rfc3339()),
        Box::new(metadata.updated_at.map(|t| t.to_rfc3339())),
        Box::new(metadata.current_step.clone()),
        Box::new(metadata.previous_step.clone()),
        Box::new(metadata.previous_outcome.clone()),
        Box::new(serialize_string_list(&metadata.next_step_candidates)),
        Box::new(metadata.log_path.clone()),
        Box::new(metadata.artifact_root.clone()),
        Box::new(metadata.workspace_path.clone()),
        Box::new(metadata.repository.clone()),
        Box::new(metadata.issue_number),
        Box::new(metadata.pr_number),
        Box::new(metadata.head_sha.clone()),
        Box::new(metadata.process_pid),
        Box::new(serialize_pid_list(&metadata.child_pids)),
        Box::new(metadata.continuation_rearm_checkpoint_id.clone()),
        Box::new(
            metadata
                .failure_cleanup
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        ),
        Box::new(
            metadata
                .launch_provenance
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        ),
    ])
}

/// Get a run record by id using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn get_run_with_conn(conn: &Connection, run_id: &str) -> SqliteResult<Option<RunMetadata>> {
    let sql = format!("SELECT {} FROM runs WHERE run_id = ?1", RUN_SELECT_COLUMNS);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![run_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(map_run_row(row)?))
    } else {
        Ok(None)
    }
}

/// List all run records using a borrowed connection.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub fn list_runs_with_conn(conn: &Connection) -> SqliteResult<Vec<RunMetadata>> {
    let sql = format!(
        "SELECT {} FROM runs ORDER BY created_at DESC",
        RUN_SELECT_COLUMNS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_run_row)?;
    rows.collect()
}

const RUN_ID_QUERY_CHUNK_SIZE: usize = 500;

/// List selected run records using a borrowed connection.
/// @plan:issue-117
pub fn list_runs_by_ids_with_conn(
    conn: &Connection,
    run_ids: &[&str],
) -> SqliteResult<Vec<RunMetadata>> {
    let run_ids = unique_run_ids(run_ids);
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::with_capacity(run_ids.len());
    for chunk in run_ids.chunks(RUN_ID_QUERY_CHUNK_SIZE) {
        runs.extend(list_runs_by_id_chunk(conn, chunk)?);
    }
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    Ok(runs)
}

fn list_runs_by_id_chunk(conn: &Connection, run_ids: &[&str]) -> SqliteResult<Vec<RunMetadata>> {
    let placeholders = std::iter::repeat_n("?", run_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {} FROM runs WHERE run_id IN ({}) ORDER BY created_at DESC",
        RUN_SELECT_COLUMNS, placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(run_ids), map_run_row)?;
    rows.collect()
}

fn unique_run_ids<'a>(run_ids: &'a [&'a str]) -> Vec<&'a str> {
    let mut seen = std::collections::HashSet::new();
    run_ids
        .iter()
        .copied()
        .filter(|run_id| seen.insert(*run_id))
        .collect()
}

/// Read only the status needed to classify a rejected conditional update.
pub(super) fn get_run_status_with_conn(
    conn: &Connection,
    run_id: &str,
) -> SqliteResult<Option<RunStatus>> {
    let mut stmt = conn.prepare_cached("SELECT status FROM runs WHERE run_id = ?1")?;
    let mut rows = stmt.query(params![run_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let status = row.get::<_, String>(0)?.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Some(status))
}

fn parse_run_status(row: &rusqlite::Row<'_>) -> SqliteResult<RunStatus> {
    let value: String = row.get(3)?;
    value.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn parse_created_at(row: &rusqlite::Row<'_>) -> SqliteResult<chrono::DateTime<chrono::Utc>> {
    row.get::<_, String>(4)?.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid datetime",
            )),
        )
    })
}

fn parse_launch_provenance(
    row: &rusqlite::Row<'_>,
) -> SqliteResult<Option<crate::persistence::launch_provenance::LaunchProvenance>> {
    row.get::<_, Option<String>>(21)?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                21,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

/// Map a SQLite row into a `RunMetadata`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub(super) fn map_run_row(row: &rusqlite::Row<'_>) -> SqliteResult<RunMetadata> {
    let status = parse_run_status(row)?;
    let created_at = parse_created_at(row)?;

    Ok(RunMetadata {
        run_id: row.get(0)?,
        workflow_type_id: row.get(1)?,
        config_id: row.get(2)?,
        status,
        created_at,
        updated_at: row
            .get::<_, Option<String>>(5)?
            .and_then(|s| s.parse().ok()),
        current_step: row.get(6)?,
        previous_step: row.get(7)?,
        previous_outcome: row.get(8)?,
        next_step_candidates: deserialize_string_list(row.get::<_, Option<String>>(9)?),
        log_path: row.get(10)?,
        artifact_root: row.get(11)?,
        workspace_path: row.get(12)?,
        repository: row.get(13)?,
        issue_number: row.get(14)?,
        pr_number: row.get(15)?,
        head_sha: row.get(16)?,
        process_pid: row.get::<_, Option<i64>>(17)?.map(|p| p as u32),
        child_pids: deserialize_pid_list(row.get::<_, Option<String>>(18)?),
        continuation_rearm_checkpoint_id: row.get(19)?,
        failure_cleanup: row
            .get::<_, Option<String>>(20)?
            .map(|raw| serde_json::from_str(&raw))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    20,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        launch_provenance: parse_launch_provenance(row)?,
    })
}
