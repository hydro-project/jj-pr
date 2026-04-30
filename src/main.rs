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
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let yes = cli.yes;

    let jj_entries = jj::load_entries()?;
    let prs = gh::load_prs()?
        .into_iter()
        .map(|pr| (pr.number, pr))
        .collect::<BTreeMap<_, _>>();
    let default_branch = gh::default_branch()?;
    let state = pr_dag::build(&jj_entries, &prs, &default_branch)?;

    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {}));
    match command {
        Command::Show(_args) => pr_dag::render_show(&state, &prs, &mut std::io::stdout()),
        Command::Log(args) => pr_dag::render_log(&state, &prs, &jj_entries, args.all, &mut std::io::stdout()),
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
        Command::Create(args) => {
            pr_dag::cmd_create(&state, &prs, &jj_entries, &default_branch, &args.bookmark, args.title.as_deref(), args.body.as_deref())
        }
        Command::Dump => {
            let fixture = serde_json::json!({
                "jj_entries": jj_entries,
                "prs": prs.values().collect::<Vec<_>>(),
                "default_branch": default_branch,
            });
            println!("{}", serde_json::to_string_pretty(&fixture)?);
            Ok(())
        }
    }
}
