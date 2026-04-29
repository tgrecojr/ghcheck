mod github;
mod model;
mod render;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "ghcheck",
    about = "Quick consolidated status of your personal GitHub PRs and CI",
    version
)]
struct Cli {
    /// Show only PRs with failing CI
    #[arg(long)]
    failing: bool,

    /// Show only bot-authored PRs (renovate, dependabot, etc.)
    #[arg(long)]
    bot: bool,

    /// Hide draft PRs
    #[arg(long)]
    no_drafts: bool,

    /// Output JSON instead of a table
    #[arg(long)]
    json: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let owner = github::current_user()?;
    let prs = github::fetch_prs(&owner)?;
    let filtered = render::filter(prs, cli.failing, cli.bot, cli.no_drafts);

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
    } else {
        render::print(&filtered);
    }
    Ok(())
}
