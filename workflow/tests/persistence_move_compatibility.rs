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

    // The schema is written out here rather than obtained from
    // `init_epoch_table`, and that is the whole point: a database on disk was
    // created by the *old* code, so building the fixture with the *current*
    // code would move the goalposts with the implementation and agree with
    // any schema it happened to produce. This literal is what a pre-move
    // database actually contains.
    connection
        .execute(
            "CREATE TABLE recovery_epoch (
                run_id TEXT PRIMARY KEY,
                epoch INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            )",
            [],
        )
        .expect("the pre-move schema is valid SQL");

    // Column names are explicit: `INSERT INTO t VALUES (...)` binds
    // positionally and would keep working against a differently-shaped table.
    connection
        .execute(
            "INSERT INTO recovery_epoch (run_id, epoch, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params!["run-pre-move", 7i64, "2026-01-01T00:00:00Z"],
        )
        .expect("a row shaped as it was before the move must still be insertable");

    // Reading through the moved code is what proves compatibility. Raw SQL
    // here would only prove that SQLite works.
    let epoch = recovery_epoch::read_epoch(&connection, "run-pre-move").expect(
        "the moved reader must understand a database written before the move; if it cannot, \
         every run whose database predates the change is stranded",
    );
    assert_eq!(epoch, 7, "the persisted epoch value must survive unchanged");

    // `init_epoch_table` must also accept a database that already has the
    // table, since that is what every restart does.
    recovery_epoch::init_epoch_table(&connection)
        .expect("initialisation must be idempotent against a pre-existing table");
    assert_eq!(
        recovery_epoch::read_epoch(&connection, "run-pre-move").expect("still readable"),
        7,
        "initialising over an existing table must not disturb the row it already held"
    );
}

/// A missing epoch table is reported, not silently read as epoch zero.
///
/// `read_epoch` combines `.optional()` with `unwrap_or(0)`, which looks like
/// it would flatten every failure into "epoch 0". It does not: `.optional()`
/// converts only `QueryReturnedNoRows`, and the `?` after it propagates a
/// missing-table error. The two cases are genuinely different and the code
/// already distinguishes them.
///
/// The distinction is worth pinning because epoch 0 is not a neutral value —
/// it is the fencing token meaning "no recovery has happened". If a lost
/// table ever did read as 0, a database with a failed migration would be
/// indistinguishable from a fresh run and would be allowed to advance from
/// zero, which is the stale-fencing case the epoch exists to prevent.
///
/// Written after a control on the DDL failed to fire and I attributed it to
/// this flattening. That was wrong, and this test is what proved it wrong.
/// The DDL control does not fire because `init_epoch_table` and the fixture
/// both define the schema, so renaming a column renames it consistently in
/// both — not because errors are being swallowed.
#[test]
fn a_missing_epoch_table_is_an_error_not_epoch_zero() {
    use luther_engine_core::recovery_epoch;

    let connection = rusqlite::Connection::open_in_memory().expect("in-memory database opens");
    // Deliberately not initialised: this is a database whose epoch table is
    // absent, which is what a failed or partial migration leaves behind.
    let result = recovery_epoch::read_epoch(&connection, "run-without-table");

    assert!(
        result.is_err(),
        "reading an absent epoch table returned {result:?} instead of an error; epoch 0 is \
         indistinguishable from a run that has never advanced, so a lost table would silently \
         re-enable advancement from zero"
    );
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
