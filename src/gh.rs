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

fn parse_review_decision(s: &str) -> Option<ReviewDecision> {
    match s {
        "APPROVED" => Some(ReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(ReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

/// Fetch PR data + statuses + default branch in a single GraphQL call.
/// Only fetches PRs for the given PR numbers (extracted from jj trailers).
pub fn load_prs_and_default_branch(
    pr_nums: &[PrNum],
) -> Result<(Vec<GhPr>, std::collections::BTreeMap<PrNum, PrStatus>, String)> {
    let mut pr_fields = String::new();
    for n in pr_nums {
        use std::fmt::Write;
        write!(
            pr_fields,
            r#" pr{0}: pullRequest(number: {0}) {{ number headRefName baseRefName state isDraft url title reviewDecision commits(last:1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }} }}"#,
            n.get()
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
    let resp: serde_json::Value = serde_json::from_str(&stdout).context("Failed to parse GraphQL response")?;
    let repo_data = &resp["data"]["repository"];

    let default_branch = repo_data["defaultBranchRef"]["name"]
        .as_str()
        .context("missing defaultBranchRef.name")?
        .to_owned();

    let mut prs = Vec::new();
    let mut statuses = std::collections::BTreeMap::new();

    for &pr_num in pr_nums {
        let key = format!("pr{}", pr_num.get());
        let d = &repo_data[key];
        if d.is_null() {
            continue;
        }

        let state = match d["state"].as_str() {
            Some("OPEN") => PrState::Open,
            Some("MERGED") => PrState::Merged,
            Some("CLOSED") => PrState::Closed,
            other => bail!("unexpected PR state: {other:?}"),
        };

        prs.push(GhPr {
            number: pr_num,
            head_ref_name: d["headRefName"].as_str().unwrap_or_default().to_owned(),
            base_ref_name: d["baseRefName"].as_str().unwrap_or_default().to_owned(),
            state,
            is_draft: d["isDraft"].as_bool().unwrap_or(false),
            url: d["url"].as_str().unwrap_or_default().to_owned(),
            title: d["title"].as_str().unwrap_or_default().to_owned(),
        });

        if state == PrState::Open {
            let mut status = PrStatus::default();
            if let Some(rd) = d["reviewDecision"].as_str() {
                status.review_decision = parse_review_decision(rd);
            }
            let rollup = &d["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"];
            if let Some(s) = rollup.as_str() {
                status.checks_status = parse_check_state(s);
            }
            statuses.insert(pr_num, status);
        }
    }

    Ok((prs, statuses, default_branch))
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
