use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct GraphQLResponse {
    pub data: Data,
}

#[derive(Debug, Deserialize)]
pub struct Data {
    pub search: Search,
}

#[derive(Debug, Deserialize)]
pub struct Search {
    pub nodes: Vec<PullRequest>,
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

    pub fn is_failing(&self) -> bool {
        matches!(
            self.rollup().map(|r| r.state.as_str()),
            Some("FAILURE") | Some("ERROR")
        )
    }

    pub fn is_bot(&self) -> bool {
        let login = self.author_login();
        login.ends_with("[bot]")
            || login.eq_ignore_ascii_case("renovate")
            || login.eq_ignore_ascii_case("dependabot")
            || login.eq_ignore_ascii_case("mend")
    }
}
