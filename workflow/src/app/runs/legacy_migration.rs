use super::*;

/// Handle `runs migrate-legacy-ownership` (issue 158 recoverable migration).
///
/// This is a narrowly scoped operator action that publishes the bootstrap
/// workspace ownership marker for a provenance-less, marker-less legacy row.
/// It requires the exact persisted workspace path, the canonical config root
/// to record in provenance, and explicit `--confirm`.
///
/// @plan:PLAN-20260722-ISSUE158-LEGACY-OWNERSHIP-MIGRATION
pub fn handle_runs_migrate_legacy_ownership(
    args: &luther_workflow::cli::RunsMigrateLegacyOwnershipArgs,
) {
    if !args.confirm {
        eprintln!(
            "Error: migrate-legacy-ownership requires --confirm to proceed. \
             This operation publishes a workspace ownership marker for a legacy row."
        );
        process::exit(1);
    }
    let store = require_runs_store(&args.run_id);
    let outcome = match luther_workflow::engine::continuation::legacy_ownership_migration::migrate_legacy_ownership(
        store.conn(),
        &args.run_id,
        &args.workspace,
        &args.config_root,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("Error: {error}");
            process::exit(1);
        }
    };
    let detail = match &outcome {
        luther_workflow::engine::continuation::legacy_ownership_migration::LegacyMigrationOutcome::Migrated => {
            "migrated"
        }
        luther_workflow::engine::continuation::legacy_ownership_migration::LegacyMigrationOutcome::AlreadyCompleted => {
            "already_completed"
        }
        luther_workflow::engine::continuation::legacy_ownership_migration::LegacyMigrationOutcome::IdempotentlyCompleted => {
            "idempotently_completed"
        }
    };
    if args.json {
        let value = serde_json::json!({
            "run_id": args.run_id,
            "workspace": args.workspace,
            "config_root": args.config_root,
            "outcome": detail,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        println!(
            "Legacy ownership migration for run '{}': {detail}.",
            args.run_id
        );
    }
}
