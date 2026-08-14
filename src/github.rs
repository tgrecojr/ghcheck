use crate::model::{GraphQLResponse, MergedPullRequest, PullRequest, Search};
use anyhow::{Context, Result};
use std::process::Command;

/// Maximum number of characters of child-process stderr echoed into an error.
const MAX_STDERR_CHARS: usize = 2048;

/// Render child-process stderr for inclusion in an error message.
///
/// `gh`'s stderr is not this process's own text: it can carry whatever an
/// upstream response persuaded `gh` to print. Encode it before it reaches the
/// terminal, and cap it so a child that floods stderr cannot flood the
/// operator's screen.
fn describe_stderr(stderr: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(stderr);
    let encoded = crate::text::sanitize(lossy.trim());
    if encoded.chars().count() <= MAX_STDERR_CHARS {
        return encoded;
    }
    let head: String = encoded.chars().take(MAX_STDERR_CHARS).collect();
    format!("{head}… (truncated)")
}

/// Hard ceiling on records accumulated across pages, so a looping or
/// pathological cursor cannot spin forever.
const MAX_RECORDS: usize = 2000;

/// Walk a paginated search to exhaustion.
///
/// GitHub's search returns a *relevance-ordered page*, not a complete set.
/// Consuming only the first page meant a repository's true latest merge could
/// be absent with nothing to indicate anything had been dropped — and
/// `latest_failing_per_repo`'s correctness depends on observing exactly that
/// record. Paginating removes the possibility rather than reducing its odds.
///
/// A response with no `pageInfo` is treated as a single complete page, so an
/// endpoint that does not return one still works.
fn paginate<T, F>(mut fetch_page: F) -> Result<Vec<T>>
where
    F: FnMut(Option<&str>) -> Result<Search<T>>,
{
    let mut cursor: Option<String> = None;
    let mut all: Vec<T> = Vec::new();

    loop {
        let page = fetch_page(cursor.as_deref())?;
        let info = page.page_info.clone().unwrap_or_default();
        all.extend(page.nodes);

        if !info.has_next_page {
            // GitHub's search caps at 1000 results regardless of paging, so a
            // larger issueCount is a real signal that the answer is partial.
            if let Some(total) = page.issue_count {
                if (all.len() as u32) < total {
                    eprintln!(
                        "⚠ search reported {total} matches but only {} were retrievable; \
                         post-merge status may be incomplete",
                        all.len()
                    );
                }
            }
            return Ok(all);
        }

        // Fail loudly rather than return a quietly short list.
        if all.len() >= MAX_RECORDS {
            anyhow::bail!(
                "more than {MAX_RECORDS} records returned for one query — \
                 narrow the window rather than trusting a truncated result"
            );
        }
        cursor = Some(info.end_cursor.ok_or_else(|| {
            anyhow::anyhow!("search reported another page but returned no end cursor")
        })?);
    }
}

const QUERY_TEMPLATE: &str = r#"
query($q: String!, $cursor: String) {
  search(query: $q, type: ISSUE, first: 100, after: $cursor) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        number
        title
        url
        createdAt
        author { login }
        mergeable
        isDraft
        repository { nameWithOwner }
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup {
                state
                contexts(first: 50) {
                  nodes {
                    __typename
                    ... on CheckRun {
                      name
                      conclusion
                      status
                      detailsUrl
                    }
                    ... on StatusContext {
                      context
                      state
                      targetUrl
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

pub fn current_user() -> Result<String> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("failed to invoke gh — is the GitHub CLI installed and authenticated?")?;

    if !output.status.success() {
        anyhow::bail!("`gh api user` failed: {}", describe_stderr(&output.stderr));
    }

    let login = String::from_utf8(output.stdout)
        .context("gh returned non-utf8 username")?
        .trim()
        .to_string();

    if login.is_empty() {
        anyhow::bail!("gh returned empty username — run `gh auth login`");
    }
    Ok(login)
}

pub fn fetch_prs(owner: &str) -> Result<Vec<PullRequest>> {
    let search = format!("is:pr is:open archived:false user:{owner}");
    paginate(|cursor| search_page::<PullRequest>(QUERY_TEMPLATE, &search, cursor))
}

/// Build the argv for one `gh api graphql` page request.
///
/// Split out from `search_page` so the cursor wiring is testable without
/// spawning a subprocess: dropping the `cursor=` variable would silently
/// re-fetch page one forever, which looks identical to "no more pages".
fn graphql_args(query: &str, search: &str, cursor: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "api".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={query}"),
        "-f".to_string(),
        format!("q={search}"),
    ];
    if let Some(cursor) = cursor {
        args.push("-f".to_string());
        args.push(format!("cursor={cursor}"));
    }
    args
}

/// Run one page of a search query through `gh api graphql`.
fn search_page<T: serde::de::DeserializeOwned>(
    query: &str,
    search: &str,
    cursor: Option<&str>,
) -> Result<Search<T>> {
    let output = Command::new("gh")
        .args(graphql_args(query, search, cursor))
        .output()
        .context("failed to invoke gh api graphql")?;

    if !output.status.success() {
        anyhow::bail!(
            "`gh api graphql` failed: {}",
            describe_stderr(&output.stderr)
        );
    }

    let response: GraphQLResponse<T> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh GraphQL response")?;

    Ok(response.data.search)
}

const MERGED_QUERY_TEMPLATE: &str = r#"
query($q: String!, $cursor: String) {
  search(query: $q, type: ISSUE, first: 100, after: $cursor) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        number
        title
        url
        mergedAt
        author { login }
        repository { nameWithOwner }
        mergeCommit {
          statusCheckRollup {
            state
            contexts(first: 50) {
              nodes {
                __typename
                ... on CheckRun {
                  name
                  conclusion
                  status
                  detailsUrl
                }
                ... on StatusContext {
                  context
                  state
                  targetUrl
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

/// Fetch PRs merged on/after `since` (an `YYYY-MM-DD` date), so we can inspect
/// post-merge CI on the default branch. The merge-commit check suite is the
/// supply-chain scan / docker build+push that runs after the PR lands.
pub fn fetch_merged_prs(owner: &str, since: &str) -> Result<Vec<MergedPullRequest>> {
    let search = format!("is:pr is:merged archived:false user:{owner} merged:>={since}");
    paginate(|cursor| search_page::<MergedPullRequest>(MERGED_QUERY_TEMPLATE, &search, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageInfo;

    fn page(nodes: Vec<u32>, next: Option<&str>, total: Option<u32>) -> Search<u32> {
        Search {
            nodes,
            issue_count: total,
            page_info: Some(PageInfo {
                has_next_page: next.is_some(),
                end_cursor: next.map(|c| c.to_string()),
            }),
        }
    }

    /// The truncation signals must be present in the documents actually sent
    /// to GitHub. The paginate() tests below drive a synthetic closure, so
    /// without this a revert of the query strings — the original defect —
    /// would leave every other test green.
    #[test]
    fn both_query_documents_request_the_truncation_signals() {
        for (name, doc) in [
            ("QUERY_TEMPLATE", QUERY_TEMPLATE),
            ("MERGED_QUERY_TEMPLATE", MERGED_QUERY_TEMPLATE),
        ] {
            assert!(
                doc.contains("issueCount"),
                "{name} does not request issueCount"
            );
            assert!(
                doc.contains("hasNextPage") && doc.contains("endCursor"),
                "{name} does not request pageInfo {{ hasNextPage endCursor }}"
            );
            assert!(
                doc.contains("$cursor: String") && doc.contains("after: $cursor"),
                "{name} does not accept or apply a cursor"
            );
        }
    }

    #[test]
    fn the_cursor_is_passed_through_to_gh() {
        // Dropping the cursor variable would silently re-request page one
        // forever, which is indistinguishable from "no further pages".
        let first = graphql_args("Q", "is:pr", None);
        assert!(
            !first.iter().any(|a| a.starts_with("cursor=")),
            "first page should not send a cursor: {first:?}"
        );

        let next = graphql_args("Q", "is:pr", Some("CURSOR1"));
        assert!(
            next.iter().any(|a| a == "cursor=CURSOR1"),
            "cursor was not forwarded to gh: {next:?}"
        );
        assert_eq!(
            next.iter().filter(|a| *a == "-f").count(),
            3,
            "cursor was not passed as its own -f variable: {next:?}"
        );
    }

    #[test]
    fn paginate_walks_every_page() {
        // GitHub search returns a relevance-ordered PAGE, not a complete set.
        // Stopping after the first one is how a repository's true latest merge
        // goes missing with no signal that anything was dropped.
        let mut seen = 0usize;
        let all = paginate(|cursor| {
            seen += 1;
            Ok(match cursor {
                None => page(vec![1, 2], Some("c1"), Some(5)),
                Some("c1") => page(vec![3, 4], Some("c2"), Some(5)),
                _ => page(vec![5], None, Some(5)),
            })
        })
        .unwrap();
        assert_eq!(all, vec![1, 2, 3, 4, 5], "pagination dropped records");
        assert_eq!(seen, 3, "did not follow the cursor to exhaustion");
    }

    #[test]
    fn paginate_handles_a_response_without_page_info() {
        // An unpaginated response must still deserialize and be treated as one
        // complete page, so a server that omits pageInfo does not break the tool.
        let all = paginate(|_| {
            Ok(Search {
                nodes: vec![7, 8],
                issue_count: None,
                page_info: None,
            })
        })
        .unwrap();
        assert_eq!(all, vec![7, 8]);
    }

    #[test]
    fn paginate_fails_loudly_rather_than_truncating_silently() {
        // A cursor that never terminates must produce a visible error, not a
        // quietly short list. Converting a silently wrong answer into an
        // obviously failed one is the whole point.
        let err = paginate(|_| Ok(page(vec![0u32; 500], Some("forever"), None)))
            .expect_err("runaway pagination was not caught");
        assert!(
            format!("{err}").contains("records"),
            "unhelpful error: {err}"
        );
    }

    #[test]
    fn paginate_reports_a_cursor_that_promises_more_but_gives_none() {
        let err = paginate(|_| {
            Ok(Search {
                nodes: vec![1u32],
                issue_count: None,
                page_info: Some(PageInfo {
                    has_next_page: true,
                    end_cursor: None,
                }),
            })
        })
        .expect_err("missing cursor was not caught");
        assert!(
            format!("{err}").contains("cursor"),
            "unhelpful error: {err}"
        );
    }

    /// A hostile stderr: clear screen, home cursor, green "all is well", then
    /// an OSC window-title set. On a real terminal this paints a reassuring lie
    /// at the moment the fetch has actually failed.
    const HOSTILE: &[u8] =
        b"\x1b[2J\x1b[H\x1b[32mHTTP 200: everything is fine\x1b[0m\x1b]0;pwned\x07";

    #[test]
    fn describe_stderr_encodes_terminal_control_sequences() {
        let out = describe_stderr(HOSTILE);
        assert!(!out.contains('\u{1b}'), "ESC survived: {out:?}");
        assert!(!out.contains('\u{7}'), "BEL survived: {out:?}");
    }

    #[test]
    fn describe_stderr_keeps_the_readable_message() {
        // Encoding must not destroy the diagnostic — the operator still needs
        // to know what gh said.
        assert!(describe_stderr(HOSTILE).contains("HTTP 200: everything is fine"));
        assert_eq!(
            describe_stderr(b"  gh: not authenticated  "),
            "gh: not authenticated"
        );
    }

    #[test]
    fn describe_stderr_caps_pathological_output() {
        // A child process that floods stderr must not flood the terminal.
        let flood = vec![b'A'; 200_000];
        let out = describe_stderr(&flood);
        assert!(
            out.chars().count() <= 4096,
            "output not capped: {} chars",
            out.chars().count()
        );
        assert!(
            out.contains("truncated"),
            "truncation not signalled: {out:?}"
        );
    }

    #[test]
    fn describe_stderr_handles_invalid_utf8() {
        // from_utf8_lossy must not panic on a non-UTF-8 byte stream.
        let out = describe_stderr(&[0xff, 0xfe, b'h', b'i']);
        assert!(out.contains("hi"));
    }
}
