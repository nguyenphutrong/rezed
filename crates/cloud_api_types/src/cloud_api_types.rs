mod extension;
pub mod internal_api;
mod known_or_unknown;
mod plan;
mod timestamp;
pub mod websocket_protocol;

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use crate::extension::*;
pub use crate::known_or_unknown::*;
pub use crate::plan::*;
pub use crate::timestamp::Timestamp;

pub const ZED_SYSTEM_ID_HEADER_NAME: &str = "x-zed-system-id";

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct GetAuthenticatedUserResponse {
    pub user: AuthenticatedUser,
    pub feature_flags: Vec<String>,
    #[serde(default)]
    pub organizations: Vec<Organization>,
    #[serde(default)]
    pub default_organization_id: Option<OrganizationId>,
    #[serde(default)]
    pub plans_by_organization: BTreeMap<OrganizationId, KnownOrUnknown<Plan, String>>,
    #[serde(default)]
    pub configuration_by_organization: BTreeMap<OrganizationId, OrganizationConfiguration>,
    pub plan: PlanInfo,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthenticatedUser {
    pub id: i32,
    pub metrics_id: String,
    pub avatar_url: String,
    pub github_login: String,
    pub name: Option<String>,
    pub is_staff: bool,
    pub accepted_tos_at: Option<Timestamp>,
    pub has_connected_to_collab_once: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubConnectedAccount {
    pub login: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub access_token: String,
}

pub const GITHUB_REQUIRED_OAUTH_SCOPES: &[&str] = &["repo", "read:user"];

impl GitHubConnectedAccount {
    pub fn missing_required_scopes(&self) -> Vec<&'static str> {
        GITHUB_REQUIRED_OAUTH_SCOPES
            .iter()
            .copied()
            .filter(|required_scope| !self.scopes.iter().any(|scope| scope == required_scope))
            .collect()
    }

    pub fn to_status(&self) -> GitHubIntegrationStatus {
        GitHubIntegrationStatus {
            login: self.login.clone(),
            scopes: self.scopes.clone(),
            missing_scopes: self
                .missing_required_scopes()
                .into_iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

impl std::fmt::Debug for GitHubConnectedAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitHubConnectedAccount")
            .field("login", &self.login)
            .field("scopes", &self.scopes)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIntegrationStatus {
    pub login: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub missing_scopes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubActivitySyncBatch {
    pub repository_name_with_owner: String,
    pub items: Vec<GitHubActivityItem>,
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
    pub updated_at: Option<String>,
    pub workflow_run_id: Option<u64>,
    pub workflow_status: Option<String>,
    pub workflow_conclusion: Option<String>,
    pub workflow_event: Option<String>,
    pub workflow_head_branch: Option<String>,
    pub workflow_head_sha: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubActivityKind {
    Issue,
    PullRequest,
    WorkflowRun,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Serialize, Deserialize)]
pub struct OrganizationId(pub Arc<str>);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Organization {
    pub id: OrganizationId,
    pub name: Arc<str>,
    pub is_personal: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct OrganizationConfiguration {
    pub is_zed_model_provider_enabled: bool,
    pub is_agent_thread_feedback_enabled: bool,
    pub is_collaboration_enabled: bool,
    pub edit_prediction: OrganizationEditPredictionConfiguration,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct OrganizationEditPredictionConfiguration {
    pub is_enabled: bool,
    pub is_feedback_enabled: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct AcceptTermsOfServiceResponse {
    pub user: AuthenticatedUser,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct LlmToken(pub String);

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct CreateLlmTokenBody {
    pub organization_id: OrganizationId,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct CreateLlmTokenResponse {
    pub token: LlmToken,
}

#[derive(Debug, Default, PartialEq, Clone, Serialize, Deserialize)]
pub struct UpdateSystemSettingsBody {
    pub selected_organization_id: Option<OrganizationId>,
}

#[derive(Debug, Default, PartialEq, Clone, Serialize, Deserialize)]
pub struct SystemSettings {
    pub selected_organization_id: Option<OrganizationId>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitAgentThreadFeedbackBody {
    pub organization_id: Option<OrganizationId>,
    pub agent: String,
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub rating: String,
    pub thread: serde_json::Value,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitAgentThreadFeedbackCommentsBody {
    pub organization_id: Option<OrganizationId>,
    pub agent: String,
    pub session_id: String,
    pub comments: String,
    pub thread: serde_json::Value,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitEditPredictionFeedbackBody {
    pub organization_id: Option<OrganizationId>,
    pub request_id: String,
    pub rating: String,
    pub inputs: serde_json::Value,
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<String>,
    pub feedback: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SettledEditPrediction {
    pub request_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_editable_region: Option<String>,
    pub ts_error_count_before_prediction: usize,
    pub ts_error_count_after_prediction: usize,
    pub can_collect_data: bool,
    pub is_in_open_source_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_data: Option<SettledEditPredictionSampleData>,
    #[serde(flatten)]
    pub kept_chars: EditPredictionSettledKeptChars,
    pub example: Option<serde_json::Value>,
    pub model_version: Option<String>,
    #[serde(rename = "e2e_latency")]
    pub e2e_latency_ms: u64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SettledEditPredictionSampleData {
    pub repository_url: Option<String>,
    pub revision: Option<String>,
    /// Note: this is only the uncommitted diff for files in `edit_history`
    /// This is done to avoid excessive memory usage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncommitted_diff: Option<String>,
    pub editable_path: Arc<Path>,
    pub editable_offset_range: Range<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buffer_diagnostics: Vec<zeta_prompt::ActiveBufferDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub future_edit_history_events: Vec<Arc<zeta_prompt::Event>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub navigation_history: Vec<EditPredictionRecentFile>,
    pub edit_events_before_quiescence: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_edit_cursor_offset: Option<usize>,
}

pub const MAX_EDIT_PREDICTION_SETTLED_PER_REQUEST: usize = 32;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitEditPredictionSettledBatchBody {
    pub predictions: Vec<SettledEditPrediction>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitEditPredictionSettledResponse {}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct EditPredictionRecentFile {
    pub path: Arc<Path>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_position: Option<usize>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct EditPredictionSettledKeptChars {
    #[serde(rename = "edit_bytes_candidate_new")]
    pub candidate_new: usize,
    #[serde(rename = "edit_bytes_reference_new")]
    pub reference_new: usize,
    #[serde(rename = "edit_bytes_candidate_deleted")]
    pub candidate_deleted: usize,
    #[serde(rename = "edit_bytes_reference_deleted")]
    pub reference_deleted: usize,
    #[serde(rename = "edit_bytes_kept")]
    pub kept: usize,
    #[serde(rename = "edit_bytes_correctly_deleted")]
    pub correctly_deleted: usize,
    #[serde(rename = "edit_bytes_discarded")]
    pub discarded: usize,
    #[serde(rename = "edit_bytes_context")]
    pub context: usize,
    #[serde(rename = "edit_bytes_kept_rate")]
    pub kept_rate: f64,
    #[serde(rename = "edit_bytes_recall_rate")]
    pub recall_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::{
        GITHUB_REQUIRED_OAUTH_SCOPES, GitHubActivityItem, GitHubActivityKind,
        GitHubActivitySyncBatch, GitHubConnectedAccount,
    };

    #[test]
    fn github_connected_account_debug_redacts_token() {
        let account = GitHubConnectedAccount {
            login: "octo".to_string(),
            scopes: vec!["repo".to_string(), "read:user".to_string()],
            access_token: "github-secret-token".to_string(),
        };

        let formatted = format!("{account:?}");
        assert!(formatted.contains("octo"));
        assert!(formatted.contains("<redacted>"));
        assert!(!formatted.contains("github-secret-token"));
    }

    #[test]
    fn github_connected_account_reports_missing_required_scopes() {
        let account = GitHubConnectedAccount {
            login: "octo".to_string(),
            scopes: vec!["repo".to_string(), "read:user".to_string()],
            access_token: "github-secret-token".to_string(),
        };
        assert!(account.missing_required_scopes().is_empty());

        let account = GitHubConnectedAccount {
            scopes: vec!["read:user".to_string()],
            ..account.clone()
        };
        assert_eq!(account.missing_required_scopes(), vec!["repo"]);

        let account = GitHubConnectedAccount {
            scopes: vec!["repo".to_string()],
            ..account
        };
        assert_eq!(account.missing_required_scopes(), vec!["read:user"]);
    }

    #[test]
    fn github_required_oauth_scopes_match_integration_contract() {
        assert_eq!(GITHUB_REQUIRED_OAUTH_SCOPES, &["repo", "read:user"]);
    }

    #[test]
    fn github_connected_account_status_excludes_token() {
        let account = GitHubConnectedAccount {
            login: "octo".to_string(),
            scopes: vec!["read:user".to_string()],
            access_token: "github-secret-token".to_string(),
        };

        let status = account.to_status();
        assert_eq!(status.login, "octo");
        assert_eq!(status.scopes, vec!["read:user"]);
        assert_eq!(status.missing_scopes, vec!["repo"]);

        let json = serde_json::to_value(&status).expect("status should serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "login": "octo",
                "scopes": ["read:user"],
                "missing_scopes": ["repo"]
            })
        );
        assert!(!json.to_string().contains("github-secret-token"));
    }

    #[test]
    fn github_activity_sync_batch_serializes_for_inbox_sync() {
        let batch = GitHubActivitySyncBatch {
            repository_name_with_owner: "owner/repo".to_string(),
            items: vec![GitHubActivityItem {
                kind: GitHubActivityKind::PullRequest,
                source_id: "github:owner/repo:pull_request:7".to_string(),
                repository_name_with_owner: "owner/repo".to_string(),
                title: "Improve graph".to_string(),
                body: Some("body".to_string()),
                author_login: Some("octo".to_string()),
                labels: vec!["enhancement".to_string()],
                url: "https://github.com/owner/repo/pull/7".to_string(),
                number: Some(7),
                state: Some("open".to_string()),
                draft: Some(false),
                updated_at: Some("2026-06-24T11:00:00Z".to_string()),
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
                        "kind": "pull_request",
                        "source_id": "github:owner/repo:pull_request:7",
                        "repository_name_with_owner": "owner/repo",
                        "title": "Improve graph",
                        "body": "body",
                        "author_login": "octo",
                        "labels": ["enhancement"],
                        "url": "https://github.com/owner/repo/pull/7",
                        "number": 7,
                        "state": "open",
                        "draft": false,
                        "updated_at": "2026-06-24T11:00:00Z",
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

        let round_trip = serde_json::from_value::<GitHubActivitySyncBatch>(json)
            .expect("sync batch should deserialize");
        assert_eq!(round_trip, batch);
    }
}
