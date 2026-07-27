//! Contract tests validated against output captured from the real tool.
//!
//! The fixtures under `tests/fixtures/tool-contracts/` were produced by
//! running the installed binary. Issue #174 is why they are digested rather
//! than merely present: a parser and a hand-written fixture that encode the
//! same guess agree with each other and disagree only with reality, so the
//! unit tests confirm an invention. A digest makes "this came from the tool"
//! a checkable claim.

use super::*;

fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool-contracts/ocr")
}

fn read_fixture(name: &str) -> String {
    let path = fixture_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read captured fixture {}: {error}", path.display()))
}

fn ocr_contract() -> ToolContract {
    crate::tool_contract::ocr::contract()
}

fn subcommand(name: &str) -> SubcommandContract {
    ocr_contract()
        .subcommand(name)
        .unwrap_or_else(|| panic!("{name} is recorded in the contract"))
        .clone()
}

/// Read a capture through the contract, so the declared filename is
/// load-bearing rather than decorative.
fn capture_named(subcommand_name: &str, file: &str) -> String {
    let contract = subcommand(subcommand_name);
    let capture = contract
        .captures
        .iter()
        .find(|capture| capture.file == file)
        .unwrap_or_else(|| panic!("{subcommand_name} declares a capture named {file}"));
    read_fixture(&capture.file)
}

/// Every declared capture must exist and still hash to its recorded digest.
///
/// This is the guard that makes the captures evidence. Without it a fixture
/// can be edited to agree with a mistaken parser, which is the #174 defect
/// reproduced inside the mechanism built to prevent it.
#[test]
fn every_capture_matches_its_recorded_digest() {
    let contract = ocr_contract();
    let mut checked = 0;

    for subcommand in &contract.subcommands {
        assert!(
            !subcommand.captures.is_empty(),
            "{} records behaviour with no captured evidence",
            subcommand.subcommand
        );
        for capture in &subcommand.captures {
            let path = fixture_dir().join(&capture.file);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let actual = sha256_hex(&bytes);
            assert_eq!(
                actual, capture.sha256,
                "{} no longer matches its recorded digest; if the tool changed, re-capture and \
                 update the contract, and if it did not, this file was edited by hand",
                capture.file
            );
            checked += 1;
        }
    }

    assert!(checked >= 5, "expected every capture to be checked");
}

/// The digest function must agree with the platform's SHA-256.
///
/// A hand-rolled hash that is wrong in a stable way would still make the
/// digest test pass while comparing nothing meaningful.
#[test]
fn the_digest_function_agrees_with_known_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // Spans a block boundary, exercising the padding path.
    assert_eq!(
        sha256_hex(&b"a".repeat(1000)),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

/// The pinned version must match what the capture actually recorded.
#[test]
fn the_pinned_version_matches_the_captured_version_output() {
    let captured = read_fixture("version.txt");
    let contract = ocr_contract();

    assert!(
        captured
            .split_whitespace()
            .any(|token| token == contract.version),
        "contract pins {} but the captured version output is: {}",
        contract.version,
        captured.trim()
    );
}

/// Version matching is exact, so a pin cannot accept a version it was never
/// verified against.
#[test]
fn version_matching_is_exact_not_substring() {
    let contract = ocr_contract();
    let real = read_fixture("version.txt");

    contract
        .verify_version(&real)
        .expect("the captured version must satisfy its own pin");

    for impostor in [
        "open-code-review v1.7.160 (a0b49d5b) darwin/arm64",
        "open-code-review v1.7.16-rc1 (a0b49d5b) darwin/arm64",
        "open-code-review prev1.7.16 (a0b49d5b) darwin/arm64",
        "open-code-review v1.7.1 (a0b49d5b) darwin/arm64",
        "open-code-review v9.9.9 (deadbeef) darwin/arm64",
    ] {
        let error = contract
            .verify_version(impostor)
            .expect_err("a version that is not the pinned token must be rejected");
        let message = error.to_string();
        assert!(
            message.contains(&contract.version),
            "diagnostic must name the pinned version: {message}"
        );
    }
}

/// `session show` accepts `--json` and prints a human table.
///
/// Verified against the installed binary rather than trusted from the issue
/// text. This is the #183 and #186 defect, still present in the pinned
/// version.
#[test]
fn session_show_json_is_accepted_and_ignored() {
    let captured = capture_named("session show", "session-show--json.stdout");

    assert!(
        serde_json::from_str::<serde_json::Value>(&captured).is_err(),
        "the capture is expected to be a human table, not JSON; if the tool now emits JSON, \
         re-capture and update the contract"
    );

    let contract = subcommand("session show");
    for required in &contract.required_fields {
        assert!(
            captured.contains(&required.name),
            "the capture is missing {}, which is needed for {}",
            required.name,
            required.used_for
        );
    }
    // The real table names the session it was asked about; an invented
    // fixture that merely starts with the header would not.
    assert!(
        captured.contains("8e17b8ad-373c-4742-8cf7-99b239de7ed3"),
        "the capture should echo the requested session id"
    );

    let error = contract
        .require_honoured("--json")
        .expect_err("--json must be reported as ignored");

    let ContractViolation::FlagIgnored { use_instead, .. } = &error else {
        panic!("expected an ignored-flag violation, got: {error}");
    };
    assert!(
        !use_instead.trim().is_empty(),
        "an ignored flag must name a usable alternative"
    );
    assert!(
        error.to_string().contains(use_instead.as_str()),
        "the diagnostic must carry the alternative: {error}"
    );
}

/// `session list` genuinely honours `--json`, and the fields consumers read
/// are present in the capture.
#[test]
fn session_list_json_is_honoured_and_carries_the_expected_fields() {
    let captured = capture_named("session list", "session-list--json.stdout");

    let parsed: serde_json::Value =
        serde_json::from_str(&captured).expect("session list --json is expected to emit real JSON");
    let entries = parsed
        .as_array()
        .expect("session list --json emits an array when sessions exist");
    let first = entries
        .first()
        .expect("the capture should contain at least one session");

    let contract = subcommand("session list");
    for required in &contract.required_fields {
        assert!(
            first.get(&required.name).is_some(),
            "captured session is missing {}, which is needed for {}; if the tool renamed it, \
             every consumer reading that field must be updated",
            required.name,
            required.used_for
        );
    }

    contract
        .require_honoured("--json")
        .expect("--json is honoured for session list");
}

/// `session list` honours `--repo`; the working directory is its default, not
/// its key.
///
/// The discriminating experiment: varying only the working directory cannot
/// separate those two, because both predict the same result.
#[test]
fn session_list_honours_the_repo_argument() {
    let without_repo = capture_named("session list", "session-list--json--foreign-cwd.stdout");
    let with_repo = capture_named(
        "session list",
        "session-list--json--foreign-cwd-with-repo.stdout",
    );

    assert_eq!(
        without_repo.trim(),
        "null",
        "from an unrelated directory with no --repo, the tool should find nothing"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(&with_repo).expect("--repo output should be JSON");
    assert!(
        parsed.as_array().is_some_and(|entries| !entries.is_empty()),
        "the same command from the same directory WITH --repo should find sessions, which is \
         what makes --repo honoured"
    );

    let contract = subcommand("session list");
    assert_eq!(
        contract.state_key,
        StateKey::PathArgument { flag: "--repo" },
        "session list selects state by path argument, defaulting to the working directory"
    );
    contract
        .require_honoured("--repo")
        .expect("--repo is honoured for session list");
}

/// `session show` ignores `--repo` and keys off the working directory.
///
/// Same tool, same flag, opposite behaviour from `session list`. This is why
/// behaviour is recorded per subcommand.
#[test]
fn session_show_ignores_the_repo_argument() {
    let captured = capture_named("session show", "session-show--foreign-cwd-with-repo.stderr");

    assert!(
        captured.contains("/sessions/tmp/"),
        "with --repo pointing at the repository but the process in /tmp, the tool should still \
         derive its store path from the working directory; capture was: {}",
        captured.trim()
    );

    let contract = subcommand("session show");
    assert_eq!(contract.state_key, StateKey::LogicalWorkingDirectory);

    let error = contract
        .require_honoured("--repo")
        .expect_err("--repo must be reported as ignored for session show");
    assert!(
        matches!(error, ContractViolation::FlagIgnored { .. }),
        "expected an ignored-flag violation: {error}"
    );
}

/// Where stdout is not authoritative, the contract must say so.
#[test]
fn session_show_declares_a_durable_result_source() {
    match &subcommand("session show").result_source {
        ResultSource::DurableArtifact { description } => {
            assert!(
                description.contains("jsonl"),
                "the durable source should identify the session log: {description}"
            );
        }
        ResultSource::Stdout => panic!(
            "session show prints a human table, so stdout cannot be the authoritative source"
        ),
    }
}

/// Depending on an unrecorded flag is an error, not a silent pass.
#[test]
fn an_unrecorded_flag_is_rejected() {
    let error = subcommand("session show")
        .require_honoured("--totally-uncaptured")
        .expect_err("an unrecorded flag must not be assumed to work");

    assert!(
        matches!(error, ContractViolation::FlagNotRecorded { .. }),
        "unrecorded flags must be distinguishable from ignored ones: {error}"
    );
}

/// Every ignored flag must name a non-empty alternative.
///
/// The value of recording an ignored flag is the remediation it carries; an
/// empty one leaves the caller exactly where the decode error would have.
#[test]
fn every_ignored_flag_names_an_alternative() {
    for subcommand in &ocr_contract().subcommands {
        for (flag, behaviour) in &subcommand.flags {
            if let FlagBehaviour::AcceptedAndIgnored { use_instead } = behaviour {
                assert!(
                    !use_instead.trim().is_empty(),
                    "{} records {flag} as ignored without naming an alternative",
                    subcommand.subcommand
                );
            }
        }
    }
}

/// The fields a caller must read to locate durable evidence stay recorded.
///
/// Without this, deleting an entry from `required_fields` silently narrows
/// what the capture is checked for, and the contract weakens without any test
/// failing.
///
/// The list is anchored to the durable-artifact description rather than to a
/// hardcoded set, so the contract cannot be trimmed on one side only: dropping
/// `file_path` while still claiming the jsonl is reachable "by session list's
/// file_path field" is a contradiction this catches.
#[test]
fn the_fields_needed_to_reach_durable_evidence_stay_recorded() {
    let listing = subcommand("session list");
    let recorded: Vec<&str> = listing
        .required_fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();

    let ResultSource::DurableArtifact { description } = &subcommand("session show").result_source
    else {
        panic!("session show must declare a durable artifact");
    };

    for field in &recorded {
        assert!(!field.trim().is_empty(), "a required field must be named");
    }

    for field in ["session_id", "file_path", "repo_dir"] {
        assert!(
            recorded.contains(&field),
            "session list must record {field} as required; a caller reaching the durable \
             artifact reads it, and an unrecorded field can be renamed by the tool without \
             any test noticing"
        );
    }

    assert!(
        description.contains("file_path"),
        "the durable-artifact description names the field that locates it: {description}"
    );
}

/// The pinned version must match the version CI actually installs.
///
/// Binding to the workflow rather than to a source comment is deliberate: a
/// comment can drift silently, and issue #186 is what happens when tool
/// knowledge lives only in prose. Upgrading the tool in CI now fails this
/// test until the contract is re-verified.
#[test]
fn the_pinned_version_matches_the_version_ci_installs() {
    let workflow = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.github/workflows/ocr-pr-review.yml");
    let source = std::fs::read_to_string(&workflow)
        .unwrap_or_else(|error| panic!("read {}: {error}", workflow.display()));

    let declared = source
        .lines()
        .find_map(|line| line.trim().strip_prefix("OCR_VERSION:"))
        .map(|value| value.trim().trim_matches('"').to_string())
        .expect("the review workflow declares OCR_VERSION");

    let pinned = ocr_contract().version;
    assert_eq!(
        format!("v{declared}"),
        pinned,
        "the contract is pinned to {pinned} but CI installs {declared}; re-capture the contract \
         fixtures against the version CI runs"
    );
}
