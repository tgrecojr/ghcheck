use crate::model::{GraphQLResponse, MergedPullRequest, PullRequest};
use anyhow::{Context, Result};
use std::process::Command;

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
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`gh api user` failed: {}", stderr.trim());
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`gh api graphql` failed: {}", stderr.trim());
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`gh api graphql` failed: {}", stderr.trim());
    }

    let response: GraphQLResponse<MergedPullRequest> =
        serde_json::from_slice(&output.stdout).context("failed to parse gh GraphQL response")?;

    Ok(response.data.search.nodes)
}
