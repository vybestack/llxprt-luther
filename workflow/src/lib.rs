/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
pub mod engine;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
pub mod adapters;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P05
pub mod persistence;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P03
pub mod workflow;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod monitor;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod repo;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
pub mod service;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
pub mod runtime_paths;
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub mod cli;

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
