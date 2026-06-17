use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::types::{Bookmark, CommitId};

/// Resolve the git directory via `jj git root` (cached, fallible).
fn git_dir() -> Result<&'static Path> {
    static GIT_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    GIT_DIR
        .get_or_init(|| {
            let output = Command::new("jj")
                .args(["git", "root"])
                .output()
                .map_err(|e| format!("failed to run `jj git root`: {e}"))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("jj git root failed: {stderr}"));
            }
            let path = String::from_utf8(output.stdout).map_err(|e| format!("jj git root output not UTF-8: {e}"))?;
            Ok(PathBuf::from(path.trim()))
        })
        .as_ref()
        .map(|p| p.as_path())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Create a `gh` command with `GIT_DIR` set so it works from any jj workspace.
fn gh_command() -> Result<Command> {
    let mut cmd = Command::new("gh");
    cmd.env("GIT_DIR", git_dir()?);
    Ok(cmd)
}

/// Get the owner of the upstream (canonical) repo as `gh` resolves it.
pub fn repo_owner() -> Result<String> {
    let output = gh_command()?
        .args(["repo", "view", "--json", "owner", "-q", ".owner.login"])
        .output()
        .context("Failed to run `gh repo view`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh repo view failed: {stderr}");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Newtype for GitHub PR numbers.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PrNum(NonZeroU64);

impl PrNum {
    pub fn new(n: u64) -> Option<Self> {
        NonZeroU64::new(n).map(Self)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for PrNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrState {
    Closed,
    Open,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum CheckStatus {
    Pass,
    Fail,
    Pending,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhPr {
    pub number: PrNum,
    pub head_ref_name: Bookmark,
    pub base_ref_name: Bookmark,
    pub state: PrState,
    pub is_draft: bool,
    pub url: String,
    pub title: String,
    /// The commit SHA of the merge/squash commit on the base branch (only for merged PRs).
    #[serde(default)]
    pub merge_commit_oid: Option<CommitId>,
    /// The owner (user/org) of the head repository (fork). None if same repo.
    #[serde(default)]
    pub head_repo_owner: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrStatus {
    pub review_decision: Option<ReviewDecision>,
    pub checks_status: Option<CheckStatus>,
}

/// GraphQL fields for a PR node, used in query construction.
const PR_NODE_FIELDS: &str = "number headRefName baseRefName state isDraft url title reviewDecision latestReviews(first:10) { nodes { state } } headRepositoryOwner { login } mergeCommit { oid } commits(last:1) { nodes { commit { statusCheckRollup { state } } } }";

/// Raw GraphQL response types for serde deserialization.
#[derive(Deserialize)]
struct GraphQlResponse {
    data: Option<GraphQlData>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Deserialize)]
struct GraphQlData {
    repository: Option<RepositoryData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryData {
    default_branch_ref: DefaultBranchRef,
    /// All `prN` and `brN` aliased fields, collected into a flat list of PrNodes.
    #[serde(flatten, deserialize_with = "deserialize_pr_nodes")]
    pr_nodes: Vec<PrNode>,
}

/// Custom deserializer that extracts PrNode values from the flattened GraphQL aliases.
/// Handles both `prN: PrNode | null` and `brN: { nodes: [PrNode] }` shapes.
fn deserialize_pr_nodes<'de, D>(deserializer: D) -> std::result::Result<Vec<PrNode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let map: BTreeMap<String, serde_json::Value> = BTreeMap::deserialize(deserializer)?;
    let mut nodes = Vec::new();
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        if key.starts_with("pr") {
            let node = serde_json::from_value::<PrNode>(value)
                .map_err(|e| D::Error::custom(format!("failed to deserialize `{key}`: {e}")))?;
            nodes.push(node);
        } else if key.starts_with("br") {
            #[derive(Deserialize)]
            struct Connection {
                nodes: Vec<PrNode>,
            }
            let conn = serde_json::from_value::<Connection>(value)
                .map_err(|e| D::Error::custom(format!("failed to deserialize `{key}`: {e}")))?;
            nodes.extend(conn.nodes);
        }
    }
    Ok(nodes)
}

#[derive(Deserialize)]
struct DefaultBranchRef {
    name: Bookmark,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: PrNum,
    head_ref_name: Bookmark,
    base_ref_name: Bookmark,
    state: PrState,
    is_draft: bool,
    url: String,
    title: String,
    review_decision: Option<ReviewDecision>,
    latest_reviews: Option<LatestReviews>,
    merge_commit: Option<MergeCommit>,
    head_repository_owner: Option<HeadRepoOwner>,
    commits: CommitsConnection,
}

#[derive(Deserialize)]
struct LatestReviews {
    nodes: Vec<ReviewNode>,
}

#[derive(Deserialize)]
struct ReviewNode {
    state: ReviewState,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

#[derive(Deserialize)]
struct MergeCommit {
    oid: CommitId,
}

#[derive(Deserialize)]
struct HeadRepoOwner {
    login: String,
}

#[derive(Deserialize)]
struct CommitsConnection {
    nodes: Vec<CommitNode>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: CommitData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitData {
    status_check_rollup: Option<StatusCheckRollup>,
}

#[derive(Deserialize)]
struct StatusCheckRollup {
    state: CheckState,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum CheckState {
    Success,
    Failure,
    Error,
    Pending,
    Expected,
}

impl From<CheckState> for CheckStatus {
    fn from(s: CheckState) -> Self {
        match s {
            CheckState::Success => CheckStatus::Pass,
            CheckState::Failure | CheckState::Error => CheckStatus::Fail,
            CheckState::Pending | CheckState::Expected => CheckStatus::Pending,
        }
    }
}

/// Fetch PR data + statuses + default branch in a single GraphQL call.
/// Discovers PRs both by number (from trailers) and by branch name (from local bookmarks).
pub fn load_prs_and_default_branch<'a>(
    pr_nums: &[PrNum],
    bookmarks: impl IntoIterator<Item = &'a Bookmark<str>>,
) -> Result<(Vec<GhPr>, BTreeMap<PrNum, PrStatus>, Bookmark)> {
    let mut pr_fields = String::new();
    for n in pr_nums {
        use std::fmt::Write;
        write!(
            pr_fields,
            r#" pr{0}: pullRequest(number: {0}) {{ {PR_NODE_FIELDS} }}"#,
            n.get()
        )
        .unwrap();
    }
    // Also look up PRs by branch name for bookmarks not already covered by trailers.
    for (i, bm) in bookmarks.into_iter().enumerate() {
        use std::fmt::Write;
        // Escape the bookmark name for safe embedding in a GraphQL string literal.
        let escaped = bm.as_str().replace('\\', "\\\\").replace('"', "\\\"");
        write!(
            pr_fields,
            r#" br{i}: pullRequests(first:1, headRefName:"{escaped}") {{ nodes {{ {PR_NODE_FIELDS} }} }}"#,
        )
        .unwrap();
    }

    let query = format!(
        "query($owner: String!, $repo: String!) {{ repository(owner: $owner, name: $repo) {{ defaultBranchRef {{ name }}{pr_fields} }} }}"
    );

    let output = gh_command()?
        .args([
            "api",
            "graphql",
            "-f",
            &format!("query={query}"),
            "-F",
            "owner={owner}",
            "-F",
            "repo={repo}",
        ])
        .output()
        .context("Failed to run `gh api graphql`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh api graphql failed: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout).context("gh output not UTF-8")?;
    let resp: GraphQlResponse = serde_json::from_str(&stdout).context("Failed to parse GraphQL response")?;
    if let Some(errors) = resp.errors {
        let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        bail!("GraphQL errors: {}", msgs.join("; "));
    }
    let repo_data = resp
        .data
        .context("GraphQL response missing `data`")?
        .repository
        .context("GraphQL response missing `repository` (not found or insufficient permissions)")?;

    let default_branch = repo_data.default_branch_ref.name;

    let mut prs = BTreeMap::<PrNum, GhPr>::new();
    let mut statuses = BTreeMap::new();

    for d in repo_data.pr_nodes {
        let std::collections::btree_map::Entry::Vacant(entry) = prs.entry(d.number) else {
            continue; // Deduplicate (same PR found by number and by branch).
        };

        let checks_status = d
            .commits
            .nodes
            .first()
            .and_then(|n| n.commit.status_check_rollup.as_ref())
            .map(|rollup| CheckStatus::from(rollup.state));
        // `reviewDecision` is sometimes null even when reviews exist (possibly
        // related to rulesets vs classic branch protection — stacked PRs
        // targeting non-main branches behave differently too). Fall back to
        // deriving status from `latestReviews` (best-effort, doesn't account
        // for required review groups).
        let review_decision = d.review_decision.or_else(|| {
            let reviews = d.latest_reviews.as_ref()?;
            let mut has_approved = false;
            for review in &reviews.nodes {
                match review.state {
                    ReviewState::ChangesRequested => return Some(ReviewDecision::ChangesRequested),
                    ReviewState::Approved => has_approved = true,
                    ReviewState::Commented | ReviewState::Dismissed | ReviewState::Pending => {}
                }
            }
            has_approved.then_some(ReviewDecision::Approved)
        });
        statuses.insert(
            d.number,
            PrStatus {
                review_decision,
                checks_status,
            },
        );
        entry.insert(GhPr {
            number: d.number,
            head_ref_name: d.head_ref_name,
            base_ref_name: d.base_ref_name,
            state: d.state,
            is_draft: d.is_draft,
            url: d.url,
            title: d.title,
            merge_commit_oid: d.merge_commit.map(|mc| mc.oid),
            head_repo_owner: d.head_repository_owner.map(|o| o.login),
        });
    }

    Ok((prs.into_values().collect(), statuses, default_branch))
}

pub fn create_pr(
    head: &Bookmark<str>,
    base: &Bookmark<str>,
    title: &str,
    body: &str,
    draft: bool,
    fork_owner: Option<&str>,
) -> Result<(PrNum, String)> {
    // For cross-repo (fork) PRs, GitHub requires "OWNER:branch" as the head ref.
    let head_ref = match fork_owner {
        Some(owner) => format!("{owner}:{}", head.as_str()),
        None => head.as_str().to_owned(),
    };
    let base = base.as_str();
    let mut args = vec![
        "pr", "create", "--head", &head_ref, "--base", base, "--title", title, "--body", body,
    ];
    if draft {
        args.push("--draft");
    }

    let output = gh_command()?
        .args(&args)
        .output()
        .context("Failed to run `gh pr create`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr create failed: {stderr}");
    }

    // gh pr create prints the URL, extract PR number from it.
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    // URL format: https://github.com/owner/repo/pull/123
    let pr_number = url
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(PrNum::new)
        .with_context(|| format!("Could not parse PR number from gh output: {url}"))?;

    Ok((pr_number, url))
}

pub fn edit_base(pr_number: u64, base: &Bookmark<str>) -> Result<()> {
    let num = pr_number.to_string();
    let output = gh_command()?
        .args(["pr", "edit", &num, "--base", base.as_str()])
        .output()
        .context("Failed to run `gh pr edit`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr edit {pr_number} --base {base} failed: {stderr}");
    }
    Ok(())
}

#[expect(dead_code, reason = "TODO")]
pub fn set_ready(pr_number: u64, ready: bool) -> Result<()> {
    let num = pr_number.to_string();
    let args = if ready {
        vec!["pr", "ready", &num]
    } else {
        vec!["pr", "ready", &num, "--undo"]
    };
    let output = gh_command()?
        .args(&args)
        .output()
        .context("Failed to run `gh pr ready`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr ready failed: {stderr}");
    }
    Ok(())
}
