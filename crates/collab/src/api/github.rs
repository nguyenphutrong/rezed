use crate::{AppState, Error, Result, auth, rpc::Principal};
use anyhow::{Context as _, anyhow};
use axum::{
    Extension, Json, Router, http::StatusCode, middleware, response::IntoResponse, routing::get,
};
use cloud_api_types::{GitHubActivitySyncBatch, GitHubConnectedAccount};
use http_client::github::{
    GitHubIssue, GitHubLabel, GitHubPullRequest, GitHubRepositoryActivity, GitHubUser,
    GitHubWorkflowRun,
};
use serde::Deserialize;
use std::sync::Arc;

pub fn router() -> Router {
    Router::new()
        .route(
            "/client/integrations/github",
            get(get_status).delete(disconnect),
        )
        .route(
            "/client/integrations/github/token",
            get(get_token).put(upsert_token),
        )
        .route(
            "/client/integrations/github/activity",
            axum::routing::post(sync_activity),
        )
        .route(
            "/client/integrations/github/activity/sync",
            axum::routing::post(sync_repository_activity),
        )
        .layer(middleware::from_fn(auth::validate_header))
}

async fn get_status(
    Extension(app): Extension<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
) -> Result<axum::response::Response> {
    let Principal::User(user) = principal;

    let Some(account) = app
        .db
        .get_github_integration(user.id, &app.config.zed_cloud_internal_api_key)
        .await?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };

    Ok(Json(account.to_status()).into_response())
}

async fn get_token(
    Extension(app): Extension<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
) -> Result<axum::response::Response> {
    let Principal::User(user) = principal;

    let Some(account) = app
        .db
        .get_github_integration(user.id, &app.config.zed_cloud_internal_api_key)
        .await?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };

    Ok(Json(account).into_response())
}

async fn upsert_token(
    Extension(app): Extension<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<SetGitHubIntegrationRequest>,
) -> Result<StatusCode> {
    let Principal::User(user) = principal;

    let Some(account) = request.into_account() else {
        return Err(Error::http(
            StatusCode::BAD_REQUEST,
            "GitHub login and access token are required".into(),
        ));
    };

    app.db
        .upsert_github_integration(
            user.id,
            account.login,
            account.scopes,
            account.access_token,
            &app.config.zed_cloud_internal_api_key,
        )
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn disconnect(
    Extension(app): Extension<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
) -> Result<StatusCode> {
    let Principal::User(user) = principal;

    app.db.delete_github_integration(user.id).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn sync_activity(
    Extension(app): Extension<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Json(batch): Json<GitHubActivitySyncBatch>,
) -> Result<StatusCode> {
    let Principal::User(user) = principal;

    app.db.sync_github_inbox_items(user.id, batch).await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn sync_repository_activity(
    Extension(app): Extension<Arc<AppState>>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<SyncGitHubRepositoryActivityRequest>,
) -> Result<StatusCode> {
    let Principal::User(user) = principal;
    let Some(account) = app
        .db
        .get_github_integration(user.id, &app.config.zed_cloud_internal_api_key)
        .await?
    else {
        return Err(Error::http(
            StatusCode::CONFLICT,
            "GitHub is not connected".into(),
        ));
    };
    let http_client = app
        .http_client
        .as_ref()
        .context("HTTP client is unavailable")?;

    let activity = fetch_github_repository_activity(
        http_client,
        &request.repository_name_with_owner,
        &account.access_token,
    )
    .await?;
    app.db
        .sync_github_inbox_items(user.id, activity.to_sync_batch())
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SetGitHubIntegrationRequest {
    login: String,
    scopes: Vec<String>,
    access_token: String,
}

#[derive(Deserialize)]
struct SyncGitHubRepositoryActivityRequest {
    repository_name_with_owner: String,
}

impl SetGitHubIntegrationRequest {
    fn into_account(self) -> Option<GitHubConnectedAccount> {
        let login = self.login.trim().to_string();
        let access_token = self.access_token.trim().to_string();
        if login.is_empty() || access_token.is_empty() {
            return None;
        }

        Some(GitHubConnectedAccount {
            login,
            scopes: self.scopes,
            access_token,
        })
    }
}

#[derive(Deserialize)]
struct GitHubIssueItem {
    number: u64,
    title: String,
    html_url: String,
    user: GitHubUser,
    labels: Vec<GitHubLabel>,
    state: String,
    body: Option<String>,
    updated_at: Option<String>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GitHubWorkflowRunsResponse {
    workflow_runs: Vec<GitHubWorkflowRun>,
}

async fn fetch_github_repository_activity(
    http_client: &reqwest::Client,
    repository_name_with_owner: &str,
    access_token: &str,
) -> Result<GitHubRepositoryActivity> {
    if repository_name_with_owner.trim().is_empty() {
        return Err(Error::http(
            StatusCode::BAD_REQUEST,
            "repository_name_with_owner is required".into(),
        ));
    }

    let issues_url = format!(
        "https://api.github.com/repos/{repository_name_with_owner}/issues?state=all&per_page=100&sort=updated&direction=desc"
    );
    let pulls_url = format!(
        "https://api.github.com/repos/{repository_name_with_owner}/pulls?state=all&per_page=100&sort=updated&direction=desc"
    );
    let workflow_runs_url = format!(
        "https://api.github.com/repos/{repository_name_with_owner}/actions/runs?per_page=100&exclude_pull_requests=false"
    );

    let issue_items: Vec<GitHubIssueItem> =
        get_github_paginated_json(http_client, &issues_url, access_token).await?;
    let pull_requests: Vec<GitHubPullRequest> =
        get_github_paginated_json(http_client, &pulls_url, access_token).await?;
    let mut workflow_runs = Vec::new();
    let mut next_url = Some(workflow_runs_url);
    while let Some(url) = next_url.take() {
        let (page, next) =
            get_github_json_page::<GitHubWorkflowRunsResponse>(http_client, &url, access_token)
                .await?;
        workflow_runs.extend(page.workflow_runs);
        next_url = next;
    }

    Ok(GitHubRepositoryActivity {
        repository_name_with_owner: repository_name_with_owner.to_string(),
        issues: issue_items
            .into_iter()
            .filter(|issue| issue.pull_request.is_none())
            .map(|issue| GitHubIssue {
                number: issue.number,
                title: issue.title,
                html_url: issue.html_url,
                user: issue.user,
                labels: issue.labels,
                state: issue.state,
                body: issue.body,
                updated_at: issue.updated_at,
            })
            .collect(),
        pull_requests,
        workflow_runs,
    })
}

async fn get_github_paginated_json<T: for<'de> Deserialize<'de>>(
    http_client: &reqwest::Client,
    url: &str,
    access_token: &str,
) -> Result<Vec<T>> {
    let mut results = Vec::new();
    let mut next_url = Some(url.to_string());
    while let Some(url) = next_url.take() {
        let (mut page, next) =
            get_github_json_page::<Vec<T>>(http_client, &url, access_token).await?;
        results.append(&mut page);
        next_url = next;
    }
    Ok(results)
}

async fn get_github_json_page<T: for<'de> Deserialize<'de>>(
    http_client: &reqwest::Client,
    url: &str,
    access_token: &str,
) -> Result<(T, Option<String>)> {
    let response = http_client
        .get(url)
        .bearer_auth(access_token)
        .header("User-Agent", "Rezed")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .context("failed to call GitHub API")?;
    let next = response
        .headers()
        .get(reqwest::header::LINK)
        .and_then(|link| link.to_str().ok())
        .and_then(github_next_link);
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("failed to read GitHub API response")?;
    if !status.is_success() {
        return Err(Error::http(
            StatusCode::BAD_GATEWAY,
            format!("GitHub returned status {status}, response: {body:?}"),
        ));
    }
    let page = serde_json::from_slice(&body)
        .map_err(|error| anyhow!("error deserializing GitHub API response: {error:?}"))?;

    Ok((page, next))
}

fn github_next_link(link_header: &str) -> Option<String> {
    for link in link_header.split(',') {
        let Some((url, rel)) = link.trim().split_once(';') else {
            continue;
        };
        if rel.trim() == r#"rel="next""# {
            return Some(
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
            );
        }
    }
    None
}
