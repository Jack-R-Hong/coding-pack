# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- **New workflows**: `coding-docs`, `coding-hotfix`, `coding-migration`, `coding-perf-review`, `coding-release`, `coding-security-audit`, `project-init` — covering the full SDLC gap
- **3-step coding-quick-dev workflow**: spec → implement → test now runs end-to-end; `run_tests` step restored via `plugin-test-runner`
- **Dashboard pages**: board, worktree, and sprint views added to `dashboard/manifest.json` with proxy bridge routing to `plugin-board`
- **Execution history module** (`src/execution_history.rs`): persists workflow run results for dashboard replay
- **Config injector** (`src/config_injector.rs`) and **validator** (`src/validator.rs`) modules supporting runtime config and schema enforcement
- **`plugin-board` as optional dependency** in `plugin-packs/coding.toml`; `install.sh` skips gracefully when not present
- Documented full plugin dependency tree (pack plugins vs bridge plugins) in `README.md` and `docs/architecture.md`

### Changed

- **Project renamed** from `bmad-method-flow` to `coding-pack` across all configs, docs, and dashboard mock data
- **Agent name corrected**: `bmad/quick-flow` → `bmad/quick-flow-solo-dev` in `coding-hotfix` and related workflows (was causing step dispatch failures)
- `coding-quick-dev` now uses `_exec_input` for the commit message instead of a hardcoded string

### Fixed

- **Removed `plugin-memory` dependency** from 7 workflows (`coding-bug-fix`, `coding-feature-dev`, `coding-parallel-review`, `coding-refactor`, `coding-review`, `coding-story-dev`); deleted `coding-memory-index.yaml` — all 18 workflows now load without unresolved plugin references
- **Workflow YAML schema compliance**: removed invalid `optional: true` from `requires` entries and steps; added missing `command` field to `run_tests` function steps — previously caused pulse-server startup failure
- **Fixed 6 broken dashboard endpoints**: `workflows/execute`, `agents/{id}`, `worktrees/{id}`, and 3 display-customization routes now return correct responses
- **`pulse.yaml` server path config**: corrected binary path so `pulse serve` resolves the plugin correctly
- **`.gitignore` hardened**: added `.claude/pulse` to prevent Claude Code symlink from leaking into commits; removed auto-generated test artifacts (`hello()`, `greet()`, `multiply()` stubs written by auto-dev loop)
- **`depends_on` chains** repaired across all workflows after memory step removal to maintain correct step ordering
