//! Native GitHub sub-issue paging: GraphQL page shapes, query strings, page
//! parsing, and the pagination/fallback error helpers.
use std::collections::BTreeSet;

use serde::Deserialize;

use crate::adapters::github::GithubError;

use super::graphql::{
    graphql_issue_argv, graphql_response_error, GraphqlIssue, GraphqlResponse, GraphqlSubIssueEdge,
};
use super::{graphql_issue_to_issue, GithubSubIssue, SubIssueSource};

#[derive(Debug, Default, Deserialize)]
pub(super) struct GraphqlPageInfo {
    #[serde(default, rename = "hasNextPage")]
    pub(super) has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub(super) end_cursor: Option<String>,
}

pub(super) type GraphqlSubIssuePageResponse = GraphqlResponse<GraphqlSubIssuePageIssue>;

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlSubIssuePageIssue {
    #[serde(default, rename = "subIssues")]
    pub(super) sub_issues: super::graphql::GraphqlSubIssueConnection,
}

pub(super) const SUB_ISSUE_PAGE_LIMIT_PREFIX: &str = "sub-issue GraphQL pagination exceeded ";

pub(super) const SUB_ISSUES_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){issue(number:$number){number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title} subIssues(first:100){edges{node{number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}} pageInfo{hasNextPage endCursor}}}}}";

pub(super) const SUB_ISSUES_PAGE_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String!){repository(owner:$owner,name:$name){issue(number:$number){subIssues(first:100,after:$after){edges{cursor node{number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}} pageInfo{hasNextPage endCursor}}}}}";

pub(super) const PARENT_ISSUE_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){issue(number:$number){parent{number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}}}}";

pub(super) struct ParsedSubIssuePage {
    pub(super) children: Vec<GithubSubIssue>,
    pub(super) next_cursor: Option<String>,
}

pub(super) fn is_native_sub_issue_fallback_error(error: &GithubError) -> bool {
    matches!(
        error,
        GithubError::CommandFailed { stderr, .. }
            if stderr.contains("Field 'subIssues' doesn't exist")
                || stderr.contains("Cannot query field \"subIssues\"")
                || stderr.contains("subIssues unavailable")
    )
}

pub(super) fn native_sub_issue_page_limit_error(
    repo: &str,
    number: u64,
    cursor: &str,
) -> GithubError {
    GithubError::CommandFailed {
        argv: graphql_sub_issue_page_argv(repo, number, cursor)
            .unwrap_or_else(|_| vec!["gh".to_string(), "api".to_string(), "graphql".to_string()]),
        exit_code: None,
        stderr: format!(
            "{SUB_ISSUE_PAGE_LIMIT_PREFIX}{} pages for {repo} issue #{number}",
            super::MAX_NATIVE_SUB_ISSUE_PAGES
        ),
    }
}

fn missing_sub_issue_parent_error(argv: &[String]) -> GithubError {
    GithubError::CommandFailed {
        argv: argv.to_vec(),
        exit_code: None,
        stderr: "sub-issue GraphQL response did not include the requested parent issue".to_string(),
    }
}

fn missing_sub_issue_end_cursor_error(argv: &[String]) -> GithubError {
    GithubError::CommandFailed {
        argv: argv.to_vec(),
        exit_code: None,
        stderr: "sub-issue GraphQL page indicated another page without an endCursor".to_string(),
    }
}

pub(super) fn graphql_sub_issue_page_argv(
    repo: &str,
    number: u64,
    cursor: &str,
) -> Result<Vec<String>, GithubError> {
    let mut argv = graphql_issue_argv(repo, number, SUB_ISSUES_PAGE_QUERY)?;
    argv.push("-F".to_string());
    argv.push(format!("after={cursor}"));
    Ok(argv)
}

#[cfg(test)]
pub(super) fn parse_sub_issue_response(json: &str) -> Result<Vec<GithubSubIssue>, GithubError> {
    parse_first_sub_issue_page(
        json,
        &["gh".to_string(), "api".to_string(), "graphql".to_string()],
    )
    .map(|page| page.children)
}

pub(super) fn parse_sub_issue_page_response(
    json: &str,
    seen: &mut BTreeSet<u64>,
    children: &mut Vec<GithubSubIssue>,
    argv: &[String],
) -> Result<Option<String>, GithubError> {
    let response: GraphqlSubIssuePageResponse =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse sub-issue GraphQL page JSON: {e}"),
        })?;
    if let Some(err) = graphql_response_error(argv, &response.errors) {
        return Err(err);
    }
    let Some(issue) = response.data.and_then(|data| data.repository.issue) else {
        return Err(missing_sub_issue_parent_error(argv));
    };
    append_sub_issue_edges(issue.sub_issues.edges, seen, children);
    if issue.sub_issues.page_info.has_next_page {
        return match issue.sub_issues.page_info.end_cursor {
            Some(cursor) => Ok(Some(cursor)),
            None => Err(missing_sub_issue_end_cursor_error(argv)),
        };
    }
    Ok(None)
}

fn append_sub_issue_edges(
    edges: Vec<GraphqlSubIssueEdge>,
    seen: &mut BTreeSet<u64>,
    children: &mut Vec<GithubSubIssue>,
) {
    for edge in edges {
        let Some(node) = edge.node else {
            continue;
        };
        if seen.insert(node.number) {
            children.push(GithubSubIssue {
                issue: graphql_issue_to_issue(node),
                position: Some(children.len() as u64),
                source: SubIssueSource::Native,
            });
        }
    }
}

pub(super) fn parse_first_sub_issue_page(
    json: &str,
    argv: &[String],
) -> Result<ParsedSubIssuePage, GithubError> {
    let response: GraphqlResponse<GraphqlIssue> =
        serde_json::from_str(json).map_err(|e| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: format!("failed to parse sub-issue GraphQL JSON: {e}"),
        })?;
    if let Some(err) = graphql_response_error(argv, &response.errors) {
        return Err(err);
    }
    let Some(issue) = response.data.and_then(|data| data.repository.issue) else {
        return Err(missing_sub_issue_parent_error(argv));
    };
    let mut seen = BTreeSet::new();
    let mut children = Vec::new();
    append_sub_issue_edges(issue.sub_issues.edges, &mut seen, &mut children);
    let page_info = issue.sub_issues.page_info;
    let next_cursor = if page_info.has_next_page {
        match page_info.end_cursor {
            Some(cursor) => Ok(Some(cursor)),
            None => Err(missing_sub_issue_end_cursor_error(argv)),
        }?
    } else {
        None
    };
    Ok(ParsedSubIssuePage {
        children,
        next_cursor,
    })
}
