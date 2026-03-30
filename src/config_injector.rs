//! BMAD Agent ConfigInjector — loads agent personas from CSV and injects them into LLM requests.

use async_trait::async_trait;
use pulse_plugin_sdk::types::injection::{
    Injection, InjectionError, InjectionQuery, InjectionTarget, InsertPosition,
};
use pulse_plugin_sdk::ConfigInjector;
use std::collections::HashMap;
use std::path::Path;

/// A single BMAD agent's persona data, parsed from the agent manifest CSV.
#[derive(Debug, Clone)]
pub struct AgentPersona {
    pub display_name: String,
    pub title: String,
    pub role: String,
    pub identity: String,
    pub communication_style: String,
    pub principles: String,
}

/// ConfigInjector that provides per-agent persona injections for BMAD agents.
///
/// Loads agent data from the CSV manifest at construction time and caches it
/// in memory for zero-I/O lookups during injection.
#[derive(Debug, Clone)]
pub struct BmadAgentInjector {
    agents: HashMap<String, AgentPersona>,
}

impl BmadAgentInjector {
    /// Create a new injector by loading the agent manifest CSV.
    ///
    /// If the file does not exist or cannot be read, returns an injector with
    /// an empty agent map and logs a warning.
    pub fn new(manifest_path: &Path) -> Self {
        let content = match std::fs::read_to_string(manifest_path) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(
                    "BmadAgentInjector: agent manifest not found at {}, initializing with empty agent map",
                    manifest_path.display()
                );
                return Self {
                    agents: HashMap::new(),
                };
            }
        };

        let mut agents = HashMap::new();
        let logical_rows = split_csv_rows(&content);

        // Parse header to get column indices
        let header = match logical_rows.first() {
            Some(h) => h.as_str(),
            None => {
                tracing::warn!("BmadAgentInjector: manifest CSV is empty");
                return Self { agents };
            }
        };

        let header_fields = parse_csv_row(header);
        let col_index =
            |name: &str| -> Option<usize> { header_fields.iter().position(|f| f == name) };

        let idx_name = match col_index("name") {
            Some(i) => i,
            None => {
                tracing::warn!("BmadAgentInjector: CSV header missing 'name' column");
                return Self { agents };
            }
        };
        let idx_display_name = col_index("displayName");
        let idx_title = col_index("title");
        let idx_role = col_index("role");
        let idx_identity = col_index("identity");
        let idx_communication_style = col_index("communicationStyle");
        let idx_principles = col_index("principles");

        let max_required = [
            Some(idx_name),
            idx_display_name,
            idx_title,
            idx_role,
            idx_identity,
            idx_communication_style,
            idx_principles,
        ]
        .iter()
        .filter_map(|i| *i)
        .max()
        .unwrap_or(idx_name);

        for row in &logical_rows[1..] {
            if row.trim().is_empty() {
                continue;
            }

            let fields = parse_csv_row(row);
            if fields.len() <= max_required {
                tracing::warn!(
                    "BmadAgentInjector: skipping CSV row with insufficient columns (got {}, need {})",
                    fields.len(),
                    max_required + 1
                );
                continue;
            }

            let name = fields[idx_name].trim();
            if name.is_empty() {
                tracing::warn!("BmadAgentInjector: skipping CSV row with empty name");
                continue;
            }
            let sdk_key = format!("bmad/{name}");

            let get_field = |idx: Option<usize>| -> String {
                match idx {
                    Some(i) if i < fields.len() => fields[i].clone(),
                    _ => String::new(),
                }
            };

            let persona = AgentPersona {
                display_name: get_field(idx_display_name),
                title: get_field(idx_title),
                role: get_field(idx_role),
                identity: get_field(idx_identity),
                communication_style: get_field(idx_communication_style),
                principles: get_field(idx_principles),
            };

            agents.insert(sdk_key, persona);
        }

        Self { agents }
    }

    /// Returns the number of loaded agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Returns whether an agent with the given SDK name is loaded.
    pub fn has_agent(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }
}

/// Build the system prompt injection content from an agent persona.
fn build_system_prompt(persona: &AgentPersona) -> String {
    format!(
        "You are {}, {}.\n\n## Identity\n{}\n\n## Communication Style\n{}\n\n## Role\n{}",
        persona.display_name,
        persona.title,
        persona.identity,
        persona.communication_style,
        persona.role
    )
}

/// Build the principles injection content from an agent persona.
fn build_principles(persona: &AgentPersona) -> String {
    format!("## Principles\n{}", persona.principles)
}

#[async_trait]
impl ConfigInjector for BmadAgentInjector {
    fn injector_name(&self) -> &str {
        "bmad-agent-injector"
    }

    fn priority(&self) -> i32 {
        100
    }

    fn applies_to(&self, query: &InjectionQuery) -> bool {
        match &query.agent_name {
            Some(name) => name.starts_with("bmad/") && self.agents.contains_key(name.as_str()),
            None => false,
        }
    }

    async fn provide_injections(
        &self,
        query: &InjectionQuery,
    ) -> Result<Vec<Injection>, InjectionError> {
        let agent_name = match &query.agent_name {
            Some(name) => name,
            None => return Err(InjectionError::custom("no agent_name in query")),
        };

        match self.agents.get(agent_name.as_str()) {
            Some(persona) => {
                let injection_1 = Injection::new(build_system_prompt(persona))
                    .with_target(InjectionTarget::SystemPrompt {
                        position: InsertPosition::Prepend,
                    })
                    .with_priority(100)
                    .with_source("bmad-agent-injector");

                let injection_2 = Injection::new(build_principles(persona))
                    .with_target(InjectionTarget::SystemPrompt {
                        position: InsertPosition::Append,
                    })
                    .with_priority(110)
                    .with_source("bmad-agent-injector");

                Ok(vec![injection_1, injection_2])
            }
            None => Err(InjectionError::custom(format!(
                "unknown BMAD agent: {agent_name}"
            ))),
        }
    }
}

/// Split CSV content into logical rows, handling quoted fields that span newlines.
pub fn split_csv_rows(content: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for line in content.lines() {
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);

        // Count unescaped quotes to determine if we're inside a quoted field
        let mut i = 0;
        let bytes = line.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2; // skip escaped quote
                } else {
                    in_quotes = !in_quotes;
                    i += 1;
                }
            } else {
                i += 1;
            }
        }

        if !in_quotes {
            rows.push(std::mem::take(&mut current));
        }
    }

    // If there's remaining content (unclosed quote), add it as the last row
    if !current.is_empty() {
        rows.push(current);
    }

    rows
}

/// Parse a single CSV row, handling quoted fields that may contain commas.
pub fn parse_csv_row(row: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                // Check for escaped quote (double-quote)
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            ',' if !in_quotes => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }

    fields.push(current.trim().to_string());
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_manifest_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_bmad/_config/agent-manifest.csv")
    }

    #[test]
    fn test_applies_to_bmad_agents_only() {
        let injector = BmadAgentInjector::new(&test_manifest_path());

        // bmad/ prefixed agents should match
        let query_architect = InjectionQuery::new().with_agent_name("bmad/architect");
        assert!(injector.applies_to(&query_architect));

        let query_dev = InjectionQuery::new().with_agent_name("bmad/dev");
        assert!(injector.applies_to(&query_dev));

        // Non-bmad agent should not match
        let query_other = InjectionQuery::new().with_agent_name("other/agent");
        assert!(!injector.applies_to(&query_other));

        // No agent_name should not match
        let query_none = InjectionQuery::new();
        assert!(!injector.applies_to(&query_none));
    }

    #[tokio::test]
    async fn test_provides_injections_for_all_9_agents() {
        let injector = BmadAgentInjector::new(&test_manifest_path());
        assert_eq!(injector.agent_count(), 9);

        let agent_names = [
            "bmad/analyst",
            "bmad/architect",
            "bmad/dev",
            "bmad/pm",
            "bmad/qa",
            "bmad/quick-flow-solo-dev",
            "bmad/sm",
            "bmad/tech-writer",
            "bmad/ux-designer",
        ];

        for name in &agent_names {
            let query = InjectionQuery::new().with_agent_name(*name);
            let result = injector.provide_injections(&query).await;
            let injections =
                result.unwrap_or_else(|e| panic!("provide_injections failed for {name}: {e}"));

            assert_eq!(
                injections.len(),
                2,
                "expected 2 injections for {name}, got {}",
                injections.len()
            );

            // First injection: system prompt at priority 100
            assert_eq!(injections[0].priority, 100);
            assert!(
                !injections[0].content.is_empty(),
                "content empty for {name}"
            );
            assert_eq!(injections[0].source, "bmad-agent-injector");

            // Second injection: principles at priority 110
            assert_eq!(injections[1].priority, 110);
            assert!(
                !injections[1].content.is_empty(),
                "principles empty for {name}"
            );
            assert_eq!(injections[1].source, "bmad-agent-injector");
        }
    }

    #[tokio::test]
    async fn test_unknown_agent_returns_error() {
        let injector = BmadAgentInjector::new(&test_manifest_path());

        let query = InjectionQuery::new().with_agent_name("bmad/nonexistent");
        let result = injector.provide_injections(&query).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown BMAD agent"),
            "error message should contain 'unknown BMAD agent', got: {msg}"
        );
    }

    #[test]
    fn test_graceful_degradation() {
        let injector = BmadAgentInjector::new(Path::new("/nonexistent/path.csv"));
        assert_eq!(injector.agent_count(), 0);

        // With empty agent map, applies_to returns false (agent not in manifest)
        let query = InjectionQuery::new().with_agent_name("bmad/architect");
        assert!(!injector.applies_to(&query));
    }

    #[test]
    fn test_send_sync() {
        let injector = BmadAgentInjector::new(&test_manifest_path());
        let _arc: Arc<dyn ConfigInjector> = Arc::new(injector);
    }

    #[test]
    fn test_csv_parser_handles_quoted_fields() {
        let row = r#""hello","world, with comma","no quote"#;
        let fields = parse_csv_row(row);
        assert_eq!(fields, vec!["hello", "world, with comma", "no quote"]);
    }

    #[test]
    fn test_csv_parser_handles_unquoted_fields() {
        let row = "a,b,c";
        let fields = parse_csv_row(row);
        assert_eq!(fields, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_agent_count_and_has_agent() {
        let injector = BmadAgentInjector::new(&test_manifest_path());
        assert_eq!(injector.agent_count(), 9);
        assert!(injector.has_agent("bmad/architect"));
        assert!(injector.has_agent("bmad/dev"));
        assert!(!injector.has_agent("bmad/nonexistent"));
    }

    #[test]
    fn test_injector_name_and_priority() {
        let injector = BmadAgentInjector::new(&test_manifest_path());
        assert_eq!(injector.injector_name(), "bmad-agent-injector");
        assert_eq!(injector.priority(), 100);
    }

    // ── CSV edge case tests ──────────────────────────────────────────────

    #[test]
    fn test_split_csv_rows_empty_string() {
        let rows = split_csv_rows("");
        assert!(rows.is_empty(), "expected empty vec, got: {:?}", rows);
    }

    #[test]
    fn test_split_csv_rows_header_only() {
        let rows = split_csv_rows("name,title,role");
        // split_csv_rows returns all logical rows including the header
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], "name,title,role");
        // When used by BmadAgentInjector, the first row is the header and
        // the rest (none) are data rows, so agent_count would be 0.
    }

    #[test]
    fn test_split_csv_rows_unclosed_quote() {
        // An unclosed quote should not panic; the remaining content is
        // pushed as the final row.
        let input = "name,title\n\"unclosed value,still going";
        let rows = split_csv_rows(input);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], "name,title");
        assert_eq!(rows[1], "\"unclosed value,still going");
    }

    #[test]
    fn test_parse_csv_row_fewer_columns_than_expected() {
        // A row with only 2 columns when we expect 7 should not panic.
        let fields = parse_csv_row("alice,dev");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0], "alice");
        assert_eq!(fields[1], "dev");

        // BmadAgentInjector skips rows with insufficient columns, so let's
        // verify that behavior via the injector as well.
        let csv = "name,displayName,title,role,identity,communicationStyle,principles\nalice,dev";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-manifest.csv");
        std::fs::write(&path, csv).unwrap();
        let injector = BmadAgentInjector::new(&path);
        // The short row should be skipped, so no agents are loaded.
        assert_eq!(injector.agent_count(), 0);
    }
}
