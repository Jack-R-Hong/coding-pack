#![forbid(unsafe_code)]

use plugin_coding_pack::agent_registry::BmadAgentRegistry;
use plugin_coding_pack::config_injector::BmadAgentInjector;
use plugin_coding_pack::tool_provider::BmadToolProvider;
use plugin_coding_pack::CodingPackPlugin;
use pulse_plugin_sdk::wit_traits::{
    DashboardExtensionPlugin, InstallContext, PluginLifecycle, StepExecutorPlugin, UninstallContext,
};
use pulse_plugin_sdk::wit_types::{StepConfig, TaskInput};
use pulse_plugin_sdk::ConfigInjector;

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let plugin = CodingPackPlugin;
    let manifest_path = std::path::PathBuf::from("_bmad/_config/agent-manifest.csv");
    let injector = BmadAgentInjector::new(&manifest_path);
    let tool_provider = BmadToolProvider::new(
        plugin_coding_pack::workspace::WorkspaceConfig::resolve(None),
    );
    let agent_registry = BmadAgentRegistry::new(&manifest_path);
    // Combined adapter: handles step-executor + dashboard-extension + config-injector + tool-provider + agent-definition methods
    pulse_plugin_sdk::dev_adapter::run_custom_stdio(move |method, params| {
        dispatch_combined(
            &plugin,
            &injector,
            &tool_provider,
            &agent_registry,
            method,
            params,
        )
    });
}

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn dispatch_combined(
    plugin: &CodingPackPlugin,
    injector: &BmadAgentInjector,
    tool_provider: &BmadToolProvider,
    agent_registry: &BmadAgentRegistry,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, pulse_plugin_sdk::dev_adapter::DispatchError> {
    use pulse_plugin_sdk::dev_adapter::DispatchError;
    use pulse_plugin_sdk::traits::agent_definition::AgentDefinitionProvider;
    use pulse_plugin_sdk::traits::tool_provider::ToolProvider;
    use pulse_plugin_sdk::types::injection::InjectionQuery;
    use pulse_plugin_sdk::types::llm::ToolCall;

    match method {
        // ── Plugin lifecycle ──
        "plugin-lifecycle.get-info" => {
            let info = plugin.get_info();
            serde_json::to_value(&info).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "plugin-lifecycle.health-check" => Ok(serde_json::Value::Bool(plugin.health_check())),
        "plugin-lifecycle.on-install" => {
            let ctx = InstallContext {
                plugin_dir: params
                    .get("plugin_dir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| {
                        DispatchError::InvalidParams("missing plugin_dir".to_string())
                    })?,
                workflows_dir: params
                    .get("workflows_dir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| {
                        DispatchError::InvalidParams("missing workflows_dir".to_string())
                    })?,
                config_dir: params
                    .get("config_dir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| {
                        DispatchError::InvalidParams("missing config_dir".to_string())
                    })?,
            };
            plugin
                .on_install(&ctx)
                .map(|_| serde_json::json!({"ok": true}))
                .map_err(|e| DispatchError::Internal(e.to_string()))
        }

        "plugin-lifecycle.on-uninstall" => {
            let ctx = UninstallContext {
                plugin_dir: params
                    .get("plugin_dir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| {
                        DispatchError::InvalidParams("missing plugin_dir".to_string())
                    })?,
                workflows_dir: params
                    .get("workflows_dir")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| {
                        DispatchError::InvalidParams("missing workflows_dir".to_string())
                    })?,
                purge: params
                    .get("purge")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            };
            plugin
                .on_uninstall(&ctx)
                .map(|_| serde_json::json!({"ok": true}))
                .map_err(|e| DispatchError::Internal(e.to_string()))
        }

        "plugin-lifecycle.on-enable" => plugin
            .on_enable()
            .map(|_| serde_json::json!({"ok": true}))
            .map_err(|e| DispatchError::Internal(e.to_string())),

        "plugin-lifecycle.on-disable" => plugin
            .on_disable()
            .map(|_| serde_json::json!({"ok": true}))
            .map_err(|e| DispatchError::Internal(e.to_string())),

        // ── Step executor ──
        "step-executor.execute" => {
            let task: TaskInput = serde_json::from_value(
                params
                    .get("task")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )
            .map_err(|e| DispatchError::InvalidParams(format!("invalid task: {e}")))?;
            let config: StepConfig = serde_json::from_value(
                params
                    .get("config")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )
            .map_err(|e| DispatchError::InvalidParams(format!("invalid config: {e}")))?;
            match plugin.execute(task, config) {
                Ok(result) => serde_json::to_value(&result)
                    .map_err(|e| DispatchError::Internal(e.to_string())),
                Err(e) => Err(DispatchError::Internal(e.to_string())),
            }
        }

        // ── Dashboard extension ──
        "dashboard-extension.get-pages-json" => {
            let json_str = plugin.get_pages_json();
            serde_json::from_str(&json_str)
                .map_err(|e| DispatchError::Internal(format!("invalid pages JSON: {e}")))
        }
        "dashboard-extension.get-api-routes-json" => {
            let json_str = plugin.get_api_routes_json();
            serde_json::from_str(&json_str)
                .map_err(|e| DispatchError::Internal(format!("invalid routes JSON: {e}")))
        }
        "dashboard-extension.get-display-customizations-json" => {
            let json_str = plugin.get_display_customizations_json();
            serde_json::from_str(&json_str)
                .map_err(|e| DispatchError::Internal(format!("invalid customizations JSON: {e}")))
        }

        // ── Config injector ──
        "config-injector.injector-name" => {
            let name = injector.injector_name();
            serde_json::to_value(name).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "config-injector.priority" => {
            let priority = injector.priority();
            serde_json::to_value(priority).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "config-injector.applies-to" => {
            let query: InjectionQuery = serde_json::from_value(params)
                .map_err(|e| DispatchError::InvalidParams(format!("invalid query: {e}")))?;
            let applies = injector.applies_to(&query);
            serde_json::to_value(applies).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "config-injector.provide-injections" => {
            let query: InjectionQuery = serde_json::from_value(params)
                .map_err(|e| DispatchError::InvalidParams(format!("invalid query: {e}")))?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| DispatchError::Internal(format!("runtime error: {e}")))?;
            let result = rt.block_on(injector.provide_injections(&query));
            match result {
                Ok(injections) => serde_json::to_value(injections)
                    .map_err(|e| DispatchError::Internal(e.to_string())),
                Err(e) => Err(DispatchError::Internal(e.to_string())),
            }
        }

        // ── Tool provider ──
        "tool-provider.provider-name" => {
            let name = tool_provider.provider_name();
            serde_json::to_value(name).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "tool-provider.available-tools" => {
            let tools = tool_provider.available_tools();
            serde_json::to_value(tools).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "tool-provider.execute-tool" => {
            let call: ToolCall = serde_json::from_value(params)
                .map_err(|e| DispatchError::InvalidParams(format!("invalid tool call: {e}")))?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| DispatchError::Internal(format!("runtime error: {e}")))?;
            let result = rt.block_on(tool_provider.execute_tool(call));
            match result {
                Ok(tool_result) => serde_json::to_value(tool_result)
                    .map_err(|e| DispatchError::Internal(e.to_string())),
                Err(e) => Err(DispatchError::Internal(e.to_string())),
            }
        }

        // ── Agent definition provider ──
        "agent-definition.provider-name" => {
            let name = agent_registry.provider_name();
            serde_json::to_value(name).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "agent-definition.list-agents" => {
            let workspace = params
                .get("workspace")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let agents = agent_registry.list_agents(workspace.as_deref());
            serde_json::to_value(agents).map_err(|e| DispatchError::Internal(e.to_string()))
        }
        "agent-definition.get-agent" => {
            let name = params.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                DispatchError::InvalidParams("'name' parameter required".to_string())
            })?;
            let workspace = params
                .get("workspace")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let agent = agent_registry.get_agent(name, workspace.as_deref());
            serde_json::to_value(agent).map_err(|e| DispatchError::Internal(e.to_string()))
        }

        _ => Err(DispatchError::MethodNotFound(format!(
            "Method not found: {}",
            method
        ))),
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use pulse_plugin_sdk::dev_adapter::DispatchError;

    fn make_test_fixtures() -> (
        CodingPackPlugin,
        BmadAgentInjector,
        BmadToolProvider,
        BmadAgentRegistry,
    ) {
        let manifest_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("_bmad/_config/agent-manifest.csv");
        let plugin = CodingPackPlugin;
        let injector = BmadAgentInjector::new(&manifest_path);
        let tool_provider = BmadToolProvider::new(
            plugin_coding_pack::workspace::WorkspaceConfig::resolve(None),
        );
        let agent_registry = BmadAgentRegistry::new(&manifest_path);
        (plugin, injector, tool_provider, agent_registry)
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "nonexistent.method", serde_json::Value::Null,
        );
        match result {
            Err(DispatchError::MethodNotFound(msg)) => {
                assert!(msg.contains("nonexistent.method"), "msg was: {msg}");
            }
            other => panic!("expected MethodNotFound, got: {:?}", other),
        }
    }

    #[test]
    fn lifecycle_on_install_returns_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-install", serde_json::json!({
                "plugin_dir": plugin_dir.to_str().unwrap(),
                "workflows_dir": tmp.path().join("workflows").to_str().unwrap(),
                "config_dir": tmp.path().join("config").to_str().unwrap(),
            }),
        );
        assert_eq!(result.unwrap()["ok"], true);
    }

    #[test]
    fn lifecycle_on_uninstall_returns_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-uninstall", serde_json::json!({
                "plugin_dir": tmp.path().join("plugin").to_str().unwrap(),
                "workflows_dir": tmp.path().join("workflows").to_str().unwrap(),
                "purge": false,
            }),
        );
        assert_eq!(result.unwrap()["ok"], true);
    }

    #[test]
    fn lifecycle_on_enable_returns_ok() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-enable", serde_json::json!({}),
        );
        assert_eq!(result.unwrap()["ok"], true);
    }

    #[test]
    fn lifecycle_on_disable_returns_ok() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-disable", serde_json::json!({}),
        );
        assert_eq!(result.unwrap()["ok"], true);
    }

    #[test]
    fn get_info_returns_valid_json() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.get-info", serde_json::json!({}),
        );
        let val = result.unwrap();
        assert_eq!(val["name"].as_str(), Some("plugin-coding-pack"));
    }

    #[test]
    fn health_check_returns_bool() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.health-check", serde_json::json!({}),
        );
        assert!(result.unwrap().is_boolean());
    }

    #[test]
    fn injector_name_returns_expected_value() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "config-injector.injector-name", serde_json::Value::Null,
        );
        assert_eq!(result.unwrap(), "bmad-agent-injector");
    }

    #[test]
    fn config_injector_priority_returns_number() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "config-injector.priority", serde_json::Value::Null,
        );
        assert_eq!(result.unwrap(), 100);
    }

    #[test]
    fn tool_provider_name_returns_expected_value() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "tool-provider.provider-name", serde_json::Value::Null,
        );
        assert!(result.unwrap().as_str().is_some());
    }

    #[test]
    fn agent_definition_provider_name_returns_expected_value() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "agent-definition.provider-name", serde_json::Value::Null,
        );
        assert!(result.unwrap().as_str().is_some());
    }

    #[test]
    fn dashboard_extension_pages_returns_valid_json() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "dashboard-extension.get-pages-json", serde_json::Value::Null,
        );
        assert!(result.unwrap().as_array().is_some());
    }

    #[test]
    fn step_executor_execute_with_invalid_params_returns_error() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "step-executor.execute",
            serde_json::json!({"task": "not-a-valid-task-object"}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn agent_definition_get_agent_missing_name_returns_error() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "agent-definition.get-agent", serde_json::json!({}),
        );
        match result {
            Err(DispatchError::InvalidParams(msg)) => {
                assert!(msg.contains("'name' parameter required"), "msg was: {msg}");
            }
            other => panic!("expected InvalidParams, got: {:?}", other),
        }
    }

    #[test]
    fn on_install_missing_plugin_dir_returns_invalid_params() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-install",
            serde_json::json!({"workflows_dir": "/tmp/w", "config_dir": "/tmp/c"}),
        );
        match result {
            Err(DispatchError::InvalidParams(msg)) => {
                assert!(msg.contains("missing plugin_dir"), "msg was: {msg}");
            }
            other => panic!("expected InvalidParams, got: {:?}", other),
        }
    }

    #[test]
    fn on_install_missing_workflows_dir_returns_invalid_params() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-install",
            serde_json::json!({"plugin_dir": "/tmp/p", "config_dir": "/tmp/c"}),
        );
        match result {
            Err(DispatchError::InvalidParams(msg)) => {
                assert!(msg.contains("missing workflows_dir"), "msg was: {msg}");
            }
            other => panic!("expected InvalidParams, got: {:?}", other),
        }
    }

    #[test]
    fn on_uninstall_missing_plugin_dir_returns_invalid_params() {
        let (plugin, injector, tool_provider, agent_registry) = make_test_fixtures();
        let result = dispatch_combined(
            &plugin, &injector, &tool_provider, &agent_registry,
            "plugin-lifecycle.on-uninstall",
            serde_json::json!({"workflows_dir": "/tmp/w"}),
        );
        match result {
            Err(DispatchError::InvalidParams(msg)) => {
                assert!(msg.contains("missing plugin_dir"), "msg was: {msg}");
            }
            other => panic!("expected InvalidParams, got: {:?}", other),
        }
    }
}
