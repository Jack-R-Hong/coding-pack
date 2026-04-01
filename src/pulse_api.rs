//! Minimal Pulse Task API client.
//!
//! Board-specific operations have moved to plugin-board.
//! This module retains only `get_task()` for workspace resolution in lib.rs.

use pulse_plugin_sdk::error::WitPluginError;
use serde::{Deserialize, Serialize};

fn api_base() -> String {
    let port = std::env::var("PULSE_API_PORT").unwrap_or_else(|_| "8080".to_string());
    format!("http://127.0.0.1:{}/api/v1", port)
}

fn api_err(msg: impl std::fmt::Display) -> WitPluginError {
    WitPluginError::internal(format!("Pulse API error: {msg}"))
}

#[derive(Debug, Clone, Deserialize)]
pub struct PulseTask {
    pub id: String,
    #[serde(default)]
    pub workflow_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default, alias = "workspace")]
    pub workspace_id: String,
}

/// Snapshot of Pulse runtime metrics parsed from a Prometheus exposition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PulseMetricsSnapshot {
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub tasks_running: u64,
    pub total_llm_tokens: u64,
    pub total_cost_usd: f64,
    pub active_workflows: f64,
}

/// Parse a Prometheus text-format exposition into a [`PulseMetricsSnapshot`].
///
/// Lines beginning with `#` are skipped. Every other non-empty line is expected
/// to have the form `<metric_name_with_labels> <value>` (whitespace-separated).
pub fn parse_prometheus_text(body: &str) -> PulseMetricsSnapshot {
    let mut snap = PulseMetricsSnapshot::default();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split into at most two parts: "name{labels}" and "value"
        let mut parts = line.splitn(2, ' ');
        let name_with_labels = match parts.next() {
            Some(n) => n.trim(),
            None => continue,
        };
        let raw_value = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };

        // Parse the numeric value; skip lines that are not valid floats.
        let value: f64 = match raw_value.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        if name_with_labels.starts_with("pulse_tasks_total") {
            if name_with_labels.contains("state=\"completed\"") {
                snap.tasks_completed = value as u64;
            } else if name_with_labels.contains("state=\"failed\"") {
                snap.tasks_failed = value as u64;
            } else if name_with_labels.contains("state=\"running\"") {
                snap.tasks_running = value as u64;
            }
        } else if name_with_labels.starts_with("pulse_llm_tokens_total") {
            snap.total_llm_tokens += value as u64;
        } else if name_with_labels.starts_with("pulse_cost_usd_total") {
            snap.total_cost_usd += value;
        } else if name_with_labels == "pulse_active_workflows" {
            snap.active_workflows = value;
        }
    }

    snap
}

/// Fetch live metrics from the Pulse daemon and return a parsed snapshot.
pub fn fetch_pulse_metrics() -> Result<PulseMetricsSnapshot, WitPluginError> {
    let url = format!("{}/metrics", api_base());
    let body = reqwest::blocking::get(&url)
        .map_err(|e| api_err(format!("GET {url}: {e}")))?
        .text()
        .map_err(api_err)?;

    Ok(parse_prometheus_text(&body))
}

/// Parse a task from a JSON response body.
///
/// Handles two response shapes:
/// - `{ "task": { ... } }` — extracts the inner task object
/// - `{ "id": "...", ... }` — uses the value directly
pub fn parse_task_response(val: &serde_json::Value) -> Result<PulseTask, WitPluginError> {
    let task_val = if val.get("task").is_some() {
        val["task"].clone()
    } else {
        val.clone()
    };

    serde_json::from_value(task_val).map_err(|e| api_err(format!("deserialize task: {e}")))
}

/// Get a single task by ID (used for workspace resolution).
pub fn get_task(task_id: &str) -> Result<PulseTask, WitPluginError> {
    let url = format!("{}/tasks/{}", api_base(), task_id);
    let body = reqwest::blocking::get(&url)
        .map_err(|e| api_err(format!("GET {url}: {e}")))?
        .text()
        .map_err(api_err)?;

    let val: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| api_err(format!("parse: {e}")))?;

    parse_task_response(&val)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_prometheus_text ────────────────────────────────────────────────

    const SAMPLE_METRICS: &str = r#"
# HELP pulse_tasks_total Total tasks by state
# TYPE pulse_tasks_total counter
pulse_tasks_total{state="completed"} 42
pulse_tasks_total{state="failed"} 3
pulse_tasks_total{state="running"} 7
# HELP pulse_llm_tokens_total Total LLM tokens consumed
# TYPE pulse_llm_tokens_total counter
pulse_llm_tokens_total{model="gpt-4"} 1000
pulse_llm_tokens_total{model="claude-3"} 2500
# HELP pulse_cost_usd_total Cumulative cost in USD
# TYPE pulse_cost_usd_total counter
pulse_cost_usd_total{model="gpt-4"} 0.05
pulse_cost_usd_total{model="claude-3"} 0.12
# HELP pulse_active_workflows Currently active workflows
# TYPE pulse_active_workflows gauge
pulse_active_workflows 5
"#;

    #[test]
    fn parse_prometheus_text_full_sample() {
        let snap = parse_prometheus_text(SAMPLE_METRICS);
        assert_eq!(snap.tasks_completed, 42);
        assert_eq!(snap.tasks_failed, 3);
        assert_eq!(snap.tasks_running, 7);
        assert_eq!(snap.total_llm_tokens, 3500);
        assert!((snap.total_cost_usd - 0.17).abs() < 1e-9);
        assert_eq!(snap.active_workflows, 5.0);
    }

    #[test]
    fn parse_prometheus_text_empty_returns_all_zeros() {
        let snap = parse_prometheus_text("");
        assert_eq!(snap.tasks_completed, 0);
        assert_eq!(snap.tasks_failed, 0);
        assert_eq!(snap.tasks_running, 0);
        assert_eq!(snap.total_llm_tokens, 0);
        assert_eq!(snap.total_cost_usd, 0.0);
        assert_eq!(snap.active_workflows, 0.0);
    }

    #[test]
    fn parse_prometheus_text_partial_only_populates_matched_fields() {
        let partial = r#"
pulse_tasks_total{state="completed"} 10
pulse_active_workflows 2
"#;
        let snap = parse_prometheus_text(partial);
        assert_eq!(snap.tasks_completed, 10);
        assert_eq!(snap.tasks_failed, 0);   // not present
        assert_eq!(snap.tasks_running, 0);  // not present
        assert_eq!(snap.total_llm_tokens, 0);
        assert_eq!(snap.total_cost_usd, 0.0);
        assert_eq!(snap.active_workflows, 2.0);
    }

    #[test]
    fn parse_prometheus_text_skips_comment_and_blank_lines() {
        let input = "# comment\n\npulse_active_workflows 9\n";
        let snap = parse_prometheus_text(input);
        assert_eq!(snap.active_workflows, 9.0);
        assert_eq!(snap.tasks_completed, 0);
    }

    #[test]
    fn parse_prometheus_text_accumulates_multiple_llm_token_lines() {
        let input = "pulse_llm_tokens_total{model=\"a\"} 100\npulse_llm_tokens_total{model=\"b\"} 200\n";
        let snap = parse_prometheus_text(input);
        assert_eq!(snap.total_llm_tokens, 300);
    }

    #[test]
    fn parse_prometheus_text_accumulates_multiple_cost_lines() {
        let input = "pulse_cost_usd_total{model=\"a\"} 0.10\npulse_cost_usd_total{model=\"b\"} 0.20\n";
        let snap = parse_prometheus_text(input);
        assert!((snap.total_cost_usd - 0.30).abs() < 1e-9);
    }

    // ── existing tests ───────────────────────────────────────────────────────

    #[test]
    fn api_base_returns_default_port_when_env_not_set() {
        // Remove the env var to ensure default behavior
        std::env::remove_var("PULSE_API_PORT");
        let base = api_base();
        assert_eq!(base, "http://127.0.0.1:8080/api/v1");
    }

    #[test]
    fn api_base_reads_env_var() {
        std::env::set_var("PULSE_API_PORT", "9999");
        let base = api_base();
        assert_eq!(base, "http://127.0.0.1:9999/api/v1");
        // Clean up
        std::env::remove_var("PULSE_API_PORT");
    }

    #[test]
    fn parse_task_response_flat_object() {
        let json = serde_json::json!({
            "id": "task-1",
            "workflow_id": "wf-1",
            "state": "running",
            "workspace_id": "ws-1"
        });
        let task = parse_task_response(&json).unwrap();
        assert_eq!(task.id, "task-1");
        assert_eq!(task.workflow_id, "wf-1");
        assert_eq!(task.state, "running");
        assert_eq!(task.workspace_id, "ws-1");
    }

    #[test]
    fn parse_task_response_nested_task_key() {
        let json = serde_json::json!({
            "task": {
                "id": "task-2",
                "workflow_id": "wf-2",
                "state": "done",
                "workspace": "ws-2"
            }
        });
        let task = parse_task_response(&json).unwrap();
        assert_eq!(task.id, "task-2");
        assert_eq!(task.workspace_id, "ws-2");
    }

    #[test]
    fn parse_task_response_missing_id_returns_error() {
        let json = serde_json::json!({
            "workflow_id": "wf-1",
            "state": "running"
        });
        let result = parse_task_response(&json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_task_response_defaults_optional_fields() {
        let json = serde_json::json!({
            "id": "task-3"
        });
        let task = parse_task_response(&json).unwrap();
        assert_eq!(task.id, "task-3");
        assert_eq!(task.workflow_id, "");
        assert_eq!(task.state, "");
        assert_eq!(task.workspace_id, "");
    }
}
