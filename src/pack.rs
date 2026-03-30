use crate::util::is_executable;
use crate::validator;
use crate::workspace::WorkspaceConfig;
use pulse_plugin_sdk::error::WitPluginError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CodingPackInput {
    pub action: String,
    /// Optional: target plugin name for plugin-specific actions
    #[serde(default)]
    pub target: Option<String>,
    /// Workflow ID for execute-workflow action
    #[serde(default)]
    pub workflow_id: Option<String>,
    /// User input / task description for execute-workflow action
    #[serde(default)]
    pub input: Option<String>,
    /// Data endpoint path for data-query action (set by Pulse proxy)
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Mutation payload for data-mutate action
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    /// Optional workspace root directory path override.
    /// If not set, falls back to PULSE_WORKSPACE_DIR env var, then current directory.
    #[serde(default, alias = "workspace_path")]
    pub workspace_dir: Option<String>,
    /// Workspace name for Pulse API task filtering (e.g. "Default", "my-project").
    /// This is NOT a filesystem path — it's the Pulse workspace identifier.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Board ID for multi-board filtering within a workspace.
    #[serde(default)]
    pub board_id: Option<String>,
}

/// Execute a pack-level action.
///
/// Actions that previously called into local modules (auto_dev, executor,
/// github_client, github_sync, worktree_tracker) now delegate to platform
/// plugins via `plugin_bridge`.
pub fn execute_action(input: &CodingPackInput) -> Result<String, WitPluginError> {
    let config = WorkspaceConfig::resolve(input.workspace_dir.as_deref());

    match input.action.as_str() {
        // ── Local pack operations (no delegation needed) ───────────────
        "validate-pack" => to_json_string(validate_pack_value(&config)),
        "validate-workflows" => to_json_string(validate_workflows_value(&config)),
        "list-workflows" => to_json_string(list_workflows_value(&config)),
        "list-plugins" => to_json_string(list_plugins_value(&config)),
        "status" => to_json_string(pack_status_value(&config)),
        "data-query" => {
            let endpoint = input.endpoint.as_deref().unwrap_or("");
            execute_data_query(endpoint, &config, input.workspace.as_deref(), input.board_id.as_deref())
        }
        "data-mutate" => {
            let endpoint = input.endpoint.as_deref().unwrap_or("");
            let payload = input.payload.clone().unwrap_or(serde_json::Value::Null);
            execute_data_mutate(endpoint, &payload, &config, input.workspace.as_deref(), input.board_id.as_deref())
        }
        #[cfg(not(target_arch = "wasm32"))]
        "generate-agents-yaml" => {
            to_json_string(generate_agents_yaml(&config))
        }
        #[cfg(target_arch = "wasm32")]
        "generate-agents-yaml" => {
            Err(WitPluginError::internal("generate-agents-yaml is not available in WASM builds"))
        }

        // ── Delegated to plugin-auto-loop via plugin_bridge ────────────
        "execute-workflow" => {
            let workflow_id = input.workflow_id.as_deref().ok_or_else(|| {
                WitPluginError::invalid_input("execute-workflow requires 'workflow_id'")
            })?;
            if !config.is_workflow_enabled(workflow_id) {
                return Err(WitPluginError::not_found(format!(
                    "Workflow '{}' is disabled in this workspace",
                    workflow_id
                )));
            }
            let user_input = input.input.as_deref().unwrap_or("");
            to_json_string(crate::plugin_bridge::execute_workflow(
                workflow_id,
                user_input,
                &config,
            ))
        }
        "auto-dev-status" => to_json_string(crate::plugin_bridge::auto_loop_status(&config)),
        "auto-dev-next" => {
            let result = crate::plugin_bridge::auto_loop_next(&config)?;
            match result {
                Some(r) => to_json_string(
                    serde_json::to_value(&r)
                        .map_err(|e| WitPluginError::internal(format!("JSON error: {e}"))),
                ),
                None => Ok(r#"{"status":"idle","message":"No ready-for-dev tasks found"}"#.to_string()),
            }
        }
        "auto-dev-watch" => {
            let max = input
                .payload
                .as_ref()
                .and_then(|p| p.get("max_iterations"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let results = crate::plugin_bridge::auto_loop_watch(&config, max)?;
            to_json_string(
                serde_json::to_value(&results)
                    .map_err(|e| WitPluginError::internal(format!("JSON error: {e}"))),
            )
        }

        // ── Delegated to plugin-issue-sync via plugin_bridge ───────────
        "sync-github-issues" => {
            to_json_string(crate::plugin_bridge::sync_github_issues(&config))
        }

        // ── Delegated to plugin-workspace-tracker via plugin_bridge ────
        "cleanup-worktrees" => {
            to_json_string(crate::plugin_bridge::cleanup_worktrees(&config))
        }
        "worktree-status" => {
            to_json_string(crate::plugin_bridge::worktree_status(&config))
        }
        "recover-worktrees" => {
            to_json_string(crate::plugin_bridge::recover_worktrees(&config))
        }

        // ── Delegated to plugin-feedback-loop via plugin_bridge ────────
        "check-pr-reviews" => {
            to_json_string(crate::plugin_bridge::check_pr_reviews(&config))
        }
        "build-fix-context" => {
            let pr_number = input
                .payload
                .as_ref()
                .and_then(|p| p.get("pr_number"))
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    WitPluginError::invalid_input(
                        "build-fix-context requires 'pr_number' in payload",
                    )
                })?;
            to_json_string(crate::plugin_bridge::build_fix_context(pr_number))
        }

        other => Err(WitPluginError::not_found(format!(
            "Unknown action: '{}'. Available: validate-pack, validate-workflows, list-workflows, list-plugins, status, execute-workflow, data-query, data-mutate, auto-dev-status, auto-dev-next, auto-dev-watch, sync-github-issues, cleanup-worktrees, worktree-status, recover-worktrees, check-pr-reviews, build-fix-context, generate-agents-yaml",
            other
        ))),
    }
}

/// Generate agents.yaml ACL configuration from the BMAD agent registry.
#[cfg(not(target_arch = "wasm32"))]
fn generate_agents_yaml(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    use std::collections::BTreeMap;

    let manifest_path = config.base_dir.join("_bmad/_config/agent-manifest.csv");
    let registry = crate::agent_registry::BmadAgentRegistry::new(&manifest_path);

    let agents = {
        use pulse_plugin_sdk::traits::agent_definition::AgentDefinitionProvider;
        registry.list_agents(None)
    };

    let mut agents_map: BTreeMap<String, BTreeMap<String, serde_yaml::Value>> = BTreeMap::new();

    for agent in &agents {
        let acl = registry.get_acl(&agent.name);
        let mut entry: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();

        entry.insert(
            "allowed_tools".to_string(),
            serde_yaml::to_value(agent.tools.as_ref().cloned().unwrap_or_default())
                .map_err(|e| WitPluginError::internal(format!("YAML error: {e}")))?,
        );
        entry.insert(
            "can_invoke".to_string(),
            serde_yaml::to_value(&acl.can_invoke)
                .map_err(|e| WitPluginError::internal(format!("YAML error: {e}")))?,
        );
        entry.insert(
            "can_respond_to".to_string(),
            serde_yaml::to_value(&acl.can_respond_to)
                .map_err(|e| WitPluginError::internal(format!("YAML error: {e}")))?,
        );
        entry.insert(
            "description".to_string(),
            serde_yaml::Value::String(agent.description.clone().unwrap_or_default()),
        );
        entry.insert(
            "max_budget_usd".to_string(),
            serde_yaml::to_value(5.0_f64)
                .map_err(|e| WitPluginError::internal(format!("YAML error: {e}")))?,
        );
        entry.insert(
            "max_turns".to_string(),
            serde_yaml::to_value(25_u32)
                .map_err(|e| WitPluginError::internal(format!("YAML error: {e}")))?,
        );
        entry.insert(
            "model".to_string(),
            serde_yaml::Value::String("claude-sonnet-4-20250514".to_string()),
        );
        entry.insert(
            "timeout_secs".to_string(),
            serde_yaml::to_value(300_u32)
                .map_err(|e| WitPluginError::internal(format!("YAML error: {e}")))?,
        );

        agents_map.insert(agent.name.clone(), entry);
    }

    let yaml_body = serde_yaml::to_string(&agents_map)
        .map_err(|e| WitPluginError::internal(format!("YAML serialization error: {e}")))?;
    let output = format!(
        "# Generated by plugin-coding-pack. Do not edit manually.\n\n{}",
        yaml_body
    );

    if let Some(p) = config.agent_mesh.agents_yaml_path.as_deref() {
        if p.contains("..") || std::path::Path::new(p).is_absolute() {
            return Err(WitPluginError::invalid_input(
                "agents_yaml_path must be a relative path without '..' segments",
            ));
        }
    }

    let output_path = config
        .agent_mesh
        .agents_yaml_path
        .as_deref()
        .map(|p| config.base_dir.join(p))
        .unwrap_or_else(|| config.base_dir.join("config/agents.yaml"));

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| WitPluginError::internal(format!("cannot create directory: {e}")))?;
    }
    std::fs::write(&output_path, &output)
        .map_err(|e| WitPluginError::internal(format!("cannot write agents.yaml: {e}")))?;

    Ok(serde_json::json!({
        "status": "generated",
        "path": output_path.display().to_string(),
        "agent_count": agents_map.len(),
    }))
}

fn to_json_string(
    result: Result<serde_json::Value, WitPluginError>,
) -> Result<String, WitPluginError> {
    result.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
}

fn validate_pack_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let mut issues = Vec::new();
    let mut ok_count = 0;

    // Check required plugins
    let required_plugins = ["bmad-method", "provider-claude-code"];
    let optional_plugins = ["plugin-git-worktree", "plugin-memory"];

    for plugin in &required_plugins {
        let path = config.plugins_dir.join(plugin);
        if path.exists() {
            ok_count += 1;
        } else {
            issues.push(format!("MISSING required plugin: {}", plugin));
        }
    }

    for plugin in &optional_plugins {
        let path = config.plugins_dir.join(plugin);
        if path.exists() {
            ok_count += 1;
        } else {
            issues.push(format!(
                "MISSING optional plugin: {} (non-blocking)",
                plugin
            ));
        }
    }

    // Check workflow files
    let workflow_count = if config.workflows_dir.exists() {
        std::fs::read_dir(&config.workflows_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("yaml"))
                    .count()
            })
            .unwrap_or(0)
    } else {
        issues.push(format!(
            "MISSING workflows directory: {}",
            config.workflows_dir.display()
        ));
        0
    };

    Ok(serde_json::json!({
        "valid": issues.iter().all(|i| i.contains("optional") || i.contains("non-blocking")),
        "plugins_ok": ok_count,
        "workflows_found": workflow_count,
        "issues": issues,
    }))
}

fn validate_workflows_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    if !config.workflows_dir.exists() {
        return Ok(serde_json::json!({
            "valid": false,
            "results": [],
            "issues": [format!("workflows directory not found: {}", config.workflows_dir.display())],
        }));
    }

    let mut results = Vec::new();
    let mut all_valid = true;

    let mut entries: Vec<_> = std::fs::read_dir(&config.workflows_dir)
        .map_err(|e| WitPluginError::internal(format!("cannot read workflows dir: {}", e)))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("yaml"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        match validator::validate_workflow_file(&path, &config.plugins_dir) {
            Ok(result) => {
                if !result.valid {
                    all_valid = false;
                }
                results.push(serde_json::json!({
                    "file": result.file,
                    "valid": result.valid,
                    "issues": result.issues,
                }));
            }
            Err(e) => {
                all_valid = false;
                results.push(serde_json::json!({
                    "file": path.display().to_string(),
                    "valid": false,
                    "issues": [e],
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "valid": all_valid,
        "count": results.len(),
        "results": results,
    }))
}

fn list_workflows_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let mut workflows = Vec::new();

    if config.workflows_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&config.workflows_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                        if config.is_workflow_enabled(name) {
                            workflows.push(name.to_string());
                        }
                    }
                }
            }
        }
    }

    workflows.sort();
    Ok(serde_json::json!({
        "workflows": workflows,
        "count": workflows.len(),
    }))
}

fn list_plugins_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let mut plugins = Vec::new();

    if config.plugins_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&config.plugins_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if !name.starts_with('.') {
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        plugins.push(serde_json::json!({
                            "name": name,
                            "size_bytes": size,
                            "executable": is_executable(&path),
                        }));
                    }
                }
            }
        }
    }

    Ok(serde_json::json!({
        "plugins": plugins,
        "count": plugins.len(),
    }))
}

fn pack_status_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    Ok(serde_json::json!({
        "validation": validate_pack_value(config)?,
        "workflows": list_workflows_value(config)?,
        "plugins": list_plugins_value(config)?,
    }))
}

/// Handle data-query requests from dashboard proxy.
/// Routes endpoint paths to internal data functions.
/// `workspace` is the Pulse workspace name (not a path) for task filtering.
fn execute_data_query(
    endpoint: &str,
    config: &WorkspaceConfig,
    workspace: Option<&str>,
    board_id: Option<&str>,
) -> Result<String, WitPluginError> {
    let endpoint = endpoint.trim_start_matches('/');
    let result = match endpoint {
        "status" => pack_status_value(config)?,
        "status/health" => status_health_value(config)?,
        "workflows/list" => list_workflows_detail_value(config)?,
        "agents/list" => list_agents_value(config)?,
        // Local sprint badge — delegates to auto-loop for task counts
        "board/summary" => board_summary_value(config)?,
        // Active worktrees list — delegates to plugin-workspace-tracker
        "worktrees/list" => crate::plugin_bridge::worktrees_list(config)?,
        ep if ep.starts_with("tasks/") && ep.ends_with("/workflow-context") => {
            let task_id = ep
                .strip_prefix("tasks/")
                .and_then(|s| s.strip_suffix("/workflow-context"))
                .unwrap_or("");
            task_workflow_context_value(task_id, config)?
        }
        ep if ep.starts_with("tasks/") && ep.ends_with("/agent-info") => {
            let task_id = ep
                .strip_prefix("tasks/")
                .and_then(|s| s.strip_suffix("/agent-info"))
                .unwrap_or("");
            task_agent_info_value(task_id, config)?
        }
        ep if ep.starts_with("workflows/") => {
            let id = ep.strip_prefix("workflows/").unwrap_or("");
            get_workflow_detail_value(id, config)?
        }
        // Board data queries — proxied to plugin-board
        ep if ep.starts_with("board/") => {
            crate::plugin_bridge::board_query(ep, workspace, board_id, config)?
        }
        _ => {
            return Err(WitPluginError::not_found(format!(
                "Unknown data endpoint: '{}'. Available: status, status/health, workflows/list, agents/list, workflows/{{id}}, tasks/{{id}}/workflow-context, tasks/{{id}}/agent-info, board/summary, board/data, board/boards/list, board/epics/list, board/filters, board/epics/{{id}}, board/stories/{{id}}, board/assignments/{{id}}, worktrees/list",
                endpoint
            )));
        }
    };
    serde_json::to_string_pretty(&result)
        .map_err(|e| WitPluginError::internal(format!("JSON serialization error: {e}")))
}

/// Health badge data for the dashboard.
/// Derives status from `validate_pack_value()`: healthy if validation passes, degraded otherwise.
fn status_health_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let validation = validate_pack_value(config)?;
    let valid = validation["valid"].as_bool().unwrap_or(false);
    let plugins_ok = valid;
    let workflows_found = validation["workflows_found"].as_u64().unwrap_or(0);
    let pack_status = if valid { "healthy" } else { "degraded" };

    Ok(serde_json::json!({
        "pack_status": pack_status,
        "plugins_ok": plugins_ok,
        "workflows_found": workflows_found,
    }))
}

/// Workflow context for a specific task (dashboard task-view widget).
/// Attempts to fetch task metadata from the Pulse API; returns minimal defaults on failure.
fn task_workflow_context_value(
    task_id: &str,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    // Try to get task details from Pulse API
    if let Ok(task) = crate::pulse_api::get_task(task_id) {
        let workflow_id = if task.workflow_id.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(task.workflow_id)
        };
        return Ok(serde_json::json!({
            "task_id": task_id,
            "workflow_id": workflow_id,
            "step_id": null,
            "executor": null,
            "model_tier": null,
        }));
    }

    // Try auto-loop status for additional context
    if let Ok(status) = crate::plugin_bridge::auto_loop_status(config) {
        if let Some(current) = status.get("current_task") {
            if current.get("task_id").and_then(|v| v.as_str()) == Some(task_id) {
                return Ok(serde_json::json!({
                    "task_id": task_id,
                    "workflow_id": current.get("workflow_id").cloned().unwrap_or(serde_json::Value::Null),
                    "step_id": current.get("step_id").cloned().unwrap_or(serde_json::Value::Null),
                    "executor": current.get("executor").cloned().unwrap_or(serde_json::Value::Null),
                    "model_tier": current.get("model_tier").cloned().unwrap_or(serde_json::Value::Null),
                }));
            }
        }
    }

    // Fallback: return minimal JSON with just the task_id
    Ok(serde_json::json!({
        "task_id": task_id,
        "workflow_id": null,
        "step_id": null,
        "executor": null,
        "model_tier": null,
    }))
}

/// Agent info for a specific task (dashboard task-view badge).
/// Looks up agent from task metadata or falls back to defaults.
fn task_agent_info_value(
    task_id: &str,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    // Try to get task details and look for agent_name in metadata
    if let Ok(task) = crate::pulse_api::get_task(task_id) {
        // If the task has a workflow_id, try to infer agent from workflow
        if !task.workflow_id.is_empty() {
            // Check if we can look up the agent in the registry
            #[cfg(not(target_arch = "wasm32"))]
            {
                let manifest_path = config.base_dir.join("_bmad/_config/agent-manifest.csv");
                let registry = crate::agent_registry::BmadAgentRegistry::new(&manifest_path);
                let agents = {
                    use pulse_plugin_sdk::traits::agent_definition::AgentDefinitionProvider;
                    registry.list_agents(None)
                };
                // Try to match agent from workflow name convention (e.g. "coding-quick-dev" -> "bmad/quick-flow-solo-dev")
                if let Some(agent) = agents.first() {
                    let (display_name, title) = agent
                        .description
                        .as_deref()
                        .and_then(|d| d.split_once(" \u{2014} "))
                        .map(|(name, role)| (name.to_string(), role.to_string()))
                        .unwrap_or_else(|| (agent.name.clone(), String::new()));
                    // Return the first matching agent as a reasonable default
                    return Ok(serde_json::json!({
                        "task_id": task_id,
                        "agent_name": agent.name,
                        "display_name": display_name,
                        "title": title,
                    }));
                }
            }
        }
    }

    // Try auto-loop status for agent info on the current task
    if let Ok(status) = crate::plugin_bridge::auto_loop_status(config) {
        if let Some(current) = status.get("current_task") {
            if current.get("task_id").and_then(|v| v.as_str()) == Some(task_id) {
                if let Some(agent_name) = current.get("agent_name").and_then(|v| v.as_str()) {
                    return Ok(serde_json::json!({
                        "task_id": task_id,
                        "agent_name": agent_name,
                        "display_name": current.get("display_name").and_then(|v| v.as_str()).unwrap_or(agent_name),
                        "title": current.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    }));
                }
            }
        }
    }

    let _ = config;

    // Fallback: return default agent
    Ok(serde_json::json!({
        "task_id": task_id,
        "agent_name": "bmad-dev",
        "display_name": "Amelia",
        "title": "Developer",
    }))
}

/// Compact sprint/board progress summary for the dashboard badge.
/// Calls auto-loop status to get board task counts.
fn board_summary_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    // Try to get board status from plugin-auto-loop
    if let Ok(status) = crate::plugin_bridge::auto_loop_status(config) {
        let total = status.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
        let done = status.get("done").and_then(|v| v.as_u64()).unwrap_or(0);
        let in_progress = status
            .get("in_progress")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let blocked = status
            .get("blocked")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let ready = status.get("ready").and_then(|v| v.as_u64()).unwrap_or(0);

        let sprint_progress = if total > 0 && done == total {
            "completed"
        } else if blocked > 0 {
            "at-risk"
        } else {
            "on-track"
        };

        return Ok(serde_json::json!({
            "sprint_progress": sprint_progress,
            "total": total,
            "done": done,
            "in_progress": in_progress,
            "ready": ready,
        }));
    }

    // Fallback: return defaults when auto-loop is unavailable
    Ok(serde_json::json!({
        "sprint_progress": "on-track",
        "total": 0,
        "done": 0,
        "in_progress": 0,
        "ready": 0,
    }))
}

/// Detailed workflow list for dashboard table view.
fn list_workflows_detail_value(
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let mut workflows = Vec::new();

    if config.workflows_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&config.workflows_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown");

                    if !config.is_workflow_enabled(id) {
                        continue;
                    }

                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(wf) = serde_yaml::from_str::<serde_json::Value>(&content) {
                            let category = if id.starts_with("bootstrap") {
                                "bootstrap"
                            } else {
                                "coding"
                            };
                            workflows.push(serde_json::json!({
                                "id": id,
                                "description": wf.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                                "category": category,
                                "step_count": wf.get("steps").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
                                "requires": wf.get("requires").and_then(|r| r.as_array()).map(|arr| {
                                    arr.iter().filter_map(|r| r.get("plugin").and_then(|p| p.as_str())).collect::<Vec<_>>().join(", ")
                                }).unwrap_or_default(),
                            }));
                        }
                    }
                }
            }
        }
    }

    workflows.sort_by(|a, b| {
        let a_id = a["id"].as_str().unwrap_or("");
        let b_id = b["id"].as_str().unwrap_or("");
        a_id.cmp(b_id)
    });

    Ok(serde_json::json!(workflows))
}

/// BMAD agent list for dashboard table view.
/// Uses the live BmadAgentRegistry for consistent, authoritative agent data.
#[cfg(not(target_arch = "wasm32"))]
fn list_agents_value(config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    let manifest_path = config.base_dir.join("_bmad/_config/agent-manifest.csv");
    let registry = crate::agent_registry::BmadAgentRegistry::new(&manifest_path);

    let agents = {
        use pulse_plugin_sdk::traits::agent_definition::AgentDefinitionProvider;
        registry.list_agents(None)
    };

    if agents.is_empty() {
        return Ok(serde_json::json!([]));
    }

    let result: Vec<serde_json::Value> = agents
        .iter()
        .map(|a| {
            // Extract display name and role from description format "DisplayName \u{2014} Role Title"
            let (display_name, role) = a
                .description
                .as_deref()
                .and_then(|d| d.split_once(" \u{2014} "))
                .map(|(name, role)| (name.to_string(), role.to_string()))
                .unwrap_or_else(|| (a.name.clone(), String::new()));

            serde_json::json!({
                "id": a.name,
                "name": display_name,
                "role": role,
                "description": a.description.as_deref().unwrap_or(""),
                "model_tier": a.model_tier.as_deref().unwrap_or("balanced"),
                "skills": a.skills.as_ref().cloned().unwrap_or_default(),
                "tools": a.tools.as_ref().cloned().unwrap_or_default(),
            })
        })
        .collect();

    Ok(serde_json::json!(result))
}

#[cfg(target_arch = "wasm32")]
fn list_agents_value(_config: &WorkspaceConfig) -> Result<serde_json::Value, WitPluginError> {
    // Fallback for WASM builds -- registry not available
    Ok(serde_json::json!([]))
}

/// Single workflow detail for dashboard detail view.
fn get_workflow_detail_value(
    workflow_id: &str,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let path = config.workflows_dir.join(format!("{}.yaml", workflow_id));
    if !path.exists() {
        return Err(WitPluginError::not_found(format!(
            "Workflow '{}' not found",
            workflow_id
        )));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| WitPluginError::internal(format!("Cannot read workflow: {e}")))?;
    let wf: serde_json::Value = serde_yaml::from_str(&content)
        .map_err(|e| WitPluginError::internal(format!("Invalid YAML: {e}")))?;

    let steps = wf
        .get("steps")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let step_pipeline: Vec<String> = steps
        .iter()
        .filter_map(|s| {
            s.get("id")
                .and_then(|id| id.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    let category = if workflow_id.starts_with("bootstrap") {
        "bootstrap"
    } else {
        "coding"
    };

    Ok(serde_json::json!({
        "id": workflow_id,
        "description": wf.get("description").and_then(|d| d.as_str()).unwrap_or(""),
        "category": category,
        "step_count": steps.len(),
        "step_pipeline": step_pipeline.join(" → "),
        "requires": wf.get("requires").and_then(|r| r.as_array()).map(|arr| {
            arr.iter().filter_map(|r| r.get("plugin").and_then(|p| p.as_str())).collect::<Vec<_>>().join(", ")
        }).unwrap_or_default(),
    }))
}

/// Handle data-mutate requests. Board mutations are proxied to plugin-board.
fn execute_data_mutate(
    endpoint: &str,
    payload: &serde_json::Value,
    config: &WorkspaceConfig,
    workspace: Option<&str>,
    board_id: Option<&str>,
) -> Result<String, WitPluginError> {
    let endpoint = endpoint.trim_start_matches('/');
    if endpoint.starts_with("board/") {
        let result = crate::plugin_bridge::board_mutate(endpoint, payload, workspace, board_id, config)?;
        return serde_json::to_string_pretty(&result)
            .map_err(|e| WitPluginError::internal(format!("JSON serialization error: {e}")));
    }
    Err(WitPluginError::not_found(format!(
        "Unknown mutation endpoint: '{}'. Available: board/sync, board/status/{{id}}, board/epics, board/epics/{{id}}, board/stories, board/stories/{{id}}, board/assignments, board/assignments/{{id}}",
        endpoint
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_input(action: &str) -> CodingPackInput {
        CodingPackInput {
            action: action.to_string(),
            target: None,
            workflow_id: None,
            input: None,
            endpoint: None,
            payload: None,
            workspace_dir: None,
            workspace: None,
            board_id: None,
        }
    }

    #[test]
    fn validate_pack_returns_valid_json() {
        let input = test_input("validate-pack");
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("plugins_ok").is_some());
        assert!(parsed.get("workflows_found").is_some());
    }

    #[test]
    fn validate_workflows_returns_valid_json() {
        let input = test_input("validate-workflows");
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("count").is_some());
        assert!(parsed.get("results").is_some());
    }

    #[test]
    fn list_workflows_returns_valid_json() {
        let input = test_input("list-workflows");
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("workflows").is_some());
        assert!(parsed.get("count").is_some());
    }

    #[test]
    fn list_plugins_returns_valid_json() {
        let input = test_input("list-plugins");
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("plugins").is_some());
    }

    #[test]
    fn unknown_action_returns_not_found() {
        let input = test_input("does-not-exist");
        let err = execute_action(&input).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    fn test_input_with_workspace(action: &str, workspace_dir: &str) -> CodingPackInput {
        CodingPackInput {
            action: action.to_string(),
            target: None,
            workflow_id: None,
            input: None,
            endpoint: None,
            payload: None,
            workspace_dir: Some(workspace_dir.to_string()),
            workspace: None,
            board_id: None,
        }
    }

    // ── Story 25-5: generate-agents-yaml ──────────────────────────────

    fn make_test_workspace_config() -> WorkspaceConfig {
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        WorkspaceConfig::from_base_dir(&base)
    }

    #[test]
    fn generate_agents_yaml_action_recognized() {
        // Point workspace at the project root so it can find the manifest
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let tmp = tempfile::tempdir().unwrap();
        // Copy manifest into temp workspace structure
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();
        let input = test_input_with_workspace("generate-agents-yaml", tmp.path().to_str().unwrap());
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["status"], "generated");
        assert_eq!(parsed["agent_count"], 9);
    }

    #[test]
    fn generate_agents_yaml_produces_valid_yaml_with_all_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        let result = generate_agents_yaml(&config).unwrap();
        assert_eq!(result["agent_count"], 9);

        // Read back and parse the YAML
        let yaml_path = tmp.path().join("config/agents.yaml");
        assert!(yaml_path.exists(), "agents.yaml should be written");
        let content = std::fs::read_to_string(&yaml_path).unwrap();

        // Verify header comment
        assert!(
            content.starts_with("# Generated by plugin-coding-pack. Do not edit manually."),
            "Should have header comment"
        );

        // Parse YAML body (skip comment lines)
        let parsed: std::collections::BTreeMap<String, serde_yaml::Value> =
            serde_yaml::from_str(&content).unwrap();
        assert_eq!(parsed.len(), 9, "should have 9 agents");
    }

    #[test]
    fn generate_agents_yaml_alphabetical_ordering() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        generate_agents_yaml(&config).unwrap();

        let yaml_path = tmp.path().join("config/agents.yaml");
        let content = std::fs::read_to_string(&yaml_path).unwrap();
        let parsed: std::collections::BTreeMap<String, serde_yaml::Value> =
            serde_yaml::from_str(&content).unwrap();

        let keys: Vec<&String> = parsed.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "agent keys should be alphabetically sorted");
    }

    #[test]
    fn generate_agents_yaml_each_agent_has_required_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        generate_agents_yaml(&config).unwrap();

        let yaml_path = tmp.path().join("config/agents.yaml");
        let content = std::fs::read_to_string(&yaml_path).unwrap();
        let parsed: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, serde_yaml::Value>,
        > = serde_yaml::from_str(&content).unwrap();

        let required_fields = [
            "description",
            "model",
            "max_turns",
            "max_budget_usd",
            "timeout_secs",
            "can_invoke",
            "can_respond_to",
            "allowed_tools",
        ];

        for (name, entry) in &parsed {
            for field in &required_fields {
                assert!(
                    entry.contains_key(*field),
                    "agent '{name}' should have field '{field}'"
                );
            }
        }
    }

    #[test]
    fn generate_agents_yaml_acl_rules_match_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        generate_agents_yaml(&config).unwrap();

        let yaml_path = tmp.path().join("config/agents.yaml");
        let content = std::fs::read_to_string(&yaml_path).unwrap();
        let parsed: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, serde_yaml::Value>,
        > = serde_yaml::from_str(&content).unwrap();

        // Architect should have specific can_invoke
        let architect = &parsed["bmad/architect"];
        let can_invoke: Vec<String> =
            serde_yaml::from_value(architect["can_invoke"].clone()).unwrap();
        assert_eq!(
            can_invoke,
            vec!["bmad/analyst", "bmad/developer", "bmad/ux-designer"]
        );

        // QA should have can_invoke = [bmad/developer]
        let qa = &parsed["bmad/qa"];
        let can_invoke: Vec<String> = serde_yaml::from_value(qa["can_invoke"].clone()).unwrap();
        assert_eq!(can_invoke, vec!["bmad/developer"]);

        // All agents should have can_respond_to = [bmad/pm, bmad/sm]
        for (name, entry) in &parsed {
            let respond_to: Vec<String> =
                serde_yaml::from_value(entry["can_respond_to"].clone()).unwrap();
            assert_eq!(
                respond_to,
                vec!["bmad/pm", "bmad/sm"],
                "agent {name} should respond to pm and sm"
            );
        }
    }

    #[test]
    fn generate_agents_yaml_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        generate_agents_yaml(&config).unwrap();
        let yaml_path = tmp.path().join("config/agents.yaml");
        let first = std::fs::read_to_string(&yaml_path).unwrap();

        generate_agents_yaml(&config).unwrap();
        let second = std::fs::read_to_string(&yaml_path).unwrap();

        assert_eq!(
            first, second,
            "running twice should produce identical output"
        );
    }

    #[test]
    fn generate_agents_yaml_custom_output_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let mut config = WorkspaceConfig::from_base_dir(tmp.path());
        config.agent_mesh.agents_yaml_path = Some("custom/my-agents.yaml".to_string());
        let result = generate_agents_yaml(&config).unwrap();
        let path = result["path"].as_str().unwrap();
        assert!(
            path.contains("custom/my-agents.yaml"),
            "should use custom path, got: {path}"
        );
        assert!(
            tmp.path().join("custom/my-agents.yaml").exists(),
            "custom path file should exist"
        );
    }

    // ── Story 25-6: list-agents refactored to use registry ────────────

    #[test]
    fn list_agents_returns_9_agents_from_registry() {
        let config = make_test_workspace_config();
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 9, "should have 9 agents from registry");
    }

    #[test]
    fn list_agents_uses_registry_names() {
        let config = make_test_workspace_config();
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();
        let ids: Vec<&str> = arr.iter().filter_map(|a| a["id"].as_str()).collect();

        // Registry names from CSV (authoritative source of truth)
        assert!(
            ids.contains(&"bmad/dev"),
            "should have bmad/dev from registry"
        );
        assert!(
            ids.contains(&"bmad/quick-flow-solo-dev"),
            "should have bmad/quick-flow-solo-dev from registry"
        );
        assert!(
            ids.contains(&"bmad/architect"),
            "should have bmad/architect"
        );
        assert!(ids.contains(&"bmad/analyst"), "should have bmad/analyst");
        assert!(ids.contains(&"bmad/pm"), "should have bmad/pm");
        assert!(ids.contains(&"bmad/qa"), "should have bmad/qa");
        assert!(ids.contains(&"bmad/sm"), "should have bmad/sm");
        assert!(
            ids.contains(&"bmad/tech-writer"),
            "should have bmad/tech-writer"
        );
        assert!(
            ids.contains(&"bmad/ux-designer"),
            "should have bmad/ux-designer"
        );

        // Old hardcoded incorrect names no longer present
        assert!(
            !ids.contains(&"bmad/quick-flow"),
            "should NOT have old bmad/quick-flow"
        );
    }

    #[test]
    fn list_agents_alphabetically_sorted() {
        let config = make_test_workspace_config();
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();
        let ids: Vec<&str> = arr.iter().filter_map(|a| a["id"].as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "agents should be sorted alphabetically by id");
    }

    #[test]
    fn list_agents_each_entry_has_required_fields() {
        let config = make_test_workspace_config();
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();

        for agent in arr {
            let id = agent["id"].as_str().unwrap_or("unknown");
            assert!(agent.get("id").is_some(), "agent should have 'id'");
            assert!(agent.get("name").is_some(), "agent {id} should have 'name'");
            assert!(agent.get("role").is_some(), "agent {id} should have 'role'");
            assert!(
                agent.get("description").is_some(),
                "agent {id} should have 'description'"
            );
            assert!(
                agent.get("model_tier").is_some(),
                "agent {id} should have 'model_tier'"
            );
            assert!(
                agent.get("skills").is_some(),
                "agent {id} should have 'skills'"
            );
            assert!(
                agent.get("tools").is_some(),
                "agent {id} should have 'tools'"
            );
        }
    }

    #[test]
    fn list_agents_display_name_and_role_parsed_from_description() {
        let config = make_test_workspace_config();
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();

        // Find architect
        let architect = arr
            .iter()
            .find(|a| a["id"].as_str() == Some("bmad/architect"))
            .expect("should find bmad/architect");
        assert_eq!(architect["name"].as_str(), Some("Winston"));
        assert!(
            architect["role"]
                .as_str()
                .unwrap_or("")
                .contains("Architect"),
            "architect role should contain 'Architect'"
        );
    }

    #[test]
    fn list_agents_graceful_degradation_missing_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let config = WorkspaceConfig::from_base_dir(tmp.path());
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();
        assert!(
            arr.is_empty(),
            "should return empty array when manifest is missing"
        );
    }

    // ── Delegated action dispatch tests ───────────────────────────────
    // These verify the action is recognized (dispatched to plugin_bridge)
    // even though the actual plugin call will fail without a running server.

    #[test]
    fn build_fix_context_action_recognized() {
        // Without pr_number in payload -> invalid_input, NOT not_found
        let input = test_input("build-fix-context");
        let err = execute_action(&input).unwrap_err();
        assert_eq!(
            err.code, "invalid_input",
            "build-fix-context should be recognized and require pr_number"
        );
    }
}
