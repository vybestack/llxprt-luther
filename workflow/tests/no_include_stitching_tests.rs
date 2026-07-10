//! Regression guard for issue #125: assert that no `include!()`-based source
//! stitching and no numbered/tail split-file names remain under `workflow/src`.
//!
//! The xtask `guard`/`complexity` commands enforce the same rules, but this
//! test keeps enforcement visible inside the standard `cargo test` run too.

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn no_include_stitching_in_src() {
    let src_dir = src_dir();
    let mut violations = Vec::new();
    walk(&src_dir, &src_dir, &mut violations);
    assert!(
        violations.is_empty(),
        "found include!()/split-file source stitching in workflow/src:\n{}\n{}",
        violations
            .iter()
            .map(|(p, l, c)| format!("{}:{}: {}", p.display(), l, c.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
        "Source stitching with include!(\"*.rs\") is not allowed for Rust module \
         assembly. Split this code into semantic mod submodules with cohesive \
         responsibilities and narrow visibility. Do not use numbered part files, \
         tail files, or generic support buckets to satisfy file-size limits."
    );
}

fn src_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.join("src")
}

fn walk(dir: &Path, root: &Path, violations: &mut Vec<(PathBuf, usize, String)>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("read dir {}: {err}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_split_component_name(name) {
                violations.push((relative(&path, root), 1, format!("dir {name}")));
            }
            walk(&path, root, violations);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if is_split_source_file_name(file_name) {
            violations.push((relative(&path, root), 1, format!("file {file_name}")));
        }
        if let Ok(content) = fs::read_to_string(&path) {
            for (idx, line) in content.lines().enumerate() {
                if line_contains_include_rs(line) {
                    violations.push((relative(&path, root), idx + 1, line.to_string()));
                }
            }
        }
    }
}

fn relative(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn is_split_component_name(name: &str) -> bool {
    is_part_numbered_name(name) || is_core_numbered_name(name)
}

fn is_split_source_file_name(name: &str) -> bool {
    // Callers only invoke this for files that already end in `.rs`, so stripping
    // the suffix always yields a distinct stem; a `== name` equality guard would
    // be unreachable here. Mirrors the xtask enforcement helper.
    let stem = name.strip_suffix(".rs").unwrap_or(name);
    is_part_numbered_name(stem) || is_core_numbered_name(stem) || stem.ends_with("_tail")
}

fn is_part_numbered_name(stem: &str) -> bool {
    let Some(rest) = stem.strip_prefix("part_") else {
        return false;
    };
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    let mut seen_alpha = false;
    for ch in chars {
        if ch.is_ascii_digit() {
            if seen_alpha {
                return false;
            }
            continue;
        }
        if ch.is_ascii_lowercase() {
            if seen_alpha {
                return false;
            }
            seen_alpha = true;
            continue;
        }
        return false;
    }
    true
}

fn is_core_numbered_name(stem: &str) -> bool {
    let Some(rest) = stem.strip_prefix("core_") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

fn line_contains_include_rs(line: &str) -> bool {
    // Mirror the xtask enforcement helper: only flag actual
    // `include!( ... ".rs" ... )` macro invocations. Ignore comments and
    // mentions inside string literals so guidance text or doc comments that
    // merely reference the macro name do not trip the check.
    let code = strip_line_comment(line);
    let Some(idx) = code.find("include!") else {
        return false;
    };
    let after = code[idx + "include!".len()..].trim_start();
    // A real macro invocation places an opening delimiter after the name.
    if !after.starts_with('(') && !after.starts_with('[') && !after.starts_with('{') {
        return false;
    }
    after.contains(".rs\"")
}

/// Remove a trailing `//` line comment, ignoring `//` sequences that appear
/// inside a double-quoted string literal. Mirrors the xtask helper.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'/' && idx + 1 < bytes.len() && bytes[idx + 1] == b'/' {
            return &line[..idx];
        }
        idx += 1;
    }
    line
}
