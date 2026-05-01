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

    let command = cli.command.unwrap_or(Command::Show(cli::ShowArgs {}));

    // Handle subcommands that don't need jj/gh data.
    if let Command::Util(util_args) = &command
        && let cli::UtilCommand::InstallAliases = &util_args.command
    {
        return install_aliases();
    }

    let jj_entries = jj::load_entries()?;
    let prs = gh::load_prs()?;
    let default_branch = gh::default_branch()?;

    // Store input data globally for panic/error dump.
    let input = INPUT_DATA.get_or_init(|| InputData {
        jj_entries,
        prs,
        default_branch,
    });

    // Handle util subcommands that need raw input.
    if let Command::Util(util_args) = &command {
        return match &util_args.command {
            cli::UtilCommand::Dump => {
                serde_json::to_writer(std::io::stdout(), input)?;
                println!();
                Ok(())
            }
            cli::UtilCommand::InstallAliases => unreachable!("handled above"),
        };
    }

    let prs = input.prs_map();
    let state = pr_dag::build(&input.jj_entries, &prs, &input.default_branch)?;

    let result = match command {
        Command::Show(_args) => pr_dag::render_show(&state, &prs, &mut std::io::stdout()),
        Command::Log(args) => pr_dag::render_log(&state, &prs, &input.jj_entries, args.all, &mut std::io::stdout()),
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
        Command::Util(_) => unreachable!("handled above"),
    };

    if result.is_err() {
        dump_input_on_failure(INPUT_DATA.get());
    }
    result
}

/// Install jj aliases so `jj pr` invokes `jj-pr`.
fn install_aliases() -> Result<()> {
    let config_path = {
        let output = std::process::Command::new("jj")
            .args(["config", "path", "--user"])
            .output()
            .context("failed to run `jj config path --user`")?;
        anyhow::ensure!(output.status.success(), "jj config path --user failed");
        String::from_utf8(output.stdout)?.trim().to_owned()
    };

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();

    // Check if alias already exists.
    if existing.contains("[aliases]") {
        // Parse to check if `pr` is already defined.
        let table: toml::Value = toml::from_str(&existing).with_context(|| format!("failed to parse {config_path}"))?;
        if table.get("aliases").and_then(|a| a.get("pr")).is_some() {
            eprintln!("Alias `pr` already exists in {config_path}");
            return Ok(());
        }
    }

    // Append the alias.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_path)
        .with_context(|| format!("failed to open {config_path}"))?;

    // Add a blank line separator if file is non-empty and doesn't end with newline.
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }

    writeln!(file, "[aliases]")?;
    writeln!(file, r#"pr = ["util", "--", "exec", "--", "jj-pr"]"#)?;

    eprintln!("Installed alias `jj pr` -> `jj-pr` in {config_path}");
    Ok(())
}
