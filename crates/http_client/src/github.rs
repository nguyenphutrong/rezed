use crate::{AsyncBody, HttpClient, HttpRequestExt};
use anyhow::{Context as _, Result, anyhow, bail};
pub use cloud_api_types::{GitHubActivityItem, GitHubActivityKind, GitHubActivitySyncBatch};
use futures::AsyncReadExt;
use http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;

const GITHUB_API_URL: &str = "https://api.github.com";
const GITHUB_ACTIVITY_PER_PAGE: usize = 100;

pub struct GitHubLspBinaryVersion {
    pub name: String,
    pub url: String,
    pub digest: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct GithubRelease {
    pub tag_name: String,
    #[serde(rename = "prerelease")]
    pub pre_release: bool,
    pub assets: Vec<GithubReleaseAsset>,
    pub tarball_url: String,
    pub zipball_url: String,
}

#[derive(Deserialize, Debug)]
pub struct GithubReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubRepositoryActivity {
    pub repository_name_with_owner: String,
    pub issues: Vec<GitHubIssue>,
    pub pull_requests: Vec<GitHubPullRequest>,
    pub workflow_runs: Vec<GitHubWorkflowRun>,
}

impl GitHubRepositoryActivity {
    pub fn to_sync_batch(&self) -> GitHubActivitySyncBatch {
        GitHubActivitySyncBatch {
            repository_name_with_owner: self.repository_name_with_owner.clone(),
            items: self.to_activity_items(),
        }
    }

    pub fn to_activity_items(&self) -> Vec<GitHubActivityItem> {
        let mut items: Vec<_> = self
            .issues
            .iter()
            .map(|issue| GitHubActivityItem {
                kind: GitHubActivityKind::Issue,
                source_id: format!(
                    "github:{}:issue:{}",
                    self.repository_name_with_owner, issue.number
                ),
                repository_name_with_owner: self.repository_name_with_owner.clone(),
                title: issue.title.clone(),
                body: issue.body.clone(),
                author_login: Some(issue.user.login.clone()),
                labels: label_names(&issue.labels),
                url: issue.html_url.clone(),
                number: Some(issue.number),
                state: Some(issue.state.clone()),
                draft: None,
                updated_at: issue.updated_at.clone(),
                workflow_run_id: None,
                workflow_status: None,
                workflow_conclusion: None,
                workflow_event: None,
                workflow_head_branch: None,
                workflow_head_sha: None,
            })
            .chain(
                self.pull_requests
                    .iter()
                    .map(|pull_request| GitHubActivityItem {
                        kind: GitHubActivityKind::PullRequest,
                        source_id: format!(
                            "github:{}:pull_request:{}",
                            self.repository_name_with_owner, pull_request.number
                        ),
                        repository_name_with_owner: self.repository_name_with_owner.clone(),
                        title: pull_request.title.clone(),
                        body: pull_request.body.clone(),
                        author_login: Some(pull_request.user.login.clone()),
                        labels: label_names(&pull_request.labels),
                        url: pull_request.html_url.clone(),
                        number: Some(pull_request.number),
                        state: Some(pull_request.state.clone()),
                        draft: Some(pull_request.draft),
                        updated_at: pull_request.updated_at.clone(),
                        workflow_run_id: None,
                        workflow_status: None,
                        workflow_conclusion: None,
                        workflow_event: None,
                        workflow_head_branch: None,
                        workflow_head_sha: None,
                    }),
            )
            .chain(self.workflow_runs.iter().map(|run| {
                GitHubActivityItem {
                    kind: GitHubActivityKind::WorkflowRun,
                    source_id: format!(
                        "github:{}:workflow_run:{}",
                        self.repository_name_with_owner, run.id
                    ),
                    repository_name_with_owner: self.repository_name_with_owner.clone(),
                    title: run
                        .name
                        .clone()
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| "Workflow run".to_string()),
                    body: None,
                    author_login: run.actor.as_ref().map(|actor| actor.login.clone()),
                    labels: Vec::new(),
                    url: run.html_url.clone(),
                    number: None,
                    state: None,
                    draft: None,
                    updated_at: run.updated_at.clone(),
                    workflow_run_id: Some(run.id),
                    workflow_status: run.status.clone(),
                    workflow_conclusion: run.conclusion.clone(),
                    workflow_event: Some(run.event.clone()),
                    workflow_head_branch: run.head_branch.clone(),
                    workflow_head_sha: run.head_sha.clone(),
                }
            }))
            .collect();

        items.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.source_id.cmp(&right.source_id))
        });
        items
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub state: String,
    pub user: GitHubUser,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubPullRequest {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub state: String,
    pub user: GitHubUser,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubWorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    pub html_url: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub head_branch: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub actor: Option<GitHubUser>,
    #[serde(default)]
    pub updated_at: Option<String>,
    pub event: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubLabel {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubUser {
    pub login: String,
}

fn label_names(labels: &[GitHubLabel]) -> Vec<String> {
    labels.iter().map(|label| label.name.clone()).collect()
}

#[derive(Deserialize)]
struct GitHubIssueItem {
    number: u64,
    title: String,
    html_url: String,
    state: String,
    user: GitHubUser,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    labels: Vec<GitHubLabel>,
    #[serde(default)]
    body: Option<String>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GitHubWorkflowRunsResponse {
    #[serde(default)]
    workflow_runs: Vec<GitHubWorkflowRun>,
}

pub async fn repository_activity(
    repo_name_with_owner: &str,
    token: Option<&str>,
    http: Arc<dyn HttpClient>,
) -> anyhow::Result<GitHubRepositoryActivity> {
    let issues_url = format!(
        "{GITHUB_API_URL}/repos/{repo_name_with_owner}/issues?state=all&per_page={GITHUB_ACTIVITY_PER_PAGE}&sort=updated&direction=desc"
    );
    let pulls_url = format!(
        "{GITHUB_API_URL}/repos/{repo_name_with_owner}/pulls?state=all&per_page={GITHUB_ACTIVITY_PER_PAGE}&sort=updated&direction=desc"
    );
    let workflow_runs_url = format!(
        "{GITHUB_API_URL}/repos/{repo_name_with_owner}/actions/runs?per_page={GITHUB_ACTIVITY_PER_PAGE}&exclude_pull_requests=false"
    );

    let issue_items: Vec<GitHubIssueItem> =
        get_github_paginated_json(http.clone(), &issues_url, token).await?;
    let pull_requests = get_github_paginated_json(http.clone(), &pulls_url, token).await?;
    let mut workflow_runs = Vec::new();
    let mut next_url = Some(workflow_runs_url);
    while let Some(url) = next_url {
        let (response, next) =
            get_github_json_page::<GitHubWorkflowRunsResponse>(http.clone(), &url, token).await?;
        workflow_runs.extend(response.workflow_runs);
        next_url = next;
    }

    Ok(GitHubRepositoryActivity {
        repository_name_with_owner: repo_name_with_owner.to_string(),
        issues: issue_items
            .into_iter()
            .filter(|issue| issue.pull_request.is_none())
            .map(|issue| GitHubIssue {
                number: issue.number,
                title: issue.title,
                html_url: issue.html_url,
                state: issue.state,
                user: issue.user,
                updated_at: issue.updated_at,
                labels: issue.labels,
                body: issue.body,
            })
            .collect(),
        pull_requests,
        workflow_runs,
    })
}

async fn get_github_paginated_json<T: for<'de> Deserialize<'de>>(
    http: Arc<dyn HttpClient>,
    url: &str,
    token: Option<&str>,
) -> anyhow::Result<Vec<T>> {
    let mut items = Vec::new();
    let mut next_url = Some(url.to_string());
    while let Some(url) = next_url {
        let (mut page, next) = get_github_json_page::<Vec<T>>(http.clone(), &url, token).await?;
        items.append(&mut page);
        next_url = next;
    }
    Ok(items)
}

async fn get_github_json_page<T: for<'de> Deserialize<'de>>(
    http: Arc<dyn HttpClient>,
    url: &str,
    token: Option<&str>,
) -> anyhow::Result<(T, Option<String>)> {
    let request = Request::get(url)
        .follow_redirects(crate::RedirectPolicy::FollowAll)
        .header("User-Agent", "Rezed")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .when_some(token, |builder, token| {
            builder.header("Authorization", format!("Bearer {}", token))
        })
        .body(AsyncBody::default())?;

    let mut response = http.send(request).await?;
    let status = response.status();
    let next_url = response
        .headers()
        .get("link")
        .and_then(|link| link.to_str().ok())
        .and_then(github_next_link);
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("error reading GitHub API response")?;

    if !status.is_success() {
        return Err(github_status_error(status, &body));
    }

    let value = serde_json::from_slice(&body).map_err(|err| {
        log::error!("Error deserializing GitHub API response: {err:?}");
        log::error!(
            "GitHub API response text: {:?}",
            String::from_utf8_lossy(body.as_slice())
        );
        anyhow!("error deserializing GitHub API response: {err:?}")
    })?;
    Ok((value, next_url))
}

fn github_next_link(link_header: &str) -> Option<String> {
    link_header.split(',').find_map(|link| {
        let (url, params) = link.split_once(';')?;
        let is_next = params
            .split(';')
            .any(|param| param.trim() == r#"rel="next""#);
        if !is_next {
            return None;
        }
        Some(url.trim().strip_prefix('<')?.strip_suffix('>')?.to_string())
    })
}

fn github_status_error(status: StatusCode, body: &[u8]) -> anyhow::Error {
    let text = String::from_utf8_lossy(body);
    anyhow!(
        "GitHub returned status {}, response: {text:?}",
        status.as_u16()
    )
}

pub async fn latest_github_release(
    repo_name_with_owner: &str,
    require_assets: bool,
    pre_release: bool,
    http: Arc<dyn HttpClient>,
) -> anyhow::Result<GithubRelease> {
    let url = format!("{GITHUB_API_URL}/repos/{repo_name_with_owner}/releases");

    let request = Request::get(&url)
        .follow_redirects(crate::RedirectPolicy::FollowAll)
        .when_some(std::env::var("GITHUB_TOKEN").ok(), |builder, token| {
            builder.header("Authorization", format!("Bearer {}", token))
        })
        .body(Default::default())?;

    let mut response = http
        .send(request)
        .await
        .context("error fetching latest release")?;

    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("error reading latest release")?;

    if response.status().is_client_error() {
        let text = String::from_utf8_lossy(body.as_slice());
        bail!(
            "status error {}, response: {text:?}",
            response.status().as_u16()
        );
    }

    let releases = match serde_json::from_slice::<Vec<GithubRelease>>(body.as_slice()) {
        Ok(releases) => releases,

        Err(err) => {
            log::error!("Error deserializing: {err:?}");
            log::error!(
                "GitHub API response text: {:?}",
                String::from_utf8_lossy(body.as_slice())
            );
            anyhow::bail!("error deserializing latest release: {err:?}");
        }
    };

    let mut release = releases
        .into_iter()
        .filter(|release| !require_assets || !release.assets.is_empty())
        .find(|release| release.pre_release == pre_release)
        .context("finding a prerelease")?;
    release.assets.iter_mut().for_each(|asset| {
        if let Some(digest) = &mut asset.digest
            && let Some(stripped) = digest.strip_prefix("sha256:")
        {
            *digest = stripped.to_owned();
        }
    });
    Ok(release)
}

pub async fn get_release_by_tag_name(
    repo_name_with_owner: &str,
    tag: &str,
    http: Arc<dyn HttpClient>,
) -> anyhow::Result<GithubRelease> {
    let url = format!("{GITHUB_API_URL}/repos/{repo_name_with_owner}/releases/tags/{tag}");

    let request = Request::get(&url)
        .follow_redirects(crate::RedirectPolicy::FollowAll)
        .when_some(std::env::var("GITHUB_TOKEN").ok(), |builder, token| {
            builder.header("Authorization", format!("Bearer {}", token))
        })
        .body(Default::default())?;

    let mut response = http
        .send(request)
        .await
        .context("error fetching latest release")?;

    let mut body = Vec::new();
    let status = response.status();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("error reading latest release")?;

    if status.is_client_error() {
        let text = String::from_utf8_lossy(body.as_slice());
        bail!(
            "status error {}, response: {text:?}",
            response.status().as_u16()
        );
    }

    let release = serde_json::from_slice::<GithubRelease>(body.as_slice()).map_err(|err| {
        log::error!("Error deserializing: {err:?}");
        log::error!(
            "GitHub API response text: {:?}",
            String::from_utf8_lossy(body.as_slice())
        );
        anyhow!("error deserializing GitHub release: {err:?}")
    })?;

    Ok(release)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AssetKind {
    TarGz,
    TarBz2,
    Gz,
    Zip,
}

pub fn build_asset_url(repo_name_with_owner: &str, tag: &str, kind: AssetKind) -> Result<String> {
    let mut url = Url::parse(&format!(
        "https://github.com/{repo_name_with_owner}/archive/refs/tags",
    ))?;
    // We're pushing this here, because tags may contain `/` and other characters
    // that need to be escaped.
    let asset_filename = format!(
        "{tag}.{extension}",
        extension = match kind {
            AssetKind::TarGz => "tar.gz",
            AssetKind::TarBz2 => "tar.bz2",
            AssetKind::Gz => "gz",
            AssetKind::Zip => "zip",
        }
    );
    url.path_segments_mut()
        .map_err(|()| anyhow!("cannot modify url path segments"))?
        .push(&asset_filename);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use crate::{
        AsyncBody, HttpClient, RedirectPolicy, Response, StatusCode,
        github::{AssetKind, GitHubActivityKind, build_asset_url, repository_activity},
    };
    use anyhow::Result;
    use futures::future::BoxFuture;
    use http::Request;
    use parking_lot::Mutex;
    use std::{collections::VecDeque, sync::Arc};
    use url::Url;

    #[test]
    fn test_build_asset_url() {
        let tag = "release/2.3.5";
        let repo_name_with_owner = "microsoft/vscode-eslint";

        let tarball = build_asset_url(repo_name_with_owner, tag, AssetKind::TarGz).unwrap();
        assert_eq!(
            tarball,
            "https://github.com/microsoft/vscode-eslint/archive/refs/tags/release%2F2.3.5.tar.gz"
        );

        let zip = build_asset_url(repo_name_with_owner, tag, AssetKind::Zip).unwrap();
        assert_eq!(
            zip,
            "https://github.com/microsoft/vscode-eslint/archive/refs/tags/release%2F2.3.5.zip"
        );
    }

    #[test]
    fn test_repository_activity_fetches_issues_pull_requests_and_runs() {
        futures::executor::block_on(async {
            let http = Arc::new(TestHttpClient::new_with_headers(vec![
                TestResponse {
                    status: 200,
                    headers: vec![(
                        "link",
                        r#"<https://api.github.com/repos/owner/repo/issues?page=2>; rel="next""#,
                    )],
                    body: r#"[
                    {
                        "number": 1,
                        "title": "Real issue",
                        "html_url": "https://github.com/owner/repo/issues/1",
                        "state": "open",
                        "user": { "login": "octo" },
                        "updated_at": "2026-06-24T10:00:00Z",
                        "labels": [{ "name": "bug" }],
                        "body": "issue body"
                    },
                    {
                        "number": 2,
                        "title": "PR issue item",
                        "html_url": "https://github.com/owner/repo/pull/2",
                        "state": "open",
                        "user": { "login": "octo" },
                        "pull_request": {}
                    }
                ]"#,
                },
                TestResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: r#"[
                    {
                        "number": 3,
                        "title": "Closed issue",
                        "html_url": "https://github.com/owner/repo/issues/3",
                        "state": "closed",
                        "user": { "login": "octo" },
                        "updated_at": "2026-06-24T09:00:00Z",
                        "labels": [],
                        "body": null
                    }
                ]"#,
                },
                TestResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: r#"[
                    {
                        "number": 7,
                        "title": "Improve graph",
                        "html_url": "https://github.com/owner/repo/pull/7",
                        "state": "open",
                        "user": { "login": "hubot" },
                        "draft": true,
                        "updated_at": "2026-06-24T11:00:00Z",
                        "labels": [{ "name": "enhancement" }],
                        "body": "pull request body"
                    }
                ]"#,
                },
                TestResponse {
                    status: 200,
                    headers: vec![(
                        "link",
                        r#"<https://api.github.com/repos/owner/repo/actions/runs?page=2>; rel="next""#,
                    )],
                    body: r#"{
                    "workflow_runs": [
                        {
                            "id": 42,
                            "name": "CI",
                            "html_url": "https://github.com/owner/repo/actions/runs/42",
                            "status": "completed",
                            "conclusion": "success",
                            "head_branch": "main",
                            "head_sha": "1234567890abcdef",
                            "actor": { "login": "ci-user" },
                            "updated_at": "2026-06-24T12:00:00Z",
                            "event": "push"
                        }
                    ]
                }"#,
                },
                TestResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: r#"{
                    "workflow_runs": [
                        {
                            "id": 43,
                            "name": "Deploy",
                            "html_url": "https://github.com/owner/repo/actions/runs/43",
                            "status": "completed",
                            "conclusion": "failure",
                            "head_branch": "main",
                            "head_sha": "abcdef1234567890",
                            "actor": { "login": "deploy-user" },
                            "updated_at": "2026-06-24T13:00:00Z",
                            "event": "workflow_dispatch"
                        }
                    ]
                }"#,
                },
            ]));

            let activity = repository_activity("owner/repo", Some("secret"), http.clone())
                .await
                .expect("activity should parse");

            assert_eq!(activity.repository_name_with_owner, "owner/repo");
            assert_eq!(activity.issues.len(), 2);
            assert_eq!(activity.issues[0].number, 1);
            assert_eq!(activity.issues[1].number, 3);
            assert_eq!(activity.pull_requests.len(), 1);
            assert_eq!(activity.pull_requests[0].number, 7);
            assert_eq!(activity.pull_requests[0].labels[0].name, "enhancement");
            assert_eq!(
                activity.pull_requests[0].body.as_deref(),
                Some("pull request body")
            );
            assert_eq!(activity.workflow_runs.len(), 2);
            assert_eq!(activity.workflow_runs[0].id, 42);
            assert_eq!(activity.workflow_runs[1].id, 43);

            let sync_batch = activity.to_sync_batch();
            assert_eq!(sync_batch.repository_name_with_owner, "owner/repo");
            assert_eq!(
                sync_batch
                    .items
                    .iter()
                    .map(|item| item.source_id.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "github:owner/repo:workflow_run:43",
                    "github:owner/repo:workflow_run:42",
                    "github:owner/repo:pull_request:7",
                    "github:owner/repo:issue:1",
                    "github:owner/repo:issue:3",
                ]
            );

            let items = activity.to_activity_items();
            assert_eq!(items.len(), 5);
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.source_id.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "github:owner/repo:workflow_run:43",
                    "github:owner/repo:workflow_run:42",
                    "github:owner/repo:pull_request:7",
                    "github:owner/repo:issue:1",
                    "github:owner/repo:issue:3",
                ]
            );

            let issue = items
                .iter()
                .find(|item| item.source_id == "github:owner/repo:issue:1")
                .expect("issue item should exist");
            assert_eq!(issue.kind, GitHubActivityKind::Issue);
            assert_eq!(issue.repository_name_with_owner, "owner/repo");
            assert_eq!(issue.title, "Real issue");
            assert_eq!(issue.body.as_deref(), Some("issue body"));
            assert_eq!(issue.author_login.as_deref(), Some("octo"));
            assert_eq!(issue.labels, vec!["bug"]);
            assert_eq!(issue.number, Some(1));
            assert_eq!(issue.state.as_deref(), Some("open"));
            assert_eq!(issue.updated_at.as_deref(), Some("2026-06-24T10:00:00Z"));

            let closed_issue = items
                .iter()
                .find(|item| item.source_id == "github:owner/repo:issue:3")
                .expect("closed issue item should exist");
            assert_eq!(closed_issue.kind, GitHubActivityKind::Issue);
            assert_eq!(closed_issue.state.as_deref(), Some("closed"));

            let pull_request = items
                .iter()
                .find(|item| item.source_id == "github:owner/repo:pull_request:7")
                .expect("pull request item should exist");
            assert_eq!(pull_request.kind, GitHubActivityKind::PullRequest);
            assert_eq!(pull_request.title, "Improve graph");
            assert_eq!(pull_request.body.as_deref(), Some("pull request body"));
            assert_eq!(pull_request.author_login.as_deref(), Some("hubot"));
            assert_eq!(pull_request.labels, vec!["enhancement"]);
            assert_eq!(pull_request.number, Some(7));
            assert_eq!(pull_request.state.as_deref(), Some("open"));
            assert_eq!(pull_request.draft, Some(true));
            assert_eq!(
                pull_request.updated_at.as_deref(),
                Some("2026-06-24T11:00:00Z")
            );

            let workflow_run = items
                .iter()
                .find(|item| item.source_id == "github:owner/repo:workflow_run:42")
                .expect("workflow run item should exist");
            assert_eq!(workflow_run.kind, GitHubActivityKind::WorkflowRun);
            assert_eq!(workflow_run.title, "CI");
            assert_eq!(workflow_run.author_login.as_deref(), Some("ci-user"));
            assert_eq!(workflow_run.workflow_run_id, Some(42));
            assert_eq!(
                workflow_run.updated_at.as_deref(),
                Some("2026-06-24T12:00:00Z")
            );
            assert_eq!(workflow_run.workflow_status.as_deref(), Some("completed"));
            assert_eq!(workflow_run.workflow_conclusion.as_deref(), Some("success"));
            assert_eq!(workflow_run.workflow_event.as_deref(), Some("push"));
            assert_eq!(workflow_run.workflow_head_branch.as_deref(), Some("main"));
            assert_eq!(
                workflow_run.workflow_head_sha.as_deref(),
                Some("1234567890abcdef")
            );

            let deploy_run = items
                .iter()
                .find(|item| item.source_id == "github:owner/repo:workflow_run:43")
                .expect("deploy workflow run item should exist");
            assert_eq!(deploy_run.workflow_conclusion.as_deref(), Some("failure"));

            let requests = http.requests.lock();
            assert_eq!(requests.len(), 5);
            assert!(
                requests[0]
                    .uri()
                    .to_string()
                    .ends_with("/issues?state=all&per_page=100&sort=updated&direction=desc")
            );
            assert!(requests[1].uri().to_string().ends_with("/issues?page=2"));
            assert!(
                requests[2]
                    .uri()
                    .to_string()
                    .ends_with("/pulls?state=all&per_page=100&sort=updated&direction=desc")
            );
            assert!(
                requests[3]
                    .uri()
                    .to_string()
                    .ends_with("/actions/runs?per_page=100&exclude_pull_requests=false")
            );
            assert!(
                requests[4]
                    .uri()
                    .to_string()
                    .ends_with("/actions/runs?page=2")
            );
            assert!(requests.iter().all(|request| {
                request
                    .headers()
                    .get("Authorization")
                    .and_then(|header| header.to_str().ok())
                    == Some("Bearer secret")
            }));
            assert!(requests.iter().all(|request| {
                request
                    .headers()
                    .get("User-Agent")
                    .and_then(|header| header.to_str().ok())
                    == Some("Rezed")
            }));
            assert!(requests.iter().all(|request| {
                request
                    .headers()
                    .get("Accept")
                    .and_then(|header| header.to_str().ok())
                    == Some("application/vnd.github+json")
            }));
            assert!(requests.iter().all(|request| {
                request
                    .headers()
                    .get("X-GitHub-Api-Version")
                    .and_then(|header| header.to_str().ok())
                    == Some("2022-11-28")
            }));
        });
    }

    #[test]
    fn test_activity_items_serialize_for_inbox_sync() {
        let item = super::GitHubActivityItem {
            kind: GitHubActivityKind::WorkflowRun,
            source_id: "github:owner/repo:workflow_run:42".to_string(),
            repository_name_with_owner: "owner/repo".to_string(),
            title: "CI".to_string(),
            body: None,
            author_login: None,
            labels: Vec::new(),
            url: "https://github.com/owner/repo/actions/runs/42".to_string(),
            number: None,
            state: None,
            draft: None,
            updated_at: Some("2026-06-24T12:00:00Z".to_string()),
            workflow_run_id: Some(42),
            workflow_status: Some("completed".to_string()),
            workflow_conclusion: Some("success".to_string()),
            workflow_event: Some("push".to_string()),
            workflow_head_branch: Some("main".to_string()),
            workflow_head_sha: Some("1234567890abcdef".to_string()),
        };

        let json = serde_json::to_value(&item).expect("activity item should serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "workflow_run",
                "source_id": "github:owner/repo:workflow_run:42",
                "repository_name_with_owner": "owner/repo",
                "title": "CI",
                "body": null,
                "author_login": null,
                "labels": [],
                "url": "https://github.com/owner/repo/actions/runs/42",
                "number": null,
                "state": null,
                "draft": null,
                "updated_at": "2026-06-24T12:00:00Z",
                "workflow_run_id": 42,
                "workflow_status": "completed",
                "workflow_conclusion": "success",
                "workflow_event": "push",
                "workflow_head_branch": "main",
                "workflow_head_sha": "1234567890abcdef"
            })
        );

        let round_trip = serde_json::from_value::<super::GitHubActivityItem>(json)
            .expect("activity item should deserialize");
        assert_eq!(round_trip, item);
    }

    #[test]
    fn test_activity_sync_batch_serializes_for_inbox_sync() {
        let batch = super::GitHubActivitySyncBatch {
            repository_name_with_owner: "owner/repo".to_string(),
            items: vec![super::GitHubActivityItem {
                kind: GitHubActivityKind::Issue,
                source_id: "github:owner/repo:issue:1".to_string(),
                repository_name_with_owner: "owner/repo".to_string(),
                title: "Issue".to_string(),
                body: Some("body".to_string()),
                author_login: Some("octo".to_string()),
                labels: vec!["bug".to_string()],
                url: "https://github.com/owner/repo/issues/1".to_string(),
                number: Some(1),
                state: Some("open".to_string()),
                draft: None,
                updated_at: Some("2026-06-24T10:00:00Z".to_string()),
                workflow_run_id: None,
                workflow_status: None,
                workflow_conclusion: None,
                workflow_event: None,
                workflow_head_branch: None,
                workflow_head_sha: None,
            }],
        };

        let json = serde_json::to_value(&batch).expect("sync batch should serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "repository_name_with_owner": "owner/repo",
                "items": [
                    {
                        "kind": "issue",
                        "source_id": "github:owner/repo:issue:1",
                        "repository_name_with_owner": "owner/repo",
                        "title": "Issue",
                        "body": "body",
                        "author_login": "octo",
                        "labels": ["bug"],
                        "url": "https://github.com/owner/repo/issues/1",
                        "number": 1,
                        "state": "open",
                        "draft": null,
                        "updated_at": "2026-06-24T10:00:00Z",
                        "workflow_run_id": null,
                        "workflow_status": null,
                        "workflow_conclusion": null,
                        "workflow_event": null,
                        "workflow_head_branch": null,
                        "workflow_head_sha": null
                    }
                ]
            })
        );

        let round_trip = serde_json::from_value::<super::GitHubActivitySyncBatch>(json)
            .expect("sync batch should deserialize");
        assert_eq!(round_trip, batch);
    }

    #[test]
    fn test_repository_activity_serializes_for_sync_cache() {
        let activity = super::GitHubRepositoryActivity {
            repository_name_with_owner: "owner/repo".to_string(),
            issues: vec![super::GitHubIssue {
                number: 1,
                title: "Issue".to_string(),
                html_url: "https://github.com/owner/repo/issues/1".to_string(),
                state: "open".to_string(),
                user: super::GitHubUser {
                    login: "octo".to_string(),
                },
                updated_at: Some("2026-06-24T10:00:00Z".to_string()),
                labels: vec![super::GitHubLabel {
                    name: "bug".to_string(),
                }],
                body: Some("body".to_string()),
            }],
            pull_requests: vec![super::GitHubPullRequest {
                number: 7,
                title: "PR".to_string(),
                html_url: "https://github.com/owner/repo/pull/7".to_string(),
                state: "open".to_string(),
                user: super::GitHubUser {
                    login: "hubot".to_string(),
                },
                draft: false,
                updated_at: Some("2026-06-24T11:00:00Z".to_string()),
                labels: Vec::new(),
                body: None,
            }],
            workflow_runs: vec![super::GitHubWorkflowRun {
                id: 42,
                name: Some("CI".to_string()),
                html_url: "https://github.com/owner/repo/actions/runs/42".to_string(),
                status: Some("completed".to_string()),
                conclusion: Some("success".to_string()),
                head_branch: Some("main".to_string()),
                head_sha: Some("1234567890abcdef".to_string()),
                actor: Some(super::GitHubUser {
                    login: "ci-user".to_string(),
                }),
                updated_at: Some("2026-06-24T12:00:00Z".to_string()),
                event: "push".to_string(),
            }],
        };

        let json = serde_json::to_value(&activity).expect("activity should serialize");
        assert_eq!(json["repository_name_with_owner"], "owner/repo");
        assert_eq!(json["issues"][0]["labels"][0]["name"], "bug");
        assert_eq!(json["pull_requests"][0]["number"], 7);
        assert_eq!(json["workflow_runs"][0]["head_sha"], "1234567890abcdef");

        let round_trip = serde_json::from_value::<super::GitHubRepositoryActivity>(json)
            .expect("activity should deserialize");
        assert_eq!(round_trip, activity);
    }

    struct TestHttpClient {
        responses: Mutex<VecDeque<TestResponse>>,
        requests: Mutex<Vec<Request<AsyncBody>>>,
    }

    struct TestResponse {
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: &'static str,
    }

    impl TestHttpClient {
        fn new_with_headers(responses: Vec<TestResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl HttpClient for TestHttpClient {
        fn user_agent(&self) -> Option<&http::HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(&self, req: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
            self.requests.lock().push(req);
            let Some(test_response) = self.responses.lock().pop_front() else {
                return Box::pin(async { anyhow::bail!("no test response queued") });
            };
            Box::pin(async move {
                let status = StatusCode::from_u16(test_response.status)
                    .expect("test status should be valid");
                let mut response = Response::builder()
                    .status(status)
                    .extension(RedirectPolicy::FollowAll);
                for (name, value) in test_response.headers {
                    response = response.header(name, value);
                }
                Ok(response.body(AsyncBody::from(test_response.body))?)
            })
        }
    }
}
