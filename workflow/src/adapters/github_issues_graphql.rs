#[derive(Debug, Deserialize)]
struct GraphqlResponse<Issue> {
    data: Option<GraphqlData<Issue>>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Debug, Deserialize)]
struct GraphqlData<Issue> {
    repository: GraphqlRepository<Issue>,
}

#[derive(Debug, Deserialize)]
struct GraphqlRepository<Issue> {
    issue: Option<Issue>,
}

type GraphqlParentResponse = GraphqlResponse<GraphqlParentLinkIssue>;

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
    #[serde(default)]
    path: Option<serde_json::Value>,
    #[serde(default)]
    locations: Option<serde_json::Value>,
    #[serde(default)]
    extensions: Option<serde_json::Value>,
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
    #[serde(default)]
    node: Option<GraphqlIssue>,
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
