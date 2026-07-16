use serde::Deserialize;

use crate::adapters::github::GithubError;

#[derive(Debug, Deserialize)]
struct IssueEvent {
    event: String,
    label: Option<EventLabel>,
    actor: Option<EventActor>,
}

#[derive(Debug, Deserialize)]
struct EventLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct EventActor {
    login: String,
}

pub(super) fn issue_events_argv(repo: &str, number: u64) -> Vec<String> {
    vec![
        "gh".to_owned(),
        "api".to_owned(),
        format!("repos/{repo}/issues/{number}/events"),
        "--paginate".to_owned(),
        "--slurp".to_owned(),
    ]
}

pub(super) fn latest_label_actor(json: &str, label: &str) -> Result<Option<String>, GithubError> {
    let pages: Vec<Vec<IssueEvent>> =
        serde_json::from_str(json).map_err(|error| GithubError::CommandFailed {
            argv: vec!["gh".to_owned(), "api".to_owned()],
            exit_code: None,
            stderr: format!("failed to parse issue events JSON: {error}"),
        })?;
    Ok(pages
        .into_iter()
        .flatten()
        .rev()
        .find(|event| {
            event.event == "labeled"
                && event
                    .label
                    .as_ref()
                    .is_some_and(|value| value.name.eq_ignore_ascii_case(label))
        })
        .and_then(|event| event.actor.map(|actor| actor.login)))
}

#[cfg(test)]
mod tests {
    use super::{issue_events_argv, latest_label_actor};

    #[test]
    fn selects_latest_matching_label_actor_across_pages() {
        let json = r#"[[{"event":"labeled","label":{"name":"OK for Luther"},"actor":{"login":"bot"}}],[{"event":"labeled","label":{"name":"other"},"actor":{"login":"x"}},{"event":"labeled","label":{"name":"ok FOR luther"},"actor":{"login":"acoliver"}}]]"#;
        assert_eq!(
            latest_label_actor(json, "OK for Luther").expect("events parse"),
            Some("acoliver".to_owned())
        );
    }

    #[test]
    fn emits_paginated_safe_argv() {
        assert_eq!(
            issue_events_argv("owner/repo", 42),
            [
                "gh",
                "api",
                "repos/owner/repo/issues/42/events",
                "--paginate",
                "--slurp"
            ]
        );
    }
}
