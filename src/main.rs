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

/// Encode every string in a serialized value tree.
///
/// Intercepting once, at the boundary, covers every field that is serialized
/// today and every field added later — including the three that table mode
/// never renders (`CheckRun.status`, `CheckRun.detailsUrl`,
/// `StatusContext.targetUrl`, the last two being attacker-supplied URLs).
/// Object keys are untouched: they come from the `json!` literal and from
/// `derive(Serialize)` field names, not from remote data.
fn sanitize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = text::sanitize(s),
        serde_json::Value::Array(items) => items.iter_mut().for_each(sanitize_json),
        serde_json::Value::Object(map) => map.values_mut().for_each(sanitize_json),
        _ => {}
    }
}

/// Serialize the `--json` payload. Split out from `run` so the exact bytes the
/// tool emits can be asserted on in tests.
///
/// `serde_json`'s escape table covers `0x00-0x1F`, `"` and `\` only. DEL, the
/// whole C1 range and category Cf pass through it untouched, which made
/// `--json` a strictly weaker egress path than the table. Encoding here keeps
/// the two at parity.
fn json_output(
    open: &[model::PullRequest],
    post_merge_failures: &[model::MergedPullRequest],
) -> Result<String> {
    let mut out = serde_json::json!({
        "open": open,
        "post_merge_failures": post_merge_failures,
    });
    sanitize_json(&mut out);
    Ok(serde_json::to_string_pretty(&out)?)
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
        println!("{}", json_output(&filtered, &post_merge_failures)?);
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

    /// Characters serde_json's escape table does NOT cover: it escapes
    /// 0x00-0x1F, quote and backslash only. DEL, the C1 range (U+009B CSI is a
    /// single-byte Control Sequence Introducer many terminals honour like
    /// ESC-[) and category Cf all pass through raw.
    const JSON_PROBES: &[(&str, char)] = &[
        ("U+009B CSI", '\u{9b}'),
        ("U+0085 NEL", '\u{85}'),
        ("U+007F DEL", '\u{7f}'),
        ("U+202E RLO", '\u{202e}'),
    ];

    fn pr_titled(title: &str) -> model::PullRequest {
        model::PullRequest {
            number: 1,
            title: title.to_string(),
            url: "https://example.test/o/r/pull/1".to_string(),
            created_at: "2026-06-20T10:00:00Z".to_string(),
            author: Some(model::Author {
                login: title.to_string(),
            }),
            mergeable: "MERGEABLE".to_string(),
            is_draft: false,
            repository: model::Repository {
                name_with_owner: "o/r".to_string(),
            },
            commits: model::Commits { nodes: vec![] },
        }
    }

    #[test]
    fn json_output_is_encoded() {
        let hostile: String = JSON_PROBES.iter().map(|(_, c)| *c).collect();
        let out = json_output(&[pr_titled(&hostile)], &[]).unwrap();
        for (name, ch) in JSON_PROBES {
            assert!(
                !out.contains(*ch),
                "{name} reached the --json output raw: {out:?}"
            );
        }
    }

    #[test]
    fn json_output_stays_valid_json() {
        // Encoding must not corrupt the document consumers parse.
        let hostile: String = JSON_PROBES.iter().map(|(_, c)| *c).collect();
        let out = json_output(&[pr_titled(&hostile)], &[]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(parsed.get("open").is_some());
        assert!(parsed.get("post_merge_failures").is_some());
    }

    #[test]
    fn json_output_preserves_ordinary_values() {
        let out = json_output(&[pr_titled("Bump lodash to 4.17.21")], &[]).unwrap();
        assert!(out.contains("Bump lodash to 4.17.21"));
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
