//! Auto-dev loop integration — PR review checking, task generation, and issue linking.

use serde::{Deserialize, Serialize};

use pulse_plugin_sdk::error::WitPluginError;

#[cfg(not(target_arch = "wasm32"))]
use crate::github_client::{aggregate_review_state, is_auto_dev_pr, GitHubClient};

/// A task assignment generated from PR review feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrFixAssignment {
    pub id: String,
    pub title: String,
    pub status: String,
    pub workflow_id: String,
    pub priority: String,
    pub labels: Vec<String>,
    pub pr_number: u64,
    pub pr_branch: String,
    pub pr_url: String,
}

/// Template variables for issue-linked PRs.
#[derive(Debug, Clone, Serialize)]
pub struct IssueTemplateVars {
    pub issue_number: Option<u64>,
    pub issue_url: Option<String>,
    pub issue_closing_ref: String,
}

/// Check open PRs for pending review feedback. Returns PR fix assignments
/// for PRs with "changes_requested" status.
///
/// Graceful degradation: no GITHUB_TOKEN → Ok(vec![]); API failure → log warning, return Ok(vec![])
/// FIFO ordering: lowest PR number picked first.
#[cfg(not(target_arch = "wasm32"))]
pub fn check_pending_reviews() -> Result<Vec<PrFixAssignment>, WitPluginError> {
    let client = match GitHubClient::new() {
        Ok(c) => c,
        Err(_) => {
            tracing::debug!(
                plugin = "coding-pack",
                "No GitHub token available — skipping PR review check"
            );
            return Ok(vec![]);
        }
    };

    let prs = match client.list_open_prs() {
        Ok(prs) => prs,
        Err(e) => {
            tracing::warn!(
                plugin = "coding-pack",
                error = %e,
                "Failed to list open PRs — skipping PR review check"
            );
            return Ok(vec![]);
        }
    };

    let auto_dev_prs: Vec<_> = prs.into_iter().filter(is_auto_dev_pr).collect();

    let mut assignments = Vec::new();

    for pr in auto_dev_prs {
        let reviews = match client.list_pr_reviews(pr.number) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    plugin = "coding-pack",
                    pr_number = pr.number,
                    error = %e,
                    "Failed to list reviews for PR — skipping"
                );
                continue;
            }
        };

        let state = aggregate_review_state(&reviews);

        if state == "changes_requested" {
            let branch = pr.head.ref_field.clone();
            let pr_url = pr.html_url.clone();

            assignments.push(PrFixAssignment {
                id: format!("pr-fix-{}", pr.number),
                title: format!("Fix PR #{}: {}", pr.number, pr.title),
                status: "ready-for-dev".to_string(),
                workflow_id: "coding-pr-fix".to_string(),
                priority: "critical".to_string(),
                labels: vec!["pr-fix".to_string()],
                pr_number: pr.number,
                pr_branch: branch,
                pr_url,
            });
        }
    }

    // FIFO: sort by PR number ascending (oldest first)
    assignments.sort_by_key(|a| a.pr_number);

    Ok(assignments)
}

#[cfg(target_arch = "wasm32")]
pub fn check_pending_reviews() -> Result<Vec<PrFixAssignment>, WitPluginError> {
    Ok(vec![])
}

/// Pick the next task to work on. PR fix tasks have priority over board tasks.
///
/// 1. Check pending PR reviews first — if any, return the first (FIFO)
/// 2. If no PR tasks, fall back to board task picking (returns None, let caller handle board)
pub fn pick_next_task() -> Result<Option<PrFixAssignment>, WitPluginError> {
    let pr_fixes = check_pending_reviews()?;
    Ok(pr_fixes.into_iter().next())
}

/// Build template variables for issue-linked PRs.
///
/// If task metadata contains issue_number, returns variables with Closes #N ref.
/// If no issue_number, returns empty closing ref (graceful fallback).
pub fn build_issue_template_vars(task_metadata: &serde_json::Value) -> IssueTemplateVars {
    let issue_number = task_metadata
        .get("issue_number")
        .and_then(|v| v.as_u64());

    let issue_url = task_metadata
        .get("issue_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let issue_closing_ref = match issue_number {
        Some(n) => format!("Closes #{}", n),
        None => String::new(),
    };

    IssueTemplateVars {
        issue_number,
        issue_url,
        issue_closing_ref,
    }
}

/// After a PR fix workflow succeeds, re-request review from original reviewers.
///
/// Graceful degradation: failures are logged but don't cause errors.
#[cfg(not(target_arch = "wasm32"))]
pub fn re_request_review_for_pr_fix(pr_number: u64) -> Result<(), WitPluginError> {
    let client = match GitHubClient::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                plugin = "coding-pack",
                pr_number = pr_number,
                error = %e,
                "Cannot re-request review — no GitHub client available"
            );
            return Ok(());
        }
    };

    // Get PR details to find requested_reviewers
    let pr = match client.get_pull_request(pr_number) {
        Ok(pr) => pr,
        Err(e) => {
            tracing::warn!(
                plugin = "coding-pack",
                pr_number = pr_number,
                error = %e,
                "Failed to fetch PR for re-request review — fix commits were pushed successfully"
            );
            return Ok(());
        }
    };

    // Collect reviewers from PR's requested_reviewers field
    let mut reviewer_logins: Vec<String> = pr
        .requested_reviewers
        .iter()
        .map(|r| r.login.clone())
        .collect();

    // Also check previous reviewers from reviews who requested changes
    if let Ok(reviews) = client.list_pr_reviews(pr_number) {
        for review in &reviews {
            if review.state == "CHANGES_REQUESTED" {
                let login = review.user.login.clone();
                if !login.is_empty() && !reviewer_logins.contains(&login) {
                    reviewer_logins.push(login);
                }
            }
        }
    }

    if reviewer_logins.is_empty() {
        tracing::debug!(
            plugin = "coding-pack",
            pr_number = pr_number,
            "No reviewers to re-request — skipping"
        );
        return Ok(());
    }

    if let Err(e) = client.request_reviewers(pr_number, &reviewer_logins) {
        tracing::warn!(
            plugin = "coding-pack",
            pr_number = pr_number,
            error = %e,
            "Failed to re-request review — fix commits were pushed successfully"
        );
    } else {
        tracing::info!(
            plugin = "coding-pack",
            pr_number = pr_number,
            reviewers = ?reviewer_logins,
            "Re-requested review after PR fix"
        );
    }

    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn re_request_review_for_pr_fix(_pr_number: u64) -> Result<(), WitPluginError> {
    Ok(())
}

/// Get the workflow ID for a task based on its labels.
pub fn route_task_to_workflow(labels: &[String]) -> &'static str {
    for label in labels {
        match label.as_str() {
            "story" => return "coding-story-dev",
            "bug" => return "coding-bug-fix",
            "refactor" => return "coding-refactor",
            "feature" => return "coding-feature-dev",
            "review" => return "coding-review",
            "pr-fix" => return "coding-pr-fix",
            _ => continue,
        }
    }
    "coding-quick-dev"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_fix_assignment_construction() {
        let assignment = PrFixAssignment {
            id: "pr-fix-42".to_string(),
            title: "Fix PR #42: Add login page".to_string(),
            status: "ready-for-dev".to_string(),
            workflow_id: "coding-pr-fix".to_string(),
            priority: "critical".to_string(),
            labels: vec!["pr-fix".to_string()],
            pr_number: 42,
            pr_branch: "auto-dev/task-1".to_string(),
            pr_url: "https://github.com/owner/repo/pull/42".to_string(),
        };
        assert_eq!(assignment.id, "pr-fix-42");
        assert_eq!(assignment.priority, "critical");
        assert_eq!(assignment.workflow_id, "coding-pr-fix");
    }

    #[test]
    fn pr_fix_assignment_serialization_roundtrip() {
        let assignment = PrFixAssignment {
            id: "pr-fix-1".to_string(),
            title: "Fix PR #1: test".to_string(),
            status: "ready-for-dev".to_string(),
            workflow_id: "coding-pr-fix".to_string(),
            priority: "critical".to_string(),
            labels: vec!["pr-fix".to_string()],
            pr_number: 1,
            pr_branch: "auto-dev/t-1".to_string(),
            pr_url: "https://github.com/o/r/pull/1".to_string(),
        };
        let json = serde_json::to_string(&assignment).unwrap();
        let deserialized: PrFixAssignment = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pr_number, 1);
        assert_eq!(deserialized.id, "pr-fix-1");
    }

    #[test]
    fn issue_template_vars_with_issue() {
        let metadata = serde_json::json!({
            "issue_number": 42,
            "issue_url": "https://github.com/owner/repo/issues/42"
        });
        let vars = build_issue_template_vars(&metadata);
        assert_eq!(vars.issue_number, Some(42));
        assert_eq!(vars.issue_closing_ref, "Closes #42");
        assert_eq!(
            vars.issue_url.as_deref(),
            Some("https://github.com/owner/repo/issues/42")
        );
    }

    #[test]
    fn issue_template_vars_without_issue() {
        let metadata = serde_json::json!({});
        let vars = build_issue_template_vars(&metadata);
        assert_eq!(vars.issue_number, None);
        assert_eq!(vars.issue_closing_ref, "");
        assert!(vars.issue_url.is_none());
    }

    #[test]
    fn issue_template_vars_with_only_number() {
        let metadata = serde_json::json!({"issue_number": 7});
        let vars = build_issue_template_vars(&metadata);
        assert_eq!(vars.issue_number, Some(7));
        assert_eq!(vars.issue_closing_ref, "Closes #7");
        assert!(vars.issue_url.is_none());
    }

    #[test]
    fn route_story_label() {
        assert_eq!(
            route_task_to_workflow(&["story".into()]),
            "coding-story-dev"
        );
    }

    #[test]
    fn route_bug_label() {
        assert_eq!(route_task_to_workflow(&["bug".into()]), "coding-bug-fix");
    }

    #[test]
    fn route_pr_fix_label() {
        assert_eq!(
            route_task_to_workflow(&["pr-fix".into()]),
            "coding-pr-fix"
        );
    }

    #[test]
    fn route_default_no_labels() {
        assert_eq!(route_task_to_workflow(&[]), "coding-quick-dev");
    }

    #[test]
    fn route_unknown_label_defaults() {
        assert_eq!(
            route_task_to_workflow(&["unknown".into()]),
            "coding-quick-dev"
        );
    }

    #[test]
    fn route_first_matching_label_wins() {
        assert_eq!(
            route_task_to_workflow(&["feature".into(), "bug".into()]),
            "coding-feature-dev"
        );
    }

    #[test]
    fn pick_next_task_returns_none_when_no_pr_fixes() {
        // pick_next_task calls check_pending_reviews which needs GitHubClient
        // Without GITHUB_TOKEN, it gracefully returns empty vec, so pick_next_task returns None
        let result = pick_next_task().unwrap();
        assert!(result.is_none(), "should return None when no GitHub token is available");
    }

    #[test]
    fn pr_fix_assignment_has_correct_defaults() {
        let assignment = PrFixAssignment {
            id: "pr-fix-99".to_string(),
            title: "Fix PR #99: test".to_string(),
            status: "ready-for-dev".to_string(),
            workflow_id: "coding-pr-fix".to_string(),
            priority: "critical".to_string(),
            labels: vec!["pr-fix".to_string()],
            pr_number: 99,
            pr_branch: "auto-dev/task-99".to_string(),
            pr_url: "https://github.com/o/r/pull/99".to_string(),
        };
        // Verify all expected field values per spec
        assert_eq!(assignment.status, "ready-for-dev");
        assert_eq!(assignment.workflow_id, "coding-pr-fix");
        assert_eq!(assignment.priority, "critical");
        assert!(assignment.labels.contains(&"pr-fix".to_string()));
        assert!(assignment.id.starts_with("pr-fix-"));
    }

    #[test]
    fn route_refactor_label() {
        assert_eq!(route_task_to_workflow(&["refactor".into()]), "coding-refactor");
    }

    #[test]
    fn route_feature_label() {
        assert_eq!(route_task_to_workflow(&["feature".into()]), "coding-feature-dev");
    }

    #[test]
    fn route_review_label() {
        assert_eq!(route_task_to_workflow(&["review".into()]), "coding-review");
    }
}
