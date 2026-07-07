#[derive(Debug, Default, Deserialize)]
struct GraphqlPageInfo {
    #[serde(default, rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(default, rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubIssuePageResponse {
    data: Option<GraphqlSubIssuePageData>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubIssuePageData {
    repository: GraphqlSubIssuePageRepository,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubIssuePageRepository {
    issue: Option<GraphqlSubIssuePageIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubIssuePageIssue {
    #[serde(default, rename = "subIssues")]
    sub_issues: GraphqlSubIssueConnection,
}


const SUB_ISSUE_PAGE_LIMIT_PREFIX: &str = "sub-issue GraphQL pagination exceeded ";

fn is_native_sub_issue_fallback_error(error: &GithubError) -> bool {
    matches!(
        error,
        GithubError::CommandFailed { stderr, .. }
            if stderr.contains("Field 'subIssues' doesn't exist")
                || stderr.contains("Cannot query field \"subIssues\"")
                || stderr.contains("subIssues unavailable")
    )
}

fn native_sub_issue_page_limit_error(repo: &str, number: u64, cursor: &str) -> GithubError {
    GithubError::CommandFailed {
        argv: graphql_sub_issue_page_argv(repo, number, cursor).unwrap_or_else(|_| {
            vec![
                "gh".to_string(),
                "api".to_string(),
                "graphql".to_string(),
            ]
        }),
        exit_code: None,
        stderr: format!("{SUB_ISSUE_PAGE_LIMIT_PREFIX}{MAX_NATIVE_SUB_ISSUE_PAGES} pages"),
    }
}

fn missing_sub_issue_parent_error(argv: &[String]) -> GithubError {
    GithubError::CommandFailed {
        argv: argv.to_vec(),
        exit_code: None,
        stderr: "sub-issue GraphQL response did not include the requested parent issue".to_string(),
    }
}

fn graphql_sub_issue_page_argv(
    repo: &str,
    number: u64,
    cursor: &str,
) -> Result<Vec<String>, GithubError> {
    let mut argv = graphql_issue_argv(repo, number, SUB_ISSUES_PAGE_QUERY)?;
    argv.push("-F".to_string());
    argv.push(format!("after={cursor}"));
    Ok(argv)
}

const SUB_ISSUES_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){issue(number:$number){number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title} subIssues(first:100){edges{node{number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}} pageInfo{hasNextPage endCursor}}}}}";

const SUB_ISSUES_PAGE_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String!){repository(owner:$owner,name:$name){issue(number:$number){subIssues(first:100,after:$after){edges{cursor node{number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}} pageInfo{hasNextPage endCursor}}}}}";

const PARENT_ISSUE_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){issue(number:$number){parent{number title state body labels(first:50){nodes{name}} assignees(first:10){nodes{login}} milestone{title}}}}}";

struct ParsedSubIssuePage {
    children: Vec<GithubSubIssue>,
    next_cursor: Option<String>,
}

pub fn parse_sub_issue_response(json: &str) -> Result<Vec<GithubSubIssue>, GithubError> {
    parse_first_sub_issue_page(
        json,
        &["gh".to_string(), "api".to_string(), "graphql".to_string()],
    )
    .map(|page| page.children)
}

fn parse_sub_issue_page_response(
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
            None => Err(GithubError::CommandFailed {
                argv: argv.to_vec(),
                exit_code: None,
                stderr: "sub-issue GraphQL page indicated another page without an endCursor".to_string(),
            }),
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
        if seen.insert(edge.node.number) {
            children.push(GithubSubIssue {
                issue: graphql_issue_to_issue(edge.node),
                position: Some(children.len() as u64),
                source: SubIssueSource::Native,
            });
        }
    }
}

fn parse_first_sub_issue_page(
    json: &str,
    argv: &[String],
) -> Result<ParsedSubIssuePage, GithubError> {
    let response: GraphqlResponse =
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
        Some(page_info.end_cursor.ok_or_else(|| GithubError::CommandFailed {
            argv: argv.to_vec(),
            exit_code: None,
            stderr: "sub-issue GraphQL page indicated another page without an endCursor".to_string(),
        })?)
    } else {
        None
    };
    Ok(ParsedSubIssuePage {
        children,
        next_cursor,
    })
}
