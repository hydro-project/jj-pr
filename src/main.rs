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

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};

fn main() -> Result<()> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        DUMP_JSON.with(|cell| {
            if let Some(json) = cell.borrow().as_ref() {
                eprintln!("jj-pr panicked! Dumping pre-mutation state...");
                write_dump(json);
            }
        });
        hook(info);
    }));
    run()
}

std::thread_local! {
    static DUMP_JSON: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Write a JSON dump to a temp file for debugging.
fn write_dump(json: &str) {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("jj-pr-dump-{}.json", std::process::id()));
    if std::fs::write(&path, json).is_ok() {
        eprintln!("State dumped to: {}", path.display());
    }
}

/// Serialize state as JSON to a writer (shared by `dump` command and error handling).
fn write_state_json(
    jj_entries: &[jj::JjLogEntry],
    prs: &BTreeMap<gh::PrNum, gh::GhPr>,
    default_branch: &str,
    out: &mut impl std::io::Write,
) -> Result<()> {
    let fixture = serde_json::json!({
        "jj_entries": jj_entries,
        "prs": prs.values().collect::<Vec<_>>(),
        "default_branch": default_branch,
    });
    serde_json::to_writer(out, &fixture)?;
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

    // Stash serialized state for panic dump (before build/mutations).
    DUMP_JSON.with(|cell| {
        let mut buf = Vec::new();
        if write_state_json(&jj_entries, &prs, &default_branch, &mut buf).is_ok() {
            *cell.borrow_mut() = String::from_utf8(buf).ok();
        }
    });

    let state = pr_dag::build(&jj_entries, &prs, &default_branch)?;

    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {}));
    let result = match command {
        Command::Show(_args) => pr_dag::render_show(&state, &prs, &mut std::io::stdout()),
        Command::Log(args) => {
            pr_dag::render_log(&state, &prs, &jj_entries, args.all, &mut std::io::stdout())
        }
        Command::Sync(args) => {
            let actions = pr_dag::plan_sync(&state, &prs, &jj_entries, &default_branch)?;
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
            &prs,
            &jj_entries,
            &default_branch,
            &args.bookmark,
            args.title.as_deref(),
            args.body.as_deref(),
        ),
        Command::Dump => {
            write_state_json(&jj_entries, &prs, &default_branch, &mut std::io::stdout())?;
            println!();
            Ok(())
        }
    };

    if result.is_err() {
        // Dump pre-mutation state from thread-local (already serialized).
        DUMP_JSON.with(|cell| {
            if let Some(json) = cell.borrow().as_ref() {
                write_dump(json);
            }
        });
    }
    result
}
