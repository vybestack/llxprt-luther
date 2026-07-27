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

/// `cargo tree` for core must show no edge to any workspace domain package.
///
/// Asserted against the resolved graph rather than against the manifest: a
/// manifest says what was written, the graph says what Cargo actually links,
/// and a transitive edge would appear only in the latter.
#[test]
fn core_has_no_dependency_on_any_domain_package() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "-p", "luther-engine-core", "--prefix", "none"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo tree runs");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);

    for domain in ["luther-workflow", "xtask"] {
        assert!(
            !tree.contains(domain),
            "luther-engine-core depends on the domain package `{domain}`, which inverts the \
             allowed direction. The DAG in docs/architecture/package-boundaries.md permits \
             core <- domain only.\n{tree}"
        );
    }
}

/// The domain package must depend on core, not the other way around.
///
/// Without this the previous test could be satisfied by two packages that
/// simply never referenced each other, which would pass while proving nothing.
#[test]
fn the_domain_package_does_depend_on_core() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "-p", "luther-workflow", "--prefix", "none"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo tree runs");
    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(
        tree.contains("luther-engine-core"),
        "luther-workflow must depend on luther-engine-core; if it does not, the boundary test \
         above is vacuous because the two packages are simply unrelated.\n{tree}"
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
