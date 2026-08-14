use crate::model::{CheckContext, MergedPullRequest, PullRequest, Verdict};
use crate::text::sanitize;
use chrono::{DateTime, Utc};
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;
use std::collections::HashSet;

pub fn filter(
    prs: Vec<PullRequest>,
    only_failing: bool,
    only_bot: bool,
    no_drafts: bool,
) -> Vec<PullRequest> {
    prs.into_iter()
        .filter(|pr| !(no_drafts && pr.is_draft))
        .filter(|pr| !only_bot || pr.is_bot())
        // --failing means "not known to be green", not "known to be red". A PR
        // whose verdict is unsettled is exactly the case an attacker can
        // manufacture by pushing a commit, so excluding it would reopen the
        // hole the Verdict tri-state exists to close.
        .filter(|pr| !only_failing || pr.verdict() != Verdict::Passing)
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

/// Build a table cell from untrusted remote data.
///
/// Every column carrying a remote string goes through here, so adding a column
/// without an encoder is not something you can do by forgetting — it is
/// something you would have to do deliberately by calling `Cell::new` instead.
/// Cells holding already-styled local content (`status_cell`, `merge_cell`) are
/// the deliberate exception: they encode their remote input themselves, before
/// applying the tool's own colours.
fn cell(s: impl AsRef<str>) -> Cell {
    Cell::new(sanitize(s.as_ref()))
}

/// Build the open-PR table. Split out from `print` so the rendered output can
/// be asserted on in tests without capturing stdout.
fn build_table(sorted: &[&PullRequest]) -> Table {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "REPO", "PR", "TITLE", "AUTHOR", "STATUS", "MERGE", "AGE",
        ]);

    for pr in sorted {
        table.add_row(vec![
            cell(&pr.repository.name_with_owner),
            cell(format!("#{}", pr.number)),
            cell(format!("{}{}", truncate(&pr.title, 60), draft_marker(pr))),
            cell(pr.author_login()),
            // Pre-styled: these encode their remote input internally so the
            // tool's own colour codes survive. See status_cell / merge_cell.
            Cell::new(status_cell(pr)),
            Cell::new(merge_cell(pr)),
            cell(humanize_age(&pr.created_at)),
        ]);
    }
    table
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

    println!("{}", build_table(&sorted));

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

/// Report, per repository, the newest merge that has a settled verdict — but
/// only when that verdict is a failure.
///
/// Each merge to the default branch re-runs the post-merge pipeline
/// (supply-chain scan, docker build+push) over the full rolled-up tree. A
/// *passing* rebuild is therefore a positive argument that every earlier change
/// in the window is now green, and it supersedes earlier failures.
///
/// Only a passing one. The previous implementation picked each repo's winner by
/// recency and consulted the verdict afterwards, so a newer merge that had not
/// finished CI — or had no merge commit at all — evicted a real FAILURE that
/// then never printed. An unsettled verdict is not evidence of anything, so it
/// no longer supersedes: the scan falls through to the newest merge that does
/// have a verdict. No attacker is needed to hit that case; an ordinary merge
/// landing while CI is queued is enough.
pub fn latest_failing_per_repo(mut merged: Vec<MergedPullRequest>) -> Vec<MergedPullRequest> {
    // Newest first, so the first settled verdict per repo is the deciding one.
    merged.sort_by(|a, b| b.merged_at.cmp(&a.merged_at));

    let mut decided: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for pr in merged {
        if decided.contains(&pr.repository.name_with_owner) {
            continue;
        }
        match pr.verdict() {
            // A green rebuild settles this repo and supersedes what came before.
            Verdict::Passing => {
                decided.insert(pr.repository.name_with_owner.clone());
            }
            // A red rebuild settles this repo and is what we are here to report.
            Verdict::Failing => {
                decided.insert(pr.repository.name_with_owner.clone());
                out.push(pr);
            }
            // No verdict yet: decides nothing, so keep looking further back.
            Verdict::Unknown => {}
        }
    }
    out
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
        CheckContext::CheckRun { name, .. } if ctx.has_failed() => Some(name.clone()),
        CheckContext::StatusContext { context, .. } if ctx.has_failed() => Some(context.clone()),
        _ => None,
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
        Author, Commit, CommitNode, Commits, Contexts, MergedPullRequest, Repository,
        StatusCheckRollup,
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

    /// An open PR with an attacker-chosen author login.
    fn open_pr_authored_by(login: &str) -> PullRequest {
        let mut pr = open_pr("MERGEABLE", "SUCCESS");
        pr.author = Some(Author {
            login: login.to_string(),
        });
        pr
    }

    #[test]
    fn author_column_is_encoded() {
        // GitHub constrains logins to [A-Za-z0-9-], but that is an external
        // guarantee this codebase asserts nowhere. The renderer must not depend
        // on it.
        let pr = open_pr_authored_by(HOSTILE);
        let rendered = build_table(&[&pr]).to_string();
        assert!(
            !rendered.contains("\u{1b}[31mFORGED-PASS"),
            "attacker colour sequence reached the table: {rendered:?}"
        );
        assert!(!rendered.contains('\u{7}'), "BEL reached the table");
    }

    #[test]
    fn every_remote_string_column_is_encoded() {
        // Closes the class rather than the one column that was found: repo
        // name, title and author all carry remote data into the table.
        let mut pr = open_pr_authored_by(HOSTILE);
        pr.title = HOSTILE.to_string();
        pr.repository.name_with_owner = HOSTILE.to_string();
        let rendered = build_table(&[&pr]).to_string();
        assert!(!rendered.contains('\u{7}'), "BEL reached the table");
        assert!(
            !rendered.contains("\u{1b}[31mFORGED-PASS"),
            "attacker colour sequence reached the table: {rendered:?}"
        );
    }

    #[test]
    fn ordinary_author_is_left_intact() {
        let pr = open_pr_authored_by("renovate[bot]");
        assert!(build_table(&[&pr]).to_string().contains("renovate[bot]"));
    }

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

    fn open_pr(number: u32, rollup_state: Option<&str>) -> PullRequest {
        use crate::model::{Commit, CommitNode, Commits};
        PullRequest {
            number,
            title: format!("PR #{number}"),
            url: "u".to_string(),
            created_at: "2026-06-20T10:00:00Z".to_string(),
            author: None,
            mergeable: "MERGEABLE".to_string(),
            is_draft: false,
            repository: Repository {
                name_with_owner: "o/r".to_string(),
            },
            commits: Commits {
                nodes: vec![CommitNode {
                    commit: Commit {
                        status_check_rollup: rollup_state.map(|st| StatusCheckRollup {
                            state: st.to_string(),
                            contexts: Contexts { nodes: vec![] },
                        }),
                    },
                }],
            },
        }
    }

    #[test]
    fn failing_filter_surfaces_every_non_passing_pr() {
        // --failing is the documented triage flag. A PR that is not known to
        // be green must not be silently omitted from it: that omission is
        // exactly how a red PR hides.
        let out = filter(
            vec![
                open_pr(1, None),                   // no verdict at all
                open_pr(2, Some("SOME_NEW_STATE")), // unrecognized member
                open_pr(3, Some("FAILURE")),        // outright failure
                open_pr(4, Some("PENDING")),        // still running
            ],
            true,
            false,
            false,
        );
        assert_eq!(
            out.len(),
            4,
            "non-passing PRs omitted from --failing: kept {:?}",
            out.iter().map(|p| p.number).collect::<Vec<_>>()
        );
    }

    #[test]
    fn failing_filter_still_excludes_passing_prs() {
        let out = filter(vec![open_pr(1, Some("SUCCESS"))], true, false, false);
        assert!(out.is_empty(), "a green PR was surfaced by --failing");
    }

    /// A merged PR whose merge commit carries no rollup at all.
    fn merged_no_rollup(repo: &str, number: u32, merged_at: &str) -> MergedPullRequest {
        let mut m = merged(repo, number, merged_at, "SUCCESS");
        m.merge_commit = Some(Commit {
            status_check_rollup: None,
        });
        m
    }

    /// A merged PR with no merge commit recorded at all.
    fn merged_no_commit(repo: &str, number: u32, merged_at: &str) -> MergedPullRequest {
        let mut m = merged(repo, number, merged_at, "SUCCESS");
        m.merge_commit = None;
        m
    }

    #[test]
    fn a_verdictless_newer_merge_does_not_evict_a_failure() {
        // Supersession is justified only by a PASSING rebuild — that is the
        // argument that the earlier changes are now green. A pending or absent
        // verdict proves nothing and must not consume the evidence.
        for (label, newer) in [
            (
                "null rollup",
                merged_no_rollup("o/r", 2, "2026-08-12T10:00:00Z"),
            ),
            (
                "no merge commit",
                merged_no_commit("o/r", 2, "2026-08-12T10:00:00Z"),
            ),
            (
                "pending",
                merged("o/r", 2, "2026-08-12T10:00:00Z", "PENDING"),
            ),
        ] {
            let out = latest_failing_per_repo(vec![
                merged("o/r", 1, "2026-08-11T10:00:00Z", "FAILURE"),
                newer,
            ]);
            assert_eq!(
                out.len(),
                1,
                "a newer merge with a {label} verdict evicted a real FAILURE"
            );
            assert_eq!(out[0].number, 1);
        }
    }

    #[test]
    fn an_unsettled_merge_falls_through_to_the_newest_settled_one() {
        // Two unsettled merges on top of a failure must still not hide it.
        let out = latest_failing_per_repo(vec![
            merged("o/r", 1, "2026-08-10T10:00:00Z", "FAILURE"),
            merged("o/r", 2, "2026-08-11T10:00:00Z", "PENDING"),
            merged_no_rollup("o/r", 3, "2026-08-12T10:00:00Z"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].number, 1);
    }

    #[test]
    fn an_unsettled_merge_does_not_resurrect_a_superseded_failure() {
        // Ordering matters: FAILURE, then a genuine PASS, then an unsettled
        // merge. The pass already superseded the failure, so nothing reports.
        let out = latest_failing_per_repo(vec![
            merged("o/r", 1, "2026-08-10T10:00:00Z", "FAILURE"),
            merged("o/r", 2, "2026-08-11T10:00:00Z", "SUCCESS"),
            merged_no_rollup("o/r", 3, "2026-08-12T10:00:00Z"),
        ]);
        assert!(
            out.is_empty(),
            "a failure already superseded by a green rebuild was resurrected"
        );
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
