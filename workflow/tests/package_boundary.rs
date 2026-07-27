//! Mechanical enforcement of the engine/domain package boundary.
//!
//! B1's premise is that a boundary described in prose is not a boundary. The
//! archived attempt-1 postmortem records exactly this failure: the separation
//! was declared in documents and asserted in tests that never ran the check.
//! These tests read the real dependency graph and the real source, so they
//! fail when the boundary is crossed rather than when someone forgets to
//! update a document.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Names that carry domain meaning and must never appear in core.
///
/// Taken verbatim from the issue rather than invented here, so the list can be
/// checked against the specification instead of against this file's author.
const FORBIDDEN_IN_CORE: &[&str] = &[
    "github",
    "issue",
    "pull request",
    "coderabbit",
    "llxprt",
    "branch",
    "merge strategy",
    "remediation",
    "scope policy",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn core_src() -> PathBuf {
    workspace_root().join("crates/luther-engine-core/src")
}

/// The names a package declares as dependencies, read from `cargo metadata`.
///
/// `cargo metadata --format-version 1` is a versioned, documented contract;
/// `cargo tree` renders for humans and is free to change its layout. Matching
/// on rendered text also admits a false positive from any package whose name
/// merely contains the string being searched for.
///
/// Scanned rather than deserialised because adding a JSON dependency to the
/// workspace to support one test is a heavier change than this warrants. The
/// scan is bracket-matched, not stopped at the first `]`: a dependency object
/// carries its own arrays, so a naive scan truncates inside the first entry.
/// That is not hypothetical — the first version of this function reported
/// `luther-workflow` as depending only on `anyhow`, which would have made the
/// boundary assertion pass while reading almost nothing.
fn declared_dependencies_of(package: &str) -> Vec<String> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = String::from_utf8_lossy(&output.stdout);

    let package_key = format!("\"name\":\"{package}\"");
    let start = json
        .find(&package_key)
        .unwrap_or_else(|| panic!("package `{package}` absent from cargo metadata"));
    let deps_start = json[start..]
        .find("\"dependencies\":[")
        .map(|offset| start + offset)
        .expect("every package object carries a dependencies array");

    let array_open = deps_start + "\"dependencies\":".len();
    let mut depth = 0usize;
    let mut deps_end = None;
    for (offset, character) in json[array_open..].char_indices() {
        match character {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    deps_end = Some(array_open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let deps_end = deps_end.expect("the dependencies array is terminated");
    let deps_block = &json[array_open..deps_end];

    deps_block
        .match_indices("\"name\":\"")
        .map(|(index, needle)| {
            let value_start = index + needle.len();
            let value_end = deps_block[value_start..]
                .find('"')
                .map(|offset| value_start + offset)
                .expect("a dependency name is a terminated string");
            deps_block[value_start..value_end].to_string()
        })
        .collect()
}

/// `cargo tree` for core must show no edge to any workspace domain package.
///
/// Asserted against the resolved graph rather than against the manifest: a
/// manifest says what was written, the graph says what Cargo actually links,
/// and a transitive edge would appear only in the latter.
#[test]
fn core_has_no_dependency_on_any_domain_package() {
    let deps = declared_dependencies_of("luther-engine-core");
    for domain in ["luther-workflow", "xtask"] {
        assert!(
            !deps.iter().any(|dep| dep == domain),
            "luther-engine-core depends on the domain package `{domain}`, which inverts the \
             allowed direction. The DAG in docs/architecture/package-boundaries.md permits \
             core <- domain only.\ndeclared dependencies: {deps:?}"
        );
    }
}

/// The domain package must depend on core, not the other way around.
///
/// Without this the previous test could be satisfied by two packages that
/// simply never referenced each other, which would pass while proving nothing.
#[test]
fn the_domain_package_does_depend_on_core() {
    let deps = declared_dependencies_of("luther-workflow");
    assert!(
        deps.iter().any(|dep| dep == "luther-engine-core"),
        "luther-workflow must depend on luther-engine-core; if it does not, the boundary test \
         above is vacuous because the two packages are simply unrelated.\n\
         declared dependencies: {deps:?}"
    );
}

/// The metadata scan finds dependencies that are known to exist.
///
/// Without this, a change to `cargo metadata`'s shape would make the scanner
/// return an empty list, and every dependency assertion above would pass by
/// finding no forbidden name in nothing at all. This is the guard against the
/// boundary tests becoming vacuous together.
#[test]
fn the_metadata_scan_actually_finds_dependencies() {
    let core_deps = declared_dependencies_of("luther-engine-core");
    assert!(
        core_deps.iter().any(|dep| dep == "sha2"),
        "the scanner did not find sha2, which core certainly depends on, so it is reading the \
         wrong thing and the boundary assertions are worthless.\nfound: {core_deps:?}"
    );

    let domain_deps = declared_dependencies_of("luther-workflow");
    assert!(
        domain_deps.len() > 1,
        "the scanner found {} dependency for luther-workflow; the array was almost certainly \
         truncated inside the first entry.\nfound: {domain_deps:?}",
        domain_deps.len()
    );
}

/// No domain vocabulary may appear in core's source, including in comments.
///
/// Comments are checked deliberately. A core primitive whose documentation
/// explains itself in terms of pull requests has domain knowledge baked into
/// its rationale even when its types do not, and that is how the concept
/// re-enters: the next maintainer reads the comment and writes to it.
#[test]
fn core_source_contains_no_domain_vocabulary() {
    let mut findings = Vec::new();
    for entry in std::fs::read_dir(core_src()).expect("core src is readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("core source is readable");
        let lowered = text.to_lowercase();
        for forbidden in FORBIDDEN_IN_CORE {
            if lowered.contains(forbidden) {
                findings.push(format!("{}: {forbidden}", path.display()));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "domain vocabulary found in luther-engine-core: {findings:?}. Core must be expressible \
         without reference to what it is orchestrating."
    );
}

/// Core must build on its own, with the domain package absent from the build.
///
/// `cargo check -p` still resolves the whole workspace, so this additionally
/// asserts core's own manifest names no workspace member. That is the property
/// the acceptance criterion is really asking about: core is buildable in
/// isolation, not merely buildable alongside.
#[test]
fn core_manifest_names_no_workspace_member() {
    let manifest =
        std::fs::read_to_string(workspace_root().join("crates/luther-engine-core/Cargo.toml"))
            .expect("core manifest is readable");
    for member in ["luther-workflow", "xtask", "path = \"../"] {
        assert!(
            !manifest.contains(member),
            "core's manifest references `{member}`; core must not depend on anything in this \
             workspace.\n{manifest}"
        );
    }
}
