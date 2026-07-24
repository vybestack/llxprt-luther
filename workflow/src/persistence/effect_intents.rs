//! Durable effect-intent state machine table.
//!
//! A complete effect-intent state machine for recoverable external effects
//! (commit, push, open PR, merge). Each effect has a stable unique
//! [`effect_key`](compute_effect_key) bound to its operation, attempt, and
//! sequence; a canonical payload with digest and version; expected
//! target/predecessor; and an observed result. The intent transitions through
//! `Prepared → Completed|Conflict`. [C7]
//!
//! [`prepare_effect`] uses an insert-or-load exact-binding comparison: if a row
//! with the same `effect_key` already exists, the exact binding (canonical
//! payload, digest, target, predecessor) is compared. On mismatch the intent
//! transitions to `Conflict`. On match the existing intent is returned
//! (idempotent). [B5]
//!
//! @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
//! @requirement:REQ-RP-008

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Result as SqliteResult};
use sha2::{Digest, Sha256};

/// Table name for the durable effect-intent state machine. [C7/B5]
pub const EFFECT_INTENTS_TABLE: &str = "effect_intents";

/// Fixed canonicalization version for effect payloads. [C7]
pub const PAYLOAD_VERSION: u32 = 1;

/// The kind of external effect an intent records. [C7]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectKind {
    /// A `git commit` effect.
    Commit,
    /// A `git push` effect.
    Push,
    /// A `gh pr create` effect.
    OpenPr,
    /// A `gh pr merge` effect.
    Merge,
}

impl EffectKind {
    /// The string stored in the `effect_kind` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Push => "push",
            Self::OpenPr => "open_pr",
            Self::Merge => "merge",
        }
    }

    /// Parse a persisted kind string, returning `None` for unknown values.
    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "commit" => Some(Self::Commit),
            "push" => Some(Self::Push),
            "open_pr" => Some(Self::OpenPr),
            "merge" => Some(Self::Merge),
            _ => None,
        }
    }
}

/// External state observed by an effect-specific reconciler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedState {
    /// Observed local HEAD SHA (used by commit reconciliation).
    pub head_sha: Option<String>,
    /// Observed remote ref SHA (used by push reconciliation).
    pub remote_ref_sha: Option<String>,
    /// Observed matching PR number for the head (used by open-PR reconciliation).
    pub matching_pr_number: Option<u64>,
}

/// The verdict of reconciling an effect against observed external state. [C7]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileVerdict {
    /// The effect is already completed (or was observed as completed). Carries
    /// the prior observed result, if any.
    Completed {
        /// The observed result recorded for the effect, if any.
        result: Option<String>,
    },
    /// The effect has not yet taken effect; the caller must re-issue it.
    NeedsReissue,
    /// The observed state conflicts with the expected binding.
    Conflict {
        /// A human-readable conflict detail.
        detail: String,
    },
}

/// A row in the durable effect-intents table.
///
/// Binds a stable unique key to its operation, attempt, and sequence, with a
/// canonical payload (digest + version), expected target/predecessor, observed
/// result, and a `Prepared|Completed|Conflict` status. [C7/B5]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectIntent {
    /// Stable unique key (PRIMARY KEY). [C7]
    pub effect_key: String,
    /// Bound to the recovery operation. [C7]
    pub operation_id: String,
    /// Bound to the attempt. [C7/B4]
    pub attempt_id: i64,
    /// Ordinal within the attempt. [C7]
    pub sequence: i64,
    /// The effect kind. [C7]
    pub effect_kind: EffectKind,
    /// Canonical serialization of the effect payload. [C7]
    pub canonical_payload: Vec<u8>,
    /// SHA-256 of `canonical_payload`. [C7]
    pub payload_digest: String,
    /// Canonicalization version. [C7]
    pub payload_version: u32,
    /// Expected target (e.g. branch name, PR number). [C7]
    pub expected_target: Option<String>,
    /// Expected predecessor (e.g. expected parent commit SHA). [C7]
    pub expected_predecessor: Option<String>,
    /// Observed result after the effect. [C7]
    pub observed_result: Option<String>,
    /// `'prepared'|'completed'|'conflict'`. [C7/B5]
    pub status: String,
}

/// Initialize the durable effect-intents table (idempotent). [C7/B5]
///
/// DDL (intents pseudocode lines 02–17):
/// ```text
/// CREATE TABLE IF NOT EXISTS effect_intents (
///   effect_key TEXT PRIMARY KEY,             -- stable unique key [C7]
///   operation_id TEXT NOT NULL,              -- bound to recovery operation [C7]
///   attempt_id INTEGER NOT NULL,             -- bound to attempt [C7/B4]
///   sequence INTEGER NOT NULL,               -- ordinal within the attempt [C7]
///   effect_kind TEXT NOT NULL,               -- 'commit'|'push'|'open_pr'|'merge'
///   canonical_payload BLOB NOT NULL,         -- canonical serialization
///   payload_digest TEXT NOT NULL,            -- SHA-256 of canonical_payload
///   payload_version INTEGER NOT NULL,        -- canonicalization version [C7]
///   expected_target TEXT,                    -- e.g. branch name, PR number [C7]
///   expected_predecessor TEXT,               -- e.g. expected parent commit SHA [C7]
///   observed_result TEXT,                    -- observed result after effect [C7]
///   status TEXT NOT NULL DEFAULT 'prepared', -- 'prepared'|'completed'|'conflict' [C7/B5]
///   created_at TEXT NOT NULL,
///   finalized_at TEXT
/// )
/// ```
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
pub fn init_effect_intents_table(conn: &Connection) -> SqliteResult<()> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {EFFECT_INTENTS_TABLE} (
                effect_key TEXT PRIMARY KEY,
                operation_id TEXT NOT NULL,
                attempt_id INTEGER NOT NULL,
                sequence INTEGER NOT NULL,
                effect_kind TEXT NOT NULL,
                canonical_payload BLOB NOT NULL,
                payload_digest TEXT NOT NULL,
                payload_version INTEGER NOT NULL,
                expected_target TEXT,
                expected_predecessor TEXT,
                observed_result TEXT,
                status TEXT NOT NULL DEFAULT 'prepared',
                created_at TEXT NOT NULL,
                finalized_at TEXT
            )"
        ),
        [],
    )?;
    Ok(())
}

/// Compute the lowercase-hex SHA-256 digest of a byte slice.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Append a length-prefixed (u64 big-endian) byte slice to the canonical buffer.
fn push_len_prefixed(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Compute the stable unique effect key. [C7]
///
/// Binds the operation id, attempt id, sequence, and effect kind so that two
/// different effects under the same operation+attempt+sequence but different
/// kinds cannot alias (intents pseudocode lines 20–27).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
#[must_use]
pub fn compute_effect_key(
    operation_id: &str,
    attempt_id: i64,
    sequence: i64,
    effect_kind: &str,
) -> String {
    let mut buf = Vec::new();
    push_len_prefixed(&mut buf, operation_id.as_bytes());
    push_len_prefixed(&mut buf, &attempt_id.to_be_bytes());
    push_len_prefixed(&mut buf, &sequence.to_be_bytes());
    push_len_prefixed(&mut buf, effect_kind.as_bytes());
    sha256_hex(&buf)
}

/// Canonical values required to prepare a durable effect intent.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
#[derive(Debug, Clone)]
pub struct EffectPreparation<'a> {
    /// Bound to the recovery operation. [C7]
    pub operation_id: &'a str,
    /// Bound to the attempt. [C7/B4]
    pub attempt_id: i64,
    /// Ordinal within the attempt. [C7]
    pub sequence: i64,
    /// The effect kind. [C7]
    pub kind: EffectKind,
    /// Raw payload bytes; canonicalized internally. [C7]
    pub payload: &'a [u8],
    /// Expected target (e.g. branch name, PR number). [C7]
    pub expected_target: Option<&'a str>,
    /// Expected predecessor (e.g. expected parent commit SHA). [C7]
    pub expected_predecessor: Option<&'a str>,
}

/// Prepare an effect intent BEFORE the external effect is issued. [C7/B5]
///
/// Uses an insert-or-load exact-binding comparison: if a row with the same
/// `effect_key` already exists, load it and compare the exact binding
/// (canonical payload, digest, target, predecessor). On mismatch, transition
/// to `Conflict` and error. On match, return the existing intent (idempotent).
/// [B5] (intents pseudocode lines 34–71).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
pub fn prepare_effect(
    conn: &Connection,
    preparation: &EffectPreparation<'_>,
) -> SqliteResult<EffectIntent> {
    let canonical = canonicalize_payload(&preparation.kind, preparation.payload);
    let digest = sha256_hex(&canonical);
    let key = compute_effect_key(
        preparation.operation_id,
        preparation.attempt_id,
        preparation.sequence,
        preparation.kind.as_str(),
    );

    match load_effect_optional(conn, &key)? {
        None => {
            insert_prepared_intent(conn, &key, preparation, &canonical, &digest)?;
            // Re-load to return the persisted row.
            load_effect_optional(conn, &key)?.ok_or_else(|| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some(format!(
                        "prepared effect intent {key} not found after insert"
                    )),
                )
            })
        }
        Some(existing) => {
            // [B5] exact-binding comparison against the existing intent.
            let target_matches = existing.expected_target.as_deref() == preparation.expected_target;
            let predecessor_matches =
                existing.expected_predecessor.as_deref() == preparation.expected_predecessor;
            if existing.payload_digest != digest || !target_matches || !predecessor_matches {
                // Mismatch: transition to conflict in the caller's transaction.
                // The caller commits the durable conflict or rolls it back with
                // the rest of the reservation; this function never commits a
                // transaction it does not own. [C5/B5]
                finalize_effect(conn, &key, "conflict", None)?;
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some(format!("binding conflict for effect intent {key}")),
                ));
            }
            Ok(existing)
        }
    }
}

/// Insert a new `Prepared` effect intent. [C7]
fn insert_prepared_intent(
    conn: &Connection,
    key: &str,
    preparation: &EffectPreparation<'_>,
    canonical: &[u8],
    digest: &str,
) -> SqliteResult<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        &format!(
            "INSERT INTO {EFFECT_INTENTS_TABLE}
               (effect_key, operation_id, attempt_id, sequence, effect_kind,
                canonical_payload, payload_digest, payload_version,
                expected_target, expected_predecessor, observed_result,
                status, created_at, finalized_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, 'prepared', ?11, NULL)"
        ),
        params![
            key,
            preparation.operation_id,
            preparation.attempt_id,
            preparation.sequence,
            preparation.kind.as_str(),
            canonical,
            digest,
            i64::from(PAYLOAD_VERSION),
            preparation.expected_target,
            preparation.expected_predecessor,
            now,
        ],
    )?;
    Ok(())
}

/// Load an effect intent by key.
///
/// Intents pseudocode line 76.
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
pub fn load_effect(conn: &Connection, key: &str) -> SqliteResult<EffectIntent> {
    conn.query_row(
        &format!(
            "SELECT effect_key, operation_id, attempt_id, sequence, effect_kind,
                    canonical_payload, payload_digest, payload_version,
                    expected_target, expected_predecessor, observed_result, status
             FROM {EFFECT_INTENTS_TABLE}
             WHERE effect_key = ?1"
        ),
        params![key],
        map_effect_row,
    )
}

/// Load an effect intent by key, returning `None` when no row exists.
fn load_effect_optional(conn: &Connection, key: &str) -> SqliteResult<Option<EffectIntent>> {
    let intent = conn
        .query_row(
            &format!(
                "SELECT effect_key, operation_id, attempt_id, sequence, effect_kind,
                        canonical_payload, payload_digest, payload_version,
                        expected_target, expected_predecessor, observed_result, status
                 FROM {EFFECT_INTENTS_TABLE}
                 WHERE effect_key = ?1"
            ),
            params![key],
            map_effect_row,
        )
        .optional()?;
    Ok(intent)
}

/// Reconcile an effect against observed external state. [C7]
///
/// Dispatches per kind (commit/push/open_pr/merge), comparing observed state to
/// the expected target/predecessor (intents pseudocode lines 75–94). Each
/// effect-specific reconciler may finalize the intent (Completed or Conflict)
/// and returns the corresponding [`ReconcileVerdict`].
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
pub fn reconcile_effect(
    conn: &Connection,
    key: &str,
    observed: &ObservedState,
) -> SqliteResult<ReconcileVerdict> {
    let intent = load_effect(conn, key)?;
    match intent.status.as_str() {
        "completed" => Ok(ReconcileVerdict::Completed {
            result: intent.observed_result.clone(),
        }),
        "conflict" => Ok(ReconcileVerdict::Conflict {
            detail: "prior conflict".to_string(),
        }),
        "prepared" => match intent.effect_kind {
            EffectKind::Commit => reconcile_commit(conn, &intent, observed),
            EffectKind::Push => reconcile_push(conn, &intent, observed),
            EffectKind::OpenPr => reconcile_open_pr(conn, &intent, observed),
            EffectKind::Merge => reconcile_merge(conn, &intent, observed),
        },
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            11,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown effect intent status '{other}'"),
            )),
        )),
    }
}

/// Reconcile a `Commit` effect against observed HEAD. [C7]
///
/// If the observed HEAD matches `expected_target`, the effect is completed.
/// If it matches `expected_predecessor`, HEAD has not moved and the commit
/// must be reissued. Otherwise the observed HEAD is unexpected → conflict.
fn reconcile_commit(
    conn: &Connection,
    intent: &EffectIntent,
    observed: &ObservedState,
) -> SqliteResult<ReconcileVerdict> {
    let head_sha = observed.head_sha.as_deref();
    if head_sha == intent.expected_target.as_deref() {
        finalize_effect(conn, &intent.effect_key, "completed", head_sha)?;
        Ok(ReconcileVerdict::Completed {
            result: head_sha.map(str::to_string),
        })
    } else if head_sha == intent.expected_predecessor.as_deref() {
        Ok(ReconcileVerdict::NeedsReissue)
    } else {
        finalize_effect(conn, &intent.effect_key, "conflict", None)?;
        Ok(ReconcileVerdict::Conflict {
            detail: "unexpected HEAD".to_string(),
        })
    }
}

/// Reconcile a `Push` effect against the observed remote ref. [C7]
fn reconcile_push(
    conn: &Connection,
    intent: &EffectIntent,
    observed: &ObservedState,
) -> SqliteResult<ReconcileVerdict> {
    let remote_sha = observed.remote_ref_sha.as_deref();
    if remote_sha == intent.expected_target.as_deref() {
        finalize_effect(conn, &intent.effect_key, "completed", remote_sha)?;
        Ok(ReconcileVerdict::Completed {
            result: remote_sha.map(str::to_string),
        })
    } else if remote_sha == intent.expected_predecessor.as_deref() {
        Ok(ReconcileVerdict::NeedsReissue)
    } else {
        finalize_effect(conn, &intent.effect_key, "conflict", None)?;
        Ok(ReconcileVerdict::Conflict {
            detail: "remote ref diverged".to_string(),
        })
    }
}

/// Reconcile an `OpenPr` effect against observed matching PRs. [C7]
fn reconcile_open_pr(
    conn: &Connection,
    intent: &EffectIntent,
    observed: &ObservedState,
) -> SqliteResult<ReconcileVerdict> {
    if let Some(pr_number) = observed.matching_pr_number {
        let result = pr_number.to_string();
        finalize_effect(conn, &intent.effect_key, "completed", Some(&result))?;
        Ok(ReconcileVerdict::Completed {
            result: Some(result),
        })
    } else {
        Ok(ReconcileVerdict::NeedsReissue)
    }
}

/// Reconcile a `Merge` effect. [C7/B5]
///
/// P17 owns authoritative merge observation. When a typed merge artifact
/// already exists for this run (inserted by `complete_typed_merge` in the
/// atomic artifact+status transaction), the intent is finalized as Completed.
/// Otherwise it needs re-issue — the caller must construct a `MergeVerifier`
/// and call `complete_typed_merge`, which finalizes the merge intent inside
/// the same atomic transaction as the artifact+status. [B5]
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P17
/// @requirement:REQ-RP-010
fn reconcile_merge(
    conn: &Connection,
    intent: &EffectIntent,
    _observed: &ObservedState,
) -> SqliteResult<ReconcileVerdict> {
    // Resolve run_id from the operation_id (the effect is bound to an
    // operation which is bound to a run).
    let run_id: Option<String> = conn
        .query_row(
            "SELECT run_id FROM recovery_operations WHERE operation_id = ?1",
            rusqlite::params![intent.operation_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(run_id) = run_id else {
        return Ok(ReconcileVerdict::NeedsReissue);
    };
    // If a typed merge artifact already exists, the merge is completed.
    let artifact = crate::engine::recovery::typed_merge::load_merge_artifact_conn(conn, &run_id)?;
    if let Some(artifact) = artifact {
        finalize_effect(
            conn,
            &intent.effect_key,
            "completed",
            Some(&artifact.result_sha),
        )?;
        return Ok(ReconcileVerdict::Completed {
            result: Some(artifact.result_sha),
        });
    }
    Ok(ReconcileVerdict::NeedsReissue)
}

/// Guarded finalize: transition the effect to a terminal state. [C7]
///
/// Only transitions from `'prepared'` and checks affected rows (intents
/// pseudocode lines 142–151).
///
/// @plan:PLAN-20260723-SELFHOST-RELIABILITY.P05
/// @requirement:REQ-RP-008
pub fn finalize_effect(
    conn: &Connection,
    key: &str,
    status: &str,
    observed_result: Option<&str>,
) -> SqliteResult<()> {
    let now = Utc::now().to_rfc3339();
    let affected = conn.execute(
        &format!(
            "UPDATE {EFFECT_INTENTS_TABLE}
             SET status = ?2, observed_result = ?3, finalized_at = ?4
             WHERE effect_key = ?1 AND status = 'prepared'"
        ),
        params![key, status, observed_result, now],
    )?;
    if affected != 1 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "finalize_effect guard failed for {key} -> {status} (affected {affected})"
            )),
        ));
    }
    Ok(())
}

/// Canonicalize a payload for an effect kind. [C7]
///
/// For P05 the canonical form is the raw payload bytes as supplied; the digest
/// is computed over these canonical bytes. Future versions may apply
/// kind-specific canonicalization.
fn canonicalize_payload(_kind: &EffectKind, payload: &[u8]) -> Vec<u8> {
    payload.to_vec()
}

/// Map an `effect_intents` row into an [`EffectIntent`].
fn map_effect_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EffectIntent> {
    let kind_str: String = row.get(4)?;
    let effect_kind = EffectKind::parse_str(&kind_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown effect kind '{kind_str}'"),
            )),
        )
    })?;
    let payload_version_i64: i64 = row.get(7)?;
    let payload_version = u32::try_from(payload_version_i64).unwrap_or(0);
    Ok(EffectIntent {
        effect_key: row.get(0)?,
        operation_id: row.get(1)?,
        attempt_id: row.get(2)?,
        sequence: row.get(3)?,
        effect_kind,
        canonical_payload: row.get(5)?,
        payload_digest: row.get(6)?,
        payload_version,
        expected_target: row.get(8)?,
        expected_predecessor: row.get(9)?,
        observed_result: row.get(10)?,
        status: row.get(11)?,
    })
}

/// Count effect intents associated with recovery operations for one run.
pub fn count_effect_intents_for_run(conn: &Connection, run_id: &str) -> SqliteResult<i64> {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {EFFECT_INTENTS_TABLE} WHERE operation_id IN \
             (SELECT operation_id FROM recovery_operations WHERE run_id = ?1)"
        ),
        params![run_id],
        |row| row.get(0),
    )
}
