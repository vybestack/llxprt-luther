use super::{CheckResult, ErrorRecord};
use regex::Regex;

fn cap_error_message(message: &str) -> String {
    const MAX_ERROR_MESSAGE_BYTES: usize = 4_000;
    if message.len() <= MAX_ERROR_MESSAGE_BYTES {
        return message.to_string();
    }
    let end = message
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_ERROR_MESSAGE_BYTES)
        .last()
        .unwrap_or(0);
    format!(
        "{}\n\n[... verifier error message truncated ...]",
        &message[..end]
    )
}

/// Parse the output of a check and extract errors.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
pub(super) fn parse_check_output(
    check_type: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> Vec<ErrorRecord> {
    if exit_code == 0 {
        return vec![];
    }

    match check_type {
        "typecheck" => parse_typescript_errors(stdout, stderr),
        "test" => parse_test_results(stdout, stderr),
        "lint" => parse_lint_errors(stdout, stderr),
        "format" => parse_format_errors(stdout, stderr),
        "build" => parse_build_errors(stdout, stderr),
        "diff" => parse_diff_errors(stdout, stderr),
        _ => {
            // Unknown check type - wrap raw output in ErrorRecord
            let combined = format!("{stdout}{stderr}").trim().to_string();
            vec![ErrorRecord {
                file: None,
                line: None,
                column: None,
                message: if combined.is_empty() {
                    format!("Check failed with exit code {exit_code}")
                } else {
                    combined
                },
                severity: Some("error".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            }]
        }
    }
}

/// Parse TypeScript compiler errors from output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_typescript_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();
    let combined = format!("{stdout}{stderr}");

    // Regex pattern: file(line,col): error TSxxxx: message
    // Example: src/foo.ts(10,5): error TS2322: Type X is not assignable to Type Y
    let ts_regex = Regex::new(r"^(.+)\((\d+),(\d+)\): error (TS\d+): (.+)$").unwrap();

    for line in combined.lines() {
        if let Some(caps) = ts_regex.captures(line) {
            let file = caps
                .get(1)
                .map(|m: regex::Match<'_>| m.as_str().to_string());
            let line_num = caps
                .get(2)
                .and_then(|m: regex::Match<'_>| m.as_str().parse::<u32>().ok());
            let col_num = caps
                .get(3)
                .and_then(|m: regex::Match<'_>| m.as_str().parse::<u32>().ok());
            let error_code = caps
                .get(4)
                .map(|m: regex::Match<'_>| m.as_str().to_string());
            let message = caps
                .get(5)
                .map(|m: regex::Match<'_>| m.as_str().to_string());

            let full_message = if let Some(code) = error_code {
                format!("{}: {}", code, message.unwrap_or_default())
            } else {
                message.unwrap_or_default()
            };

            errors.push(ErrorRecord {
                file,
                line: line_num,
                column: col_num,
                message: full_message,
                severity: Some("error".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            });
        }
    }

    // Fallback: if no errors parsed but there was output, wrap raw output
    if errors.is_empty() && !combined.trim().is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: combined.trim().to_string(),
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

/// Unescape a string that may have shell-escaped quotes.
/// Converts \\\" back to " for JSON parsing.
fn unescape_shell_json(s: &str) -> String {
    s.replace("\\\"", "\"")
}

/// Escape helper: converts escaped JSON from test commands
/// Parse test results from test runner output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-006
fn parse_test_results(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();

    // Try JSON parse first (vitest --reporter=json)
    // Also try with unescaped quotes in case shell escaped them
    let json_result = serde_json::from_str::<serde_json::Value>(stdout)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&unescape_shell_json(stdout)));

    if let Ok(json) = json_result {
        if let Some(test_results) = json.get("testResults").and_then(|v| v.as_array()) {
            for test_file in test_results {
                let file_path = test_file
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if let Some(assertion_results) =
                    test_file.get("assertionResults").and_then(|v| v.as_array())
                {
                    for test in assertion_results {
                        if let Some(status) = test.get("status").and_then(|v| v.as_str()) {
                            if status == "failed" {
                                let test_name = test
                                    .get("fullName")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);

                                let message = test
                                    .get("failureMessages")
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str())
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    })
                                    .unwrap_or_default();

                                errors.push(ErrorRecord {
                                    file: file_path.clone(),
                                    line: None,
                                    column: None,
                                    message,
                                    severity: Some("error".to_string()),
                                    test_name,
                                    assertion_kind: Some("assertion".to_string()),
                                    expected: None,
                                    actual: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        if !errors.is_empty() {
            return errors;
        }
    }

    // Fallback: just return raw output as a single error
    let combined = format!("{stdout}{stderr}").trim().to_string();
    if !combined.is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: combined,
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

/// Parse lint errors from linter output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_lint_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();

    // Try JSON parse (eslint --format json)
    // Also try with unescaped quotes in case shell escaped them
    let json_result = serde_json::from_str::<serde_json::Value>(stdout)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&unescape_shell_json(stdout)));

    if let Ok(json_array) = json_result {
        if let Some(results) = json_array.as_array() {
            for file_result in results {
                let file_path = file_result
                    .get("filePath")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if let Some(messages) = file_result.get("messages").and_then(|v| v.as_array()) {
                    for msg in messages {
                        let line = msg.get("line").and_then(|v| v.as_u64()).map(|v| v as u32);
                        let column = msg.get("column").and_then(|v| v.as_u64()).map(|v| v as u32);
                        let message = msg
                            .get("message")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .unwrap_or_default();
                        let severity = msg.get("severity").and_then(|v| v.as_u64()).map(|v| {
                            if v == 2 {
                                "error".to_string()
                            } else {
                                "warning".to_string()
                            }
                        });

                        errors.push(ErrorRecord {
                            file: file_path.clone(),
                            line,
                            column,
                            message,
                            severity,
                            test_name: None,
                            assertion_kind: None,
                            expected: None,
                            actual: None,
                        });
                    }
                }
            }

            if !errors.is_empty() {
                return errors;
            }
        }
    }

    let combined = format!("{stdout}{stderr}");
    let stylish_errors = parse_eslint_stylish_errors(&combined);
    if !stylish_errors.is_empty() {
        return stylish_errors;
    }

    let combined = combined.trim().to_string();
    if !combined.is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: cap_error_message(&combined),
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

fn parse_eslint_stylish_errors(output: &str) -> Vec<ErrorRecord> {
    let diagnostic_regex =
        Regex::new(r"^\s*(\d+):(\d+)\s+(error|warning)\s+(.+?)(?:\s{2,}([^\s].*?))?\s*$").unwrap();
    let mut current_file: Option<String> = None;
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('/') || trimmed.starts_with("./") || trimmed.starts_with("../") {
            current_file = Some(trimmed.to_string());
            continue;
        }

        let Some(caps) = diagnostic_regex.captures(line) else {
            continue;
        };
        let severity = caps
            .get(3)
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "error".to_string());
        if severity != "error" {
            continue;
        }

        errors.push(ErrorRecord {
            file: current_file.clone(),
            line: caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok()),
            column: caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok()),
            message: caps
                .get(4)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default(),
            severity: Some(severity),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}

/// Parse format errors from format check output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_format_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let mut errors = Vec::new();
    let combined = format!("{stdout}{stderr}");

    for line in combined.lines() {
        let trimmed = line.trim();

        // Prettier --check outputs unformatted filenames
        // Example lines: "[warn] src/foo.ts" or just "src/foo.ts"
        if trimmed.starts_with("[warn]") {
            let file_path = trimmed
                .strip_prefix("[warn]")
                .map(|s| s.trim())
                .unwrap_or(trimmed);
            if !file_path.is_empty() && file_path.contains('.') {
                errors.push(ErrorRecord {
                    file: Some(file_path.to_string()),
                    line: None,
                    column: None,
                    message: "File is not formatted".to_string(),
                    severity: Some("warning".to_string()),
                    test_name: None,
                    assertion_kind: None,
                    expected: None,
                    actual: None,
                });
            }
        } else if trimmed.ends_with(".ts")
            || trimmed.ends_with(".tsx")
            || trimmed.ends_with(".js")
            || trimmed.ends_with(".jsx")
            || trimmed.ends_with(".json")
            || trimmed.ends_with(".md")
            || trimmed.ends_with(".css")
            || trimmed.ends_with(".scss")
            || trimmed.ends_with(".html")
        {
            // Likely a file path from prettier output
            errors.push(ErrorRecord {
                file: Some(trimmed.to_string()),
                line: None,
                column: None,
                message: "File is not formatted".to_string(),
                severity: Some("warning".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            });
        }
    }

    // Fallback: if no errors parsed but there was output, wrap raw output
    if errors.is_empty() && !combined.trim().is_empty() {
        errors.push(ErrorRecord {
            file: None,
            line: None,
            column: None,
            message: combined.trim().to_string(),
            severity: Some("error".to_string()),
            test_name: None,
            assertion_kind: None,
            expected: None,
            actual: None,
        });
    }

    errors
}
fn parse_diff_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    let combined = format!("{stdout}{stderr}").trim().to_string();
    vec![ErrorRecord {
        file: None,
        line: None,
        column: None,
        message: if combined.is_empty() {
            "No repository changes were produced".to_string()
        } else {
            combined
        },
        severity: Some("error".to_string()),
        test_name: None,
        assertion_kind: None,
        expected: None,
        actual: None,
    }]
}

/// Parse build errors from build output.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-005
fn parse_build_errors(stdout: &str, stderr: &str) -> Vec<ErrorRecord> {
    // Try to extract TypeScript-style errors first
    let errors = parse_typescript_errors(stdout, stderr);

    // Fallback: if no errors parsed but there was output, wrap raw output
    if errors.is_empty() {
        let combined = format!("{stdout}{stderr}").trim().to_string();
        if !combined.is_empty() {
            return vec![ErrorRecord {
                file: None,
                line: None,
                column: None,
                message: combined,
                severity: Some("error".to_string()),
                test_name: None,
                assertion_kind: None,
                expected: None,
                actual: None,
            }];
        }
    }

    errors
}

/// Build a summary string from check results.
/// @plan:PLAN-20260408-LLXPRT-FIRST.P06
/// @plan:PLAN-20260408-LLXPRT-FIRST.P08
/// @requirement:REQ-LF-VERIFY-004
pub(super) fn build_summary(checks: &[CheckResult]) -> String {
    let mut parts = Vec::new();

    for check in checks {
        let status = if check.passed {
            "pass".to_string()
        } else {
            format!("{} errors", check.errors.len())
        };
        parts.push(format!("{}: {}", check.check_type, status));
    }

    if parts.is_empty() {
        "No checks ran".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(check_type: &str, passed: bool, errors: Vec<ErrorRecord>) -> CheckResult {
        CheckResult {
            check_type: check_type.to_string(),
            passed,
            exit_code: if passed { 0 } else { 1 },
            errors,
            raw_stdout: String::new(),
            raw_stderr: String::new(),
            command: None,
        }
    }

    #[test]
    fn parse_check_output_returns_empty_on_success() {
        assert!(parse_check_output("test", "anything", "anything", 0).is_empty());
        assert!(parse_check_output("build", "", "", 0).is_empty());
    }

    #[test]
    fn parse_check_output_unknown_type_wraps_output() {
        let records = parse_check_output("mystery", "out", "err", 3);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "outerr");
        assert_eq!(records[0].severity.as_deref(), Some("error"));
    }

    #[test]
    fn parse_check_output_unknown_type_empty_output_uses_exit_code() {
        let records = parse_check_output("mystery", "", "", 7);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "Check failed with exit code 7");
    }

    #[test]
    fn typescript_errors_are_parsed_with_location() {
        let stdout = "src/foo.ts(10,5): error TS2322: Type X is not assignable to Type Y";
        let records = parse_check_output("typecheck", stdout, "", 1);
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.file.as_deref(), Some("src/foo.ts"));
        assert_eq!(rec.line, Some(10));
        assert_eq!(rec.column, Some(5));
        assert!(rec.message.starts_with("TS2322:"));
        assert!(rec.message.contains("not assignable"));
    }

    #[test]
    fn typescript_multiple_errors_parsed() {
        let stdout = "a.ts(1,1): error TS1: bad\nb.ts(2,3): error TS2: worse\nnoise line";
        let records = parse_check_output("typecheck", stdout, "", 1);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].file.as_deref(), Some("a.ts"));
        assert_eq!(records[1].file.as_deref(), Some("b.ts"));
    }

    #[test]
    fn typescript_fallback_wraps_unmatched_output() {
        let records = parse_check_output("typecheck", "some random compiler noise", "", 2);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "some random compiler noise");
        assert!(records[0].file.is_none());
    }

    #[test]
    fn test_results_json_extracts_failures() {
        let json = r#"{
            "testResults": [
                {
                    "name": "/repo/foo.test.ts",
                    "assertionResults": [
                        {"status": "passed", "fullName": "ok test"},
                        {"status": "failed", "fullName": "bad test", "failureMessages": ["expected 1 got 2"]}
                    ]
                }
            ]
        }"#;
        let records = parse_check_output("test", json, "", 1);
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.file.as_deref(), Some("/repo/foo.test.ts"));
        assert_eq!(rec.test_name.as_deref(), Some("bad test"));
        assert_eq!(rec.message, "expected 1 got 2");
        assert_eq!(rec.assertion_kind.as_deref(), Some("assertion"));
    }

    #[test]
    fn test_results_json_multiple_failure_messages_joined() {
        let json = r#"{"testResults":[{"name":"f","assertionResults":[{"status":"failed","fullName":"t","failureMessages":["line1","line2"]}]}]}"#;
        let records = parse_check_output("test", json, "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "line1\nline2");
    }

    #[test]
    fn test_results_shell_escaped_json_parsed() {
        let json = r#"{\"testResults\":[{\"name\":\"f\",\"assertionResults\":[{\"status\":\"failed\",\"fullName\":\"t\",\"failureMessages\":[\"boom\"]}]}]}"#;
        let records = parse_check_output("test", json, "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "boom");
    }

    #[test]
    fn test_results_non_json_falls_back_to_raw() {
        let records = parse_check_output("test", "plain failure text", "stderr text", 1);
        assert_eq!(records.len(), 1);
        assert!(records[0].message.contains("plain failure text"));
        assert!(records[0].message.contains("stderr text"));
    }

    #[test]
    fn test_results_no_failures_falls_back() {
        let json = r#"{"testResults":[{"name":"f","assertionResults":[{"status":"passed","fullName":"t"}]}]}"#;
        let records = parse_check_output("test", json, "", 1);
        // No failed assertions -> falls back to raw output wrapping
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn lint_json_eslint_format_parsed() {
        let json = r#"[
            {
                "filePath": "/repo/a.ts",
                "messages": [
                    {"line": 3, "column": 4, "message": "no-unused", "severity": 2},
                    {"line": 5, "column": 1, "message": "prefer-const", "severity": 1}
                ]
            }
        ]"#;
        let records = parse_check_output("lint", json, "", 1);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].file.as_deref(), Some("/repo/a.ts"));
        assert_eq!(records[0].line, Some(3));
        assert_eq!(records[0].severity.as_deref(), Some("error"));
        assert_eq!(records[1].severity.as_deref(), Some("warning"));
    }

    #[test]
    fn lint_stylish_format_parsed() {
        let output = "/repo/a.ts\n  3:4  error  Unexpected thing  no-unused\n  5:1  warning  Prefer const  prefer-const\n";
        let records = parse_check_output("lint", output, "", 1);
        // Only errors kept (warnings skipped in stylish path)
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].file.as_deref(), Some("/repo/a.ts"));
        assert_eq!(records[0].line, Some(3));
        assert_eq!(records[0].column, Some(4));
        assert_eq!(records[0].severity.as_deref(), Some("error"));
    }

    #[test]
    fn lint_unparseable_falls_back_to_raw() {
        let records = parse_check_output("lint", "totally opaque lint output", "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "totally opaque lint output");
    }

    #[test]
    fn lint_long_message_is_capped() {
        let big = "x".repeat(5_000);
        let records = parse_check_output("lint", &big, "", 1);
        assert_eq!(records.len(), 1);
        assert!(records[0].message.contains("truncated"));
        assert!(records[0].message.len() < big.len());
    }

    #[test]
    fn format_prettier_warn_lines_parsed() {
        let output = "[warn] src/foo.ts\n[warn] src/bar.css\n";
        let records = parse_check_output("format", output, "", 1);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].file.as_deref(), Some("src/foo.ts"));
        assert_eq!(records[0].message, "File is not formatted");
        assert_eq!(records[0].severity.as_deref(), Some("warning"));
    }

    #[test]
    fn format_bare_filenames_parsed() {
        let output = "src/a.tsx\nsrc/b.json\nsrc/c.md\nnot-a-file-line\n";
        let records = parse_check_output("format", output, "", 1);
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|r| r.message == "File is not formatted"));
    }

    #[test]
    fn format_no_recognizable_output_falls_back() {
        let records = parse_check_output("format", "prettier crashed hard", "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "prettier crashed hard");
        assert_eq!(records[0].severity.as_deref(), Some("error"));
    }

    #[test]
    fn diff_errors_with_output() {
        let records = parse_check_output("diff", "diff --git a b", "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "diff --git a b");
    }

    #[test]
    fn diff_errors_empty_output_uses_default_message() {
        let records = parse_check_output("diff", "", "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "No repository changes were produced");
    }

    #[test]
    fn build_errors_parse_typescript_style() {
        let stdout = "src/x.ts(9,2): error TS5: broken";
        let records = parse_check_output("build", stdout, "", 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].file.as_deref(), Some("src/x.ts"));
        assert_eq!(records[0].line, Some(9));
    }

    #[test]
    fn build_errors_fallback_wraps_raw() {
        let records = parse_check_output("build", "linker exploded", "ld: fatal", 1);
        assert_eq!(records.len(), 1);
        assert!(records[0].message.contains("linker exploded"));
        assert!(records[0].message.contains("ld: fatal"));
    }

    #[test]
    fn build_errors_empty_output_returns_empty() {
        let records = parse_check_output("build", "", "", 1);
        assert!(records.is_empty());
    }

    #[test]
    fn build_summary_reports_pass_and_fail() {
        let checks = vec![
            check("lint", true, vec![]),
            check(
                "test",
                false,
                vec![ErrorRecord::default(), ErrorRecord::default()],
            ),
        ];
        let summary = build_summary(&checks);
        assert_eq!(summary, "lint: pass, test: 2 errors");
    }

    #[test]
    fn build_summary_empty_reports_no_checks() {
        assert_eq!(build_summary(&[]), "No checks ran");
    }

    #[test]
    fn cap_error_message_short_passthrough() {
        assert_eq!(cap_error_message("short"), "short");
    }

    #[test]
    fn cap_error_message_truncates_and_preserves_utf8() {
        let msg = "é".repeat(3_000); // 6000 bytes > 4000 cap
        let capped = cap_error_message(&msg);
        assert!(capped.contains("truncated"));
        // Must remain valid UTF-8 (no panic slicing) and shorter than input.
        assert!(capped.len() < msg.len());
    }

    #[test]
    fn unescape_shell_json_converts_escaped_quotes() {
        assert_eq!(unescape_shell_json(r#"\"a\""#), r#""a""#);
        assert_eq!(unescape_shell_json("plain"), "plain");
    }
}
