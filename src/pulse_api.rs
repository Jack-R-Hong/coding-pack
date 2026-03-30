//! Minimal Pulse Task API client.
//!
//! Board-specific operations have moved to plugin-board.
//! This module retains only `get_task()` for workspace resolution in lib.rs.

use pulse_plugin_sdk::error::WitPluginError;
use serde::Deserialize;

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
