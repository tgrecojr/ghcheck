use crate::model::{CheckContext, MergedPullRequest, PullRequest};
use crate::text::sanitize;
use chrono::{DateTime, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;
use std::collections::HashMap;

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
            // A member GitHub adds later reaches this arm as a raw remote
            // string. Encode it here, before any styling, so the cell's own
            // escape sequences stay intact while the remote value cannot
            // contribute any of its own.
            other => sanitize(other),
        },
        None => "no checks".dimmed().to_string(),
    }
}

fn merge_cell(pr: &PullRequest) -> String {
    match pr.mergeable.as_str() {
        "MERGEABLE" => "ok".green().to_string(),
        "CONFLICTING" => "CONFLICT".red().bold().to_string(),
        "UNKNOWN" => "?".dimmed().to_string(),
        // `.dimmed()` wraps the value in escape sequences; it does not encode
        // it. Encode first, then style.
        other => sanitize(other).dimmed().to_string(),
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
        .load_style(UTF8_FULL)
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

/// Reduce merged PRs to the single most-recently-merged PR per repository, then
/// keep only those whose post-merge CI is still failing.
///
/// Each merge to the default branch re-runs the post-merge pipeline (supply-chain
/// scan, docker build+push) over the full rolled-up tree. If the latest merge in a
/// repo is green, any earlier failures in the window are already superseded — their
/// changes were rolled into the latest (passing) build. So only a still-red latest
/// merge warrants attention; reporting the stale failures is just noise.
pub fn latest_failing_per_repo(merged: Vec<MergedPullRequest>) -> Vec<MergedPullRequest> {
    let mut latest: HashMap<String, MergedPullRequest> = HashMap::new();
    for pr in merged {
        match latest.get(&pr.repository.name_with_owner) {
            // `merged_at` is RFC3339 UTC ("…Z"), so lexicographic == chronological order.
            Some(existing) if existing.merged_at >= pr.merged_at => {}
            _ => {
                latest.insert(pr.repository.name_with_owner.clone(), pr);
            }
        }
    }
    latest.into_values().filter(|pr| pr.is_failing()).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Commit, CommitNode, Commits, Contexts, MergedPullRequest, Repository, StatusCheckRollup,
    };

    /// An open PR whose rollup state and mergeable state are attacker-chosen.
    /// Both feed match arms with an `other =>` fallback.
    fn open_pr(mergeable: &str, rollup_state: &str) -> PullRequest {
        PullRequest {
            number: 1,
            title: "t".to_string(),
            url: "https://example.test/o/r/pull/1".to_string(),
            created_at: "2026-06-20T10:00:00Z".to_string(),
            author: None,
            mergeable: mergeable.to_string(),
            is_draft: false,
            repository: Repository {
                name_with_owner: "o/r".to_string(),
            },
            commits: Commits {
                nodes: vec![CommitNode {
                    commit: Commit {
                        status_check_rollup: Some(StatusCheckRollup {
                            state: rollup_state.to_string(),
                            contexts: Contexts { nodes: vec![] },
                        }),
                    },
                }],
            },
        }
    }

    /// A payload the terminal would act on: colour escape, then BEL.
    const HOSTILE: &str = "\u{1b}[31mFORGED-PASS\u{1b}[0m\u{7}";

    /// A benign but unrecognized member — reaches the same fallback arm without
    /// carrying any escape sequence of its own. Any control character the
    /// hostile case emits beyond what this emits is attacker-contributed.
    const BENIGN_UNKNOWN: &str = "SOME_NEW_STATE";

    fn control_chars(s: &str) -> usize {
        s.chars().filter(|c| c.is_control()).count()
    }

    #[test]
    fn status_cell_fallback_arm_encodes_unrecognized_state() {
        // An unrecognized StatusCheckRollup.state (a member GitHub adds later)
        // must not contribute escape sequences of its own.
        let hostile = status_cell(&open_pr("MERGEABLE", HOSTILE));
        let benign = status_cell(&open_pr("MERGEABLE", BENIGN_UNKNOWN));
        assert_eq!(
            control_chars(&hostile),
            control_chars(&benign),
            "remote state contributed control characters: {hostile:?}"
        );
        assert!(!hostile.contains('\u{7}'), "BEL survived: {hostile:?}");
        assert!(
            !hostile.contains("\u{1b}[31m"),
            "attacker colour sequence survived: {hostile:?}"
        );
    }

    #[test]
    fn merge_cell_fallback_arm_encodes_unrecognized_state() {
        // `.dimmed()` wraps the value in the tool's OWN escapes; it does not
        // encode the value. The differential isolates the attacker's bytes from
        // the styling the fix legitimately applies.
        let hostile = merge_cell(&open_pr(HOSTILE, "SUCCESS"));
        let benign = merge_cell(&open_pr(BENIGN_UNKNOWN, "SUCCESS"));
        assert_eq!(
            control_chars(&hostile),
            control_chars(&benign),
            "remote mergeable contributed control characters: {hostile:?}"
        );
        assert!(!hostile.contains('\u{7}'), "BEL survived: {hostile:?}");
        assert!(
            !hostile.contains("\u{1b}[31m"),
            "attacker colour sequence survived: {hostile:?}"
        );
    }

    #[test]
    fn recognized_states_keep_their_styling() {
        // The fix must encode the untrusted value without stripping the tool's
        // own colour codes, which comfy-table's custom_styling renders.
        let pass = status_cell(&open_pr("MERGEABLE", "SUCCESS"));
        assert!(pass.contains("PASS"), "expected PASS text: {pass:?}");
        assert!(
            pass.contains('\u{1b}'),
            "own colour codes were stripped: {pass:?}"
        );
        let ok = merge_cell(&open_pr("MERGEABLE", "SUCCESS"));
        assert!(ok.contains("ok"), "expected ok text: {ok:?}");
        assert!(
            ok.contains('\u{1b}'),
            "own colour codes were stripped: {ok:?}"
        );
    }

    fn merged(repo: &str, number: u32, merged_at: &str, state: &str) -> MergedPullRequest {
        MergedPullRequest {
            number,
            title: format!("PR #{number}"),
            url: format!("https://example.test/{repo}/pull/{number}"),
            merged_at: merged_at.to_string(),
            author: None,
            repository: Repository {
                name_with_owner: repo.to_string(),
            },
            merge_commit: Some(Commit {
                status_check_rollup: Some(StatusCheckRollup {
                    state: state.to_string(),
                    contexts: Contexts { nodes: vec![] },
                }),
            }),
        }
    }

    #[test]
    fn latest_green_supersedes_earlier_failures() {
        // Same repo: an early failure followed by a later passing merge — should be silent.
        let out = latest_failing_per_repo(vec![
            merged("o/r", 1, "2026-06-18T10:00:00Z", "FAILURE"),
            merged("o/r", 2, "2026-06-20T10:00:00Z", "SUCCESS"),
        ]);
        assert!(out.is_empty());
    }

    #[test]
    fn latest_red_is_reported_despite_earlier_pass() {
        let out = latest_failing_per_repo(vec![
            merged("o/r", 1, "2026-06-18T10:00:00Z", "SUCCESS"),
            merged("o/r", 2, "2026-06-20T10:00:00Z", "FAILURE"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].number, 2);
    }

    #[test]
    fn repos_are_independent() {
        // One repo's latest is red, another's latest is green; only the red one surfaces.
        let out = latest_failing_per_repo(vec![
            merged("o/red", 1, "2026-06-20T10:00:00Z", "FAILURE"),
            merged("o/green", 2, "2026-06-19T10:00:00Z", "FAILURE"),
            merged("o/green", 3, "2026-06-21T10:00:00Z", "SUCCESS"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].repository.name_with_owner, "o/red");
    }
}
