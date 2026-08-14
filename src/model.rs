use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct GraphQLResponse<T> {
    pub data: Data<T>,
}

#[derive(Debug, Deserialize)]
pub struct Data<T> {
    pub search: Search<T>,
}

#[derive(Debug, Deserialize)]
pub struct Search<T> {
    pub nodes: Vec<T>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PullRequest {
    pub number: u32,
    pub title: String,
    pub url: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub author: Option<Author>,
    pub mergeable: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    pub repository: Repository,
    pub commits: Commits,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Author {
    pub login: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Repository {
    #[serde(rename = "nameWithOwner")]
    pub name_with_owner: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Commits {
    pub nodes: Vec<CommitNode>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CommitNode {
    pub commit: Commit,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Commit {
    #[serde(rename = "statusCheckRollup")]
    pub status_check_rollup: Option<StatusCheckRollup>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StatusCheckRollup {
    pub state: String,
    pub contexts: Contexts,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Contexts {
    pub nodes: Vec<CheckContext>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "__typename")]
pub enum CheckContext {
    CheckRun {
        name: String,
        conclusion: Option<String>,
        status: String,
        #[serde(rename = "detailsUrl")]
        details_url: Option<String>,
    },
    StatusContext {
        context: String,
        state: String,
        #[serde(rename = "targetUrl")]
        target_url: Option<String>,
    },
}

/// A recently-merged PR, used to check post-merge CI on the default branch.
///
/// The post-merge supply-chain scan / docker build+push workflows are triggered
/// by the push to the default branch, so their check suite attaches to the merge
/// commit — not the PR head commit. `merge_commit.status_check_rollup` is that
/// post-merge status.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MergedPullRequest {
    pub number: u32,
    pub title: String,
    pub url: String,
    #[serde(rename = "mergedAt")]
    pub merged_at: String,
    pub author: Option<Author>,
    pub repository: Repository,
    #[serde(rename = "mergeCommit")]
    pub merge_commit: Option<Commit>,
}

/// The CI verdict for a pull request or a merge commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// CI ran and passed.
    Passing,
    /// CI ran and failed.
    Failing,
    /// No verdict yet — still running, not started, absent, or a state this
    /// build does not recognize. Deliberately distinct from `Passing`.
    Unknown,
}

impl Verdict {
    /// Combine two verdicts, keeping the more severe: `Failing` beats
    /// `Unknown` beats `Passing`. Used to reduce over several commits or
    /// checks without letting one green result mask a red one.
    fn worst(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::Failing, _) | (_, Verdict::Failing) => Verdict::Failing,
            (Verdict::Unknown, _) | (_, Verdict::Unknown) => Verdict::Unknown,
            _ => Verdict::Passing,
        }
    }
}

/// Whether a pull request can be merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeability {
    /// GitHub has determined the PR merges cleanly.
    Clean,
    /// GitHub has determined the PR conflicts.
    Conflicting,
    /// Mergeability has not been determined — still being calculated, or a
    /// state this build does not recognize. Deliberately distinct from `Clean`.
    Unknown,
}

impl CheckContext {
    /// Whether this individual check has already concluded in failure.
    ///
    /// The conclusion set is wider than the rollup's aggregate state, and it
    /// is the authoritative one: a rollup can still read `PENDING` overall
    /// while one of its checks has already given up.
    pub fn has_failed(&self) -> bool {
        match self {
            CheckContext::CheckRun { conclusion, .. } => matches!(
                conclusion.as_deref(),
                Some("FAILURE")
                    | Some("TIMED_OUT")
                    | Some("CANCELLED")
                    | Some("STARTUP_FAILURE")
                    | Some("ACTION_REQUIRED")
            ),
            CheckContext::StatusContext { state, .. } => state == "FAILURE" || state == "ERROR",
        }
    }
}

/// Reduce one check-suite rollup to a verdict.
///
/// Every state that is not explicitly `SUCCESS`, `FAILURE` or `ERROR` — and an
/// absent rollup — is `Unknown` rather than passing. The domain is genuinely
/// tri-state; collapsing it to a boolean is what let "no verdict" be read as
/// "green".
fn rollup_verdict(rollup: Option<&StatusCheckRollup>) -> Verdict {
    let Some(rollup) = rollup else {
        return Verdict::Unknown;
    };
    if rollup.contexts.nodes.iter().any(CheckContext::has_failed) {
        return Verdict::Failing;
    }
    match rollup.state.as_str() {
        "SUCCESS" => Verdict::Passing,
        "FAILURE" | "ERROR" => Verdict::Failing,
        _ => Verdict::Unknown,
    }
}

impl MergedPullRequest {
    pub fn merge_rollup(&self) -> Option<&StatusCheckRollup> {
        self.merge_commit
            .as_ref()
            .and_then(|c| c.status_check_rollup.as_ref())
    }

    pub fn verdict(&self) -> Verdict {
        rollup_verdict(self.merge_rollup())
    }
}

impl PullRequest {
    pub fn author_login(&self) -> &str {
        self.author
            .as_ref()
            .map(|a| a.login.as_str())
            .unwrap_or("?")
    }

    pub fn rollup(&self) -> Option<&StatusCheckRollup> {
        self.commits
            .nodes
            .first()
            .and_then(|n| n.commit.status_check_rollup.as_ref())
    }

    /// The verdict across every commit node the API actually returned.
    ///
    /// `rollup()` reads only `commits.nodes.first()`, and the query asks for
    /// `commits(last: 1)` — a coupling nothing in the type system enforces.
    /// Reading a single node is what let a pushed commit with no checks yet
    /// hide a real `FAILURE` on the commit behind it: anyone able to push to
    /// the PR branch could reset the verdict on demand. Reducing over every
    /// node returned, worst-first, removes that lever.
    pub fn verdict(&self) -> Verdict {
        self.commits
            .nodes
            .iter()
            .map(|node| rollup_verdict(node.commit.status_check_rollup.as_ref()))
            .reduce(Verdict::worst)
            .unwrap_or(Verdict::Unknown)
    }

    pub fn is_failing(&self) -> bool {
        matches!(self.verdict(), Verdict::Failing)
    }

    /// Mergeability as a tri-state.
    ///
    /// `UNKNOWN` means GitHub is still calculating, not that the PR merges
    /// cleanly, and nothing here re-queries — so folding it into "clean" made
    /// an undetermined PR read as fine indefinitely. Unrecognized future
    /// members resolve the same way rather than defaulting to clean.
    pub fn mergeability(&self) -> Mergeability {
        match self.mergeable.as_str() {
            "MERGEABLE" => Mergeability::Clean,
            "CONFLICTING" => Mergeability::Conflicting,
            _ => Mergeability::Unknown,
        }
    }

    pub fn is_bot(&self) -> bool {
        let login = self.author_login();
        login.ends_with("[bot]")
            || login.eq_ignore_ascii_case("renovate")
            || login.eq_ignore_ascii_case("dependabot")
            || login.eq_ignore_ascii_case("mend")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollup(state: &str) -> Option<StatusCheckRollup> {
        Some(StatusCheckRollup {
            state: state.to_string(),
            contexts: Contexts { nodes: vec![] },
        })
    }

    fn pr_with_commits(rollups: Vec<Option<StatusCheckRollup>>) -> PullRequest {
        PullRequest {
            number: 1,
            title: "t".to_string(),
            url: "u".to_string(),
            created_at: "2026-06-20T10:00:00Z".to_string(),
            author: None,
            mergeable: "MERGEABLE".to_string(),
            is_draft: false,
            repository: Repository {
                name_with_owner: "o/r".to_string(),
            },
            commits: Commits {
                nodes: rollups
                    .into_iter()
                    .map(|r| CommitNode {
                        commit: Commit {
                            status_check_rollup: r,
                        },
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn a_failure_on_any_returned_commit_is_reported() {
        // The query asks for commits(last: 1) and rollup() reads .first() — an
        // invariant nothing enforces. When a pushed commit carries no checks
        // yet, reading only one node let a real FAILURE go unseen. Whatever
        // nodes GitHub returns, a FAILURE among them must surface.
        let pr = pr_with_commits(vec![None, rollup("FAILURE")]);
        assert!(
            pr.is_failing(),
            "FAILURE on a returned commit was not reported"
        );
    }

    #[test]
    fn a_null_rollup_is_not_treated_as_passing() {
        let pr = pr_with_commits(vec![None]);
        assert_ne!(
            pr.verdict(),
            Verdict::Passing,
            "absent verdict aliased onto passing"
        );
    }

    #[test]
    fn an_unrecognized_state_is_not_treated_as_passing() {
        // A member GitHub adds later must not default to green.
        let pr = pr_with_commits(vec![rollup("SOME_NEW_STATE")]);
        assert_ne!(pr.verdict(), Verdict::Passing);
    }

    #[test]
    fn pending_is_unknown_not_passing_and_not_failing() {
        let pr = pr_with_commits(vec![rollup("PENDING")]);
        assert_eq!(pr.verdict(), Verdict::Unknown);
        assert!(
            !pr.is_failing(),
            "PENDING must not be reported as a failure"
        );
    }

    fn pr_mergeable(state: &str) -> PullRequest {
        let mut pr = pr_with_commits(vec![rollup("SUCCESS")]);
        pr.mergeable = state.to_string();
        pr
    }

    #[test]
    fn unknown_mergeability_is_not_treated_as_clean() {
        // MergeableState::UNKNOWN means "still being calculated", not "merges
        // cleanly", and this codebase never re-queries — so an UNKNOWN observed
        // once would otherwise read as clean forever.
        assert_eq!(
            pr_mergeable("UNKNOWN").mergeability(),
            Mergeability::Unknown
        );
    }

    #[test]
    fn an_unrecognized_mergeable_state_is_not_treated_as_clean() {
        assert_eq!(
            pr_mergeable("SOME_NEW_STATE").mergeability(),
            Mergeability::Unknown
        );
    }

    #[test]
    fn known_mergeable_states_map_directly() {
        assert_eq!(
            pr_mergeable("MERGEABLE").mergeability(),
            Mergeability::Clean
        );
        assert_eq!(
            pr_mergeable("CONFLICTING").mergeability(),
            Mergeability::Conflicting
        );
    }

    #[test]
    fn success_is_passing() {
        assert_eq!(
            pr_with_commits(vec![rollup("SUCCESS")]).verdict(),
            Verdict::Passing
        );
    }

    #[test]
    fn a_concluded_failed_context_outranks_a_non_failure_aggregate() {
        // A rollup whose aggregate is not FAILURE but which contains a context
        // that has already concluded failed is Failing, not Unknown.
        let pr = PullRequest {
            commits: Commits {
                nodes: vec![CommitNode {
                    commit: Commit {
                        status_check_rollup: Some(StatusCheckRollup {
                            state: "PENDING".to_string(),
                            contexts: Contexts {
                                nodes: vec![CheckContext::CheckRun {
                                    name: "build".to_string(),
                                    conclusion: Some("TIMED_OUT".to_string()),
                                    status: "COMPLETED".to_string(),
                                    details_url: None,
                                }],
                            },
                        }),
                    },
                }],
            },
            ..pr_with_commits(vec![])
        };
        assert_eq!(pr.verdict(), Verdict::Failing);
    }

    #[test]
    fn merged_pr_null_rollup_is_not_passing() {
        let m = MergedPullRequest {
            number: 1,
            title: "t".to_string(),
            url: "u".to_string(),
            merged_at: "2026-06-20T10:00:00Z".to_string(),
            author: None,
            repository: Repository {
                name_with_owner: "o/r".to_string(),
            },
            merge_commit: None,
        };
        assert_eq!(m.verdict(), Verdict::Unknown);
    }
}
