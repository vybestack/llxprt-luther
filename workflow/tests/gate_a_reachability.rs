//! Gate A-R reachability measurement.
//!
//! The project's history is of green signals that never crossed the product
//! boundary. This suite is written so that a green result here means the
//! measurement matched its recorded expectation -- not that the product
//! succeeded. The expectation is currently FAIL, and an unexplained move to
//! PASS fails the assertion and demands an explanation.

// The harness generates bash scripts, installs them with Unix execute
// permissions, and intercepts tools by name on PATH. Those mechanics are
// Unix-specific by construction, so the suite is gated rather than partially
// adapted -- a half-ported harness would report platform gaps as product
// failures.
#![cfg(unix)]

mod gate_a_harness;

use gate_a_harness::{run_gate_a, GateOutcome, GateRun};

/// The recorded expectation for Gate A-R on this commit.
///
/// Changing it is a deliberate act that belongs in a PR description with the
/// evidence that justifies it. See `docs/architecture/product-gates.md`.
const EXPECTED_GATE_A_OUTCOME: GateOutcome = GateOutcome::Fail;

/// The furthest step the product is currently observed to reach.
///
/// Recorded so that regression *and* progress are both visible. A run that
/// stops earlier than this is a regression; one that gets further is progress
/// that should be recorded here in the same PR.
const EXPECTED_FURTHEST_STEP: &str = "scope_measure";

/// Digest of the workflow definition the child actually resolved.
///
/// Pinned so that changing the workflow the run loads is a visible event.
/// Recording a digest without asserting on it detects nothing.
///
/// Only the workflow digest is pinned. The resolved *config* digest embeds
/// per-run values (workspace and artifact paths under a fresh temporary
/// directory) and therefore differs on every run; asserting on it would be a
/// flake rather than a control. Config drift is covered instead by
/// `the_resolved_config_reflects_the_file_on_disk`.
/// Implementation profile the shipping config declares.
const EXPECTED_IMPLEMENTATION_PROFILE: &str = "gpt56solhigh";

const EXPECTED_RESOLVED_WORKFLOW_DIGEST: &str =
    "565057dee0bc503abac403172750608a26737d9ce380553727bf8ed1f084a8f1";

/// Steps the product is currently observed to reach, in order.
///
/// The terminal step alone cannot detect a regression that drops a step
/// without changing where the run stops, so the trajectory is pinned. Any
/// change here is a behavioural change and belongs in the PR that causes it.
const EXPECTED_STEPS_REACHED: &[&str] = &[
    "workspace_ownership_verify",
    "select_issue",
    "setup_workspace_init",
    "git_config_publish",
    "workspace_ownership",
    "setup_workspace",
    "task_charter",
    "route_pr_path",
    "fetch_issue",
    "prepare_plan",
    "create_plan",
    "evaluate_plan",
    "plan_gate",
    "workflow_auth_preflight_plan",
    "implement",
    "scope_measure",
];

/// Gate A-R: explicit work item -> run -> new draft PR.
///
/// Asserts the observed outcome equals the recorded expectation. This check is
/// green while the product cannot reach the gate, and turns red the moment the
/// result changes in either direction.
#[test]
fn gate_a_reachability_matches_the_recorded_expectation() {
    let result = run_gate_a(&GateRun::new(4242));
    let report = serde_json::to_string_pretty(&result).expect("result serializes");
    eprintln!("{report}");

    assert_eq!(
        result.outcome, EXPECTED_GATE_A_OUTCOME,
        "Gate A-R outcome changed. If the product now reaches the gate, update \
         EXPECTED_GATE_A_OUTCOME in the same PR and include this report:\n{report}"
    );

    assert_eq!(
        result.terminal_step.as_deref(),
        Some(EXPECTED_FURTHEST_STEP),
        "the furthest step reached changed. Update EXPECTED_FURTHEST_STEP in the \
         same PR that changes the behaviour, with this report:\n{report}"
    );

    assert_eq!(
        result.steps_reached, EXPECTED_STEPS_REACHED,
        "the step trajectory changed. A step gained or lost is a behavioural \
         change even when the run still stops in the same place; update \
         EXPECTED_STEPS_REACHED in the PR that causes it:\n{report}"
    );
}

/// The harness must start the shipping binary, observed rather than intended.
#[test]
fn the_harness_executes_the_shipping_binary() {
    let result = run_gate_a(&GateRun::new(4242));

    let observed = result
        .observed_binary
        .as_deref()
        .expect("the launched process must report the binary it executed");

    assert!(
        observed.ends_with("luther-workflow"),
        "the process that ran must be the product binary, observed as: {observed}"
    );

    // The digest is computed by the child against the file it is about to
    // exec, so it cannot be satisfied by harness configuration alone.
    let digest = result
        .observed_binary_digest
        .as_deref()
        .expect("the launched process must report its own digest");
    let expected = {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(observed).expect("observed binary is readable");
        format!("{:x}", Sha256::digest(&bytes))
    };
    assert_eq!(
        digest, expected,
        "the digest reported by the child must match the binary on disk"
    );

    assert!(
        result.exit_code.is_some(),
        "the binary must run to completion and yield an exit status"
    );
}

/// A change to the config the run loads must be observable.
///
/// The resolved config digest embeds per-run paths, so it cannot be pinned
/// directly. This pins the resolved implementation profile instead. The
/// expected value is a literal: deriving it from the same file the product
/// reads would move both sides together and detect nothing.
#[test]
fn the_resolved_config_reflects_the_file_on_disk() {
    let result = run_gate_a(&GateRun::new(4242));

    assert!(
        result
            .agent_invocations
            .iter()
            .any(|step| step == "IMPLEMENTATION_COMPLETE"),
        "the run must reach the implementation agent for this check to mean anything"
    );
    assert_eq!(
        result.resolved_profile.as_deref(),
        Some(EXPECTED_IMPLEMENTATION_PROFILE),
        "the profile the child resolved changed. Update \
         EXPECTED_IMPLEMENTATION_PROFILE in the PR that changes the configuration."
    );
}

/// The agent must actually be spawned for the implementation step.
///
/// Without this, a no-change control is vacuous: a run that dies before the
/// agent is reached produces no change for reasons that have nothing to do
/// with the agent, and the control cannot tell the two cases apart.
#[test]
fn the_implementation_agent_is_actually_invoked() {
    let result = run_gate_a(&GateRun::new(4242));

    assert!(
        result
            .agent_invocations
            .iter()
            .any(|step| step == "IMPLEMENTATION_COMPLETE"),
        "the implementation agent must be spawned; observed invocations: {:?}\nreport:\n{}",
        result.agent_invocations,
        serde_json::to_string_pretty(&result).unwrap()
    );
}

/// Primary construct-validity control.
///
/// With an agent that changes nothing, the gate must not pass. A harness that
/// passes here is measuring its own scaffolding rather than the product.
#[test]
fn a_run_that_changes_nothing_cannot_pass_the_gate() {
    let mut run = GateRun::new(4242);
    run.agent_makes_change = false;

    let result = run_gate_a(&run);

    assert_eq!(
        result.outcome,
        GateOutcome::Fail,
        "a run producing no change must not reach a draft PR; report:\n{}",
        serde_json::to_string_pretty(&result).unwrap()
    );
    assert!(
        result.pushed_ref.is_none(),
        "no branch may reach the remote when nothing changed"
    );
}

/// A pre-existing open PR must not let the gate pass.
///
/// Gate A-R requires a *new* draft PR. Reusing an open one satisfies the
/// wording while proving nothing about reachability.
#[test]
fn a_pre_existing_open_pr_cannot_satisfy_the_gate() {
    let mut run = GateRun::new(4242);
    run.github.existing_pr = Some(gate_a_harness::fake_gh::ExistingPr {
        number: 99,
        is_draft: false,
        state: "OPEN".to_string(),
    });

    let result = run_gate_a(&run);

    // Both conditions independently. An `A || B` form would pass when the
    // workflow failed to notice the existing PR and called `pr create` anyway,
    // so long as the gate happened to fail for some unrelated reason.
    assert_eq!(
        result.outcome,
        GateOutcome::Fail,
        "a pre-existing PR must not satisfy the gate; report:\n{}",
        serde_json::to_string_pretty(&result).unwrap()
    );
    assert!(
        !result.created_draft_pr,
        "no new draft PR may be recorded when one already exists; report:\n{}",
        serde_json::to_string_pretty(&result).unwrap()
    );
}

/// The fake GitHub must fail closed on anything it was not taught.
///
/// A stub that answers plausibly to an unknown command silently widens the
/// contract and lets the harness pass on behaviour nobody captured.
#[test]
fn an_unrecognized_gh_invocation_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("gh.log");
    let script = gate_a_harness::fake_gh::FakeGitHub::new(1).script(&log);
    let path = root.path().join("gh");
    gate_a_harness::install_script(&path, &script);

    let output = gate_a_harness::spawn_tolerating_busy_text(
        std::process::Command::new(&path).args(["release", "delete", "v1.0.0"]),
    );

    assert!(
        !output.status.success(),
        "an uncaptured invocation must fail rather than return a default"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unrecognized invocation"),
        "the failure must name the uncaptured invocation"
    );
}

/// Config identity must come from what the child recorded, not from a file the
/// harness chose to hash.
#[test]
fn the_recorded_config_identity_comes_from_the_child() {
    let result = run_gate_a(&GateRun::new(4242));

    let workflow_digest = result
        .resolved_workflow_digest
        .as_deref()
        .expect("the child must persist a resolved workflow digest");
    let config_digest = result
        .resolved_config_digest
        .as_deref()
        .expect("the child must persist a resolved config digest");

    assert_eq!(
        workflow_digest, EXPECTED_RESOLVED_WORKFLOW_DIGEST,
        "the resolved workflow changed. Update EXPECTED_RESOLVED_WORKFLOW_DIGEST \
         in the PR that changes the workflow definition."
    );
    assert_eq!(config_digest.len(), 64, "config digest must be SHA-256");
    assert!(
        result
            .canonical_config_root
            .as_deref()
            .is_some_and(|root| !root.is_empty()),
        "the child must record the config root it resolved from"
    );
}
