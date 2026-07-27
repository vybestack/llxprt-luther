//! Golden digests captured before persistence moves into core.
//!
//! Issue #205 calls this the highest-risk move in the boundary series, for a
//! specific reason: recovery resolves policy from the canonical `StepDef` and
//! capsules embed workflow bytes, so a digest that shifts by one byte strands
//! every run that was mid-flight. Nothing in the type system notices, and no
//! existing test fails — the damage only surfaces when a real run needs
//! recovery and its capsule no longer matches.
//!
//! These values are recorded from the tree *before* the move. They are
//! deliberately literal: computing an expected digest from the same code under
//! test would agree with itself no matter what the code did. A literal
//! disagrees, which is the entire point.
//!
//! If a value here must change, that is a serialization change requiring an
//! explicit versioned migration — not an edit to this file.

use luther_workflow::persistence::launch_provenance::{
    compute_config_digest, compute_workflow_digest,
};
use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
use std::path::{Path, PathBuf};

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("config")
}

/// Every production workflow type, with the digest recorded pre-move.
///
/// Listing the types explicitly rather than globbing the directory is
/// deliberate: a glob that matched nothing would pass, and silently stop
/// covering the thing it exists to protect.
const WORKFLOW_TYPE_DIGESTS: &[(&str, &str)] = &[
    (
        "issue-fix-v1",
        "dd6bcf9de21e1ed4220123b03f246a95fa763b23c07c592a6fdcdfa5ce669b36",
    ),
    (
        "llxprt-issue-fix-v1",
        "572e819a33a0c59e945413ff9dfc553d99882ebb0442ab44dfec7eccf5399245",
    ),
    (
        "llxprt-luther-dogfood-v1",
        "565057dee0bc503abac403172750608a26737d9ce380553727bf8ed1f084a8f1",
    ),
    (
        "parent-issue-orchestrator-v1",
        "7d6ca5ca07665ab381337cb57b1581c0410f1b75230637fd68a844b548f10558",
    ),
];

/// Every production config, with the digest recorded pre-move.
const CONFIG_DIGESTS: &[(&str, &str)] = &[
    (
        "profile-0",
        "30f7d76ba48c1faef6603ab094d68e5d1c8affc608ae77871d3c5f2189bc2621",
    ),
    (
        "llxprt-code",
        "5d081cdb45f67c7cf2a255f75463f66ca102ffa4c720cf212c20f7aa07abe440",
    ),
    (
        "llxprt-jefe",
        "edae5f266d1ccdf5bca6d7e131b21a07a2fe02aed5d06b69ba28bd97d16c38ae",
    ),
    (
        "llxprt-luther",
        "cb0d2239078ddbe17a901d9d2febf4f2c18b633e921479707106cc8d1ebf3778",
    ),
    (
        "codepuppy",
        "39a8fe7ada61764b7018669fb19077a90f5305281813cb34aa13897f87119693",
    ),
    (
        "parent-orchestrator-code",
        "db70e22623a4d8240f7470a32dd210289256e9b8e08a5ddc3f5ca1688bdf2050",
    ),
    (
        "parent-orchestrator-luther",
        "38524f6e94f0966b325b08485880604e98e1e9b2bb7876f2bc0fed71ff1291bf",
    ),
];

/// A database written before the move is still readable after it.
///
/// The digest fixtures above cover canonical bytes, but not the durable
/// tables, and a control exposed the gap: renaming a column in the epoch
/// table broke no test at all. Every suite starts from an empty database and
/// the schema uses `CREATE TABLE IF NOT EXISTS`, so a renamed column silently
/// creates a *second* table and the run proceeds against it — which is
/// precisely the "stranded in-flight run" failure #205 warns about, invisible
/// until a real run needs recovery.
///
/// This writes the table as it exists today, then reads it back through the
/// moved code, so a schema change during the move fails here rather than in
/// production.
#[test]
fn an_epoch_row_written_before_the_move_is_still_readable() {
    use luther_engine_core::recovery_epoch;

    let connection = rusqlite::Connection::open_in_memory().expect("in-memory database opens");
    recovery_epoch::init_epoch_table(&connection).expect("the epoch table initialises");

    // Written with explicit column names: an INSERT naming columns fails
    // loudly if one was renamed, where `INSERT INTO t VALUES (...)` would
    // bind positionally and keep working against a differently-shaped table.
    connection
        .execute(
            "INSERT INTO recovery_epoch (run_id, epoch, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params!["run-pre-move", 7i64, "2026-01-01T00:00:00Z"],
        )
        .expect(
            "the epoch table must still accept a row shaped as it was before the move; a \
             renamed or dropped column strands every run whose database predates the change",
        );

    let epoch: i64 = connection
        .query_row(
            "SELECT epoch FROM recovery_epoch WHERE run_id = ?1",
            rusqlite::params!["run-pre-move"],
            |row| row.get(0),
        )
        .expect("the row written before the move must be readable after it");
    assert_eq!(epoch, 7, "the persisted epoch value must survive unchanged");
}

#[test]
fn workflow_type_digests_are_unchanged() {
    let mut drifted = Vec::new();
    for (id, expected) in WORKFLOW_TYPE_DIGESTS {
        let workflow_type = resolve_workflow_type(id, &config_root())
            .unwrap_or_else(|error| panic!("workflow type `{id}` must resolve: {error:?}"));
        let actual = compute_workflow_digest(&workflow_type);
        if actual != *expected {
            drifted.push(format!("{id}: expected {expected}, got {actual}"));
        }
    }
    assert!(
        drifted.is_empty(),
        "canonical workflow digests changed, which strands in-flight runs whose capsules embed \
         the previous bytes: {drifted:?}. This requires a versioned migration, not an update to \
         the expected values."
    );
}

#[test]
fn config_digests_are_unchanged() {
    let mut drifted = Vec::new();
    for (id, expected) in CONFIG_DIGESTS {
        let config = resolve_workflow_config(id, &config_root())
            .unwrap_or_else(|error| panic!("config `{id}` must resolve: {error:?}"));
        let actual = compute_config_digest(&config);
        if actual != *expected {
            drifted.push(format!("{id}: expected {expected}, got {actual}"));
        }
    }
    assert!(
        drifted.is_empty(),
        "canonical config digests changed: {drifted:?}"
    );
}
