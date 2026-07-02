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
