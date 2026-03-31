#!/usr/bin/env bash
# uninstall.sh — Remove plugin-coding-pack binaries and workflows
#
# Usage:
#   ./uninstall.sh              # Full uninstall (interactive confirmation)
#   ./uninstall.sh --plugins    # Remove plugin binaries only
#   ./uninstall.sh --yes        # Skip confirmation prompt
#
set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_DIR="$SCRIPT_DIR"

DEST="$PLUGIN_DIR/config/plugins"
WORKFLOWS_DIR="$PLUGIN_DIR/config/workflows"

# Plugin binaries installed by this pack
PLUGIN_BINS=(
  plugin-coding-pack
  bmad-method
  provider-claude-code
  plugin-git-ops
  plugin-git-worktree
  plugin-board
  plugin-auto-loop
  plugin-feedback-loop
  plugin-issue-sync
  plugin-test-runner
  plugin-workspace-tracker
)

# Workflow files owned by this pack
WORKFLOW_FILES=(
  coding-quick-dev.yaml
  coding-feature-dev.yaml
  coding-story-dev.yaml
  coding-bug-fix.yaml
  coding-refactor.yaml
  coding-review.yaml
  coding-memory-index.yaml
  bootstrap-plugin.yaml
  bootstrap-rebuild.yaml
  bootstrap-cycle.yaml
)

# ── Helpers ──────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}>>>${NC} $*"; }
ok()    { echo -e "${GREEN}  RM${NC} $*"; }
skip()  { echo -e "${YELLOW}  --${NC} $*"; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

# ── Parse args ───────────────────────────────────────────────────────────────

DO_PLUGINS=true
DO_WORKFLOWS=true
AUTO_YES=false

for arg in "$@"; do
  case "$arg" in
    --plugins)    DO_WORKFLOWS=false ;;
    --yes|-y)     AUTO_YES=true ;;
    -h|--help)
      echo "Usage: ./uninstall.sh [--plugins] [--yes] [-h|--help]"
      echo ""
      echo "  (no args)      Full uninstall: plugins + workflows"
      echo "  --plugins      Remove plugin binaries only"
      echo "  --yes, -y      Skip confirmation prompt"
      echo "  -h, --help     Show this help"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 1
      ;;
  esac
done

# ── Preview what will be removed ─────────────────────────────────────────────

echo -e "${BOLD}plugin-coding-pack uninstaller${NC}"
echo ""
echo "This will remove:"

if [[ "$DO_PLUGINS" == true ]]; then
  count=0
  for bin in "${PLUGIN_BINS[@]}"; do
    [[ -f "$DEST/$bin" ]] && count=$((count + 1))
  done
  echo "  - $count plugin binary(ies) from config/plugins/"
fi

if [[ "$DO_WORKFLOWS" == true ]]; then
  count=0
  for wf in "${WORKFLOW_FILES[@]}"; do
    [[ -f "$WORKFLOWS_DIR/$wf" ]] && count=$((count + 1))
  done
  echo "  - $count workflow file(s) from config/workflows/"
fi

echo ""

# ── Confirm ──────────────────────────────────────────────────────────────────

if [[ "$AUTO_YES" == false ]]; then
  read -rp "Proceed? [y/N] " answer
  case "$answer" in
    [yY]|[yY][eE][sS]) ;;
    *)
      echo "Aborted."
      exit 0
      ;;
  esac
fi

# ── Step 1: Remove plugin binaries ───────────────────────────────────────────

if [[ "$DO_PLUGINS" == true ]]; then
  header "Removing plugin binaries"

  for bin in "${PLUGIN_BINS[@]}"; do
    target="$DEST/$bin"
    if [[ -f "$target" ]]; then
      rm "$target"
      ok "$target"
    else
      skip "$bin (not found)"
    fi
  done
fi

# ── Step 2: Remove workflow files ────────────────────────────────────────────

if [[ "$DO_WORKFLOWS" == true ]]; then
  header "Removing workflow files"

  for wf in "${WORKFLOW_FILES[@]}"; do
    target="$WORKFLOWS_DIR/$wf"
    if [[ -f "$target" ]]; then
      rm "$target"
      ok "$target"
    else
      skip "$wf (not found)"
    fi
  done

  # Remove workflows dir if empty
  if [[ -d "$WORKFLOWS_DIR" ]] && [[ -z "$(ls -A "$WORKFLOWS_DIR" 2>/dev/null)" ]]; then
    rmdir "$WORKFLOWS_DIR"
    ok "$WORKFLOWS_DIR/ (empty, removed)"
  fi
fi

# ── Step 3: Database info ────────────────────────────────────────────────────

header "Database"

DB_FILE="$PLUGIN_DIR/pulse.db"
if [[ -f "$DB_FILE" ]]; then
  echo -e "  SQLite database remains at: ${CYAN}$DB_FILE${NC}"
  echo "  To remove execution history:  rm pulse.db pulse.db-shm pulse.db-wal"
else
  skip "No database file found"
fi

# ── Step 4: Build artifacts info ─────────────────────────────────────────────

header "Build artifacts"

if [[ -d "$PLUGIN_DIR/target" ]]; then
  echo -e "  Build cache remains at: ${CYAN}$PLUGIN_DIR/target/${NC}"
  echo "  To free disk space:  cargo clean"
else
  skip "No build artifacts found"
fi

# ── Done ─────────────────────────────────────────────────────────────────────

echo ""
echo -e "${GREEN}${BOLD}Uninstall complete.${NC}"
echo ""
echo "To reinstall:  ./install.sh"
