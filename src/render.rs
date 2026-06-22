use crate::model::{CheckContext, MergedPullRequest, PullRequest};
use chrono::{DateTime, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;

pub fn filter(
    prs: Vec<PullRequest>,
    only_failing: bool,
    only_bot: bool,
    no_drafts: bool,
) -> Vec<PullRequest> {
    prs.into_iter()
        .filter(|pr| !(no_drafts && pr.is_draft))
        .filter(|pr| !only_bot || pr.is_bot())
        .filter(|pr| !only_failing || pr.is_failing())
        .collect()
}

fn status_cell(pr: &PullRequest) -> String {
    match pr.rollup() {
        Some(r) => match r.state.as_str() {
            "SUCCESS" => "PASS".green().to_string(),
            "FAILURE" | "ERROR" => "FAIL".red().bold().to_string(),
            "PENDING" => "PENDING".yellow().to_string(),
            "EXPECTED" => "EXPECTED".dimmed().to_string(),
            other => other.to_string(),
        },
        None => "no checks".dimmed().to_string(),
    }
}

fn merge_cell(pr: &PullRequest) -> String {
    match pr.mergeable.as_str() {
        "MERGEABLE" => "ok".green().to_string(),
        "CONFLICTING" => "CONFLICT".red().bold().to_string(),
        "UNKNOWN" => "?".dimmed().to_string(),
        other => other.dimmed().to_string(),
    }
}

fn draft_marker(pr: &PullRequest) -> &'static str {
    if pr.is_draft {
        " (draft)"
    } else {
        ""
    }
}

pub fn print(prs: &[PullRequest]) {
    if prs.is_empty() {
        println!("{}", "No open PRs match the filters.".green());
        return;
    }

    let mut sorted: Vec<&PullRequest> = prs.iter().collect();
    sorted.sort_by(|a, b| {
        b.is_failing()
            .cmp(&a.is_failing())
            .then_with(|| {
                a.repository
                    .name_with_owner
                    .cmp(&b.repository.name_with_owner)
            })
            .then_with(|| a.number.cmp(&b.number))
    });

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "REPO", "PR", "TITLE", "AUTHOR", "STATUS", "MERGE", "AGE",
        ]);

    for pr in &sorted {
        table.add_row(vec![
            Cell::new(sanitize(&pr.repository.name_with_owner)),
            Cell::new(format!("#{}", pr.number)),
            Cell::new(format!("{}{}", truncate(&pr.title, 60), draft_marker(pr))),
            Cell::new(pr.author_login()),
            Cell::new(status_cell(pr)),
            Cell::new(merge_cell(pr)),
            Cell::new(humanize_age(&pr.created_at)),
        ]);
    }

    println!("{table}");

    let failing: Vec<&PullRequest> = sorted.iter().copied().filter(|p| p.is_failing()).collect();
    let conflicts: Vec<&PullRequest> = sorted
        .iter()
        .copied()
        .filter(|p| p.mergeable == "CONFLICTING")
        .collect();

    if !failing.is_empty() {
        println!("\n{}", "Failing checks:".bold().underline());
        for pr in &failing {
            println!(
                "\n  {} {} {}",
                sanitize(&pr.repository.name_with_owner).cyan(),
                format!("#{}", pr.number).cyan(),
                truncate(&pr.title, 80).dimmed()
            );
            if let Some(rollup) = pr.rollup() {
                for ctx in &rollup.contexts.nodes {
                    if let Some(name) = failed_check_name(ctx) {
                        println!("    {} {}", "✗".red(), sanitize(&name));
                    }
                }
            }
            println!("    {}", sanitize(&pr.url).dimmed());
        }
    }

    if !conflicts.is_empty() {
        println!("\n{}", "Merge conflicts:".bold().underline());
        for pr in &conflicts {
            println!(
                "  {} {} {}",
                sanitize(&pr.repository.name_with_owner).cyan(),
                format!("#{}", pr.number).cyan(),
                truncate(&pr.title, 80).dimmed()
            );
        }
    }

    let total = prs.len();
    let bot_count = prs.iter().filter(|p| p.is_bot()).count();
    println!(
        "\n{}",
        format!(
            "{total} open PR(s) — {} failing, {} conflicting, {bot_count} from bots",
            failing.len(),
            conflicts.len()
        )
        .dimmed()
    );
}

/// Render post-merge CI failures: PRs that merged cleanly but whose default-branch
/// workflow (supply-chain scan, docker build+push, etc.) failed. Silent when empty.
pub fn print_post_merge(prs: &[MergedPullRequest]) {
    if prs.is_empty() {
        return;
    }

    let mut sorted: Vec<&MergedPullRequest> = prs.iter().collect();
    sorted.sort_by(|a, b| {
        a.repository
            .name_with_owner
            .cmp(&b.repository.name_with_owner)
            .then_with(|| a.number.cmp(&b.number))
    });

    println!("\n{}", "⚠ Post-merge CI failures:".bold().underline().red());
    for pr in &sorted {
        println!(
            "\n  {} {} {} {}",
            sanitize(&pr.repository.name_with_owner).cyan(),
            format!("#{}", pr.number).cyan(),
            truncate(&pr.title, 60).dimmed(),
            format!("(merged {} ago)", humanize_age(&pr.merged_at)).dimmed()
        );
        if let Some(rollup) = pr.merge_rollup() {
            for ctx in &rollup.contexts.nodes {
                if let Some(name) = failed_check_name(ctx) {
                    println!("    {} {}", "✗".red(), sanitize(&name));
                }
            }
        }
        println!("    {}", sanitize(&pr.url).dimmed());
    }
}

fn failed_check_name(ctx: &CheckContext) -> Option<String> {
    match ctx {
        CheckContext::CheckRun {
            name, conclusion, ..
        } => match conclusion.as_deref() {
            Some("FAILURE")
            | Some("TIMED_OUT")
            | Some("CANCELLED")
            | Some("STARTUP_FAILURE")
            | Some("ACTION_REQUIRED") => Some(name.clone()),
            _ => None,
        },
        CheckContext::StatusContext { context, state, .. } => {
            if state == "FAILURE" || state == "ERROR" {
                Some(context.clone())
            } else {
                None
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() && c != '\t' { '·' } else { c })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    let s = sanitize(s);
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn humanize_age(created_at: &str) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(created_at) else {
        return "?".to_string();
    };
    let diff = Utc::now().signed_duration_since(parsed.with_timezone(&Utc));
    let days = diff.num_days();
    if days >= 1 {
        return format!("{days}d");
    }
    let hours = diff.num_hours();
    if hours >= 1 {
        return format!("{hours}h");
    }
    let minutes = diff.num_minutes().max(0);
    format!("{minutes}m")
}
