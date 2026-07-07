struct MarkerReplyRecordInput<'a> {
    action: &'a PendingMarkerAction,
    comment_key: &'a str,
    body: &'a str,
    body_path: &'a Path,
    comment_id: Value,
    comment_url: Value,
    in_thread_reply: bool,
    in_reply_to_id: Value,
    warnings: Value,
}

fn marker_reply_record(input: MarkerReplyRecordInput<'_>) -> Value {
    json!({
        "idempotency_key": input.comment_key,
        "comment_id": input.comment_id,
        "comment_url": input.comment_url,
        "in_thread_reply": input.in_thread_reply,
        "in_reply_to_id": input.in_reply_to_id,
        "body_hash": stable_hash(input.body),
        "body_path": input.body_path.display().to_string(),
        "action_id": input.action.value.get("action_id").cloned().unwrap_or(Value::Null),
        "warnings": input.warnings,
        "source": "posted"
    })
}

fn graphql_error_summary(parsed: &Value) -> String {
    let summary = parsed
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| graphql_error_messages(errors))
        .filter(|summary| !summary.is_empty())
        .or_else(|| graphql_parse_error_summary(parsed))
        .unwrap_or_else(|| "no GraphQL error message returned".to_string());
    summary.chars().take(500).collect()
}

fn graphql_error_messages(errors: &[Value]) -> String {
    errors
        .iter()
        .filter_map(|error| error.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("; ")
}

fn graphql_parse_error_summary(parsed: &Value) -> Option<String> {
    let parse_error = parsed.get("parse_error").and_then(Value::as_str)?;
    let raw_response = parsed.get("raw_response").and_then(Value::as_str).unwrap_or("");
    Some(format!(
        "response parse error: {parse_error}; raw_response_hash={}; raw_response_bytes={}",
        stable_hash(raw_response),
        raw_response.len()
    ))
}
