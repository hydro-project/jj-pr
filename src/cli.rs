use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "jj-pr", about = "Sync jj bookmarks with GitHub PRs")]
pub struct Cli {
    /// Skip confirmation prompts
    #[arg(short = 'y', long = "yes", global = true)]
    pub yes: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Display PRs as a graph (default command)
    #[command(alias = "s")]
    Show(ShowArgs),
    /// Display JJ changes and their associated PRs
    Log(LogArgs),
    /// Reconcile local jj state with GitHub (push, update bases, rebase merged children)
    Sync(SyncArgs),
    /// Create a new PR for an existing bookmark
    Create(CreateArgs),
    /// Utility commands
    Util(UtilArgs),
}

#[derive(clap::Args, Clone)]
pub struct ShowArgs {}

#[derive(clap::Args, Clone)]
pub struct LogArgs {
    /// Show JJ changes that are not associated with any PRs
    #[arg(long)]
    pub all: bool,
}

#[derive(clap::Args, Clone)]
pub struct SyncArgs {
    /// Show what would be done without doing it
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(clap::Args, Clone)]
pub struct CreateArgs {
    /// Bookmark name (must already exist)
    pub bookmark: String,

    /// PR title (default: first line of tip commit description)
    #[arg(short, long)]
    pub title: Option<String>,

    /// PR description/body
    #[arg(long)]
    pub body: Option<String>,

    /// Show what would be done without doing it
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(clap::Args, Clone)]
pub struct UtilArgs {
    #[command(subcommand)]
    pub command: UtilCommand,
}

#[derive(Subcommand, Clone)]
pub enum UtilCommand {
    /// Dump raw state (jj + gh) as JSON for test fixtures
    Dump,
    /// Install recommended jj revset aliases for working with PRs
    InstallAliases,
}
