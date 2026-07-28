//! No source file exceeds the hard line limit.
//!
//! `cargo xtask complexity --changed` only inspects files touched by the diff,
//! so a file that grows past the limit in one PR and is untouched afterwards
//! stays over it indefinitely without any gate objecting. Both files found by
//! the audit for this issue were latent in exactly that way.
//!
//! This checks every file on every run, so a breach surfaces when it happens
//! rather than when someone next edits the file.

use std::path::{Path, PathBuf};

/// Matches the hard limit enforced by `cargo xtask complexity`.
const HARD_LIMIT: usize = 1000;

/// Known breaches that predate this gate.
///
/// `pr_remediation.rs` is 3025 lines of implementation - its tests are already
/// in separate modules - so reducing it is a genuine refactor rather than a
/// move, and doing it inside a file-size change would bury real behavioural
/// risk in a diff that is supposed to be mechanical. Tracked in #275.
///
/// Nothing may be added here without shrinking the list elsewhere: an
/// allowlist that only grows is not a gate.
const ACCEPTED_BREACHES: [&str; 13] = [
    "src/components/github/pr_remediation.rs",
    // The `tests/` tree. `cargo xtask complexity` does enforce the limit here
    // on a full scan, but CI only ever runs `--changed`, and that path fails a
    // file only when it GREW - so every one of these is grandfathered and has
    // never been enforced. Recording them is what makes that visible.
    "tests/github_pr_followup_executor_tests.rs",
    "tests/e2e_workflow_integration.rs",
    "tests/recovery_protocol_integration_tests.rs",
    "tests/quality_release_guardrails.rs",
    "tests/pr_followup_replay_e2e_tests.rs",
    "tests/typed_merge_integration_tests.rs",
    "tests/engine_integration_llxprt_first.rs",
    "tests/recovery_failpoint_matrix_tests.rs",
    "tests/canary_harness_tests.rs",
    "tests/verify_executor_tests.rs",
    "tests/command_manifest_executor_tests.rs",
    "tests/capsule_wiring_integration_tests.rs",
];

/// Collect `.rs` files under `dir`.
///
/// Fails closed: an unreadable directory or entry panics rather than being
/// skipped, because silently omitting a subtree would let this gate pass while
/// covering less than it claims - the same failure mode it exists to catch.
///
/// Uses `symlink_metadata`, so a symlinked directory is not descended into.
/// `is_dir()` follows links, which would both double-count files reachable by
/// two paths and hang on a link loop.
fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let entry =
            entry.unwrap_or_else(|e| panic!("cannot read an entry in {}: {e}", dir.display()));
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", path.display()));
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_source_file_exceeds_the_hard_line_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_sources(&root.join("src"), &mut files);
    rust_sources(&root.join("crates"), &mut files);
    // Including tests/ means this file is subject to its own gate.
    rust_sources(&root.join("tests"), &mut files);

    assert!(
        !files.is_empty(),
        "no sources were scanned; this test would pass vacuously"
    );

    let mut breaches = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if ACCEPTED_BREACHES.contains(&relative.as_str()) {
            continue;
        }
        let lines = std::fs::read_to_string(path)
            .expect("a readable source file")
            .lines()
            .count();
        if lines > HARD_LIMIT {
            breaches.push(format!("{relative}: {lines} lines"));
        }
    }

    assert!(
        breaches.is_empty(),
        "these files exceed the {HARD_LIMIT}-line hard limit:\n  {}\n\
         Split them, or the CI complexity gate will reject the next PR that touches them.",
        breaches.join("\n  ")
    );
}

/// Every accepted breach is still a breach.
///
/// Without this, a file could be split below the limit and left on the
/// allowlist, where it would silently permit regrowth.
#[test]
fn accepted_breaches_are_still_over_the_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // The allowlist may shrink but never grow. Without this, a file that
    // crossed the limit could be waved through by appending a line here, which
    // is exactly the bypass this gate exists to prevent - and the length in the
    // type above would be edited in the same motion, silently.
    //
    // Lowering this number as files are split is the intended direction, and
    // the assertion below then forces the stale entry out.
    const MAX_ACCEPTED_BREACHES: usize = 13;
    assert!(
        ACCEPTED_BREACHES.len() <= MAX_ACCEPTED_BREACHES,
        "ACCEPTED_BREACHES grew to {}; the limit is {MAX_ACCEPTED_BREACHES}. \
         Split the file instead of allowlisting it.",
        ACCEPTED_BREACHES.len()
    );

    for relative in ACCEPTED_BREACHES {
        let lines = std::fs::read_to_string(root.join(relative))
            .expect("an accepted breach names a file that exists")
            .lines()
            .count();
        assert!(
            lines > HARD_LIMIT,
            "{relative} is now {lines} lines, within the limit - remove it from \
             ACCEPTED_BREACHES so it cannot grow back unnoticed"
        );
    }
}
