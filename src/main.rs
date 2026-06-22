mod github;
mod model;
mod render;

use anyhow::Result;
use chrono::{Duration, Utc};
use clap::Parser;

/// How far back to look for merged PRs when checking post-merge CI.
const POST_MERGE_LOOKBACK_DAYS: i64 = 7;

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

    let since = (Utc::now() - Duration::days(POST_MERGE_LOOKBACK_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let merged = github::fetch_merged_prs(&owner, &since)?;
    let post_merge_failures = render::latest_failing_per_repo(merged);

    if cli.json {
        let out = serde_json::json!({
            "open": filtered,
            "post_merge_failures": post_merge_failures,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        render::print(&filtered);
        render::print_post_merge(&post_merge_failures);
    }
    Ok(())
}
