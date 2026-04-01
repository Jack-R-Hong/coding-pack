## Available Capabilities

### Native Tools (always available)
- bash: run shell commands (git, cargo, scripts)
- file read/write: read and modify source files
- git: diff, log, status, commit, branch operations

### Pulse Skills (via MCP tools)
- `pulse_tp_bmad_validate_pack` — health-check plugins, workflows, config
- `pulse_tp_bmad_list_workflows` — enumerate available workflows
- `pulse_tp_bmad_list_plugins` — list installed plugins and status
- `pulse_tp_bmad_data_query` — query board/dashboard data
- `pulse_tp_bmad_data_mutate` — update board/dashboard data
- `pulse_tp_bmad_auto_dev_next` — pick and execute next board task

### Workflow Dispatch (via MCP)
- `mcp__pulse__dispatch` — dispatch a named workflow to pulse engine (sync or async)

### Agent Mesh (via MCP)
- `mcp__pulse-agent-mesh__list_agents` — list available agents
- `mcp__pulse-agent-mesh__invoke_agent` — invoke a specialized agent (architect, qa, dev)

### Metrics & Validation
- `./scripts/capture-otel-metrics.sh .` — capture build/test/pulse metrics as JSON
- `cargo test` — run test suite
- `cargo clippy` — static analysis
