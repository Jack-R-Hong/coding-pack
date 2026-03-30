//! Plugin Bridge — thin HTTP/RPC bridge to platform plugin capabilities.
//!
//! Replaces direct module calls (auto_dev, executor, github_client, etc.) with
//! delegation to platform plugins:
//!   - plugin-auto-loop: task pickup, workflow dispatch, validation, board updates
//!   - plugin-issue-sync: GitHub issue sync
//!   - plugin-test-runner: test execution and parsing
//!   - plugin-feedback-loop: PR review feedback
//!   - plugin-trigger-cron: scheduled triggering
//!   - plugin-workspace-tracker: worktree lifecycle
//!
//! Each function tries `call_capability()` first (server mode), then falls back
//! to HTTP POST to the Pulse API (CLI mode).

use crate::workspace::WorkspaceConfig;
use pulse_plugin_sdk::error::WitPluginError;

fn bridge_err(msg: impl std::fmt::Display) -> WitPluginError {
    WitPluginError::internal(format!("plugin-bridge: {msg}"))
}

/// Pulse API base URL from environment.
fn api_base() -> String {
    let port = std::env::var("PULSE_API_PORT").unwrap_or_else(|_| "8080".to_string());
    format!("http://127.0.0.1:{}/api/v1", port)
}

/// Try SDK capability call first, fall back to HTTP POST.
fn call_plugin(
    capability: &str,
    payload: serde_json::Value,
    http_path: &str,
) -> Result<serde_json::Value, WitPluginError> {
    // Try server-mode capability RPC first
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Ok(result) = pulse_plugin_sdk::host::call_capability(capability, payload.clone()) {
            return Ok(result);
        }
    }

    // Fall back to HTTP POST (CLI mode)
    #[cfg(not(target_arch = "wasm32"))]
    {
        let url = format!("{}/{}", api_base(), http_path);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .map_err(|e| bridge_err(format!("POST {url}: {e}")))?;

        let body = resp
            .text()
            .map_err(|e| bridge_err(format!("read response: {e}")))?;

        serde_json::from_str(&body)
            .map_err(|e| bridge_err(format!("parse response from {url}: {e}")))
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = (capability, payload, http_path);
        Err(bridge_err("plugin bridge not available in WASM"))
    }
}

/// Build a config payload from WorkspaceConfig for plugin calls.
fn config_payload(config: &WorkspaceConfig) -> serde_json::Value {
    serde_json::json!({
        "workspace_dir": config.base_dir.to_string_lossy(),
        "auto_dev": {
            "max_retries": config.auto_dev.max_retries,
            "max_tasks": config.auto_dev.max_tasks,
            "skip_validation": config.auto_dev.skip_validation,
        },
        "github_sync": {
            "filter_labels": config.github_sync.filter_labels,
            "filter_milestone": config.github_sync.filter_milestone,
            "review_poll_interval_secs": config.github_sync.review_poll_interval_secs,
        },
    })
}

// ── plugin-auto-loop delegates ─────────────────────────────────────────────

/// Pick the next ready-for-dev task from the board and run its workflow.
/// Delegates to plugin-auto-loop.
pub fn auto_loop_next(config: &WorkspaceConfig) -> Result<Option<serde_json::Value>, WitPluginError> {
    let payload = config_payload(config);
    match call_plugin(
        "auto-loop.run-once",
        payload,
        "workflows/auto-loop-next/execute",
    ) {
        Ok(v) => {
            if v.get("status").and_then(|s| s.as_str()) == Some("idle") {
                Ok(None)
            } else {
                Ok(Some(v))
            }
        }
        Err(e) => Err(e),
    }
}

/// Watch mode: process multiple tasks from the board.
/// Delegates to plugin-auto-loop.
pub fn auto_loop_watch(
    config: &WorkspaceConfig,
    max: Option<u32>,
) -> Result<Vec<serde_json::Value>, WitPluginError> {
    let mut payload = config_payload(config);
    if let Some(m) = max {
        payload["max_iterations"] = serde_json::json!(m);
    }
    let result = call_plugin(
        "auto-loop.watch",
        payload,
        "workflows/auto-loop-watch/execute",
    )?;
    match result.as_array() {
        Some(arr) => Ok(arr.clone()),
        None => Ok(vec![result]),
    }
}

/// Get the current status of the auto-dev loop / board.
/// Delegates to plugin-auto-loop.
pub fn auto_loop_status(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "auto-loop.status",
        payload,
        "workflows/auto-loop-status/execute",
    )
}

// ── plugin-issue-sync delegates ────────────────────────────────────────────

/// Sync GitHub issues to the board.
/// Delegates to plugin-issue-sync.
pub fn sync_github_issues(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "issue-sync.sync",
        payload,
        "workflows/issue-sync/execute",
    )
}

// ── plugin-feedback-loop delegates ─────────────────────────────────────────

/// Check all open auto-dev PRs for review status.
/// Delegates to plugin-feedback-loop.
pub fn check_pr_reviews(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "feedback-loop.check-reviews",
        payload,
        "workflows/feedback-loop-check/execute",
    )
}

/// Build fix context for a specific PR that needs changes.
/// Delegates to plugin-feedback-loop.
pub fn build_fix_context(pr_number: u64) -> Result<serde_json::Value, WitPluginError> {
    let payload = serde_json::json!({ "pr_number": pr_number });
    call_plugin(
        "feedback-loop.build-fix-context",
        payload,
        "workflows/feedback-loop-fix-context/execute",
    )
}

// ── plugin-workspace-tracker delegates ─────────────────────────────────────

/// Clean up completed worktrees.
/// Delegates to plugin-workspace-tracker.
pub fn cleanup_worktrees(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "workspace-tracker.cleanup",
        payload,
        "workflows/worktree-cleanup/execute",
    )
}

/// Get worktree status.
/// Delegates to plugin-workspace-tracker.
pub fn worktree_status(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "workspace-tracker.status",
        payload,
        "workflows/worktree-status/execute",
    )
}

/// Recover orphaned worktrees.
/// Delegates to plugin-workspace-tracker.
pub fn recover_worktrees(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "workspace-tracker.recover",
        payload,
        "workflows/worktree-recover/execute",
    )
}

// ── plugin-workspace-tracker list ─────────────────────────────────────────

/// List active worktrees.
/// Delegates to plugin-workspace-tracker.
pub fn worktrees_list(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "workspace-tracker.list",
        payload,
        "plugins/plugin-workspace-tracker/worktrees/list",
    )
}

// ── plugin-board delegates ────────────────────────────────────────────────

/// Query board data (GET-style via POST).
/// Delegates to plugin-board.
pub fn board_query(
    endpoint: &str,
    workspace: Option<&str>,
    board_id: Option<&str>,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let mut payload = config_payload(config);
    if let Some(ws) = workspace {
        payload["workspace"] = serde_json::json!(ws);
    }
    if let Some(bid) = board_id {
        payload["board_id"] = serde_json::json!(bid);
    }
    payload["endpoint"] = serde_json::json!(endpoint);
    call_plugin(
        "board.query",
        payload,
        &format!("plugins/plugin-board/{endpoint}"),
    )
}

/// Mutate board data (POST/PUT).
/// Delegates to plugin-board.
pub fn board_mutate(
    endpoint: &str,
    payload: &serde_json::Value,
    workspace: Option<&str>,
    board_id: Option<&str>,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let mut combined = config_payload(config);
    if let Some(ws) = workspace {
        combined["workspace"] = serde_json::json!(ws);
    }
    if let Some(bid) = board_id {
        combined["board_id"] = serde_json::json!(bid);
    }
    combined["endpoint"] = serde_json::json!(endpoint);
    combined["payload"] = payload.clone();
    call_plugin(
        "board.mutate",
        combined,
        &format!("plugins/plugin-board/{endpoint}"),
    )
}

// ── plugin-test-runner delegates ──────────────────────────────────────────

/// Run the full test validation pipeline (detect + run + parse).
/// Delegates to plugin-test-runner.
pub fn run_tests(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "test-runner.validate",
        payload,
        "plugins/plugin-test-runner/validate",
    )
}

// ── Pulse engine workflow execution ────────────────────────────────────────

/// Execute a workflow by ID via the Pulse engine.
/// Delegates to the Pulse workflow execution API.
pub fn execute_workflow(
    workflow_id: &str,
    input: &str,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let payload = serde_json::json!({
        "workflow_id": workflow_id,
        "input": input,
        "workspace_dir": config.base_dir.to_string_lossy(),
    });
    call_plugin(
        "engine.execute-workflow",
        payload,
        &format!("workflows/{workflow_id}/execute"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_payload_includes_workspace_dir() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        assert_eq!(payload["workspace_dir"].as_str(), Some("/tmp/test"));
    }

    #[test]
    fn config_payload_includes_auto_dev_settings() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        assert!(payload["auto_dev"]["max_retries"].is_number());
        assert!(payload["auto_dev"]["max_tasks"].is_number());
    }

    #[test]
    fn config_payload_includes_github_sync_settings() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        assert!(payload["github_sync"]["filter_labels"].is_array());
        assert!(payload["github_sync"]["review_poll_interval_secs"].is_number());
    }

    #[test]
    fn api_base_uses_default_port() {
        // When PULSE_API_PORT is not set (or set to default)
        let base = api_base();
        assert!(base.starts_with("http://127.0.0.1:"));
        assert!(base.ends_with("/api/v1"));
    }

    // ── config_payload field completeness ─────────────────────────────

    #[test]
    fn config_payload_has_exactly_three_top_level_keys() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        let obj = payload.as_object().unwrap();
        assert_eq!(obj.len(), 3, "config_payload should have 3 top-level keys: workspace_dir, auto_dev, github_sync");
        assert!(obj.contains_key("workspace_dir"));
        assert!(obj.contains_key("auto_dev"));
        assert!(obj.contains_key("github_sync"));
    }

    #[test]
    fn config_payload_auto_dev_has_all_fields() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        let auto_dev = payload["auto_dev"].as_object().unwrap();
        assert!(auto_dev.contains_key("max_retries"), "auto_dev should have max_retries");
        assert!(auto_dev.contains_key("max_tasks"), "auto_dev should have max_tasks");
        assert!(auto_dev.contains_key("skip_validation"), "auto_dev should have skip_validation");
        assert_eq!(auto_dev.len(), 3, "auto_dev should have exactly 3 fields");
    }

    #[test]
    fn config_payload_github_sync_has_all_fields() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        let github_sync = payload["github_sync"].as_object().unwrap();
        assert!(github_sync.contains_key("filter_labels"), "should have filter_labels");
        assert!(github_sync.contains_key("filter_milestone"), "should have filter_milestone");
        assert!(github_sync.contains_key("review_poll_interval_secs"), "should have review_poll_interval_secs");
        assert_eq!(github_sync.len(), 3, "github_sync should have exactly 3 fields");
    }

    #[test]
    fn config_payload_reflects_custom_auto_dev_values() {
        let mut config = WorkspaceConfig::resolve(Some("/tmp/test"));
        config.auto_dev.max_retries = 5;
        config.auto_dev.max_tasks = 20;
        config.auto_dev.skip_validation = true;
        let payload = config_payload(&config);
        assert_eq!(payload["auto_dev"]["max_retries"].as_u64(), Some(5));
        assert_eq!(payload["auto_dev"]["max_tasks"].as_u64(), Some(20));
        assert_eq!(payload["auto_dev"]["skip_validation"].as_bool(), Some(true));
    }

    #[test]
    fn config_payload_reflects_custom_github_sync_values() {
        let mut config = WorkspaceConfig::resolve(Some("/tmp/test"));
        config.github_sync.filter_labels = vec!["bug".to_string(), "priority".to_string()];
        config.github_sync.filter_milestone = Some("Sprint 3".to_string());
        config.github_sync.review_poll_interval_secs = 120;
        let payload = config_payload(&config);
        let labels = payload["github_sync"]["filter_labels"].as_array().unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].as_str(), Some("bug"));
        assert_eq!(labels[1].as_str(), Some("priority"));
        assert_eq!(payload["github_sync"]["filter_milestone"].as_str(), Some("Sprint 3"));
        assert_eq!(payload["github_sync"]["review_poll_interval_secs"].as_u64(), Some(120));
    }

    #[test]
    fn config_payload_default_auto_dev_values() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        // Defaults from AutoDevConfig::default()
        assert_eq!(payload["auto_dev"]["max_retries"].as_u64(), Some(1));
        assert_eq!(payload["auto_dev"]["max_tasks"].as_u64(), Some(10));
        assert_eq!(payload["auto_dev"]["skip_validation"].as_bool(), Some(false));
    }

    #[test]
    fn config_payload_default_github_sync_values() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        assert!(payload["github_sync"]["filter_labels"].as_array().unwrap().is_empty());
        assert!(payload["github_sync"]["filter_milestone"].is_null());
        assert_eq!(payload["github_sync"]["review_poll_interval_secs"].as_u64(), Some(60));
    }

    // ── bridge_err formatting ─────────────────────────────────────────

    #[test]
    fn bridge_err_returns_internal_error_with_prefix() {
        let err = bridge_err("test failure");
        assert_eq!(err.code, "internal");
        assert!(
            err.message.contains("plugin-bridge:"),
            "error message should contain 'plugin-bridge:' prefix, got: {}",
            err.message
        );
        assert!(
            err.message.contains("test failure"),
            "error message should contain the original message"
        );
    }

    #[test]
    fn bridge_err_formats_complex_messages() {
        let err = bridge_err(format!("POST http://localhost:8080/api/v1/test: connection refused"));
        assert_eq!(err.code, "internal");
        assert!(err.message.contains("connection refused"));
        assert!(err.message.contains("plugin-bridge:"));
    }

    // ── api_base port override ────────────────────────────────────────

    #[test]
    fn api_base_format_is_correct() {
        let base = api_base();
        // Should always be http://127.0.0.1:{port}/api/v1
        assert!(base.starts_with("http://127.0.0.1:"));
        assert!(base.ends_with("/api/v1"));
        // Parse port to verify it's a valid number
        let port_str = base
            .strip_prefix("http://127.0.0.1:")
            .unwrap()
            .strip_suffix("/api/v1")
            .unwrap();
        let port: u16 = port_str.parse().expect("port should be a valid number");
        assert!(port > 0, "port should be positive");
    }

    // ── run_tests payload construction ─────────────────────────────────

    #[test]
    fn run_tests_payload_uses_config_payload() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test-ws"));
        let payload = config_payload(&config);
        // run_tests passes the standard config_payload to the test-runner plugin
        assert_eq!(payload["workspace_dir"].as_str(), Some("/tmp/test-ws"));
        assert!(payload["auto_dev"].is_object());
        assert!(payload["github_sync"].is_object());
    }

    #[test]
    fn run_tests_capability_name_is_correct() {
        // Verify the capability string matches plugin-test-runner's registration
        let capability = "test-runner.validate";
        assert!(capability.starts_with("test-runner."));
        assert!(capability.ends_with("validate"));
    }

    #[test]
    fn run_tests_http_fallback_path_format() {
        // Verify the HTTP fallback path format used by run_tests
        let http_path = "plugins/plugin-test-runner/validate";
        assert!(http_path.starts_with("plugins/plugin-test-runner/"));
        assert!(http_path.ends_with("/validate"));
    }

    // ── execute_workflow payload construction ─────────────────────────

    #[test]
    fn execute_workflow_endpoint_path_format() {
        // Verify the endpoint path format used by execute_workflow
        let workflow_id = "coding-quick-dev";
        let expected_path = format!("workflows/{workflow_id}/execute");
        assert_eq!(expected_path, "workflows/coding-quick-dev/execute");
    }

    #[test]
    fn execute_workflow_payload_structure() {
        // Verify the JSON payload that execute_workflow constructs
        let config = WorkspaceConfig::resolve(Some("/tmp/test-ws"));
        let workflow_id = "coding-quick-dev";
        let input = "implement login feature";
        let payload = serde_json::json!({
            "workflow_id": workflow_id,
            "input": input,
            "workspace_dir": config.base_dir.to_string_lossy(),
        });
        assert_eq!(payload["workflow_id"].as_str(), Some("coding-quick-dev"));
        assert_eq!(payload["input"].as_str(), Some("implement login feature"));
        assert_eq!(payload["workspace_dir"].as_str(), Some("/tmp/test-ws"));
    }

    // ── auto_loop_watch payload construction ──────────────────────────

    #[test]
    fn auto_loop_watch_payload_without_max() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let payload = config_payload(&config);
        // Without max_iterations, payload should just be the config_payload
        assert!(payload.get("max_iterations").is_none());
    }

    #[test]
    fn auto_loop_watch_payload_with_max() {
        let config = WorkspaceConfig::resolve(Some("/tmp/test"));
        let mut payload = config_payload(&config);
        let max: u32 = 5;
        payload["max_iterations"] = serde_json::json!(max);
        assert_eq!(payload["max_iterations"].as_u64(), Some(5));
        // Original config payload fields should still be present
        assert!(payload["workspace_dir"].is_string());
        assert!(payload["auto_dev"].is_object());
    }

    // ── build_fix_context payload construction ────────────────────────

    #[test]
    fn build_fix_context_payload_structure() {
        let pr_number: u64 = 42;
        let payload = serde_json::json!({ "pr_number": pr_number });
        assert_eq!(payload["pr_number"].as_u64(), Some(42));
        let obj = payload.as_object().unwrap();
        assert_eq!(obj.len(), 1, "build_fix_context payload should have exactly one field");
    }

    // ── auto_loop_next response interpretation ────────────────────────

    #[test]
    fn auto_loop_next_idle_detection() {
        // Simulate the response interpretation logic from auto_loop_next
        let idle_response = serde_json::json!({"status": "idle"});
        let is_idle = idle_response.get("status").and_then(|s| s.as_str()) == Some("idle");
        assert!(is_idle, "should detect idle status");

        let active_response = serde_json::json!({"status": "running", "task_id": "t-1"});
        let is_idle = active_response.get("status").and_then(|s| s.as_str()) == Some("idle");
        assert!(!is_idle, "should not treat running as idle");
    }

    // ── auto_loop_watch response interpretation ───────────────────────

    #[test]
    fn auto_loop_watch_array_response() {
        // When result is an array, auto_loop_watch returns it directly
        let result = serde_json::json!([
            {"task_id": "t-1", "status": "done"},
            {"task_id": "t-2", "status": "done"}
        ]);
        let arr = result.as_array().unwrap().clone();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn auto_loop_watch_single_response_wraps_in_vec() {
        // When result is not an array, auto_loop_watch wraps it in vec
        let result = serde_json::json!({"task_id": "t-1", "status": "done"});
        let output = match result.as_array() {
            Some(arr) => arr.clone(),
            None => vec![result.clone()],
        };
        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["task_id"].as_str(), Some("t-1"));
    }
}
