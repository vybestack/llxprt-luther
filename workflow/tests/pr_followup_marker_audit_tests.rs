/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
/// Marker audit coverage for Phase 03 PR follow-through touched Rust files.
use std::fs;
use std::path::Path;

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
const P03_TOUCHED_RUST_FILES: [&str; 9] = [
    "src/engine/executor.rs",
    "src/engine/executors/mod.rs",
    "src/engine/executors/pr_followup_artifacts.rs",
    "src/engine/executors/pr_followup_types.rs",
    "src/engine/executors/github_pr.rs",
    "src/engine/executors/github_feedback.rs",
    "src/engine/executors/feedback_eval.rs",
    "src/engine/executors/pr_remediation.rs",
    "tests/github_pr_followup_executor_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
const P05_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/pr_followup_artifacts.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006,REQ-PRFU-017
/// @pseudocode lines 1-7,16-33
const P06_TOUCHED_RUST_FILES: [&str; 4] = [
    "src/engine/executors/github_pr.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007,REQ-PRFU-017
/// @pseudocode lines 1-21
const P07_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/github_pr.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
const P08_TOUCHED_RUST_FILES: [&str; 4] = [
    "src/engine/executors/github_feedback.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
const P09_TOUCHED_RUST_FILES: [&str; 4] = [
    "src/engine/executor.rs",
    "src/engine/executors/feedback_eval.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013
/// @pseudocode lines 1-11
const P10_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/pr_remediation.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
const P11_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/pr_remediation.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 12-17
const P12_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/pr_remediation.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
const P13_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/pr_remediation.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
const P14_TOUCHED_RUST_FILES: [&str; 3] = [
    "src/engine/executors/pr_remediation.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
const P15_TOUCHED_RUST_FILES: [&str; 4] = [
    "src/engine/executors/github_feedback.rs",
    "src/engine/executors/mod.rs",
    "tests/github_pr_followup_executor_tests.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A
/// @pseudocode lines 1-53
const P16_TOUCHED_RUST_FILES: [&str; 3] = [
    "tests/e2e_workflow_integration.rs",
    "tests/pr_followup_workflow_integration.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @pseudocode lines 1-53
const P03_MARKER_REQUIRED_TOKENS: [&str; 3] = [
    "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03",
    "@requirement:",
    "@pseudocode lines ",
];
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-001,REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A
/// @pseudocode lines 1-53
const P17_TOUCHED_RUST_FILES: [&str; 2] = [
    "tests/e2e_workflow_integration.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-035
/// @pseudocode lines 1-53
const P18_TOUCHED_RUST_FILES: [&str; 2] = [
    "tests/pr_followup_workflow_integration.rs",
    "tests/pr_followup_marker_audit_tests.rs",
];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P19
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 41-49
const P19_TOUCHED_RUST_FILES: [&str; 1] = ["tests/pr_followup_marker_audit_tests.rs"];

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
fn p03_forbidden_marker_patterns() -> Vec<String> {
    vec![
        format!("{} {}", "@pseudocode lines", "X-Y"),
        format!("{} {}", "@pseudocode", "TBD"),
        format!("{} {}", "@pseudocode", concat!("place", "holder")),
        format!("{} {}", concat!("TO", "DO"), "API"),
        format!("{} {}", "json_path", "TBD"),
        format!("{} {}", "fixture", "TBD"),
        format!("{} {}", "assertion", "TBD"),
    ]
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn p05_forbidden_marker_patterns() -> Vec<String> {
    p03_forbidden_marker_patterns()
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05
/// @requirement:REQ-PRFU-002
/// @pseudocode lines 5-7
fn assert_file_contains_marker_set(file_path: &str, plan_token: &str) {
    let source = fs::read_to_string(file_path).unwrap_or_else(|err| {
        panic!("failed to read {file_path}: {err}");
    });

    assert!(
        source.contains(plan_token),
        "{file_path} missing {plan_token}"
    );
    assert!(
        source.contains("@requirement:"),
        "{file_path} missing @requirement"
    );
    assert!(
        source.contains("@pseudocode lines "),
        "{file_path} missing @pseudocode lines"
    );

    for token in p05_forbidden_marker_patterns() {
        assert!(!source.contains(&token), "{file_path} contains {token}");
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn p03_markers_cover_all_touched_items() {
    for file_path in P03_TOUCHED_RUST_FILES {
        let source = fs::read_to_string(file_path).unwrap_or_else(|err| {
            panic!("failed to read {file_path}: {err}");
        });

        for token in P03_MARKER_REQUIRED_TOKENS {
            assert!(source.contains(token), "{file_path} missing {token}");
        }

        for token in p03_forbidden_marker_patterns() {
            assert!(!source.contains(&token), "{file_path} contains {token}");
        }
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07
/// @requirement:REQ-PRFU-007,REQ-PRFU-017
/// @pseudocode lines 1-21
#[test]
fn p07_markers_cover_all_touched_items() {
    for file_path in P07_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P07",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06
/// @requirement:REQ-PRFU-001,REQ-PRFU-004,REQ-PRFU-005,REQ-PRFU-006,REQ-PRFU-017
/// @pseudocode lines 1-7,16-33
#[test]
fn p06_markers_cover_all_touched_items() {
    for file_path in P06_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P06",
        );
    }
}

/// @pseudocode lines 5-7
#[test]
fn p05_markers_cover_all_touched_items() {
    for file_path in P05_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P05",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08
/// @requirement:REQ-PRFU-008,REQ-PRFU-009,REQ-PRFU-017,REQ-PRFU-024,REQ-PRFU-034
/// @pseudocode lines 1-29
#[test]
fn p08_markers_cover_all_touched_items() {
    for file_path in P08_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P08",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P03
/// @requirement:REQ-PRFU-020
/// @pseudocode lines 1-53
#[test]
fn p03_touched_file_manifest_paths_exist() {
    for file_path in P03_TOUCHED_RUST_FILES {
        assert!(
            Path::new(file_path).exists(),
            "missing touched file {file_path}"
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15
/// @requirement:REQ-PRFU-015,REQ-PRFU-016,REQ-PRFU-017,REQ-PRFU-026
/// @pseudocode lines 41-49
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16
/// @requirement:REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A
/// @pseudocode lines 1-53
#[test]
fn p16_markers_cover_all_touched_items() {
    for file_path in P16_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P16",
        );
    }
}

#[test]
fn p15_markers_cover_all_touched_items() {
    for file_path in P15_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P15",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09
/// @requirement:REQ-PRFU-011,REQ-PRFU-012,REQ-PRFU-017
/// @pseudocode lines 1-23
#[test]

fn p09_markers_cover_all_touched_items() {
    for file_path in P09_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P09",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17
/// @requirement:REQ-PRFU-001,REQ-PRFU-018,REQ-PRFU-020,REQ-PRFU-020A
/// @pseudocode lines 1-53
/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18
/// @requirement:REQ-PRFU-035
/// @pseudocode lines 1-53
#[test]
fn p18_markers_cover_all_touched_items() {
    for file_path in P18_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P18",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P19
/// @requirement:REQ-PRFU-016,REQ-PRFU-017
/// @pseudocode lines 41-49
#[test]
fn p19_markers_cover_all_touched_items() {
    for file_path in P19_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P19",
        );
    }
}

#[test]
fn p17_markers_cover_all_touched_items() {
    for file_path in P17_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P17",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10
/// @requirement:REQ-PRFU-013

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11
/// @requirement:REQ-PRFU-012,REQ-PRFU-014
/// @pseudocode lines 18-28
#[test]
fn p11_markers_cover_all_touched_items() {
    for file_path in P11_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P11",
        );
    }
}

/// @pseudocode lines 1-11
#[test]
fn p10_markers_cover_all_touched_items() {
    for file_path in P10_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P10",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13
/// @requirement:REQ-PRFU-014,REQ-PRFU-017
/// @pseudocode lines 29-33
#[test]
fn p13_markers_cover_all_touched_items() {
    for file_path in P13_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P13",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12
/// @requirement:REQ-PRFU-013,REQ-PRFU-017
/// @pseudocode lines 12-17
#[test]
fn p12_markers_cover_all_touched_items() {
    for file_path in P12_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P12",
        );
    }
}

/// @plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14
/// @requirement:REQ-PRFU-015,REQ-PRFU-017
/// @pseudocode lines 34-40
#[test]
fn p14_markers_cover_all_touched_items() {
    for file_path in P14_TOUCHED_RUST_FILES {
        assert_file_contains_marker_set(
            file_path,
            "@plan:PLAN-20260429-CODERABBIT-PR-FOLLOWUP.P14",
        );
    }
}
