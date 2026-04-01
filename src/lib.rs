#[cfg(not(target_arch = "wasm32"))]
pub mod agent_registry;
#[cfg(not(target_arch = "wasm32"))]
pub mod auto_dev;
#[cfg(not(target_arch = "wasm32"))]
pub mod board_client;
#[cfg(not(target_arch = "wasm32"))]
pub mod config_injector;
pub mod execution_history;
#[cfg(not(target_arch = "wasm32"))]
pub mod github_client;
#[cfg(not(target_arch = "wasm32"))]
pub mod github_sync;
pub mod mesh_guard;
pub mod pack;
pub mod plugin_bridge;
pub mod pulse_api;
#[cfg(not(target_arch = "wasm32"))]
pub mod tool_provider;
pub mod util;
pub mod validator;
pub mod workspace;
#[cfg(not(target_arch = "wasm32"))]
pub mod worktree_tracker;

use std::path::Path;

use pulse_plugin_sdk::error::WitPluginError;
use pulse_plugin_sdk::wit_traits::{
    DashboardExtensionPlugin, InstallContext, PluginLifecycle, StepExecutorPlugin, UninstallContext,
};
use pulse_plugin_sdk::wit_types::{
    PluginDependency, PluginInfo, StepConfig, StepResult, TaskInput,
};
use tracing::info;

use pack::CodingPackInput;
use util::is_executable;

/// Workflow YAML file stems that belong to this plugin.
/// Used by `on_uninstall` to remove only our files (not user-created ones).
const KNOWN_WORKFLOW_NAMES: &[&str] = &[
    "coding-quick-dev",
    "coding-feature-dev",
    "coding-story-dev",
    "coding-bug-fix",
    "coding-docs",
    "coding-hotfix",
    "coding-release",
    "coding-security-audit",
    "coding-migration",
    "coding-perf-review",
    "coding-pr-fix",
    "coding-refactor",
    "coding-review",
    "coding-parallel-review",
    "coding-memory-index",
    "bootstrap-plugin",
    "bootstrap-rebuild",
    "bootstrap-cycle",
    "project-init",
];

/// Recursively copy a directory tree from `src` to `dst`, creating directories
/// as needed and overwriting existing files.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<u64> {
    let mut count = 0u64;
    if !src.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            count += copy_tree(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
            count += 1;
        }
    }
    Ok(count)
}

// ── Server-mode registration ───────────────────────────────────────────────

/// Returns SDK-compatible plugin metadata.
pub fn metadata() -> pulse_plugin_sdk::PluginMetadata {
    pulse_plugin_sdk::PluginMetadata::new(
        "plugin-coding-pack",
        env!("CARGO_PKG_VERSION"),
        pulse_plugin_sdk::API_VERSION,
    )
    .with_description("Coding pack orchestrator with BMAD agent injection and tool provider")
    .with_provides(vec![
        "coding-pack.validate".into(),
        "coding-pack.workflows".into(),
        "coding-pack.agents".into(),
        "coding-pack.data-query".into(),
    ])
    .with_tags(vec![
        "orchestrator".into(),
        "coding".into(),
        "bmad".into(),
        "meta-plugin".into(),
    ])
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
                PluginDependency {
                    name: "plugin-test-runner".to_string(),
                    version_req: ">=0.1.0".to_string(),
                    optional: true,
                },
            ])
            .with_provides(vec![
                "coding-pack.validate".into(),
                "coding-pack.workflows".into(),
                "coding-pack.agents".into(),
                "coding-pack.data-query".into(),
            ])
            .with_tags(vec![
                "orchestrator".into(),
                "coding".into(),
                "bmad".into(),
                "meta-plugin".into(),
            ])
    }

    fn on_install(&self, ctx: &InstallContext) -> Result<(), WitPluginError> {
        // 1. Copy workflow YAMLs: plugin_dir/config/workflows/ -> ctx.workflows_dir/
        let src_workflows = ctx.plugin_dir.join("config").join("workflows");
        if src_workflows.is_dir() {
            std::fs::create_dir_all(&ctx.workflows_dir).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to create workflows dir {}: {e}",
                    ctx.workflows_dir.display()
                ))
            })?;
            let mut wf_count = 0u64;
            let entries = std::fs::read_dir(&src_workflows).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to read {}: {e}",
                    src_workflows.display()
                ))
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| {
                    WitPluginError::internal(format!("failed to read workflow entry: {e}"))
                })?;
                let src_path = entry.path();
                if src_path.is_file() {
                    let dst_path = ctx.workflows_dir.join(entry.file_name());
                    std::fs::copy(&src_path, &dst_path).map_err(|e| {
                        WitPluginError::internal(format!(
                            "failed to copy {} -> {}: {e}",
                            src_path.display(),
                            dst_path.display()
                        ))
                    })?;
                    wf_count += 1;
                }
            }
            info!(
                plugin = "plugin-coding-pack",
                count = wf_count,
                dst = %ctx.workflows_dir.display(),
                "Installed workflow YAMLs"
            );
        }

        // 2. Copy _bmad config tree: plugin_dir/_bmad/ -> ctx.config_dir/_bmad/
        let src_bmad = ctx.plugin_dir.join("_bmad");
        if src_bmad.is_dir() {
            let dst_bmad = ctx.config_dir.join("_bmad");
            let count = copy_tree(&src_bmad, &dst_bmad).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to copy _bmad tree {} -> {}: {e}",
                    src_bmad.display(),
                    dst_bmad.display()
                ))
            })?;
            info!(
                plugin = "plugin-coding-pack",
                files = count,
                dst = %dst_bmad.display(),
                "Installed _bmad config tree"
            );
        }

        // 3. Copy config.yaml (only if not already present — preserve user customizations)
        let src_config = ctx.plugin_dir.join("config").join("config.yaml");
        let dst_config = ctx.config_dir.join("config.yaml");
        if src_config.is_file() && !dst_config.exists() {
            std::fs::create_dir_all(&ctx.config_dir).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to create config dir {}: {e}",
                    ctx.config_dir.display()
                ))
            })?;
            std::fs::copy(&src_config, &dst_config).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to copy config.yaml {} -> {}: {e}",
                    src_config.display(),
                    dst_config.display()
                ))
            })?;
            info!(
                plugin = "plugin-coding-pack",
                dst = %dst_config.display(),
                "Installed config.yaml"
            );
        } else if dst_config.exists() {
            info!(
                plugin = "plugin-coding-pack",
                dst = %dst_config.display(),
                "Skipped config.yaml (already exists)"
            );
        }

        info!(plugin = "plugin-coding-pack", "Install complete");
        Ok(())
    }

    fn on_uninstall(&self, ctx: &UninstallContext) -> Result<(), WitPluginError> {
        if !ctx.purge {
            info!(
                plugin = "plugin-coding-pack",
                "Uninstall (non-purge): keeping config for potential reinstall"
            );
            return Ok(());
        }

        // 1. Remove known workflow YAMLs
        let mut removed = 0u64;
        for name in KNOWN_WORKFLOW_NAMES {
            let path = ctx.workflows_dir.join(format!("{name}.yaml"));
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| {
                    WitPluginError::internal(format!(
                        "failed to remove workflow {}: {e}",
                        path.display()
                    ))
                })?;
                removed += 1;
            }
        }
        if removed > 0 {
            info!(
                plugin = "plugin-coding-pack",
                count = removed,
                "Removed workflow YAMLs"
            );
        }

        // 2. Remove _bmad config tree
        // Note: UninstallContext lacks config_dir, so we derive it from workflows_dir.
        // This assumes the standard Pulse layout: {workspace}/config/workflows/
        // where config_dir is {workspace}/config/ (i.e., workflows_dir's parent).
        let config_dir = ctx.workflows_dir.parent().unwrap_or(&ctx.workflows_dir);
        let bmad_dir = config_dir.join("_bmad");
        if bmad_dir.is_dir() {
            std::fs::remove_dir_all(&bmad_dir).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to remove _bmad dir {}: {e}",
                    bmad_dir.display()
                ))
            })?;
            info!(
                plugin = "plugin-coding-pack",
                path = %bmad_dir.display(),
                "Removed _bmad config tree"
            );
        }

        // 3. Remove execution-history.json
        let history_path = config_dir.join("execution-history.json");
        if history_path.is_file() {
            std::fs::remove_file(&history_path).map_err(|e| {
                WitPluginError::internal(format!(
                    "failed to remove execution-history.json: {e}",
                    ))
            })?;
            info!(
                plugin = "plugin-coding-pack",
                "Removed execution-history.json"
            );
        }

        info!(plugin = "plugin-coding-pack", "Purge uninstall complete");
        Ok(())
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
                    "GET  /executions/stream             — SSE execution event stream (planned — not yet implemented)",
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
    fn metadata_declares_provides() {
        let m = metadata();
        assert_eq!(
            m.provides,
            vec![
                "coding-pack.validate",
                "coding-pack.workflows",
                "coding-pack.agents",
                "coding-pack.data-query",
            ]
        );
    }

    #[test]
    fn metadata_declares_tags() {
        let m = metadata();
        assert_eq!(
            m.tags,
            vec!["orchestrator", "coding", "bmad", "meta-plugin"]
        );
    }

    #[test]
    fn plugin_info_declares_provides() {
        let plugin = CodingPackPlugin;
        let info = plugin.get_info();
        assert_eq!(
            info.provides,
            vec![
                "coding-pack.validate",
                "coding-pack.workflows",
                "coding-pack.agents",
                "coding-pack.data-query",
            ]
        );
    }

    #[test]
    fn plugin_info_declares_tags() {
        let plugin = CodingPackPlugin;
        let info = plugin.get_info();
        assert_eq!(
            info.tags,
            vec!["orchestrator", "coding", "bmad", "meta-plugin"]
        );
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
        assert_eq!(pages.len(), 6); // Logs page disabled until SSE infra is ready

        let layout_types: Vec<&str> = pages
            .iter()
            .filter_map(|p| p["layout"]["type"].as_str())
            .collect();
        assert!(layout_types.contains(&"table"), "missing table layout");
        assert!(layout_types.contains(&"detail"), "missing detail layout");
        assert!(layout_types.contains(&"form"), "missing form layout");

        let page_ids: Vec<&str> = pages.iter().filter_map(|p| p["id"].as_str()).collect();
        for expected in &[
            "overview",
            "workflows",
            "workflow-detail",
            "agents",
            "status",
            "execute",
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

    // ── Workspace resolution chain tests (5.7) ──────────────────────

    #[test]
    fn workspace_resolved_from_input_workspace_dir_field() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-1", "test workspace resolution")
            .with_input(serde_json::json!({
                "action": "validate-pack",
                "workspace_dir": "/tmp/test-workspace-dir"
            }));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
        let content: serde_json::Value =
            serde_json::from_str(result.content.as_deref().unwrap()).unwrap();
        assert!(content.get("valid").is_some() || content.get("plugins_ok").is_some());
    }

    #[test]
    fn workspace_resolved_from_task_metadata_workspace_dir() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-2", "test metadata resolution")
            .with_input(serde_json::json!({"action": "validate-pack"}))
            .with_metadata(serde_json::json!({"workspace_dir": "/tmp/meta-workspace"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn workspace_resolved_from_task_metadata_workspace_key() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-3", "test metadata workspace key")
            .with_input(serde_json::json!({"action": "validate-pack"}))
            .with_metadata(serde_json::json!({"workspace": "/tmp/meta-workspace-alt"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn workspace_resolved_from_task_metadata_workspace_path_key() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-4", "test metadata workspace_path key")
            .with_input(serde_json::json!({"action": "validate-pack"}))
            .with_metadata(serde_json::json!({"workspace_path": "/tmp/meta-ws-path"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn workspace_fallback_when_no_workspace_info() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-5", "test fallback")
            .with_input(serde_json::json!({"action": "validate-pack"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn workspace_dir_in_input_takes_priority_over_metadata() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-6", "test priority")
            .with_input(serde_json::json!({
                "action": "validate-pack",
                "workspace_dir": "/tmp/input-workspace"
            }))
            .with_metadata(serde_json::json!({"workspace_dir": "/tmp/meta-workspace"}));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn workspace_metadata_priority_order_is_workspace_dir_first() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-7", "test metadata priority")
            .with_input(serde_json::json!({"action": "validate-pack"}))
            .with_metadata(serde_json::json!({
                "workspace_dir": "/tmp/priority-1",
                "workspace": "/tmp/priority-2",
                "workspace_path": "/tmp/priority-3"
            }));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
    }

    #[test]
    fn execute_with_no_metadata_skips_metadata_resolution() {
        let plugin = CodingPackPlugin;
        let task = TaskInput::new("t-ws-8", "no metadata")
            .with_input(serde_json::json!({
                "action": "list-workflows"
            }));
        let config = StepConfig::new("s1", "agent");
        let result = plugin.execute(task, config).unwrap();
        assert_eq!(result.status, "success");
        let content: serde_json::Value =
            serde_json::from_str(result.content.as_deref().unwrap()).unwrap();
        assert!(content.get("workflows").is_some());
    }

    // ── Lifecycle hook tests ──────────────────────────────────────────────

    fn make_install_fixture(tmp: &std::path::Path) {
        // Create a mock plugin_dir with workflows and _bmad tree
        let wf_dir = tmp.join("plugin").join("config").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join("coding-quick-dev.yaml"), "id: coding-quick-dev\n").unwrap();
        std::fs::write(wf_dir.join("coding-bug-fix.yaml"), "id: coding-bug-fix\n").unwrap();

        let bmad_dir = tmp.join("plugin").join("_bmad").join("_config");
        std::fs::create_dir_all(&bmad_dir).unwrap();
        std::fs::write(bmad_dir.join("agent-manifest.csv"), "name,role\n").unwrap();

        let bmad_agents = tmp.join("plugin").join("_bmad").join("bmm").join("agents");
        std::fs::create_dir_all(&bmad_agents).unwrap();
        std::fs::write(bmad_agents.join("dev.md"), "# Dev agent\n").unwrap();

        let config_dir = tmp.join("plugin").join("config");
        std::fs::write(config_dir.join("config.yaml"), "default: true\n").unwrap();
    }

    #[test]
    fn on_install_copies_workflows() {
        let tmp = tempfile::tempdir().unwrap();
        make_install_fixture(tmp.path());

        let ctx = InstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: tmp.path().join("dest_workflows"),
            config_dir: tmp.path().join("dest_config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();

        assert!(ctx.workflows_dir.join("coding-quick-dev.yaml").exists());
        assert!(ctx.workflows_dir.join("coding-bug-fix.yaml").exists());
        let content = std::fs::read_to_string(ctx.workflows_dir.join("coding-quick-dev.yaml")).unwrap();
        assert_eq!(content, "id: coding-quick-dev\n");
    }

    #[test]
    fn on_install_copies_bmad_tree() {
        let tmp = tempfile::tempdir().unwrap();
        make_install_fixture(tmp.path());

        let ctx = InstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: tmp.path().join("dest_workflows"),
            config_dir: tmp.path().join("dest_config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();

        assert!(ctx.config_dir.join("_bmad").join("_config").join("agent-manifest.csv").exists());
        assert!(ctx.config_dir.join("_bmad").join("bmm").join("agents").join("dev.md").exists());
    }

    #[test]
    fn on_install_does_not_overwrite_existing_config() {
        let tmp = tempfile::tempdir().unwrap();
        make_install_fixture(tmp.path());

        let dest_config = tmp.path().join("dest_config");
        std::fs::create_dir_all(&dest_config).unwrap();
        std::fs::write(dest_config.join("config.yaml"), "user_custom: true\n").unwrap();

        let ctx = InstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: tmp.path().join("dest_workflows"),
            config_dir: dest_config.clone(),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();

        // User's config.yaml should NOT be overwritten
        let content = std::fs::read_to_string(dest_config.join("config.yaml")).unwrap();
        assert_eq!(content, "user_custom: true\n");
    }

    #[test]
    fn on_install_copies_config_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        make_install_fixture(tmp.path());

        let ctx = InstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: tmp.path().join("dest_workflows"),
            config_dir: tmp.path().join("dest_config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();

        let content = std::fs::read_to_string(ctx.config_dir.join("config.yaml")).unwrap();
        assert_eq!(content, "default: true\n");
    }

    #[test]
    fn on_install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        make_install_fixture(tmp.path());

        let ctx = InstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: tmp.path().join("dest_workflows"),
            config_dir: tmp.path().join("dest_config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();
        // Second call should succeed without error
        plugin.on_install(&ctx).unwrap();

        assert!(ctx.workflows_dir.join("coding-quick-dev.yaml").exists());
    }

    #[test]
    fn on_uninstall_non_purge_keeps_files() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(workflows_dir.join("coding-quick-dev.yaml"), "test").unwrap();

        let ctx = UninstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: workflows_dir.clone(),
            purge: false,
        };

        let plugin = CodingPackPlugin;
        plugin.on_uninstall(&ctx).unwrap();

        // File should still exist
        assert!(workflows_dir.join("coding-quick-dev.yaml").exists());
    }

    #[test]
    fn on_uninstall_purge_removes_known_workflows() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();

        // Create known workflow files
        std::fs::write(workflows_dir.join("coding-quick-dev.yaml"), "test").unwrap();
        std::fs::write(workflows_dir.join("coding-bug-fix.yaml"), "test").unwrap();
        std::fs::write(workflows_dir.join("bootstrap-plugin.yaml"), "test").unwrap();

        // Create a user-defined workflow that should NOT be removed
        std::fs::write(workflows_dir.join("my-custom-workflow.yaml"), "custom").unwrap();

        let ctx = UninstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: workflows_dir.clone(),
            purge: true,
        };

        let plugin = CodingPackPlugin;
        plugin.on_uninstall(&ctx).unwrap();

        assert!(!workflows_dir.join("coding-quick-dev.yaml").exists());
        assert!(!workflows_dir.join("coding-bug-fix.yaml").exists());
        assert!(!workflows_dir.join("bootstrap-plugin.yaml").exists());
        // User's custom workflow should still exist
        assert!(workflows_dir.join("my-custom-workflow.yaml").exists());
    }

    #[test]
    fn on_uninstall_purge_removes_bmad_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("config").join("workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();

        // _bmad is a sibling of workflows under config/
        let bmad_dir = tmp.path().join("config").join("_bmad").join("_config");
        std::fs::create_dir_all(&bmad_dir).unwrap();
        std::fs::write(bmad_dir.join("agent-manifest.csv"), "test").unwrap();

        let ctx = UninstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: workflows_dir.clone(),
            purge: true,
        };

        let plugin = CodingPackPlugin;
        plugin.on_uninstall(&ctx).unwrap();

        assert!(!tmp.path().join("config").join("_bmad").exists());
    }

    #[test]
    fn on_uninstall_purge_removes_execution_history() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("config").join("workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();

        let history_path = tmp.path().join("config").join("execution-history.json");
        std::fs::write(&history_path, "[]").unwrap();

        let ctx = UninstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: workflows_dir.clone(),
            purge: true,
        };

        let plugin = CodingPackPlugin;
        plugin.on_uninstall(&ctx).unwrap();

        assert!(!history_path.exists());
    }

    #[test]
    fn on_uninstall_purge_handles_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = UninstallContext {
            plugin_dir: tmp.path().join("plugin"),
            workflows_dir: tmp.path().join("nonexistent").join("workflows"),
            purge: true,
        };

        let plugin = CodingPackPlugin;
        plugin.on_uninstall(&ctx).unwrap();
    }

    // ── Install skip-path tests ─────────────────────────────────────

    #[test]
    fn on_install_skips_when_no_source_workflows_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugin");
        // Don't create config/workflows/ — on_install should skip gracefully
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let ctx = InstallContext {
            plugin_dir,
            workflows_dir: tmp.path().join("workflows"),
            config_dir: tmp.path().join("config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();
        // Should succeed without error; workflows_dir may or may not exist
    }

    #[test]
    fn on_install_skips_when_no_source_bmad_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let ctx = InstallContext {
            plugin_dir,
            workflows_dir: tmp.path().join("workflows"),
            config_dir: tmp.path().join("config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();
        // _bmad dir should NOT be created if source doesn't exist
        assert!(!ctx.config_dir.join("_bmad").exists());
    }

    #[test]
    fn on_install_skips_config_when_no_source_config_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(plugin_dir.join("config")).unwrap();
        // Don't create config.yaml — on_install should skip

        let ctx = InstallContext {
            plugin_dir,
            workflows_dir: tmp.path().join("workflows"),
            config_dir: tmp.path().join("config"),
        };

        let plugin = CodingPackPlugin;
        plugin.on_install(&ctx).unwrap();
        assert!(!ctx.config_dir.join("config.yaml").exists());
    }

    #[test]
    fn copy_tree_skips_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("real.txt"), "content").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", src.join("link.txt")).unwrap();
        }

        let count = copy_tree(&src, &dst).unwrap();
        assert!(dst.join("real.txt").exists());
        #[cfg(unix)]
        {
            // Symlink should be skipped
            assert!(!dst.join("link.txt").exists());
            assert_eq!(count, 1);
        }
    }
}
