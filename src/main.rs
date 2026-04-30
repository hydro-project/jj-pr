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
use serde::Serialize;

/// Raw input data loaded from jj and gh, before any derived state is computed.
#[derive(Serialize)]
struct InputData {
    jj_entries: Vec<jj::JjLogEntry>,
    prs: BTreeMap<gh::PrNum, gh::GhPr>,
    default_branch: String,
}

/// Global input data, set once after loading. Accessible from panic hook.
static INPUT_DATA: OnceLock<InputData> = OnceLock::new();

fn main() -> Result<()> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(input) = INPUT_DATA.get() {
            eprintln!("jj-pr panicked! Dumping input state...");
            dump_input(input);
        }
        hook(info);
    }));
    run()
}

/// Write input data as JSON to a temp file for debugging.
fn dump_input(input: &InputData) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("jj-pr-dump-{}.json", std::process::id()));
    if let Ok(file) = std::fs::File::create(&path)
        && serde_json::to_writer(file, input).is_ok()
    {
        eprintln!("State dumped to: {}", path.display());
    }
}

/// Write input data as JSON to a writer (used by `dump` command).
fn write_input_json(input: &InputData, out: &mut impl std::io::Write) -> Result<()> {
    serde_json::to_writer(out, input)?;
    Ok(())
}

fn run() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let yes = cli.yes;

    let jj_entries = jj::load_entries()?;
    let prs = gh::load_prs()?
        .into_iter()
        .map(|pr| (pr.number, pr))
        .collect::<BTreeMap<_, _>>();
    let default_branch = gh::default_branch()?;

    // Handle `dump` before building derived state — it only needs raw input.
    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {}));
    if let Command::Dump = &command {
        let input = InputData { jj_entries, prs, default_branch };
        write_input_json(&input, &mut std::io::stdout())?;
        println!();
        return Ok(());
    }

    // Store input data globally for panic dump.
    let input = INPUT_DATA.get_or_init(|| InputData { jj_entries, prs, default_branch });

    let state = pr_dag::build(&input.jj_entries, &input.prs, &input.default_branch)?;

    let result = match command {
        Command::Show(_args) => pr_dag::render_show(&state, &input.prs, &mut std::io::stdout()),
        Command::Log(args) => {
            pr_dag::render_log(&state, &input.prs, &input.jj_entries, args.all, &mut std::io::stdout())
        }
        Command::Sync(args) => {
            let actions = pr_dag::plan_sync(&state, &input.prs, &input.jj_entries, &input.default_branch)?;
            if actions.is_empty() {
                eprintln!("Nothing to sync.");
                return Ok(());
            }
            for action in &actions {
                eprintln!("  {action}");
            }
            if args.dry_run {
                eprintln!("\n{}", style::warn("Dry run: no changes applied."));
            } else if ui::confirm(&format!("Apply {} action(s)?", actions.len()), yes) {
                pr_dag::execute_sync(&actions)?;
            }
            Ok(())
        }
        Command::Create(args) => pr_dag::cmd_create(
            &state,
            &input.prs,
            &input.jj_entries,
            &input.default_branch,
            &args.bookmark,
            args.title.as_deref(),
            args.body.as_deref(),
        ),
        Command::Dump => unreachable!("handled above"),
    };

    if result.is_err() && let Some(data) = INPUT_DATA.get() {
        dump_input(data);
    }
    result
}
