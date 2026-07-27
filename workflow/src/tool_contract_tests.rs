//! Contract tests validated against output captured from the real tool.
//!
//! The fixtures under `tests/fixtures/tool-contracts/` were produced by
//! running the installed binary, not by writing what it was expected to
//! print. Issue #174 is the reason: a parser and a hand-written fixture that
//! encode the same guess agree with each other and disagree only with
//! reality, so the unit tests confirm an invention.

use super::*;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tool-contracts/ocr")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read captured fixture {}: {error}", path.display()))
}

fn ocr_contract() -> ToolContract {
    crate::tool_contract::ocr::contract()
}

/// The pinned version must match what the capture actually recorded.
///
/// Without this the contract could drift from its own evidence while still
/// looking authoritative.
#[test]
fn the_pinned_version_matches_the_captured_version_output() {
    let captured = fixture("version.txt");
    let contract = ocr_contract();

    assert!(
        captured.contains(&contract.version),
        "contract pins {} but the captured version output is: {}",
        contract.version,
        captured.trim()
    );
}

/// A different installed version must fail closed.
#[test]
fn a_version_mismatch_is_rejected_and_names_both_versions() {
    let contract = ocr_contract();

    let error = contract
        .verify_version("open-code-review v9.9.9 (deadbeef) darwin/arm64")
        .expect_err("a foreign version must not be accepted");

    let message = error.to_string();
    assert!(
        message.contains(&contract.version) && message.contains("9.9.9"),
        "diagnostic must name both the pinned and the observed version: {message}"
    );
}

/// `session show` accepts `--json` and prints a human table.
///
/// Verified against the installed binary rather than trusted from the issue
/// text. This is the #183 and #186 defect, still present in the pinned
/// version.
#[test]
fn session_show_json_is_accepted_and_ignored() {
    let captured = fixture("session-show--json.stdout");

    assert!(
        serde_json::from_str::<serde_json::Value>(&captured).is_err(),
        "the capture is expected to be a human table, not JSON; if the tool now emits JSON, \
         re-capture and update the contract"
    );
    assert!(
        captured.starts_with("Session:"),
        "captured output should begin with the human table header, got: {}",
        &captured[..captured.len().min(40)]
    );

    let contract = ocr_contract();
    let subcommand = contract
        .subcommand("session show")
        .expect("session show is recorded");

    let error = subcommand
        .require_honoured("--json")
        .expect_err("--json must be reported as ignored");

    assert!(
        error.to_string().contains("use"),
        "the diagnostic must name what to use instead: {error}"
    );
}

/// `session list` genuinely honours `--json`.
///
/// The two subcommands differ, which is why behaviour is recorded per
/// subcommand rather than per tool.
#[test]
fn session_list_json_is_honoured() {
    let captured = fixture("session-list--json.stdout");

    serde_json::from_str::<serde_json::Value>(&captured)
        .expect("session list --json is expected to emit real JSON");

    let contract = ocr_contract();
    contract
        .subcommand("session list")
        .expect("session list is recorded")
        .require_honoured("--json")
        .expect("--json is honoured for session list");
}

/// Session lookup keys off the process working directory.
///
/// The captures are the controlled experiment from #182: identical command,
/// different working directory, different answer.
#[test]
fn session_lookup_keys_off_the_process_working_directory() {
    let from_repo = fixture("session-list--json.stdout");
    let from_elsewhere = fixture("session-list--json--foreign-cwd.stdout");

    assert!(
        from_repo.contains("session_id"),
        "the repository capture should list sessions"
    );
    assert_eq!(
        from_elsewhere.trim(),
        "null",
        "the same command from another directory should find nothing, which is what makes the \
         working directory the lookup key"
    );

    assert_eq!(
        ocr_contract()
            .subcommand("session list")
            .expect("recorded")
            .state_key,
        StateKey::ProcessWorkingDirectory
    );
}

/// Where stdout is not authoritative, the contract must say so.
#[test]
fn session_show_declares_a_durable_result_source() {
    let contract = ocr_contract();
    let subcommand = contract.subcommand("session show").expect("recorded");

    match &subcommand.result_source {
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
    let contract = ocr_contract();
    let error = contract
        .subcommand("session show")
        .expect("recorded")
        .require_honoured("--totally-uncaptured")
        .expect_err("an unrecorded flag must not be assumed to work");

    assert!(
        matches!(error, ContractViolation::FlagNotRecorded { .. }),
        "unrecorded flags must be distinguishable from ignored ones: {error}"
    );
}

/// The shipping evidence reader claims verification against a specific
/// version; that claim must match the pinned contract.
///
/// `.github/scripts/ocr-session-evidence.js` already reads the durable
/// artifact rather than trusting `--json`, but it records the version it was
/// verified against in a comment, where nothing enforces it. Issue #186 is
/// what happens when tool knowledge lives only in prose: the same fact was
/// written down once before and did not survive a rewrite. This test fails if
/// the two drift apart, so upgrading the tool forces both to be re-verified.
#[test]
fn the_shipping_evidence_reader_agrees_with_the_pinned_version() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.github/scripts/ocr-session-evidence.js");
    let source = std::fs::read_to_string(&script)
        .unwrap_or_else(|error| panic!("read {}: {error}", script.display()));

    let pinned = ocr_contract().version;
    let bare = pinned.trim_start_matches('v');

    assert!(
        source.contains(bare),
        "{} claims verification against a different version than the contract pins ({pinned}); \
         re-capture the contract fixtures and update both together",
        script.display()
    );
}

/// Every contract that names a capture must point at a file that exists.
#[test]
fn every_declared_capture_exists() {
    for subcommand in &ocr_contract().subcommands {
        if let Some(name) = &subcommand.captured_output {
            let text = fixture(name);
            assert!(
                !text.is_empty(),
                "capture {name} for {} is empty",
                subcommand.subcommand
            );
        }
    }
}
