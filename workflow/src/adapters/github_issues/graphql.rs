//! GraphQL deserialize shapes and the shared `gh api graphql` argv/error helpers
//! used by both the parent module and the sub-issue paging submodule.
use serde::Deserialize;

use crate::adapters::github::GithubError;

#[derive(Debug, Default, Deserialize)]
pub(super) struct GraphqlPageInfo {
    #[serde(default, rename = "hasNextPage")]
    pub(super) has_next_page: bool,
    #[serde(rename = "endCursor")]
    pub(super) end_cursor: Option<String>,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error(
        message: &str,
        path: Option<serde_json::Value>,
        locations: Option<serde_json::Value>,
        extensions: Option<serde_json::Value>,
    ) -> GraphqlError {
        GraphqlError {
            message: message.to_string(),
            path,
            locations,
            extensions,
        }
    }

    #[test]
    fn graphql_issue_argv_builds_owner_name_number_flags() {
        let argv = graphql_issue_argv("acme/widgets", 42, "query Foo { bar }").unwrap();
        assert_eq!(
            argv,
            vec![
                "gh".to_string(),
                "api".to_string(),
                "graphql".to_string(),
                "-f".to_string(),
                "query=query Foo { bar }".to_string(),
                "-F".to_string(),
                "owner=acme".to_string(),
                "-F".to_string(),
                "name=widgets".to_string(),
                "-F".to_string(),
                "number=42".to_string(),
            ]
        );
    }

    #[test]
    fn graphql_issue_argv_rejects_malformed_repo() {
        assert!(graphql_issue_argv("no-slash", 1, "q").is_err());
        assert!(graphql_issue_argv("owner/", 1, "q").is_err());
    }

    #[test]
    fn graphql_response_error_is_none_without_errors() {
        let argv = vec!["gh".to_string()];
        assert!(graphql_response_error(&argv, &[]).is_none());
    }

    #[test]
    fn graphql_response_error_joins_multiple_error_contexts() {
        let argv = vec!["gh".to_string(), "api".to_string()];
        let errors = vec![
            error("first failed", None, None, None),
            error(
                "second failed",
                Some(json!(["repository", "issue"])),
                Some(json!([{ "line": 3, "column": 5 }])),
                Some(json!({ "code": "NOT_FOUND" })),
            ),
        ];
        let Some(GithubError::CommandFailed {
            argv: reported_argv,
            exit_code,
            stderr,
        }) = graphql_response_error(&argv, &errors)
        else {
            panic!("expected CommandFailed error");
        };
        assert_eq!(reported_argv, argv);
        assert_eq!(exit_code, None);
        assert!(stderr.contains("GitHub GraphQL returned errors:"));
        assert!(stderr.contains("first failed"));
        assert!(stderr.contains("second failed"));
        assert!(stderr.contains("path="));
        assert!(stderr.contains("locations="));
        assert!(stderr.contains("extensions="));
        // The two error contexts are separated by "; ".
        assert!(stderr.contains("; "));
    }

    #[test]
    fn graphql_error_context_omits_empty_and_null_optional_fields() {
        // Null, empty array, and empty object are all treated as absent so the
        // context contains only the message.
        let only_message = error("boom", Some(json!(null)), Some(json!([])), Some(json!({})));
        assert_eq!(graphql_error_context(&only_message), "boom");
    }

    #[test]
    fn non_empty_json_filters_empty_containers_and_null() {
        assert_eq!(non_empty_json(&None), None);
        assert_eq!(non_empty_json(&Some(json!(null))), None);
        assert_eq!(non_empty_json(&Some(json!([]))), None);
        assert_eq!(non_empty_json(&Some(json!({}))), None);
        assert_eq!(
            non_empty_json(&Some(json!(["a"]))),
            Some("[\"a\"]".to_string())
        );
        assert_eq!(
            non_empty_json(&Some(json!({ "k": 1 }))),
            Some("{\"k\":1}".to_string())
        );
    }
}
