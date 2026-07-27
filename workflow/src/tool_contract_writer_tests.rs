//! Contract tests for the subcommand that writes the session store.
//!
//! Split from `tool_contract_tests.rs` when that file reached its size
//! limit. The seam is deliberate rather than arbitrary: everything here
//! concerns the writer and the evidence binding its recorded behaviour to a
//! capture, while the original file covers the two readers.

use super::tool_contract_tests::{ocr_contract, read_fixture};
use super::*;

/// `review` keys its session store on the Git root, not on the directory it
/// was invoked from.
///
/// Measured against 1.7.16: invoked from `/tmp/gr179/sub/deep`, the store
/// appeared under the slug for `/tmp/gr179`. Control: the same binary outside
/// a repository created no store and did not walk up past it.
///
/// This is the writer half of the disagreement in issue 248 - the readers key
/// on the working directory - so recording it wrongly would make the contract
/// agree with the bug.
#[test]
fn review_keys_its_store_on_the_git_root() {
    let contract = ocr_contract();
    let review = contract
        .subcommand("review")
        .expect("review is recorded: it is the subcommand that writes the store");
    assert_eq!(
        review.state_key,
        StateKey::GitRoot,
        "review keys on the Git root; recording the working directory here would describe the \
         readers' behaviour and contradict what was measured"
    );
    assert_ne!(
        review.state_key,
        contract
            .subcommand("session list")
            .expect("session list is recorded")
            .state_key,
        "the writer and the readers must disagree: that disagreement is the defect in 248, and \
         a contract that hides it cannot explain the failure"
    );
}

/// `review`'s reviewable result lives in the session jsonl, not on stdout.
///
/// Issue 195: stdout carries progress, and a caller parsing it sees a run that
/// reviewed nothing rather than a run whose result is elsewhere.
#[test]
fn reviews_result_is_a_durable_artifact_naming_the_session_jsonl() {
    let contract = ocr_contract();
    let review = contract.subcommand("review").expect("review is recorded");
    match &review.result_source {
        ResultSource::DurableArtifact { description } => {
            assert!(
                description.contains("session jsonl"),
                "the artifact description must name the session jsonl so a caller knows what to \
                 read; got: {description}"
            );
        }
        other => panic!(
            "review's result is a durable artifact, not {other:?}; parsing stdout is what made \
             a completed review look empty"
        ),
    }
}

/// Every required field must actually appear in the subcommand's captures.
///
/// `every_subcommand_records_required_fields` checks only that the list is
/// non-empty, so a name could be recorded that the tool never emits - which is
/// the shape of issue 174, where a parser expected "Excluded (2):" and the
/// tool printed "Excluded from review (2):". Checking presence against the
/// capture is what makes the recorded name evidence rather than a claim.
///
/// A plain substring test is not enough, and neither is a delimiter test.
/// Shortening "Excluded from review" to "Excluded" leaves a match followed by
/// a space, so "must be followed by a non-word character" accepts the
/// truncation - measured, not assumed. That truncation is the 174 failure
/// exactly: a parser matching a shortened header and reading the wrong
/// section.
///
/// The name must therefore span its whole line up to the first delimiter that
/// ends it. A JSON key ends at its closing quote; a text header ends at the
/// count or colon that follows. Anything shorter than that span is a
/// truncation and must fail.
#[test]
fn every_required_field_appears_in_a_capture() {
    for subcommand in &ocr_contract().subcommands {
        let captured: String = subcommand
            .captures
            .iter()
            .map(|capture| read_fixture(&capture.file))
            .collect();
        for field in &subcommand.required_fields {
            // Terminators that legitimately end a name where it is used:
            // a JSON key's closing quote, and the `(` or `:` that follows a
            // preview section header. A space does not end a name, which is
            // what makes "Excluded" fail against "Excluded from review (1):".
            let ends_with_punctuation = field
                .name
                .chars()
                .last()
                .is_some_and(|last| !last.is_alphanumeric());
            let appears_whole = captured.match_indices(&field.name).any(|(at, matched)| {
                let rest = &captured[at + matched.len()..];
                // A name that already carries its own terminator, such as
                // "Session:", is complete where it stands.
                if ends_with_punctuation {
                    return true;
                }
                // Otherwise the name must be followed by what ends it: a
                // JSON key's closing quote, or - allowing the single space the
                // tool prints - the count or colon closing a section header.
                // A word character means the record is a truncation.
                match rest.chars().next() {
                    None | Some('"') | Some(':') | Some('\n') => true,
                    Some(' ') => matches!(rest[1..].chars().next(), Some('(') | Some(':')),
                    _ => false,
                }
            });
            assert!(
                appears_whole,
                "{} records required field {:?}, but no capture contains it as a whole name; \
                 either the tool does not emit it, or the recorded name is a truncation of what \
                 it prints - both are the 174 failure",
                subcommand.subcommand, field.name
            );
        }
    }
}
