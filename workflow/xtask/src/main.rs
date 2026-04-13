use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

const LINE_COVERAGE_GATE: f64 = 80.0;
const LIZARD_COMPLEXITY_MAX: u32 = 25;
const LIZARD_FUNCTION_LINES_MAX: u32 = 80;
const FILE_LINES_MAX: usize = 1000;
const FILE_LINES_WARN: usize = 750;

const RELEASE_BINARY_NAME: &str = "luther-workflow";
const DEFAULT_HOMEBREW_TAP_REPO: &str = "acoliver/homebrew-tap";
const DEFAULT_HOMEBREW_FORMULA_NAME: &str = "luther-workflow";

fn main() {
    if let Err(error) = run() {
        eprintln!("xtask error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("qa") => qa(),
        Some("guard") => guard(),
        Some("coverage") => coverage(),
        Some("complexity") => complexity(),
        Some("release") => release(args.next().as_deref()),
        Some("release-package") => release_package_cmd(args.next().as_deref()),
        Some("release-publish") => release_publish_cmd(args.next().as_deref()),
        Some("release-update-tap") => release_update_tap_cmd(args.next().as_deref()),
        Some("fmt") => run_checked(
            command("cargo", ["fmt", "--all", "--", "--check"]),
            "cargo fmt",
        ),
        Some("clippy") => run_checked(
            command(
                "cargo",
                [
                    "clippy",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                    "-D",
                    "clippy::cognitive_complexity",
                    "-D",
                    "clippy::too_many_lines",
                    "-D",
                    "clippy::too_many_arguments",
                    "-D",
                    "clippy::type_complexity",
                    "-D",
                    "clippy::struct_excessive_bools",
                ],
            ),
            "cargo clippy",
        ),
        Some("test") => run_checked(command("cargo", ["test", "--lib", "--tests"]), "cargo test"),
        Some(cmd) => bail!("unknown xtask command: {cmd}"),
        None => {
            eprintln!(
                "usage: cargo xtask <qa|guard|coverage|complexity|fmt|clippy|test|release|release-package|release-publish|release-update-tap> [vX.Y.Z]"
            );
            Ok(())
        }
    }
}

fn qa() -> Result<()> {
    guard()?;
    run_checked(
        command("cargo", ["fmt", "--all", "--", "--check"]),
        "cargo fmt",
    )?;
    run_checked(
        command(
            "cargo",
            [
                "clippy",
                "--all-targets",
                "--",
                "-D",
                "warnings",
                "-D",
                "clippy::cognitive_complexity",
                "-D",
                "clippy::too_many_lines",
                "-D",
                "clippy::too_many_arguments",
                "-D",
                "clippy::type_complexity",
                "-D",
                "clippy::struct_excessive_bools",
            ],
        ),
        "cargo clippy",
    )?;
    complexity()?;
    run_checked(command("cargo", ["test", "--lib", "--tests"]), "cargo test")?;
    coverage()
}

fn guard() -> Result<()> {
    let workspace_root = workspace_root();
    let src_dir = workspace_root.join("src");

    for pattern in ["TODO", "FIXME", "todo!(", "unimplemented!("] {
        ensure_no_pattern_in_tree(&src_dir, pattern)?;
    }

    Ok(())
}

fn coverage() -> Result<()> {
    ensure_tool("cargo-llvm-cov", "cargo install cargo-llvm-cov --locked")?;
    ensure_tool("rustup", "install rustup and llvm-tools-preview")?;

    let workspace_root = workspace_root();
    let llvm_cov = find_rustup_llvm_tool("llvm-cov")?;
    let llvm_profdata = find_rustup_llvm_tool("llvm-profdata")?;

    let target_dir = workspace_root.join("target/llvm-cov-target");
    let summary_path = target_dir.join("workspace-summary.json");
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir)
            .with_context(|| format!("remove stale coverage directory {}", target_dir.display()))?;
    }

    run_checked(
        command("cargo", ["llvm-cov", "clean", "--workspace"]),
        "cargo llvm-cov clean",
    )?;

    let mut run_cmd = command(
        "cargo",
        ["llvm-cov", "--no-report", "--lib", "--tests", "-q"],
    );
    run_cmd.env("LLVM_COV", &llvm_cov);
    run_cmd.env("LLVM_PROFDATA", &llvm_profdata);
    run_checked(run_cmd, "cargo llvm-cov --no-report")?;

    let summary_path_arg = summary_path.to_string_lossy().into_owned();
    let mut report_cmd = command(
        "cargo",
        [
            "llvm-cov",
            "report",
            "--json",
            "--summary-only",
            "--skip-functions",
            "--ignore-filename-regex",
            coverage_ignore_regex().as_str(),
            "--output-path",
            summary_path_arg.as_str(),
        ],
    );
    report_cmd.env("LLVM_COV", &llvm_cov);
    report_cmd.env("LLVM_PROFDATA", &llvm_profdata);
    run_checked(report_cmd, "cargo llvm-cov report")?;

    let coverage = load_workspace_line_coverage(&summary_path, &workspace_root)?;
    eprintln!(
        "workspace line coverage: {:.2}% (source: {})",
        coverage,
        summary_path.display()
    );

    if coverage < LINE_COVERAGE_GATE {
        bail!(
            "workspace line coverage {:.2}% is below the {:.2}% gate",
            coverage,
            LINE_COVERAGE_GATE
        );
    }

    Ok(())
}

fn complexity() -> Result<()> {
    let workspace_root = workspace_root();
    let venv_dir = workspace_root.join(".venv-lizard");
    let venv_python = venv_dir.join("bin/python");

    if !venv_python.exists() {
        run_checked(
            command("python3", ["-m", "venv", ".venv-lizard"]),
            "python3 -m venv .venv-lizard",
        )?;
    }

    run_checked(
        command(
            venv_python.to_string_lossy().as_ref(),
            ["-m", "pip", "install", "--upgrade", "pip"],
        ),
        "pip upgrade",
    )?;

    run_checked(
        command(
            venv_python.to_string_lossy().as_ref(),
            ["-m", "pip", "install", "lizard"],
        ),
        "pip install lizard",
    )?;

    run_checked(
        command(
            venv_python.to_string_lossy().as_ref(),
            [
                "-m",
                "lizard",
                "-C",
                &LIZARD_COMPLEXITY_MAX.to_string(),
                "-L",
                &LIZARD_FUNCTION_LINES_MAX.to_string(),
                "-w",
                "src/",
            ],
        ),
        "lizard complexity gate",
    )?;

    enforce_file_line_limits(&workspace_root.join("src"))
}

fn release(tag_arg: Option<&str>) -> Result<()> {
    let release_tag = resolve_release_tag(tag_arg)?;
    release_package_for_tag(&release_tag)?;
    release_publish_for_tag(&release_tag)?;
    release_update_tap_for_tag(&release_tag)
}

fn release_package_cmd(tag_arg: Option<&str>) -> Result<()> {
    let release_tag = resolve_release_tag(tag_arg)?;
    release_package_for_tag(&release_tag)
}

fn release_publish_cmd(tag_arg: Option<&str>) -> Result<()> {
    let release_tag = resolve_release_tag(tag_arg)?;
    release_publish_for_tag(&release_tag)
}

fn release_update_tap_cmd(tag_arg: Option<&str>) -> Result<()> {
    let release_tag = resolve_release_tag(tag_arg)?;
    release_update_tap_for_tag(&release_tag)
}

fn release_package_for_tag(release_tag: &str) -> Result<()> {
    validate_release_tag(release_tag)?;

    let workspace_root = workspace_root();
    let artifact_dir = workspace_root.join("artifacts/release");
    if artifact_dir.exists() {
        fs::remove_dir_all(&artifact_dir).with_context(|| {
            format!("remove stale artifact directory {}", artifact_dir.display())
        })?;
    }
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("create artifact directory {}", artifact_dir.display()))?;

    run_checked(
        command(
            "cargo",
            ["build", "--release", "--bin", RELEASE_BINARY_NAME],
        ),
        "cargo build --release",
    )?;

    let binary_path = workspace_root
        .join("target")
        .join("release")
        .join(RELEASE_BINARY_NAME);
    if !binary_path.is_file() {
        bail!(
            "expected release binary not found at {}",
            binary_path.display()
        );
    }

    let binary_path_arg = binary_path.to_string_lossy().into_owned();
    run_checked(
        command(
            "codesign",
            ["--force", "--sign", "-", binary_path_arg.as_str()],
        ),
        "codesign --force --sign -",
    )?;
    run_checked(
        command(
            "codesign",
            ["--verify", "--verbose=2", binary_path_arg.as_str()],
        ),
        "codesign --verify",
    )?;

    let archs = capture("lipo", ["-archs", binary_path_arg.as_str()])?;
    if !archs.split_whitespace().any(|arch| arch == "arm64") {
        bail!(
            "release binary is not arm64; lipo -archs output was `{}`",
            archs.trim()
        );
    }

    let package_dir = workspace_root.join("target/release-package-tmp");
    if package_dir.exists() {
        fs::remove_dir_all(&package_dir)
            .with_context(|| format!("remove stale package directory {}", package_dir.display()))?;
    }
    fs::create_dir_all(&package_dir)
        .with_context(|| format!("create package directory {}", package_dir.display()))?;

    let packaged_binary_path = package_dir.join(RELEASE_BINARY_NAME);
    fs::copy(&binary_path, &packaged_binary_path).with_context(|| {
        format!(
            "copy binary from {} to {}",
            binary_path.display(),
            packaged_binary_path.display()
        )
    })?;

    let asset_name = format!("{RELEASE_BINARY_NAME}-{release_tag}-aarch64-apple-darwin.tar.gz");
    let asset_path = artifact_dir.join(&asset_name);
    let package_dir_arg = package_dir.to_string_lossy().into_owned();
    let asset_path_arg = asset_path.to_string_lossy().into_owned();
    run_checked(
        command(
            "tar",
            [
                "-C",
                package_dir_arg.as_str(),
                "-czf",
                asset_path_arg.as_str(),
                RELEASE_BINARY_NAME,
            ],
        ),
        "tar release artifact",
    )?;

    let sha_line = capture("shasum", ["-a", "256", asset_path_arg.as_str()])?;
    let sha256 = sha_line
        .split_whitespace()
        .next()
        .context("parse shasum output")?
        .to_string();

    let sha_sums = format!("{sha256}  {asset_name}\n");
    fs::write(artifact_dir.join("SHA256SUMS.txt"), sha_sums).context("write SHA256SUMS.txt")?;
    fs::write(
        artifact_dir.join("asset_name.txt"),
        format!("{asset_name}\n"),
    )
    .context("write asset_name.txt")?;
    fs::write(
        artifact_dir.join("asset_path.txt"),
        format!("{}\n", asset_path.display()),
    )
    .context("write asset_path.txt")?;
    fs::write(artifact_dir.join("sha256.txt"), format!("{sha256}\n"))
        .context("write sha256.txt")?;

    fs::remove_dir_all(&package_dir)
        .with_context(|| format!("remove package directory {}", package_dir.display()))?;

    eprintln!("Created release artifact: {}", asset_path.display());
    eprintln!("SHA256: {sha256}");

    Ok(())
}

fn release_publish_for_tag(release_tag: &str) -> Result<()> {
    validate_release_tag(release_tag)?;

    let workspace_root = workspace_root();
    let artifact_dir = workspace_root.join("artifacts/release");
    let asset_path = read_trimmed(&artifact_dir.join("asset_path.txt"))?;
    let sha_sums_path = artifact_dir.join("SHA256SUMS.txt");
    if !Path::new(&asset_path).is_file() {
        bail!("release asset path does not exist: {asset_path}");
    }
    if !sha_sums_path.is_file() {
        bail!("missing SHA256SUMS.txt at {}", sha_sums_path.display());
    }

    let mut view_cmd = command("gh", ["release", "view", release_tag]);
    let release_exists = view_cmd
        .status()
        .with_context(|| format!("check existing release for tag {release_tag}"))?
        .success();

    let sha_sums_arg = sha_sums_path.to_string_lossy().into_owned();
    if release_exists {
        let mut upload_cmd = command("gh", ["release", "upload", release_tag]);
        upload_cmd.arg(asset_path.as_str());
        upload_cmd.arg(sha_sums_arg.as_str());
        upload_cmd.arg("--clobber");
        run_checked(upload_cmd, "gh release upload")?;
    } else {
        let mut create_cmd = command("gh", ["release", "create", release_tag]);
        create_cmd.arg(asset_path.as_str());
        create_cmd.arg(sha_sums_arg.as_str());
        create_cmd.arg("--verify-tag");
        create_cmd.arg("--title");
        create_cmd.arg(release_tag);
        create_cmd.arg("--generate-notes");
        run_checked(create_cmd, "gh release create")?;
    }

    Ok(())
}

fn release_update_tap_for_tag(release_tag: &str) -> Result<()> {
    validate_release_tag(release_tag)?;

    let homebrew_tap_github_token =
        env::var("HOMEBREW_TAP_GITHUB_TOKEN").context("HOMEBREW_TAP_GITHUB_TOKEN must be set")?;
    let github_repository =
        env::var("GITHUB_REPOSITORY").context("GITHUB_REPOSITORY must be set")?;
    let homebrew_tap_repo =
        env::var("HOMEBREW_TAP_REPO").unwrap_or_else(|_| DEFAULT_HOMEBREW_TAP_REPO.to_string());
    let formula_name = env::var("HOMEBREW_FORMULA_NAME")
        .unwrap_or_else(|_| DEFAULT_HOMEBREW_FORMULA_NAME.to_string());
    let formula_class_name = derive_formula_class_name(&formula_name)
        .context("unable to derive formula class name from HOMEBREW_FORMULA_NAME")?;

    let workspace_root = workspace_root();
    let artifact_dir = workspace_root.join("artifacts/release");
    let asset_name = read_trimmed(&artifact_dir.join("asset_name.txt"))?;
    let asset_sha256 = read_trimmed(&artifact_dir.join("sha256.txt"))?;

    let version = release_tag.trim_start_matches('v');
    let release_url = format!(
        "https://github.com/{github_repository}/releases/download/{release_tag}/{asset_name}"
    );

    let work_dir = workspace_root.join("target/homebrew-tap-update");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir)
            .with_context(|| format!("remove stale work directory {}", work_dir.display()))?;
    }
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("create work directory {}", work_dir.display()))?;

    let tap_dir = work_dir.join("homebrew-tap");
    let clone_url = format!(
        "https://x-access-token:{homebrew_tap_github_token}@github.com/{homebrew_tap_repo}.git"
    );
    let tap_dir_arg = tap_dir.to_string_lossy().into_owned();
    run_checked(
        command("git", ["clone", clone_url.as_str(), tap_dir_arg.as_str()]),
        "clone homebrew tap",
    )?;

    fs::create_dir_all(tap_dir.join("Formula")).context("create Formula directory")?;
    let formula_path = tap_dir.join("Formula").join(format!("{formula_name}.rb"));

    let formula_content = format!(
        "class {formula_class_name} < Formula\n  desc \"Luther workflow runtime\"\n  homepage \"https://github.com/{github_repository}\"\n  url \"{release_url}\"\n  version \"{version}\"\n  sha256 \"{asset_sha256}\"\n  license \"MIT\"\n\n  def install\n    bin.install \"{RELEASE_BINARY_NAME}\"\n  end\n\n  test do\n    assert_predicate bin/\"{RELEASE_BINARY_NAME}\", :exist?\n  end\nend\n"
    );

    fs::write(&formula_path, formula_content).with_context(|| {
        format!(
            "write Homebrew formula file {}",
            formula_path.to_string_lossy()
        )
    })?;

    let formula_rel_path = format!("Formula/{formula_name}.rb");
    let porcelain = capture_in_dir(
        &tap_dir,
        "git",
        ["status", "--porcelain", "--", formula_rel_path.as_str()],
    )?;
    if porcelain.trim().is_empty() {
        eprintln!("No changes detected in {formula_rel_path}; skipping Homebrew tap push.");
        return Ok(());
    }

    let git_author_name =
        env::var("GIT_AUTHOR_NAME").unwrap_or_else(|_| "github-actions[bot]".to_string());
    let git_author_email = env::var("GIT_AUTHOR_EMAIL")
        .unwrap_or_else(|_| "41898282+github-actions[bot]@users.noreply.github.com".to_string());

    run_checked(
        command_in_dir(
            &tap_dir,
            "git",
            ["config", "user.name", git_author_name.as_str()],
        ),
        "configure git user.name",
    )?;
    run_checked(
        command_in_dir(
            &tap_dir,
            "git",
            ["config", "user.email", git_author_email.as_str()],
        ),
        "configure git user.email",
    )?;
    run_checked(
        command_in_dir(&tap_dir, "git", ["add", formula_rel_path.as_str()]),
        "git add formula",
    )?;

    let commit_message = format!("{formula_name} {version}");
    run_checked(
        command_in_dir(&tap_dir, "git", ["commit", "-m", commit_message.as_str()]),
        "git commit formula update",
    )?;
    run_checked(
        command_in_dir(&tap_dir, "git", ["push", "origin", "HEAD"]),
        "git push formula update",
    )?;

    eprintln!("Updated {homebrew_tap_repo} {formula_rel_path} for {release_tag}");

    Ok(())
}

fn resolve_release_tag(tag_arg: Option<&str>) -> Result<String> {
    let release_tag = match tag_arg {
        Some(tag) if !tag.trim().is_empty() => tag.trim().to_string(),
        _ => env::var("GITHUB_REF_NAME")
            .context("missing release tag; pass vX.Y.Z or set GITHUB_REF_NAME")?,
    };

    validate_release_tag(&release_tag)?;
    Ok(release_tag)
}

fn validate_release_tag(release_tag: &str) -> Result<()> {
    if is_valid_release_tag(release_tag) {
        Ok(())
    } else {
        bail!("release tag must look like vX.Y.Z (received: {release_tag})")
    }
}

fn is_valid_release_tag(release_tag: &str) -> bool {
    if !release_tag.starts_with('v') {
        return false;
    }

    let version = &release_tag[1..];
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

fn derive_formula_class_name(formula_name: &str) -> Option<String> {
    let mut class_name = String::new();

    for part in formula_name.split(|ch: char| !ch.is_ascii_alphanumeric()) {
        if part.is_empty() {
            continue;
        }

        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            class_name.push(first.to_ascii_uppercase());
            for ch in chars {
                class_name.push(ch.to_ascii_lowercase());
            }
        }
    }

    if class_name.is_empty() {
        None
    } else {
        Some(class_name)
    }
}

fn read_trimmed(path: &Path) -> Result<String> {
    let value = fs::read_to_string(path)
        .with_context(|| format!("read required file {}", path.display()))?;
    Ok(value.trim().to_string())
}

fn enforce_file_line_limits(src_dir: &Path) -> Result<()> {
    let mut error_exit = false;
    for path in walk_rs_files(src_dir)? {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read source file {}", path.display()))?;
        let lines = content.lines().count();
        if lines > FILE_LINES_MAX {
            eprintln!(
                "ERROR: {} has {} lines (max {})",
                path.display(),
                lines,
                FILE_LINES_MAX
            );
            error_exit = true;
        } else if lines > FILE_LINES_WARN {
            eprintln!(
                "WARNING: {} has {} lines (recommended max {})",
                path.display(),
                lines,
                FILE_LINES_WARN
            );
        }
    }

    if error_exit {
        bail!("file line limit exceeded");
    }

    Ok(())
}

fn walk_rs_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read dir {}", dir.display()))? {
            let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn ensure_no_pattern_in_tree(root: &Path, pattern: &str) -> Result<()> {
    let mut violations = Vec::new();
    collect_pattern_violations(root, root, pattern, &mut violations)?;

    if violations.is_empty() {
        return Ok(());
    }

    let details = violations
        .into_iter()
        .map(|(path, line, content)| format!("{}:{}: {}", path.display(), line, content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    bail!("forbidden source pattern `{pattern}` detected in src/:\n{details}")
}

fn collect_pattern_violations(
    dir: &Path,
    root: &Path,
    pattern: &str,
    violations: &mut Vec<(PathBuf, usize, String)>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
        let path = entry.path();

        if path.is_dir() {
            collect_pattern_violations(&path, root, pattern, violations)?;
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let content = fs::read_to_string(&path)
            .with_context(|| format!("read source file {}", path.display()))?;
        for (idx, line) in content.lines().enumerate() {
            if line.contains(pattern) {
                violations.push((PathBuf::from(&relative), idx + 1, line.to_string()));
            }
        }
    }

    Ok(())
}

fn coverage_ignore_regex() -> String {
    [
        // Ignore binaries and generated/build content.
        "src/main\\.rs$",
        "target/",
        "junk/",
    ]
    .join("|")
}

fn load_workspace_line_coverage(summary_path: &Path, workspace_root: &Path) -> Result<f64> {
    let report = fs::read_to_string(summary_path)
        .with_context(|| format!("read coverage summary {}", summary_path.display()))?;
    let report: Value = serde_json::from_str(&report)
        .with_context(|| format!("parse coverage summary {}", summary_path.display()))?;

    let files = report
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .and_then(|entry| entry.get("files"))
        .and_then(Value::as_array)
        .context("coverage summary missing data[0].files")?;

    let mut covered = 0_u64;
    let mut count = 0_u64;

    for file in files {
        let filename = file
            .get("filename")
            .and_then(Value::as_str)
            .context("coverage file missing filename")?;
        let path = Path::new(filename);
        if path.is_absolute() && !path.starts_with(workspace_root) {
            continue;
        }

        let summary = file
            .get("summary")
            .context("coverage file missing summary")?;
        let lines = summary
            .get("lines")
            .context("coverage summary missing lines metric")?;
        covered += lines
            .get("covered")
            .and_then(Value::as_u64)
            .context("coverage metric lines.covered missing or invalid")?;
        count += lines
            .get("count")
            .and_then(Value::as_u64)
            .context("coverage metric lines.count missing or invalid")?;
    }

    if count == 0 {
        bail!("coverage summary had no line data for workspace files");
    }

    Ok((covered as f64 / count as f64) * 100.0)
}

fn ensure_tool(tool: &str, install_hint: &str) -> Result<()> {
    if which(tool).is_some() {
        Ok(())
    } else {
        bail!("required tool `{tool}` not found; install with `{install_hint}`")
    }
}

fn find_rustup_llvm_tool(tool: &str) -> Result<PathBuf> {
    let rustc = capture("rustup", ["which", "rustc"])?;
    let rustc = PathBuf::from(rustc.trim());
    let toolchain_root = rustc
        .parent()
        .and_then(Path::parent)
        .context("resolve rustup toolchain root")?;
    let host = capture("rustc", ["-vV"])?;
    let host = host
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .context("read rustc host triple")?;
    let candidate = toolchain_root
        .join("lib")
        .join("rustlib")
        .join(host)
        .join("bin")
        .join(tool);

    if candidate.is_file() {
        Ok(candidate)
    } else {
        bail!(
            "required rustup LLVM tool `{}` not found at {}; run `rustup component add llvm-tools-preview`",
            tool,
            candidate.display()
        )
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under workspace root")
        .to_path_buf()
}

fn command<I, S>(program: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    command_in_dir(&workspace_root(), program, args)
}

fn command_in_dir<I, S>(dir: &Path, program: &str, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new(program);
    cmd.current_dir(dir);
    cmd.args(args);
    cmd
}

fn run_checked(mut cmd: Command, label: &str) -> Result<()> {
    eprintln!("==> {label}");
    let status = cmd.status().with_context(|| format!("spawn {label}"))?;
    ensure_success(status, label)
}

fn ensure_success(status: ExitStatus, label: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{label} failed with status {status}")
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
}

fn capture<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = command(program, args)
        .output()
        .with_context(|| format!("spawn {program}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout).context("decode command output")
    } else {
        bail!(
            "{} failed: {}",
            program,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn capture_in_dir<I, S>(dir: &Path, program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = command_in_dir(dir, program, args)
        .output()
        .with_context(|| format!("spawn {program} in {}", dir.display()))?;
    if output.status.success() {
        String::from_utf8(output.stdout).context("decode command output")
    } else {
        bail!(
            "{} failed: {}",
            program,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}
