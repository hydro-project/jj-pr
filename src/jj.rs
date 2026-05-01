use std::borrow::Borrow;
use std::fmt::Display;
use std::ops::Deref;
use std::process::Command;

use anyhow::{Context, Result, bail};
use ref_cast::RefCast;
use serde::{Deserialize, Serialize};

use crate::gh::PrNum;

/// Newtype for the commit_id hash.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd, RefCast)]
#[repr(transparent)]
#[serde(transparent)]
pub struct CommitId<T: ?Sized = String>(pub T);
impl<T: ?Sized> Display for CommitId<T>
where
    T: Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: ?Sized> Deref for CommitId<T>
where
    T: Deref,
{
    type Target = CommitId<T::Target>;
    fn deref(&self) -> &Self::Target {
        CommitId::ref_cast(self.0.deref())
    }
}

impl Borrow<CommitId<str>> for CommitId {
    fn borrow(&self) -> &CommitId<str> {
        self
    }
}

impl ToOwned for CommitId<str> {
    type Owned = CommitId<String>;

    fn to_owned(&self) -> Self::Owned {
        CommitId(self.0.to_owned())
    }
}

/// Raw commit data from `json(self)`.
#[derive(Debug, Deserialize, Serialize)]
pub struct JjCommit {
    pub commit_id: CommitId,
    pub change_id: String,
    pub parents: Vec<CommitId>,
    pub description: String,
}

/// Bookmark reference from `json(local_bookmarks)` or `json(remote_bookmarks)`.
#[derive(Debug, Deserialize, Serialize)]
pub struct JjBookmark {
    pub name: String,
    pub target: Vec<Option<CommitId>>,
}

/// Remote bookmark reference from `json(remote_bookmarks)`.
#[derive(Debug, Deserialize, Serialize)]
pub struct JjRemoteBookmark {
    pub name: String,
    pub remote: Option<String>,
    pub target: Vec<Option<CommitId>>,
}

/// One line of JSONL output from our composite template.
#[derive(Debug, Deserialize, Serialize)]
pub struct JjLogEntry {
    pub commit: JjCommit,
    pub local_bookmarks: Vec<JjBookmark>,
    pub remote_bookmarks: Vec<JjRemoteBookmark>,
    pub immutable: bool,
    pub is_trunk_tip: bool,
    #[serde(default)]
    pub empty: bool,
    #[serde(default)]
    pub is_working_copy: bool,
}

/// Parsed PR trailer value, e.g. `PR: #1234` → `1234`.
pub fn parse_pr_trailer(description: &str) -> Option<PrNum> {
    // Trailers are `Key: Value` lines at the end of the description.
    // We look for `PR: #<number>` in the trailer block, skipping trailing blank lines.
    let mut in_trailer_block = false;
    for line in description.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            if in_trailer_block {
                break; // Blank line separator before the trailer block.
            }
            continue; // Skip trailing blank lines.
        }
        in_trailer_block = true;
        if let Some(value) = line.strip_prefix("PR: #")
            && let Ok(n) = value.trim().parse::<u64>()
            && let Some(pr_num) = PrNum::new(n)
        {
            return Some(pr_num);
        }
    }
    None
}

/// Update or append a `PR: #N` trailer in a description.
pub fn set_pr_trailer(description: &str, pr: PrNum) -> String {
    let trailer_line = format!("PR: #{}", pr.get());
    let lines: Vec<&str> = description.lines().collect();

    // Find existing PR trailer and replace it, skipping trailing blank lines.
    let mut seen_content = false;
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if seen_content {
                break;
            }
            continue;
        }
        seen_content = true;
        if trimmed.starts_with("PR: #") {
            let mut new_lines: Vec<&str> = lines[..i].to_vec();
            new_lines.push(&trailer_line);
            new_lines.extend_from_slice(&lines[i + 1..]);
            let mut result = new_lines.join("\n");
            if description.ends_with('\n') {
                result.push('\n');
            }
            return result;
        }
    }

    // No existing trailer — append after a blank line if needed.
    let trimmed = description.trim_end();
    if trimmed.is_empty() {
        return format!("{trailer_line}\n");
    }
    // Check if there's already a trailer block (last non-empty line contains `: `).
    let last_line = trimmed.lines().last().unwrap_or("");
    if last_line.contains(": ") {
        // Append to existing trailer block.
        format!("{trimmed}\n{trailer_line}\n")
    } else {
        // Start new trailer block with blank line separator.
        format!("{trimmed}\n\n{trailer_line}\n")
    }
}

/// Remove the `PR: #N` trailer from a description.
#[expect(dead_code, reason = "used by tidy command (TODO)")]
pub fn remove_pr_trailer(description: &str) -> String {
    let lines: Vec<&str> = description.lines().collect();
    let mut result_lines: Vec<&str> = Vec::new();

    // Find and remove the PR trailer line.
    let mut found = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("PR: #") && !found {
            found = true;
            // If previous line is blank (trailer separator), remove it too.
            if result_lines.last().is_some_and(|l: &&str| l.trim().is_empty()) {
                // Check if there are other trailers — if so, keep the blank line.
                let has_other_trailers = lines[..i]
                    .iter()
                    .rev()
                    .take_while(|l| !l.trim().is_empty())
                    .any(|l| l.contains(": ") && !l.trim().starts_with("PR: #"));
                if !has_other_trailers {
                    result_lines.pop();
                }
            }
            continue;
        }
        result_lines.push(line);
    }

    let mut result = result_lines.join("\n");
    if description.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Resolve a revision string to a commit_id.
#[expect(dead_code, reason = "used by track command (TODO)")]
pub fn resolve_revision(rev: &str) -> Result<String> {
    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", rev, "-T", "commit_id"])
        .output()
        .context("Failed to resolve revision")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to resolve revision {rev}: {stderr}");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub fn load_entries() -> Result<Vec<JjLogEntry>> {
    load_entries_with_revset("trunk().. | trunk()")
}

pub fn load_entries_with_revset(revset: &str) -> Result<Vec<JjLogEntry>> {
    const JJ_TEMPLATE: &str = r#""{\"commit\": " ++ json(self) ++ ", \"local_bookmarks\": " ++ json(local_bookmarks) ++ ", \"remote_bookmarks\": " ++ json(remote_bookmarks) ++ ", \"immutable\": " ++ json(self.immutable()) ++ ", \"is_trunk_tip\": " ++ json(self.contained_in("trunk()")) ++ ", \"empty\": " ++ json(self.empty()) ++ ", \"is_working_copy\": " ++ json(self.contained_in("@")) ++ "}\n""#;

    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", revset, "-T", JJ_TEMPLATE])
        .output()
        .context("Failed to run `jj log`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj log failed: {stderr}");
    }

    let mut entries = Vec::new();

    let stdout = String::from_utf8(output.stdout).context("jj log output not UTF-8")?;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry = serde_json::from_str::<JjLogEntry>(line).with_context(|| format!("Failed to parse: {line}"))?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Read the description of a revision.
pub fn read_description(revision: &str) -> Result<String> {
    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", revision, "-T", "description"])
        .output()
        .context("Failed to run `jj log` for description")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj log failed: {stderr}");
    }
    String::from_utf8(output.stdout).context("description not UTF-8")
}

/// Set the description of a revision via `jj describe --stdin`.
pub fn describe_stdin(revision: &str, description: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("jj")
        .args(["describe", revision, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn `jj describe`")?;

    child
        .stdin
        .take()
        .unwrap()
        .write_all(description.as_bytes())
        .context("Failed to write to jj describe stdin")?;

    let output = child.wait_with_output().context("Failed to wait for `jj describe`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj describe failed: {stderr}");
    }
    Ok(())
}

/// Push a bookmark to the remote.
pub fn git_push_bookmark(bookmark: &str) -> Result<()> {
    let output = Command::new("jj")
        .args(["git", "push", "--bookmark", bookmark])
        .output()
        .context("Failed to run `jj git push`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj git push --bookmark {bookmark} failed: {stderr}");
    }
    Ok(())
}

/// Set a bookmark to point at a revision.
#[expect(dead_code, reason = "used by track command (TODO)")]
pub fn bookmark_set(name: &str, revision: &str) -> Result<()> {
    let output = Command::new("jj")
        .args(["bookmark", "set", name, "-r", revision])
        .output()
        .context("Failed to run `jj bookmark set`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj bookmark set {name} failed: {stderr}");
    }
    Ok(())
}

/// Track a remote bookmark.
pub fn bookmark_track(name: &str, remote: &str) -> Result<()> {
    let refname = format!("{name}@{remote}");
    let output = Command::new("jj")
        .args(["bookmark", "track", &refname])
        .output()
        .context("Failed to run `jj bookmark track`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj bookmark track {refname} failed: {stderr}");
    }
    Ok(())
}

/// Rebase revisions onto a destination.
/// `sources` is a revset expression for `-s`, `dest` is a revset for `-d`.
pub fn rebase(sources: &str, dest: &str) -> Result<()> {
    let output = Command::new("jj")
        .args(["rebase", "-s", sources, "-d", dest])
        .output()
        .context("Failed to run `jj rebase`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj rebase -s {sources} -d {dest} failed: {stderr}");
    }
    Ok(())
}

/// Abandon revisions matching a revset.
pub fn abandon(revset: &str) -> Result<()> {
    let output = Command::new("jj")
        .args(["abandon", revset])
        .output()
        .context("Failed to run `jj abandon`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj abandon {revset} failed: {stderr}");
    }
    Ok(())
}

/// Push multiple bookmarks to the remote in a single command.
pub fn git_push_bookmarks(bookmarks: &[&str]) -> Result<()> {
    let mut args = vec!["git", "push"];
    for bm in bookmarks {
        args.push("--bookmark");
        args.push(bm);
    }
    let output = Command::new("jj")
        .args(&args)
        .output()
        .context("Failed to run `jj git push`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("jj git push failed: {stderr}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_trailer_basic() {
        assert_eq!(parse_pr_trailer("some commit\n\nPR: #42\n"), PrNum::new(42));
    }

    #[test]
    fn parse_pr_trailer_with_other_trailers() {
        let desc = "fix bug\n\nCo-authored-by: Alice\nPR: #123\n";
        assert_eq!(parse_pr_trailer(desc), PrNum::new(123));
    }

    #[test]
    fn parse_pr_trailer_missing() {
        assert_eq!(parse_pr_trailer("just a commit message\n"), None);
    }

    #[test]
    fn parse_pr_trailer_no_trailing_newline() {
        assert_eq!(parse_pr_trailer("msg\n\nPR: #7"), PrNum::new(7));
    }

    #[test]
    fn set_pr_trailer_append_new() {
        let result = set_pr_trailer("add feature\n", PrNum::new(99).unwrap());
        assert_eq!(result, "add feature\n\nPR: #99\n");
    }

    #[test]
    fn set_pr_trailer_replace_existing() {
        let result = set_pr_trailer("fix\n\nPR: #10\n", PrNum::new(20).unwrap());
        assert_eq!(result, "fix\n\nPR: #20\n");
    }

    #[test]
    fn set_pr_trailer_append_to_existing_trailers() {
        let result = set_pr_trailer("fix\n\nCo-authored-by: Bob\n", PrNum::new(55).unwrap());
        assert_eq!(result, "fix\n\nCo-authored-by: Bob\nPR: #55\n");
    }

    #[test]
    fn set_pr_trailer_empty_description() {
        let result = set_pr_trailer("", PrNum::new(1).unwrap());
        assert_eq!(result, "PR: #1\n");
    }

    #[test]
    fn parse_pr_trailer_trailing_blank_lines() {
        assert_eq!(parse_pr_trailer("msg\n\nPR: #42\n\n"), PrNum::new(42));
        assert_eq!(parse_pr_trailer("msg\n\nPR: #42\n\n\n"), PrNum::new(42));
    }

    #[test]
    fn set_pr_trailer_replace_with_trailing_blank_lines() {
        let result = set_pr_trailer("fix\n\nPR: #10\n\n", PrNum::new(20).unwrap());
        assert_eq!(result, "fix\n\nPR: #20\n\n");
    }
}
