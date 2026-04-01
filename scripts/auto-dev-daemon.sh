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
ACTIVE_WORKTREE=""
ACTIVE_BRANCH=""
cleanup_on_exit() {
    if [ -n "$ACTIVE_WORKTREE" ]; then
        echo "[auto-dev] Cleaning up worktree on exit: $ACTIVE_WORKTREE"
        git -C "$SCRIPT_DIR" worktree remove "$ACTIVE_WORKTREE" --force 2>/dev/null || true
        git -C "$SCRIPT_DIR" branch -D "$ACTIVE_BRANCH" 2>/dev/null || true
    fi
    rm -f "$LOCKFILE"
}
trap 'cleanup_on_exit; exit' INT TERM EXIT

echo "[auto-dev] Starting daemon (interval=${INTERVAL}s, port=$PORT)"
cd "$SCRIPT_DIR"

# ── Startup: recover stale in-progress tasks ─────────────────────────────
if curl -sf "$BASE_URL/api/v1/health" > /dev/null 2>&1; then
    STALE_TASKS=$(curl -sf "$BASE_URL/api/v1/plugins/plugin-board/data/board/data" | python3 -c "
import sys,json
d=json.load(sys.stdin)
stale=[i.get('id') for i in d.get('items',[]) if i.get('status')=='in-progress' and i.get('id')]
for t in stale: print(t)
" 2>/dev/null)
    for STALE_ID in $STALE_TASKS; do
        echo "[auto-dev] Recovering stale task: $STALE_ID (in-progress → backlog)"
        # Clean up any leftover remote branch from previous run
        git -C "$SCRIPT_DIR" push origin --delete "auto-dev/${STALE_ID}" 2>/dev/null || true
        curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$STALE_ID/metadata" \
            -H "Content-Type: application/json" \
            -d '{"status":"backlog"}' > /dev/null 2>&1 || true
    done
else
    echo "[auto-dev] ⚠ Server unavailable at startup — stale task recovery skipped"
fi

# ── Helper: run feedback loop ─────────────────────────────────────────────
run_feedback_loop() {
    FEEDBACK_RESULT=$(curl -sf -X POST "$BASE_URL/api/v1/plugins/plugin-feedback-loop/execute" \
        -H "Content-Type: application/json" \
        -d '{"action": "run-cycle"}' 2>/dev/null) || true

    if [ -n "$FEEDBACK_RESULT" ] && [ "$FEEDBACK_RESULT" != "null" ]; then
        FIX_COUNT=$(echo "$FEEDBACK_RESULT" | python3 -c "
import sys,json
try:
    d=json.load(sys.stdin)
    c=d.get('content',d)
    if isinstance(c,str): c=json.loads(c)
    tasks=c.get('fix_tasks_created',c.get('tasks_created',0))
    print(tasks if isinstance(tasks,int) else len(tasks) if isinstance(tasks,list) else 0)
except: print(0)
" 2>/dev/null)
        if [ "${FIX_COUNT:-0}" -gt 0 ]; then
            echo "[auto-dev] $(date +%H:%M:%S) Feedback loop: created $FIX_COUNT fix task(s) from PR reviews"
        fi
    fi
}

while true; do
    # Check server health
    if ! curl -sf "$BASE_URL/api/v1/health" > /dev/null 2>&1; then
        echo "[auto-dev] $(date +%H:%M:%S) Server not reachable, skipping"
        sleep "$INTERVAL"
        continue
    fi

    # [H-3 fix] Always run feedback loop, even when board is empty
    run_feedback_loop

    # Find next ready-for-dev task from board
    TASK_JSON=$(curl -sf "$BASE_URL/api/v1/plugins/plugin-board/data/board/data" | python3 -c "
import sys,json
d=json.load(sys.stdin)
ready=[i for i in d.get('items',[]) if i.get('status')=='ready-for-dev']
if not ready:
    print('null')
else:
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

    # Create an isolated git worktree for this task
    BRANCH_NAME="auto-dev/${TASK_ID}"
    WORKTREE_DIR="${TMPDIR:-/tmp}/pulse-worktree-${TASK_ID}"

    # Clean up stale worktree/branch from previous failed runs
    if [ -d "$WORKTREE_DIR" ]; then
        echo "[auto-dev]   → Removing stale worktree: $WORKTREE_DIR"
        git -C "$SCRIPT_DIR" worktree remove "$WORKTREE_DIR" --force 2>/dev/null || rm -rf "$WORKTREE_DIR"
    fi
    if git -C "$SCRIPT_DIR" rev-parse --verify "$BRANCH_NAME" >/dev/null 2>&1; then
        echo "[auto-dev]   → Removing stale local branch: $BRANCH_NAME"
        git -C "$SCRIPT_DIR" branch -D "$BRANCH_NAME" 2>/dev/null || true
    fi
    # [M-1 fix] Clean stale remote branch to avoid push conflicts
    git -C "$SCRIPT_DIR" push origin --delete "$BRANCH_NAME" 2>/dev/null || true

    if ! git -C "$SCRIPT_DIR" worktree add "$WORKTREE_DIR" -b "$BRANCH_NAME" 2>&1; then
        echo "[auto-dev]   → Failed to create worktree for $BRANCH_NAME, skipping task"
        sleep "$INTERVAL"
        continue
    fi
    ACTIVE_WORKTREE="$WORKTREE_DIR"
    ACTIVE_BRANCH="$BRANCH_NAME"
    echo "[auto-dev]   → Worktree created: $WORKTREE_DIR (branch=$BRANCH_NAME)"

    # Update task status to in-progress
    curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
        -H "Content-Type: application/json" \
        -d "{\"status\":\"in-progress\"}" > /dev/null 2>&1 || true

    # Dispatch workflow
    PAYLOAD=$(python3 -c "
import json, sys
title = sys.argv[1]
desc = sys.argv[2]
workspace = sys.argv[3]
task_id = sys.argv[4]
inp = title if not desc else f'{title}\n\n{desc}'
print(json.dumps({'input': inp, 'metadata': {'workspace_path': workspace, 'task_id': task_id}}))
" "$TASK_TITLE" "$TASK_DESC" "$WORKTREE_DIR" "$TASK_ID")

    RESULT=$(curl -sf -X POST "$BASE_URL/api/v1/workflows/$WORKFLOW/execute" \
        -H "Content-Type: application/json" \
        -d "$PAYLOAD" 2>&1)

    WF_TASK_ID=$(echo "$RESULT" | python3 -c "import sys,json; print(json.load(sys.stdin).get('task_id',''))" 2>/dev/null)

    if [ -n "$WF_TASK_ID" ] && [ "$WF_TASK_ID" != "" ]; then
        echo "[auto-dev]   → Submitted: task_id=$WF_TASK_ID"

        # Wait for completion (poll every 10s, max 5min)
        SKIP_CLEANUP=false
        for i in $(seq 1 30); do
            sleep 10
            STATE=$(curl -sf "$BASE_URL/api/v1/tasks/$WF_TASK_ID" | python3 -c "import sys,json; print(json.load(sys.stdin)['state'])" 2>/dev/null)
            if [ "$STATE" = "Completed" ]; then
                echo "[auto-dev]   → Completed in $((i*10))s"

                # Check QA verdict from apply_fixes step output
                QA_REJECTED=$(curl -sf "$BASE_URL/api/v1/tasks/$WF_TASK_ID" 2>/dev/null | python3 -c "
import sys,json,urllib.request
d=json.load(sys.stdin)
for e in d.get('events',[]):
    if 'Completed' not in e: continue
    for s in e['Completed'].get('output',{}).get('content',{}).get('step_summaries',[]):
        if s['step_id'] in ('apply_fixes','qa_review'):
            try:
                resp=urllib.request.urlopen(f'http://127.0.0.1:${PORT}/api/v1/tasks/{s[\"task_id\"]}')
                detail=json.loads(resp.read())
                for ev in detail.get('events',[]):
                    if 'Completed' in ev:
                        c=str(ev['Completed'].get('output',{}).get('content',''))
                        if 'CRITICAL' in c and ('reject' in c.lower() or 'escalat' in c.lower()):
                            print('yes'); sys.exit()
            except: pass
print('no')
" 2>/dev/null)

                # [H-1 fix] QA rejection — preserve worktree, skip cleanup
                if [ "$QA_REJECTED" = "yes" ]; then
                    echo "[auto-dev]   ⛔ QA REJECTED (CRITICAL issues) — NOT merging, escalate to human"
                    curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
                        -H "Content-Type: application/json" \
                        -d "{\"status\":\"review\"}" > /dev/null 2>&1 || true
                    echo "[auto-dev]   → Worktree kept at: $WORKTREE_DIR (branch: $BRANCH_NAME)"
                    SKIP_CLEANUP=true
                    ACTIVE_WORKTREE=""
                    ACTIVE_BRANCH=""
                    break
                fi

                # Push branch and create PR if there are changes
                DIFF_COUNT=$(git -C "$SCRIPT_DIR" rev-list --count "main..$BRANCH_NAME" 2>/dev/null || echo "0")
                LANDED=false
                if [ "$DIFF_COUNT" -gt 0 ]; then
                    echo "[auto-dev]   → Pushing $BRANCH_NAME ($DIFF_COUNT commits)"
                    if git -C "$SCRIPT_DIR" push origin "$BRANCH_NAME" 2>&1; then
                        echo "[auto-dev]   → Push successful"

                        PR_BODY=$(printf "## Auto-Dev\n\n**Task:** %s\n**Task ID:** %s\n**Workflow:** %s\n**Commits:** %s\n\n---\nGenerated by Pulse Auto-Dev" \
                            "$TASK_TITLE" "$TASK_ID" "$WORKFLOW" "$DIFF_COUNT")
                        PR_URL=$(cd "$SCRIPT_DIR" && gh pr create \
                            --title "auto-dev: $TASK_TITLE" \
                            --body "$PR_BODY" \
                            --head "$BRANCH_NAME" \
                            --base main 2>&1) || true

                        if echo "$PR_URL" | grep -q 'https://'; then
                            echo "[auto-dev]   → PR created: $PR_URL"
                            LANDED=true
                        else
                            echo "[auto-dev]   ⚠ PR creation failed: $PR_URL"
                            echo "[auto-dev]   → Falling back to local merge"
                            if git -C "$SCRIPT_DIR" merge "$BRANCH_NAME" --no-edit; then
                                LANDED=true
                            else
                                echo "[auto-dev]   ⚠ Merge conflict — branch kept as $BRANCH_NAME"
                                SKIP_CLEANUP=true
                            fi
                        fi
                    else
                        echo "[auto-dev]   ⚠ Push failed — merging locally instead"
                        if git -C "$SCRIPT_DIR" merge "$BRANCH_NAME" --no-edit; then
                            echo "[auto-dev]   → Local merge successful"
                            LANDED=true
                        else
                            echo "[auto-dev]   ⚠ Merge conflict — branch kept as $BRANCH_NAME"
                            SKIP_CLEANUP=true
                        fi
                    fi
                else
                    echo "[auto-dev]   → No changes on $BRANCH_NAME, skipping"
                    LANDED=true
                fi

                # [H-2 fix] Only mark done AFTER code is landed
                if [ "$LANDED" = true ]; then
                    curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
                        -H "Content-Type: application/json" \
                        -d "{\"status\":\"done\"}" > /dev/null 2>&1 || true
                else
                    curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
                        -H "Content-Type: application/json" \
                        -d "{\"status\":\"review\"}" > /dev/null 2>&1 || true
                fi

                # Cleanup worktree unless we need to preserve it
                if [ "$SKIP_CLEANUP" = false ]; then
                    git -C "$SCRIPT_DIR" worktree remove "$WORKTREE_DIR" --force 2>/dev/null || true
                    git -C "$SCRIPT_DIR" branch -D "$BRANCH_NAME" 2>/dev/null || true
                fi
                ACTIVE_WORKTREE=""
                ACTIVE_BRANCH=""

                break
            elif [ "$STATE" = "Failed" ]; then
                echo "[auto-dev]   → Failed after $((i*10))s"
                curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
                    -H "Content-Type: application/json" \
                    -d "{\"status\":\"backlog\"}" > /dev/null 2>&1 || true

                git -C "$SCRIPT_DIR" worktree remove "$WORKTREE_DIR" --force 2>/dev/null || true
                git -C "$SCRIPT_DIR" branch -D "$BRANCH_NAME" 2>/dev/null || true
                ACTIVE_WORKTREE=""
                ACTIVE_BRANCH=""

                break
            fi
        done
    else
        echo "[auto-dev]   → Failed to submit workflow"
        curl -sf -X PATCH "$BASE_URL/api/v1/tasks/$TASK_ID/metadata" \
            -H "Content-Type: application/json" \
            -d "{\"status\":\"backlog\"}" > /dev/null 2>&1 || true

        git -C "$SCRIPT_DIR" worktree remove "$WORKTREE_DIR" --force 2>/dev/null || true
        git -C "$SCRIPT_DIR" branch -D "$BRANCH_NAME" 2>/dev/null || true
        ACTIVE_WORKTREE=""
        ACTIVE_BRANCH=""
    fi

    sleep "$INTERVAL"
done
