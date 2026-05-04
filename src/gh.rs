use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub state: PrState,
    pub is_draft: bool,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrStatus {
    pub review_decision: Option<ReviewDecision>,
    pub checks_status: Option<CheckStatus>,
}

/// GraphQL fields for a PR node, used in query construction.
const PR_NODE_FIELDS: &str = "number headRefName baseRefName state isDraft url title reviewDecision commits(last:1) { nodes { commit { statusCheckRollup { state } } } }";

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
    let map: BTreeMap<String, serde_json::Value> = BTreeMap::deserialize(deserializer)?;
    let mut nodes = Vec::new();
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        if key.starts_with("pr") {
            if let Ok(node) = serde_json::from_value::<PrNode>(value) {
                nodes.push(node);
            }
        } else if key.starts_with("br") {
            #[derive(Deserialize)]
            struct Connection {
                nodes: Vec<PrNode>,
            }
            if let Ok(conn) = serde_json::from_value::<Connection>(value) {
                nodes.extend(conn.nodes);
            }
        }
    }
    Ok(nodes)
}

#[derive(Deserialize)]
struct DefaultBranchRef {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: PrNum,
    head_ref_name: String,
    base_ref_name: String,
    state: PrState,
    is_draft: bool,
    url: String,
    title: String,
    review_decision: Option<ReviewDecision>,
    commits: CommitsConnection,
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
pub fn load_prs_and_default_branch(
    pr_nums: &[PrNum],
    bookmarks: &[&str],
) -> Result<(Vec<GhPr>, BTreeMap<PrNum, PrStatus>, String)> {
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
    for (i, bm) in bookmarks.iter().enumerate() {
        use std::fmt::Write;
        write!(
            pr_fields,
            r#" br{i}: pullRequests(first:1, headRefName:"{bm}") {{ nodes {{ {PR_NODE_FIELDS} }} }}"#,
        )
        .unwrap();
    }

    let query = format!(
        "query($owner: String!, $repo: String!) {{ repository(owner: $owner, name: $repo) {{ defaultBranchRef {{ name }}{pr_fields} }} }}"
    );

    let output = Command::new("gh")
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
        statuses.insert(
            d.number,
            PrStatus {
                review_decision: d.review_decision,
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
        });
    }

    Ok((prs.into_values().collect(), statuses, default_branch))
}

pub fn create_pr(head: &str, base: &str, title: &str, body: &str, draft: bool) -> Result<(PrNum, String)> {
    let mut args = vec![
        "pr".to_owned(),
        "create".to_owned(),
        "--head".to_owned(),
        head.to_owned(),
        "--base".to_owned(),
        base.to_owned(),
        "--title".to_owned(),
        title.to_owned(),
        "--body".to_owned(),
        body.to_owned(),
    ];
    if draft {
        args.push("--draft".to_owned());
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = Command::new("gh")
        .args(&str_args)
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

pub fn edit_base(pr_number: u64, base: &str) -> Result<()> {
    let num = pr_number.to_string();
    let output = Command::new("gh")
        .args(["pr", "edit", &num, "--base", base])
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
    let output = Command::new("gh")
        .args(&args)
        .output()
        .context("Failed to run `gh pr ready`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr ready failed: {stderr}");
    }
    Ok(())
}
