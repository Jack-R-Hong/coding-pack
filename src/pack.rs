use crate::execution_history;
use crate::util::is_executable;
use crate::validator;
use crate::workspace::WorkspaceConfig;
use pulse_plugin_sdk::error::WitPluginError;
use serde::Deserialize;
use std::path::{Path, PathBuf};

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

/// Runtime status of a single plugin binary.
#[derive(Debug, Clone)]
pub struct PluginStatus {
    /// File name of the plugin binary (e.g. `"bmad-method"`).
    pub name: String,
    /// Absolute path to the binary.
    pub path: PathBuf,
    /// Whether the file exists on disk.
    pub exists: bool,
    /// Whether the file has executable permissions (Unix) or is a regular file (Windows).
    pub executable: bool,
    /// Whether the binary responded successfully to a JSON-RPC health probe.
    pub healthy: bool,
}

/// Validate all plugin binaries found in `plugins_dir`.
///
/// Iterates every non-hidden entry in the directory and for each one checks
/// whether the file exists, carries executable permissions, and responds to a
/// `{"method":"health"}` JSON-RPC probe.  Returns one [`PluginStatus`] per
/// discovered entry, sorted by name.  Returns an empty `Vec` when the
/// directory does not exist or cannot be read.
pub fn validate_plugins(plugins_dir: &Path) -> Vec<PluginStatus> {
    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let exists = path.exists();
            let executable = exists && is_executable(&path);
            let healthy = executable && probe_plugin_health(&path);
            PluginStatus {
                name,
                path,
                exists,
                executable,
                healthy,
            }
        })
        .collect()
}

/// Spawn `path` as a subprocess, write a minimal JSON-RPC health request to
/// its stdin, and return `true` iff it replies with a JSON object that
/// contains a `"result"` key within three seconds.
#[cfg(not(target_arch = "wasm32"))]
fn probe_plugin_health(path: &Path) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let mut child = match Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Send the health probe and close stdin.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"health\",\"params\":{}}\n");
    }

    // Read stdout on a background thread so we can apply a deadline.
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut out) = stdout {
            use std::io::Read;
            let _ = out.read_to_string(&mut buf);
        }
        let _ = tx.send(buf);
    });

    let output = match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(s) => s,
        Err(_) => {
            let _ = child.kill();
            return false;
        }
    };
    let _ = child.wait();

    // A healthy plugin returns JSON with a top-level "result" field.
    serde_json::from_str::<serde_json::Value>(&output)
        .map(|v| v.get("result").is_some())
        .unwrap_or(false)
}

/// WASM cannot spawn subprocesses; health is always reported as `false`.
#[cfg(target_arch = "wasm32")]
fn probe_plugin_health(_path: &Path) -> bool {
    false
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
            let result = crate::plugin_bridge::execute_workflow(
                workflow_id,
                user_input,
                &config,
            );

            // Record execution history (best-effort, don't fail on I/O errors)
            let success = result.is_ok();
            let record = execution_history::new_record(workflow_id, success);
            let _ = execution_history::save_execution_record(&config, &record);

            to_json_string(result)
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

        // ── Delegated to plugin-test-runner via plugin_bridge ──────────
        "run-tests" => {
            serde_json::to_string(&crate::plugin_bridge::run_tests(&config)?)
                .map_err(|e| WitPluginError::internal(format!("JSON error: {e}")))
        }

        other => Err(WitPluginError::not_found(format!(
            "Unknown action: '{}'. Available: validate-pack, validate-workflows, list-workflows, list-plugins, status, execute-workflow, data-query, data-mutate, auto-dev-status, auto-dev-next, auto-dev-watch, sync-github-issues, cleanup-worktrees, worktree-status, recover-worktrees, check-pr-reviews, build-fix-context, run-tests, generate-agents-yaml",
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
        ep if ep.starts_with("agents/") && ep != "agents/list" => {
            let agent_id = ep.strip_prefix("agents/").unwrap_or("");
            get_agent_detail_value(agent_id, config)?
        }
        ep if ep.starts_with("worktrees/") && ep != "worktrees/list" => {
            let _worktree_id = ep.strip_prefix("worktrees/").unwrap_or("");
            let status = crate::plugin_bridge::worktree_status(config)?;
            // If the response contains a list, filter to the requested worktree
            if let Some(worktrees) = status.get("worktrees").and_then(|w| w.as_array()) {
                if let Some(wt) = worktrees.iter().find(|w| {
                    w.get("task_id").and_then(|v| v.as_str()) == Some(_worktree_id)
                        || w.get("id").and_then(|v| v.as_str()) == Some(_worktree_id)
                }) {
                    wt.clone()
                } else {
                    // Return full status if no matching worktree found by ID
                    status
                }
            } else {
                status
            }
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
                "Unknown data endpoint: '{}'. Available: status, status/health, workflows/list, agents/list, agents/{{id}}, worktrees/{{id}}, workflows/{{id}}, tasks/{{id}}/workflow-context, tasks/{{id}}/agent-info, board/summary, board/data, board/boards/list, board/epics/list, board/filters, board/epics/{{id}}, board/stories/{{id}}, board/assignments/{{id}}, worktrees/list",
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
    let history = execution_history::load_execution_history(config);

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
                            let stats = execution_history::get_workflow_stats(&history, id);
                            workflows.push(serde_json::json!({
                                "id": id,
                                "description": wf.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                                "category": category,
                                "step_count": wf.get("steps").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
                                "requires": wf.get("requires").and_then(|r| r.as_array()).map(|arr| {
                                    arr.iter().filter_map(|r| r.get("plugin").and_then(|p| p.as_str())).collect::<Vec<_>>().join(", ")
                                }).unwrap_or_default(),
                                "last_run": stats.last_run,
                                "total_runs": stats.total_runs,
                                "success_rate": stats.success_rate,
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

#[cfg(not(target_arch = "wasm32"))]
/// Count how many workflows reference each agent by scanning workflow YAML files.
///
/// Agents are referenced in workflows via three patterns:
/// - `system_prompt` containing `You are bmad/<name>`
/// - `agent: bmad/<name>` in session participants
/// - `agent_name: bmad/<name>` in step config
fn count_agent_workflow_assignments(
    config: &WorkspaceConfig,
) -> std::collections::HashMap<String, usize> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    if !config.workflows_dir.exists() {
        return counts;
    }

    let entries = match std::fs::read_dir(&config.workflows_dir) {
        Ok(e) => e,
        Err(_) => return counts,
    };

    let prefixes = ["You are ", "agent: ", "agent_name: "];

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Collect unique agents referenced in this workflow
        let mut seen_in_workflow: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for line in content.lines() {
            let trimmed = line.trim();
            for prefix in &prefixes {
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    // Handle YAML list items like "- agent: bmad/qa"
                    if let Some(agent_ref) = rest.trim().strip_prefix(prefix) {
                        if let Some(agent_id) = extract_bmad_agent_id(agent_ref) {
                            seen_in_workflow.insert(agent_id);
                        }
                    }
                }
                if let Some(agent_ref) = trimmed.strip_prefix(prefix) {
                    if let Some(agent_id) = extract_bmad_agent_id(agent_ref) {
                        seen_in_workflow.insert(agent_id);
                    }
                }
            }
        }

        for agent_id in seen_in_workflow {
            *counts.entry(agent_id).or_insert(0) += 1;
        }
    }

    counts
}

#[cfg(not(target_arch = "wasm32"))]
/// Extract a `bmad/<name>` agent ID from text that starts at the agent reference.
/// Returns `None` if the text doesn't start with `bmad/`.
fn extract_bmad_agent_id(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.starts_with("bmad/") {
        return None;
    }
    // Agent ID is "bmad/" followed by word chars and hyphens
    let id: String = text
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '/' || *c == '-' || *c == '_')
        .collect();
    // Strip trailing punctuation that might have been included
    let id = id.trim_end_matches(|c: char| c == '.' || c == ',');
    if id.len() > "bmad/".len() {
        Some(id.to_string())
    } else {
        None
    }
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

    let workflow_counts = count_agent_workflow_assignments(config);

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

            let assigned_workflows = workflow_counts.get(&a.name).copied().unwrap_or(0);

            serde_json::json!({
                "id": a.name,
                "name": display_name,
                "role": role,
                "description": a.description.as_deref().unwrap_or(""),
                "model_tier": a.model_tier.as_deref().unwrap_or("balanced"),
                "skills": a.skills.as_ref().cloned().unwrap_or_default(),
                "tools": a.tools.as_ref().cloned().unwrap_or_default(),
                "assigned_workflows": assigned_workflows,
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

/// Single agent detail for dashboard detail view.
/// Looks up agent by ID from the BMAD agent registry.
#[cfg(not(target_arch = "wasm32"))]
fn get_agent_detail_value(
    agent_id: &str,
    config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    let manifest_path = config.base_dir.join("_bmad/_config/agent-manifest.csv");
    let registry = crate::agent_registry::BmadAgentRegistry::new(&manifest_path);

    let agents = {
        use pulse_plugin_sdk::traits::agent_definition::AgentDefinitionProvider;
        registry.list_agents(None)
    };

    let agent = agents
        .iter()
        .find(|a| a.name == agent_id)
        .ok_or_else(|| WitPluginError::not_found(format!("Agent '{}' not found", agent_id)))?;

    let (display_name, role) = agent
        .description
        .as_deref()
        .and_then(|d| d.split_once(" \u{2014} "))
        .map(|(name, role)| (name.to_string(), role.to_string()))
        .unwrap_or_else(|| (agent.name.clone(), String::new()));

    Ok(serde_json::json!({
        "id": agent.name,
        "name": display_name,
        "role": role,
        "description": agent.description.as_deref().unwrap_or(""),
        "model_tier": agent.model_tier.as_deref().unwrap_or("balanced"),
        "skills": agent.skills.as_ref().cloned().unwrap_or_default(),
    }))
}

#[cfg(target_arch = "wasm32")]
fn get_agent_detail_value(
    agent_id: &str,
    _config: &WorkspaceConfig,
) -> Result<serde_json::Value, WitPluginError> {
    Err(WitPluginError::not_found(format!(
        "Agent '{}' not found (registry not available in WASM)",
        agent_id
    )))
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

    let history = execution_history::load_execution_history(config);
    let stats = execution_history::get_workflow_stats(&history, workflow_id);
    let recent = execution_history::get_recent_executions(&history, workflow_id, 10);
    let recent_executions: Vec<serde_json::Value> = recent
        .iter()
        .map(|r| {
            serde_json::json!({
                "timestamp": r.timestamp,
                "success": r.success,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "id": workflow_id,
        "description": wf.get("description").and_then(|d| d.as_str()).unwrap_or(""),
        "category": category,
        "step_count": steps.len(),
        "step_pipeline": step_pipeline.join(" \u{2192} "),
        "requires": wf.get("requires").and_then(|r| r.as_array()).map(|arr| {
            arr.iter().filter_map(|r| r.get("plugin").and_then(|p| p.as_str())).collect::<Vec<_>>().join(", ")
        }).unwrap_or_default(),
        "last_run": stats.last_run,
        "total_runs": stats.total_runs,
        "success_rate": stats.success_rate,
        "recent_executions": recent_executions,
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
    let result = match endpoint {
        // Execute Workflow form submit
        "workflows/execute" => {
            let workflow_id = payload
                .get("workflow_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    WitPluginError::invalid_input("workflows/execute requires 'workflow_id' in payload")
                })?;
            let input = payload
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            crate::plugin_bridge::execute_workflow(workflow_id, input, config)?
        }
        // Worktree cleanup: worktrees/{id}/cleanup
        ep if ep.starts_with("worktrees/") && ep.ends_with("/cleanup") => {
            let _task_id = ep
                .strip_prefix("worktrees/")
                .and_then(|s| s.strip_suffix("/cleanup"))
                .unwrap_or("");
            crate::plugin_bridge::cleanup_worktrees(config)?
        }
        // Workflow table row action: workflows/{id}/execute
        ep if ep.starts_with("workflows/") && ep.ends_with("/execute") => {
            let workflow_id = ep
                .strip_prefix("workflows/")
                .and_then(|s| s.strip_suffix("/execute"))
                .unwrap_or("");
            if workflow_id.is_empty() {
                return Err(WitPluginError::invalid_input(
                    "workflow ID cannot be empty in workflows/{id}/execute",
                ));
            }
            let input = payload
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            crate::plugin_bridge::execute_workflow(workflow_id, input, config)?
        }
        _ => {
            return Err(WitPluginError::not_found(format!(
                "Unknown mutation endpoint: '{}'. Available: workflows/execute, workflows/{{id}}/execute, worktrees/{{id}}/cleanup. Board mutations moved to plugin-board.",
                endpoint
            )));
        }
    };
    serde_json::to_string_pretty(&result)
        .map_err(|e| WitPluginError::internal(format!("JSON serialization error: {e}")))
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

    #[test]
    fn run_tests_action_is_recognized() {
        // The run-tests action delegates to plugin-test-runner via plugin_bridge.
        // It will fail because no plugin server is running, but it should NOT
        // return "Unknown action" (not_found with that message).
        let input = test_input("run-tests");
        let result = execute_action(&input);
        match result {
            Ok(_) => {} // If the bridge somehow succeeds, that's fine
            Err(e) => {
                assert!(
                    !e.message.contains("Unknown action"),
                    "run-tests should be a recognized action, got: {}",
                    e.message
                );
            }
        }
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
            assert!(
                agent.get("assigned_workflows").is_some(),
                "agent {id} should have 'assigned_workflows'"
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

    #[test]
    fn list_agents_includes_assigned_workflows_count() {
        let config = make_test_workspace_config();
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();

        // Every agent should have the assigned_workflows field as a number
        for agent in arr {
            let id = agent["id"].as_str().unwrap_or("unknown");
            assert!(
                agent["assigned_workflows"].is_u64(),
                "agent {id} should have numeric 'assigned_workflows'"
            );
        }

        // bmad/architect and bmad/qa are referenced in many workflows,
        // so they must have non-zero counts
        let architect = arr
            .iter()
            .find(|a| a["id"].as_str() == Some("bmad/architect"))
            .expect("should find bmad/architect");
        assert!(
            architect["assigned_workflows"].as_u64().unwrap() > 0,
            "bmad/architect should be assigned to at least one workflow"
        );

        let qa = arr
            .iter()
            .find(|a| a["id"].as_str() == Some("bmad/qa"))
            .expect("should find bmad/qa");
        assert!(
            qa["assigned_workflows"].as_u64().unwrap() > 0,
            "bmad/qa should be assigned to at least one workflow"
        );

        // At least some agents should have non-zero counts overall
        let total: u64 = arr
            .iter()
            .filter_map(|a| a["assigned_workflows"].as_u64())
            .sum();
        assert!(
            total > 0,
            "at least some agents should have non-zero assigned_workflows"
        );
    }

    #[test]
    fn list_agents_assigned_workflows_zero_when_no_workflows_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        // Set up manifest but no workflows directory
        let bmad_config = tmp.path().join("_bmad/_config");
        std::fs::create_dir_all(&bmad_config).unwrap();
        std::fs::copy(
            base.join("_bmad/_config/agent-manifest.csv"),
            bmad_config.join("agent-manifest.csv"),
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        let result = list_agents_value(&config).unwrap();
        let arr = result.as_array().unwrap();

        // All agents should have assigned_workflows = 0
        for agent in arr {
            let id = agent["id"].as_str().unwrap_or("unknown");
            assert_eq!(
                agent["assigned_workflows"].as_u64().unwrap(),
                0,
                "agent {id} should have 0 assigned_workflows when no workflows dir exists"
            );
        }
    }

    // ── Execution history integration tests ────────────────────────────

    #[test]
    fn list_workflows_detail_includes_execution_history_fields() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a workflow file
        let workflows_dir = tmp.path().join("config/workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("test-wf.yaml"),
            "name: test-wf\ndescription: A test workflow\nsteps:\n  - id: step1\n    plugin: test\n",
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        let result = list_workflows_detail_value(&config).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);

        let wf = &arr[0];
        assert_eq!(wf["id"].as_str(), Some("test-wf"));
        // Execution history fields should be present with defaults
        assert!(wf.get("last_run").is_some(), "should have last_run field");
        assert!(wf["last_run"].is_null(), "last_run should be null when never run");
        assert_eq!(wf["total_runs"].as_u64(), Some(0));
        assert_eq!(wf["success_rate"].as_str(), Some("0%"));
    }

    #[test]
    fn list_workflows_detail_shows_stats_from_history() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("config/workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("my-wf.yaml"),
            "name: my-wf\ndescription: My workflow\nsteps:\n  - id: s1\n    plugin: p\n",
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());

        // Write some execution history
        let r1 = crate::execution_history::ExecutionRecord {
            workflow_id: "my-wf".to_string(),
            timestamp: "2026-03-31T10:00:00+00:00".to_string(),
            success: true,
        };
        let r2 = crate::execution_history::ExecutionRecord {
            workflow_id: "my-wf".to_string(),
            timestamp: "2026-03-31T11:00:00+00:00".to_string(),
            success: false,
        };
        crate::execution_history::save_execution_record(&config, &r1).unwrap();
        crate::execution_history::save_execution_record(&config, &r2).unwrap();

        let result = list_workflows_detail_value(&config).unwrap();
        let arr = result.as_array().unwrap();
        let wf = &arr[0];

        assert_eq!(wf["total_runs"].as_u64(), Some(2));
        assert_eq!(wf["success_rate"].as_str(), Some("50%"));
        assert_eq!(
            wf["last_run"].as_str(),
            Some("2026-03-31T11:00:00+00:00")
        );
    }

    #[test]
    fn get_workflow_detail_includes_execution_history_and_recent_executions() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("config/workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("detail-wf.yaml"),
            "name: detail-wf\ndescription: Detail workflow\nsteps:\n  - id: s1\n    plugin: p\n",
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());

        // Add 3 execution records
        for (i, success) in [(1, true), (2, false), (3, true)] {
            let record = crate::execution_history::ExecutionRecord {
                workflow_id: "detail-wf".to_string(),
                timestamp: format!("2026-03-31T{:02}:00:00+00:00", 10 + i),
                success,
            };
            crate::execution_history::save_execution_record(&config, &record).unwrap();
        }

        let result = get_workflow_detail_value("detail-wf", &config).unwrap();

        assert_eq!(result["total_runs"].as_u64(), Some(3));
        assert_eq!(result["success_rate"].as_str(), Some("66%"));
        assert_eq!(
            result["last_run"].as_str(),
            Some("2026-03-31T13:00:00+00:00")
        );

        // recent_executions should be present and sorted most-recent-first
        let recent = result["recent_executions"].as_array().unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(
            recent[0]["timestamp"].as_str(),
            Some("2026-03-31T13:00:00+00:00")
        );
        assert_eq!(recent[0]["success"].as_bool(), Some(true));
        assert_eq!(
            recent[1]["timestamp"].as_str(),
            Some("2026-03-31T12:00:00+00:00")
        );
        assert_eq!(recent[1]["success"].as_bool(), Some(false));
    }

    #[test]
    fn get_workflow_detail_no_history_returns_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("config/workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("empty-wf.yaml"),
            "name: empty-wf\ndescription: Empty\nsteps:\n  - id: s1\n    plugin: p\n",
        )
        .unwrap();

        let config = WorkspaceConfig::from_base_dir(tmp.path());
        let result = get_workflow_detail_value("empty-wf", &config).unwrap();

        assert!(result["last_run"].is_null());
        assert_eq!(result["total_runs"].as_u64(), Some(0));
        assert_eq!(result["success_rate"].as_str(), Some("0%"));
        assert_eq!(result["recent_executions"].as_array().unwrap().len(), 0);
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

    // ── Phase 1: Dashboard endpoint gap fixes ────────────────────────

    #[test]
    fn data_mutate_workflows_execute_requires_workflow_id() {
        let mut input = test_input("data-mutate");
        input.endpoint = Some("workflows/execute".to_string());
        input.payload = Some(serde_json::json!({}));
        let err = execute_action(&input).unwrap_err();
        assert_eq!(
            err.code, "invalid_input",
            "workflows/execute should require workflow_id in payload"
        );
    }

    #[test]
    fn data_mutate_workflows_id_execute_empty_id_rejected() {
        let mut input = test_input("data-mutate");
        input.endpoint = Some("workflows//execute".to_string());
        input.payload = Some(serde_json::json!({}));
        let err = execute_action(&input).unwrap_err();
        assert_eq!(
            err.code, "invalid_input",
            "workflows//execute should reject empty workflow ID"
        );
    }

    #[test]
    fn data_mutate_unknown_endpoint_returns_not_found() {
        let mut input = test_input("data-mutate");
        input.endpoint = Some("unknown/endpoint".to_string());
        input.payload = Some(serde_json::json!({}));
        let err = execute_action(&input).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn data_query_agent_detail_found() {
        let config = make_test_workspace_config();
        let result = get_agent_detail_value("bmad/architect", &config).unwrap();
        assert_eq!(result["id"].as_str(), Some("bmad/architect"));
        assert_eq!(result["name"].as_str(), Some("Winston"));
        assert!(result["role"].as_str().unwrap_or("").contains("Architect"));
        assert!(result.get("model_tier").is_some());
        assert!(result.get("skills").is_some());
    }

    #[test]
    fn data_query_agent_detail_not_found() {
        let config = make_test_workspace_config();
        let err = get_agent_detail_value("nonexistent/agent", &config).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn data_query_agents_id_routed_correctly() {
        let config = make_test_workspace_config();
        let result = execute_data_query("agents/bmad/architect", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["id"].as_str(), Some("bmad/architect"));
    }

    #[test]
    fn data_query_worktrees_id_routed_correctly() {
        // worktrees/{id} should be recognized (delegates to plugin bridge,
        // which will fail without a server, but the endpoint should be dispatched)
        let config = make_test_workspace_config();
        let result = execute_data_query("worktrees/some-task", &config, None, None);
        // The call will fail because no server is running, but it should NOT
        // return "Unknown data endpoint" (not_found with that message)
        match result {
            Ok(_) => {} // If the bridge somehow succeeds, that's fine
            Err(e) => {
                assert!(
                    !e.message.contains("Unknown data endpoint"),
                    "worktrees/some-task should be routed, not unknown: {}",
                    e.message
                );
            }
        }
    }

    #[test]
    fn data_mutate_worktrees_cleanup_routed_correctly() {
        // worktrees/{id}/cleanup should be recognized as a mutation endpoint
        let mut input = test_input("data-mutate");
        input.endpoint = Some("worktrees/task-123/cleanup".to_string());
        input.payload = Some(serde_json::json!({}));
        let result = execute_action(&input);
        match result {
            Ok(_) => {} // Bridge succeeded somehow
            Err(e) => {
                assert!(
                    !e.message.contains("Unknown mutation endpoint"),
                    "worktrees/task-123/cleanup should be routed, not unknown: {}",
                    e.message
                );
            }
        }
    }

    #[test]
    fn data_mutate_workflows_id_execute_routed_correctly() {
        // workflows/{id}/execute should be recognized as a mutation endpoint
        let mut input = test_input("data-mutate");
        input.endpoint = Some("workflows/coding-quick-dev/execute".to_string());
        input.payload = Some(serde_json::json!({}));
        let result = execute_action(&input);
        match result {
            Ok(_) => {} // Bridge succeeded somehow
            Err(e) => {
                assert!(
                    !e.message.contains("Unknown mutation endpoint"),
                    "workflows/coding-quick-dev/execute should be routed, not unknown: {}",
                    e.message
                );
            }
        }
    }

    // ── Data query routing tests (Phase 5b) ──────────────────────────

    #[test]
    fn data_query_status_returns_valid_json() {
        let config = make_test_workspace_config();
        let result = execute_data_query("status", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("validation").is_some(), "status should contain 'validation'");
        assert!(parsed.get("workflows").is_some(), "status should contain 'workflows'");
        assert!(parsed.get("plugins").is_some(), "status should contain 'plugins'");
    }

    #[test]
    fn data_query_status_health_returns_badge_data() {
        let config = make_test_workspace_config();
        let result = execute_data_query("status/health", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("pack_status").is_some(), "health should contain 'pack_status'");
        let status = parsed["pack_status"].as_str().unwrap();
        assert!(
            status == "healthy" || status == "degraded",
            "pack_status should be 'healthy' or 'degraded', got: {status}"
        );
        assert!(parsed.get("plugins_ok").is_some(), "health should contain 'plugins_ok'");
        assert!(parsed.get("workflows_found").is_some(), "health should contain 'workflows_found'");
    }

    #[test]
    fn data_query_workflows_list_returns_array() {
        let config = make_test_workspace_config();
        let result = execute_data_query("workflows/list", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed.as_array().expect("workflows/list should return an array");
        assert!(!arr.is_empty(), "should have at least one workflow");
        let first = &arr[0];
        assert!(first.get("id").is_some(), "workflow entry should have 'id'");
        assert!(first.get("description").is_some(), "workflow entry should have 'description'");
        assert!(first.get("category").is_some(), "workflow entry should have 'category'");
        assert!(first.get("step_count").is_some(), "workflow entry should have 'step_count'");
    }

    #[test]
    fn data_query_workflow_detail_coding_quick_dev() {
        let config = make_test_workspace_config();
        let result = execute_data_query("workflows/coding-quick-dev", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["id"].as_str(), Some("coding-quick-dev"));
        assert_eq!(parsed["category"].as_str(), Some("coding"));
        assert!(parsed["step_count"].as_u64().unwrap() > 0, "should have steps");
        assert!(parsed.get("step_pipeline").is_some(), "should have step_pipeline");
        assert!(parsed.get("description").is_some(), "should have description");
    }

    #[test]
    fn data_query_workflow_detail_bootstrap_has_bootstrap_category() {
        let config = make_test_workspace_config();
        let result = execute_data_query("workflows/bootstrap-cycle", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["category"].as_str(), Some("bootstrap"));
    }

    #[test]
    fn data_query_workflow_detail_nonexistent_returns_not_found() {
        let config = make_test_workspace_config();
        let err = execute_data_query("workflows/nonexistent-workflow", &config, None, None).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn data_query_agents_list_returns_array() {
        let config = make_test_workspace_config();
        let result = execute_data_query("agents/list", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let arr = parsed.as_array().expect("agents/list should return an array");
        assert_eq!(arr.len(), 9, "should have 9 agents from registry");
        let first = &arr[0];
        assert!(first.get("id").is_some(), "agent should have 'id'");
        assert!(first.get("name").is_some(), "agent should have 'name'");
        assert!(first.get("role").is_some(), "agent should have 'role'");
    }

    #[test]
    fn data_query_unknown_endpoint_returns_not_found() {
        let config = make_test_workspace_config();
        let err = execute_data_query("unknown-endpoint", &config, None, None).unwrap_err();
        assert_eq!(err.code, "not_found");
        assert!(
            err.message.contains("Unknown data endpoint"),
            "error should mention unknown endpoint, got: {}",
            err.message
        );
    }

    #[test]
    fn data_query_strips_leading_slash() {
        let config = make_test_workspace_config();
        let result = execute_data_query("/status/health", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("pack_status").is_some());
    }

    #[test]
    fn data_query_board_summary_returns_fallback() {
        let config = make_test_workspace_config();
        let result = execute_data_query("board/summary", &config, None, None).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("sprint_progress").is_some(), "should have sprint_progress");
        assert!(parsed.get("total").is_some(), "should have total");
        assert!(parsed.get("done").is_some(), "should have done");
        assert!(parsed.get("in_progress").is_some(), "should have in_progress");
        assert!(parsed.get("ready").is_some(), "should have ready");
    }

    #[test]
    fn data_mutate_board_proxied_to_plugin_board() {
        // board/ mutations are now proxied to plugin-board (will fail without server)
        let config = make_test_workspace_config();
        let payload = serde_json::json!({"test": true});
        let result = execute_data_mutate("board/sync", &payload, &config, None, None);
        match result {
            Ok(_) => {} // If board plugin is available, fine
            Err(e) => {
                // Should be a bridge error, not "Unknown mutation endpoint"
                assert!(
                    !e.message.contains("Unknown mutation endpoint"),
                    "board/sync should be proxied to plugin-board, got: {}",
                    e.message
                );
            }
        }
    }

    #[test]
    fn data_mutate_strips_leading_slash() {
        let config = make_test_workspace_config();
        let payload = serde_json::json!({});
        let result = execute_data_mutate("/unknown/endpoint", &payload, &config, None, None);
        match result {
            Err(e) => assert_eq!(e.code, "not_found"),
            Ok(_) => panic!("should return error for unknown mutation"),
        }
    }

    #[test]
    fn execute_action_data_query_routes_correctly() {
        let config_base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let input = CodingPackInput {
            action: "data-query".to_string(),
            target: None,
            workflow_id: None,
            input: None,
            endpoint: Some("status/health".to_string()),
            payload: None,
            workspace_dir: Some(config_base.to_string_lossy().to_string()),
            workspace: None,
            board_id: None,
        };
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("pack_status").is_some());
    }

    #[test]
    fn execute_action_data_query_empty_endpoint_defaults_to_empty() {
        let config_base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let input = CodingPackInput {
            action: "data-query".to_string(),
            target: None,
            workflow_id: None,
            input: None,
            endpoint: None,
            payload: None,
            workspace_dir: Some(config_base.to_string_lossy().to_string()),
            workspace: None,
            board_id: None,
        };
        let err = execute_action(&input).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn status_action_returns_composite_data() {
        let config_base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let input = test_input_with_workspace("status", config_base.to_str().unwrap());
        let result = execute_action(&input).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("validation").is_some(), "status should include validation");
        assert!(parsed.get("workflows").is_some(), "status should include workflows");
        assert!(parsed.get("plugins").is_some(), "status should include plugins");
    }

    #[test]
    fn task_workflow_context_fallback_returns_minimal_json() {
        let config = make_test_workspace_config();
        let result = task_workflow_context_value("nonexistent-task", &config).unwrap();
        assert_eq!(result["task_id"].as_str(), Some("nonexistent-task"));
        assert!(result["workflow_id"].is_null());
        assert!(result["step_id"].is_null());
        assert!(result["executor"].is_null());
        assert!(result["model_tier"].is_null());
    }

    #[test]
    fn task_agent_info_fallback_returns_default_agent() {
        let config = make_test_workspace_config();
        let result = task_agent_info_value("nonexistent-task", &config).unwrap();
        assert_eq!(result["task_id"].as_str(), Some("nonexistent-task"));
        assert_eq!(result["agent_name"].as_str(), Some("bmad-dev"));
        assert_eq!(result["display_name"].as_str(), Some("Amelia"));
        assert_eq!(result["title"].as_str(), Some("Developer"));
    }
}
