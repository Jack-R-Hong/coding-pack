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

// ── plugin-board delegates ─────────────────────────────────────────────────

/// Try SDK capability call first (POST payload via RPC), fall back to HTTP GET.
/// Used for board data queries which are semantically GET operations.
fn get_from_plugin(
    capability: &str,
    payload: serde_json::Value,
    http_get_path: &str,
) -> Result<serde_json::Value, WitPluginError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Ok(result) = pulse_plugin_sdk::host::call_capability(capability, payload.clone()) {
            return Ok(result);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let url = format!("{}/{}", api_base(), http_get_path);
        let client = reqwest::blocking::Client::new();
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| bridge_err(format!("GET {url}: {e}")))?;
        let body = resp
            .text()
            .map_err(|e| bridge_err(format!("read response: {e}")))?;
        return serde_json::from_str(&body)
            .map_err(|e| bridge_err(format!("parse response from {url}: {e}")));
    }

    #[cfg(target_arch = "wasm32")]
    {
        let _ = (capability, payload, http_get_path);
        Err(bridge_err("plugin bridge not available in WASM"))
    }
}

/// Query board data from plugin-board.
/// Proxies data-query requests via capability RPC (server mode) or HTTP GET (CLI mode).
pub fn board_query(
    endpoint: &str,
    workspace: Option<&str>,
    board_id: Option<&str>,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let payload = serde_json::json!({
        "action": "data-query",
        "endpoint": endpoint,
        "workspace": workspace,
        "board_id": board_id,
        "workspace_dir": config.base_dir.to_string_lossy(),
    });
    get_from_plugin(
        "board.query",
        payload,
        &format!("plugins/plugin-board/{}", endpoint),
    )
}

/// Mutate board data in plugin-board.
/// Proxies data-mutate requests via capability RPC (server mode) or HTTP POST (CLI mode).
pub fn board_mutate(
    endpoint: &str,
    payload: &serde_json::Value,
    workspace: Option<&str>,
    board_id: Option<&str>,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let full_payload = serde_json::json!({
        "action": "data-mutate",
        "endpoint": endpoint,
        "payload": payload,
        "workspace": workspace,
        "board_id": board_id,
        "workspace_dir": config.base_dir.to_string_lossy(),
    });
    call_plugin(
        "board.mutate",
        full_payload,
        "plugins/plugin-board/execute",
    )
}

/// List active worktrees with task and git context.
/// Delegates to plugin-workspace-tracker.
pub fn worktrees_list(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let payload = config_payload(config);
    call_plugin(
        "workspace-tracker.list",
        payload,
        "plugins/plugin-workspace-tracker/worktrees/list",
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
}
