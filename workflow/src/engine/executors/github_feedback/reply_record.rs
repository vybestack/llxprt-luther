use super::*;

pub(super) const WARN_REST_REPLY_NOT_JSON: &str = "rest_reply_response_not_json";
pub(super) const WARN_NO_REVIEW_THREAD_IDENTITY_TOP_LEVEL: &str =
    "no_review_thread_identity_posted_top_level_comment";
pub(super) const WARN_REST_REPLY_ERROR_MESSAGE: &str = "rest_reply_response_error_message";
pub(super) const WARN_MISSING_ID_REST_REPLY: &str = "missing_id_in_rest_reply_response";
pub(super) const WARN_MISSING_URL_REST_REPLY: &str = "missing_url_in_rest_reply_response";
pub(super) const WARN_POSTED_REVIEW_THREAD_REPLY_GRAPHQL: &str =
    "posted_review_thread_reply_via_graphql";
pub(super) const WARN_PARTIAL_SUCCESS_GRAPHQL_ERRORS_PRESENT: &str =
    "partial_success_graphql_errors_present";
pub(super) const WARN_MISSING_COMMENT_GRAPHQL_THREAD_REPLY: &str =
    "missing_comment_in_graphql_thread_reply_response";
pub(super) const WARN_NON_IDEMPOTENT_GRAPHQL_REPLY_UNKNOWN: &str =
    "non_idempotent_graphql_reply_result_unknown";
pub(super) const WARN_MISSING_DATABASE_ID_GRAPHQL_THREAD_REPLY: &str =
    "missing_database_id_in_graphql_thread_reply_response";
pub(super) const WARN_MISSING_URL_GRAPHQL_THREAD_REPLY: &str =
    "missing_url_in_graphql_thread_reply_response";

pub(super) const UNKNOWN_DELIVERY_WARNINGS: &[&str] = &[
    WARN_REST_REPLY_NOT_JSON,
    WARN_REST_REPLY_ERROR_MESSAGE,
    WARN_MISSING_ID_REST_REPLY,
    WARN_MISSING_URL_REST_REPLY,
    WARN_MISSING_COMMENT_GRAPHQL_THREAD_REPLY,
    WARN_NON_IDEMPOTENT_GRAPHQL_REPLY_UNKNOWN,
    WARN_MISSING_DATABASE_ID_GRAPHQL_THREAD_REPLY,
    WARN_MISSING_URL_GRAPHQL_THREAD_REPLY,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeliveryStatus {
    Confirmed,
    Unknown,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unknown => "unknown",
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            Self::Confirmed => "posted",
            Self::Unknown => "posted_result_unknown",
        }
    }

    pub fn retry_suppressed(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

pub(super) fn post_marker_reply_rest(
    runner: &dyn GithubPrCommandRunner,
    endpoint: String,
    body_path: &Path,
) -> Result<Value, EngineError> {
    let response = runner.run_github_command(&[
        "gh".to_string(),
        "api".to_string(),
        endpoint,
        "--method".to_string(),
        "POST".to_string(),
        "--field".to_string(),
        format!("body=@{}", body_path.display()),
    ])?;
    let parsed = serde_json::from_str::<Value>(&response);
    Ok(match parsed {
        Ok(value @ Value::Object(_)) => value,
        Ok(value) => rest_parse_error_record(
            &response,
            format!("expected JSON object, got {}", json_value_kind(&value)),
        ),
        Err(err) => rest_parse_error_record(&response, err.to_string()),
    })
}

pub(super) fn rest_parse_error_record(response: &str, parse_error: String) -> Value {
    json!({
        "parse_error": parse_error,
        "raw_response_hash": stable_hash(response),
        "raw_response_bytes": response.len(),
        "raw_response_preview": truncated_raw_response_preview(response)
    })
}

pub(super) fn json_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(super) fn rest_reply_warnings(parsed: &Value, fallback_warning: Option<&str>) -> Value {
    let mut warnings = Vec::new();
    if let Some(warning) = fallback_warning {
        warnings.push(warning);
    }
    let parse_error_present = parsed.get("parse_error").is_some();
    let error_message = if parse_error_present {
        None
    } else {
        rest_reply_error_message(parsed)
    };
    if parse_error_present {
        warnings.push(WARN_REST_REPLY_NOT_JSON);
    } else if error_message.is_some() {
        warnings.push(WARN_REST_REPLY_ERROR_MESSAGE);
    }
    if !parse_error_present && error_message.is_none() {
        if parsed.get("id").is_none() {
            warnings.push(WARN_MISSING_ID_REST_REPLY);
        }
        if parsed.get("html_url").is_none() {
            warnings.push(WARN_MISSING_URL_REST_REPLY);
        }
    }
    json!(warnings)
}

pub(super) struct MarkerReplyRecordInput<'a> {
    pub action: &'a PendingMarkerAction,
    pub comment_key: &'a str,
    pub body: &'a str,
    pub body_path: &'a Path,
    pub comment_id: Value,
    pub comment_url: Value,
    pub in_thread_reply: bool,
    pub in_reply_to_id: Value,
    pub warnings: Value,
    pub github_response_summary: Option<&'a str>,
    pub github_response_preview: Option<&'a str>,
}

pub(super) fn marker_reply_record(input: MarkerReplyRecordInput<'_>) -> Value {
    let delivery_status = marker_reply_delivery_status(&input);
    let mut record = json!({
        "idempotency_key": input.comment_key,
        "comment_id": input.comment_id,
        "comment_url": input.comment_url,
        "in_thread_reply": input.in_thread_reply,
        "in_reply_to_id": input.in_reply_to_id,
        "body_hash": stable_hash(input.body),
        "body_path": input.body_path.display().to_string(),
        "action_id": marker_action_id(input.action),
        "warnings": input.warnings,
        "source": delivery_status.source(),
        "github_delivery_status": delivery_status.as_str(),
        "retry_suppressed": delivery_status.retry_suppressed()
    });
    if let Some(summary) = input.github_response_summary {
        record["github_response_summary"] = json!(summary);
    }
    if let Some(preview) = input.github_response_preview {
        record["github_response_preview"] = json!(preview);
    }
    record
}

pub(super) fn marker_reply_delivery_status(input: &MarkerReplyRecordInput<'_>) -> DeliveryStatus {
    if input.comment_id.is_null()
        || input.comment_url.is_null()
        || marker_reply_warnings_include_unknown_delivery(&input.warnings)
    {
        DeliveryStatus::Unknown
    } else {
        DeliveryStatus::Confirmed
    }
}

pub(super) fn marker_reply_warnings_include_unknown_delivery(warnings: &Value) -> bool {
    let Some(warnings) = warnings.as_array() else {
        tracing::warn!("warnings should be a JSON array, got: {warnings}");
        return true;
    };
    warnings
        .iter()
        .filter_map(Value::as_str)
        .any(|warning| UNKNOWN_DELIVERY_WARNINGS.contains(&warning))
}

pub(super) fn marker_reply_record_for_missing_graphql_comment(
    action: &PendingMarkerAction,
    comment_key: &str,
    body: &str,
    body_path: &Path,
    thread_id: &str,
    parsed: &Value,
) -> Result<Value, EngineError> {
    let error_summary = graphql_error_summary(parsed);
    let has_partial_data = parsed.get("data").is_some_and(|data| !data.is_null());
    if !has_partial_data {
        return Err(github_feedback_error(format!(
            "GraphQL addPullRequestReviewThreadReply failed for thread {thread_id}; no data returned; {error_summary}"
        )));
    }
    Ok(marker_reply_record(MarkerReplyRecordInput {
        action,
        comment_key,
        body,
        body_path,
        comment_id: Value::Null,
        comment_url: Value::Null,
        in_thread_reply: true,
        in_reply_to_id: Value::Null,
        warnings: json!([
            WARN_POSTED_REVIEW_THREAD_REPLY_GRAPHQL,
            WARN_MISSING_COMMENT_GRAPHQL_THREAD_REPLY,
            WARN_NON_IDEMPOTENT_GRAPHQL_REPLY_UNKNOWN
        ]),
        github_response_summary: Some(error_summary.as_str()),
        github_response_preview: None,
    }))
}

pub(super) fn graphql_error_summary(parsed: &Value) -> String {
    parsed
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| graphql_error_messages(errors))
        .filter(|summary| !summary.is_empty())
        .or_else(|| graphql_parse_error_summary(parsed))
        .unwrap_or_else(|| "no GraphQL error message returned".to_string())
}

pub(super) fn graphql_error_messages(errors: &[Value]) -> String {
    const MAX_GRAPHQL_ERROR_SUMMARY_CHARS: usize = 500;
    const GRAPHQL_ERROR_SEPARATOR: &str = "; ";
    let separator_len = GRAPHQL_ERROR_SEPARATOR.chars().count();
    let mut summary = String::new();
    let mut summary_len = 0;
    for message in errors
        .iter()
        .filter_map(|error| error.get("message").and_then(Value::as_str))
        .filter(|message| !message.is_empty())
    {
        if !summary.is_empty() {
            let remaining = MAX_GRAPHQL_ERROR_SUMMARY_CHARS.saturating_sub(summary_len);
            if remaining <= separator_len {
                break;
            }
            summary.push_str(GRAPHQL_ERROR_SEPARATOR);
            summary_len += separator_len;
        }
        summary_len += append_truncated(
            &mut summary,
            message,
            summary_len,
            MAX_GRAPHQL_ERROR_SUMMARY_CHARS,
        );
        if summary_len >= MAX_GRAPHQL_ERROR_SUMMARY_CHARS {
            break;
        }
    }
    summary
}

pub(super) fn append_truncated(
    target: &mut String,
    value: &str,
    current_chars: usize,
    max_chars: usize,
) -> usize {
    let remaining = max_chars.saturating_sub(current_chars);
    let take = value
        .char_indices()
        .nth(remaining)
        .map_or(value.len(), |(idx, _)| idx);
    let truncated = &value[..take];
    target.push_str(truncated);
    truncated.chars().count()
}

pub(super) fn graphql_parse_error_summary(parsed: &Value) -> Option<String> {
    let parse_error = parsed.get("parse_error").and_then(Value::as_str)?;
    let raw_response = parsed
        .get("raw_response")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(format!(
        "response parse error: {parse_error}; raw_response_hash={}; raw_response_bytes={}",
        stable_hash(raw_response),
        raw_response.len()
    ))
}

pub(super) fn rest_response_preview(parsed: &Value) -> Option<&str> {
    parsed
        .get("parse_error")
        .and_then(|_| parsed.get("raw_response_preview"))
        .and_then(Value::as_str)
}

pub(super) fn rest_response_summary(parsed: &Value) -> Option<String> {
    parsed
        .get("parse_error")
        .and_then(Value::as_str)
        .map(|parse_error| {
            let raw_response_hash = parsed
                .get("raw_response_hash")
                .and_then(Value::as_str)
                .unwrap_or("");
            let raw_response_bytes = parsed
                .get("raw_response_bytes")
                .and_then(Value::as_u64)
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "response parse error: {parse_error}; raw_response_hash={raw_response_hash}; raw_response_bytes={raw_response_bytes}"
            )
        })
        .or_else(|| rest_reply_error_message(parsed).map(ToString::to_string))
}

pub(super) fn rest_reply_error_message(parsed: &Value) -> Option<&str> {
    parsed
        .get("message")
        .and_then(Value::as_str)
        .filter(|_| is_rest_error_response(parsed))
}

pub(super) fn truncated_raw_response_preview(response: &str) -> String {
    const MAX_RAW_RESPONSE_PREVIEW_CHARS: usize = 500;
    let Some((end, _)) = response.char_indices().nth(MAX_RAW_RESPONSE_PREVIEW_CHARS) else {
        return response.to_string();
    };
    let mut preview = response[..end].to_string();
    preview.push_str("...[truncated]");
    preview
}

pub(super) fn is_rest_error_response(parsed: &Value) -> bool {
    parsed.get("documentation_url").is_some()
        || parsed
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
}
