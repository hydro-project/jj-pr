mod cli;
mod gh;
mod graph_algorithms;
mod jj;
mod pr_dag;
mod style;
#[cfg(test)]
mod tests;
pub(crate) mod types;
mod ui;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Command};
use serde::{Deserialize, Serialize};

/// Raw input data loaded from jj and gh, before any derived state is computed.
/// Also used as the test fixture format (matches `jj-pr dump` output).
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct InputData {
    pub(crate) jj_entries: Vec<jj::JjLogEntry>,
    pub(crate) prs: Vec<gh::GhPr>,
    pub(crate) default_branch: types::Bookmark,
    /// Bookmark names → tracked remotes (excluding `git`).
    /// `None` means all bookmarks are considered tracked (legacy behavior).
    // TODO(mingwei): upgrade old fixtures to the map format so we can remove the legacy array deserialization.
    #[serde(default, deserialize_with = "deserialize_tracked_bookmarks")]
    pub(crate) tracked_bookmarks: Option<BTreeMap<types::Bookmark, BTreeSet<types::Remote>>>,
    /// Merge commit OIDs that exist in the local repo (for stale trunk detection).
    /// `None` means all merge commits are considered present (legacy behavior).
    // TODO(mingwei): upgrade fixtures to always include this field.
    #[serde(default)]
    pub(crate) existing_merge_commits: Option<std::collections::HashSet<types::CommitId>>,
    /// Remote name → GitHub owner, for all configured git remotes.
    // TODO(mingwei): upgrade fixtures to always include this field.
    #[serde(default)]
    pub(crate) remote_owners: BTreeMap<types::Remote, types::Owner>,
    /// Owner of the upstream (base) repo as resolved by `gh`.
    // TODO(mingwei): upgrade fixtures to always include this field.
    #[serde(default)]
    pub(crate) upstream_owner: Option<types::Owner>,
}

impl InputData {
    pub(crate) fn prs_map(&self) -> BTreeMap<gh::PrNum, &gh::GhPr> {
        self.prs.iter().map(|pr| (pr.number, pr)).collect()
    }
}

/// Deserialize `tracked_bookmarks` from either the legacy array format (list of bookmark names,
/// all assumed tracked on "origin") or the new map format (bookmark → set of remotes).
// TODO(mingwei): remove legacy array handling once all fixtures are upgraded to map format.
fn deserialize_tracked_bookmarks<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<BTreeMap<types::Bookmark, BTreeSet<types::Remote>>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Array(arr) => {
            // Legacy format: array of bookmark names, all tracked on "origin".
            let mut map = BTreeMap::new();
            for item in arr {
                let name: types::Bookmark = serde_json::from_value(item).map_err(D::Error::custom)?;
                map.insert(name, [types::Remote("origin".to_owned())].into());
            }
            Ok(Some(map))
        }
        serde_json::Value::Object(_) => {
            let map = serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(Some(map))
        }
        serde_json::Value::Null => Ok(None),
        _ => Err(D::Error::custom("tracked_bookmarks must be null, array, or object")),
    }
}

/// Global input data, set once after loading. Accessible from panic hook.
static INPUT_DATA: OnceLock<InputData> = OnceLock::new();

fn main() -> Result<()> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("jj-pr panicked! Dumping input state...");
        dump_input_on_failure(INPUT_DATA.get());
        hook(info);
    }));
    run()
}

/// Dump input data as JSON to a temp file for debugging, or print a message if unavailable.
fn dump_input_on_failure(input: Option<&InputData>) {
    let Some(input) = input else {
        eprintln!("Input state was not yet loaded, no dump available.");
        return;
    };
    let dir = std::env::temp_dir();
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("jj-pr-dump-{epoch}.json"));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => match serde_json::to_writer(file, input) {
            Ok(()) => eprintln!("State dumped to: {}", path.display()),
            Err(e) => eprintln!("Failed to serialize state dump: {e}"),
        },
        Err(e) => eprintln!("Failed to create state dump file {}: {e}", path.display()),
    }
}

fn run() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let yes = cli.yes;

    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {
        all: false,
        reversed: false,
    }));

    // Handle commands that don't need jj/gh state early.
    if let Command::Util(cli::UtilArgs {
        command: cli::UtilCommand::InstallAliases(args),
    }) = &command
    {
        return install_aliases(args.repo);
    }

    // Step 1: Load jj entries.
    let jj_entries = jj::load_entries()?;

    // Step 2: Extract PR numbers from trailers and local bookmark names.
    // We query both because trailers and bookmarks may reference different PRs
    // (e.g. stale trailers from a closed PR, bookmark pointing to a new PR).
    let pr_nums = pr_dag::extract_pr_nums(&jj_entries);
    let local_bookmarks = jj_entries
        .iter()
        .flat_map(|e| e.local_bookmarks.iter().map(|bm| &*bm.name))
        .collect::<BTreeSet<_>>();

    // Step 3: Single GraphQL call for PR data + statuses + default branch.
    let (prs, pr_statuses, default_branch) = gh::load_prs_and_default_branch(&pr_nums, local_bookmarks)?;

    // Step 4: Load tracked bookmarks, remote owners, and upstream owner.
    let tracked_bookmarks = jj::load_tracked_bookmarks()?;
    let remote_owners = jj::load_remote_owners()?;
    let upstream_owner = gh::upstream_repo_owner().ok();

    // Step 5: Check which merged PRs have their merge commit in the local repo.
    let merge_oids: Vec<&types::CommitId<str>> = prs
        .iter()
        .filter(|pr| pr.state == gh::PrState::Merged)
        .filter_map(|pr| pr.merge_commit_oid.as_deref())
        .collect();
    let existing_merge_commits = jj::check_commits_exist(&merge_oids)?;

    // Store input data globally for panic/error dump.
    let input = INPUT_DATA.get_or_init(|| InputData {
        jj_entries,
        prs,
        default_branch,
        tracked_bookmarks: Some(tracked_bookmarks),
        existing_merge_commits: Some(existing_merge_commits),
        remote_owners,
        upstream_owner,
    });

    // Handle util commands that need input data.
    if let Command::Util(cli::UtilArgs {
        command: cli::UtilCommand::Dump,
    }) = &command
    {
        serde_json::to_writer(std::io::stdout(), input)?;
        println!();
        return Ok(());
    }

    let prs = input.prs_map();
    let state = pr_dag::build(
        &input.jj_entries,
        &prs,
        &input.default_branch,
        input.tracked_bookmarks.as_ref(),
        &input.remote_owners,
    )?;

    let result = match command {
        Command::Show(args) => pr_dag::render_show(
            &state,
            &prs,
            &pr_statuses,
            args.all,
            args.reversed,
            &mut std::io::stdout(),
        ),
        Command::Log(args) => pr_dag::render_log(
            &state,
            &prs,
            &pr_statuses,
            &input.jj_entries,
            args.all,
            args.reversed,
            &mut std::io::stdout(),
        ),
        Command::Sync(args) => {
            let plan = pr_dag::plan_sync(
                &state,
                &prs,
                &input.jj_entries,
                &input.default_branch,
                input.existing_merge_commits.as_ref(),
            )?;
            for warning in &plan.warnings {
                eprintln!("  {}", style::warn(warning));
            }
            if plan.actions.is_empty() {
                eprintln!("Nothing to sync.");
                return Ok(());
            }
            for action in &plan.actions {
                eprintln!("  {action}");
            }
            if args.dry_run {
                eprintln!("\n{}", style::warn("Dry run: no changes applied."));
            } else if !ui::confirm(&format!("Apply {} action(s)?", plan.actions.len()), yes) {
                anyhow::bail!("Aborted.");
            } else {
                pr_dag::execute_sync(&plan.actions)?;
            }
            Ok(())
        }
        Command::Create(args) => {
            let push_remote_config = jj::push_remote_config()?;
            let plan = pr_dag::plan_create(
                &state,
                &prs,
                &input.jj_entries,
                &input.default_branch,
                input.tracked_bookmarks.as_ref(),
                &input.remote_owners,
                input.upstream_owner.as_deref(),
                push_remote_config.as_deref(),
                &args.bookmark,
                args.title.as_deref(),
                args.body.as_deref(),
            )?;
            eprint!("{plan}");
            if args.dry_run {
                eprintln!("\n{}", style::warn("Dry run: no changes applied."));
            } else if !ui::confirm("Create PR?", yes) {
                anyhow::bail!("Aborted.");
            } else {
                pr_dag::execute_create(&plan)?;
            }
            Ok(())
        }
        Command::Util(_) => unreachable!("handled above"),
    };

    if result.is_err() {
        dump_input_on_failure(INPUT_DATA.get());
    }
    result
}

fn install_aliases(repo: bool) -> Result<()> {
    use std::process::Command as Cmd;

    let scope = if repo { "--repo" } else { "--user" };

    // Install `jj pr` subcommand alias.
    let output = Cmd::new("jj")
        .args([
            "config",
            "set",
            scope,
            "aliases.pr",
            r#"["util", "exec", "--", "jj-pr"]"#,
        ])
        .output()
        .context("Failed to run `jj config set` for alias")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj config set failed for aliases.pr: {stderr}");
    }

    eprintln!("Installed to {scope} config:");
    eprintln!("  command alias: jj pr — runs jj-pr");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  jj pr show");
    eprintln!("  jj pr sync");
    Ok(())
}
