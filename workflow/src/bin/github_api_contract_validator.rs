use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

const FORBIDDEN: [&str; 7] = [
    "TBD",
    "TODO",
    "json_path TBD",
    "fixture TBD",
    "assertion TBD",
    "@pseudocode lines X-Y",
    "@pseudocode TBD",
];

const REQUIRED_FIXTURES: [&str; 20] = [
    "pr_identity_gh_pr_view.json",
    "pr_identity_graphql_fallback.json",
    "checks_gh_pr_checks_page1.json",
    "check_runs_rest_page2.json",
    "statuses_rest_fallback.json",
    "actions_jobs_page1.json",
    "actions_jobs_page2.json",
    "actions_job_log.json",
    "review_threads_graphql_page1.json",
    "review_threads_graphql_page2.json",
    "review_comments_rest_fallback.json",
    "issue_comments_rest_page2.json",
    "create_issue_comment_response.json",
    "resolve_review_thread_response.json",
    "coderabbit_readiness_ready_empty.json",
    "coderabbit_readiness_in_progress.json",
    "coderabbit_readiness_stable_ready.json",
    "coderabbit_readiness_timeout.json",
    "permission_denied_graphql.json",
    "permission_denied_rest_actions.json",
];

const REQUIRED_ASSERTIONS: [&str; 20] = [
    "github_api_contract::pr_identity_paths",
    "github_api_contract::checks_rest_page_two",
    "github_api_contract::actions_jobs_page_two",
    "github_api_contract::actions_log_paths",
    "github_api_contract::review_threads_page_two",
    "github_api_contract::review_comments_fallback_success",
    "github_api_contract::remote_marker_page_two",
    "github_api_contract::create_marker_comment_response",
    "github_api_contract::resolve_thread_response",
    "github_api_contract::pending_marker_action_paths",
    "github_api_contract::readiness_ready_empty",
    "github_api_contract::readiness_in_progress_overrides",
    "github_api_contract::readiness_stable_ready",
    "github_api_contract::readiness_timeout_budget",
    "github_api_contract::readiness_permission_denied",
    "github_api_contract::permission_denied_graphql",
    "github_api_contract::permission_denied_actions_logs",
    "github_api_contract::permission_denied_resolution_mutation",
    "github_api_contract::marker_body_shell_safe",
    "github_api_contract::resolve_after_idempotency",
];

const REQUIRED_PHRASES: [&str; 14] = [
    "gh pr view <number-or-url> --repo <owner>/<repo> --json number,url,headRefName,headRefOid,baseRefName,baseRefOid,state,isDraft,id",
    "gh pr checks <number> --repo <owner>/<repo> --json name,state,bucket,link,workflow,startedAt,completedAt",
    "GET /repos/{owner}/{repo}/commits/{head_sha}/check-runs?per_page=100&page=<n>",
    "GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs?per_page=100&page=<n>",
    "GET /repos/{owner}/{repo}/pulls/{pull_number}/comments?per_page=100&page=<n>",
    "GET /repos/{owner}/{repo}/issues/{pull_number}/comments?per_page=100&page=<n>",
    "mutation resolveReviewThread($threadId:ID!)",
    "CodeRabbit readiness truth table",
    "Permission denied cases",
    "Follow `Link` pages until exhausted",
    "page 2",
    "body file or GraphQL variables",
    "pending-feedback-marker-actions.json",
    "Artifact schema refinements from API validation",
];

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let contract_path = args
        .next()
        .ok_or_else(|| anyhow!("missing contract path"))?;
    let fixture_root = args.next().ok_or_else(|| anyhow!("missing fixture root"))?;
    if args.next().is_some() {
        bail!("unexpected extra arguments");
    }

    let contract = fs::read_to_string(&contract_path)
        .with_context(|| format!("read contract {contract_path}"))?;
    validate_forbidden(&contract, &contract_path)?;
    validate_required_content(&contract)?;
    validate_fixture_mentions(&contract, Path::new(&fixture_root))?;
    validate_fixture_pointers(&contract, Path::new(&fixture_root))?;
    println!("github_api_contract_validator: PASS");
    Ok(())
}

fn validate_forbidden(contract: &str, contract_path: &str) -> Result<()> {
    for forbidden in FORBIDDEN {
        if contract.contains(forbidden) {
            bail!("forbidden template token {forbidden:?} found in {contract_path}");
        }
    }
    Ok(())
}

fn validate_required_content(contract: &str) -> Result<()> {
    for phrase in REQUIRED_PHRASES {
        if !contract.contains(phrase) {
            bail!("contract missing required phrase: {phrase}");
        }
    }
    for assertion in REQUIRED_ASSERTIONS {
        if !contract.contains(assertion) {
            bail!("contract missing assertion name: {assertion}");
        }
    }
    Ok(())
}

fn validate_fixture_mentions(contract: &str, fixture_root: &Path) -> Result<()> {
    for fixture in REQUIRED_FIXTURES {
        if !contract.contains(fixture) {
            bail!("contract does not mention required fixture {fixture}");
        }
        let path = fixture_root.join(fixture);
        if !path.is_file() {
            bail!("required fixture file is missing: {}", path.display());
        }
    }
    Ok(())
}

fn validate_fixture_pointers(contract: &str, fixture_root: &Path) -> Result<()> {
    let fixture_names: BTreeSet<&str> = REQUIRED_FIXTURES.into_iter().collect();
    for fixture in fixture_names {
        let path = fixture_root.join(fixture);
        let value = read_json_fixture(&path)?;
        let pointers = pointers_for_fixture(contract, fixture);
        if pointers.is_empty() {
            bail!("no JSON pointers documented for fixture {fixture}");
        }
        for pointer in pointers {
            if value.pointer(&pointer).is_none() {
                bail!("fixture {fixture} does not contain documented pointer {pointer}");
            }
        }
    }
    Ok(())
}

fn read_json_fixture(path: &Path) -> Result<Value> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read fixture {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse fixture {}", path.display()))
}

fn pointers_for_fixture(contract: &str, fixture: &str) -> BTreeSet<String> {
    let mut pointers = BTreeSet::new();
    for line in contract.lines().filter(|line| line.contains(fixture)) {
        let Some(after_fixture) = line.split_once(fixture).map(|(_, right)| right) else {
            continue;
        };
        let segment = after_fixture
            .split("; `")
            .next()
            .unwrap_or(after_fixture)
            .trim_start_matches('`')
            .trim_start_matches(':');
        collect_backticked_pointers(segment, &mut pointers);
    }
    pointers
}

fn collect_backticked_pointers(segment: &str, pointers: &mut BTreeSet<String>) {
    let mut remainder = segment;
    while let Some(start) = remainder.find('`') {
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        let token = &after_start[..end];
        if token.starts_with('/') {
            pointers.insert(token.trim_end_matches(',').to_string());
        }
        remainder = &after_start[end + 1..];
    }
}

#[allow(dead_code)]
fn _ensure_pathbuf_linked(_: PathBuf) {}
