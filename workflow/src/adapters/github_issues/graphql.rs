//! GraphQL deserialize shapes and the shared `gh api graphql` argv/error helpers
//! used by both the parent module and the sub-issue paging submodule.
use serde::Deserialize;

use crate::adapters::github::GithubError;

use super::subissues::GraphqlPageInfo;

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlResponse<Issue> {
    pub(super) data: Option<GraphqlData<Issue>>,
    #[serde(default)]
    pub(super) errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlData<Issue> {
    pub(super) repository: GraphqlRepository<Issue>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlRepository<Issue> {
    pub(super) issue: Option<Issue>,
}

pub(super) type GraphqlParentResponse = GraphqlResponse<GraphqlParentLinkIssue>;

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlError {
    pub(super) message: String,
    pub(super) path: Option<serde_json::Value>,
    pub(super) locations: Option<serde_json::Value>,
    pub(super) extensions: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlParentLinkIssue {
    pub(super) parent: Option<GraphqlIssue>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlIssue {
    pub(super) number: u64,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) state: String,
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) labels: GraphqlNodeList<GraphqlLabel>,
    #[serde(default)]
    pub(super) assignees: GraphqlNodeList<GraphqlAssignee>,
    pub(super) milestone: Option<GraphqlMilestone>,
    #[serde(default, rename = "subIssues")]
    pub(super) sub_issues: GraphqlSubIssueConnection,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct GraphqlSubIssueConnection {
    #[serde(default)]
    pub(super) edges: Vec<GraphqlSubIssueEdge>,
    #[serde(default, rename = "pageInfo")]
    pub(super) page_info: GraphqlPageInfo,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlSubIssueEdge {
    pub(super) node: Option<GraphqlIssue>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlNodeList<T> {
    #[serde(default)]
    pub(super) nodes: Vec<T>,
}

impl<T> Default for GraphqlNodeList<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct GraphqlLabel {
    #[serde(default)]
    pub(super) name: String,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct GraphqlAssignee {
    #[serde(default)]
    pub(super) login: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct GraphqlMilestone {
    #[serde(default)]
    pub(super) title: String,
}

pub(super) fn graphql_issue_argv(
    repo: &str,
    number: u64,
    query: &str,
) -> Result<Vec<String>, GithubError> {
    let (owner, name) = super::repo_owner_name(repo)?;
    Ok(vec![
        "gh".to_string(),
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={query}"),
        "-F".to_string(),
        format!("owner={owner}"),
        "-F".to_string(),
        format!("name={name}"),
        "-F".to_string(),
        format!("number={number}"),
    ])
}

pub(super) fn graphql_response_error(
    argv: &[String],
    errors: &[GraphqlError],
) -> Option<GithubError> {
    if errors.is_empty() {
        return None;
    }
    Some(GithubError::CommandFailed {
        argv: argv.to_vec(),
        exit_code: None,
        stderr: format!(
            "GitHub GraphQL returned errors: {}",
            errors
                .iter()
                .map(graphql_error_context)
                .collect::<Vec<_>>()
                .join("; ")
        ),
    })
}

fn graphql_error_context(error: &GraphqlError) -> String {
    let mut parts = vec![error.message.clone()];
    if let Some(path) = non_empty_json(&error.path) {
        parts.push(format!("path={path}"));
    }
    if let Some(locations) = non_empty_json(&error.locations) {
        parts.push(format!("locations={locations}"));
    }
    if let Some(extensions) = non_empty_json(&error.extensions) {
        parts.push(format!("extensions={extensions}"));
    }
    parts.join(" ")
}

fn non_empty_json(value: &Option<serde_json::Value>) -> Option<String> {
    let value = value.as_ref()?;
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Array(values) if values.is_empty() => None,
        serde_json::Value::Object(values) if values.is_empty() => None,
        _ => Some(value.to_string()),
    }
}
