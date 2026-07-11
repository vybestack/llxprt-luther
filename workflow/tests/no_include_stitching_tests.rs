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
            for (line_no, snippet) in scan_include_rs_violations(&content) {
                violations.push((relative(&path, root), line_no, snippet));
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

/// A minimal lexical token stream for the include-stitching scanner. Mirrors
/// the xtask enforcement helper: trivia (whitespace, line/block comments) is
/// dropped so that adjacent tokens correspond to the next meaningful source
/// token across newlines, while string literals retain raw content.
#[derive(Debug)]
enum Tok {
    Ident(String, usize),
    Bang,
    Open(char),
    Close,
    Str(String),
    Other,
}

/// Scan an entire Rust source file for `include!(...".rs"...)` macro
/// invocations, ignoring comments and string mentions and detecting multiline
/// invocations across `()`, `[]`, and `{}` delimiters. `include_str!` and
/// `include_bytes!` are intentionally not matched. Mirrors the xtask helper.
fn scan_include_rs_violations(content: &str) -> Vec<(usize, String)> {
    let tokens = lex_rust(content);
    let lines: Vec<&str> = content.lines().collect();
    let mut violations = Vec::new();
    let mut idx = 0;
    while idx < tokens.len() {
        let include_line = match &tokens[idx] {
            Tok::Ident(name, line) if name == "include" => Some(*line),
            _ => None,
        };
        if let Some(line) = include_line {
            if matches!(tokens.get(idx + 1), Some(Tok::Bang)) {
                if let Some(Tok::Open(open)) = tokens.get(idx + 2) {
                    if let Some((has_rs, end_idx)) = include_group_scan(&tokens, idx + 2, *open) {
                        if has_rs {
                            let snippet = lines
                                .get(line.saturating_sub(1))
                                .map(|source_line| source_line.trim().to_string())
                                .filter(|source_line| !source_line.is_empty())
                                .unwrap_or_else(|| "include!(...)".to_string());
                            violations.push((line, snippet));
                        }
                        idx = end_idx + 1;
                        continue;
                    }
                }
            }
        }
        idx += 1;
    }
    violations
}

fn include_group_scan(tokens: &[Tok], open_idx: usize, open: char) -> Option<(bool, usize)> {
    if !matches!(open, '(' | '[' | '{') {
        return None;
    }
    let mut depth = 0usize;
    let mut found = false;
    let mut idx = open_idx;
    while idx < tokens.len() {
        match &tokens[idx] {
            Tok::Open(_) => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    return Some((found, idx));
                }
            }
            Tok::Str(value) if depth >= 1 && value.ends_with(".rs") => {
                found = true;
            }
            _ => {}
        }
        idx += 1;
    }
    None
}

fn lex_rust(content: &str) -> Vec<Tok> {
    let bytes = content.as_bytes();
    let mut tokens = Vec::new();
    let mut idx = 0;
    let mut line = 1usize;
    while idx < bytes.len() {
        let byte = bytes[idx];
        match byte {
            b'\n' => {
                line += 1;
                idx += 1;
            }
            b' ' | b'\t' | b'\r' => idx += 1,
            b'/' if bytes.get(idx + 1) == Some(&b'/') => {
                idx += 2;
                while idx < bytes.len() && bytes[idx] != b'\n' {
                    idx += 1;
                }
            }
            b'/' if bytes.get(idx + 1) == Some(&b'*') => {
                idx += 2;
                let mut depth = 1usize;
                while idx < bytes.len() && depth > 0 {
                    match bytes[idx] {
                        b'\n' => {
                            line += 1;
                            idx += 1;
                        }
                        b'/' if bytes.get(idx + 1) == Some(&b'*') => {
                            depth += 1;
                            idx += 2;
                        }
                        b'*' if bytes.get(idx + 1) == Some(&b'/') => {
                            depth -= 1;
                            idx += 2;
                        }
                        _ => idx += 1,
                    }
                }
            }
            b'r' if is_raw_string_start(bytes, idx) => {
                let (value, next, next_line) = lex_raw_string(bytes, idx, line);
                tokens.push(Tok::Str(value));
                idx = next;
                line = next_line;
            }
            b'b' if bytes.get(idx + 1) == Some(&b'r') && is_raw_string_start(bytes, idx + 1) => {
                let (value, next, next_line) = lex_raw_string(bytes, idx + 1, line);
                tokens.push(Tok::Str(value));
                idx = next;
                line = next_line;
            }
            b'b' if bytes.get(idx + 1) == Some(&b'"') => {
                let (value, next, next_line) = lex_normal_string(bytes, idx + 1, line);
                tokens.push(Tok::Str(value));
                idx = next;
                line = next_line;
            }
            b'"' => {
                let (value, next, next_line) = lex_normal_string(bytes, idx, line);
                tokens.push(Tok::Str(value));
                idx = next;
                line = next_line;
            }
            b'\'' => {
                let (next, next_line) = lex_char_or_lifetime(bytes, idx, line);
                tokens.push(Tok::Other);
                idx = next;
                line = next_line;
            }
            b'!' => {
                tokens.push(Tok::Bang);
                idx += 1;
            }
            b'(' | b'[' | b'{' => {
                tokens.push(Tok::Open(byte as char));
                idx += 1;
            }
            b')' | b']' | b'}' => {
                tokens.push(Tok::Close);
                idx += 1;
            }
            _ if is_ident_start(byte) => {
                let start = idx;
                idx += 1;
                while idx < bytes.len() && is_ident_continue(bytes[idx]) {
                    idx += 1;
                }
                let text = String::from_utf8_lossy(&bytes[start..idx]).into_owned();
                tokens.push(Tok::Ident(text, line));
            }
            _ => {
                tokens.push(Tok::Other);
                idx += 1;
            }
        }
    }
    tokens
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic() || byte >= 0x80
}

fn is_ident_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric() || byte >= 0x80
}

fn is_raw_string_start(bytes: &[u8], idx: usize) -> bool {
    if bytes.get(idx) != Some(&b'r') {
        return false;
    }
    let mut cursor = idx + 1;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    bytes.get(cursor) == Some(&b'"')
}

/// Lex a raw string literal starting at `bytes[idx] == b'r'`. Returns the raw
/// content, the index just past the closing delimiter, and the updated line.
fn lex_raw_string(bytes: &[u8], idx: usize, line: usize) -> (String, usize, usize) {
    let mut cursor = idx + 1;
    let mut hashes = 0usize;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    cursor += 1; // opening quote
    let start = cursor;
    let mut current_line = line;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => {
                let mut ahead = cursor + 1;
                let mut count = 0usize;
                while count < hashes && bytes.get(ahead) == Some(&b'#') {
                    count += 1;
                    ahead += 1;
                }
                if count == hashes {
                    let value = String::from_utf8_lossy(&bytes[start..cursor]).into_owned();
                    return (value, ahead, current_line);
                }
                cursor += 1;
            }
            b'\n' => {
                current_line += 1;
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    let value = String::from_utf8_lossy(&bytes[start..cursor]).into_owned();
    (value, cursor, current_line)
}

/// Lex a normal (or byte) string literal starting at `bytes[idx] == b'"'`.
fn lex_normal_string(bytes: &[u8], idx: usize, line: usize) -> (String, usize, usize) {
    let mut cursor = idx + 1;
    let start = cursor;
    let mut current_line = line;
    let mut escaped = false;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if escaped {
            escaped = false;
            if byte == b'\n' {
                current_line += 1;
            }
            cursor += 1;
        } else if byte == b'\\' {
            escaped = true;
            cursor += 1;
        } else if byte == b'"' {
            let value = String::from_utf8_lossy(&bytes[start..cursor]).into_owned();
            return (value, cursor + 1, current_line);
        } else {
            if byte == b'\n' {
                current_line += 1;
            }
            cursor += 1;
        }
    }
    let value = String::from_utf8_lossy(&bytes[start..cursor]).into_owned();
    (value, cursor, current_line)
}

/// Consume a char literal or a lifetime/label starting at `bytes[idx] == b'\''`.
/// Char literals are fully consumed so that a `"` inside (for example `'"'`)
/// never toggles string state; lifetimes consume only the leading quote.
fn lex_char_or_lifetime(bytes: &[u8], idx: usize, line: usize) -> (usize, usize) {
    let mut current_line = line;
    let cursor = idx + 1;
    if bytes.get(cursor) == Some(&b'\\') {
        let mut scan = cursor + 2;
        while scan < bytes.len() && bytes[scan] != b'\'' {
            if bytes[scan] == b'\n' {
                current_line += 1;
            }
            scan += 1;
        }
        if scan < bytes.len() {
            scan += 1;
        }
        return (scan, current_line);
    }
    if bytes.get(cursor + 1) == Some(&b'\'') {
        if bytes.get(cursor) == Some(&b'\n') {
            current_line += 1;
        }
        return (cursor + 2, current_line);
    }
    (idx + 1, current_line)
}

#[cfg(test)]
mod scanner_tests {
    use super::scan_include_rs_violations;

    fn scan_lines(content: &str) -> Vec<usize> {
        scan_include_rs_violations(content)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    #[test]
    fn detects_single_and_multiline_include_rs() {
        assert_eq!(scan_lines(r#"include!("part_1.rs");"#), vec![1]);
        assert_eq!(
            scan_lines(
                r#"include!
(
    "generated/tail.rs"
);"#
            ),
            vec![1]
        );
    }

    #[test]
    fn detects_bracket_brace_and_raw_string_targets() {
        assert_eq!(scan_lines(r#"include!["a.rs"];"#), vec![1]);
        assert_eq!(scan_lines(r#"include!{"b.rs"}"#), vec![1]);
        assert_eq!(scan_lines(r##"include!(r#"weird/name.rs"#);"##), vec![1]);
    }

    #[test]
    fn ignores_include_str_bytes_and_non_rs_targets() {
        assert!(scan_lines(r#"include_str!("template.rs");"#).is_empty());
        assert!(scan_lines(r#"include_bytes!("blob.rs");"#).is_empty());
        assert!(scan_lines(r#"include!("data.txt");"#).is_empty());
    }

    #[test]
    fn ignores_comment_and_string_mentions() {
        assert!(scan_lines(r#"// include!("part_1.rs")"#).is_empty());
        assert!(scan_lines(
            r#"/*
 include!(
 "part_1.rs"
 );
*/"#
        )
        .is_empty());
        assert!(scan_lines(r#"let s = "include!(\"part_1.rs\")";"#).is_empty());
        assert!(scan_lines(r##"let s = r#"include!("part_1.rs")"#;"##).is_empty());
    }

    #[test]
    fn char_quote_does_not_break_scanning_and_reports_keyword_line() {
        assert_eq!(
            scan_lines(r#"let q = '"'; include!("part_1.rs");"#),
            vec![1]
        );
        assert_eq!(
            scan_lines(
                r#"fn main() {}

include!(
    "x.rs"
);"#
            ),
            vec![3]
        );
    }
}
