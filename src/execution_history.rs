//! Execution history tracking for workflows.
//!
//! Stores execution records in `{base_dir}/config/execution-history.json`.
//! Each record captures the workflow ID, timestamp, and success/failure status.
//! This data feeds the dashboard workflow list and detail pages (last_run,
//! total_runs, success_rate columns).

use crate::workspace::WorkspaceConfig;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A single workflow execution record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionRecord {
    pub workflow_id: String,
    pub timestamp: String,
    pub success: bool,
}

/// Computed statistics for a single workflow.
#[derive(Debug, Clone)]
pub struct WorkflowStats {
    /// ISO 8601 timestamp of the most recent run, or None if never run.
    pub last_run: Option<String>,
    /// Total number of executions.
    pub total_runs: u64,
    /// Success rate formatted as "XX%" (e.g. "75%"), or "0%" if never run.
    pub success_rate: String,
}

/// Path to the execution history file for a workspace.
fn history_path(config: &WorkspaceConfig) -> PathBuf {
    config.base_dir.join("config/execution-history.json")
}

/// Load all execution records from disk.
/// Returns an empty vec if the file doesn't exist or can't be parsed.
pub fn load_execution_history(config: &WorkspaceConfig) -> Vec<ExecutionRecord> {
    let path = history_path(config);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Save a new execution record by appending to the history file.
/// Creates the file (and parent directories) if they don't exist.
pub fn save_execution_record(
    config: &WorkspaceConfig,
    record: &ExecutionRecord,
) -> Result<(), String> {
    let path = history_path(config);

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create config directory: {e}"))?;
    }

    // Load existing records, append new one, write back
    let mut records = load_execution_history(config);
    records.push(record.clone());

    let json = serde_json::to_string_pretty(&records)
        .map_err(|e| format!("cannot serialize execution history: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("cannot write execution history: {e}"))?;

    Ok(())
}

/// Compute stats (last_run, total_runs, success_rate) for a single workflow.
pub fn get_workflow_stats(history: &[ExecutionRecord], workflow_id: &str) -> WorkflowStats {
    let matching: Vec<&ExecutionRecord> = history
        .iter()
        .filter(|r| r.workflow_id == workflow_id)
        .collect();

    if matching.is_empty() {
        return WorkflowStats {
            last_run: None,
            total_runs: 0,
            success_rate: "0%".to_string(),
        };
    }

    let total = matching.len() as u64;
    let successes = matching.iter().filter(|r| r.success).count() as u64;
    let rate = if total > 0 {
        (successes * 100) / total
    } else {
        0
    };

    // Find the most recent timestamp (lexicographic comparison works for ISO 8601)
    let last_run = matching
        .iter()
        .map(|r| r.timestamp.as_str())
        .max()
        .map(|s| s.to_string());

    WorkflowStats {
        last_run,
        total_runs: total,
        success_rate: format!("{rate}%"),
    }
}

/// Get the last N execution records for a specific workflow, most recent first.
pub fn get_recent_executions<'a>(
    history: &'a [ExecutionRecord],
    workflow_id: &str,
    limit: usize,
) -> Vec<&'a ExecutionRecord> {
    let mut matching: Vec<&ExecutionRecord> = history
        .iter()
        .filter(|r| r.workflow_id == workflow_id)
        .collect();

    // Sort by timestamp descending (most recent first)
    matching.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    matching.truncate(limit);
    matching
}

/// Create a new execution record with the current UTC timestamp.
pub fn new_record(workflow_id: &str, success: bool) -> ExecutionRecord {
    ExecutionRecord {
        workflow_id: workflow_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        success,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_execution_history_no_file_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig::from_base_dir(tmp.path());
        let history = load_execution_history(&config);
        assert!(history.is_empty());
    }

    #[test]
    fn save_and_load_execution_record() {
        let tmp = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig::from_base_dir(tmp.path());

        let record = ExecutionRecord {
            workflow_id: "test-workflow".to_string(),
            timestamp: "2026-03-31T12:00:00+00:00".to_string(),
            success: true,
        };

        save_execution_record(&config, &record).unwrap();

        let history = load_execution_history(&config);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0], record);
    }

    #[test]
    fn save_execution_record_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig::from_base_dir(tmp.path());

        let r1 = ExecutionRecord {
            workflow_id: "wf-a".to_string(),
            timestamp: "2026-03-31T12:00:00+00:00".to_string(),
            success: true,
        };
        let r2 = ExecutionRecord {
            workflow_id: "wf-b".to_string(),
            timestamp: "2026-03-31T13:00:00+00:00".to_string(),
            success: false,
        };

        save_execution_record(&config, &r1).unwrap();
        save_execution_record(&config, &r2).unwrap();

        let history = load_execution_history(&config);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].workflow_id, "wf-a");
        assert_eq!(history[1].workflow_id, "wf-b");
    }

    #[test]
    fn save_execution_record_creates_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig::from_base_dir(tmp.path());

        // config/ directory should not exist yet
        assert!(!tmp.path().join("config").exists());

        let record = ExecutionRecord {
            workflow_id: "test".to_string(),
            timestamp: "2026-03-31T12:00:00+00:00".to_string(),
            success: true,
        };
        save_execution_record(&config, &record).unwrap();

        assert!(tmp.path().join("config/execution-history.json").exists());
    }

    #[test]
    fn get_workflow_stats_no_records() {
        let history: Vec<ExecutionRecord> = vec![];
        let stats = get_workflow_stats(&history, "nonexistent");
        assert_eq!(stats.last_run, None);
        assert_eq!(stats.total_runs, 0);
        assert_eq!(stats.success_rate, "0%");
    }

    #[test]
    fn get_workflow_stats_all_success() {
        let history = vec![
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T10:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T11:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T12:00:00+00:00".to_string(),
                success: true,
            },
        ];
        let stats = get_workflow_stats(&history, "wf-1");
        assert_eq!(stats.last_run, Some("2026-03-31T12:00:00+00:00".to_string()));
        assert_eq!(stats.total_runs, 3);
        assert_eq!(stats.success_rate, "100%");
    }

    #[test]
    fn get_workflow_stats_mixed_results() {
        let history = vec![
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T10:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T11:00:00+00:00".to_string(),
                success: false,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T12:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T13:00:00+00:00".to_string(),
                success: false,
            },
        ];
        let stats = get_workflow_stats(&history, "wf-1");
        assert_eq!(stats.last_run, Some("2026-03-31T13:00:00+00:00".to_string()));
        assert_eq!(stats.total_runs, 4);
        assert_eq!(stats.success_rate, "50%");
    }

    #[test]
    fn get_workflow_stats_filters_by_workflow_id() {
        let history = vec![
            ExecutionRecord {
                workflow_id: "wf-a".to_string(),
                timestamp: "2026-03-31T10:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-b".to_string(),
                timestamp: "2026-03-31T11:00:00+00:00".to_string(),
                success: false,
            },
            ExecutionRecord {
                workflow_id: "wf-a".to_string(),
                timestamp: "2026-03-31T12:00:00+00:00".to_string(),
                success: false,
            },
        ];
        let stats_a = get_workflow_stats(&history, "wf-a");
        assert_eq!(stats_a.total_runs, 2);
        assert_eq!(stats_a.success_rate, "50%");

        let stats_b = get_workflow_stats(&history, "wf-b");
        assert_eq!(stats_b.total_runs, 1);
        assert_eq!(stats_b.success_rate, "0%");
    }

    #[test]
    fn get_recent_executions_returns_limited_sorted() {
        let history = vec![
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T10:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T12:00:00+00:00".to_string(),
                success: false,
            },
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T11:00:00+00:00".to_string(),
                success: true,
            },
            ExecutionRecord {
                workflow_id: "wf-other".to_string(),
                timestamp: "2026-03-31T13:00:00+00:00".to_string(),
                success: true,
            },
        ];

        let recent = get_recent_executions(&history, "wf-1", 2);
        assert_eq!(recent.len(), 2);
        // Most recent first
        assert_eq!(recent[0].timestamp, "2026-03-31T12:00:00+00:00");
        assert_eq!(recent[1].timestamp, "2026-03-31T11:00:00+00:00");
    }

    #[test]
    fn get_recent_executions_returns_all_when_fewer_than_limit() {
        let history = vec![
            ExecutionRecord {
                workflow_id: "wf-1".to_string(),
                timestamp: "2026-03-31T10:00:00+00:00".to_string(),
                success: true,
            },
        ];

        let recent = get_recent_executions(&history, "wf-1", 10);
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn new_record_sets_current_timestamp() {
        let record = new_record("my-workflow", true);
        assert_eq!(record.workflow_id, "my-workflow");
        assert!(record.success);
        // Timestamp should be a valid RFC 3339 string and roughly current
        assert!(!record.timestamp.is_empty());
        assert!(record.timestamp.contains("T"));
    }

    #[test]
    fn load_execution_history_handles_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig::from_base_dir(tmp.path());

        // Write corrupt JSON
        let path = tmp.path().join("config");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("execution-history.json"), "not valid json{{{").unwrap();

        let history = load_execution_history(&config);
        assert!(history.is_empty());
    }
}
