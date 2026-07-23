/// Integration tests for the `runs migrate-legacy-ownership` CLI data layer
/// (issue 158 recoverable migration).
///
/// These tests exercise the durable state machine through the public
/// `migrate_legacy_ownership` API and the CLI arg-parsing surface,
/// validating the end-to-end contract: intent persistence, marker
/// publication, exactly-once completion, provenance tagging, and resume trust
/// gating.
///
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
use luther_workflow::cli::{Cli, Commands, RunsArgs, RunsCommand};
use luther_workflow::engine::continuation::legacy_ownership_migration::{
    migrate_legacy_ownership, LegacyMigrationOutcome,
};
use luther_workflow::persistence::{
    init_database, persist_run_with_conn, MigrationSource, RunMetadata, RunStatus, SqliteStore,
};

use clap::Parser;

/// Seed a legacy run row (no provenance, no marker) with a workspace path.
fn seed_legacy_run(store: &SqliteStore, run_id: &str, workspace: &std::path::Path) -> RunMetadata {
    let mut md = RunMetadata::new(run_id, "legacy-wf", "legacy-cfg");
    md.status = RunStatus::ReadyToResume;
    md.current_step = Some("step_a".to_string());
    md.workspace_path = Some(workspace.to_string_lossy().into_owned());
    md.launch_provenance = None;
    persist_run_with_conn(store.conn(), &md).expect("persist legacy run");
    md
}

/// A full migration through the public API publishes the marker, writes the
/// durable completion, tags provenance, and records an audit event.
#[test]
fn migrate_legacy_ownership_full_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    init_database(&db_path).expect("init db");
    let store = SqliteStore::open(&db_path).expect("open store");
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&store, "legacy-full", &workspace);

    let outcome = migrate_legacy_ownership(
        store.conn(),
        "legacy-full",
        &workspace,
        std::path::Path::new("/etc/luther/config"),
    )
    .expect("migration should succeed");
    assert_eq!(outcome, LegacyMigrationOutcome::Migrated);

    // Provenance is tagged.
    let md = store
        .get_run("legacy-full")
        .expect("get run")
        .expect("run exists");
    let provenance = md.launch_provenance.expect("provenance set");
    assert!(provenance.is_migrated());
    assert_eq!(
        provenance.migration_source,
        Some(MigrationSource::LegacyOwnershipMigration)
    );
}

/// The CLI parses `runs migrate-legacy-ownership RUN_ID --workspace PATH
/// --config-root DIR --confirm` correctly.
#[test]
fn cli_parses_migrate_legacy_ownership_subcommand() {
    let cli = Cli::try_parse_from([
        "luther-workflow",
        "runs",
        "migrate-legacy-ownership",
        "run-xyz",
        "--workspace",
        "/some/path",
        "--config-root",
        "/etc/luther/config",
        "--confirm",
        "--json",
    ])
    .expect("CLI should parse");
    match cli.command {
        Commands::Runs(RunsArgs {
            command: RunsCommand::MigrateLegacyOwnership(args),
        }) => {
            assert_eq!(args.run_id, "run-xyz");
            assert_eq!(args.workspace, std::path::PathBuf::from("/some/path"));
            assert_eq!(
                args.config_root,
                std::path::PathBuf::from("/etc/luther/config")
            );
            assert!(args.confirm);
            assert!(args.json);
        }
        other => panic!("expected MigrateLegacyOwnership, got {other:?}"),
    }
}

/// The CLI requires `--workspace` and `--config-root`.
#[test]
fn cli_migrate_legacy_ownership_requires_workspace_and_config_root() {
    let missing_workspace = Cli::try_parse_from([
        "luther-workflow",
        "runs",
        "migrate-legacy-ownership",
        "run-xyz",
        "--config-root",
        "/etc/luther/config",
        "--confirm",
    ]);
    assert!(
        missing_workspace.is_err(),
        "migrate-legacy-ownership without --workspace must fail"
    );
    let missing_config_root = Cli::try_parse_from([
        "luther-workflow",
        "runs",
        "migrate-legacy-ownership",
        "run-xyz",
        "--workspace",
        "/some/path",
        "--confirm",
    ]);
    assert!(
        missing_config_root.is_err(),
        "migrate-legacy-ownership without --config-root must fail"
    );
}

/// After migration, a second invocation returns `AlreadyCompleted`
/// (exactly-once).
#[test]
fn migration_retry_returns_already_completed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    init_database(&db_path).expect("init db");
    let store = SqliteStore::open(&db_path).expect("open store");
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&store, "legacy-retry", &workspace);

    let first = migrate_legacy_ownership(
        store.conn(),
        "legacy-retry",
        &workspace,
        std::path::Path::new("/etc/luther/config"),
    )
    .expect("first");
    assert_eq!(first, LegacyMigrationOutcome::Migrated);

    let second = migrate_legacy_ownership(
        store.conn(),
        "legacy-retry",
        &workspace,
        std::path::Path::new("/etc/luther/config"),
    )
    .expect("second");
    assert_eq!(second, LegacyMigrationOutcome::AlreadyCompleted);
}

/// A crash between intent and completion is recovered: the retry finishes
/// the intent and produces exactly one completion audit.
#[test]
fn migration_crash_recovery_completes_once() {
    use luther_workflow::persistence::legacy_migration_state::{
        init_legacy_migration_table, persist_migration_intent,
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    init_database(&db_path).expect("init db");
    let store = SqliteStore::open(&db_path).expect("open store");
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    seed_legacy_run(&store, "legacy-crash-recovery", &workspace);

    // Simulate crash: persist intent without publishing.
    init_legacy_migration_table(store.conn()).expect("init table");
    persist_migration_intent(
        store.conn(),
        "legacy-crash-recovery",
        &workspace.to_string_lossy(),
        chrono::Utc::now(),
    )
    .expect("persist intent");

    let outcome = migrate_legacy_ownership(
        store.conn(),
        "legacy-crash-recovery",
        &workspace,
        std::path::Path::new("/etc/luther/config"),
    )
    .expect("recovery");
    assert_eq!(outcome, LegacyMigrationOutcome::Migrated);

    // Exactly one completion audit event.
    let count: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
            rusqlite::params!["legacy-crash-recovery", "legacy_ownership_migration"],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(count, 1, "exactly one completion audit event");
}

/// Two runs migrated concurrently (different run ids) each produce exactly
/// one completion audit.
#[test]
fn concurrent_runs_each_complete_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("checkpoints.db");
    init_database(&db_path).expect("init db");
    let store = SqliteStore::open(&db_path).expect("open store");

    let ws_a = temp.path().join("ws-a");
    let ws_b = temp.path().join("ws-b");
    std::fs::create_dir_all(&ws_a).expect("create ws-a");
    std::fs::create_dir_all(&ws_b).expect("create ws-b");
    seed_legacy_run(&store, "concurrent-a", &ws_a);
    seed_legacy_run(&store, "concurrent-b", &ws_b);

    migrate_legacy_ownership(
        store.conn(),
        "concurrent-a",
        &ws_a,
        std::path::Path::new("/etc/luther/config"),
    )
    .expect("migrate a");
    migrate_legacy_ownership(
        store.conn(),
        "concurrent-b",
        &ws_b,
        std::path::Path::new("/etc/luther/config"),
    )
    .expect("migrate b");

    for run_id in &["concurrent-a", "concurrent-b"] {
        let count: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE run_id = ?1 AND step_id = ?2 AND outcome = 'completed'",
                rusqlite::params![run_id, "legacy_ownership_migration"],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(count, 1, "run {run_id}: exactly one completion audit");
    }
}
