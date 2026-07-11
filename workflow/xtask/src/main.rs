mod ocr_review;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

const LINE_COVERAGE_GATE: f64 = 80.0;
const LIZARD_COMPLEXITY_MAX: u32 = 25;
const LIZARD_FUNCTION_LINES_MAX: u32 = 80;
const FILE_LINES_MAX: usize = 1000;
const FILE_LINES_WARN: usize = 750;
const COMPLEXITY_USAGE: &str = "usage: cargo xtask complexity [--changed <base> <head>]";

const RELEASE_BINARY_NAME: &str = "luther-workflow";
const DEFAULT_HOMEBREW_TAP_REPO: &str = "acoliver/homebrew-tap";
const DEFAULT_HOMEBREW_FORMULA_NAME: &str = "luther-workflow";
const CLIPPY_ARGS: [&str; 7] = [
    "clippy",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--",
    "-D",
    "warnings",
];

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
        Some("complexity") => complexity(&args.collect::<Vec<_>>()),
        Some("ocr-review") => ocr_review::run(args.collect()),
        Some("release") => release(args.next().as_deref()),
        Some("release-package") => release_package_cmd(args.next().as_deref()),
        Some("release-publish") => release_publish_cmd(args.next().as_deref()),
        Some("release-update-tap") => release_update_tap_cmd(args.next().as_deref()),
        Some("fmt") => run_checked(
            command("cargo", ["fmt", "--all", "--", "--check"]),
            "cargo fmt",
        ),
        Some("clippy") => run_checked(command("cargo", CLIPPY_ARGS), "cargo clippy"),
        Some("test") => run_checked(
            command(
                "cargo",
                ["test", "--workspace", "--all-features", "--lib", "--tests"],
            ),
            "cargo test",
        ),
        Some(cmd) => bail!("unknown xtask command: {cmd}"),
        None => {
            eprintln!(
                "usage: cargo xtask <qa|guard|coverage|complexity|ocr-review|fmt|clippy|test|release|release-package|release-publish|release-update-tap> [vX.Y.Z]"
            );
            eprintln!(
                "ocr-review modes: [--current | --from <ref> --to <ref> | --pr <number>] [--preview] [--format json]"
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
    run_checked(command("cargo", CLIPPY_ARGS), "cargo clippy")?;
    complexity(&[])?;
    run_checked(
        command(
            "cargo",
            ["test", "--workspace", "--all-features", "--lib", "--tests"],
        ),
        "cargo test",
    )?;
    coverage()
}

fn guard() -> Result<()> {
    let workspace_root = workspace_root();
    let src_dir = workspace_root.join("src");

    for pattern in ["TODO", "FIXME", "todo!(", "unimplemented!("] {
        ensure_no_pattern_in_tree(&src_dir, pattern)?;
    }

    enforce_no_include_stitching(&src_dir)?;

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
        [
            "llvm-cov",
            "--no-report",
            "--workspace",
            "--all-features",
            "--lib",
            "--tests",
            "-q",
        ],
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

fn complexity(args: &[String]) -> Result<()> {
    let workspace_root = workspace_root();
    let venv_dir = workspace_root.join(".venv-lizard");
    let venv_python = venv_dir.join("bin/python");
    let changed_paths = complexity_paths(args, &workspace_root)?;

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

    match changed_paths {
        Some(paths) if paths.is_empty() => {
            eprintln!("No changed Rust source files under src/; skipping lizard complexity gate.");
        }
        Some(paths) => {
            let base = changed_base(args)?;
            run_changed_lizard(&workspace_root, &venv_python, base, &paths)?;
            enforce_changed_file_line_limits(&workspace_root, base, &paths)?;
            enforce_no_include_stitching_for_paths(&paths, &workspace_root)?;
        }
        None => {
            run_lizard(&venv_python, [workspace_root.join("src")].iter())?;
            enforce_file_line_limits(&workspace_root.join("src"))?;

            let tests_dir = workspace_root.join("tests");
            if tests_dir.is_dir() {
                enforce_file_line_limits(&tests_dir)?;
            }

            enforce_no_include_stitching(&workspace_root.join("src"))?;
        }
    }

    Ok(())
}

fn complexity_paths(args: &[String], workspace_root: &Path) -> Result<Option<Vec<PathBuf>>> {
    match args {
        [] => Ok(None),
        [flag, base, head] if flag == "--changed" => {
            changed_rust_source_files(workspace_root, base, head).map(Some)
        }
        _ => bail!(COMPLEXITY_USAGE),
    }
}

fn changed_base(args: &[String]) -> Result<&str> {
    match args {
        [flag, base, _head] if flag == "--changed" => Ok(base),
        _ => bail!(COMPLEXITY_USAGE),
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LizardWarningKey {
    path: String,
    function: String,
}

#[derive(Clone, Debug)]
struct LizardWarning {
    key: LizardWarningKey,
    nloc: u32,
    ccn: u32,
    length: u32,
    line: String,
}

fn run_changed_lizard(
    workspace_root: &Path,
    venv_python: &Path,
    base: &str,
    paths: &[PathBuf],
) -> Result<()> {
    let head_warnings = lizard_warnings(venv_python, workspace_root, paths.iter())?;
    let base_root = workspace_root.join(format!(
        "target/xtask-complexity-baseline-{}",
        std::process::id()
    ));
    let base_warnings = (|| {
        let base_paths = materialize_base_sources(workspace_root, &base_root, base, paths)?;
        lizard_warnings(venv_python, &base_root, base_paths.iter())
    })();
    let _ = fs::remove_dir_all(&base_root);
    let base_warnings = base_warnings?;

    let mut baseline = HashMap::new();
    for warning in base_warnings {
        baseline.insert(warning.key.clone(), warning);
    }

    let regressions = head_warnings
        .into_iter()
        .filter(|warning| lizard_warning_regressed(warning, baseline.get(&warning.key)))
        .map(|warning| warning.line)
        .collect::<Vec<_>>();

    if regressions.is_empty() {
        eprintln!("Changed-file lizard gate passed without new or worsened warnings.");
        Ok(())
    } else {
        for regression in &regressions {
            eprintln!("{regression}");
        }
        bail!(
            "changed-file lizard gate found {} new or worsened warning(s)",
            regressions.len()
        )
    }
}

fn materialize_base_sources(
    workspace_root: &Path,
    base_root: &Path,
    base: &str,
    paths: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let git_prefix = capture_in_dir(workspace_root, "git", ["rev-parse", "--show-prefix"])?;
    let git_prefix = git_prefix.trim();
    fs::create_dir_all(base_root).with_context(|| format!("create {}", base_root.display()))?;
    let mut base_paths = Vec::new();
    for path in paths {
        let rel_path = path
            .strip_prefix(workspace_root)
            .with_context(|| format!("relativize {}", path.display()))?;
        let blob_path = format!("{git_prefix}{}", rel_path.to_string_lossy());
        let output = command_in_dir(
            workspace_root,
            "git",
            ["show", format!("{base}:{blob_path}").as_str()],
        )
        .output()
        .with_context(|| format!("spawn git show {base}:{blob_path}"))?;
        if !output.status.success() {
            continue;
        }
        let base_path = base_root.join(rel_path);
        if let Some(parent) = base_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&base_path, output.stdout)
            .with_context(|| format!("write {}", base_path.display()))?;
        base_paths.push(base_path);
    }
    Ok(base_paths)
}

fn lizard_warnings<'a, I>(venv_python: &Path, root: &Path, paths: I) -> Result<Vec<LizardWarning>>
where
    I: IntoIterator<Item = &'a PathBuf>,
{
    let mut args = vec![
        "-m".to_string(),
        "lizard".to_string(),
        "-C".to_string(),
        LIZARD_COMPLEXITY_MAX.to_string(),
        "-L".to_string(),
        LIZARD_FUNCTION_LINES_MAX.to_string(),
        "-w".to_string(),
    ];
    args.extend(
        paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    let output = command(venv_python.to_string_lossy().as_ref(), args)
        .output()
        .context("spawn lizard complexity gate")?;
    let stdout = String::from_utf8(output.stdout).context("decode lizard stdout")?;
    let mut warnings = Vec::new();
    for line in stdout.lines() {
        if let Some(warning) = parse_lizard_warning(line, root) {
            warnings.push(warning);
        } else if line.contains(": warning: ") {
            bail!("unparseable lizard warning output: {line}");
        }
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if warnings.is_empty() || !stderr.is_empty() {
            bail!("lizard complexity gate failed: {stderr}");
        }
    }
    Ok(warnings)
}

fn parse_lizard_warning(line: &str, root: &Path) -> Option<LizardWarning> {
    let (location, details) = line.split_once(": warning: ")?;
    let (path, _) = location.rsplit_once(':')?;
    let (function, metrics) = details.split_once(" has ")?;
    let mut fields = metrics.split(',').map(str::trim);
    let nloc = parse_metric(fields.next()?, "NLOC")?;
    let ccn = parse_metric(fields.next()?, "CCN")?;
    let _tokens = fields.next()?;
    let _params = fields.next()?;
    let length = parse_metric(fields.next()?, "length")?;
    let path = normalize_lizard_path(path, root);
    Some(LizardWarning {
        key: LizardWarningKey {
            path,
            function: function.to_string(),
        },
        nloc,
        ccn,
        length,
        line: line.to_string(),
    })
}

fn parse_metric(value: &str, suffix: &str) -> Option<u32> {
    value.strip_suffix(suffix)?.trim().parse().ok()
}

fn normalize_lizard_path(path: &str, root: &Path) -> String {
    let path = PathBuf::from(path);
    path.strip_prefix(root)
        .unwrap_or(path.as_path())
        .to_string_lossy()
        .into_owned()
}

fn lizard_warning_regressed(warning: &LizardWarning, base: Option<&LizardWarning>) -> bool {
    base.is_none_or(|base| {
        warning.nloc > base.nloc || warning.ccn > base.ccn || warning.length > base.length
    })
}

fn changed_rust_source_files(
    workspace_root: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<PathBuf>> {
    let range = format!("{base}...{head}");
    let output = capture_in_dir(
        workspace_root,
        "git",
        [
            "diff",
            "--name-only",
            "--relative",
            "--diff-filter=ACMRT",
            range.as_str(),
            "--",
            "src",
        ],
    )?;

    let mut paths = Vec::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let path = workspace_root.join(line);
        if path.extension().and_then(|ext| ext.to_str()) == Some("rs") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn run_lizard<'a, I>(venv_python: &Path, paths: I) -> Result<()>
where
    I: IntoIterator<Item = &'a PathBuf>,
{
    let mut args = vec![
        "-m".to_string(),
        "lizard".to_string(),
        "-C".to_string(),
        LIZARD_COMPLEXITY_MAX.to_string(),
        "-L".to_string(),
        LIZARD_FUNCTION_LINES_MAX.to_string(),
        "-w".to_string(),
    ];
    args.extend(
        paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );

    run_checked(
        command(venv_python.to_string_lossy().as_ref(), args),
        "lizard complexity gate",
    )
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
    let files = walk_rs_files(src_dir)?;
    enforce_file_line_limits_for_files(files.iter())
}

fn enforce_changed_file_line_limits(
    workspace_root: &Path,
    base: &str,
    paths: &[PathBuf],
) -> Result<()> {
    let git_prefix = capture_in_dir(workspace_root, "git", ["rev-parse", "--show-prefix"])?;
    let git_prefix = git_prefix.trim();
    let mut error_exit = false;
    for path in paths {
        let lines = file_line_count(path)?;
        let base_lines = base_file_line_count(workspace_root, base, git_prefix, path)?;
        if lines > FILE_LINES_MAX && base_lines.is_none_or(|base| lines > base) {
            eprintln!(
                "ERROR: {} has {} lines (max {}, base {})",
                path.display(),
                lines,
                FILE_LINES_MAX,
                base_lines
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "missing".to_string())
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

fn base_file_line_count(
    workspace_root: &Path,
    base: &str,
    git_prefix: &str,
    path: &Path,
) -> Result<Option<usize>> {
    let rel_path = path
        .strip_prefix(workspace_root)
        .with_context(|| format!("relativize {}", path.display()))?;
    let blob_path = format!("{git_prefix}{}", rel_path.to_string_lossy());
    let output = command_in_dir(
        workspace_root,
        "git",
        ["show", format!("{base}:{blob_path}").as_str()],
    )
    .output()
    .with_context(|| format!("spawn git show {base}:{blob_path}"))?;
    if output.status.success() {
        let content = String::from_utf8(output.stdout).context("decode base source")?;
        Ok(Some(content.lines().count()))
    } else {
        Ok(None)
    }
}

fn file_line_count(path: &Path) -> Result<usize> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read source file {}", path.display()))?;
    Ok(content.lines().count())
}

fn enforce_file_line_limits_for_files<'a, I>(paths: I) -> Result<()>
where
    I: IntoIterator<Item = &'a PathBuf>,
{
    let mut error_exit = false;
    for path in paths {
        let content = fs::read_to_string(path)
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

const INCLUDE_STITCHING_GUIDANCE: &str = "Source stitching with include!(\"*.rs\") is not allowed for Rust module assembly. Split this code into semantic mod submodules with cohesive responsibilities and narrow visibility. Do not use numbered part files, tail files, or generic support buckets to satisfy file-size limits.";

fn enforce_no_include_stitching(src_dir: &Path) -> Result<()> {
    let violations = collect_include_stitching_violations(src_dir, src_dir)?;
    report_include_stitching_violations(violations)
}

fn enforce_no_include_stitching_for_paths(paths: &[PathBuf], root: &Path) -> Result<()> {
    let mut violations = Vec::new();
    for path in paths {
        check_file_include_stitching(path, root, &mut violations)?;
    }
    report_include_stitching_violations(violations)
}

fn report_include_stitching_violations(
    mut violations: Vec<(PathBuf, usize, String)>,
) -> Result<()> {
    if violations.is_empty() {
        return Ok(());
    }

    violations.sort();
    violations.dedup();
    let details = violations
        .into_iter()
        .map(|(path, line, content)| format!("{}:{}: {}", path.display(), line, content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    bail!(
        "forbidden include!()/split-file source stitching detected in src/:\n{details}\n\n{INCLUDE_STITCHING_GUIDANCE}"
    )
}

fn collect_include_stitching_violations(
    dir: &Path,
    root: &Path,
) -> Result<Vec<(PathBuf, usize, String)>> {
    let mut violations = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
        let path = entry.path();

        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if is_split_component_name(name) {
                violations.push((relative_path(&path, root), 1, format!("dir {name}")));
            }
            violations.extend(collect_include_stitching_violations(&path, root)?);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }

        check_file_include_stitching(&path, root, &mut violations)?;
    }

    Ok(violations)
}

fn check_file_include_stitching(
    path: &Path,
    root: &Path,
    violations: &mut Vec<(PathBuf, usize, String)>,
) -> Result<()> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if is_split_source_file_name(file_name) {
        violations.push((relative_path(path, root), 1, format!("file {file_name}")));
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("read source file {}", path.display()))?;
    for (line_no, snippet) in scan_include_rs_violations(&content) {
        violations.push((relative_path(path, root), line_no, snippet));
    }

    Ok(())
}

fn relative_path(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn is_split_component_name(name: &str) -> bool {
    is_part_numbered_name(name) || is_core_numbered_name(name)
}

fn is_split_source_file_name(name: &str) -> bool {
    // Callers only invoke this for files that already end in `.rs`, so stripping
    // the suffix always yields a distinct stem.
    let stem = name.strip_suffix(".rs").unwrap_or(name);
    is_part_numbered_name(stem) || is_core_numbered_name(stem) || stem.ends_with("_tail")
}

fn is_part_numbered_name(stem: &str) -> bool {
    let Some(rest) = stem.strip_prefix("part_") else {
        return false;
    };
    let mut digits = rest.chars();
    let Some(first) = digits.next() else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    let mut seen_alpha = false;
    for ch in digits {
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

/// A minimal lexical token stream for the include-stitching scanner. Trivia
/// (whitespace, line/block comments) is dropped entirely so that "adjacent"
/// tokens in the stream correspond to the next meaningful source token even
/// across newlines. String literals retain their raw content so the scanner can
/// test for a `.rs` suffix without being fooled by `.rs` mentions inside
/// comments.
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
/// invocations. Returns `(line_number, snippet)` for each violation.
///
/// The scan is lexical rather than physical-line based: it ignores `//` line
/// comments, nesting `/* */` block comments, and any `include`/`.rs` mentions
/// inside normal or raw string literals, while still detecting invocations that
/// span multiple lines or use `()`, `[]`, or `{}` delimiters. `include_str!`
/// and `include_bytes!` are intentionally not matched because their identifier
/// tokens differ from `include`.
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

/// Walk from the opening delimiter of an `include!` invocation to its matching
/// close, reporting whether any string literal argument ends with `.rs`.
/// Returns `(found_rs_literal, close_token_index)`.
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

fn coverage_ignore_regex() -> String {
    [
        // Ignore binaries and generated/build content.
        "src/main\\.rs$",
        "src/main_parts/",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lizard_warning_regression_detects_new_or_worse_metrics() {
        let base = warning("src/lib.rs", "large", 80, 20, 90);
        assert!(!lizard_warning_regressed(
            &warning("src/lib.rs", "large", 80, 20, 90),
            Some(&base)
        ));
        assert!(!lizard_warning_regressed(
            &warning("src/lib.rs", "large", 70, 19, 80),
            Some(&base)
        ));
        assert!(lizard_warning_regressed(
            &warning("src/lib.rs", "large", 81, 20, 90),
            Some(&base)
        ));
        assert!(lizard_warning_regressed(
            &warning("src/lib.rs", "new_large", 81, 20, 90),
            None
        ));
    }

    #[test]
    fn parse_lizard_warning_uses_relative_path_and_metrics() {
        let root = Path::new("/repo/workflow");
        let parsed = parse_lizard_warning(
            "/repo/workflow/src/lib.rs:42: warning: parse_me has 81 NLOC, 26 CCN, 100 token, 2 PARAM, 90 length, 0 ND",
            root,
        )
        .expect("warning parses");

        assert_eq!(parsed.key.path, "src/lib.rs");
        assert_eq!(parsed.key.function, "parse_me");
        assert_eq!(parsed.nloc, 81);
        assert_eq!(parsed.ccn, 26);
        assert_eq!(parsed.length, 90);
    }

    fn warning(path: &str, function: &str, nloc: u32, ccn: u32, length: u32) -> LizardWarning {
        LizardWarning {
            key: LizardWarningKey {
                path: path.to_string(),
                function: function.to_string(),
            },
            nloc,
            ccn,
            length,
            line: String::new(),
        }
    }

    fn scan_lines(content: &str) -> Vec<usize> {
        scan_include_rs_violations(content)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    #[test]
    fn detects_single_line_include_rs() {
        assert_eq!(scan_lines(r#"include!("part_1.rs");"#), vec![1]);
    }

    #[test]
    fn detects_multiline_include_rs_across_whitespace() {
        let source = r#"include!
(
    "generated/tail.rs"
);"#;
        assert_eq!(scan_lines(source), vec![1]);
    }

    #[test]
    fn detects_include_rs_with_bracket_and_brace_delimiters() {
        assert_eq!(scan_lines(r#"include!["a.rs"];"#), vec![1]);
        assert_eq!(scan_lines(r#"include!{"b.rs"}"#), vec![1]);
    }

    #[test]
    fn detects_include_rs_in_raw_string_target() {
        assert_eq!(scan_lines(r##"include!(r#"weird/name.rs"#);"##), vec![1]);
    }

    #[test]
    fn ignores_include_str_and_include_bytes() {
        assert!(scan_lines(r#"include_str!("template.rs");"#).is_empty());
        assert!(scan_lines(r#"include_bytes!("blob.rs");"#).is_empty());
    }

    #[test]
    fn ignores_non_rs_include_targets() {
        assert!(scan_lines(r#"include!("data.txt");"#).is_empty());
        assert!(scan_lines(r#"include!(concat!(env!("OUT_DIR"), "/gen.md"));"#).is_empty());
    }

    #[test]
    fn ignores_include_rs_mentions_in_line_comments() {
        assert!(scan_lines(r#"// include!("part_1.rs")"#).is_empty());
    }

    #[test]
    fn ignores_include_rs_mentions_in_block_comments() {
        let source = r#"/*
 include!(
 "part_1.rs"
 );
*/"#;
        assert!(scan_lines(source).is_empty());
    }

    #[test]
    fn ignores_include_rs_mentions_inside_string_literals() {
        let source = r#"let s = "include!(\"part_1.rs\")";"#;
        assert!(scan_lines(source).is_empty());
    }

    #[test]
    fn ignores_include_rs_mentions_inside_raw_string_literals() {
        let source = r##"let s = r#"include!("part_1.rs")"#;"##;
        assert!(scan_lines(source).is_empty());
    }

    #[test]
    fn char_literal_quote_does_not_break_scanning() {
        let source = r#"let q = '"'; include!("part_1.rs");"#;
        assert_eq!(scan_lines(source), vec![1]);
    }

    #[test]
    fn reports_line_of_include_keyword_for_multiline() {
        let source = r#"fn main() {}

include!(
    "x.rs"
);"#;
        assert_eq!(scan_lines(source), vec![3]);
    }
}
