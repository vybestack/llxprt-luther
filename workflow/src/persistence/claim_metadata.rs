use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};

use super::leases::LeaseStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimMetadataReceipt {
    pub lease_id: String,
    pub assignee: String,
    pub label: String,
    pub assignment_added: bool,
    pub label_added: bool,
    pub cleanup_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingClaimCleanup {
    pub issue_repo: String,
    pub issue_number: u64,
    pub lease_status: LeaseStatus,
    pub receipt: ClaimMetadataReceipt,
}

pub(crate) fn init_claim_metadata_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS claim_metadata (
            lease_id TEXT PRIMARY KEY NOT NULL,
            assignee TEXT NOT NULL,
            label TEXT NOT NULL,
            assignment_added INTEGER NOT NULL,
            label_added INTEGER NOT NULL,
            cleanup_pending INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL,
            FOREIGN KEY (lease_id) REFERENCES issue_leases(lease_id)
                ON UPDATE CASCADE ON DELETE CASCADE
        );",
    )
}

pub(crate) fn upsert_claim_metadata(
    conn: &Connection,
    receipt: &ClaimMetadataReceipt,
) -> SqliteResult<()> {
    conn.execute(
        "INSERT INTO claim_metadata (
            lease_id, assignee, label, assignment_added, label_added,
            cleanup_pending, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(lease_id) DO UPDATE SET
            assignee = excluded.assignee,
            label = excluded.label,
            assignment_added = excluded.assignment_added,
            label_added = excluded.label_added,
            cleanup_pending = excluded.cleanup_pending,
            updated_at = excluded.updated_at",
        params![
            receipt.lease_id,
            receipt.assignee,
            receipt.label,
            receipt.assignment_added,
            receipt.label_added,
            receipt.cleanup_pending,
        ],
    )?;
    Ok(())
}

pub(crate) fn get_claim_metadata(
    conn: &Connection,
    lease_id: &str,
) -> SqliteResult<Option<ClaimMetadataReceipt>> {
    conn.query_row(
        "SELECT lease_id, assignee, label, assignment_added, label_added, cleanup_pending
         FROM claim_metadata WHERE lease_id = ?1",
        [lease_id],
        |row| {
            Ok(ClaimMetadataReceipt {
                lease_id: row.get(0)?,
                assignee: row.get(1)?,
                label: row.get(2)?,
                assignment_added: row.get(3)?,
                label_added: row.get(4)?,
                cleanup_pending: row.get(5)?,
            })
        },
    )
    .optional()
}

pub(crate) fn list_pending_claim_cleanups(
    conn: &Connection,
    repo: &str,
) -> SqliteResult<Vec<PendingClaimCleanup>> {
    let mut statement = conn.prepare(
        "SELECT l.issue_repo, l.issue_number,
                m.lease_id, m.assignee, m.label, m.assignment_added,
                m.label_added, m.cleanup_pending, l.status
         FROM claim_metadata m
         JOIN issue_leases l ON l.lease_id = m.lease_id
         WHERE m.cleanup_pending = 1
           AND l.issue_repo = ?1
           AND l.status IN ('pending', 'claimed', 'completed', 'failed', 'abandoned', 'stale')
         ORDER BY l.issue_number",
    )?;
    let rows = statement.query_map([repo], |row| {
        let issue_number = row.get::<_, i64>(1)?;
        let issue_number = u64::try_from(issue_number).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?;
        let lease_status = row.get::<_, String>(8)?.parse().map_err(|error: String| {
            rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, error.into())
        })?;
        Ok(PendingClaimCleanup {
            issue_repo: row.get(0)?,
            issue_number,
            lease_status,
            receipt: ClaimMetadataReceipt {
                lease_id: row.get(2)?,
                assignee: row.get(3)?,
                label: row.get(4)?,
                assignment_added: row.get(5)?,
                label_added: row.get(6)?,
                cleanup_pending: row.get(7)?,
            },
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::leases::{init_leases_table, try_claim};

    #[test]
    fn receipt_roundtrips_and_updates_by_lease() {
        let conn = Connection::open_in_memory().unwrap();
        init_leases_table(&conn).unwrap();
        init_claim_metadata_table(&conn).unwrap();
        let lease = try_claim(&conn, "owner/repo", 42, "cfg").unwrap().unwrap();
        let mut receipt = ClaimMetadataReceipt {
            lease_id: lease.lease_id,
            assignee: "acoliver".to_owned(),
            label: "Luther working".to_owned(),
            assignment_added: true,
            label_added: false,
            cleanup_pending: true,
        };
        upsert_claim_metadata(&conn, &receipt).unwrap();
        assert_eq!(
            get_claim_metadata(&conn, &receipt.lease_id).unwrap(),
            Some(receipt.clone())
        );
        receipt.label_added = true;
        receipt.cleanup_pending = false;
        upsert_claim_metadata(&conn, &receipt).unwrap();
        assert_eq!(
            get_claim_metadata(&conn, &receipt.lease_id).unwrap(),
            Some(receipt)
        );
    }
}
