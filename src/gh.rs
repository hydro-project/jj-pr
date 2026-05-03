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
    #[serde(default)]
    pub review_decision: Option<ReviewDecision>,
    #[serde(default)]
    pub checks_status: Option<CheckStatus>,
}

const GRAPHQL_QUERY: &str = r#"
query($owner: String!, $repo: String!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequests(first: 100, after: $cursor, states: [OPEN, CLOSED, MERGED]) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number
        headRefName
        baseRefName
        state
        isDraft
        url
        title
        reviewDecision
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup { state }
            }
          }
        }
      }
    }
  }
}
"#;

#[derive(Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Deserialize)]
struct GraphQlData {
    repository: GraphQlRepo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQlRepo {
    pull_requests: GraphQlPrConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQlPrConnection {
    page_info: GraphQlPageInfo,
    nodes: Vec<GraphQlPr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQlPr {
    number: PrNum,
    head_ref_name: String,
    base_ref_name: String,
    state: PrState,
    is_draft: bool,
    url: String,
    title: String,
    review_decision: Option<ReviewDecision>,
    commits: GraphQlCommitConnection,
}

#[derive(Deserialize)]
struct GraphQlCommitConnection {
    nodes: Vec<GraphQlCommitNode>,
}

#[derive(Deserialize)]
struct GraphQlCommitNode {
    commit: GraphQlCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphQlCommit {
    status_check_rollup: Option<GraphQlStatusCheckRollup>,
}

#[derive(Deserialize)]
struct GraphQlStatusCheckRollup {
    state: String,
}

fn parse_check_state(s: &str) -> Option<CheckStatus> {
    match s {
        "SUCCESS" => Some(CheckStatus::Pass),
        "FAILURE" | "ERROR" => Some(CheckStatus::Fail),
        "PENDING" | "EXPECTED" => Some(CheckStatus::Pending),
        _ => None,
    }
}

pub fn load_prs() -> Result<Vec<GhPr>> {
    let mut all_prs = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut args = vec![
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("query={GRAPHQL_QUERY}"),
            "-F".to_owned(),
            "owner={owner}".to_owned(),
            "-F".to_owned(),
            "repo={repo}".to_owned(),
        ];
        if let Some(ref c) = cursor {
            args.push("-f".to_owned());
            args.push(format!("cursor={c}"));
        }

        let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = Command::new("gh")
            .args(&str_args)
            .output()
            .context("Failed to run `gh api graphql`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("gh api graphql failed: {stderr}");
        }

        let stdout = String::from_utf8(output.stdout).context("gh output not UTF-8")?;
        let resp: GraphQlResponse =
            serde_json::from_str(&stdout).context("Failed to parse GraphQL response")?;
        let conn = resp.data.repository.pull_requests;

        for gql_pr in conn.nodes {
            let checks_status = gql_pr
                .commits
                .nodes
                .first()
                .and_then(|n| n.commit.status_check_rollup.as_ref())
                .and_then(|r| parse_check_state(&r.state));

            all_prs.push(GhPr {
                number: gql_pr.number,
                head_ref_name: gql_pr.head_ref_name,
                base_ref_name: gql_pr.base_ref_name,
                state: gql_pr.state,
                is_draft: gql_pr.is_draft,
                url: gql_pr.url,
                title: gql_pr.title,
                review_decision: gql_pr.review_decision,
                checks_status,
            });
        }

        if conn.page_info.has_next_page && all_prs.len() < 200 {
            cursor = conn.page_info.end_cursor;
        } else {
            break;
        }
    }

    Ok(all_prs)
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

/// Get the default branch name for the current GitHub repo.
pub fn default_branch() -> Result<String> {
    let output = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ])
        .output()
        .context("Failed to run `gh repo view`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh repo view failed: {stderr}");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
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
