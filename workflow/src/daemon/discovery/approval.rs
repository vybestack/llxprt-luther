#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ApprovalStatus {
    Authorized,
    MissingProvenance,
    UnauthorizedActor { actual: String },
}

pub(super) fn evaluate_approval_actor(
    approval_actor: Option<&str>,
    expected_actor: &str,
) -> ApprovalStatus {
    match approval_actor {
        Some(actual) if actual.eq_ignore_ascii_case(expected_actor) => ApprovalStatus::Authorized,
        Some(actual) => ApprovalStatus::UnauthorizedActor {
            actual: actual.to_owned(),
        },
        None => ApprovalStatus::MissingProvenance,
    }
}

#[cfg(test)]
mod tests {
    use super::{evaluate_approval_actor, ApprovalStatus};

    #[test]
    fn authorizes_configured_actor_case_insensitively() {
        assert_eq!(
            evaluate_approval_actor(Some("AColiver"), "acoliver"),
            ApprovalStatus::Authorized
        );
    }

    #[test]
    fn rejects_missing_and_unauthorized_provenance() {
        assert_eq!(
            evaluate_approval_actor(None, "acoliver"),
            ApprovalStatus::MissingProvenance
        );
        assert_eq!(
            evaluate_approval_actor(Some("dependabot[bot]"), "acoliver"),
            ApprovalStatus::UnauthorizedActor {
                actual: "dependabot[bot]".to_owned()
            }
        );
    }
}
