#!/usr/bin/env bash
# install.sh — Build and install plugin-coding-pack binaries
#
# Usage:
#   ./install.sh              # Full install: build + copy binaries
#   ./install.sh --skip-build # Skip cargo build, only sync binaries
#
set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_DIR="$SCRIPT_DIR"
SIBLINGS_DIR="$(dirname "$PLUGIN_DIR")"

DEST="$PLUGIN_DIR/config/plugins"

# Sibling plugins to build
SIBLINGS=(
  provider-claude-code
  git-ops
  git-worktree
  bmad-method
  plugin-board
  plugin-auto-loop
  plugin-feedback-loop
  plugin-issue-sync
  plugin-test-runner
  plugin-workspace-tracker
)

# ── Helpers ──────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}>>>${NC} $*"; }
ok()    { echo -e "${GREEN}  OK${NC} $*"; }
warn()  { echo -e "${YELLOW}  !!${NC} $*"; }
fail()  { echo -e "${RED}  FAIL${NC} $*"; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

# ── Parse args ───────────────────────────────────────────────────────────────

SKIP_BUILD=false

for arg in "$@"; do
  case "$arg" in
    --skip-build)    SKIP_BUILD=true ;;
    -h|--help)
      echo "Usage: ./install.sh [--skip-build] [-h|--help]"
      echo ""
      echo "  (no args)      Full install: build all plugins + copy binaries"
      echo "  --skip-build   Skip cargo build, sync existing binaries"
      echo "  -h, --help     Show this help"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 1
      ;;
  esac
done

# ── Step 1: Build ────────────────────────────────────────────────────────────

if [[ "$SKIP_BUILD" == false ]]; then
  header "Building plugins"

  info "Building plugin-coding-pack..."
  (cd "$PLUGIN_DIR" && cargo build --release 2>&1) && ok "plugin-coding-pack" || { fail "plugin-coding-pack"; exit 1; }

  for name in "${SIBLINGS[@]}"; do
    sibling_path="$SIBLINGS_DIR/$name"
    if [[ -d "$sibling_path" ]]; then
      info "Building $name..."
      (cd "$sibling_path" && cargo build --release 2>&1) && ok "$name" || warn "$name (build failed, skipping)"
    else
      warn "$name — directory not found at $sibling_path, skipping"
    fi
  done
fi

# ── Step 2: Install plugin binaries ─────────────────────────────────────────

header "Installing plugin binaries"

mkdir -p "$DEST"

# Orchestrator binary
src="$PLUGIN_DIR/target/release/plugin-coding-pack"
if [[ -f "$src" ]]; then
  rm -f "$DEST/plugin-coding-pack"
  cp "$src" "$DEST/"
  ok "plugin-coding-pack → $DEST/"
else
  warn "plugin-coding-pack binary not found (run without --skip-build)"
fi

# Sibling binaries
declare -A BIN_NAMES=(
  [provider-claude-code]="provider-claude-code"
  [git-ops]="plugin-git-ops"
  [git-worktree]="plugin-git-worktree"
  [bmad-method]="bmad-method"
  [plugin-board]="plugin-board"
  [plugin-auto-loop]="plugin-auto-loop"
  [plugin-feedback-loop]="plugin-feedback-loop"
  [plugin-issue-sync]="plugin-issue-sync"
  [plugin-test-runner]="plugin-test-runner"
  [plugin-workspace-tracker]="plugin-workspace-tracker"
)

for name in "${SIBLINGS[@]}"; do
  bin="${BIN_NAMES[$name]}"
  src="$SIBLINGS_DIR/$name/target/release/$bin"
  if [[ -f "$src" ]]; then
    rm -f "$DEST/$bin"
    cp "$src" "$DEST/"
    ok "$bin → $DEST/"
  else
    warn "$bin not found at $src"
  fi
done

# Remove stale shell-script stubs that are not real plugin binaries
for stub in plugin-memory plugin-git-pr; do
  stub_path="$DEST/$stub"
  if [[ -f "$stub_path" ]]; then
    rm "$stub_path"
    ok "removed stale stub: $stub"
  fi
done

# Install workflows via pulse if available
if command -v pulse &>/dev/null; then
  info "Registering pack with pulse..."
  (cd "$PLUGIN_DIR" && PULSE_DB_PATH=sqlite:pulse.db?mode=rwc pulse plugin install-pack coding --pack-dir plugin-packs 2>&1) \
    && ok "pulse plugin install-pack coding" \
    || warn "pulse install-pack failed (plugins still copied manually)"
fi

# ── Step 3: Validate ─────────────────────────────────────────────────────────

header "Validation"

missing=0
for bin in plugin-coding-pack bmad-method provider-claude-code plugin-git-ops plugin-git-worktree plugin-board \
           plugin-auto-loop plugin-feedback-loop plugin-issue-sync plugin-test-runner plugin-workspace-tracker; do
  if [[ -f "$DEST/$bin" ]]; then
    ok "$bin"
  else
    warn "$bin missing"
    ((missing++))
  fi
done

if command -v pulse &>/dev/null; then
  info "Running pulse registry validate..."
  (cd "$PLUGIN_DIR" && PULSE_DB_PATH=sqlite:pulse.db?mode=rwc pulse registry validate --config ./config 2>&1) \
    && ok "registry validate passed" \
    || warn "registry validate had issues"
fi

echo ""
if [[ $missing -eq 0 ]]; then
  echo -e "${GREEN}${BOLD}Installation complete.${NC}"
else
  echo -e "${YELLOW}${BOLD}Installation complete with $missing missing plugin(s).${NC}"
fi

echo ""
echo "Quick start:"
echo "  export PULSE_DB_PATH=sqlite:pulse.db?mode=rwc"
echo "  pulse run coding-quick-dev --config ./config -i '{\"input\": \"your task\"}'"
