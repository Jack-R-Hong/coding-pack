//! GitHub Issue-to-Board sync logic.
//!
//! Fetches open (and closed) issues from GitHub via `GitHubClient`, applies
//! label/milestone filters from `GitHubSyncConfig`, and creates or updates
//! board tasks via `board_client`.

use crate::board_client::BoardClient;
use crate::github_client::{GitHubClient, GitHubIssue};
use crate::workspace::{GitHubSyncConfig, WorkspaceConfig};
use pulse_plugin_sdk::error::WitPluginError;
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

/// Result counts from a sync run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncResult {
    pub created: u32,
    pub updated: u32,
    pub skipped: u32,
    pub closed: u32,
}

// ── Sync logic (native only) ─────────────────────────────────────────────────

/// Sync GitHub issues to the board.
///
/// 1. Fetches open issues and filters them with `matches_issue()`.
/// 2. For each matching issue, checks if a board task already exists (by
///    `issue_number` in metadata).  Creates new tasks or updates existing ones.
/// 3. Fetches closed issues and moves corresponding board tasks to `done`.
#[cfg(not(target_arch = "wasm32"))]
pub fn sync_issues_to_board(config: &WorkspaceConfig) -> Result<SyncResult, WitPluginError> {
    let client = GitHubClient::new()?;
    let board = BoardClient::new();
    let open_issues = client.list_issues("open", &[], None)?;
    let closed_issues: Vec<_> = client
        .list_issues("closed", &config.github_sync.filter_labels, config.github_sync.filter_milestone.as_deref())?
        .into_iter()
        .filter(|issue| matches_issue(issue, &config.github_sync))
        .collect();
    if !config.github_sync.filter_labels.is_empty() || config.github_sync.filter_milestone.is_some() {
        tracing::warn!(
            plugin = "coding-pack",
            closed_count = closed_issues.len(),
            "closed issues fetched with filter_labels/filter_milestone pre-filter; \
             only matching closed issues will be processed"
        );
    }

    let mut result = SyncResult::default();
    let sync_config = &config.github_sync;

    // ── Process open issues ──────────────────────────────────────────
    for issue in &open_issues {
        if !matches_issue(issue, sync_config) {
            tracing::debug!(
                plugin = "coding-pack",
                issue_number = issue.number,
                "skipping issue: does not match sync filter"
            );
            result.skipped += 1;
            continue;
        }

        let existing = board.find_task_by_issue_number(issue.number)?;

        match existing {
            Some(existing_task) => {
                // Update with current issue title/body
                if let Err(e) = board.update_task(
                    &existing_task.id,
                    &issue.title,
                    issue.body.as_deref(),
                ) {
                    tracing::warn!(issue_number = issue.number, error = %e, "Failed to update task");
                }
                result.updated += 1;
            }
            None => {
                // Create a new board task
                let metadata = serde_json::json!({
                    "issue_number": issue.number,
                    "issue_url": issue.html_url,
                    "labels": issue.labels.iter().map(|l| &l.name).collect::<Vec<_>>(),
                    "milestone": issue.milestone.as_ref().map(|m| &m.title),
                    "source": "github-sync"
                });
                board.create_task(&issue.title, issue.body.as_deref(), "ready-for-dev", &metadata)?;
                tracing::info!(
                    plugin = "coding-pack",
                    issue_number = issue.number,
                    "created board task from GitHub issue"
                );
                result.created += 1;
            }
        }
    }

    // ── Process closed issues ────────────────────────────────────────
    for issue in &closed_issues {
        if let Some(task) = board.find_task_by_issue_number(issue.number)? {
            if task.status != "done" {
                board.update_task_status(&task.id, "done")?;
                tracing::info!(
                    plugin = "coding-pack",
                    issue_number = issue.number,
                    task_id = %task.id,
                    "moved board task to done (issue closed)"
                );
                result.closed += 1;
            }
        }
    }

    tracing::info!(
        plugin = "coding-pack",
        created = result.created,
        updated = result.updated,
        skipped = result.skipped,
        closed = result.closed,
        "GitHub issue sync complete"
    );

    Ok(result)
}

/// WASM stub.
#[cfg(target_arch = "wasm32")]
pub fn sync_issues_to_board(_config: &WorkspaceConfig) -> Result<SyncResult, WitPluginError> {
    Err(WitPluginError::internal(
        "sync_issues_to_board is not available in WASM builds",
    ))
}

// ── Filter logic ─────────────────────────────────────────────────────────────

/// Check whether an issue matches the configured sync filters.
///
/// - If `filter_labels` is non-empty, the issue must have **all** listed labels
///   (AND logic).
/// - If `filter_milestone` is set, the issue's milestone title must match.
/// - Both filters are combined with AND logic.
/// - Empty/None filters always pass.
pub fn matches_issue(issue: &GitHubIssue, sync_config: &GitHubSyncConfig) -> bool {
    // Label filter: issue must have ALL configured labels
    if !sync_config.filter_labels.is_empty() {
        let issue_labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
        for required in &sync_config.filter_labels {
            if !issue_labels.contains(&required.as_str()) {
                return false;
            }
        }
    }

    // Milestone filter: issue must be in the configured milestone
    if let Some(ref required_milestone) = sync_config.filter_milestone {
        match &issue.milestone {
            Some(m) if m.title == *required_milestone => {}
            _ => return false,
        }
    }

    true
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_client::{GitHubLabel, GitHubMilestone};

    fn test_issue(number: u64, labels: &[&str], milestone: Option<&str>) -> GitHubIssue {
        GitHubIssue {
            number,
            title: format!("Test issue {number}"),
            body: None,
            labels: labels
                .iter()
                .map(|l| GitHubLabel {
                    name: l.to_string(),
                })
                .collect(),
            milestone: milestone.map(|t| GitHubMilestone {
                title: t.to_string(),
                number: 1,
            }),
            html_url: format!("https://github.com/test/repo/issues/{number}"),
            state: "open".to_string(),
            pull_request: None,
        }
    }

    // ── matches_issue ────────────────────────────────────────────────

    #[test]
    fn test_matches_issue_no_filters() {
        let config = GitHubSyncConfig::default();
        let issue = test_issue(1, &["bug"], Some("Sprint 1"));
        assert!(matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_label_filter_matches() {
        let config = GitHubSyncConfig {
            filter_labels: vec!["auto-dev".to_string()],
            ..Default::default()
        };
        let issue = test_issue(1, &["auto-dev", "bug"], None);
        assert!(matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_label_filter_no_match() {
        let config = GitHubSyncConfig {
            filter_labels: vec!["auto-dev".to_string()],
            ..Default::default()
        };
        let issue = test_issue(1, &["bug"], None);
        assert!(!matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_milestone_filter_matches() {
        let config = GitHubSyncConfig {
            filter_milestone: Some("Sprint 5".to_string()),
            ..Default::default()
        };
        let issue = test_issue(1, &[], Some("Sprint 5"));
        assert!(matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_milestone_filter_no_match() {
        let config = GitHubSyncConfig {
            filter_milestone: Some("Sprint 5".to_string()),
            ..Default::default()
        };
        let issue = test_issue(1, &[], Some("Sprint 3"));
        assert!(!matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_milestone_filter_no_milestone_on_issue() {
        let config = GitHubSyncConfig {
            filter_milestone: Some("Sprint 5".to_string()),
            ..Default::default()
        };
        let issue = test_issue(1, &[], None);
        assert!(!matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_both_filters_and_logic() {
        let config = GitHubSyncConfig {
            filter_labels: vec!["auto-dev".to_string()],
            filter_milestone: Some("Sprint 5".to_string()),
            ..Default::default()
        };

        // Both match
        let issue = test_issue(1, &["auto-dev", "bug"], Some("Sprint 5"));
        assert!(matches_issue(&issue, &config));

        // Label matches but milestone doesn't
        let issue = test_issue(2, &["auto-dev"], Some("Sprint 3"));
        assert!(!matches_issue(&issue, &config));

        // Milestone matches but label doesn't
        let issue = test_issue(3, &["bug"], Some("Sprint 5"));
        assert!(!matches_issue(&issue, &config));
    }

    #[test]
    fn test_matches_issue_multiple_labels_all_required() {
        let config = GitHubSyncConfig {
            filter_labels: vec!["auto-dev".to_string(), "priority".to_string()],
            ..Default::default()
        };

        // Has both required labels
        let issue = test_issue(1, &["auto-dev", "priority", "bug"], None);
        assert!(matches_issue(&issue, &config));

        // Missing one required label
        let issue = test_issue(2, &["auto-dev", "bug"], None);
        assert!(!matches_issue(&issue, &config));
    }

    // ── SyncResult ──────────────────────────────────────────────────

    #[test]
    fn test_sync_result_defaults() {
        let result = SyncResult::default();
        assert_eq!(result.created, 0);
        assert_eq!(result.updated, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.closed, 0);
    }

    #[test]
    fn test_sync_result_serialization() {
        let result = SyncResult {
            created: 3,
            updated: 1,
            skipped: 5,
            closed: 2,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["created"], 3);
        assert_eq!(json["updated"], 1);
        assert_eq!(json["skipped"], 5);
        assert_eq!(json["closed"], 2);
    }

    #[test]
    fn test_sync_result_deserialization() {
        let json = r#"{"created":1,"updated":2,"skipped":3,"closed":4}"#;
        let result: SyncResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.created, 1);
        assert_eq!(result.updated, 2);
        assert_eq!(result.skipped, 3);
        assert_eq!(result.closed, 4);
    }
}
