//! Agent mesh safety guards — depth limiting and mesh config injection.

use pulse_plugin_sdk::error::WitPluginError;

/// Environment variable for tracking agent invocation depth.
const DEPTH_ENV_VAR: &str = "PULSE_AGENT_DEPTH";
/// Environment variable for tracking agent name.
const NAME_ENV_VAR: &str = "PULSE_AGENT_NAME";

/// Check the agent mesh depth guard.
///
/// Reads `PULSE_AGENT_DEPTH` env var (default 0), computes next = current + 1.
/// If next > max_depth: returns Err.
/// Else: returns Ok(next_depth).
///
/// Non-numeric env var values are treated as 0.
/// Performance: <1ms (single env var read + integer comparison).
pub fn check_depth_guard(
    mesh_settings: &crate::workspace::AgentMeshSettings,
) -> Result<u32, WitPluginError> {
    if mesh_settings.max_depth == 0 {
        return Err(WitPluginError::invalid_input(
            "agent_mesh.max_depth must be at least 1 (set to 0, which disallows all agent invocations)"
        ));
    }

    let current: u32 = std::env::var(DEPTH_ENV_VAR)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let next = current.checked_add(1).ok_or_else(|| {
        WitPluginError::internal(format!(
            "Agent mesh depth overflow: current depth {} cannot be incremented",
            current
        ))
    })?;

    if next > mesh_settings.max_depth {
        return Err(WitPluginError::invalid_input(format!(
            "Agent mesh depth limit exceeded: {} > {}",
            next, mesh_settings.max_depth
        )));
    }

    Ok(next)
}

/// Build mesh configuration to inject into JSON-RPC parameters for agent steps.
///
/// When mesh is enabled and agent_name is provided, returns a JSON map containing:
/// - `agent_name`: the agent's identity
/// - `mcp_config`: mcpServers.pulse-agents with plugin binary path and --mcp-mode args
/// - `env_vars`: PULSE_AGENT_DEPTH (incremented) and PULSE_AGENT_NAME
///
/// When mesh is disabled or agent_name is missing, returns None.
///
/// Requirements:
/// - mesh_enabled=true without agent_name returns Err
/// - Non-mesh steps get no mesh fields (returns Ok(None))
pub fn build_mesh_config(
    mesh_settings: &crate::workspace::AgentMeshSettings,
    agent_name: Option<&str>,
    mesh_enabled_for_step: bool,
    plugin_binary_path: &str,
) -> Result<Option<serde_json::Value>, WitPluginError> {
    if !mesh_settings.enabled || !mesh_enabled_for_step {
        return Ok(None);
    }

    let agent_name = agent_name.ok_or_else(|| {
        WitPluginError::invalid_input("mesh_enabled=true requires agent_name to be set")
    })?;

    if plugin_binary_path.is_empty() {
        return Err(WitPluginError::invalid_input(
            "plugin_binary_path must not be empty when mesh is enabled"
        ));
    }

    let next_depth = check_depth_guard(mesh_settings)?;

    Ok(Some(serde_json::json!({
        "agent_name": agent_name,
        "mcp_config": {
            "mcpServers": {
                "pulse-agents": {
                    "command": plugin_binary_path,
                    "args": ["--mcp-mode"]
                }
            }
        },
        "env_vars": {
            DEPTH_ENV_VAR: next_depth.to_string(),
            NAME_ENV_VAR: agent_name
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::AgentMeshSettings;
    use std::sync::Mutex;

    // Mutex to prevent parallel test interference with env vars
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn test_settings(max_depth: u32) -> AgentMeshSettings {
        AgentMeshSettings {
            enabled: true,
            max_depth,
            agents_yaml_path: None,
        }
    }

    #[test]
    fn depth_guard_default_depth_zero() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_AGENT_DEPTH");
        let result = check_depth_guard(&test_settings(5));
        assert_eq!(result.unwrap(), 1); // 0 + 1 = 1
    }

    #[test]
    fn depth_guard_increments_existing() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_AGENT_DEPTH", "3");
        let result = check_depth_guard(&test_settings(5));
        assert_eq!(result.unwrap(), 4);
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }

    #[test]
    fn depth_guard_at_limit_succeeds() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_AGENT_DEPTH", "4");
        let result = check_depth_guard(&test_settings(5));
        assert_eq!(result.unwrap(), 5); // exactly at max_depth
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }

    #[test]
    fn depth_guard_exceeds_limit_fails() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_AGENT_DEPTH", "5");
        let result = check_depth_guard(&test_settings(5));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("depth limit exceeded"));
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }

    #[test]
    fn depth_guard_non_numeric_treated_as_zero() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_AGENT_DEPTH", "not-a-number");
        let result = check_depth_guard(&test_settings(5));
        assert_eq!(result.unwrap(), 1);
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }

    #[test]
    fn depth_guard_u32_max_overflow() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_AGENT_DEPTH", "4294967295");
        let result = check_depth_guard(&test_settings(5));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("overflow"));
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }

    #[test]
    fn depth_guard_max_depth_one() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_AGENT_DEPTH");
        let result = check_depth_guard(&test_settings(1));
        assert_eq!(result.unwrap(), 1); // 0 + 1 = 1, which equals max
    }

    #[test]
    fn depth_guard_max_depth_zero_returns_config_error() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_AGENT_DEPTH");
        let result = check_depth_guard(&test_settings(0));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("max_depth must be at least 1"));
    }

    // ── build_mesh_config tests ──

    #[test]
    fn mesh_config_disabled_returns_none() {
        let settings = AgentMeshSettings {
            enabled: false,
            ..test_settings(5)
        };
        let result = build_mesh_config(&settings, Some("bmad/dev"), true, "/usr/bin/plugin");
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn mesh_config_step_not_mesh_returns_none() {
        let result =
            build_mesh_config(&test_settings(5), Some("bmad/dev"), false, "/usr/bin/plugin");
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn mesh_config_enabled_without_agent_name_errors() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_AGENT_DEPTH");
        let result = build_mesh_config(&test_settings(5), None, true, "/usr/bin/plugin");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("agent_name"));
    }

    #[test]
    fn mesh_config_empty_binary_path_errors() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_AGENT_DEPTH");
        let result = build_mesh_config(&test_settings(5), Some("bmad/dev"), true, "");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("plugin_binary_path"));
    }

    #[test]
    fn mesh_config_enabled_with_agent_name_returns_config() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("PULSE_AGENT_DEPTH");
        let result = build_mesh_config(
            &test_settings(5),
            Some("bmad/architect"),
            true,
            "/path/to/plugin",
        )
        .unwrap()
        .unwrap();
        assert_eq!(result["agent_name"].as_str(), Some("bmad/architect"));
        assert!(result["mcp_config"]["mcpServers"]["pulse-agents"].is_object());
        assert_eq!(
            result["env_vars"]["PULSE_AGENT_DEPTH"].as_str(),
            Some("1")
        );
        assert_eq!(
            result["env_vars"]["PULSE_AGENT_NAME"].as_str(),
            Some("bmad/architect")
        );
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }

    #[test]
    fn mesh_config_depth_exceeds_limit_errors() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("PULSE_AGENT_DEPTH", "5");
        let result = build_mesh_config(
            &test_settings(5),
            Some("bmad/dev"),
            true,
            "/path/to/plugin",
        );
        assert!(result.is_err());
        std::env::remove_var("PULSE_AGENT_DEPTH");
    }
}
