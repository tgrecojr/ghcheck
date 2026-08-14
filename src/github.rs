use crate::model::{GraphQLResponse, MergedPullRequest, PullRequest};
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

const QUERY_TEMPLATE: &str = r#"
query($q: String!) {
  search(query: $q, type: ISSUE, first: 100) {
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
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            &format!("query={QUERY_TEMPLATE}"),
            "-f",
            &format!("q={search}"),
        ])
        .output()
        .context("failed to invoke gh api graphql")?;

    if !output.status.success() {
        anyhow::bail!(
            "`gh api graphql` failed: {}",
            describe_stderr(&output.stderr)
        );
    }

    let response: GraphQLResponse<PullRequest> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh GraphQL response")?;

    Ok(response.data.search.nodes)
}

const MERGED_QUERY_TEMPLATE: &str = r#"
query($q: String!) {
  search(query: $q, type: ISSUE, first: 100) {
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
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            &format!("query={MERGED_QUERY_TEMPLATE}"),
            "-f",
            &format!("q={search}"),
        ])
        .output()
        .context("failed to invoke gh api graphql")?;

    if !output.status.success() {
        anyhow::bail!(
            "`gh api graphql` failed: {}",
            describe_stderr(&output.stderr)
        );
    }

    let response: GraphQLResponse<MergedPullRequest> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh GraphQL response")?;

    Ok(response.data.search.nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

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
