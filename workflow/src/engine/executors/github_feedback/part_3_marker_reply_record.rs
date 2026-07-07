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
    graphql_error_summary: Option<&'a str>,
}

fn marker_reply_record(input: MarkerReplyRecordInput<'_>) -> Value {
    let mut record = json!({
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
    });
    if let Some(summary) = input.graphql_error_summary {
        record["graphql_error_summary"] = json!(summary);
    }
    record
}

fn graphql_error_summary(parsed: &Value) -> String {
    parsed
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| graphql_error_messages(errors))
        .filter(|summary| !summary.is_empty())
        .or_else(|| graphql_parse_error_summary(parsed))
        .unwrap_or_else(|| "no GraphQL error message returned".to_string())
}

fn graphql_error_messages(errors: &[Value]) -> String {
    const MAX_GRAPHQL_ERROR_SUMMARY_CHARS: usize = 500;
    let mut summary = String::new();
    for message in errors
        .iter()
        .filter_map(|error| error.get("message").and_then(Value::as_str))
    {
        if !summary.is_empty() {
            append_truncated(&mut summary, "; ", MAX_GRAPHQL_ERROR_SUMMARY_CHARS);
        }
        append_truncated(&mut summary, message, MAX_GRAPHQL_ERROR_SUMMARY_CHARS);
        if summary.chars().count() >= MAX_GRAPHQL_ERROR_SUMMARY_CHARS {
            break;
        }
    }
    summary
}

fn append_truncated(target: &mut String, value: &str, max_chars: usize) {
    let remaining = max_chars.saturating_sub(target.chars().count());
    target.extend(value.chars().take(remaining));
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
