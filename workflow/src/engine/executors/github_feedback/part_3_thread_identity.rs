fn direct_review_thread_id(value: &Value) -> Option<String> {
    string_at_paths(
        value,
        &[
            "/thread_id",
            "/evidence/thread_id",
            "/original_feedback_identity/thread_id",
        ],
    )
    .filter(|thread_id| is_review_thread_node_id(thread_id))
}

fn review_thread_id_from_stable_marker_key(value: &Value) -> Option<String> {
    value
        .get("stable_marker_key")
        .and_then(Value::as_str)
        .and_then(|key| key.strip_prefix(STABLE_MARKER_THREAD_PREFIX))
        // GitHub GraphQL Relay node IDs do not contain ':'. Any suffix after
        // the first ':' is marker metadata, not part of the thread node ID.
        .and_then(|thread_id| thread_id.split(':').next())
        .filter(|thread_id| is_review_thread_node_id(thread_id))
        .map(ToString::to_string)
}

fn review_thread_id_from_graphql_item_id(value: &Value) -> Option<String> {
    [
        "/item_id",
        "/source_id",
        "/original_feedback_identity/item_id",
    ]
    .iter()
    .find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .and_then(parse_review_thread_id_from_graphql_item_id)
    })
}

const STABLE_MARKER_THREAD_PREFIX: &str = "thread:";
const GRAPHQL_NODE_ID_PREFIX: &str = "graphql:";
const REVIEW_THREAD_NODE_ID_PREFIX: &str = "PRRT_";

fn parse_review_thread_id_from_graphql_item_id(item_id: &str) -> Option<String> {
    item_id
        .strip_prefix(GRAPHQL_NODE_ID_PREFIX)
        .and_then(|suffix| suffix.split(':').next())
        .filter(|thread_id| is_review_thread_node_id(thread_id))
        .map(ToString::to_string)
}

fn is_review_thread_node_id(thread_id: &str) -> bool {
    thread_id.starts_with(REVIEW_THREAD_NODE_ID_PREFIX)
        && thread_id.len() > REVIEW_THREAD_NODE_ID_PREFIX.len()
}

