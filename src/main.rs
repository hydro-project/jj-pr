mod cli;
mod gh;
mod graph_algorithms;
mod jj;
mod pr_dag;
mod style;
#[cfg(test)]
mod tests;
mod ui;

use std::collections::BTreeMap;
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
    pub(crate) default_branch: String,
}

impl InputData {
    pub(crate) fn prs_map(&self) -> BTreeMap<gh::PrNum, &gh::GhPr> {
        self.prs.iter().map(|pr| (pr.number, pr)).collect()
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

    // Step 1: Load jj entries (the only local I/O).
    let jj_entries = jj::load_entries()?;

    // Step 2: Extract PR numbers from trailers to know which PRs to fetch.
    let pr_nums = pr_dag::extract_pr_nums(&jj_entries);

    // Step 3: Single GraphQL call for PR data + statuses + default branch.
    let (prs, pr_statuses, default_branch) = gh::load_prs_and_default_branch(&pr_nums)?;

    // Store input data globally for panic/error dump.
    let input = INPUT_DATA.get_or_init(|| InputData {
        jj_entries,
        prs,
        default_branch,
    });

    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {}));

    // Handle util commands before building derived state.
    if let Command::Util(util_args) = &command {
        match &util_args.command {
            cli::UtilCommand::Dump => {
                serde_json::to_writer(std::io::stdout(), input)?;
                println!();
                return Ok(());
            }
            cli::UtilCommand::InstallAliases => {
                return install_aliases();
            }
        }
    }

    let prs = input.prs_map();
    let state = pr_dag::build(&input.jj_entries, &prs, &input.default_branch)?;

    let result = match command {
        Command::Show(_args) => pr_dag::render_show(&state, &prs, &pr_statuses, &mut std::io::stdout()),
        Command::Log(args) => pr_dag::render_log(
            &state,
            &prs,
            &pr_statuses,
            &input.jj_entries,
            args.all,
            &mut std::io::stdout(),
        ),
        Command::Sync(args) => {
            let actions = pr_dag::plan_sync(&state, &prs, &input.jj_entries, &input.default_branch)?;
            if actions.is_empty() {
                eprintln!("Nothing to sync.");
                return Ok(());
            }
            for action in &actions {
                eprintln!("  {action}");
            }
            if args.dry_run {
                eprintln!("\n{}", style::warn("Dry run: no changes applied."));
            } else if !ui::confirm(&format!("Apply {} action(s)?", actions.len()), yes) {
                anyhow::bail!("Aborted.");
            } else {
                pr_dag::execute_sync(&actions)?;
            }
            Ok(())
        }
        Command::Create(args) => {
            let plan = pr_dag::plan_create(
                &state,
                &prs,
                &input.jj_entries,
                &input.default_branch,
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

fn install_aliases() -> Result<()> {
    use std::process::Command as Cmd;

    let aliases = [
        ("revset-aliases.\"pr(n)\"", r#"description(regex:"PR: #" ++ n)"#),
        ("revset-aliases.\"pr_root(n)\"", r#"roots(pr(n))"#),
    ];

    for (key, value) in &aliases {
        let output = Cmd::new("jj")
            .args(["config", "set", "--repo", key, value])
            .output()
            .context("Failed to run `jj config set`")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj config set failed for {key}: {stderr}");
        }
    }

    eprintln!("Installed revset aliases:");
    eprintln!("  pr(n)      — all commits in PR #n");
    eprintln!("  pr_root(n) — root commit(s) of PR #n");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  jj log -r 'pr(\"1234\")'");
    eprintln!("  jj rebase -s 'pr_root(\"1234\")' -d main");
    Ok(())
}
