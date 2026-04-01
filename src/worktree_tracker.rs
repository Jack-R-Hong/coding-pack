//! Worktree lifecycle management — registry, cleanup, and recovery.

use std::path::Path;

use pulse_plugin_sdk::error::WitPluginError;
use serde::{Deserialize, Serialize};

const REGISTRY_FILENAME: &str = "config/worktree-registry.json";
const ORPHAN_AGE_THRESHOLD_SECS: u64 = 3600; // 1 hour

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeStatus {
    Active,
    Completed,
    Failed,
    Orphaned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub worktree_path: String,
    pub branch_name: String,
    pub task_id: String,
    pub workflow_id: String,
    pub created_at: String, // ISO 8601 UTC
    pub status: WorktreeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRegistry {
    pub entries: Vec<WorktreeEntry>,
}

#[derive(Debug, Clone)]
pub enum CleanupOutcome {
    Removed,
    AlreadyGone,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupDetail {
    pub worktree_path: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupResult {
    pub removed: u32,
    pub skipped: u32,
    pub errors: u32,
    pub details: Vec<CleanupDetail>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryResult {
    pub pruned: bool,
    pub detected_untracked: u32,
    pub marked_orphaned: u32,
    pub cleaned: u32,
    pub skipped_recent: u32,
    pub errors: u32,
    pub details: Vec<CleanupDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitWorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub head: String,
    pub bare: bool,
}

// ---------------------------------------------------------------------------
// Registry I/O
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn load_registry(base_dir: &Path) -> Result<WorktreeRegistry, WitPluginError> {
    let path = base_dir.join(REGISTRY_FILENAME);
    if !path.exists() {
        tracing::debug!(plugin = "coding-pack", "worktree registry not found, returning empty");
        return Ok(WorktreeRegistry {
            entries: Vec::new(),
        });
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| WitPluginError::internal(format!("cannot read worktree registry: {e}")))?;
    let registry: WorktreeRegistry = serde_json::from_str(&data)
        .map_err(|e| WitPluginError::internal(format!("cannot parse worktree registry: {e}")))?;
    tracing::debug!(plugin = "coding-pack", entries = registry.entries.len(), "loaded worktree registry");
    Ok(registry)
}

#[cfg(target_arch = "wasm32")]
pub fn load_registry(_base_dir: &Path) -> Result<WorktreeRegistry, WitPluginError> {
    Err(WitPluginError::internal(
        "load_registry is not available in WASM",
    ))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_registry(
    base_dir: &Path,
    registry: &WorktreeRegistry,
) -> Result<(), WitPluginError> {
    let path = base_dir.join(REGISTRY_FILENAME);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| WitPluginError::internal(format!("cannot create config dir: {e}")))?;
    }
    let json = serde_json::to_string_pretty(registry)
        .map_err(|e| WitPluginError::internal(format!("JSON serialize error: {e}")))?;
    let tmp_path = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp_path)
        .map_err(|e| WitPluginError::internal(format!("cannot create temp file: {e}")))?;
    std::io::Write::write_all(&mut file, json.as_bytes())
        .map_err(|e| WitPluginError::internal(format!("cannot write temp file: {e}")))?;
    file.sync_all()
        .map_err(|e| WitPluginError::internal(format!("cannot sync temp file: {e}")))?;
    drop(file);
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| WitPluginError::internal(format!("cannot rename temp to final: {e}")))?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn save_registry(
    _base_dir: &Path,
    _registry: &WorktreeRegistry,
) -> Result<(), WitPluginError> {
    Err(WitPluginError::internal(
        "save_registry is not available in WASM",
    ))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn register_worktree(
    base_dir: &Path,
    entry: WorktreeEntry,
) -> Result<(), WitPluginError> {
    let mut registry = load_registry(base_dir)?;
    if let Some(existing) = registry
        .entries
        .iter_mut()
        .find(|e| e.worktree_path == entry.worktree_path)
    {
        existing.branch_name = entry.branch_name;
        existing.task_id = entry.task_id.clone();
        existing.workflow_id = entry.workflow_id;
        existing.created_at = entry.created_at;
        existing.status = entry.status;
        tracing::debug!(plugin = "coding-pack", path = %existing.worktree_path, task = %entry.task_id, "updated existing worktree entry");
    } else {
        tracing::debug!(plugin = "coding-pack", path = %entry.worktree_path, task = %entry.task_id, "registered new worktree entry");
        registry.entries.push(entry);
    }
    save_registry(base_dir, &registry)
}

#[cfg(target_arch = "wasm32")]
pub fn register_worktree(
    _base_dir: &Path,
    _entry: WorktreeEntry,
) -> Result<(), WitPluginError> {
    Err(WitPluginError::internal(
        "register_worktree is not available in WASM",
    ))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn update_worktree_status(
    base_dir: &Path,
    worktree_path: &str,
    status: WorktreeStatus,
) -> Result<(), WitPluginError> {
    let mut registry = load_registry(base_dir)?;
    let entry = registry
        .entries
        .iter_mut()
        .find(|e| e.worktree_path == worktree_path)
        .ok_or_else(|| {
            WitPluginError::not_found(format!(
                "worktree entry not found for path: {worktree_path}"
            ))
        })?;
    entry.status = status.clone();
    tracing::debug!(plugin = "coding-pack", path = %worktree_path, status = ?status, "updated worktree status");
    save_registry(base_dir, &registry)
}

#[cfg(target_arch = "wasm32")]
pub fn update_worktree_status(
    _base_dir: &Path,
    _worktree_path: &str,
    _status: WorktreeStatus,
) -> Result<(), WitPluginError> {
    Err(WitPluginError::internal(
        "update_worktree_status is not available in WASM",
    ))
}

// ---------------------------------------------------------------------------
// Cleanup (Story 24.2)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn cleanup_completed_worktrees(
    base_dir: &Path,
) -> Result<CleanupResult, WitPluginError> {
    let mut registry = load_registry(base_dir)?;
    let mut result = CleanupResult {
        removed: 0,
        skipped: 0,
        errors: 0,
        details: Vec::new(),
    };

    let mut indices_to_remove: Vec<usize> = Vec::new();

    for (i, entry) in registry.entries.iter().enumerate() {
        match entry.status {
            WorktreeStatus::Completed => {
                match cleanup_single_worktree(entry, base_dir) {
                    CleanupOutcome::Removed => {
                        result.removed += 1;
                        result.details.push(CleanupDetail {
                            worktree_path: entry.worktree_path.clone(),
                            outcome: "removed".to_string(),
                        });
                        indices_to_remove.push(i);
                    }
                    CleanupOutcome::AlreadyGone => {
                        result.removed += 1;
                        result.details.push(CleanupDetail {
                            worktree_path: entry.worktree_path.clone(),
                            outcome: "already_gone".to_string(),
                        });
                        indices_to_remove.push(i);
                    }
                    CleanupOutcome::Error(msg) => {
                        result.errors += 1;
                        result.details.push(CleanupDetail {
                            worktree_path: entry.worktree_path.clone(),
                            outcome: format!("error: {}", msg),
                        });
                    }
                }
            }
            WorktreeStatus::Active | WorktreeStatus::Failed | WorktreeStatus::Orphaned => {
                result.skipped += 1;
                result.details.push(CleanupDetail {
                    worktree_path: entry.worktree_path.clone(),
                    outcome: format!("skipped ({:?})", entry.status),
                });
            }
        }
    }

    // Remove cleaned entries in reverse order to preserve indices.
    for &i in indices_to_remove.iter().rev() {
        registry.entries.remove(i);
    }

    save_registry(base_dir, &registry)?;

    tracing::debug!(
        plugin = "coding-pack",
        removed = result.removed,
        skipped = result.skipped,
        errors = result.errors,
        "worktree cleanup complete"
    );

    Ok(result)
}

#[cfg(target_arch = "wasm32")]
pub fn cleanup_completed_worktrees(
    _base_dir: &Path,
) -> Result<CleanupResult, WitPluginError> {
    Err(WitPluginError::internal(
        "cleanup_completed_worktrees is not available in WASM",
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn cleanup_single_worktree(entry: &WorktreeEntry, base_dir: &Path) -> CleanupOutcome {
    match remove_git_worktree(&entry.worktree_path, base_dir) {
        Ok(()) => {}
        Err(msg) => {
            // Check if the worktree directory simply does not exist.
            if !std::path::Path::new(&entry.worktree_path).exists() {
                tracing::debug!(
                    plugin = "coding-pack",
                    path = %entry.worktree_path,
                    "worktree directory already gone"
                );
                // Still try to clean the branch.
                let _ = delete_git_branch(&entry.branch_name, base_dir);
                return CleanupOutcome::AlreadyGone;
            }
            return CleanupOutcome::Error(msg);
        }
    }
    // Branch deletion is non-fatal.
    let _ = delete_git_branch(&entry.branch_name, base_dir);
    CleanupOutcome::Removed
}

#[cfg(not(target_arch = "wasm32"))]
fn remove_git_worktree(path: &str, base_dir: &Path) -> Result<(), String> {
    let output = std::process::Command::new("git")
        .current_dir(base_dir)
        .args(["worktree", "remove", "--", path])
        .output()
        .map_err(|e| format!("cannot run git worktree remove: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // If the worktree path doesn't exist, treat as already cleaned.
    if stderr.contains("is not a working tree")
        || stderr.contains("No such file or directory")
        || stderr.contains("is not a valid directory")
    {
        tracing::debug!(plugin = "coding-pack", path = path, "worktree already removed");
        return Ok(());
    }

    Err(format!(
        "git worktree remove failed for '{}': {}",
        path,
        stderr.trim()
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn delete_git_branch(branch: &str, base_dir: &Path) -> Result<(), String> {
    let output = std::process::Command::new("git")
        .current_dir(base_dir)
        .args(["branch", "-d", "--", branch])
        .output()
        .map_err(|e| format!("cannot run git branch -d: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            plugin = "coding-pack",
            branch = branch,
            error = %stderr.trim(),
            "branch deletion failed — non-fatal"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Recovery (Story 24.3)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn recover_orphaned_worktrees(
    base_dir: &Path,
) -> Result<RecoveryResult, WitPluginError> {
    let mut result = RecoveryResult {
        pruned: false,
        detected_untracked: 0,
        marked_orphaned: 0,
        cleaned: 0,
        skipped_recent: 0,
        errors: 0,
        details: Vec::new(),
    };

    // Step 1: Prune stale git worktree metadata.
    run_git_worktree_prune(base_dir)?;
    result.pruned = true;

    // Step 2: Load registry.
    let mut registry = load_registry(base_dir)?;

    // Step 3: Mark Failed entries older than threshold as Orphaned.
    for entry in &mut registry.entries {
        if entry.status == WorktreeStatus::Failed {
            if is_old_enough_for_orphan(&entry.created_at) {
                entry.status = WorktreeStatus::Orphaned;
                result.marked_orphaned += 1;
                tracing::debug!(
                    plugin = "coding-pack",
                    path = %entry.worktree_path,
                    "marked failed worktree as orphaned"
                );
            } else {
                result.skipped_recent += 1;
            }
        }
    }
    if result.marked_orphaned > 0 {
        save_registry(base_dir, &registry)?;
    }

    // Step 4: Detect untracked worktrees with auto-dev/ branch prefix.
    let git_worktrees = list_git_worktrees(base_dir);
    let untracked = match git_worktrees {
        Ok(wts) => detect_untracked_worktrees(&registry, &wts),
        Err(e) => {
            tracing::warn!(
                plugin = "coding-pack",
                error = %e,
                "failed to list git worktrees — skipping untracked detection"
            );
            Vec::new()
        }
    };

    result.detected_untracked = untracked.len() as u32;

    if !untracked.is_empty() {
        tracing::debug!(
            plugin = "coding-pack",
            detected = untracked.len(),
            "detected untracked auto-dev worktrees"
        );
        for ut in &untracked {
            registry.entries.push(ut.clone());
        }
        save_registry(base_dir, &registry)?;
    }

    // Step 5: Clean orphaned entries.
    let mut indices_to_remove: Vec<usize> = Vec::new();

    for (i, entry) in registry.entries.iter().enumerate() {
        if entry.status == WorktreeStatus::Orphaned {
            match cleanup_single_worktree(entry, base_dir) {
                CleanupOutcome::Removed | CleanupOutcome::AlreadyGone => {
                    result.cleaned += 1;
                    result.details.push(CleanupDetail {
                        worktree_path: entry.worktree_path.clone(),
                        outcome: "cleaned orphan".to_string(),
                    });
                    indices_to_remove.push(i);
                }
                CleanupOutcome::Error(msg) => {
                    result.errors += 1;
                    result.details.push(CleanupDetail {
                        worktree_path: entry.worktree_path.clone(),
                        outcome: format!("error: {}", msg),
                    });
                }
            }
        }
    }

    // Remove cleaned entries in reverse order.
    for &i in indices_to_remove.iter().rev() {
        registry.entries.remove(i);
    }

    save_registry(base_dir, &registry)?;

    // Add summary details.
    if result.marked_orphaned > 0 {
        result.details.push(CleanupDetail {
            worktree_path: String::new(),
            outcome: format!("marked {} failed entries as orphaned", result.marked_orphaned),
        });
    }
    if result.detected_untracked > 0 {
        result.details.push(CleanupDetail {
            worktree_path: String::new(),
            outcome: format!("detected {} untracked auto-dev worktrees", result.detected_untracked),
        });
    }

    tracing::debug!(
        plugin = "coding-pack",
        cleaned = result.cleaned,
        errors = result.errors,
        "orphaned worktree recovery complete"
    );

    Ok(result)
}

#[cfg(target_arch = "wasm32")]
pub fn recover_orphaned_worktrees(
    _base_dir: &Path,
) -> Result<RecoveryResult, WitPluginError> {
    Err(WitPluginError::internal(
        "recover_orphaned_worktrees is not available in WASM",
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn run_git_worktree_prune(base_dir: &Path) -> Result<(), WitPluginError> {
    let output = match std::process::Command::new("git")
        .current_dir(base_dir)
        .args(["worktree", "prune"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                plugin = "coding-pack",
                error = %e,
                "git worktree prune spawn failed — non-fatal, skipping"
            );
            return Ok(());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            plugin = "coding-pack",
            error = %stderr.trim(),
            "git worktree prune failed — non-fatal"
        );
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn list_git_worktrees(base_dir: &Path) -> Result<Vec<GitWorktreeInfo>, WitPluginError> {
    let output = std::process::Command::new("git")
        .current_dir(base_dir)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| WitPluginError::internal(format!("cannot run git worktree list: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WitPluginError::internal(format!(
            "git worktree list failed: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_porcelain_worktree_list(&stdout))
}

#[cfg(target_arch = "wasm32")]
pub fn list_git_worktrees(_base_dir: &Path) -> Result<Vec<GitWorktreeInfo>, WitPluginError> {
    Err(WitPluginError::internal(
        "list_git_worktrees is not available in WASM",
    ))
}

/// Parse the output of `git worktree list --porcelain` into structured data.
fn parse_porcelain_worktree_list(output: &str) -> Vec<GitWorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut current_path = String::new();
    let mut current_head = String::new();
    let mut current_branch: Option<String> = None;
    let mut current_bare = false;

    for line in output.lines() {
        if line.is_empty() {
            if !current_path.is_empty() {
                worktrees.push(GitWorktreeInfo {
                    path: std::mem::take(&mut current_path),
                    head: std::mem::take(&mut current_head),
                    branch: current_branch.take(),
                    bare: current_bare,
                });
                current_bare = false;
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = path.to_string();
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = head.to_string();
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            // Strip "refs/heads/" prefix to get the branch name.
            let branch_name = branch_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(branch_ref);
            current_branch = Some(branch_name.to_string());
        } else if line == "bare" {
            current_bare = true;
        }
        // "detached" line means HEAD is detached — current_branch stays None.
    }
    // Handle last block (no trailing blank line).
    if !current_path.is_empty() {
        worktrees.push(GitWorktreeInfo {
            path: current_path,
            head: current_head,
            branch: current_branch,
            bare: current_bare,
        });
    }

    worktrees
}

fn detect_untracked_worktrees(
    registry: &WorktreeRegistry,
    git_worktrees: &[GitWorktreeInfo],
) -> Vec<WorktreeEntry> {
    let mut untracked = Vec::new();

    for wt in git_worktrees {
        let branch = match &wt.branch {
            Some(b) => b,
            None => continue, // detached HEAD or bare — skip
        };

        // Only care about auto-dev/ prefixed branches.
        if !branch.starts_with("auto-dev/") {
            continue;
        }

        // Check if this worktree is already tracked in the registry.
        let already_tracked = registry
            .entries
            .iter()
            .any(|e| e.worktree_path == wt.path);

        if already_tracked {
            continue;
        }

        let (workflow_id, task_id) = extract_ids_from_branch(branch);

        untracked.push(WorktreeEntry {
            worktree_path: wt.path.clone(),
            branch_name: branch.clone(),
            task_id,
            workflow_id,
            created_at: now_iso8601(),
            status: WorktreeStatus::Orphaned,
        });
    }

    untracked
}

/// Extract workflow_id and task_id from branch name `auto-dev/{workflow_id}/{task_id}`.
fn extract_ids_from_branch(branch: &str) -> (String, String) {
    if let Some(stripped) = branch.strip_prefix("auto-dev/") {
        if let Some(slash_pos) = stripped.find('/') {
            let workflow_id = stripped[..slash_pos].to_string();
            let task_id = stripped[slash_pos + 1..].to_string();
            if !workflow_id.is_empty() && !task_id.is_empty() {
                return (workflow_id, task_id);
            }
        }
    }
    ("unknown".to_string(), "unknown".to_string())
}

// ---------------------------------------------------------------------------
// Status reporting (Story 24.3)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub fn worktree_status(
    base_dir: &Path,
) -> Result<serde_json::Value, WitPluginError> {
    let registry = load_registry(base_dir)?;

    let mut by_status = std::collections::BTreeMap::<String, u32>::new();
    let mut worktrees = Vec::new();

    for entry in &registry.entries {
        let status_str = serde_json::to_value(&entry.status)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("{:?}", entry.status).to_lowercase());
        *by_status.entry(status_str.clone()).or_insert(0) += 1;

        let age = format_age(&entry.created_at);
        worktrees.push(serde_json::json!({
            "path": entry.worktree_path,
            "branch": entry.branch_name,
            "task_id": entry.task_id,
            "workflow_id": entry.workflow_id,
            "status": status_str,
            "age": age,
            "created_at": entry.created_at,
        }));
    }

    Ok(serde_json::json!({
        "worktrees": worktrees,
        "total": registry.entries.len(),
        "by_status": by_status,
    }))
}

#[cfg(target_arch = "wasm32")]
pub fn worktree_status(
    _base_dir: &Path,
) -> Result<serde_json::Value, WitPluginError> {
    Err(WitPluginError::internal(
        "worktree_status is not available in WASM",
    ))
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn format_age(created_at: &str) -> String {
    let now = chrono::Utc::now();
    let created = match parse_iso8601(created_at) {
        Some(dt) => dt,
        None => return "unknown".to_string(),
    };

    let diff = now.signed_duration_since(created);
    let total_secs = diff.num_seconds();
    if total_secs < 0 {
        return "0m".to_string();
    }

    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;

    if days > 0 {
        format!("{}d {}h", days, hours)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

fn parse_iso8601(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // Try RFC 3339 first (covers ISO 8601 with timezone).
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // Try basic ISO 8601 format without timezone offset (assume UTC).
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(dt.and_utc());
    }
    None
}

fn parse_iso8601_to_epoch(s: &str) -> Option<i64> {
    parse_iso8601(s).map(|dt| dt.timestamp())
}

fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn is_old_enough_for_orphan(created_at: &str) -> bool {
    let now_secs = chrono::Utc::now().timestamp();
    match parse_iso8601_to_epoch(created_at) {
        Some(created_secs) => {
            let diff = now_secs.saturating_sub(created_secs);
            diff as u64 > ORPHAN_AGE_THRESHOLD_SECS
        }
        None => true, // If we can't parse the timestamp, treat as old.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Serialization roundtrip tests --

    #[test]
    fn test_worktree_status_serialization_roundtrip() {
        let statuses = [
            (WorktreeStatus::Active, "\"active\""),
            (WorktreeStatus::Completed, "\"completed\""),
            (WorktreeStatus::Failed, "\"failed\""),
            (WorktreeStatus::Orphaned, "\"orphaned\""),
        ];
        for (status, expected_json) in &statuses {
            let json = serde_json::to_string(status).expect("serialize");
            assert_eq!(&json, expected_json, "status {:?} should serialize to {}", status, expected_json);

            let roundtripped: WorktreeStatus =
                serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&roundtripped, status);
        }
    }

    #[test]
    fn test_worktree_entry_serialization_roundtrip() {
        let entry = WorktreeEntry {
            worktree_path: "/tmp/wt/test".to_string(),
            branch_name: "auto-dev/coding-story-dev/task-1".to_string(),
            task_id: "task-1".to_string(),
            workflow_id: "coding-story-dev".to_string(),
            created_at: "2026-03-27T14:30:00Z".to_string(),
            status: WorktreeStatus::Active,
        };

        let json = serde_json::to_string_pretty(&entry).expect("serialize");
        let roundtripped: WorktreeEntry = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(roundtripped.worktree_path, entry.worktree_path);
        assert_eq!(roundtripped.branch_name, entry.branch_name);
        assert_eq!(roundtripped.task_id, entry.task_id);
        assert_eq!(roundtripped.workflow_id, entry.workflow_id);
        assert_eq!(roundtripped.created_at, entry.created_at);
        assert_eq!(roundtripped.status, entry.status);
    }

    // -- Registry I/O tests --

    #[test]
    fn test_load_registry_missing_file_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = load_registry(dir.path()).expect("load");
        assert!(registry.entries.is_empty());
    }

    #[test]
    fn test_save_and_load_registry_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");

        let registry = WorktreeRegistry {
            entries: vec![
                WorktreeEntry {
                    worktree_path: "/tmp/wt/a".to_string(),
                    branch_name: "auto-dev/wf/task-a".to_string(),
                    task_id: "task-a".to_string(),
                    workflow_id: "wf".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    status: WorktreeStatus::Active,
                },
                WorktreeEntry {
                    worktree_path: "/tmp/wt/b".to_string(),
                    branch_name: "auto-dev/wf/task-b".to_string(),
                    task_id: "task-b".to_string(),
                    workflow_id: "wf".to_string(),
                    created_at: "2026-01-01T01:00:00Z".to_string(),
                    status: WorktreeStatus::Completed,
                },
            ],
        };

        save_registry(dir.path(), &registry).expect("save");
        let loaded = load_registry(dir.path()).expect("load");

        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].task_id, "task-a");
        assert_eq!(loaded.entries[1].task_id, "task-b");
        assert_eq!(loaded.entries[0].status, WorktreeStatus::Active);
        assert_eq!(loaded.entries[1].status, WorktreeStatus::Completed);

        // Verify no .tmp file remains.
        let tmp_path = dir.path().join("config/worktree-registry.json.tmp");
        assert!(!tmp_path.exists(), "temp file should not remain after save");
    }

    // -- Registration tests --

    #[test]
    fn test_register_worktree_adds_new_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = WorktreeEntry {
            worktree_path: "/tmp/wt/new".to_string(),
            branch_name: "auto-dev/wf/task-new".to_string(),
            task_id: "task-new".to_string(),
            workflow_id: "wf".to_string(),
            created_at: now_iso8601(),
            status: WorktreeStatus::Active,
        };

        register_worktree(dir.path(), entry).expect("register");

        let registry = load_registry(dir.path()).expect("load");
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(registry.entries[0].task_id, "task-new");
        assert_eq!(registry.entries[0].status, WorktreeStatus::Active);
    }

    #[test]
    fn test_register_worktree_replaces_existing_with_same_path() {
        let dir = tempfile::tempdir().expect("tempdir");

        let entry1 = WorktreeEntry {
            worktree_path: "/tmp/wt/same".to_string(),
            branch_name: "auto-dev/wf/task-1".to_string(),
            task_id: "task-1".to_string(),
            workflow_id: "wf".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            status: WorktreeStatus::Active,
        };
        register_worktree(dir.path(), entry1).expect("register first");

        let entry2 = WorktreeEntry {
            worktree_path: "/tmp/wt/same".to_string(),
            branch_name: "auto-dev/wf/task-2".to_string(),
            task_id: "task-2".to_string(),
            workflow_id: "wf2".to_string(),
            created_at: "2026-02-01T00:00:00Z".to_string(),
            status: WorktreeStatus::Active,
        };
        register_worktree(dir.path(), entry2).expect("register second");

        let registry = load_registry(dir.path()).expect("load");
        assert_eq!(registry.entries.len(), 1, "should not duplicate");
        assert_eq!(registry.entries[0].task_id, "task-2");
        assert_eq!(registry.entries[0].workflow_id, "wf2");
    }

    // -- Status update tests --

    #[test]
    fn test_update_worktree_status_changes_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = WorktreeEntry {
            worktree_path: "/tmp/wt/upd".to_string(),
            branch_name: "auto-dev/wf/task-upd".to_string(),
            task_id: "task-upd".to_string(),
            workflow_id: "wf".to_string(),
            created_at: now_iso8601(),
            status: WorktreeStatus::Active,
        };
        register_worktree(dir.path(), entry).expect("register");

        update_worktree_status(dir.path(), "/tmp/wt/upd", WorktreeStatus::Completed)
            .expect("update");

        let registry = load_registry(dir.path()).expect("load");
        assert_eq!(registry.entries[0].status, WorktreeStatus::Completed);
    }

    #[test]
    fn test_update_worktree_status_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result =
            update_worktree_status(dir.path(), "/nonexistent", WorktreeStatus::Completed);
        assert!(result.is_err());
    }

    // -- Utility tests --

    #[test]
    fn test_format_age_produces_readable_output() {
        // Use a timestamp far enough in the past to produce stable output.
        // 2020-01-01T00:00:00Z is definitely more than a day ago.
        let age = format_age("2020-01-01T00:00:00Z");
        assert!(
            age.contains('d'),
            "expected days in age for old timestamp, got: {}",
            age
        );
    }

    #[test]
    fn test_format_age_invalid_timestamp() {
        let age = format_age("not-a-timestamp");
        assert_eq!(age, "unknown");
    }

    #[test]
    fn test_parse_iso8601_to_epoch_valid() {
        let epoch = parse_iso8601_to_epoch("2026-03-27T14:30:00Z");
        assert!(epoch.is_some());
        // 2026-03-27T14:30:00Z is well after unix epoch.
        assert!(epoch.unwrap() > 0);
    }

    #[test]
    fn test_parse_iso8601_to_epoch_invalid() {
        assert!(parse_iso8601_to_epoch("garbage").is_none());
    }

    #[test]
    fn test_now_iso8601_format() {
        let ts = now_iso8601();
        // Should match pattern YYYY-MM-DDTHH:MM:SSZ
        assert!(ts.ends_with('Z'), "should end with Z: {}", ts);
        assert_eq!(ts.len(), 20, "should be 20 chars: {}", ts);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
    }

    // -- Porcelain parsing tests --

    #[test]
    fn test_parse_porcelain_worktree_list() {
        let output = "\
worktree /home/user/project
HEAD abc123def456
branch refs/heads/main

worktree /home/user/project/.worktrees/auto-dev/coding-story-dev/task-42
HEAD def456abc789
branch refs/heads/auto-dev/coding-story-dev/task-42

worktree /home/user/project/.worktrees/feature/my-thing
HEAD 789abc123def
branch refs/heads/feature/my-thing
";

        let worktrees = parse_porcelain_worktree_list(output);
        assert_eq!(worktrees.len(), 3);

        assert_eq!(worktrees[0].path, "/home/user/project");
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(worktrees[0].head, "abc123def456");
        assert!(!worktrees[0].bare);

        assert_eq!(
            worktrees[1].path,
            "/home/user/project/.worktrees/auto-dev/coding-story-dev/task-42"
        );
        assert_eq!(
            worktrees[1].branch.as_deref(),
            Some("auto-dev/coding-story-dev/task-42")
        );

        assert_eq!(
            worktrees[2].path,
            "/home/user/project/.worktrees/feature/my-thing"
        );
        assert_eq!(worktrees[2].branch.as_deref(), Some("feature/my-thing"));
    }

    #[test]
    fn test_parse_porcelain_worktree_list_no_trailing_newline() {
        let output = "\
worktree /home/user/project
HEAD abc123
branch refs/heads/main";

        let worktrees = parse_porcelain_worktree_list(output);
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, "/home/user/project");
    }

    #[test]
    fn test_parse_porcelain_worktree_list_bare() {
        let output = "\
worktree /home/user/bare-repo
HEAD abc123
bare
";

        let worktrees = parse_porcelain_worktree_list(output);
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].bare);
        assert!(worktrees[0].branch.is_none());
    }

    #[test]
    fn test_parse_porcelain_worktree_list_detached() {
        let output = "\
worktree /home/user/project/.worktrees/detached-wt
HEAD abc123
detached
";

        let worktrees = parse_porcelain_worktree_list(output);
        assert_eq!(worktrees.len(), 1);
        assert!(worktrees[0].branch.is_none());
        assert!(!worktrees[0].bare);
    }

    // -- CleanupResult default values --

    #[test]
    fn test_cleanup_result_default_values() {
        let result = CleanupResult {
            removed: 0,
            skipped: 0,
            errors: 0,
            details: Vec::new(),
        };
        assert_eq!(result.removed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.errors, 0);
        assert!(result.details.is_empty());
    }

    // -- detect_untracked_worktrees tests --

    #[test]
    fn test_detect_untracked_finds_autodev_not_in_registry() {
        let registry = WorktreeRegistry {
            entries: vec![],
        };

        let git_worktrees = vec![
            GitWorktreeInfo {
                path: "/home/user/project".to_string(),
                branch: Some("main".to_string()),
                head: "abc".to_string(),
                bare: false,
            },
            GitWorktreeInfo {
                path: "/home/user/project/.worktrees/auto-dev/wf/task-x".to_string(),
                branch: Some("auto-dev/wf/task-x".to_string()),
                head: "def".to_string(),
                bare: false,
            },
            GitWorktreeInfo {
                path: "/home/user/project/.worktrees/feature/foo".to_string(),
                branch: Some("feature/foo".to_string()),
                head: "ghi".to_string(),
                bare: false,
            },
        ];

        let untracked = detect_untracked_worktrees(&registry, &git_worktrees);

        // Only the auto-dev/ one should be detected.
        assert_eq!(untracked.len(), 1);
        assert_eq!(
            untracked[0].worktree_path,
            "/home/user/project/.worktrees/auto-dev/wf/task-x"
        );
        assert_eq!(untracked[0].branch_name, "auto-dev/wf/task-x");
        assert_eq!(untracked[0].task_id, "task-x");
        assert_eq!(untracked[0].workflow_id, "wf");
        assert_eq!(untracked[0].status, WorktreeStatus::Orphaned);
    }

    #[test]
    fn test_detect_untracked_skips_already_tracked() {
        let registry = WorktreeRegistry {
            entries: vec![WorktreeEntry {
                worktree_path: "/home/user/project/.worktrees/auto-dev/wf/task-x"
                    .to_string(),
                branch_name: "auto-dev/wf/task-x".to_string(),
                task_id: "task-x".to_string(),
                workflow_id: "wf".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                status: WorktreeStatus::Active,
            }],
        };

        let git_worktrees = vec![GitWorktreeInfo {
            path: "/home/user/project/.worktrees/auto-dev/wf/task-x".to_string(),
            branch: Some("auto-dev/wf/task-x".to_string()),
            head: "def".to_string(),
            bare: false,
        }];

        let untracked = detect_untracked_worktrees(&registry, &git_worktrees);
        assert!(untracked.is_empty());
    }

    // -- extract_ids_from_branch tests --

    #[test]
    fn test_extract_ids_from_branch_valid() {
        let (wf, task) = extract_ids_from_branch("auto-dev/coding-story-dev/task-42");
        assert_eq!(wf, "coding-story-dev");
        assert_eq!(task, "task-42");
    }

    #[test]
    fn test_extract_ids_from_branch_no_prefix() {
        let (wf, task) = extract_ids_from_branch("feature/something");
        assert_eq!(wf, "unknown");
        assert_eq!(task, "unknown");
    }

    #[test]
    fn test_extract_ids_from_branch_no_task() {
        let (wf, task) = extract_ids_from_branch("auto-dev/wf-only");
        // No second slash, so no task_id split.
        assert_eq!(wf, "unknown");
        assert_eq!(task, "unknown");
    }

    // -- is_old_enough_for_orphan tests --

    #[test]
    fn test_is_old_enough_for_orphan_old() {
        // Timestamp from 2020 is definitely > 1 hour ago.
        assert!(is_old_enough_for_orphan("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn test_is_old_enough_for_orphan_recent() {
        // Use current time — should not be old enough.
        let ts = now_iso8601();
        assert!(!is_old_enough_for_orphan(&ts));
    }

    #[test]
    fn test_is_old_enough_for_orphan_unparseable() {
        // Unparseable timestamp should be treated as old.
        assert!(is_old_enough_for_orphan("garbage"));
    }

    // -- worktree_status tests --

    #[test]
    fn test_worktree_status_returns_json_structure() {
        let dir = tempfile::tempdir().expect("tempdir");

        let entry1 = WorktreeEntry {
            worktree_path: "/tmp/wt/s1".to_string(),
            branch_name: "auto-dev/wf/task-s1".to_string(),
            task_id: "task-s1".to_string(),
            workflow_id: "wf".to_string(),
            created_at: now_iso8601(),
            status: WorktreeStatus::Active,
        };
        let entry2 = WorktreeEntry {
            worktree_path: "/tmp/wt/s2".to_string(),
            branch_name: "auto-dev/wf/task-s2".to_string(),
            task_id: "task-s2".to_string(),
            workflow_id: "wf".to_string(),
            created_at: "2020-01-01T00:00:00Z".to_string(),
            status: WorktreeStatus::Failed,
        };
        register_worktree(dir.path(), entry1).expect("register");
        register_worktree(dir.path(), entry2).expect("register");

        let status = worktree_status(dir.path()).expect("status");

        assert!(status.get("worktrees").is_some());
        assert!(status.get("total").is_some());
        assert!(status.get("by_status").is_some());

        let total = status["total"].as_u64().expect("total is u64");
        assert_eq!(total, 2);

        let by_status = status["by_status"].as_object().expect("by_status is object");
        assert_eq!(by_status.get("active").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(by_status.get("failed").and_then(|v| v.as_u64()), Some(1));

        let worktrees = status["worktrees"].as_array().expect("worktrees is array");
        assert_eq!(worktrees.len(), 2);

        // Verify each worktree has the expected fields.
        for wt in worktrees {
            assert!(wt.get("path").is_some());
            assert!(wt.get("branch").is_some());
            assert!(wt.get("task_id").is_some());
            assert!(wt.get("workflow_id").is_some());
            assert!(wt.get("status").is_some());
            assert!(wt.get("age").is_some());
            assert!(wt.get("created_at").is_some());
        }
    }
}
