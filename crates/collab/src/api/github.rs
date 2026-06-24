use crate::{AppState, Error, Result, auth, rpc::Principal};
use axum::{
    Extension, Json, Router, http::StatusCode, middleware, response::IntoResponse, routing::get,
};
use cloud_api_types::{GitHubActivitySyncBatch, GitHubConnectedAccount};
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

#[derive(Deserialize)]
struct SetGitHubIntegrationRequest {
    login: String,
    scopes: Vec<String>,
    access_token: String,
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
