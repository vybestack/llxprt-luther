//! The composition root: where the product decides what it is.
//!
//! The engine used to answer this question. `ExecutorRegistry::with_defaults`
//! installed LLxprt, GitHub PR handling, feedback evaluation, remediation,
//! parent orchestration, scope control, workspace ownership, and merge waiting
//! — so any consumer of the "generic engine" received the entire issue-fixing
//! product whether or not it wanted it.
//!
//! Assembling the catalog here inverts that. The engine offers bundles; the
//! application chooses. A different product picks a different set, and nothing
//! in the engine has to change to allow it.

use luther_workflow::engine::executor::ExecutorRegistry;

/// The catalog for the GitHub issue-fixing product.
///
/// Composed of all three bundles, which is exactly what the previous
/// `with_defaults` installed. The set is deliberately unchanged: B2 moves the
/// decision, not the behaviour, and every in-flight run must resolve the same
/// step types after this change as before it.
#[must_use]
pub fn issue_fixing_catalog() -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register_core_bundle();
    registry.register_software_change_bundle();
    registry.register_github_followup_executors();
    registry.register_feedback_and_remediation_executors();
    registry
}

/// A catalog with no domain components, for a consumer that wants the engine
/// and none of this product.
///
/// This exists to be *used* rather than described: the claim "the engine can
/// be had without the domain" is only worth making if something constructs
/// that registry and observes what it contains. The test below does exactly
/// that, so the function is not dead weight kept for a future caller.
///
/// Only the test constructs it today, hence `cfg(test)`: shipping an unused
/// public constructor would be the same "declared but unexercised" pattern
/// this issue is correcting. B5/B6 make it a production entry point when a
/// second consumer exists.
#[cfg(test)]
#[must_use]
pub fn core_only_catalog() -> ExecutorRegistry {
    let mut registry = ExecutorRegistry::new();
    registry.register_core_bundle();
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Composing without the domain bundles yields a usable but strictly
    /// smaller catalog.
    ///
    /// The product catalog installed every bundle unconditionally, so this
    /// was previously not expressible at all. Asserting `shell` survives
    /// while `llxprt` does not is the difference between a real alternative
    /// composition and an empty registry that trivially excludes everything.
    #[test]
    fn the_core_only_catalog_excludes_the_product_without_being_empty() {
        let core = core_only_catalog();
        let product = issue_fixing_catalog();

        assert!(
            core.contains_step_type("shell") && core.contains_step_type("verify"),
            "a core catalog that cannot run a command is not a usable engine"
        );
        for domain in ["llxprt", "github_pr_checks", "pr_remediation_plan"] {
            assert!(
                !core.contains_step_type(domain),
                "core-only catalog still contains the domain step type `{domain}`"
            );
            assert!(
                product.contains_step_type(domain),
                "`{domain}` must exist in the product catalog, or excluding it proves nothing"
            );
        }
        assert!(
            core.registered_step_types().len() < product.registered_step_types().len(),
            "the core catalog must be strictly smaller than the product catalog"
        );
    }
}
