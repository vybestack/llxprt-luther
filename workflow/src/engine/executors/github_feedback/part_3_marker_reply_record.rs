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

