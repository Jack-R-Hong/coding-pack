use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Deserialized workflow YAML structure for validation.
#[derive(Debug, Deserialize)]
struct WorkflowYaml {
    name: Option<String>,
    version: Option<serde_yaml::Value>,
    #[serde(default)]
    steps: Option<Vec<StepYaml>>,
    #[serde(default)]
    requires: Option<Vec<RequiresEntry>>,
}

#[derive(Debug, Deserialize)]
struct StepYaml {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type")]
    step_type: Option<String>,
    #[serde(default)]
    executor: Option<String>,
    #[serde(default)]
    config: Option<StepConfigYaml>,
    #[serde(default)]
    depends_on: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct SessionParticipantYaml {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    activation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ConvergenceYaml {
    #[serde(default)]
    strategy: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct StepConfigYaml {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    context_from: Option<Vec<String>>,
    #[serde(default)]
    participants: Option<Vec<SessionParticipantYaml>>,
    #[serde(default)]
    convergence: Option<ConvergenceYaml>,
}

#[derive(Debug, Deserialize)]
struct RequiresEntry {
    plugin: String,
}

/// Detect cycles in a directed graph via DFS.
fn has_cycle(adj: &HashMap<String, Vec<String>>) -> bool {
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();

    fn dfs(
        node: &str,
        adj: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        in_stack: &mut HashSet<String>,
    ) -> bool {
        visited.insert(node.to_string());
        in_stack.insert(node.to_string());

        if let Some(neighbors) = adj.get(node) {
            for neighbor in neighbors {
                if !visited.contains(neighbor.as_str()) {
                    if dfs(neighbor, adj, visited, in_stack) {
                        return true;
                    }
                } else if in_stack.contains(neighbor.as_str()) {
                    return true;
                }
            }
        }

        in_stack.remove(node);
        false
    }

    for node in adj.keys() {
        if !visited.contains(node.as_str()) && dfs(node, adj, &mut visited, &mut in_stack) {
            return true;
        }
    }
    false
}

/// Validate that a workflow YAML file has required structure.
/// `plugins_dir` is the directory where plugin binaries are located.
pub fn validate_workflow_file(path: &Path, plugins_dir: &Path) -> Result<ValidationResult, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;

    let mut issues = Vec::new();

    // Parse YAML properly
    let workflow: WorkflowYaml = match serde_yaml::from_str(&content) {
        Ok(w) => w,
        Err(e) => {
            issues.push(format!("invalid YAML: {}", e));
            return Ok(ValidationResult {
                file: path.display().to_string(),
                valid: false,
                issues,
            });
        }
    };

    // Check required top-level fields
    if workflow.name.as_ref().is_none_or(|n| n.is_empty()) {
        issues.push("missing or empty 'name' field".to_string());
    }
    if workflow.version.is_none() {
        issues.push("missing 'version' field".to_string());
    }

    let steps = match &workflow.steps {
        Some(s) if !s.is_empty() => s,
        Some(_) => {
            issues.push("'steps' array is empty".to_string());
            return Ok(ValidationResult {
                file: path.display().to_string(),
                valid: issues.is_empty(),
                issues,
            });
        }
        None => {
            issues.push("missing 'steps' field".to_string());
            return Ok(ValidationResult {
                file: path.display().to_string(),
                valid: issues.is_empty(),
                issues,
            });
        }
    };

    // Validate each step
    for (i, step) in steps.iter().enumerate() {
        let step_label = step
            .id
            .as_deref()
            .map(|id| format!("step '{}'", id))
            .unwrap_or_else(|| format!("step[{}]", i));

        if step.id.is_none() {
            issues.push(format!("{}: missing 'id' field", step_label));
        }

        // Agent steps must have system_prompt
        if step.step_type.as_deref() == Some("agent") {
            let has_prompt = step
                .config
                .as_ref()
                .and_then(|c| c.system_prompt.as_ref())
                .is_some_and(|p| !p.is_empty());
            if !has_prompt {
                issues.push(format!(
                    "{}: agent step must have 'system_prompt' in config",
                    step_label
                ));
            }

            // Verify executor plugin binary exists
            if let Some(executor) = &step.executor {
                let plugin_path = plugins_dir.join(executor);
                if !plugin_path.exists() {
                    issues.push(format!(
                        "{}: executor '{}' not found at {}",
                        step_label,
                        executor,
                        plugin_path.display()
                    ));
                }
            }
        }

        // Session steps: validate participants and convergence
        if step.step_type.as_deref() == Some("session") {
            // Check participants
            match &step.config.as_ref().and_then(|c| c.participants.as_ref()) {
                Some(participants) if participants.len() >= 2 => {
                    for (j, p) in participants.iter().enumerate() {
                        match &p.agent {
                            Some(name) if name.starts_with("bmad/") => {}
                            Some(name) => issues.push(format!(
                                "{}: participant[{}] agent '{}' must use bmad/ prefix",
                                step_label, j, name
                            )),
                            None => issues.push(format!(
                                "{}: participant[{}] missing 'agent' field",
                                step_label, j
                            )),
                        }
                    }
                }
                Some(participants) => issues.push(format!(
                    "{}: session step requires at least 2 participants (found {})",
                    step_label,
                    participants.len()
                )),
                None => issues.push(format!(
                    "{}: session step requires 'participants' in config",
                    step_label
                )),
            }

            // Check convergence
            match &step.config.as_ref().and_then(|c| c.convergence.as_ref()) {
                Some(conv) => {
                    // Check strategy
                    const VALID_STRATEGIES: &[&str] = &["fixed_turns", "unanimous", "stagnation"];
                    match &conv.strategy {
                        Some(s) if VALID_STRATEGIES.contains(&s.as_str()) => {}
                        Some(s) => issues.push(format!(
                            "{}: convergence strategy '{}' must be one of: fixed_turns, unanimous, stagnation",
                            step_label, s
                        )),
                        None => issues.push(format!(
                            "{}: convergence requires 'strategy' field",
                            step_label
                        )),
                    }
                    // Check max_turns
                    match conv.max_turns {
                        Some(mt) if mt > 0 => {}
                        Some(0) => issues.push(format!(
                            "{}: convergence max_turns must be greater than 0",
                            step_label
                        )),
                        None => {} // max_turns is optional, defaults handled at runtime
                        _ => {}
                    }
                }
                None => issues.push(format!(
                    "{}: session step requires 'convergence' in config",
                    step_label
                )),
            }
        }
    }

    // Validate depends_on DAG: check references exist and detect cycles
    let step_ids: HashSet<String> = steps.iter().filter_map(|s| s.id.clone()).collect();

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for step in steps {
        let id = match &step.id {
            Some(id) => id.clone(),
            None => continue,
        };
        adj.entry(id.clone()).or_default();

        if let Some(deps) = &step.depends_on {
            for dep in deps {
                if !step_ids.contains(dep) {
                    issues.push(format!(
                        "step '{}': depends_on references unknown step '{}'",
                        id, dep
                    ));
                }
            }
            adj.get_mut(&id).unwrap().extend(deps.clone());
        }

        // Validate context_from references
        if let Some(ctx) = &step.config.as_ref().and_then(|c| c.context_from.as_ref()) {
            for ref_id in *ctx {
                if !step_ids.contains(ref_id) {
                    issues.push(format!(
                        "step '{}': context_from references unknown step '{}'",
                        id, ref_id
                    ));
                }
            }
        }
    }

    // Cycle detection via DFS
    if has_cycle(&adj) {
        issues.push("depends_on graph contains a cycle".to_string());
    }

    // Validate required plugin dependencies exist
    if let Some(requires) = &workflow.requires {
        for req in requires {
            let plugin_path = plugins_dir.join(&req.plugin);
            if !plugin_path.exists() {
                issues.push(format!(
                    "requires plugin '{}' but not found at {}",
                    req.plugin,
                    plugin_path.display()
                ));
            }
        }
    }

    Ok(ValidationResult {
        file: path.display().to_string(),
        valid: issues.is_empty(),
        issues,
    })
}

/// Validate that an agents.yaml file has required structure.
///
/// Each top-level key is an agent name. Each agent entry must have:
/// - `description` (non-empty string)
/// - `can_invoke` (list of agent names that must exist as top-level keys)
/// - `can_respond_to` (list of agent names that must exist as top-level keys)
pub fn validate_agents_yaml(path: &Path) -> Result<ValidationResult, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;

    let mut issues = Vec::new();

    let agents: HashMap<String, serde_yaml::Value> = match serde_yaml::from_str(&content) {
        Ok(a) => a,
        Err(e) => {
            issues.push(format!("invalid YAML: {}", e));
            return Ok(ValidationResult {
                file: path.display().to_string(),
                valid: false,
                issues,
            });
        }
    };

    let agent_names: HashSet<&String> = agents.keys().collect();

    for (name, value) in &agents {
        let mapping = match value.as_mapping() {
            Some(m) => m,
            None => {
                issues.push(format!("agent '{}': entry must be a mapping", name));
                continue;
            }
        };

        // Check description
        let desc_key = serde_yaml::Value::String("description".to_string());
        match mapping.get(&desc_key) {
            Some(v) => {
                let desc_str = v.as_str().unwrap_or("");
                if desc_str.is_empty() {
                    issues.push(format!("agent '{}': 'description' must not be empty", name));
                }
            }
            None => issues.push(format!("agent '{}': missing 'description' field", name)),
        }

        // Check can_invoke references
        let invoke_key = serde_yaml::Value::String("can_invoke".to_string());
        match mapping.get(&invoke_key) {
            Some(v) => {
                if let Some(list) = v.as_sequence() {
                    for item in list {
                        if let Some(ref_name) = item.as_str() {
                            if !agent_names.contains(&ref_name.to_string()) {
                                issues.push(format!(
                                    "agent '{}': can_invoke references unknown agent '{}'",
                                    name, ref_name
                                ));
                            }
                        }
                    }
                }
            }
            None => issues.push(format!("agent '{}': missing 'can_invoke' field", name)),
        }

        // Check can_respond_to references
        let respond_key = serde_yaml::Value::String("can_respond_to".to_string());
        match mapping.get(&respond_key) {
            Some(v) => {
                if let Some(list) = v.as_sequence() {
                    for item in list {
                        if let Some(ref_name) = item.as_str() {
                            if !agent_names.contains(&ref_name.to_string()) {
                                issues.push(format!(
                                    "agent '{}': can_respond_to references unknown agent '{}'",
                                    name, ref_name
                                ));
                            }
                        }
                    }
                }
            }
            None => issues.push(format!("agent '{}': missing 'can_respond_to' field", name)),
        }
    }

    Ok(ValidationResult {
        file: path.display().to_string(),
        valid: issues.is_empty(),
        issues,
    })
}

#[derive(Debug)]
pub struct ValidationResult {
    pub file: String,
    pub valid: bool,
    pub issues: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn valid_workflow_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - type: agent\n    id: s1\n    config:\n      system_prompt: hello"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn missing_name_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "version: 1\nsteps:\n  - id: s1\n    type: function").unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("name")));
    }

    #[test]
    fn missing_steps_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name: test\nversion: 1").unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("steps")));
    }

    #[test]
    fn invalid_yaml_reports_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name: test\n  bad indent: [").unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("invalid YAML")));
    }

    #[test]
    fn agent_step_without_system_prompt_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - id: s1\n    type: agent\n    config:\n      max_tokens: 1024"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("system_prompt")));
    }

    #[test]
    fn empty_steps_array_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name: test\nversion: 1\nsteps: []").unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("empty")));
    }

    #[test]
    fn step_missing_id_reports_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - type: function\n    config:\n      command: ['echo']"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("id")));
    }

    #[test]
    fn requires_missing_plugin_reports_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nrequires:\n  - plugin: nonexistent-plugin\nsteps:\n  - id: s1\n    type: function"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("nonexistent-plugin")));
    }

    #[test]
    fn depends_on_unknown_step_reports_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - id: s1\n    type: function\n    depends_on: [nonexistent]"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("depends_on") && i.contains("nonexistent")));
    }

    #[test]
    fn depends_on_cycle_reports_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - id: a\n    type: function\n    depends_on: [b]\n  - id: b\n    type: function\n    depends_on: [a]"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("cycle")));
    }

    #[test]
    fn context_from_unknown_step_reports_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - id: s1\n    type: agent\n    config:\n      system_prompt: hello\n      context_from: [ghost]"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("context_from") && i.contains("ghost")));
    }

    #[test]
    fn valid_depends_on_dag_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "name: test\nversion: 1\nsteps:\n  - id: a\n    type: function\n    depends_on: []\n  - id: b\n    type: function\n    depends_on: [a]"
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    // --- Session step validation tests ---

    #[test]
    fn session_step_with_valid_config_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name: test
version: 1
steps:
  - id: s1
    type: session
    config:
      participants:
        - agent: bmad/analyst
          activation: always
        - agent: bmad/architect
          activation: always
      convergence:
        strategy: fixed_turns
        max_turns: 3"#
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn session_step_with_one_participant_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name: test
version: 1
steps:
  - id: s1
    type: session
    config:
      participants:
        - agent: bmad/analyst
      convergence:
        strategy: fixed_turns"#
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("at least 2 participants")));
    }

    #[test]
    fn session_step_with_invalid_agent_name_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name: test
version: 1
steps:
  - id: s1
    type: session
    config:
      participants:
        - agent: bmad/analyst
        - agent: rogue-agent
      convergence:
        strategy: fixed_turns"#
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("rogue-agent") && i.contains("bmad/ prefix")));
    }

    #[test]
    fn session_step_with_invalid_convergence_strategy_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name: test
version: 1
steps:
  - id: s1
    type: session
    config:
      participants:
        - agent: bmad/analyst
        - agent: bmad/architect
      convergence:
        strategy: round_robin"#
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("round_robin") && i.contains("must be one of")));
    }

    #[test]
    fn session_step_with_zero_max_turns_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name: test
version: 1
steps:
  - id: s1
    type: session
    config:
      participants:
        - agent: bmad/analyst
        - agent: bmad/architect
      convergence:
        strategy: unanimous
        max_turns: 0"#
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("max_turns") && i.contains("greater than 0")));
    }

    #[test]
    fn session_step_missing_convergence_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"name: test
version: 1
steps:
  - id: s1
    type: session
    config:
      participants:
        - agent: bmad/analyst
        - agent: bmad/architect"#
        )
        .unwrap();

        let result = validate_workflow_file(&path, Path::new("config/plugins")).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("convergence")));
    }

    // --- Fixture-based workflow validation tests ---
    // These use real workflow fixture files to exercise more complex validation
    // code paths (diamond DAGs, context_from, multi-step agent workflows, etc.)

    /// Helper: resolve the path to a workflow fixture file.
    /// Tests using fixtures must be run from the crate root (cargo test does this).
    fn fixture_path(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/workflows")
            .join(name)
    }

    /// Mock plugins dir pointing to test fixtures so executor checks pass.
    fn mock_plugins_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-plugins")
    }

    #[test]
    fn fixture_parallel_diamond_dag_validates() {
        // Diamond DAG: root -> branch_a + branch_b -> join
        // Exercises multi-parent depends_on and cycle detection on a valid DAG.
        let path = fixture_path("test-parallel-functions.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_context_flow_validates() {
        // context_from referencing two valid producer steps.
        // Exercises the context_from reference validation on a valid case.
        let path = fixture_path("test-context-flow.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_function_only_chain_validates() {
        // Linear 3-step function chain with depends_on.
        let path = fixture_path("test-function-only.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_quality_gate_agent_steps_validate() {
        // Agent step with system_prompt + requires section.
        // Executor binaries (bmad-method, provider-claude-code) are in mock-plugins dir.
        let path = fixture_path("test-quality-gate.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_agent_mock_multi_step_validates() {
        // Two agent steps with depends_on, context_from, and requires.
        let path = fixture_path("test-agent-mock.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_missing_plugin_reports_requires_issue() {
        // requires: nonexistent-plugin. Validates the file-based requires check.
        let path = fixture_path("test-missing-plugin.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(!result.valid);
        assert!(
            result
                .issues
                .iter()
                .any(|i| i.contains("nonexistent-plugin")),
            "should report missing required plugin, got: {:?}",
            result.issues
        );
    }

    #[test]
    fn fixture_retry_loop_validates() {
        // Agent step with retry config + requires + executor pointing to mock-plugins.
        let path = fixture_path("test-retry-loop.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_template_vars_validates() {
        // Function steps with template variables in commands.
        let path = fixture_path("test-template-vars.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_timeout_step_validates() {
        // Function step with short timeout and executor.
        let path = fixture_path("test-timeout.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_optional_skip_validates() {
        // Step with optional: true flag (validator ignores unknown fields gracefully).
        let path = fixture_path("test-optional-skip.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_required_fail_validates() {
        // Required step that fails at runtime; structurally valid for the validator.
        let path = fixture_path("test-required-fail.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn fixture_pr_extraction_validates() {
        // Agent step followed by function step using PR fields; complex requires.
        let path = fixture_path("test-pr-extraction.yaml");
        let result = validate_workflow_file(&path, &mock_plugins_dir()).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    // --- agents.yaml validation tests ---

    #[test]
    fn valid_agents_yaml_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"bmad/analyst:
  description: Analyzes requirements
  can_invoke:
    - bmad/architect
  can_respond_to:
    - bmad/architect
bmad/architect:
  description: Designs architecture
  can_invoke:
    - bmad/analyst
  can_respond_to:
    - bmad/analyst"#
        )
        .unwrap();

        let result = validate_agents_yaml(&path).unwrap();
        assert!(result.valid, "issues: {:?}", result.issues);
    }

    #[test]
    fn agents_yaml_missing_description_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"bmad/analyst:
  can_invoke: []
  can_respond_to: []"#
        )
        .unwrap();

        let result = validate_agents_yaml(&path).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("description")));
    }

    #[test]
    fn agents_yaml_invalid_can_invoke_reference_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"bmad/analyst:
  description: Analyzes things
  can_invoke:
    - bmad/nonexistent
  can_respond_to: []"#
        )
        .unwrap();

        let result = validate_agents_yaml(&path).unwrap();
        assert!(!result.valid);
        assert!(result
            .issues
            .iter()
            .any(|i| i.contains("can_invoke") && i.contains("bmad/nonexistent")));
    }

    #[test]
    fn agents_yaml_parse_error_reports_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "  bad indent: [").unwrap();

        let result = validate_agents_yaml(&path).unwrap();
        assert!(!result.valid);
        assert!(result.issues.iter().any(|i| i.contains("invalid YAML")));
    }
}
