#[cfg(not(target_arch = "wasm32"))]
pub mod agent_registry;
#[cfg(not(target_arch = "wasm32"))]
pub mod config_injector;
pub mod pack;
pub mod plugin_bridge;
pub mod pulse_api;
#[cfg(not(target_arch = "wasm32"))]
pub mod tool_provider;
pub mod util;
pub mod validator;
pub mod workspace;

use pulse_plugin_sdk::error::WitPluginError;
use pulse_plugin_sdk::wit_traits::{DashboardExtensionPlugin, PluginLifecycle, StepExecutorPlugin};
use pulse_plugin_sdk::wit_types::{
    PluginDependency, PluginInfo, StepConfig, StepResult, TaskInput,
};
use tracing::info;

use pack::CodingPackInput;
use util::is_executable;

// ── Server-mode registration ───────────────────────────────────────────────

/// Returns SDK-compatible plugin metadata.
pub fn metadata() -> pulse_plugin_sdk::PluginMetadata {
    pulse_plugin_sdk::PluginMetadata::new(
        "plugin-coding-pack",
        env!("CARGO_PKG_VERSION"),
        pulse_plugin_sdk::API_VERSION,
    )
    .with_description("Coding pack orchestrator with BMAD agent injection and tool provider")
}

/// Registers plugin-coding-pack with Pulse's plugin registry (server mode).
///
/// Returns a `PluginRegistration` containing:
/// - `HookPoint::ConfigInjector` — BmadAgentInjector for per-agent persona injection
/// - `HookPoint::ToolProvider` — BmadToolProvider exposing pack operations as LLM tools
/// - `HookPoint::AgentDefinitionProvider` — BmadAgentRegistry for agent discovery and skill routing
///
/// In server mode, Pulse's plugin-loader calls this function and merges the
/// returned capabilities into the shared `PluginRegistry`. provider-claude-code
/// receives that registry via `register_with_deps()` and can query our injector
/// and tool provider at runtime.
#[cfg(not(target_arch = "wasm32"))]
pub fn register() -> pulse_plugin_sdk::PluginRegistration {
    use std::sync::Arc;

    let manifest_path = std::path::PathBuf::from("_bmad/_config/agent-manifest.csv");
    let injector = config_injector::BmadAgentInjector::new(&manifest_path);
    let tool_prov = tool_provider::BmadToolProvider::new(workspace::WorkspaceConfig::resolve(None));
    let agent_reg = agent_registry::BmadAgentRegistry::new(&manifest_path);

    pulse_plugin_sdk::PluginRegistration::new(metadata())
        .with_capability(pulse_plugin_sdk::HookPoint::ConfigInjector(Arc::new(
            injector,
        )))
        .with_capability(pulse_plugin_sdk::HookPoint::ToolProvider(Arc::new(
            tool_prov,
        )))
        .with_capability(pulse_plugin_sdk::HookPoint::AgentDefinitionProvider(
            Arc::new(agent_reg),
        ))
}

/// Meta-plugin that orchestrates the coding plugin pack.
///
/// Validates that all required sibling plugins are present and healthy,
/// provides workflow validation, and exposes a step executor for pack-level operations.
#[derive(Default)]
pub struct CodingPackPlugin;

impl PluginLifecycle for CodingPackPlugin {
    fn get_info(&self) -> PluginInfo {
        PluginInfo::new("plugin-coding-pack", env!("CARGO_PKG_VERSION"))
            .with_description(
                "Coding pack orchestrator — coordinates bmad-method, provider-claude-code, and git-worktree plugins",
            )
            .with_dependencies(vec![
                PluginDependency {
                    name: "bmad-method".to_string(),
                    version_req: ">=0.1.0".to_string(),
                    optional: false,
                },
                PluginDependency {
                    name: "provider-claude-code".to_string(),
                    version_req: ">=0.1.0".to_string(),
                    optional: false,
                },
                PluginDependency {
                    name: "plugin-git-worktree".to_string(),
                    version_req: ">=0.1.0".to_string(),
                    optional: true,
                },
                PluginDependency {
                    name: "plugin-memory".to_string(),
                    version_req: ">=0.1.0".to_string(),
                    optional: true,
                },
                PluginDependency {
                    name: "plugin-board".to_string(),
                    version_req: ">=0.1.0".to_string(),
                    optional: true,
                },
            ])
    }

    fn health_check(&self) -> bool {
        let ws_config = workspace::WorkspaceConfig::default();
        let workflows_dir = &ws_config.workflows_dir;
        let plugins_dir = &ws_config.plugins_dir;

        let workflows_ok = workflows_dir.exists();
        let plugins_ok = plugins_dir.exists();

        // Verify required plugin binaries exist and are executable
        let required_plugins = ["bmad-method", "provider-claude-code"];
        let mut plugins_healthy = true;
        for plugin_name in &required_plugins {
            let plugin_path = plugins_dir.join(plugin_name);
            if !plugin_path.exists() {
                tracing::warn!(
                    plugin = "plugin-coding-pack",
                    missing = plugin_name,
                    "Required plugin binary not found"
                );
                plugins_healthy = false;
            } else if !is_executable(&plugin_path) {
                tracing::warn!(
                    plugin = "plugin-coding-pack",
                    not_executable = plugin_name,
                    "Plugin binary is not executable"
                );
                plugins_healthy = false;
            }
        }

        let healthy = workflows_ok && plugins_ok && plugins_healthy;

        if healthy {
            info!(
                plugin = "plugin-coding-pack",
                status = "healthy",
                "Coding pack health check passed"
            );
        } else {
            tracing::warn!(
                plugin = "plugin-coding-pack",
                workflows_dir_exists = workflows_ok,
                plugins_dir_exists = plugins_ok,
                plugins_healthy = plugins_healthy,
                "Coding pack health check: issues detected"
            );
        }

        healthy
    }
}

impl StepExecutorPlugin for CodingPackPlugin {
    fn execute(&self, task: TaskInput, config: StepConfig) -> Result<StepResult, WitPluginError> {
        // Respond to capability probe
        if task.task_id == "__probe__" {
            return Ok(StepResult {
                step_id: "__probe__".to_string(),
                status: "probe_ok".to_string(),
                content: None,
                execution_time_ms: 0,
            });
        }

        let input_val = task.input.as_ref().ok_or_else(|| {
            WitPluginError::invalid_input(
                "task input required; send JSON {\"action\": \"validate-pack\"}, {\"action\": \"validate-workflows\"}, or {\"action\": \"list-workflows\"}",
            )
        })?;

        let mut pack_input: CodingPackInput = serde_json::from_value(input_val.clone())
            .map_err(|e| WitPluginError::invalid_input(format!("invalid input: {e}")))?;

        // Resolve workspace: check input fields, then task metadata,
        // then fall back to querying the Pulse task's own workspace field.
        if pack_input.workspace_dir.is_none() {
            if let Some(meta) = &task.metadata {
                let ws = meta
                    .get("workspace_dir")
                    .or_else(|| meta.get("workspace"))
                    .or_else(|| meta.get("workspace_path"))
                    .and_then(|v| v.as_str());
                if let Some(ws) = ws {
                    pack_input.workspace_dir = Some(ws.to_string());
                }
            }
        }
        // Last resort: fetch the task record from Pulse API to read its workspace.
        if pack_input.workspace.is_none() && task.task_id != "__probe__" {
            if let Ok(pulse_task) = pulse_api::get_task(&task.task_id) {
                if !pulse_task.workspace_id.is_empty() {
                    pack_input.workspace = Some(pulse_task.workspace_id);
                }
            }
        }

        let start = std::time::Instant::now();
        let result = pack::execute_action(&pack_input)?;
        let elapsed = start.elapsed().as_millis() as u64;

        Ok(StepResult {
            step_id: config.step_id,
            status: "success".to_string(),
            content: Some(result),
            execution_time_ms: elapsed,
        })
    }
}

impl DashboardExtensionPlugin for CodingPackPlugin {
    fn get_pages_json(&self) -> String {
        // Runtime read for hot-reload during development
        if let Ok(content) = std::fs::read_to_string("dashboard/manifest.json") {
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(pages) = manifest.get("pages") {
                    return pages.to_string();
                }
            }
        }
        // Compile-time fallback
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../dashboard/manifest.json"))
                .expect("embedded dashboard/manifest.json is invalid");
        manifest["pages"].to_string()
    }

    fn get_api_routes_json(&self) -> String {
        serde_json::json!([
            {
                "prefix": "/api/v1/plugin-coding-pack",
                "description": "Coding pack status, validation, workflow management, board proxy, and worktrees",
                "endpoints": [
                    "GET  /status                        — Pack health and validation",
                    "GET  /status/health                 — Health badge data",
                    "GET  /workflows/list                — All workflows as table data",
                    "GET  /workflows/{id}               — Workflow detail with steps",
                    "POST /workflows/{id}/execute        — Trigger workflow execution",
                    "GET  /agents/list                   — BMAD agent roster",
                    "GET  /agents/{id}                  — Agent detail",
                    "GET  /executions/stream             — SSE execution event stream",
                    "GET  /tasks/{task_id}/workflow-context — Task workflow context",
                    "GET  /tasks/{task_id}/agent-info    — Task agent info",
                    "GET  /board/data                    — Kanban board (proxied to plugin-board)",
                    "GET  /board/boards/list             — List boards per workspace (proxied)",
                    "GET  /board/epics/list              — All epics as table data (proxied)",
                    "GET  /board/filters                 — Available filter options (proxied)",
                    "GET  /board/summary                 — Sprint progress badge (local)",
                    "GET  /board/epics/{id}             — Epic detail with stories (proxied)",
                    "GET  /board/stories/{id}           — Story detail (proxied)",
                    "GET  /board/assignments/{id}       — Assignment detail (proxied)",
                    "POST /board/sync                    — Sync board store (proxied)",
                    "PUT  /board/status/{id}            — Update item status (proxied)",
                    "POST /board/epics                   — Create epic (proxied)",
                    "PUT  /board/epics/{id}             — Update epic (proxied)",
                    "POST /board/stories                 — Create story (proxied)",
                    "PUT  /board/stories/{id}           — Update story (proxied)",
                    "POST /board/assignments             — Create assignment (proxied)",
                    "PUT  /board/assignments/{id}       — Update assignment (proxied)",
                    "GET  /worktrees/list                — Active worktrees with task and git context",
                    "GET  /worktrees/{id}               — Worktree detail with git status"
                ]
            }
        ])
        .to_string()
    }

    fn get_display_customizations_json(&self) -> String {
        serde_json::json!([
            {
                "id": "coding-pack-health",
                "title": "Coding Pack",
                "target_view": "workflow",
                "customization": {
                    "type": "badge",
                    "key": "pack_status",
                    "label": "Pack",
                    "color_mapping": {
                        "healthy": "#10b981",
                        "degraded": "#f59e0b",
                        "error": "#ef4444",
                        "default": "#64748b"
                    }
                },
                "data_endpoint": "status/health",
                "render_priority": 10
            },
            {
                "id": "coding-workflow-info",
                "title": "Workflow Details",
                "target_view": "task",
                "customization": {
                    "type": "fields",
                    "fields": [
                        { "key": "workflow_id", "label": "Workflow" },
                        { "key": "step_id", "label": "Current Step" },
                        { "key": "executor", "label": "Executor" },
                        { "key": "model_tier", "label": "Model Tier" }
                    ]
                },
                "data_endpoint": "tasks/{task_id}/workflow-context",
                "render_priority": 20
            },
            {
                "id": "coding-pack-agent",
                "title": "BMAD Agent",
                "target_view": "task",
                "customization": {
                    "type": "badge",
                    "key": "agent_name",
                    "label": "Agent",
                    "color_mapping": {
                        "Winston": "#3b82f6",
                        "Amelia": "#10b981",
                        "John": "#8b5cf6",
                        "Quinn": "#f59e0b",
                        "Bob": "#06b6d4",
                        "Barry": "#ef4444",
                        "Mary": "#ec4899",
                        "Sally": "#14b8a6",
                        "Paige": "#a855f7",
                        "default": "#64748b"
                    }
                },
                "data_endpoint": "tasks/{task_id}/agent-info",
                "render_priority": 30
            },
            {
                "id": "sprint-progress",
                "title": "Sprint Progress",
                "target_view": "workflow",
                "customization": {
                    "type": "badge",
                    "key": "sprint_progress",
                    "label": "Sprint",
                    "color_mapping": {
                        "on-track": "#10b981",
                        "at-risk": "#f59e0b",
                        "blocked": "#ef4444",
                        "default": "#64748b"
                    }
                },
                "data_endpoint": "board/summary",
                "render_priority": 5
            }
        ])
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_health_check_returns_true() {
        // Create stub plugin binaries so health_check can find them
        let plugins_dir = std::path::Path::new("config/plugins");
        std::fs::create_dir_all(plugins_dir).unwrap();

        for name in &["bmad-method", "provider-claude-code"] {
            let path = plugins_dir.join(name);
            if !path.exists() {
                std::fs::write(&path, "#!/bin/sh\n").unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
            }
        }

        let plugin = CodingPackPlugin;
        assert!(plugin.health_check());
    }

    #[test]
    fn plugin_info_has_correct_name() {
        let plugin = CodingPackPlugin;
        let info = plugin.get_info();
        assert_eq!(info.name, "plugin-coding-pack");
        assert!(!info.version.is_empty());
    }

    #[test]
    fn plugin_info_declares_dependencies() {
        let plugin = CodingPackPlugin;
        let info = plugin.get_info();
        assert!(info.dependencies.len() >= 2);
        let names: Vec<&str> = info.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"bmad-method"));
        assert!(names.contains(&"provider-claude-code"));
    }

    #[test]
    fn probe_returns_ok() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("__probe__", "");
        let config = StepConfig::new("__probe__", "");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "probe_ok");
    }

    #[test]
    fn execute_validate_pack_returns_success() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t1", "validate pack")
            .with_input(serde_json::json!({"action": "validate-pack"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
        assert!(result.content.is_some());
    }

    #[test]
    fn execute_list_workflows_returns_success() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t1", "list workflows")
            .with_input(serde_json::json!({"action": "list-workflows"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn execute_list_plugins_returns_success() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t1", "list plugins")
            .with_input(serde_json::json!({"action": "list-plugins"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn execute_unknown_action_returns_error() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t1", "test")
            .with_input(serde_json::json!({"action": "unknown-action"}));
        let config = StepConfig::new("s1", "agent");
        let err = plugin.execute(task, config).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn execute_missing_input_returns_error() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t1", "test");
        let config = StepConfig::new("s1", "agent");
        let err = plugin.execute(task, config).unwrap_err();
        assert_eq!(err.code, "invalid_input");
    }

    #[test]
    fn dashboard_pages_json_is_valid() {
        let plugin = CodingPackPlugin;
        let json = plugin.get_pages_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let pages = parsed.as_array().unwrap();
        // 7 original + board + assignment-detail + epics + epic-detail + worktrees = 12
        assert_eq!(pages.len(), 12);

        let layout_types: Vec<&str> = pages
            .iter()
            .filter_map(|p| p["layout"]["type"].as_str())
            .collect();
        assert!(layout_types.contains(&"board"), "missing board layout");
        assert!(layout_types.contains(&"table"), "missing table layout");
        assert!(layout_types.contains(&"detail"), "missing detail layout");
        assert!(layout_types.contains(&"form"), "missing form layout");
        assert!(layout_types.contains(&"stream"), "missing stream layout");

        let page_ids: Vec<&str> = pages.iter().filter_map(|p| p["id"].as_str()).collect();
        for expected in &[
            "overview",
            "board",
            "assignment-detail",
            "epics",
            "epic-detail",
            "worktrees",
            "workflows",
            "workflow-detail",
            "agents",
            "status",
            "execute",
            "logs",
        ] {
            assert!(page_ids.contains(expected), "missing page: {expected}");
        }
    }

    #[test]
    fn dashboard_api_routes_json_is_valid() {
        let plugin = CodingPackPlugin;
        let json = plugin.get_api_routes_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let routes = parsed.as_array().unwrap();
        assert_eq!(routes.len(), 1);
        assert!(routes[0]["prefix"]
            .as_str()
            .unwrap()
            .contains("plugin-coding-pack"));
        // Verify endpoints are documented (10 core + 9 board GET proxied + 6 board mutation proxied + 2 worktrees)
        let endpoints = routes[0]["endpoints"].as_array().unwrap();
        assert!(endpoints.len() >= 27);
    }

    #[test]
    fn dashboard_display_customizations_json_is_valid() {
        let plugin = CodingPackPlugin;
        let json = plugin.get_display_customizations_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let customs = parsed.as_array().unwrap();
        assert_eq!(customs.len(), 4);

        // Pack health badge on workflow view
        assert_eq!(customs[0]["id"], "coding-pack-health");
        assert_eq!(customs[0]["target_view"], "workflow");
        assert_eq!(customs[0]["customization"]["type"], "badge");

        // Workflow context fields on task view
        assert_eq!(customs[1]["id"], "coding-workflow-info");
        assert_eq!(customs[1]["target_view"], "task");
        assert_eq!(customs[1]["customization"]["type"], "fields");

        // Agent badge on task view
        assert_eq!(customs[2]["id"], "coding-pack-agent");
        assert_eq!(customs[2]["target_view"], "task");
        assert_eq!(customs[2]["customization"]["type"], "badge");

        // Sprint progress badge on workflow view
        assert_eq!(customs[3]["id"], "sprint-progress");
        assert_eq!(customs[3]["target_view"], "workflow");
        assert_eq!(customs[3]["customization"]["type"], "badge");
    }
}
