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

#[derive(Debug, Clone, Default)]
pub struct PrStatus {
    pub review_decision: Option<ReviewDecision>,
    pub checks_status: Option<CheckStatus>,
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
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,headRefName,baseRefName,state,isDraft,url,title",
            "--limit",
            "200",
            "--state",
            "all",
        ])
        .output()
        .context("Failed to run `gh pr list`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh pr list failed: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout).context("gh output not UTF-8")?;
    let prs: Vec<GhPr> = serde_json::from_str(&stdout).context("Failed to parse gh pr list")?;
    Ok(prs)
}

/// Fetch CI and review status for the given PRs via a single GraphQL query.
pub fn load_pr_statuses(pr_nums: &[PrNum]) -> Result<std::collections::BTreeMap<PrNum, PrStatus>> {
    let mut statuses = std::collections::BTreeMap::new();
    if pr_nums.is_empty() {
        return Ok(statuses);
    }

    // Build a single GraphQL query with aliases: pr123: pullRequest(number: 123) { ... }
    let fragment = r#"reviewDecision commits(last:1) { nodes { commit { statusCheckRollup { state } } } }"#;
    let fields: Vec<String> = pr_nums
        .iter()
        .map(|n| format!("pr{}: pullRequest(number: {}) {{ {fragment} }}", n.get(), n.get()))
        .collect();
    let query = format!(
        "query($owner: String!, $repo: String!) {{ repository(owner: $owner, name: $repo) {{ {} }} }}",
        fields.join(" ")
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
        .context("Failed to run `gh api graphql` for PR statuses")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("gh api graphql (statuses) failed: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout).context("gh output not UTF-8")?;
    let resp: serde_json::Value =
        serde_json::from_str(&stdout).context("Failed to parse GraphQL status response")?;
    let repo_data = &resp["data"]["repository"];

    for &pr_num in pr_nums {
        let key = format!("pr{}", pr_num.get());
        let pr_data = &repo_data[key];
        if pr_data.is_null() {
            continue;
        }

        let mut status = PrStatus::default();

        if let Some(rd) = pr_data["reviewDecision"].as_str() {
            status.review_decision = match rd {
                "APPROVED" => Some(ReviewDecision::Approved),
                "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
                "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
                _ => None,
            };
        }

        let rollup = &pr_data["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"];
        if let Some(state) = rollup.as_str() {
            status.checks_status = parse_check_state(state);
        }

        statuses.insert(pr_num, status);
    }

    Ok(statuses)
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
