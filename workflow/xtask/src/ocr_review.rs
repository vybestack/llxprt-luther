use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

const DEFAULT_TIMEOUT_MINUTES: &str = "20";
const ARTIFACT_DIR: &str = "artifacts/ocr";
const OCR_INSTALL_HINT: &str =
    "Install OCR with npm install -g @alibaba-group/open-code-review or pass --ocr-path <path>.";

#[derive(Debug, Clone, Eq, PartialEq)]
enum Mode {
    Current,
    Range { from: String, to: String },
    Pr { number: String },
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Options {
    mode: Mode,
    preview: bool,
    format: OutputFormat,
    ocr_path: Option<PathBuf>,
    allow_excluded_tests: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolvedScope {
    mode_summary: String,
    from: Option<String>,
    to: Option<String>,
    changed_files: Vec<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct PreviewSections {
    reviewed: BTreeSet<String>,
    excluded: BTreeSet<String>,
}

pub fn run(args: Vec<String>) -> Result<()> {
    let options = parse_options(args)?;
    let workspace = super::workspace_root();
    let artifact_dir = workspace.join(ARTIFACT_DIR);
    fs::create_dir_all(&artifact_dir).context("create OCR artifact directory")?;

    let ocr = discover_ocr(options.ocr_path.as_deref())?;
    let version = ocr_version(&ocr)?;
    fs::write(artifact_dir.join("ocr-version.txt"), &version)?;
    println!("ocr: {}", version.trim());

    let scope = resolve_scope(&options.mode)?;
    let preview = run_ocr(&ocr, &scope, &options, true)?;
    write_output_artifacts(&artifact_dir, "ocr-preview", &preview)?;
    if !preview.status.success() {
        bail!(
            "OCR preview failed for {}; artifacts: {}",
            scope.mode_summary,
            artifact_dir.display()
        );
    }
    enforce_test_inclusion(
        &scope.changed_files,
        &preview.stdout,
        options.allow_excluded_tests,
    )?;

    if options.preview {
        println!(
            "OCR preview complete for {}; artifacts: {}",
            scope.mode_summary,
            artifact_dir.display()
        );
        return Ok(());
    }

    let review = run_ocr(&ocr, &scope, &options, false)?;
    write_output_artifacts(&artifact_dir, "ocr", &review)?;
    fs::write(
        artifact_dir.join("ocr-exit-code.txt"),
        exit_code_text(&review),
    )?;

    if !review.status.success() {
        bail!(
            "OCR review failed for {}; artifacts: {}",
            scope.mode_summary,
            artifact_dir.display()
        );
    }

    if options.format == OutputFormat::Json {
        validate_json_output(&review.stdout, &artifact_dir)?;
    }

    println!(
        "OCR review complete for {}; artifacts: {}",
        scope.mode_summary,
        artifact_dir.display()
    );
    Ok(())
}

fn parse_options(args: Vec<String>) -> Result<Options> {
    let mut current = false;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut pr: Option<String> = None;
    let mut preview = false;
    let mut format = OutputFormat::Text;
    let mut ocr_path: Option<PathBuf> = None;
    let mut allow_excluded_tests = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--current" => current = true,
            "--from" => from = Some(next_value(&mut iter, "--from")?),
            "--to" => to = Some(next_value(&mut iter, "--to")?),
            "--pr" => pr = Some(next_value(&mut iter, "--pr")?),
            "--preview" => preview = true,
            "--format" => format = parse_format(&next_value(&mut iter, "--format")?)?,
            "--ocr-path" => ocr_path = Some(PathBuf::from(next_value(&mut iter, "--ocr-path")?)),
            "--allow-excluded-tests" => allow_excluded_tests = true,
            "-h" | "--help" => bail!(usage()),
            other => bail!("unknown ocr-review argument: {other}\n{}", usage()),
        }
    }

    let range = match (from, to) {
        (Some(from), Some(to)) => Some(Mode::Range { from, to }),
        (None, None) => None,
        _ => bail!("--from and --to must be supplied together"),
    };
    let supplied_modes =
        usize::from(current) + usize::from(range.is_some()) + usize::from(pr.is_some());
    if supplied_modes > 1 {
        bail!("choose only one OCR review mode: --current, --from/--to, or --pr");
    }
    let mode = if let Some(range) = range {
        range
    } else if let Some(number) = pr {
        Mode::Pr { number }
    } else {
        Mode::Current
    };

    Ok(Options {
        mode,
        preview,
        format,
        ocr_path,
        allow_excluded_tests,
    })
}

fn parse_format(format: &str) -> Result<OutputFormat> {
    match format {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        other => bail!("unsupported OCR format: {other}; expected json or text"),
    }
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    iter.next()
        .ok_or_else(|| anyhow!("missing value for {flag}"))
}

fn usage() -> &'static str {
    "Usage: cargo xtask ocr-review [--current | --from <ref> --to <ref> | --pr <number>] [--preview] [--format json] [--ocr-path <path>] [--allow-excluded-tests]"
}

fn discover_ocr(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Ok(path) = env::var("OCR_BIN") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    super::which("ocr").ok_or_else(|| anyhow!("OCR binary not found. {OCR_INSTALL_HINT}"))
}

fn ocr_version(ocr: &Path) -> Result<String> {
    let version = run_command(Command::new(ocr).arg("version"))?;
    let version = if version.status.success() {
        version
    } else {
        run_command(Command::new(ocr).arg("--version"))?
    };
    if !version.status.success() {
        bail!("failed to run OCR version. {OCR_INSTALL_HINT}");
    }
    let stdout = String::from_utf8_lossy(&version.stdout);
    let stderr = String::from_utf8_lossy(&version.stderr);
    let text = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    if text.is_empty() {
        bail!("OCR version output was empty. {OCR_INSTALL_HINT}");
    }
    Ok(format!("{text}\n"))
}

fn resolve_scope(mode: &Mode) -> Result<ResolvedScope> {
    match mode {
        Mode::Current => Ok(ResolvedScope {
            mode_summary: "current working-tree diff".to_string(),
            from: None,
            to: None,
            changed_files: current_changed_files()?,
        }),
        Mode::Range { from, to } => resolve_range_scope(from, to),
        Mode::Pr { number } => resolve_pr_scope(number),
    }
}

fn resolve_range_scope(from: &str, to: &str) -> Result<ResolvedScope> {
    let resolved_from = verify_ref(from)?;
    let resolved_to = verify_ref(to)?;
    let changed_files = git_lines([
        "diff",
        "--name-only",
        &format!("{resolved_from}..{resolved_to}"),
    ])?;
    Ok(ResolvedScope {
        mode_summary: format!("range {resolved_from}..{resolved_to}"),
        from: Some(resolved_from),
        to: Some(resolved_to),
        changed_files,
    })
}

fn resolve_pr_scope(number: &str) -> Result<ResolvedScope> {
    ensure_tool("gh")?;
    let pr_json = command_stdout(Command::new("gh").args([
        "pr",
        "view",
        number,
        "--json",
        "baseRefName,headRefOid",
    ]))?;
    let value: Value = serde_json::from_str(&pr_json).context("parse gh pr view JSON")?;
    let base = value["baseRefName"]
        .as_str()
        .ok_or_else(|| anyhow!("PR JSON missing baseRefName"))?;
    let head = value["headRefOid"]
        .as_str()
        .ok_or_else(|| anyhow!("PR JSON missing headRefOid"))?;
    let local_head = format!("refs/ocr-review/pr-{number}");
    let pr_refspec = format!("pull/{number}/head:{local_head}");
    run_checked(Command::new("git").args(["fetch", "origin", &pr_refspec]))?;
    let fetched_head = verify_ref(&local_head)?;
    if fetched_head != head {
        bail!("PR head changed during fetch; expected {head}, got {fetched_head}");
    }

    let base_tracking = format!("refs/remotes/origin/{base}");
    let base_refspec = format!("{base}:{base_tracking}");
    run_checked(Command::new("git").args(["fetch", "origin", &base_refspec]))?;
    let merge_base =
        command_stdout(Command::new("git").args(["merge-base", &base_tracking, &local_head]))?;
    let merge_base = merge_base.trim().to_string();
    let changed_files = git_lines(["diff", "--name-only", &format!("{merge_base}..{head}")])?;
    Ok(ResolvedScope {
        mode_summary: format!("PR {number} range {merge_base}..{head}"),
        from: Some(merge_base),
        to: Some(head.to_string()),
        changed_files,
    })
}

fn verify_ref(reference: &str) -> Result<String> {
    let output = command_stdout(Command::new("git").args(["rev-parse", "--verify", reference]))?;
    Ok(output.trim().to_string())
}

fn current_changed_files() -> Result<Vec<String>> {
    let mut files = git_lines(["diff", "--name-only", "HEAD", "--"])?;
    files.extend(git_lines(["ls-files", "--others", "--exclude-standard"])?);
    files.sort();
    files.dedup();
    Ok(files)
}

fn git_lines<const N: usize>(args: [&str; N]) -> Result<Vec<String>> {
    let output = command_stdout(Command::new("git").args(args))?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn run_ocr(ocr: &Path, scope: &ResolvedScope, options: &Options, preview: bool) -> Result<Output> {
    let mut command = Command::new(ocr);
    command
        .arg("review")
        .arg("--audience")
        .arg("agent")
        .arg("--timeout")
        .arg(DEFAULT_TIMEOUT_MINUTES);
    if preview {
        command.arg("--preview");
    }
    if options.format == OutputFormat::Json && !preview {
        command.arg("--format").arg("json");
    }
    if let (Some(from), Some(to)) = (&scope.from, &scope.to) {
        command.arg("--from").arg(from).arg("--to").arg(to);
    }
    run_command(&mut command).with_context(|| format!("run OCR for {}", scope.mode_summary))
}

fn write_output_artifacts(artifact_dir: &Path, prefix: &str, output: &Output) -> Result<()> {
    let stdout_name = if prefix == "ocr-preview" {
        "ocr-preview.txt"
    } else {
        "ocr-stdout.raw"
    };
    let stderr_name = if prefix == "ocr-preview" {
        "ocr-preview-stderr.log"
    } else {
        "ocr-stderr.log"
    };
    fs::write(artifact_dir.join(stdout_name), &output.stdout)?;
    fs::write(artifact_dir.join(stderr_name), &output.stderr)?;
    Ok(())
}

fn enforce_test_inclusion(
    changed_files: &[String],
    preview_stdout: &[u8],
    allow_excluded_tests: bool,
) -> Result<()> {
    let test_paths = changed_files
        .iter()
        .filter(|path| is_test_path(path))
        .cloned()
        .collect::<Vec<_>>();
    if test_paths.is_empty() || allow_excluded_tests {
        return Ok(());
    }
    let preview = parse_preview(&String::from_utf8_lossy(preview_stdout));
    let missing = test_paths
        .into_iter()
        .filter(|path| !preview.reviewed.contains(path) || preview.excluded.contains(path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        bail!(
            "OCR preview excluded or omitted changed test/spec paths: {}",
            missing.join(", ")
        )
    }
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("test")
        || lower.contains("spec")
        || lower.contains("/__tests__/")
        || lower.starts_with("tests/")
}

fn parse_preview(text: &str) -> PreviewSections {
    let mut sections = PreviewSections::default();
    let mut current: Option<&str> = None;
    for raw_line in text.lines() {
        let line = strip_ansi_codes(raw_line);
        let lower = line.to_ascii_lowercase();
        if lower.contains("will review") {
            current = Some("reviewed");
            continue;
        }
        if lower.contains("excluded") {
            current = Some("excluded");
            continue;
        }
        if let Some(path) = preview_path(&line) {
            match current {
                Some("reviewed") => {
                    sections.reviewed.insert(path);
                }
                Some("excluded") => {
                    sections.excluded.insert(path);
                }
                _ => {}
            }
        }
    }
    sections
}

fn preview_path(line: &str) -> Option<String> {
    let trimmed = line.trim().trim_start_matches(['-', '*', '•']).trim();
    let trimmed = strip_preview_status(trimmed).trim();
    let token = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(['`', '"', '\'']);
    if token.is_empty() || token.ends_with(':') || !token.contains('/') {
        None
    } else {
        Some(token.to_string())
    }
}

fn strip_preview_status(line: &str) -> &str {
    let Some(rest) = line.strip_prefix('[') else {
        return line;
    };
    let Some((_, path)) = rest.split_once(']') else {
        return line;
    };
    path
}

fn strip_ansi_codes(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.next() == Some('[') {
                for code in chars.by_ref() {
                    if code.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            stripped.push(ch);
        }
    }
    stripped
}

fn validate_json_output(stdout: &[u8], artifact_dir: &Path) -> Result<()> {
    if stdout.is_empty() {
        bail!(
            "OCR JSON output was empty; raw output preserved in {}",
            artifact_dir.display()
        );
    }
    let value: Value = serde_json::from_slice(stdout).with_context(|| {
        format!(
            "OCR emitted invalid JSON; raw output preserved in {}",
            artifact_dir.display()
        )
    })?;
    fs::write(
        artifact_dir.join("ocr-result.json"),
        serde_json::to_vec_pretty(&value)?,
    )?;
    Ok(())
}

fn ensure_tool(program: &str) -> Result<()> {
    super::which(program)
        .map(|_| ())
        .ok_or_else(|| anyhow!("required tool not found on PATH: {program}"))
}

fn run_checked(command: &mut Command) -> Result<()> {
    let output = run_command(command)?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("command failed: {:?}", command)
    }
}

fn command_stdout(command: &mut Command) -> Result<String> {
    let output = run_command(command)?;
    if !output.status.success() {
        bail!("command failed: {:?}", command);
    }
    String::from_utf8(output.stdout).context("command output was not UTF-8")
}

fn run_command(command: &mut Command) -> Result<Output> {
    command.current_dir(super::workspace_root());
    command
        .output()
        .with_context(|| format!("execute {:?}", command))
}

fn exit_code_text(output: &Output) -> String {
    match output.status.code() {
        Some(code) => format!("{code}\n"),
        None => "terminated-by-signal\n".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_current_mode() {
        let options = parse_options(Vec::new()).unwrap();
        assert_eq!(options.mode, Mode::Current);
        assert!(!options.preview);
        assert_eq!(options.format, OutputFormat::Text);
    }

    #[test]
    fn parses_range_json_and_override() {
        let options = parse_options(vec![
            "--from".into(),
            "main".into(),
            "--to".into(),
            "HEAD".into(),
            "--format".into(),
            "json".into(),
            "--allow-excluded-tests".into(),
        ])
        .unwrap();
        assert_eq!(
            options.mode,
            Mode::Range {
                from: "main".into(),
                to: "HEAD".into()
            }
        );
        assert_eq!(options.format, OutputFormat::Json);
        assert!(options.allow_excluded_tests);
    }

    #[test]
    fn rejects_conflicting_modes_and_partial_ranges() {
        assert!(parse_options(vec!["--current".into(), "--pr".into(), "1".into()]).is_err());
        assert!(parse_options(vec!["--from".into(), "main".into()]).is_err());
    }

    #[test]
    fn classifies_review_relevant_test_paths() {
        for path in [
            "tests/foo.rs",
            "src/foo_test.rs",
            "foo.spec.ts",
            "src/__tests__/foo.rs",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        assert!(!is_test_path("src/lib.rs"));
    }

    #[test]
    fn parses_preview_reviewed_and_excluded_paths() {
        let preview = parse_preview(
            "Will review (3)\n- src/lib.rs\n- `tests/foo.rs`\n  \u{1b}[33m[M]\u{1b}[0m  workflow/tests/bar.rs  \u{1b}[32m+1\u{1b}[0m\nExcluded (1)\n- \"tests/baz.rs\"\n",
        );
        assert!(preview.reviewed.contains("src/lib.rs"));
        assert!(preview.reviewed.contains("tests/foo.rs"));
        assert!(preview.reviewed.contains("workflow/tests/bar.rs"));
        assert!(preview.excluded.contains("tests/baz.rs"));
    }

    #[test]
    fn enforces_changed_tests_are_reviewed() {
        let changed = vec!["tests/foo.rs".to_string()];
        let preview = b"Will review\n- tests/foo.rs\nExcluded\n";
        enforce_test_inclusion(&changed, preview, false).unwrap();
        let excluded = b"Will review\nExcluded\n- tests/foo.rs\n";
        assert!(enforce_test_inclusion(&changed, excluded, false).is_err());
    }

    #[test]
    fn builds_review_command_with_mandatory_flags() {
        let scope = ResolvedScope {
            mode_summary: "test".into(),
            from: Some("a".into()),
            to: Some("b".into()),
            changed_files: Vec::new(),
        };
        let options = Options {
            mode: Mode::Current,
            preview: false,
            format: OutputFormat::Json,
            ocr_path: None,
            allow_excluded_tests: false,
        };
        let argv = ocr_review_args(&scope, &options, false);
        assert!(argv.windows(2).any(|pair| pair == ["--audience", "agent"]));
        assert!(argv
            .windows(2)
            .any(|pair| pair == ["--timeout", DEFAULT_TIMEOUT_MINUTES]));
        assert!(argv.windows(2).any(|pair| pair == ["--format", "json"]));
    }

    fn ocr_review_args(scope: &ResolvedScope, options: &Options, preview: bool) -> Vec<String> {
        let mut args = vec![
            "review".into(),
            "--audience".into(),
            "agent".into(),
            "--timeout".into(),
            DEFAULT_TIMEOUT_MINUTES.into(),
        ];
        if preview {
            args.push("--preview".into());
        }
        if options.format == OutputFormat::Json && !preview {
            args.extend(["--format".into(), "json".into()]);
        }
        if let (Some(from), Some(to)) = (&scope.from, &scope.to) {
            args.extend(["--from".into(), from.clone(), "--to".into(), to.clone()]);
        }
        args
    }
}
