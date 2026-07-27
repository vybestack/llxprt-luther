use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::adapters::github_issues::{GithubIssue, GithubIssuePrState, GithubSubIssue};

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ParentIssueOrchestrationState {
    pub parent_issue_number: u64,
    pub child_issue_numbers: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum ChildIssueStatus {
    Open,
    ActiveRun,
    Closed,
    Merged,
    MergedIssueOpen,
    ClosedUnmerged,
    Superseded,
    StaleRun,
    FailedRun,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ChildIssueState {
    pub issue_number: u64,
    pub terminal_state: ChildIssueStatus,
    pub pr_number: Option<u64>,
}

pub fn order_subissues(children: &[GithubSubIssue]) -> Vec<u64> {
    let mut indices: Vec<usize> = (0..children.len()).collect();
    indices.sort_by(|&left, &right| {
        children[left]
            .position
            .unwrap_or(u64::MAX)
            .cmp(&children[right].position.unwrap_or(u64::MAX))
            .then(
                children[left]
                    .issue
                    .number
                    .cmp(&children[right].issue.number),
            )
    });
    indices
        .into_iter()
        .map(|index| children[index].issue.number)
        .collect()
}

pub fn classify_child(issue: &GithubIssue, pr: Option<&GithubIssuePrState>) -> ChildIssueState {
    let issue_closed = issue.state.eq_ignore_ascii_case("closed");
    let terminal_state = match pr {
        Some(pr) if pr.merged && issue_closed => ChildIssueStatus::Merged,
        Some(pr) if pr.merged => ChildIssueStatus::MergedIssueOpen,
        Some(pr) if pr.state.eq_ignore_ascii_case("superseded") => ChildIssueStatus::Superseded,
        Some(pr) if pr.state.eq_ignore_ascii_case("closed") => ChildIssueStatus::ClosedUnmerged,
        Some(_) => ChildIssueStatus::ActiveRun,
        None if issue_closed => ChildIssueStatus::Closed,
        None => ChildIssueStatus::Open,
    };
    ChildIssueState {
        issue_number: issue.number,
        terminal_state,
        pr_number: pr.map(|state| state.number),
    }
}

/// Select the first child that can still be worked in the validated order.
/// Closed children without completion evidence are skipped here and reported by
/// parent completion evaluation instead of being relaunched blindly.
/// Children missing from `states` are non-actionable here; callers must check
/// `missing_ordered_child_states` first when missing state is meaningful.
/// ActiveRun is intentionally not blocked here; launch_child_workflow rechecks
/// active leases before starting a run so recovery paths can still be selected.
pub fn next_actionable_child(states: &[ChildIssueState], order: &[u64]) -> Option<u64> {
    let states_by_number: HashMap<u64, &ChildIssueStatus> = states
        .iter()
        .map(|state| (state.issue_number, &state.terminal_state))
        .collect();
    order.iter().copied().find(|number| {
        states_by_number
            .get(number)
            .is_some_and(|state| !child_state_blocks_selection(state))
    })
}

pub fn missing_ordered_child_states(states: &[ChildIssueState], order: &[u64]) -> Vec<u64> {
    let state_numbers: BTreeSet<u64> = states.iter().map(|state| state.issue_number).collect();
    order
        .iter()
        .copied()
        .filter(|number| !state_numbers.contains(number))
        .collect()
}

/// ActiveRun intentionally does not block selection: launch_child_workflow
/// performs the active lease check before starting another run, so stale or
/// failed child run recovery can still proceed through the normal launch path.
fn child_state_blocks_selection(state: &ChildIssueStatus) -> bool {
    matches!(
        state,
        ChildIssueStatus::Merged
            | ChildIssueStatus::MergedIssueOpen
            | ChildIssueStatus::Closed
            | ChildIssueStatus::ClosedUnmerged
            | ChildIssueStatus::Superseded
            | ChildIssueStatus::Blocked,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::github_issues::SubIssueSource;

    fn github_issue(number: u64, state: &str) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("Issue {number}"),
            state: state.to_string(),
            labels: Vec::new(),
            assignees: vec![],
            milestone: None,
            body: None,
        }
    }

    #[test]
    fn order_subissues_uses_native_position_then_number() {
        let children = vec![
            GithubSubIssue {
                issue: github_issue(30, "open"),
                position: Some(2),
                source: SubIssueSource::Native,
            },
            GithubSubIssue {
                issue: github_issue(20, "open"),
                position: Some(1),
                source: SubIssueSource::Native,
            },
        ];
        assert_eq!(order_subissues(&children), vec![20, 30]);
    }

    #[test]
    fn order_subissues_sorts_missing_positions_last() {
        let children = vec![
            GithubSubIssue {
                issue: github_issue(30, "open"),
                position: None,
                source: SubIssueSource::Native,
            },
            GithubSubIssue {
                issue: github_issue(20, "open"),
                position: Some(1),
                source: SubIssueSource::Native,
            },
        ];
        assert_eq!(order_subissues(&children), vec![20, 30]);
    }

    #[test]
    fn next_actionable_child_skips_terminal_children() {
        let states = vec![
            ChildIssueState {
                issue_number: 1,
                terminal_state: ChildIssueStatus::Merged,
                pr_number: Some(10),
            },
            ChildIssueState {
                issue_number: 2,
                terminal_state: ChildIssueStatus::Open,
                pr_number: None,
            },
        ];
        assert_eq!(next_actionable_child(&states, &[1, 2]), Some(2));
    }

    #[test]
    fn next_actionable_child_selects_recoverable_failed_child_runs() {
        let states = vec![
            ChildIssueState {
                issue_number: 1,
                terminal_state: ChildIssueStatus::ClosedUnmerged,
                pr_number: Some(10),
            },
            ChildIssueState {
                issue_number: 2,
                terminal_state: ChildIssueStatus::Superseded,
                pr_number: Some(11),
            },
            ChildIssueState {
                issue_number: 3,
                terminal_state: ChildIssueStatus::FailedRun,
                pr_number: None,
            },
            ChildIssueState {
                issue_number: 4,
                terminal_state: ChildIssueStatus::Open,
                pr_number: None,
            },
        ];
        assert_eq!(next_actionable_child(&states, &[1, 2, 3, 4]), Some(3));
    }

    #[test]
    fn next_actionable_child_preserves_subissue_order_before_recovery_priority() {
        let states = vec![
            ChildIssueState {
                issue_number: 1,
                terminal_state: ChildIssueStatus::ActiveRun,
                pr_number: Some(10),
            },
            ChildIssueState {
                issue_number: 2,
                terminal_state: ChildIssueStatus::FailedRun,
                pr_number: None,
            },
        ];
        assert_eq!(next_actionable_child(&states, &[1, 2]), Some(1));
    }
}
