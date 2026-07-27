/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
pub mod adapters;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub mod cli;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod daemon;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// Hex-encoded SHA-256, shared by every caller that needs one.
/// Re-exported from `luther-engine-core` so the ~40 existing `crate::digest`
/// call sites keep working unchanged.
///
/// B1 is move-only, and rewriting every import would make the diff impossible
/// to read as a move and impossible to review for accidental semantic edits.
/// B4/B5/B6 can retire this alias once the callers themselves move.
pub use luther_engine_core as digest;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
pub mod components;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
pub mod engine;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod monitor;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub mod persistence;
pub mod polling;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod repo;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
pub mod runtime_paths;
pub mod service;
/// Typed records of what external tools actually do, as distinct from what
/// callers assume. See `docs/architecture/convergence-retrospective.md`.
pub mod tool_contract;
pub mod workflow;

#[must_use]
pub const fn project_name() -> &'static str {
    "luther-workflow"
}

#[cfg(test)]
mod tests {
    use super::project_name;

    #[test]
    fn exposes_project_name() {
        assert_eq!(project_name(), "luther-workflow");
    }
}
