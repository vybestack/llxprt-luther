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
use std::sync::OnceLock;

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

/// Every `.rs` file at or below `root`.
///
/// `read_dir` lists only one level, so a scan built on it would silently skip
/// any module core grows in a subdirectory — and skipping files makes the
/// vocabulary assertion pass by finding nothing, which is the failure mode
/// this file exists to prevent. Core is a single `lib.rs` today; the point is
/// that it will not quietly stop being checked when that changes.
fn rust_sources_under(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("core src is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                sources.push(path);
            }
        }
    }
    sources
}

/// Whether `haystack` contains `needle` as a whole word.
///
/// A raw substring test would fire on ordinary English: "branch" matches
/// "branching" and "branchless", "issue" matches "reissue". A false positive
/// here is worse than a miss, because the response to a test that fails on
/// innocent prose is to weaken the list until it stops complaining, and a
/// weakened list is what lets the real domain vocabulary back in.
///
/// A boundary is any non-alphanumeric character, which includes `_`. That is
/// deliberate in this direction: it means `github_client` still matches
/// "github", so an identifier cannot smuggle the vocabulary past the check by
/// joining it to another word with an underscore. Multi-word entries such as
/// "pull request" are matched the same way on their outer edges.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    haystack.match_indices(needle).any(|(index, matched)| {
        let before_ok = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        let after = index + matched.len();
        let after_ok = after == bytes.len() || !bytes[after].is_ascii_alphanumeric();
        before_ok && after_ok
    })
}

/// A bracket inside a string value does not end the dependency array.
///
/// JSON does not require `[` or `]` to be escaped inside a string, so a
/// dependency whose feature list or path contains one is well-formed input
/// that a naive depth counter mis-parses. Measured against the naive version,
/// the array below closes early and yields only `weird`, losing `sha2` — and
/// a short list still passes every "no forbidden name" assertion, so the
/// mis-parse would never announce itself.
#[test]
fn a_bracket_inside_a_string_does_not_close_the_dependency_array() {
    let crafted = concat!(
        r#"{"packages":[{"name":"pkg","version":"0.1.0","dependencies":["#,
        r#"{"name":"weird","features":["a]b"]},"#,
        r#"{"name":"sha2","features":[]}"#,
        r#"]}]}"#
    );
    assert_eq!(
        dependencies_in_metadata(crafted, "pkg"),
        vec!["weird".to_string(), "sha2".to_string()],
        "the scan stopped at a bracket inside a string value and truncated the list"
    );
}

/// The word check separates real vocabulary from innocent prose.
///
/// Without this, the boundary logic itself is untested, and a change that
/// silently reverted it to substring matching would look identical.
#[test]
fn the_vocabulary_check_matches_words_not_fragments() {
    assert!(contains_word("open a github client", "github"));
    assert!(
        contains_word("call github_client here", "github"),
        "an underscore must not hide the word, or `github_client` would pass"
    );
    assert!(!contains_word("branching factor of the tree", "branch"));
    assert!(!contains_word("reissue the token", "issue"));
    assert!(contains_word("the issue number", "issue"));
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
/// The workspace metadata, read once per test binary.
///
/// Each call previously spawned `cargo metadata` afresh. Caching keeps the
/// subprocess cost at one invocation and, more usefully, makes the parsing
/// below a pure function of a string — which is what allows it to be tested
/// against a crafted fixture rather than only against whatever this workspace
/// happens to contain today.
fn workspace_metadata() -> &'static str {
    static METADATA: OnceLock<String> = OnceLock::new();
    METADATA.get_or_init(|| {
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
        String::from_utf8(output.stdout).expect("cargo metadata emits UTF-8")
    })
}

fn declared_dependencies_of(package: &str) -> Vec<String> {
    dependencies_in_metadata(workspace_metadata(), package)
}

fn dependencies_in_metadata(json: &str, package: &str) -> Vec<String> {
    // Anchored on the package's `id`, not on the first `"name"` match.
    //
    // A bare name search is order-dependent: the same string appears inside
    // *other* packages' dependency lists, so it finds a package entry only
    // because `cargo metadata` happens to emit packages before the entries
    // that depend on them. Relying on that ordering would read the wrong
    // object the moment it changed, and the resulting truncated list would
    // still satisfy the "no forbidden name" assertions.
    let package_key = format!("\"name\":\"{package}\",\"version\"");
    let start = json
        .find(&package_key)
        .unwrap_or_else(|| panic!("package `{package}` absent from cargo metadata"));
    let deps_start = json[start..]
        .find("\"dependencies\":[")
        .map(|offset| start + offset)
        .expect("every package object carries a dependencies array");

    let array_open = deps_start + "\"dependencies\":".len();
    // Brackets inside string values are skipped. JSON does not require `[` or
    // `]` to be escaped inside a string, so a dependency carrying one in a
    // feature name or path would close the array early for a counter that
    // ignored strings — and a truncated dependency list satisfies every
    // "no forbidden name" assertion in this file rather than failing.
    let mut depth = 0usize;
    let mut deps_end = None;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in json[array_open..].char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
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

/// Core must declare no dependency on any workspace domain package.
///
/// This reads the *declared* dependencies (`cargo metadata --no-deps`), not
/// the resolved graph. Direct edges are what this boundary is about: a
/// transitive path from core to a domain package can only exist by way of a
/// direct edge from core, so forbidding the direct edge forbids the path.
///
/// Reading metadata rather than parsing `Cargo.toml` still matters — it is
/// Cargo's own view, so it covers dev- and build-dependencies (which appear
/// in the same array tagged by kind) and reports a renamed dependency under
/// its real package name rather than its local alias. Both verified by
/// control: adding either form of edge to core fails this test.
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
    let sources = rust_sources_under(&core_src());
    assert!(
        !sources.is_empty(),
        "the vocabulary scan found no source files at all, so it would pass whatever core \
         contained; check that {} still holds the crate's sources",
        core_src().display()
    );
    for path in sources {
        let text = std::fs::read_to_string(&path).expect("core source is readable");
        let lowered = text.to_lowercase();
        for forbidden in FORBIDDEN_IN_CORE {
            if contains_word(&lowered, forbidden) {
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

// --- the executor contract's error type -----------------------------------
//
// `EngineError` is returned by `StepExecutor::execute`, which every component
// implements. A variant naming a tool or a repository host makes it impossible
// to relocate any component into a domain-free package: the package would have
// to depend on a type that knows what LLxprt is. This is the check that stops
// that coupling from being reintroduced once it has been removed.

/// The error type in the executor signature names no specific tool or host.
#[test]
fn the_executor_error_type_names_no_domain_concept() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine/runner.rs"),
    )
    .expect("the runner source is readable");

    let body = source
        .split_once("pub enum EngineError {")
        .expect("EngineError must still be declared in runner.rs")
        .1;

    // Brace-matched, not first-`\n}`: a multi-line struct variant's inner
    // closing brace would otherwise truncate the scan and leave later
    // variants unexamined. The opening brace of the enum itself is consumed
    // by the split above, so this counter starts at 1 and walks until it
    // returns to 0 at the enum's true terminator.
    let mut depth: i32 = 1;
    let mut end = 0usize;
    for (i, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(end > 0, "the EngineError declaration is not brace-balanced");
    let body = &body[..end];

    // Only variant names are examined. Doc comments and `#[error(...)]`
    // strings legitimately mention tools: the message is written by whoever
    // raises the error, and telling an operator which binary is missing is
    // the point. It is the *type* that must stay domain-free.
    //
    // A variant line begins with an identifier whose first letter is
    // uppercase and ends its name at `{`, `(`, or end-of-line. This excludes
    // multi-line field lists (`step_id: String`) and stray closing braces,
    // which a naive per-line scan admitted as spurious variants.
    let variants: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| {
            if line.is_empty()
                || line.starts_with("//")
                || line.starts_with("#[")
                || line.starts_with("///")
            {
                return false;
            }
            let first = line.chars().next().unwrap_or(' ');
            first.is_ascii_uppercase()
        })
        .filter_map(|line| line.split(['{', '(']).next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();

    assert!(
        !variants.is_empty(),
        "no variants were parsed out of EngineError; the scan is looking at the wrong thing and \
         would pass no matter what the type contained"
    );

    // Tool and host names are matched anywhere in the variant: there is no
    // innocent use of "llxprt" or "github" inside an error name.
    let forbidden = ["llxprt", "github", "coderabbit", "pullrequest"];
    let offenders: Vec<&&str> = variants
        .iter()
        .filter(|name| {
            let lowered = name.to_lowercase();
            if forbidden.iter().any(|word| lowered.contains(word)) {
                return true;
            }
            // "issue" is matched only as a leading noun. `IssueNotFound` and
            // `IssueLeaseHeld` are the tracker's issue; `ConfigurationIssue`,
            // `IoIssue`, and `InternalIssue` are ordinary English for "a
            // problem", and flagging those would push future authors toward
            // worse names to satisfy a test rather than toward a boundary.
            // Position is what separates the two, so position is what is
            // checked.
            lowered.starts_with("issue")
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "EngineError variants name domain concepts: {offenders:?}. This type is returned by \
         every component, so a variant naming a tool blocks relocating any component into a \
         domain-free package. Carry the detail in a message formatted by the domain instead."
    );
}
