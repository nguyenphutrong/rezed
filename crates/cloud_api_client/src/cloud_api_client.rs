mod llm_token;
mod websocket;

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use cloud_api_types::websocket_protocol::{PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER_NAME};
pub use cloud_api_types::*;
use futures::AsyncReadExt as _;
use gpui::{App, Task};
use gpui_tokio::Tokio;
use http_client::http::request;
use http_client::{
    AsyncBody, HttpClientWithUrl, HttpRequestExt, Json, Method, Request, Response, StatusCode,
};
use parking_lot::RwLock;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use yawc::WebSocket;

use crate::websocket::Connection;

pub use llm_token::LlmApiToken;

struct Credentials {
    user_id: u32,
    access_token: String,
}

#[derive(Debug, Error)]
pub enum ClientApiError {
    /// 401 — credentials are invalid or expired.
    #[error("Unauthorized")]
    Unauthorized,
    /// No credentials have been set on the client.
    #[error("not signed in")]
    NotSignedIn,
    /// Connection-level failure: DNS, TCP, TLS, timeout, etc.
    /// The HTTP request never received a response.
    #[error("connection to {host} failed")]
    ConnectionFailed {
        host: String,
        #[source]
        source: anyhow::Error,
    },
    /// Server returned a non-success HTTP status (other than 401).
    #[error("{host} returned {status}")]
    ServerError {
        host: String,
        status: StatusCode,
        body: String,
    },
    /// Failed to read or parse the response body after a successful HTTP status.
    #[error("invalid response")]
    InvalidResponse(#[source] anyhow::Error),
    /// Failed to build the HTTP request (URL construction, serialization, etc.).
    /// This typically indicates a programming error.
    #[error("failed to build request")]
    RequestBuildFailed(#[source] anyhow::Error),
}

pub struct CloudApiClient {
    credentials: RwLock<Option<Credentials>>,
    http_client: Arc<HttpClientWithUrl>,
}

impl CloudApiClient {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self {
            credentials: RwLock::new(None),
            http_client,
        }
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials.read().is_some()
    }

    pub fn set_credentials(&self, user_id: u32, access_token: String) {
        *self.credentials.write() = Some(Credentials {
            user_id,
            access_token,
        });
    }

    pub fn clear_credentials(&self) {
        *self.credentials.write() = None;
    }

    fn cloud_host(&self) -> String {
        self.http_client
            .build_zed_cloud_url("/")
            .ok()
            .and_then(|url| url.host_str().map(String::from))
            .unwrap_or_else(|| "cloud.zed.dev".into())
    }

    fn build_request(
        &self,
        req: request::Builder,
        body: impl Into<AsyncBody>,
    ) -> Result<Request<AsyncBody>, ClientApiError> {
        let credentials = self.credentials.read();
        let credentials = credentials.as_ref().ok_or(ClientApiError::NotSignedIn)?;
        build_request(req, body, credentials).map_err(ClientApiError::RequestBuildFailed)
    }

    pub async fn get_authenticated_user(
        &self,
        system_id: Option<String>,
    ) -> Result<GetAuthenticatedUserResponse, ClientApiError> {
        let request_builder = Request::builder()
            .method(Method::GET)
            .uri(
                self.http_client
                    .build_zed_cloud_url("/client/users/me")
                    .map_err(ClientApiError::RequestBuildFailed)?
                    .as_ref(),
            )
            .when_some(system_id, |builder, system_id| {
                builder.header(ZED_SYSTEM_ID_HEADER_NAME, system_id)
            });

        let request = self.build_request(request_builder, AsyncBody::default())?;
        self.send_authenticated_json_request(request).await
    }

    pub async fn fetch_github_connected_account(
        &self,
    ) -> Result<Option<GitHubConnectedAccount>, ClientApiError> {
        let request_builder = Request::builder().method(Method::GET).uri(
            self.http_client
                .build_zed_cloud_url("/client/integrations/github/token")
                .map_err(ClientApiError::RequestBuildFailed)?
                .as_ref(),
        );

        let request = self.build_request(request_builder, AsyncBody::default())?;
        let host = self.cloud_host();
        let mut response = self.http_client.send(request).await.map_err(|source| {
            ClientApiError::ConnectionFailed {
                host: host.clone(),
                source,
            }
        })?;

        match response.status() {
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(None),
            StatusCode::UNAUTHORIZED => Err(ClientApiError::Unauthorized),
            status if status.is_success() => Self::read_response_json(&mut response).await,
            status => {
                let body = match Self::read_response_body(&mut response).await {
                    Ok(body) => body,
                    Err(error) => format!("failed to read response body: {error}"),
                };
                Err(ClientApiError::ServerError { host, status, body })
            }
        }
    }

    pub async fn fetch_github_integration_status(
        &self,
    ) -> Result<Option<GitHubIntegrationStatus>, ClientApiError> {
        let request_builder = Request::builder().method(Method::GET).uri(
            self.http_client
                .build_zed_cloud_url("/client/integrations/github")
                .map_err(ClientApiError::RequestBuildFailed)?
                .as_ref(),
        );

        let request = self.build_request(request_builder, AsyncBody::default())?;
        let host = self.cloud_host();
        let mut response = self.http_client.send(request).await.map_err(|source| {
            ClientApiError::ConnectionFailed {
                host: host.clone(),
                source,
            }
        })?;

        match response.status() {
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(None),
            StatusCode::UNAUTHORIZED => Err(ClientApiError::Unauthorized),
            status if status.is_success() => Self::read_response_json(&mut response).await,
            status => {
                let body = match Self::read_response_body(&mut response).await {
                    Ok(body) => body,
                    Err(error) => format!("failed to read response body: {error}"),
                };
                Err(ClientApiError::ServerError { host, status, body })
            }
        }
    }

    pub async fn fetch_github_inbox_items(
        &self,
        limit: usize,
    ) -> Result<GitHubInboxItemsResponse, ClientApiError> {
        let mut url = self
            .http_client
            .build_zed_cloud_url("/client/integrations/github/inbox")
            .map_err(ClientApiError::RequestBuildFailed)?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        let request_builder = Request::builder().method(Method::GET).uri(url.as_ref());

        let request = self.build_request(request_builder, AsyncBody::default())?;
        self.send_authenticated_json_request(request).await
    }

    pub async fn sync_github_activity(
        &self,
        batch: GitHubActivitySyncBatch,
    ) -> Result<(), ClientApiError> {
        let request_builder = Request::builder().method(Method::POST).uri(
            self.http_client
                .build_zed_cloud_url("/client/integrations/github/activity")
                .map_err(ClientApiError::RequestBuildFailed)?
                .as_ref(),
        );

        let request = self.build_request(request_builder, Json(batch))?;
        self.send_authenticated_request(request).await?;
        Ok(())
    }

    pub async fn sync_github_repository_activity(
        &self,
        repository_name_with_owner: String,
    ) -> Result<(), ClientApiError> {
        let request_builder = Request::builder().method(Method::POST).uri(
            self.http_client
                .build_zed_cloud_url("/client/integrations/github/activity/sync")
                .map_err(ClientApiError::RequestBuildFailed)?
                .as_ref(),
        );

        let request = self.build_request(
            request_builder,
            Json(SyncGitHubRepositoryActivityRequest {
                repository_name_with_owner,
            }),
        )?;
        self.send_authenticated_request(request).await?;
        Ok(())
    }

    pub async fn disconnect_github_integration(&self) -> Result<(), ClientApiError> {
        let request_builder = Request::builder().method(Method::DELETE).uri(
            self.http_client
                .build_zed_cloud_url("/client/integrations/github")
                .map_err(ClientApiError::RequestBuildFailed)?
                .as_ref(),
        );

        let request = self.build_request(request_builder, AsyncBody::default())?;
        self.send_authenticated_request(request).await?;
        Ok(())
    }

    pub fn connect(&self, cx: &App) -> Result<Task<Result<Connection>>> {
        let mut connect_url = self
            .http_client
            .build_zed_cloud_url("/client/users/connect")?;
        connect_url
            .set_scheme(match connect_url.scheme() {
                "https" => "wss",
                "http" => "ws",
                scheme => Err(anyhow!("invalid URL scheme: {scheme}"))?,
            })
            .map_err(|_| anyhow!("failed to set URL scheme"))?;

        let credentials = self.credentials.read();
        let credentials = credentials.as_ref().context("no credentials provided")?;
        let authorization_header = format!("{} {}", credentials.user_id, credentials.access_token);

        Ok(Tokio::spawn_result(cx, async move {
            let ws = WebSocket::connect(connect_url)
                .with_request(
                    request::Builder::new()
                        .header("Authorization", authorization_header)
                        .header(PROTOCOL_VERSION_HEADER_NAME, PROTOCOL_VERSION.to_string()),
                )
                .await?;

            Ok(Connection::new(ws))
        }))
    }

    async fn create_llm_token(
        &self,
        system_id: Option<String>,
        organization_id: OrganizationId,
    ) -> Result<CreateLlmTokenResponse, ClientApiError> {
        let request_builder = Request::builder()
            .method(Method::POST)
            .uri(
                self.http_client
                    .build_zed_cloud_url("/client/llm_tokens")
                    .map_err(ClientApiError::RequestBuildFailed)?
                    .as_ref(),
            )
            .when_some(system_id, |builder, system_id| {
                builder.header(ZED_SYSTEM_ID_HEADER_NAME, system_id)
            });

        let request = self.build_request(
            request_builder,
            Json(CreateLlmTokenBody { organization_id }),
        )?;
        self.send_authenticated_json_request(request).await
    }

    pub async fn update_system_settings(
        &self,
        system_id: String,
        body: UpdateSystemSettingsBody,
    ) -> Result<SystemSettings, ClientApiError> {
        let request_builder = Request::builder()
            .method(Method::PATCH)
            .uri(
                self.http_client
                    .build_zed_cloud_url("/client/system_settings")
                    .map_err(ClientApiError::RequestBuildFailed)?
                    .as_ref(),
            )
            .header(ZED_SYSTEM_ID_HEADER_NAME, system_id);

        let request = self.build_request(request_builder, Json(body))?;
        self.send_authenticated_json_request(request).await
    }

    async fn send_authenticated_json_request<T: DeserializeOwned>(
        &self,
        request: Request<AsyncBody>,
    ) -> Result<T, ClientApiError> {
        let mut response = self.send_authenticated_request(request).await?;
        Self::read_response_json(&mut response).await
    }

    async fn send_authenticated_request(
        &self,
        request: Request<AsyncBody>,
    ) -> Result<Response<AsyncBody>, ClientApiError> {
        let host = self.cloud_host();
        let mut response = self.http_client.send(request).await.map_err(|source| {
            ClientApiError::ConnectionFailed {
                host: host.clone(),
                source,
            }
        })?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        if status == StatusCode::UNAUTHORIZED {
            return Err(ClientApiError::Unauthorized);
        }

        let body = match Self::read_response_body(&mut response).await {
            Ok(body) => body,
            Err(error) => format!("failed to read response body: {error}"),
        };
        Err(ClientApiError::ServerError { host, status, body })
    }

    async fn read_response_json<T: DeserializeOwned>(
        response: &mut Response<AsyncBody>,
    ) -> Result<T, ClientApiError> {
        let body = Self::read_response_body(response).await?;
        serde_json::from_str(&body).map_err(|error| ClientApiError::InvalidResponse(error.into()))
    }

    async fn read_response_body(
        response: &mut Response<AsyncBody>,
    ) -> Result<String, ClientApiError> {
        let mut body = String::new();
        response
            .body_mut()
            .read_to_string(&mut body)
            .await
            .map_err(|error| ClientApiError::InvalidResponse(error.into()))?;
        Ok(body)
    }

    pub async fn validate_credentials(&self, user_id: u32, access_token: &str) -> Result<bool> {
        let request = build_request(
            Request::builder().method(Method::GET).uri(
                self.http_client
                    .build_zed_cloud_url("/client/users/me")?
                    .as_ref(),
            ),
            AsyncBody::default(),
            &Credentials {
                user_id,
                access_token: access_token.into(),
            },
        )?;

        let mut response = self.http_client.send(request).await?;

        if response.status().is_success() {
            Ok(true)
        } else {
            let mut body = String::new();
            response.body_mut().read_to_string(&mut body).await?;
            if response.status() == StatusCode::UNAUTHORIZED {
                Ok(false)
            } else {
                Err(anyhow!(
                    "Failed to get authenticated user.\nStatus: {:?}\nBody: {body}",
                    response.status()
                ))
            }
        }
    }

    pub async fn submit_agent_feedback(&self, body: SubmitAgentThreadFeedbackBody) -> Result<()> {
        let request = self.build_request(
            Request::builder().method(Method::POST).uri(
                self.http_client
                    .build_zed_cloud_url("/client/feedback/agent_thread")?
                    .as_ref(),
            ),
            AsyncBody::from(serde_json::to_string(&body)?),
        )?;

        self.send_authenticated_request(request).await?;
        Ok(())
    }

    pub async fn submit_agent_feedback_comments(
        &self,
        body: SubmitAgentThreadFeedbackCommentsBody,
    ) -> Result<()> {
        let request = self.build_request(
            Request::builder().method(Method::POST).uri(
                self.http_client
                    .build_zed_cloud_url("/client/feedback/agent_thread_comments")?
                    .as_ref(),
            ),
            AsyncBody::from(serde_json::to_string(&body)?),
        )?;

        self.send_authenticated_request(request).await?;
        Ok(())
    }

    pub async fn submit_edit_prediction_feedback(
        &self,
        body: SubmitEditPredictionFeedbackBody,
    ) -> Result<()> {
        let request = self.build_request(
            Request::builder().method(Method::POST).uri(
                self.http_client
                    .build_zed_cloud_url("/client/feedback/edit_prediction")?
                    .as_ref(),
            ),
            AsyncBody::from(serde_json::to_string(&body)?),
        )?;

        self.send_authenticated_request(request).await?;
        Ok(())
    }
}

#[derive(Serialize)]
struct SyncGitHubRepositoryActivityRequest {
    repository_name_with_owner: String,
}

fn build_request(
    req: request::Builder,
    body: impl Into<AsyncBody>,
    credentials: &Credentials,
) -> Result<Request<AsyncBody>> {
    Ok(req
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            format!("{} {}", credentials.user_id, credentials.access_token),
        )
        .body(body.into())?)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http_client::{FakeHttpClient, Response};
    use parking_lot::Mutex;

    use super::*;

    #[test]
    fn test_fetch_github_connected_account() {
        futures::executor::block_on(async {
            let authorization_headers = Arc::new(Mutex::new(Vec::new()));
            let http_client = FakeHttpClient::create({
                let authorization_headers = authorization_headers.clone();
                move |request| {
                    let authorization_headers = authorization_headers.clone();
                    async move {
                        assert_eq!(request.uri().path(), "/client/integrations/github/token");
                        authorization_headers.lock().push(
                            request
                                .headers()
                                .get("Authorization")
                                .and_then(|header| header.to_str().ok())
                                .map(ToString::to_string),
                        );
                        Ok(Response::builder()
                            .status(200)
                            .body(
                                serde_json::json!({
                                    "login": "octo",
                                    "scopes": ["repo", "read:user"],
                                    "access_token": "github-token"
                                })
                                .to_string()
                                .into(),
                            )
                            .unwrap())
                    }
                }
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            let account = client
                .fetch_github_connected_account()
                .await
                .expect("request should succeed")
                .expect("account should be connected");

            assert_eq!(account.login, "octo");
            assert_eq!(account.scopes, vec!["repo", "read:user"]);
            assert_eq!(account.access_token, "github-token");
            assert_eq!(
                authorization_headers.lock().as_slice(),
                &[Some("42 rezed-token".to_string())]
            );
        });
    }

    #[test]
    fn test_fetch_github_connected_account_returns_none_when_disconnected() {
        futures::executor::block_on(async {
            for status in [StatusCode::NO_CONTENT, StatusCode::NOT_FOUND] {
                let http_client = FakeHttpClient::create(move |request| async move {
                    assert_eq!(request.uri().path(), "/client/integrations/github/token");
                    Ok(Response::builder()
                        .status(status)
                        .body(Default::default())
                        .unwrap())
                });
                let client = CloudApiClient::new(http_client);
                client.set_credentials(7, "rezed-token".to_string());

                let account = client
                    .fetch_github_connected_account()
                    .await
                    .expect("request should succeed");

                assert_eq!(account, None);
            }
        });
    }

    #[test]
    fn test_fetch_github_connected_account_requires_credentials() {
        futures::executor::block_on(async {
            let request_count = Arc::new(Mutex::new(0));
            let http_client = FakeHttpClient::create({
                let request_count = request_count.clone();
                move |_request| {
                    let request_count = request_count.clone();
                    async move {
                        *request_count.lock() += 1;
                        Ok(Response::builder()
                            .status(500)
                            .body("unexpected request".into())
                            .unwrap())
                    }
                }
            });
            let client = CloudApiClient::new(http_client);

            let error = client
                .fetch_github_connected_account()
                .await
                .expect_err("missing credentials should fail before request");

            assert!(matches!(error, ClientApiError::NotSignedIn));
            assert_eq!(*request_count.lock(), 0);
        });
    }

    #[test]
    fn test_fetch_github_integration_status() {
        futures::executor::block_on(async {
            let http_client = FakeHttpClient::create(|request| async move {
                assert_eq!(request.method(), Method::GET);
                assert_eq!(request.uri().path(), "/client/integrations/github");
                assert_eq!(
                    request
                        .headers()
                        .get("Authorization")
                        .and_then(|header| header.to_str().ok()),
                    Some("42 rezed-token")
                );
                Ok(Response::builder()
                    .status(200)
                    .body(
                        serde_json::json!({
                            "login": "octo",
                            "scopes": ["repo", "read:user"],
                            "missing_scopes": []
                        })
                        .to_string()
                        .into(),
                    )
                    .unwrap())
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            let status = client
                .fetch_github_integration_status()
                .await
                .expect("request should succeed")
                .expect("account should be connected");

            assert_eq!(status.login, "octo");
            assert_eq!(status.scopes, vec!["repo", "read:user"]);
            assert!(status.missing_scopes.is_empty());
        });
    }

    #[test]
    fn test_fetch_github_integration_status_returns_none_when_disconnected() {
        futures::executor::block_on(async {
            let http_client = FakeHttpClient::create(|request| async move {
                assert_eq!(request.uri().path(), "/client/integrations/github");
                Ok(Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Default::default())
                    .unwrap())
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            let status = client
                .fetch_github_integration_status()
                .await
                .expect("request should succeed");

            assert_eq!(status, None);
        });
    }

    #[test]
    fn test_fetch_github_inbox_items() {
        futures::executor::block_on(async {
            let http_client = FakeHttpClient::create(|request| async move {
                assert_eq!(request.method(), Method::GET);
                assert_eq!(request.uri().path(), "/client/integrations/github/inbox");
                assert_eq!(request.uri().query(), Some("limit=25"));
                assert_eq!(
                    request
                        .headers()
                        .get("Authorization")
                        .and_then(|header| header.to_str().ok()),
                    Some("42 rezed-token")
                );
                Ok(Response::builder()
                    .status(200)
                    .body(
                        serde_json::json!({
                            "items": [{
                                "source_id": "github:owner/repo:issue:1",
                                "kind": "issue",
                                "repository_name_with_owner": "owner/repo",
                                "title": "Issue",
                                "body": null,
                                "author_login": "octocat",
                                "labels": ["bug"],
                                "url": "https://github.com/owner/repo/issues/1",
                                "number": 1,
                                "state": "open",
                                "draft": null,
                                "updated_at": "2026-06-25T00:00:00Z",
                                "workflow_run_id": null,
                                "workflow_status": null,
                                "workflow_conclusion": null,
                                "workflow_event": null,
                                "workflow_head_branch": null,
                                "workflow_head_sha": null
                            }]
                        })
                        .to_string()
                        .into(),
                    )
                    .unwrap())
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            let response = client
                .fetch_github_inbox_items(25)
                .await
                .expect("request should succeed");

            assert_eq!(response.items.len(), 1);
            assert_eq!(response.items[0].source_id, "github:owner/repo:issue:1");
        });
    }

    #[test]
    fn test_sync_github_activity_posts_authenticated_batch() {
        futures::executor::block_on(async {
            let request_body = Arc::new(Mutex::new(None));
            let http_client = FakeHttpClient::create({
                let request_body = request_body.clone();
                move |mut request| {
                    let request_body = request_body.clone();
                    async move {
                        assert_eq!(request.method(), Method::POST);
                        assert_eq!(request.uri().path(), "/client/integrations/github/activity");
                        assert_eq!(
                            request
                                .headers()
                                .get("Authorization")
                                .and_then(|header| header.to_str().ok()),
                            Some("42 rezed-token")
                        );

                        let mut body = String::new();
                        request.body_mut().read_to_string(&mut body).await.unwrap();
                        *request_body.lock() = Some(body);

                        Ok(Response::builder()
                            .status(StatusCode::NO_CONTENT)
                            .body(Default::default())
                            .unwrap())
                    }
                }
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            client
                .sync_github_activity(GitHubActivitySyncBatch {
                    repository_name_with_owner: "owner/repo".to_string(),
                    items: vec![GitHubActivityItem {
                        kind: GitHubActivityKind::Issue,
                        source_id: "github:owner/repo:issue:1".to_string(),
                        repository_name_with_owner: "owner/repo".to_string(),
                        title: "Issue".to_string(),
                        body: None,
                        author_login: Some("octocat".to_string()),
                        labels: vec!["bug".to_string()],
                        url: "https://github.com/owner/repo/issues/1".to_string(),
                        number: Some(1),
                        state: Some("open".to_string()),
                        draft: None,
                        updated_at: Some("2026-06-25T00:00:00Z".to_string()),
                        workflow_run_id: None,
                        workflow_status: None,
                        workflow_conclusion: None,
                        workflow_event: None,
                        workflow_head_branch: None,
                        workflow_head_sha: None,
                    }],
                })
                .await
                .expect("sync request should succeed");

            let body: serde_json::Value =
                serde_json::from_str(request_body.lock().as_deref().unwrap()).unwrap();
            assert_eq!(body["repository_name_with_owner"], "owner/repo");
            assert_eq!(body["items"][0]["kind"], "issue");
            assert_eq!(body["items"][0]["source_id"], "github:owner/repo:issue:1");
        });
    }

    #[test]
    fn test_sync_github_repository_activity_posts_authenticated_request() {
        futures::executor::block_on(async {
            let request_body = Arc::new(Mutex::new(None));
            let http_client = FakeHttpClient::create({
                let request_body = request_body.clone();
                move |mut request| {
                    let request_body = request_body.clone();
                    async move {
                        assert_eq!(request.method(), Method::POST);
                        assert_eq!(
                            request.uri().path(),
                            "/client/integrations/github/activity/sync"
                        );
                        assert_eq!(
                            request
                                .headers()
                                .get("Authorization")
                                .and_then(|header| header.to_str().ok()),
                            Some("42 rezed-token")
                        );

                        let mut body = String::new();
                        request.body_mut().read_to_string(&mut body).await.unwrap();
                        *request_body.lock() = Some(body);

                        Ok(Response::builder()
                            .status(StatusCode::NO_CONTENT)
                            .body(Default::default())
                            .unwrap())
                    }
                }
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            client
                .sync_github_repository_activity("owner/repo".to_string())
                .await
                .expect("sync request should succeed");

            let body: serde_json::Value =
                serde_json::from_str(request_body.lock().as_deref().unwrap()).unwrap();
            assert_eq!(body["repository_name_with_owner"], "owner/repo");
        });
    }

    #[test]
    fn test_disconnect_github_integration_posts_authenticated_request() {
        futures::executor::block_on(async {
            let http_client = FakeHttpClient::create(|request| async move {
                assert_eq!(request.method(), Method::DELETE);
                assert_eq!(request.uri().path(), "/client/integrations/github");
                assert_eq!(
                    request
                        .headers()
                        .get("Authorization")
                        .and_then(|header| header.to_str().ok()),
                    Some("42 rezed-token")
                );
                Ok(Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Default::default())
                    .unwrap())
            });
            let client = CloudApiClient::new(http_client);
            client.set_credentials(42, "rezed-token".to_string());

            client
                .disconnect_github_integration()
                .await
                .expect("disconnect request should succeed");
        });
    }
}
