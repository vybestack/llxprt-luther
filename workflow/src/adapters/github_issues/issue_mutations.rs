pub(super) fn edit_issue_argv(repo: &str, number: u64, flag: &str, value: &str) -> Vec<String> {
    vec![
        "gh".to_owned(),
        "issue".to_owned(),
        "edit".to_owned(),
        number.to_string(),
        "--repo".to_owned(),
        repo.to_owned(),
        flag.to_owned(),
        value.to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::edit_issue_argv;

    #[test]
    fn constructs_structured_assignee_mutation() {
        assert_eq!(
            edit_issue_argv("owner/repo", 42, "--add-assignee", "acoliver"),
            [
                "gh",
                "issue",
                "edit",
                "42",
                "--repo",
                "owner/repo",
                "--add-assignee",
                "acoliver"
            ]
        );
    }
}
