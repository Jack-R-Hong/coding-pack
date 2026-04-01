//! Board API client for task operations on the Pulse server.
//!
//! Provides stateless functions that communicate with the Pulse Task API
//! at `http://127.0.0.1:{PULSE_API_PORT}/api/v1`.  Used by `github_sync`
//! to create, find, and update board tasks.
//!
//! All HTTP operations are gated with `#[cfg(not(target_arch = "wasm32"))]`.

use pulse_plugin_sdk::error::WitPluginError;
use serde::{Deserialize, Serialize};

// ── Types ─────────────────────────────────────────────────────────────────────

/// A board task as returned by the Pulse Task API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTask {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build the Pulse API base URL from `PULSE_API_PORT` (default 8080).
#[cfg(test)]
fn api_base() -> String {
    let port = std::env::var("PULSE_API_PORT").unwrap_or_else(|_| "8080".to_string());
    format!("http://127.0.0.1:{}/api/v1", port)
}

fn api_err(msg: impl std::fmt::Display) -> WitPluginError {
    WitPluginError::internal(format!("Board API error: {msg}"))
}

// ── BoardClient struct ───────────────────────────────────────────────────────

/// Reusable board API client with connection pooling and timeout.
#[cfg(not(target_arch = "wasm32"))]
pub struct BoardClient {
    client: reqwest::blocking::Client,
    base_url: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for BoardClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl BoardClient {
    /// Create a new `BoardClient`.  Reads `PULSE_API_PORT` from the environment
    /// (default 8080) and builds a shared HTTP client with a 5-second timeout.
    pub fn new() -> Self {
        let port = std::env::var("PULSE_API_PORT").unwrap_or_else(|_| "8080".to_string());
        let base_url = format!("http://127.0.0.1:{}/api/v1", port);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self { client, base_url }
    }

    /// Create a new task on the board.
    ///
    /// POSTs to `/api/v1/tasks` with the given title, optional description,
    /// status, and metadata.  Returns the created `BoardTask`.
    pub fn create_task(
        &self,
        title: &str,
        description: Option<&str>,
        status: &str,
        metadata: &serde_json::Value,
    ) -> Result<BoardTask, WitPluginError> {
        let url = format!("{}/tasks", self.base_url);
        let mut body = serde_json::json!({
            "title": title,
            "status": status,
            "metadata": metadata,
        });
        if let Some(desc) = description {
            body["description"] = serde_json::json!(desc);
        }

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|e| api_err(format!("POST {url}: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| api_err(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(WitPluginError::internal(format!(
                "Board API error (HTTP {}): {}",
                status.as_u16(),
                &text[..text.len().min(200)]
            )));
        }
        let task: BoardTask =
            serde_json::from_str(&text).map_err(|e| api_err(format!("parse: {e}")))?;

        tracing::debug!(
            plugin = "coding-pack",
            task_id = %task.id,
            "created board task"
        );
        Ok(task)
    }

    /// Find a board task whose metadata contains a matching `issue_number`.
    ///
    /// Fetches all tasks from the board and scans metadata for the given issue
    /// number.  Returns `None` if no match is found.
    // TODO: For large boards, consider caching the task list or adding a server-side
    // filter endpoint to avoid O(n) scans per issue during sync.
    pub fn find_task_by_issue_number(
        &self,
        issue_number: u64,
    ) -> Result<Option<BoardTask>, WitPluginError> {
        let url = format!("{}/tasks", self.base_url);

        let resp = self
            .client
            .get(&url)
            .send()
            .map_err(|e| api_err(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| api_err(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(WitPluginError::internal(format!(
                "Board API error (HTTP {}): {}",
                status.as_u16(),
                &text[..text.len().min(200)]
            )));
        }

        // The API may return an array or an object wrapping an array.
        let tasks: Vec<BoardTask> = match serde_json::from_str::<Vec<BoardTask>>(&text) {
            Ok(t) => t,
            Err(_) => {
                // Try unwrapping from { "tasks": [...] }
                let wrapper: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| api_err(format!("parse: {e}")))?;
                if let Some(arr) = wrapper.get("tasks") {
                    serde_json::from_value(arr.clone())
                        .map_err(|e| api_err(format!("parse tasks array: {e}")))?
                } else {
                    return Err(api_err("unexpected response format from GET /tasks"));
                }
            }
        };

        for task in tasks {
            if let Some(num) = task.metadata.get("issue_number").and_then(|v| v.as_u64()) {
                if num == issue_number {
                    return Ok(Some(task));
                }
            }
        }

        Ok(None)
    }

    /// Update a board task's title and (optionally) description.
    ///
    /// PATCHes `/api/v1/tasks/{task_id}` with the new title and description.
    pub fn update_task(
        &self,
        task_id: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<(), WitPluginError> {
        let url = format!("{}/tasks/{}", self.base_url, task_id);
        let mut body = serde_json::json!({ "title": title });
        if let Some(desc) = description {
            body["description"] = serde_json::json!(desc);
        }

        let resp = self
            .client
            .patch(&url)
            .json(&body)
            .send()
            .map_err(|e| api_err(format!("PATCH {url}: {e}")))?;

        if !resp.status().is_success() {
            let status_code = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(api_err(format!(
                "PATCH {url} returned {status_code}: {text}"
            )));
        }

        tracing::debug!(
            plugin = "coding-pack",
            task_id = task_id,
            "updated board task title/description"
        );
        Ok(())
    }

    /// Update the status of a board task.
    pub fn update_task_status(&self, task_id: &str, status: &str) -> Result<(), WitPluginError> {
        let url = format!("{}/tasks/{}", self.base_url, task_id);
        let body = serde_json::json!({ "status": status });

        let resp = self
            .client
            .patch(&url)
            .json(&body)
            .send()
            .map_err(|e| api_err(format!("PATCH {url}: {e}")))?;

        if !resp.status().is_success() {
            let status_code = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(api_err(format!(
                "PATCH {url} returned {status_code}: {text}"
            )));
        }

        tracing::debug!(
            plugin = "coding-pack",
            task_id = task_id,
            status = status,
            "updated board task status"
        );
        Ok(())
    }

    /// Get the metadata for a board task.
    pub fn get_task_metadata(&self, task_id: &str) -> Result<serde_json::Value, WitPluginError> {
        let url = format!("{}/tasks/{}", self.base_url, task_id);

        let resp = self
            .client
            .get(&url)
            .send()
            .map_err(|e| api_err(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| api_err(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(WitPluginError::internal(format!(
                "Board API error (HTTP {}): {}",
                status.as_u16(),
                &text[..text.len().min(200)]
            )));
        }

        let task: BoardTask =
            serde_json::from_str(&text).map_err(|e| api_err(format!("parse: {e}")))?;
        Ok(task.metadata)
    }
}

// ── WASM stubs ────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
pub struct BoardClient;

#[cfg(target_arch = "wasm32")]
impl BoardClient {
    pub fn new() -> Self {
        Self
    }

    pub fn create_task(
        &self,
        _title: &str,
        _description: Option<&str>,
        _status: &str,
        _metadata: &serde_json::Value,
    ) -> Result<BoardTask, WitPluginError> {
        Err(WitPluginError::internal(
            "create_task is not available in WASM builds",
        ))
    }

    pub fn update_task(
        &self,
        _task_id: &str,
        _title: &str,
        _description: Option<&str>,
    ) -> Result<(), WitPluginError> {
        Err(WitPluginError::internal(
            "update_task is not available in WASM builds",
        ))
    }

    pub fn find_task_by_issue_number(
        &self,
        _issue_number: u64,
    ) -> Result<Option<BoardTask>, WitPluginError> {
        Err(WitPluginError::internal(
            "find_task_by_issue_number is not available in WASM builds",
        ))
    }

    pub fn update_task_status(
        &self,
        _task_id: &str,
        _status: &str,
    ) -> Result<(), WitPluginError> {
        Err(WitPluginError::internal(
            "update_task_status is not available in WASM builds",
        ))
    }

    pub fn get_task_metadata(
        &self,
        _task_id: &str,
    ) -> Result<serde_json::Value, WitPluginError> {
        Err(WitPluginError::internal(
            "get_task_metadata is not available in WASM builds",
        ))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to prevent parallel test interference with env vars
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_board_task_serialization() {
        let task = BoardTask {
            id: "task-1".to_string(),
            title: "Fix login bug".to_string(),
            status: "ready-for-dev".to_string(),
            metadata: serde_json::json!({
                "issue_number": 42,
                "source": "github-sync"
            }),
        };

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["id"], "task-1");
        assert_eq!(json["title"], "Fix login bug");
        assert_eq!(json["status"], "ready-for-dev");
        assert_eq!(json["metadata"]["issue_number"], 42);
        assert_eq!(json["metadata"]["source"], "github-sync");
    }

    #[test]
    fn test_board_task_deserialization() {
        let json = r#"{
            "id": "task-2",
            "title": "Add tests",
            "status": "done",
            "metadata": {"issue_number": 10, "issue_url": "https://github.com/o/r/issues/10"}
        }"#;
        let task: BoardTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.id, "task-2");
        assert_eq!(task.title, "Add tests");
        assert_eq!(task.status, "done");
        assert_eq!(
            task.metadata["issue_url"],
            "https://github.com/o/r/issues/10"
        );
    }

    #[test]
    fn test_board_task_deserialization_no_metadata() {
        let json = r#"{"id": "task-3", "title": "No meta", "status": "open"}"#;
        let task: BoardTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.id, "task-3");
        assert!(task.metadata.is_null());
    }

    #[test]
    fn test_api_base_default_port() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_API_PORT");
        let base = api_base();
        assert_eq!(base, "http://127.0.0.1:8080/api/v1");
    }

    #[test]
    fn test_api_base_custom_port() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_API_PORT", "3000");
        let base = api_base();
        assert_eq!(base, "http://127.0.0.1:3000/api/v1");
        std::env::remove_var("PULSE_API_PORT");
    }

    #[test]
    fn test_api_url_format_tasks() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_API_PORT");
        let url = format!("{}/tasks", api_base());
        assert_eq!(url, "http://127.0.0.1:8080/api/v1/tasks");
    }

    #[test]
    fn test_api_url_format_task_by_id() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_API_PORT");
        let url = format!("{}/tasks/{}", api_base(), "abc-123");
        assert_eq!(url, "http://127.0.0.1:8080/api/v1/tasks/abc-123");
    }
}
