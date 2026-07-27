//! Contract tests for the two subcommands that read the session store.
//!
//! Split from `tool_contract_tests.rs` to keep each file within the size
//! limit. The seam follows the subject: the original file holds invariants
//! that must hold for every subcommand, while these tests record what one
//! specific subcommand was measured to do.

use super::tool_contract_tests::{
    capture_named, help_capture_name, ocr_contract, read_fixture, subcommand,
};
use super::*;

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
        without_repo, "null\n",
        "from an unrelated directory with no --repo, the tool should find nothing; the capture \
         is compared exactly because trimming would accept padded invention as the control"
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
        StateKey::PathArgumentAbsoluteCleaned { flag: "--repo" },
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

/// Every ignored flag must name an alternative that the contract does not
/// itself contradict.
///
/// Checking only that the string is non-empty guards its length, not its
/// truth: a remediation can name a flag the same contract records as ignored,
/// sending the caller straight back into the failure. The value of recording
/// an ignored flag is the remediation, so the remediation is what must hold.
#[test]
fn every_ignored_flag_names_an_alternative_the_contract_supports() {
    let contract = ocr_contract();

    for subcommand in &contract.subcommands {
        for (flag, behaviour) in &subcommand.flags {
            let FlagBehaviour::AcceptedAndIgnored { use_instead } = behaviour else {
                continue;
            };
            assert!(
                !use_instead.trim().is_empty(),
                "{} records {flag} as ignored without naming an alternative",
                subcommand.subcommand
            );

            // A remediation must not send the caller back to a flag this same
            // subcommand does not honour. Naming another subcommand's flag is
            // legitimate and common — session show's remedy is to use session
            // list --json — so the check applies only when the remediation
            // does not name a different subcommand to run.
            // Compared as a run of whole words rather than by containment, so
            // a subcommand named "list" is not matched by the word "listing"
            // in a remediation, which would skip the check silently.
            // Each flag is attributed to the nearest subcommand named before
            // it, so a remediation mentioning more than one - "use session
            // list --json or session show --help" - checks each flag against
            // the command it belongs to. Resolving one target for the whole
            // text would check the second subcommand's flags against the
            // first and fail for a reason that has nothing to do with the
            // contract.
            let words: Vec<&str> = use_instead.split_whitespace().collect();
            let subcommand_at = |index: usize| {
                contract.subcommands.iter().find(|other| {
                    let wanted: Vec<&str> = other.subcommand.split_whitespace().collect();
                    // Whole words, so a subcommand named "list" is not matched
                    // by "listing", which would silently misattribute a flag.
                    index + wanted.len() <= words.len()
                        && words[index..index + wanted.len()] == wanted[..]
                })
            };

            for (index, word) in words.iter().enumerate() {
                let Some(bare) = word.strip_prefix("--") else {
                    continue;
                };
                // The nearest subcommand named at or before this flag; falling
                // back to the subcommand being validated when none is named,
                // which is the "use --other-flag here" case.
                let target = (0..=index)
                    .rev()
                    .find_map(subcommand_at)
                    .unwrap_or(subcommand);
                let recommended = format!(
                    "--{}",
                    bare.trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
                );
                // Requiring Honoured rather than merely "not ignored" matters:
                // an unrecorded flag yields None, which would satisfy a
                // negated check and let a remedy recommend a flag the contract
                // never observed. Silence is not evidence of support.
                assert!(
                    matches!(target.flag(&recommended), Some(FlagBehaviour::Honoured)),
                    "{}'s remediation for {flag} recommends {recommended} on {}, which is not \
                     recorded there as honoured; a remedy may only point at behaviour the \
                     contract has actually captured",
                    subcommand.subcommand,
                    target.subcommand
                );
            }
        }
    }
}

/// Recorded justifications must actually say something.
///
/// `used_for` exists so a future maintainer can judge a rename; an empty one
/// records the field without recording why it matters, which is the state the
/// contract replaces.
#[test]
fn every_required_field_records_why_it_is_needed() {
    for subcommand in &ocr_contract().subcommands {
        for field in &subcommand.required_fields {
            assert!(
                !field.name.trim().is_empty(),
                "{} records an unnamed required field",
                subcommand.subcommand
            );
            assert!(
                field.used_for.split_whitespace().count() >= 3,
                "{} records {} without saying what needs it",
                subcommand.subcommand,
                field.name
            );
        }
    }
}

/// Both subcommands must record the fields their captures are checked against.
///
/// Guarding only one list lets the other be emptied, which silently disables
/// the loop that verifies it.
#[test]
fn every_subcommand_records_required_fields() {
    for subcommand in &ocr_contract().subcommands {
        assert!(
            !subcommand.required_fields.is_empty(),
            "{} records no required fields, so nothing constrains its capture's content",
            subcommand.subcommand
        );
    }
}

/// The contract must identify the tool its captures actually came from.
///
/// The expected name is a literal rather than a read of `contract.tool`:
/// comparing the contract against itself would accept any consistent rename,
/// and the point is to bind the contract to the binary the captures came from.
#[test]
fn the_contract_names_the_tool_its_captures_came_from() {
    let contract = ocr_contract();

    assert_eq!(
        contract.tool, "open-code-review",
        "this contract describes open-code-review; a contract for another tool belongs in its \
         own module with its own captures"
    );
    assert!(
        read_fixture("version.txt").contains(&contract.tool),
        "the contract claims to describe {}, which its own version capture does not name",
        contract.tool
    );
}

/// `--limit` is honoured, and the captures show it rather than assert it.
///
/// Recorded as ignored until the tool was actually run with it. The comparison
/// is between two captures of the same command, which is the only form of
/// evidence that distinguishes a flag being honoured from a flag being
/// accepted and discarded.
#[test]
fn the_limit_flag_restricts_the_listing() {
    let unrestricted = read_fixture("session-list--json.stdout");
    let restricted = read_fixture("session-list--json--limit-1.stdout");

    let count = |text: &str| {
        serde_json::from_str::<serde_json::Value>(text)
            .expect("the listing captures are JSON")
            .as_array()
            .expect("the listing is an array")
            .len()
    };

    let (all, one) = (count(&unrestricted), count(&restricted));
    assert!(
        all > one && one == 1,
        "--limit 1 should restrict the listing; captured {all} unrestricted and {one} restricted"
    );

    ocr_contract()
        .subcommand("session list")
        .expect("recorded")
        .require_honoured("--limit")
        .expect("--limit is honoured, as the captures show");
}

/// The architecture doc's mutation count must match the battery.
///
/// Two revisions of that document have now claimed a count that was wrong in
/// both directions, and the doc itself warns against exactly that. A stated
/// count nothing checks is a claim, so this reads both files and compares.
#[test]
fn the_documented_mutation_count_matches_the_battery() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let battery = std::fs::read_to_string(root.join("tests/tool_contract_mutation/mutate.py"))
        .expect("read the mutation battery");
    let doc = std::fs::read_to_string(root.join("docs/architecture/tool-contracts.md"))
        .expect("read the architecture doc");

    let defined = battery
        .lines()
        .filter(|line| line.trim_start().starts_with("(\"M"))
        .count();
    // Both the count and the row count are read from the mutation table's own
    // section, so they cannot end up anchored to different parts of the
    // document. An unscoped search would take the first "N fail" anywhere in
    // the file and then compare it against this table's rows, reporting a
    // mismatch that says nothing about the battery.
    let section: Vec<&str> = doc
        .lines()
        .skip_while(|line| !line.starts_with("## Verified mutations"))
        .collect();
    assert!(
        !section.is_empty(),
        "the doc should contain a Verified mutations section; if it was renamed, update this test \
         rather than removing the check"
    );

    let claimed = section
        .iter()
        .find_map(|line| {
            line.split_whitespace()
                .zip(line.split_whitespace().skip(1))
                .find(|(_, next)| *next == "fail")
                .and_then(|(count, _)| count.parse::<usize>().ok())
        })
        .expect("the Verified mutations section should state how many fail the suite");

    assert_eq!(
        claimed, defined,
        "the doc claims {claimed} mutations fail the suite but the battery defines {defined}"
    );

    // Scoped to the mutation table specifically: the document contains other
    // tables, and counting all of them would compare unrelated rows.
    let rows = section
        .iter()
        .skip_while(|line| !line.starts_with("| Mutation"))
        .skip(2)
        .take_while(|line| line.starts_with("| "))
        .count();
    assert_eq!(
        rows, defined,
        "the mutation table has {rows} rows for {defined} mutations; every mutation should be \
         listed so the table cannot drift from the battery"
    );
}

/// An empty remediation is reported as an incomplete contract entry, not as an
/// uncaptured flag.
///
/// The two need different fixes: one is edited here, the other is re-captured
/// from the tool. Reporting the first as the second sends a maintainer to run
/// the binary when the binary was never involved.
#[test]
fn an_empty_remediation_blames_the_contract_not_the_tool() {
    let ignored_with_no_alternative = SubcommandContract {
        subcommand: "session show".to_string(),
        state_key: StateKey::LogicalWorkingDirectory,
        result_source: ResultSource::Stdout,
        flags: std::iter::once((
            "--json".to_string(),
            FlagBehaviour::AcceptedAndIgnored {
                use_instead: "   ".to_string(),
            },
        ))
        .collect(),
        required_fields: vec![],
        captures: vec![],
    };

    let error = ignored_with_no_alternative
        .require_honoured("--json")
        .expect_err("a remediation naming nothing cannot be relied on");

    assert!(
        matches!(error, ContractViolation::MalformedRemediation { .. }),
        "expected the contract entry to be blamed, got: {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("incomplete") && !message.contains("no recorded behaviour"),
        "the diagnostic must point at the contract entry rather than at re-capturing: {message}"
    );
}

/// `duration_ns` is present only on sessions that finished.
///
/// The tool emits a sparse object: an unfinished session omits the field and
/// carries the zero time in `end_time`. A caller that reads it unconditionally
/// fails on precisely the sessions worth investigating, so the field must stay
/// out of the required set and the capture must keep holding an example.
#[test]
fn an_unfinished_session_omits_its_duration() {
    let sessions: Vec<serde_json::Value> =
        serde_json::from_str(&read_fixture("session-list--json.stdout"))
            .expect("session list emits JSON");

    let unfinished: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|session| {
            session
                .get("end_time")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|end| end.starts_with("0001-01-01"))
        })
        .collect();

    assert!(
        !unfinished.is_empty(),
        "the capture must retain an unfinished session, otherwise this asymmetry stops being \
         evidenced; re-capture only from a store that has one"
    );
    for session in &unfinished {
        assert!(
            session.get("duration_ns").is_none(),
            "an unfinished session is expected to omit duration_ns: {session}"
        );
    }

    assert!(
        !ocr_contract()
            .subcommand("session list")
            .expect("recorded")
            .required_fields
            .iter()
            .any(|field| field.name == "duration_ns"),
        "duration_ns must not be required, because the tool does not always emit it"
    );
}

/// Provenance must describe a capture that was actually run.
///
/// This is a weak guard by nature — prose cannot be fully verified — but it
/// catches provenance rewritten to assert the opposite of the truth, which is
/// the failure mode that matters.
#[test]
fn the_capture_provenance_claims_the_tool_was_run() {
    let provenance = ocr_contract().capture_provenance.to_lowercase();

    assert!(
        provenance.contains("running") || provenance.contains("ran"),
        "provenance must state that the tool was run: {provenance}"
    );
    for disclaimer in ["hand-written", "never run", "from memory", "issue tracker"] {
        assert!(
            !provenance.contains(disclaimer),
            "provenance claims the captures were not produced by running the tool: {provenance}"
        );
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
/// Invariant for future maintainers: the path below must point at the workflow
/// that installs the tool in CI. It is a fixed relative path on purpose. A
/// configurable or defaulted path would let this test pass while comparing
/// against nothing, and since no production code consumes the contract yet,
/// this assertion is the only thing that keeps the pinned version tied to the
/// version production actually runs. If the workflow moves or renames the
/// variable, re-point this path; do not relax the check.
///
/// Binding to the workflow rather than to a source comment is deliberate: a
/// comment can drift silently, and issue #186 is what happens when tool
/// knowledge lives only in prose. Upgrading the tool in CI now fails this
/// test until the contract is re-verified.
#[test]
fn the_pinned_version_matches_the_version_ci_installs() {
    let workflow = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.github/workflows/ocr-pr-review.yml");
    // Deliberately coupled to the workflow file: this assertion is the only
    // thing tying the contract to the version production actually installs,
    // so it must fail rather than skip when the file moves. Both failure
    // paths below say where to re-point it, because a maintainer who renames
    // the workflow should be told what to update, not left guessing.
    let source = std::fs::read_to_string(&workflow).unwrap_or_else(|error| {
        panic!(
            "cannot read {} ({error}); this test binds the contract to the version CI installs, \
             so if the review workflow moved, update this path rather than deleting the check",
            workflow.display()
        )
    });

    let declared = source
        .lines()
        .find_map(|line| line.trim().strip_prefix("OCR_VERSION:"))
        .map(|value| value.trim().trim_matches('"').to_string())
        .unwrap_or_else(|| {
            panic!(
                "{} no longer declares OCR_VERSION; if the version moved to another variable or \
                 file, re-point this assertion at it so the contract stays bound to what CI \
                 installs",
                workflow.display()
            )
        });

    let pinned = ocr_contract().version;
    assert_eq!(
        format!("v{declared}"),
        pinned,
        "the contract is pinned to {pinned} but CI installs {declared}; re-capture the contract \
         fixtures against the version CI runs"
    );
}

/// The `--repo` behaviour recorded for each subcommand must actually differ.
///
/// The retrospective originally generalised one subcommand's handling of
/// `--repo` to the whole tool, and that generalisation was the defect. If a
/// future edit made every subcommand agree, the per-subcommand structure would
/// still compile and its justification would quietly become false, so the
/// disagreement is asserted rather than merely described in prose.
#[test]
fn the_repo_flag_is_recorded_as_behaving_differently_per_subcommand() {
    let contract = ocr_contract();
    let show = contract
        .subcommand("session show")
        .expect("session show is recorded")
        .flag("--repo")
        .expect("session show records --repo");
    let list = contract
        .subcommand("session list")
        .expect("session list is recorded")
        .flag("--repo")
        .expect("session list records --repo");

    assert!(
        matches!(show, FlagBehaviour::AcceptedAndIgnored { .. }),
        "session show accepts --repo and ignores it, measured against the real binary"
    );
    assert_eq!(
        list,
        &FlagBehaviour::Honoured,
        "session list honours --repo, measured against the real binary"
    );
    assert_ne!(
        show, list,
        "the two subcommands must disagree: a contract keyed on the tool rather than on the \
         subcommand would have to pick one of these and be wrong about the other"
    );
}
