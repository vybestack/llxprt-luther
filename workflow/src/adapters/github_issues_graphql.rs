#[derive(Debug, Deserialize)]
struct GraphqlResponse {
    data: Option<GraphqlData>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlData {
    repository: GraphqlRepository,
}

#[derive(Debug, Deserialize)]
struct GraphqlRepository {
    issue: Option<GraphqlIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentResponse {
    data: Option<GraphqlParentData>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentData {
    repository: GraphqlParentRepository,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentRepository {
    issue: Option<GraphqlParentLinkIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlParentLinkIssue {
    #[serde(default)]
    parent: Option<GraphqlIssue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    labels: GraphqlNodeList<GraphqlLabel>,
    #[serde(default)]
    assignees: GraphqlNodeList<GraphqlAssignee>,
    #[serde(default)]
    milestone: Option<GraphqlMilestone>,
    #[serde(default, rename = "subIssues")]
    sub_issues: GraphqlSubIssueConnection,
}

#[derive(Debug, Default, Deserialize)]
struct GraphqlSubIssueConnection {
    #[serde(default)]
    edges: Vec<GraphqlSubIssueEdge>,
    #[serde(default, rename = "pageInfo")]
    page_info: GraphqlPageInfo,
}

#[derive(Debug, Deserialize)]
struct GraphqlSubIssueEdge {
    node: GraphqlIssue,
}

#[derive(Debug, Deserialize)]
struct GraphqlNodeList<T> {
    #[serde(default)]
    nodes: Vec<T>,
}

impl<T> Default for GraphqlNodeList<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

#[derive(Debug, Deserialize, Default)]
struct GraphqlLabel {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize, Default)]
struct GraphqlAssignee {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct GraphqlMilestone {
    #[serde(default)]
    title: String,
}
