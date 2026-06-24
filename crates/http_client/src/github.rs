use crate::{AsyncBody, HttpClient, HttpRequestExt};
use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt;
use http::{Request, StatusCode};
use serde::Deserialize;
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
        github::{AssetKind, build_asset_url, repository_activity},
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
