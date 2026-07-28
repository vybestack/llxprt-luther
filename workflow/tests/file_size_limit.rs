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
const ACCEPTED_BREACHES: [&str; 1] = ["src/components/github/pr_remediation.rs"];

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
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
