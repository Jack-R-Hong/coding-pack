#!/usr/bin/env bash
# capture-otel-metrics.sh — Capture build/test/platform metrics as JSON
#
# Usage:
#   ./scripts/capture-otel-metrics.sh [project_dir]
#
# Output: JSON object to stdout with build, test, and platform metrics.
# Designed for use as a workflow function step in coding-otel-validated-dev.
set -uo pipefail

PROJECT_DIR="${1:-$(pwd)}"
PULSE_PORT="${PULSE_API_PORT:-8080}"
PULSE_URL="http://127.0.0.1:$PULSE_PORT"

# ── Build metrics ───────────────────────────────────────────────────────────

build_start=$(date +%s%N)
BUILD_OUTPUT=$(cd "$PROJECT_DIR" && cargo build --release 2>&1)
BUILD_EXIT=$?
build_end=$(date +%s%N)
BUILD_DURATION_MS=$(( (build_end - build_start) / 1000000 ))

# Binary sizes
BINARY_SIZES="{}"
if [[ -d "$PROJECT_DIR/target/release" ]]; then
  BINARY_SIZES=$(find "$PROJECT_DIR/target/release" -maxdepth 1 -type f -executable \
    ! -name '*.d' ! -name '*.so' ! -name '*.dylib' \
    -printf '"%f": %s,' 2>/dev/null | sed 's/,$//' | sed 's/^/{/;s/$/}/')
  [[ -z "$BINARY_SIZES" || "$BINARY_SIZES" == "{}" ]] && BINARY_SIZES="{}"
fi

# ── Test metrics ────────────────────────────────────────────────────────────

test_start=$(date +%s%N)
TEST_OUTPUT=$(cd "$PROJECT_DIR" && cargo test 2>&1)
TEST_EXIT=$?
test_end=$(date +%s%N)
TEST_DURATION_MS=$(( (test_end - test_start) / 1000000 ))

# Parse cargo test summary: "test result: ok. X passed; Y failed; Z ignored"
TESTS_PASSED=$(echo "$TEST_OUTPUT" | grep -oP '\d+(?= passed)' | awk '{s+=$1} END {print s+0}')
TESTS_FAILED=$(echo "$TEST_OUTPUT" | grep -oP '\d+(?= failed)' | awk '{s+=$1} END {print s+0}')
TESTS_IGNORED=$(echo "$TEST_OUTPUT" | grep -oP '\d+(?= ignored)' | awk '{s+=$1} END {print s+0}')
TESTS_TOTAL=$(( TESTS_PASSED + TESTS_FAILED + TESTS_IGNORED ))

# Extract failed test names
FAILED_TESTS=$(echo "$TEST_OUTPUT" | grep '^test .* FAILED$' | sed 's/^test //;s/ \.\.\..*FAILED$//' | head -20)
FAILED_TESTS_JSON="[]"
if [[ -n "$FAILED_TESTS" ]]; then
  FAILED_TESTS_JSON=$(echo "$FAILED_TESTS" | python3 -c "
import sys, json
tests = [line.strip() for line in sys.stdin if line.strip()]
print(json.dumps(tests))
" 2>/dev/null || echo "[]")
fi

# ── Pulse platform metrics (optional) ──────────────────────────────────────

PULSE_AVAILABLE=false
PULSE_METRICS="{}"
if curl -sf "$PULSE_URL/api/v1/health" > /dev/null 2>&1; then
  PULSE_AVAILABLE=true
  RAW_METRICS=$(curl -sf "$PULSE_URL/api/v1/metrics" 2>/dev/null || echo "")
  if [[ -n "$RAW_METRICS" ]]; then
    PULSE_METRICS=$(echo "$RAW_METRICS" | python3 -c "
import sys, json, re

metrics = {}
for line in sys.stdin:
    line = line.strip()
    if not line or line.startswith('#'):
        continue
    # Parse: metric_name{labels} value
    m = re.match(r'^(\w+?)(?:\{([^}]*)\})?\s+([\d.eE+\-]+|NaN|Inf|\+Inf|-Inf)$', line)
    if not m:
        continue
    name, labels_str, value = m.group(1), m.group(2) or '', m.group(3)
    try:
        val = float(value)
    except ValueError:
        continue

    # Only capture key pulse metrics (latest values)
    if name.startswith('pulse_'):
        key = name
        if labels_str:
            key += '{' + labels_str + '}'
        # For counters/gauges take the value; for histograms skip buckets
        if '_bucket' in name:
            continue
        metrics[key] = val

print(json.dumps(metrics, indent=2))
" 2>/dev/null || echo "{}")
  fi
fi

# ── Assemble JSON output ───────────────────────────────────────────────────

TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

cat <<ENDJSON
{
  "timestamp": "$TIMESTAMP",
  "project_dir": "$PROJECT_DIR",
  "build": {
    "duration_ms": $BUILD_DURATION_MS,
    "exit_code": $BUILD_EXIT,
    "success": $([ $BUILD_EXIT -eq 0 ] && echo true || echo false),
    "binary_sizes": $BINARY_SIZES
  },
  "tests": {
    "duration_ms": $TEST_DURATION_MS,
    "exit_code": $TEST_EXIT,
    "total": $TESTS_TOTAL,
    "passed": $TESTS_PASSED,
    "failed": $TESTS_FAILED,
    "ignored": $TESTS_IGNORED,
    "failed_tests": $FAILED_TESTS_JSON
  },
  "pulse": {
    "available": $PULSE_AVAILABLE,
    "metrics": $PULSE_METRICS
  }
}
ENDJSON
