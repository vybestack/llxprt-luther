//! Gate A-R reachability measurement.
//!
//! The project's history is of green signals that never crossed the product
//! boundary. This suite is written so that a green result here means the
//! measurement matched its recorded expectation -- not that the product
//! succeeded. The expectation is currently FAIL, and an unexplained move to
//! PASS fails the assertion and demands an explanation.

mod gate_a_harness;

use gate_a_harness::{no_change_agent_script, run_gate_a, GateOutcome, GateRun};

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
const EXPECTED_FURTHEST_STEP: &str = "implement";

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
    "abandon_and_log",
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

    assert!(
        result.executed_binary.exists(),
        "the executed path must be a real file: {}",
        result.executed_binary.display()
    );
    assert_eq!(
        result.executed_binary.file_name().and_then(|n| n.to_str()),
        Some("luther-workflow"),
        "the harness must start the product binary, not a test shim"
    );
    assert!(
        result.exit_code.is_some(),
        "the binary must run to completion and yield an exit status"
    );
}

/// Primary construct-validity control.
///
/// With an agent that changes nothing, the gate must not pass. A harness that
/// passes here is measuring its own scaffolding rather than the product.
#[test]
fn a_run_that_changes_nothing_cannot_pass_the_gate() {
    let mut run = GateRun::new(4242);
    run.agent_script = no_change_agent_script();

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

    assert!(
        !result
            .gh_invocations
            .iter()
            .any(|call| call.contains("pr create"))
            || result.outcome == GateOutcome::Fail,
        "an existing PR must not be reported as a newly created one; report:\n{}",
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
    std::fs::write(&path, &script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = std::process::Command::new(&path)
        .args(["release", "delete", "v1.0.0"])
        .output()
        .expect("fake gh runs");

    assert!(
        !output.status.success(),
        "an uncaptured invocation must fail rather than return a default"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unrecognized invocation"),
        "the failure must name the uncaptured invocation"
    );
}

/// The recorded config digest must track the file actually loaded.
#[test]
fn the_recorded_config_digest_tracks_the_loaded_config() {
    let result = run_gate_a(&GateRun::new(4242));

    assert_eq!(
        result.config_digest.len(),
        64,
        "digest must be a full SHA-256"
    );
    assert!(
        result.config_path.exists(),
        "the digested config must be the shipping file"
    );

    let recomputed = {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(&result.config_path).unwrap();
        format!("{:x}", Sha256::digest(&bytes))
    };
    assert_eq!(
        result.config_digest, recomputed,
        "the digest must be of the file on disk, not a constant"
    );
}
