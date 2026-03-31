#!/bin/bash
# Auto-dev daemon: polls board for ready-for-dev tasks and dispatches workflows
# Usage: ./scripts/auto-dev-daemon.sh [interval_secs]
#
# Prerequisites:
#   - pulse-server running on port 8080
#   - Board has tasks with status=ready-for-dev
#
# Stop: kill the process or Ctrl-C

set -euo pipefail

INTERVAL=${1:-60}
PORT=${PULSE_API_PORT:-8080}
BASE_URL="http://127.0.0.1:$PORT"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG_PATH="$SCRIPT_DIR/config/auto-loop.yaml"

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

    # Dispatch workflow
    INPUT="$TASK_TITLE"
    [ -n "$TASK_DESC" ] && INPUT="$TASK_TITLE\n\n$TASK_DESC"

    RESULT=$(curl -sf -X POST "$BASE_URL/api/v1/workflows/$WORKFLOW/execute" \
        -H "Content-Type: application/json" \
        -d "{\"input\":\"$INPUT\",\"metadata\":{\"workspace_path\":\"$SCRIPT_DIR\",\"task_id\":\"$TASK_ID\"}}" 2>&1)

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
