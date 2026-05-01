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

use anyhow::Result;
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
    #[serde(default)]
    pub(crate) current_commit: Option<jj::CommitId>,
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

    let jj_entries = jj::load_entries()?;
    let prs = gh::load_prs()?;
    let default_branch = gh::default_branch()?;
    let current_commit = jj::resolve_at()
        .map_err(|e| tracing::debug!("Failed to resolve current jj commit for '@': {e}"))
        .ok();

    // Store input data globally for panic/error dump.
    let input = INPUT_DATA.get_or_init(|| InputData {
        jj_entries,
        prs,
        default_branch,
        current_commit,
    });

    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {}));

    // Handle `dump` before building derived state — it only needs raw input.
    if let Command::Dump = &command {
        serde_json::to_writer(std::io::stdout(), input)?;
        println!();
        return Ok(());
    }

    let prs = input.prs_map();
    let state = pr_dag::build(
        &input.jj_entries,
        &prs,
        &input.default_branch,
        input.current_commit.as_deref(),
    )?;

    let result = match command {
        Command::Show(_args) => pr_dag::render_show(&state, &prs, &mut std::io::stdout()),
        Command::Log(args) => pr_dag::render_log(
            &state,
            &prs,
            &input.jj_entries,
            args.all,
            input.current_commit.as_deref(),
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
        Command::Create(args) => pr_dag::cmd_create(
            &state,
            &prs,
            &input.jj_entries,
            &input.default_branch,
            &args.bookmark,
            args.title.as_deref(),
            args.body.as_deref(),
        ),
        Command::Dump => unreachable!("handled above"),
    };

    if result.is_err() {
        dump_input_on_failure(INPUT_DATA.get());
    }
    result
}
