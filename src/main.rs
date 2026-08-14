mod github;
mod model;
mod render;
mod text;

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

/// Render a terminating error for display.
///
/// `{err:?}` on an anyhow error prints the whole `Caused by:` chain, and a
/// source error's own `Display` may quote remote data verbatim —
/// `serde_json::Error` does exactly that with the offending value. Every
/// `.context()` message in this crate is a static literal, so the leak enters
/// through the source error rather than through any format argument. Encoding
/// the fully-formatted string is what closes it, and it covers future
/// `.context()` sites for free.
fn render_error(err: &anyhow::Error) -> String {
    text::sanitize(&format!("{err:?}"))
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {}", render_error(&err));
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    /// Build the shape the vulnerability actually takes: a static .context()
    /// message wrapping a source error whose OWN Display quotes remote bytes.
    /// The leak enters through the source, not through the format arguments,
    /// which is why auditing the .context() strings could not find it.
    fn error_with_hostile_source() -> anyhow::Error {
        let hostile = "\u{1b}[2J\u{1b}[H\u{1b}[32mAll checks passed\u{1b}[0m\u{7}";
        let source: Result<(), _> = Err(std::io::Error::other(format!(
            "unknown variant `{hostile}`"
        )));
        source
            .context("failed to parse gh GraphQL response")
            .unwrap_err()
    }

    #[test]
    fn terminating_error_is_encoded() {
        let rendered = render_error(&error_with_hostile_source());
        assert!(
            !rendered.contains('\u{1b}'),
            "ESC reached stderr: {rendered:?}"
        );
        assert!(
            !rendered.contains('\u{7}'),
            "BEL reached stderr: {rendered:?}"
        );
    }

    #[test]
    fn terminating_error_keeps_the_source_chain() {
        // Encoding must not cost the operator the diagnostic: the context
        // message and the underlying cause both still have to be readable.
        let rendered = render_error(&error_with_hostile_source());
        assert!(rendered.contains("failed to parse gh GraphQL response"));
        assert!(rendered.contains("unknown variant"));
    }
}
