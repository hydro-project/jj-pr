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

pub fn create_pr(head: &str, base: &str, title: &str, body: &str, draft: bool) -> Result<(u64, String)> {
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
