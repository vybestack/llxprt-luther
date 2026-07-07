struct GraphqlResponse {
    data: Option<GraphqlData>,
}

struct GraphqlData {
    repository: GraphqlRepository,
}

struct GraphqlRepository {
    issue: Option<GraphqlIssue>,
}

struct GraphqlParentResponse {
    data: Option<GraphqlParentData>,
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

impl<'de> Deserialize<'de> for GraphqlResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Response {
            data: Option<GraphqlData>,
        }

        let response = Response::deserialize(deserializer)?;
        Ok(Self {
            data: response.data,
        })
    }
}

impl<'de> Deserialize<'de> for GraphqlData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Data {
            repository: GraphqlRepository,
        }

        let data = Data::deserialize(deserializer)?;
        Ok(Self {
            repository: data.repository,
        })
    }
}

impl<'de> Deserialize<'de> for GraphqlRepository {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Repository {
            issue: Option<GraphqlIssue>,
        }

        let repository = Repository::deserialize(deserializer)?;
        Ok(Self {
            issue: repository.issue,
        })
    }
}

impl<'de> Deserialize<'de> for GraphqlParentResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Response {
            data: Option<GraphqlParentData>,
        }

        let response = Response::deserialize(deserializer)?;
        Ok(Self {
            data: response.data,
        })
    }
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
