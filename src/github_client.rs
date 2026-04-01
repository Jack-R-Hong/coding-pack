//! GitHub REST API client for issue operations (Epic 22) and PR review
//! operations (Epic 23).
//!
//! Provides `GitHubClient` — a struct-based client that authenticates via
//! `GITHUB_TOKEN` env var and parses `owner/repo` from the git remote.
//! All HTTP methods use `reqwest::blocking` and are gated behind
//! `#[cfg(not(target_arch = "wasm32"))]`.

use pulse_plugin_sdk::error::WitPluginError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Helper ────────────────────────────────────────────────────────────────────

fn github_err(msg: impl std::fmt::Display) -> WitPluginError {
    WitPluginError::internal(format!("GitHub API error: {msg}"))
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// GitHub REST API client.  Custom `Debug` redacts the token.
#[derive(Clone)]
pub struct GitHubClient {
    token: String,
    pub owner: String,
    pub repo: String,
    api_base: String,
    #[cfg(not(target_arch = "wasm32"))]
    client: reqwest::blocking::Client,
}

impl std::fmt::Debug for GitHubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubClient")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("api_base", &self.api_base)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<GitHubLabel>,
    pub milestone: Option<GitHubMilestone>,
    pub html_url: String,
    #[serde(default)]
    pub state: String,
    /// Present on pull requests returned by the issues endpoint; absent on
    /// true issues.  Used to filter PRs out of issue listings.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubLabel {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubMilestone {
    pub title: String,
    pub number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReview {
    pub id: u64,
    pub state: String,
    pub body: Option<String>,
    pub user: GitHubUser,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewComment {
    pub id: u64,
    pub path: String,
    pub line: Option<u64>,
    pub body: String,
    pub diff_hunk: String,
    pub user: GitHubUser,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub head: PrRef,
    pub base: PrRef,
    pub html_url: String,
    pub user: GitHubUser,
    pub body: Option<String>,
    #[serde(default)]
    pub requested_reviewers: Vec<GitHubUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrRef {
    #[serde(rename = "ref")]
    pub ref_field: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

/// Structured fix context assembled from PR reviews and inline comments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixContext {
    pub pr_number: u64,
    pub branch: String,
    pub base_branch: String,
    pub html_url: String,
    pub review_summary: String,
    pub file_comments: Vec<FileCommentGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCommentGroup {
    pub file_path: String,
    pub comments: Vec<InlineComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineComment {
    pub line_number: Option<u64>,
    pub diff_hunk: String,
    pub reviewer_comment: String,
    pub reviewer: String,
}

// ── GitHubClient implementation (native only) ─────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
impl GitHubClient {
    /// Create a new client.  Reads `GITHUB_TOKEN` from the environment and
    /// parses `owner/repo` from `git remote get-url origin`.
    pub fn new() -> Result<Self, WitPluginError> {
        let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
            WitPluginError::invalid_input("GITHUB_TOKEN environment variable not set")
        })?;

        let api_base = std::env::var("GITHUB_API_URL")
            .unwrap_or_else(|_| "https://api.github.com".to_string());

        let (owner, repo) = parse_git_remote()?;

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .user_agent("pulse-auto-dev")
            .build()
            .map_err(|e| github_err(format!("build HTTP client: {e}")))?;

        tracing::info!(
            plugin = "coding-pack",
            "initializing GitHub client for {}/{}",
            owner,
            repo
        );

        Ok(Self {
            token,
            owner,
            repo,
            api_base,
            client,
        })
    }

    /// List issues, optionally filtered by state, labels, and milestone.
    /// Pull requests are filtered out.  Paginates up to 1000 results.
    pub fn list_issues(
        &self,
        state: &str,
        labels: &[String],
        milestone: Option<&str>,
    ) -> Result<Vec<GitHubIssue>, WitPluginError> {
        let base_url = format!(
            "{}/repos/{}/{}/issues",
            self.api_base, self.owner, self.repo
        );
        let mut params: Vec<(&str, String)> = vec![
            ("state", state.to_string()),
            ("per_page", "100".to_string()),
        ];
        if !labels.is_empty() {
            params.push(("labels", labels.join(",")));
        }
        if let Some(ms) = milestone {
            params.push(("milestone", ms.to_string()));
        }

        let mut all_issues: Vec<GitHubIssue> = Vec::new();
        // Build the initial URL with properly encoded query params
        let initial_url = reqwest::blocking::Client::new()
            .get(&base_url)
            .query(&params)
            .build()
            .map_err(|e| github_err(format!("build URL: {e}")))?
            .url()
            .to_string();
        let mut next_url: Option<String> = Some(initial_url);
        let mut page_num = 0u32;

        while let Some(current_url) = next_url {
            page_num += 1;
            if page_num > 10 {
                tracing::warn!(
                    plugin = "coding-pack",
                    "reached max 10 pages for issue listing, stopping"
                );
                break;
            }

            let resp = self
                .client
                .get(&current_url)
                .bearer_auth(&self.token)
                .header("User-Agent", "pulse-coding-pack")
                .header("Accept", "application/vnd.github+json")
                .send()
                .map_err(|e| github_err(format!("GET {current_url}: {e}")))?;

            let link_header = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .map(String::from);

            let status = resp.status();
            let body = resp
                .text()
                .map_err(|e| github_err(format!("read body: {e}")))?;
            if !status.is_success() {
                return Err(WitPluginError::internal(format!(
                    "GitHub API error (HTTP {}): {}",
                    status.as_u16(),
                    &body[..body.len().min(200)]
                )));
            }
            let page: Vec<GitHubIssue> = serde_json::from_str(&body)
                .map_err(|e| github_err(format!("parse issues: {e}")))?;

            tracing::debug!(
                plugin = "coding-pack",
                page = page_num,
                count = page.len(),
                "fetched issues page"
            );

            // Filter out pull requests (GitHub issues API includes them)
            let issues_only: Vec<GitHubIssue> = page
                .into_iter()
                .filter(|i| i.pull_request.is_none())
                .collect();
            all_issues.extend(issues_only);

            next_url = link_header.as_deref().and_then(parse_link_header);
        }

        Ok(all_issues)
    }

    /// List reviews for a pull request with pagination.
    pub fn list_pr_reviews(&self, pr_number: u64) -> Result<Vec<PrReview>, WitPluginError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/reviews?per_page=100",
            self.api_base, self.owner, self.repo, pr_number
        );
        self.paginated_get::<PrReview>(&url)
    }

    /// Get inline review comments for a pull request with pagination.
    pub fn get_review_comments(
        &self,
        pr_number: u64,
    ) -> Result<Vec<PrReviewComment>, WitPluginError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/comments?per_page=100",
            self.api_base, self.owner, self.repo, pr_number
        );
        self.paginated_get::<PrReviewComment>(&url)
    }

    /// List open pull requests with pagination.
    pub fn list_open_prs(&self) -> Result<Vec<PullRequest>, WitPluginError> {
        let url = format!(
            "{}/repos/{}/{}/pulls?state=open&per_page=100",
            self.api_base, self.owner, self.repo
        );
        self.paginated_get::<PullRequest>(&url)
    }

    /// Get a single pull request by number.
    pub fn get_pull_request(&self, pr_number: u64) -> Result<PullRequest, WitPluginError> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.api_base, self.owner, self.repo, pr_number
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .map_err(|e| github_err(format!("GET {url}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .map_err(|e| github_err(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(WitPluginError::internal(format!(
                "GitHub API error (HTTP {}): {}",
                status.as_u16(),
                &body[..body.len().min(200)]
            )));
        }
        serde_json::from_str(&body).map_err(|e| github_err(format!("parse pull request: {e}")))
    }

    /// Build a structured fix context by combining reviews and inline comments
    /// for a given PR.  Groups comments by file path using `BTreeMap` for
    /// deterministic ordering, and sorts within each file by line number.
    pub fn build_fix_context(&self, pr_number: u64) -> Result<FixContext, WitPluginError> {
        let pr = self.get_pull_request(pr_number)?;
        let reviews = self.list_pr_reviews(pr_number)?;
        let comments = self.get_review_comments(pr_number)?;

        // Build review_summary from CHANGES_REQUESTED reviews
        let review_summary = reviews
            .iter()
            .filter(|r| r.state == "CHANGES_REQUESTED")
            .filter_map(|r| {
                r.body
                    .as_ref()
                    .filter(|b| !b.is_empty())
                    .map(|body| format!("Reviewer: {}\n{}", r.user.login, body))
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        // Group inline comments by file path (BTreeMap for deterministic order)
        let mut by_file: BTreeMap<String, Vec<InlineComment>> = BTreeMap::new();

        for c in &comments {
            by_file
                .entry(c.path.clone())
                .or_default()
                .push(InlineComment {
                    line_number: c.line,
                    diff_hunk: c.diff_hunk.clone(),
                    reviewer_comment: c.body.clone(),
                    reviewer: c.user.login.clone(),
                });
        }

        // Sort comments within each file by line number (None sorts first as 0)
        let file_comments: Vec<FileCommentGroup> = by_file
            .into_iter()
            .map(|(file_path, mut comments)| {
                comments.sort_by_key(|c| c.line_number.unwrap_or(0));
                FileCommentGroup {
                    file_path,
                    comments,
                }
            })
            .collect();

        Ok(FixContext {
            pr_number,
            branch: pr.head.ref_field.clone(),
            base_branch: pr.base.ref_field.clone(),
            html_url: pr.html_url.clone(),
            review_summary,
            file_comments,
        })
    }

    /// Request reviewers for a pull request.
    pub fn request_reviewers(
        &self,
        pr_number: u64,
        reviewers: &[String],
    ) -> Result<(), WitPluginError> {
        if reviewers.is_empty() {
            return Ok(());
        }
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/requested_reviewers",
            self.api_base, self.owner, self.repo, pr_number
        );
        let body = serde_json::json!({ "reviewers": reviewers });
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .map_err(|e| github_err(format!("POST {url}: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(github_err(format!("POST {url} returned {status}: {text}")));
        }
        Ok(())
    }

    /// Generic paginated GET helper.  Follows `Link: rel="next"` headers,
    /// capping at 10 pages.
    fn paginated_get<T: serde::de::DeserializeOwned>(
        &self,
        initial_url: &str,
    ) -> Result<Vec<T>, WitPluginError> {
        let mut all: Vec<T> = Vec::new();
        let mut next_url: Option<String> = Some(initial_url.to_string());
        let mut page_num = 0u32;

        while let Some(current_url) = next_url {
            page_num += 1;
            if page_num > 10 {
                tracing::warn!(
                    plugin = "coding-pack",
                    "reached max 10 pages for paginated GET, stopping"
                );
                break;
            }

            let resp = self
                .client
                .get(&current_url)
                .bearer_auth(&self.token)
                .send()
                .map_err(|e| github_err(format!("GET {current_url}: {e}")))?;

            let link_header = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .map(String::from);

            let status = resp.status();
            let body = resp
                .text()
                .map_err(|e| github_err(format!("read body: {e}")))?;
            if !status.is_success() {
                return Err(WitPluginError::internal(format!(
                    "GitHub API error (HTTP {}): {}",
                    status.as_u16(),
                    &body[..body.len().min(200)]
                )));
            }
            let page: Vec<T> =
                serde_json::from_str(&body).map_err(|e| github_err(format!("parse: {e}")))?;

            all.extend(page);
            next_url = link_header.as_deref().and_then(parse_link_header);
        }

        Ok(all)
    }
}

// ── WASM stubs ────────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
impl GitHubClient {
    pub fn new() -> Result<Self, WitPluginError> {
        Err(WitPluginError::internal(
            "GitHubClient is not available in WASM builds",
        ))
    }

    pub fn list_issues(
        &self,
        _state: &str,
        _labels: &[String],
        _milestone: Option<&str>,
    ) -> Result<Vec<GitHubIssue>, WitPluginError> {
        Err(WitPluginError::internal(
            "list_issues is not available in WASM builds",
        ))
    }

    pub fn list_pr_reviews(&self, _pr_number: u64) -> Result<Vec<PrReview>, WitPluginError> {
        Err(WitPluginError::internal(
            "list_pr_reviews is not available in WASM builds",
        ))
    }

    pub fn get_review_comments(
        &self,
        _pr_number: u64,
    ) -> Result<Vec<PrReviewComment>, WitPluginError> {
        Err(WitPluginError::internal(
            "get_review_comments is not available in WASM builds",
        ))
    }

    pub fn list_open_prs(&self) -> Result<Vec<PullRequest>, WitPluginError> {
        Err(WitPluginError::internal(
            "list_open_prs is not available in WASM builds",
        ))
    }

    pub fn get_pull_request(&self, _pr_number: u64) -> Result<PullRequest, WitPluginError> {
        Err(WitPluginError::internal(
            "get_pull_request is not available in WASM builds",
        ))
    }

    pub fn build_fix_context(&self, _pr_number: u64) -> Result<FixContext, WitPluginError> {
        Err(WitPluginError::internal(
            "build_fix_context is not available in WASM builds",
        ))
    }

    pub fn request_reviewers(
        &self,
        _pr_number: u64,
        _reviewers: &[String],
    ) -> Result<(), WitPluginError> {
        Err(WitPluginError::internal(
            "request_reviewers is not available in WASM builds",
        ))
    }
}

// ── Free functions ────────────────────────────────────────────────────────────

/// Aggregate review state across all reviews for a PR.
///
/// Groups reviews by user and keeps only the latest per user (by position in
/// the list, which GitHub returns chronologically).  Only `APPROVED` and
/// `CHANGES_REQUESTED` states are considered.
///
/// Returns:
/// - `"approved"` if at least one reviewer's latest state is APPROVED and none
///   have CHANGES_REQUESTED
/// - `"changes_requested"` if any reviewer's latest state is CHANGES_REQUESTED
/// - `"pending"` otherwise (no decisive reviews)
pub fn aggregate_review_state(reviews: &[PrReview]) -> String {
    let mut latest_by_user: BTreeMap<&str, &str> = BTreeMap::new();
    for review in reviews {
        match review.state.as_str() {
            "APPROVED" | "CHANGES_REQUESTED" => {
                latest_by_user.insert(&review.user.login, &review.state);
            }
            _ => {}
        }
    }

    if latest_by_user.is_empty() {
        return "pending".to_string();
    }
    if latest_by_user.values().any(|s| *s == "CHANGES_REQUESTED") {
        return "changes_requested".to_string();
    }
    "approved".to_string()
}

/// Check whether a PR was created by the auto-dev loop.
///
/// Returns `true` if the branch name starts with `auto-dev/` OR the PR body
/// contains `Co-authored-by: pulse-auto-dev`.
pub fn is_auto_dev_pr(pr: &PullRequest) -> bool {
    let branch_match = pr.head.ref_field.starts_with("auto-dev/");
    let body_match = pr
        .body
        .as_deref()
        .map(|b| b.contains("Co-authored-by: pulse-auto-dev"))
        .unwrap_or(false);
    branch_match || body_match
}

/// Parse `owner` and `repo` from `git remote get-url origin`.
///
/// Supports both SSH (`git@github.com:owner/repo.git`) and HTTPS
/// (`https://github.com/owner/repo.git`) formats.
#[cfg(not(target_arch = "wasm32"))]
fn parse_git_remote() -> Result<(String, String), WitPluginError> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| github_err(format!("failed to run git: {e}")))?;

    if !output.status.success() {
        return Err(github_err("git remote get-url origin failed"));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_remote_url(&url)
}

/// Parse owner/repo from a remote URL string.
/// Extracted for testability without running git.
fn parse_remote_url(url: &str) -> Result<(String, String), WitPluginError> {
    // Handle ssh:// scheme: ssh://git@github.com/owner/repo.git
    // or ssh://git@github.com:22/owner/repo.git
    if url.starts_with("ssh://") {
        let without_scheme = url.strip_prefix("ssh://").unwrap();
        // Remove user@ prefix if present
        let after_user = without_scheme.rsplit_once('@').map(|(_, r)| r).unwrap_or(without_scheme);
        // Remove host:port — split on '/' and skip the first segment (host or host:port)
        let path = after_user.split_once('/').map(|(_, r)| r).unwrap_or("");
        let clean = path.trim_end_matches(".git");
        let parts: Vec<&str> = clean.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Ok((parts[0].to_string(), parts[1].to_string()));
        }
    }

    let path = if let Some(rest) = url.strip_prefix("git@") {
        // SSH: git@github.com:owner/repo.git
        rest.split(':')
            .nth(1)
            .ok_or_else(|| github_err(format!("cannot parse SSH remote: {url}")))?
            .to_string()
    } else if url.starts_with("https://") || url.starts_with("http://") {
        // HTTPS: https://github.com/owner/repo.git
        let parts: Vec<&str> = url.splitn(4, '/').collect();
        if parts.len() < 4 {
            return Err(github_err(format!("cannot parse HTTPS remote: {url}")));
        }
        parts[3].to_string()
    } else {
        return Err(github_err(format!("unsupported remote URL format: {url}")));
    };

    // Strip trailing .git
    let path = path.strip_suffix(".git").unwrap_or(&path);

    let mut parts = path.splitn(2, '/');
    let owner = parts
        .next()
        .ok_or_else(|| github_err("missing owner in remote URL"))?
        .to_string();
    let repo = parts
        .next()
        .ok_or_else(|| github_err("missing repo in remote URL"))?
        .to_string();

    if owner.is_empty() || repo.is_empty() {
        return Err(github_err(format!(
            "empty owner or repo in remote URL: {url}"
        )));
    }

    Ok((owner, repo))
}

/// Parse the `Link` header to extract the `rel="next"` URL.
///
/// GitHub Link headers look like:
/// ```text
/// <https://api.github.com/repos/o/r/issues?page=2>; rel="next",
/// <https://api.github.com/repos/o/r/issues?page=10>; rel="last"
/// ```
fn parse_link_header(header: &str) -> Option<String> {
    for segment in header.split(',') {
        let segment = segment.trim();
        if segment.contains("rel=\"next\"") {
            // Extract URL between < and >
            let start = segment.find('<')? + 1;
            let end = segment.find('>')?;
            return Some(segment[start..end].to_string());
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Remote URL parsing ────────────────────────────────────────────

    #[test]
    fn test_parse_github_remote_ssh() {
        let (owner, repo) = parse_remote_url("git@github.com:octocat/Hello-World.git").unwrap();
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "Hello-World");
    }

    #[test]
    fn test_parse_github_remote_ssh_no_suffix() {
        let (owner, repo) = parse_remote_url("git@github.com:octocat/Hello-World").unwrap();
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "Hello-World");
    }

    #[test]
    fn test_parse_github_remote_https() {
        let (owner, repo) = parse_remote_url("https://github.com/octocat/Hello-World.git").unwrap();
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "Hello-World");
    }

    #[test]
    fn test_parse_github_remote_https_no_suffix() {
        let (owner, repo) = parse_remote_url("https://github.com/octocat/Hello-World").unwrap();
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "Hello-World");
    }

    #[test]
    fn test_parse_remote_url_ssh_scheme() {
        let (owner, repo) = parse_remote_url("ssh://git@github.com/owner/repo.git").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_remote_url_ssh_scheme_with_port() {
        let (owner, repo) = parse_remote_url("ssh://git@github.com:22/owner/repo.git").unwrap();
        assert_eq!(owner, "owner");
        assert_eq!(repo, "repo");
    }

    #[test]
    fn test_parse_github_remote_invalid_format() {
        let result = parse_remote_url("ftp://example.com/foo/bar");
        assert!(result.is_err());
    }

    // ── Link header parsing ──────────────────────────────────────────

    #[test]
    fn test_parse_link_header_next() {
        let header = r#"<https://api.github.com/repos/o/r/issues?page=2&per_page=100>; rel="next", <https://api.github.com/repos/o/r/issues?page=10&per_page=100>; rel="last""#;
        let next = parse_link_header(header);
        assert_eq!(
            next,
            Some("https://api.github.com/repos/o/r/issues?page=2&per_page=100".to_string())
        );
    }

    #[test]
    fn test_parse_link_header_no_next() {
        let header = r#"<https://api.github.com/repos/o/r/issues?page=1>; rel="last""#;
        let next = parse_link_header(header);
        assert!(next.is_none());
    }

    #[test]
    fn test_parse_link_header_empty() {
        let next = parse_link_header("");
        assert!(next.is_none());
    }

    // ── aggregate_review_state ───────────────────────────────────────

    fn make_review(user: &str, state: &str) -> PrReview {
        PrReview {
            id: 1,
            state: state.to_string(),
            body: None,
            user: GitHubUser {
                login: user.to_string(),
            },
            submitted_at: None,
        }
    }

    #[test]
    fn test_aggregate_review_state_approved() {
        let reviews = vec![make_review("alice", "APPROVED")];
        assert_eq!(aggregate_review_state(&reviews), "approved");
    }

    #[test]
    fn test_aggregate_review_state_changes_requested() {
        let reviews = vec![
            make_review("alice", "APPROVED"),
            make_review("bob", "CHANGES_REQUESTED"),
        ];
        assert_eq!(aggregate_review_state(&reviews), "changes_requested");
    }

    #[test]
    fn test_aggregate_review_state_pending_no_reviews() {
        let reviews: Vec<PrReview> = vec![];
        assert_eq!(aggregate_review_state(&reviews), "pending");
    }

    #[test]
    fn test_aggregate_review_state_pending_only_commented() {
        let reviews = vec![make_review("alice", "COMMENTED")];
        assert_eq!(aggregate_review_state(&reviews), "pending");
    }

    #[test]
    fn test_aggregate_review_state_latest_per_user() {
        // Alice first requests changes, then approves
        let reviews = vec![
            make_review("alice", "CHANGES_REQUESTED"),
            make_review("alice", "APPROVED"),
        ];
        assert_eq!(aggregate_review_state(&reviews), "approved");
    }

    // ── is_auto_dev_pr ───────────────────────────────────────────────

    fn make_pr(branch: &str, body: Option<&str>) -> PullRequest {
        PullRequest {
            number: 1,
            title: "Test PR".to_string(),
            head: PrRef {
                ref_field: branch.to_string(),
                sha: "abc123".to_string(),
            },
            base: PrRef {
                ref_field: "main".to_string(),
                sha: "def456".to_string(),
            },
            html_url: "https://github.com/test/repo/pull/1".to_string(),
            user: GitHubUser {
                login: "bot".to_string(),
            },
            body: body.map(String::from),
            requested_reviewers: vec![],
        }
    }

    #[test]
    fn test_is_auto_dev_pr_by_branch() {
        let pr = make_pr("auto-dev/story-42", None);
        assert!(is_auto_dev_pr(&pr));
    }

    #[test]
    fn test_is_auto_dev_pr_by_body() {
        let pr = make_pr(
            "feature/something",
            Some("Changes\n\nCo-authored-by: pulse-auto-dev"),
        );
        assert!(is_auto_dev_pr(&pr));
    }

    #[test]
    fn test_is_not_auto_dev_pr() {
        let pr = make_pr("feature/something", Some("Regular PR"));
        assert!(!is_auto_dev_pr(&pr));
    }

    #[test]
    fn test_is_auto_dev_pr_no_body() {
        let pr = make_pr("feature/something", None);
        assert!(!is_auto_dev_pr(&pr));
    }

    // ── FixContext serialization roundtrip ────────────────────────────

    #[test]
    fn test_fix_context_serialization_roundtrip() {
        let ctx = FixContext {
            pr_number: 42,
            branch: "auto-dev/story-42".to_string(),
            base_branch: "main".to_string(),
            html_url: "https://github.com/test/repo/pull/42".to_string(),
            review_summary: "Reviewer: alice\nFix the tests".to_string(),
            file_comments: vec![FileCommentGroup {
                file_path: "src/lib.rs".to_string(),
                comments: vec![InlineComment {
                    line_number: Some(10),
                    diff_hunk: "@@ -8,3 +8,5 @@".to_string(),
                    reviewer_comment: "Add error handling".to_string(),
                    reviewer: "alice".to_string(),
                }],
            }],
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: FixContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pr_number, 42);
        assert_eq!(deserialized.branch, "auto-dev/story-42");
        assert_eq!(deserialized.file_comments.len(), 1);
        assert_eq!(deserialized.file_comments[0].comments.len(), 1);
        assert_eq!(
            deserialized.file_comments[0].comments[0].line_number,
            Some(10)
        );
    }

    // ── PrReview deserialization ─────────────────────────────────────

    #[test]
    fn test_pr_review_deserialization() {
        let json = r#"{
            "id": 123,
            "state": "APPROVED",
            "body": "Looks good!",
            "user": { "login": "reviewer1" },
            "submitted_at": "2026-03-28T12:00:00Z"
        }"#;
        let review: PrReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.id, 123);
        assert_eq!(review.state, "APPROVED");
        assert_eq!(review.body.as_deref(), Some("Looks good!"));
        assert_eq!(review.user.login, "reviewer1");
    }

    // ── GitHubIssue deserialization ──────────────────────────────────

    #[test]
    fn test_github_issue_deserialization() {
        let json = r#"{
            "number": 42,
            "title": "Fix login bug",
            "body": "The login page crashes",
            "labels": [{"name": "bug"}, {"name": "priority"}],
            "milestone": {"title": "Sprint 5", "number": 3},
            "html_url": "https://github.com/test/repo/issues/42",
            "state": "open"
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.number, 42);
        assert_eq!(issue.title, "Fix login bug");
        assert_eq!(issue.labels.len(), 2);
        assert_eq!(issue.labels[0].name, "bug");
        assert!(issue.milestone.is_some());
        assert_eq!(issue.milestone.as_ref().unwrap().title, "Sprint 5");
        assert!(issue.pull_request.is_none());
    }

    #[test]
    fn test_github_issue_with_pull_request_field() {
        let json = r#"{
            "number": 10,
            "title": "Some PR",
            "body": null,
            "labels": [],
            "milestone": null,
            "html_url": "https://github.com/test/repo/pull/10",
            "state": "open",
            "pull_request": {"url": "https://api.github.com/repos/test/repo/pulls/10"}
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).unwrap();
        assert!(issue.pull_request.is_some());
    }
}
