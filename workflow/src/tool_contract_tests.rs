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

pub(super) fn read_fixture(name: &str) -> String {
    let path = fixture_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read captured fixture {}: {error}", path.display()))
}

/// Capture holding the tool's own help text for a subcommand.
fn help_capture_name(subcommand: &str) -> String {
    format!("{}--help.txt", subcommand.replace(' ', "-"))
}

pub(super) fn ocr_contract() -> ToolContract {
    crate::tool_contract::ocr::contract()
}

pub(super) fn subcommand(name: &str) -> SubcommandContract {
    ocr_contract()
        .subcommand(name)
        .unwrap_or_else(|| panic!("{name} is recorded in the contract"))
        .clone()
}

/// Read a capture through the contract, so the declared filename is
/// load-bearing rather than decorative.
pub(super) fn capture_named(subcommand_name: &str, file: &str) -> String {
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
    fn check(capture: &Capture) {
        let path = fixture_dir().join(&capture.file);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(
            sha256_hex(&bytes),
            capture.sha256,
            "{} no longer matches its recorded digest; if the tool changed, re-capture and update \
             the contract, and if it did not, this file was edited by hand",
            capture.file
        );
    }

    let contract = ocr_contract();
    let mut checked = 0;

    check(&contract.version_capture);
    checked += 1;

    for subcommand in &contract.subcommands {
        assert!(
            !subcommand.captures.is_empty(),
            "{} records behaviour with no captured evidence",
            subcommand.subcommand
        );
        for capture in &subcommand.captures {
            check(capture);
            checked += 1;
        }
    }

    assert!(checked >= 5, "expected every capture to be checked");
}

/// The captures must be mutually consistent in the ways real tool output is.
///
/// The digest guard catches a *stale* digest, not a fabricated capture: a
/// maintainer who invents output and then refreshes the digest — which the
/// re-capture procedure tells them to do — defeats it entirely. Shape checks
/// do not help either, since any three-key JSON object satisfies them.
///
/// What a forger cannot easily produce is a set of captures that agree with
/// each other the way the tool's own output does: the store path is derived
/// from the repository path, the jsonl is named for the session, and the two
/// subcommands describe the same session. Each relationship below was read off
/// the real captures, so fabricating one file now requires fabricating all of
/// them coherently.
#[test]
fn the_captures_are_mutually_consistent_the_way_real_output_is() {
    let listing = read_fixture("session-list--json.stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&listing).expect("session list capture is JSON");
    let sessions = parsed.as_array().expect("an array of sessions");
    assert!(!sessions.is_empty(), "the capture must contain sessions");

    for session in sessions {
        let text = |key: &str| {
            session
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("captured session is missing {key}"))
                .to_string()
        };
        let session_id = text("session_id");
        let file_path = text("file_path");
        let repo_dir = text("repo_dir");

        assert!(
            file_path.ends_with(&format!("{session_id}.jsonl")),
            "the durable artifact is named for its session; {file_path} does not end with \
             {session_id}.jsonl"
        );

        // The store directory is the repository path with separators replaced,
        // which is how the tool derives a per-repository slug. Checking it ties
        // file_path and repo_dir together so neither can be invented alone.
        let slug = repo_dir.trim_start_matches('/').replace('/', "-");
        assert!(
            file_path.contains(&slug),
            "the store path should embed the repository slug {slug}; got {file_path}"
        );

        assert!(
            std::path::Path::new(&repo_dir).is_absolute()
                && std::path::Path::new(&file_path).is_absolute(),
            "captured paths are absolute"
        );
    }

    assert_captures_describe_the_same_session(sessions);
    assert_the_repo_argument_reaches_the_same_store(&listed_ids(sessions));
}

/// Session ids reported by the `session list` capture, in order.
fn listed_ids(sessions: &[serde_json::Value]) -> Vec<String> {
    sessions
        .iter()
        .filter_map(|session| session.get("session_id"))
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The two subcommands must describe the same session, which is what makes
/// the `session show` capture evidence about this store rather than an
/// unrelated fragment.
fn assert_captures_describe_the_same_session(sessions: &[serde_json::Value]) {
    let shown = read_fixture("session-show--json.stdout");
    let listed_ids = listed_ids(sessions);
    let described = listed_ids
        .iter()
        .find(|id| shown.contains(id.as_str()))
        .expect("the session show capture describes a session that session list reports");

    // The table must also echo the durable path and repository that session
    // list reports for that same session. A one-line forgery carrying only
    // the header and an id cannot satisfy this.
    let described_session = sessions
        .iter()
        .find(|session| {
            session
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                == Some(described.as_str())
        })
        .expect("the described session is in the listing");
    for key in ["file_path", "repo_dir"] {
        let value = described_session
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("listing is missing {key}"));
        assert!(
            shown.contains(value),
            "the session show capture should report the same {key} as session list ({value}); \
             got: {}",
            shown.trim()
        );
    }
}

/// The `--repo` capture reaches the same store from another directory, so it
/// must report the same sessions and the denial capture must name the session
/// the tool went looking for.
fn assert_the_repo_argument_reaches_the_same_store(listed_ids: &[String]) {
    let with_repo = read_fixture("session-list--json--foreign-cwd-with-repo.stdout");
    let with_repo_parsed: serde_json::Value =
        serde_json::from_str(&with_repo).expect("--repo capture is JSON");
    let with_repo_ids: Vec<String> = with_repo_parsed
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|session| session.get("session_id"))
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect();
    assert_eq!(
        with_repo_ids, listed_ids,
        "reaching the same store via --repo must report the same sessions"
    );

    // The failure capture must name the store path the tool actually derived
    // from the foreign working directory, not merely contain a plausible
    // substring.
    let denied = read_fixture("session-show--foreign-cwd-with-repo.stderr");
    let listed_id = listed_ids.first().expect("at least one session");
    assert!(
        denied.contains(listed_id) && denied.contains(&format!("{listed_id}.jsonl")),
        "the capture should show the tool looking for the requested session's jsonl under the \
         working directory's slug; got: {}",
        denied.trim()
    );
}

/// Every recorded flag must appear in the tool's own help output.
///
/// Without this, a flag that does not exist can be declared honoured and
/// nothing contradicts it — a guess recorded as fact, which is the state this
/// module replaces. Help text is captured from the binary like any other
/// evidence, so the check is against the tool rather than against belief.
#[test]
fn every_recorded_flag_exists_on_the_tool() {
    for subcommand in &ocr_contract().subcommands {
        let help = read_fixture(&help_capture_name(&subcommand.subcommand));
        // Matched as a whole whitespace-delimited token, not as a substring:
        // the help lists each flag as its own word, and a substring match
        // would accept --json on the strength of a future --jsonl.
        let help_tokens: Vec<&str> = help.split_whitespace().collect();
        for flag in subcommand.flags.keys() {
            assert!(
                help_tokens.contains(&flag.as_str()),
                "{} records {flag}, which does not appear as a flag in the tool's help for that \
                 subcommand; capture the flag against the real tool before recording it",
                subcommand.subcommand
            );
        }
    }
}

/// The version capture must look like the tool's version output.
#[test]
fn the_version_capture_identifies_the_tool() {
    let captured = read_fixture("version.txt");
    let contract = ocr_contract();

    assert!(
        captured.contains(&contract.tool),
        "the version capture should name {}; got: {}",
        contract.tool,
        captured.trim()
    );
    assert!(
        captured.contains("built at:"),
        "the version capture should carry the build metadata the tool emits; got: {}",
        captured.trim()
    );
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
    // Larger than any plausible internal buffer limit. The whole integrity
    // scheme rests on this function, and a hash that silently ignored input
    // past some threshold would make appended fabrication invisible in
    // exactly the large captures where it is easiest to hide. Session logs
    // run to megabytes, so this is a live case, not a theoretical one.
    assert_eq!(
        sha256_hex(&b"a".repeat(2_000_000)),
        "bcf7f9d1b4311c3352e60502255ce09a6744df84e8f2c89f79c4b5d74933a95a"
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
        // The realistic failure: the version command produced nothing,
        // because the binary was missing or crashed. Failing closed matters
        // most exactly here.
        "",
        "   \n",
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
