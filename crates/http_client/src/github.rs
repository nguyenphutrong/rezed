use crate::{AsyncBody, HttpClient, HttpRequestExt};
use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt;
use http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;

const GITHUB_API_URL: &str = "https://api.github.com";

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitHubRepositoryActivity {
    pub repository_name_with_owner: String,
    pub issues: Vec<GitHubIssue>,
    pub pull_requests: Vec<GitHubPullRequest>,
    pub workflow_runs: Vec<GitHubWorkflowRun>,
}

impl GitHubRepositoryActivity {
    pub fn to_activity_items(&self) -> Vec<GitHubActivityItem> {
        self.issues
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
                workflow_run_id: None,
                workflow_status: None,
                workflow_conclusion: None,
                workflow_event: None,
                workflow_head_branch: None,
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
                        workflow_run_id: None,
                        workflow_status: None,
                        workflow_conclusion: None,
                        workflow_event: None,
                        workflow_head_branch: None,
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
                    author_login: None,
                    labels: Vec::new(),
                    url: run.html_url.clone(),
                    number: None,
                    state: None,
                    draft: None,
                    workflow_run_id: Some(run.id),
                    workflow_status: run.status.clone(),
                    workflow_conclusion: run.conclusion.clone(),
                    workflow_event: Some(run.event.clone()),
                    workflow_head_branch: run.head_branch.clone(),
                }
            }))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubActivityItem {
    pub kind: GitHubActivityKind,
    pub source_id: String,
    pub repository_name_with_owner: String,
    pub title: String,
    pub body: Option<String>,
    pub author_login: Option<String>,
    pub labels: Vec<String>,
    pub url: String,
    pub number: Option<u64>,
    pub state: Option<String>,
    pub draft: Option<bool>,
    pub workflow_run_id: Option<u64>,
    pub workflow_status: Option<String>,
    pub workflow_conclusion: Option<String>,
    pub workflow_event: Option<String>,
    pub workflow_head_branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubActivityKind {
    Issue,
    PullRequest,
    WorkflowRun,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub state: String,
    pub user: GitHubUser,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitHubPullRequest {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub state: String,
    pub user: GitHubUser,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitHubWorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    pub html_url: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub head_branch: Option<String>,
    pub event: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct GitHubLabel {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
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
        "{GITHUB_API_URL}/repos/{repo_name_with_owner}/issues?state=open&per_page=25&sort=updated"
    );
    let pulls_url =
        format!("{GITHUB_API_URL}/repos/{repo_name_with_owner}/pulls?state=open&per_page=25");
    let workflow_runs_url = format!(
        "{GITHUB_API_URL}/repos/{repo_name_with_owner}/actions/runs?per_page=25&exclude_pull_requests=false"
    );

    let issue_items: Vec<GitHubIssueItem> =
        get_github_json(http.clone(), &issues_url, token).await?;
    let pull_requests = get_github_json(http.clone(), &pulls_url, token).await?;
    let workflow_runs: GitHubWorkflowRunsResponse =
        get_github_json(http, &workflow_runs_url, token).await?;

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
                labels: issue.labels,
                body: issue.body,
            })
            .collect(),
        pull_requests,
        workflow_runs: workflow_runs.workflow_runs,
    })
}

async fn get_github_json<T: for<'de> Deserialize<'de>>(
    http: Arc<dyn HttpClient>,
    url: &str,
    token: Option<&str>,
) -> anyhow::Result<T> {
    let request = Request::get(url)
        .follow_redirects(crate::RedirectPolicy::FollowAll)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .when_some(token, |builder, token| {
            builder.header("Authorization", format!("Bearer {}", token))
        })
        .body(AsyncBody::default())?;

    let mut response = http.send(request).await?;
    let status = response.status();
    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("error reading GitHub API response")?;

    if !status.is_success() {
        return Err(github_status_error(status, &body));
    }

    serde_json::from_slice(&body).map_err(|err| {
        log::error!("Error deserializing GitHub API response: {err:?}");
        log::error!(
            "GitHub API response text: {:?}",
            String::from_utf8_lossy(body.as_slice())
        );
        anyhow!("error deserializing GitHub API response: {err:?}")
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
            let http = Arc::new(TestHttpClient::new(vec![
                (
                    200,
                    r#"[
                    {
                        "number": 1,
                        "title": "Real issue",
                        "html_url": "https://github.com/owner/repo/issues/1",
                        "state": "open",
                        "user": { "login": "octo" },
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
                ),
                (
                    200,
                    r#"[
                    {
                        "number": 7,
                        "title": "Improve graph",
                        "html_url": "https://github.com/owner/repo/pull/7",
                        "state": "open",
                        "user": { "login": "hubot" },
                        "draft": true,
                        "labels": [{ "name": "enhancement" }],
                        "body": "pull request body"
                    }
                ]"#,
                ),
                (
                    200,
                    r#"{
                    "workflow_runs": [
                        {
                            "id": 42,
                            "name": "CI",
                            "html_url": "https://github.com/owner/repo/actions/runs/42",
                            "status": "completed",
                            "conclusion": "success",
                            "head_branch": "main",
                            "event": "push"
                        }
                    ]
                }"#,
                ),
            ]));

            let activity = repository_activity("owner/repo", Some("secret"), http.clone())
                .await
                .expect("activity should parse");

            assert_eq!(activity.repository_name_with_owner, "owner/repo");
            assert_eq!(activity.issues.len(), 1);
            assert_eq!(activity.issues[0].number, 1);
            assert_eq!(activity.pull_requests.len(), 1);
            assert_eq!(activity.pull_requests[0].number, 7);
            assert_eq!(activity.pull_requests[0].labels[0].name, "enhancement");
            assert_eq!(
                activity.pull_requests[0].body.as_deref(),
                Some("pull request body")
            );
            assert_eq!(activity.workflow_runs.len(), 1);
            assert_eq!(activity.workflow_runs[0].id, 42);

            let items = activity.to_activity_items();
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].kind, GitHubActivityKind::Issue);
            assert_eq!(items[0].source_id, "github:owner/repo:issue:1");
            assert_eq!(items[0].repository_name_with_owner, "owner/repo");
            assert_eq!(items[0].title, "Real issue");
            assert_eq!(items[0].body.as_deref(), Some("issue body"));
            assert_eq!(items[0].author_login.as_deref(), Some("octo"));
            assert_eq!(items[0].labels, vec!["bug"]);
            assert_eq!(items[0].number, Some(1));
            assert_eq!(items[0].state.as_deref(), Some("open"));
            assert_eq!(items[1].kind, GitHubActivityKind::PullRequest);
            assert_eq!(items[1].source_id, "github:owner/repo:pull_request:7");
            assert_eq!(items[1].title, "Improve graph");
            assert_eq!(items[1].body.as_deref(), Some("pull request body"));
            assert_eq!(items[1].author_login.as_deref(), Some("hubot"));
            assert_eq!(items[1].labels, vec!["enhancement"]);
            assert_eq!(items[1].number, Some(7));
            assert_eq!(items[1].state.as_deref(), Some("open"));
            assert_eq!(items[1].draft, Some(true));
            assert_eq!(items[2].kind, GitHubActivityKind::WorkflowRun);
            assert_eq!(items[2].source_id, "github:owner/repo:workflow_run:42");
            assert_eq!(items[2].title, "CI");
            assert_eq!(items[2].workflow_run_id, Some(42));
            assert_eq!(items[2].workflow_status.as_deref(), Some("completed"));
            assert_eq!(items[2].workflow_conclusion.as_deref(), Some("success"));
            assert_eq!(items[2].workflow_event.as_deref(), Some("push"));
            assert_eq!(items[2].workflow_head_branch.as_deref(), Some("main"));

            let requests = http.requests.lock();
            assert_eq!(requests.len(), 3);
            assert!(
                requests[0]
                    .uri()
                    .to_string()
                    .ends_with("/issues?state=open&per_page=25&sort=updated")
            );
            assert!(
                requests[1]
                    .uri()
                    .to_string()
                    .ends_with("/pulls?state=open&per_page=25")
            );
            assert!(
                requests[2]
                    .uri()
                    .to_string()
                    .ends_with("/actions/runs?per_page=25&exclude_pull_requests=false")
            );
            assert!(requests.iter().all(|request| {
                request
                    .headers()
                    .get("Authorization")
                    .and_then(|header| header.to_str().ok())
                    == Some("Bearer secret")
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
            workflow_run_id: Some(42),
            workflow_status: Some("completed".to_string()),
            workflow_conclusion: Some("success".to_string()),
            workflow_event: Some("push".to_string()),
            workflow_head_branch: Some("main".to_string()),
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
                "workflow_run_id": 42,
                "workflow_status": "completed",
                "workflow_conclusion": "success",
                "workflow_event": "push",
                "workflow_head_branch": "main"
            })
        );

        let round_trip = serde_json::from_value::<super::GitHubActivityItem>(json)
            .expect("activity item should deserialize");
        assert_eq!(round_trip, item);
    }

    struct TestHttpClient {
        responses: Mutex<VecDeque<(StatusCode, &'static str)>>,
        requests: Mutex<Vec<Request<AsyncBody>>>,
    }

    impl TestHttpClient {
        fn new(responses: Vec<(u16, &'static str)>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|(status, body)| {
                            (
                                StatusCode::from_u16(status).expect("test status should be valid"),
                                body,
                            )
                        })
                        .collect(),
                ),
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
            let Some((status, body)) = self.responses.lock().pop_front() else {
                return Box::pin(async { anyhow::bail!("no test response queued") });
            };
            Box::pin(async move {
                Ok(Response::builder()
                    .status(status)
                    .extension(RedirectPolicy::FollowAll)
                    .body(AsyncBody::from(body))?)
            })
        }
    }
}
