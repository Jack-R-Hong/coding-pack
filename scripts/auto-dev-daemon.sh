#!/bin/bash
# Auto-dev daemon: polls board for ready-for-dev tasks and dispatches workflows
# Usage: ./scripts/auto-dev-daemon.sh [interval_secs]
#
# Prerequisites:
#   - pulse-server running on port 8080
#   - Board has tasks with status=ready-for-dev
#
# Stop: kill the process or Ctrl-C

set -uo pipefail

INTERVAL=${1:-60}
PORT=${PULSE_API_PORT:-8080}
BASE_URL="http://127.0.0.1:$PORT"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOCKFILE="$SCRIPT_DIR/.auto-dev.lock"

# Prevent multiple daemon instances
if [ -f "$LOCKFILE" ]; then
    OLD_PID=$(cat "$LOCKFILE" 2>/dev/null)
    if kill -0 "$OLD_PID" 2>/dev/null; then
        echo "[auto-dev] Another daemon is running (PID $OLD_PID). Exiting."
        exit 1
    fi
    echo "[auto-dev] Stale lock file found (PID $OLD_PID dead). Removing."
    rm -f "$LOCKFILE"
fi
echo $$ > "$LOCKFILE"
trap 'rm -f "$LOCKFILE"; exit' INT TERM EXIT

echo "[auto-dev] Starting daemon (interval=${INTERVAL}s, port=$PORT)"
cd "$SCRIPT_DIR"

while true; do
    # Check server health
    if ! curl -sf "$BASE_URL/api/v1/health" > /dev/null 2>&1; then
        echo "[auto-dev] $(date +%H:%M:%S) Server not reachable, skipping"
        sleep "$INTERVAL"
        continue
    fi

    # Find next ready-for-dev task from board
    TASK_JSON=$(curl -sf "$BASE_URL/api/v1/plugins/plugin-board/data/board/data" | python3 -c "
import sys,json
d=json.load(sys.stdin)
ready=[i for i in d.get('items',[]) if i['status']=='ready-for-dev']
if not ready:
    print('null')
else:
    # Pick highest priority (or first)
    print(json.dumps(ready[0]))
" 2>/dev/null)

    if [ "$TASK_JSON" = "null" ] || [ -z "$TASK_JSON" ]; then
        echo "[auto-dev] $(date +%H:%M:%S) No ready-for-dev tasks"
        sleep "$INTERVAL"
        continue
    fi

    # Extract task info
    TASK_ID=$(echo "$TASK_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])")
    TASK_TITLE=$(echo "$TASK_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin)['title'])")
    TASK_DESC=$(echo "$TASK_JSON" | python3 -c "import sys,json; print(json.load(sys.stdin).get('description',''))")
    LABELS=$(echo "$TASK_JSON" | python3 -c "import sys,json; print(','.join(json.load(sys.stdin).get('labels',[])))")

    # Route label to workflow
    WORKFLOW="coding-quick-dev"  # default
    case ",$LABELS," in
        *,story,*)   WORKFLOW="coding-story-dev" ;;
        *,bug,*)     WORKFLOW="coding-bug-fix" ;;
        *,refactor,*) WORKFLOW="coding-refactor" ;;
        *,feature,*) WORKFLOW="coding-feature-dev" ;;
        *,review,*)  WORKFLOW="coding-review" ;;
        *,pr-fix,*)  WORKFLOW="coding-pr-fix" ;;
    esac

    echo "[auto-dev] $(date +%H:%M:%S) Found task: $TASK_TITLE (labels=$LABELS)"
    echo "[auto-dev]   → Dispatching workflow: $WORKFLOW"

    # Update task status to in-progress
    curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"in-progress\"}" > /dev/null 2>&1 || true

    # Capture pre-execution metrics baseline (completed count before dispatch)
    _PRE_METRICS=$(curl -sf "$BASE_URL/api/v1/metrics" 2>/dev/null || true)
    PRE_COMPLETED=$(echo "$_PRE_METRICS" | grep -m1 'pulse_tasks_total{.*state="completed"' | awk '{printf "%d\n", $NF}')
    PRE_COMPLETED=${PRE_COMPLETED:-0}

    # Dispatch workflow — use python to safely JSON-encode the input
    PAYLOAD=$(python3 -c "
import json, sys
title = sys.argv[1]
desc = sys.argv[2]
workspace = sys.argv[3]
task_id = sys.argv[4]
inp = title if not desc else f'{title}\n\n{desc}'
print(json.dumps({'input': inp, 'metadata': {'workspace_path': workspace, 'task_id': task_id}}))
" "$TASK_TITLE" "$TASK_DESC" "$SCRIPT_DIR" "$TASK_ID")

    RESULT=$(curl -sf -X POST "$BASE_URL/api/v1/workflows/$WORKFLOW/execute" \
        -H "Content-Type: application/json" \
        -d "$PAYLOAD" 2>&1)

    WF_TASK_ID=$(echo "$RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin).get('task_id',''))" 2>/dev/null)

    if [ -n "$WF_TASK_ID" ] && [ "$WF_TASK_ID" != "" ]; then
        echo "[auto-dev]   → Submitted: task_id=$WF_TASK_ID"

        # Wait for completion (poll every 10s, max 5min)
        for i in $(seq 1 30); do
            sleep 10
            STATE=$(curl -sf "$BASE_URL/api/v1/tasks/$WF_TASK_ID" | python3 -c "import sys,json; print(json.load(sys.stdin)['state'])" 2>/dev/null)
            if [ "$STATE" = "Completed" ]; then
                echo "[auto-dev]   → Completed in $((i*10))s"
                curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
                    -H "Content-Type: application/json" \
                    -d "{\"status\":\"done\"}" > /dev/null 2>&1 || true

                # Fetch post-execution metrics and log summary
                _POST_METRICS=$(curl -sf "$BASE_URL/api/v1/metrics" 2>/dev/null || true)
                POST_COMPLETED=$(echo "$_POST_METRICS" | grep -m1 'pulse_tasks_total{.*state="completed"' | awk '{printf "%d\n", $NF}')
                POST_FAILED=$(echo "$_POST_METRICS"    | grep -m1 'pulse_tasks_total{.*state="failed"'    | awk '{printf "%d\n", $NF}')
                POST_TOKENS=$(echo "$_POST_METRICS"    | grep -m1 'pulse_tokens_total'                    | awk '{printf "%d\n", $NF}')
                POST_COMPLETED=${POST_COMPLETED:-0}
                POST_FAILED=${POST_FAILED:-0}
                POST_TOKENS=${POST_TOKENS:-0}
                echo "[auto-dev]   → Metrics: completed=$POST_COMPLETED, failed=$POST_FAILED, tokens=$POST_TOKENS"
                if [ "$POST_COMPLETED" -le "$PRE_COMPLETED" ] 2>/dev/null; then
                    echo "[auto-dev]   ⚠ Warning: completion metric did not increase"
                fi

                break
            elif [ "$STATE" = "Failed" ]; then
                echo "[auto-dev]   → Failed after $((i*10))s"
                curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
                    -H "Content-Type: application/json" \
                    -d "{\"status\":\"backlog\"}" > /dev/null 2>&1 || true
                break
            fi
        done
    else
        echo "[auto-dev]   → Failed to submit workflow"
        curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
            -H "Content-Type: application/json" \
            -d "{\"status\":\"backlog\"}" > /dev/null 2>&1 || true
    fi

    sleep "$INTERVAL"
done
