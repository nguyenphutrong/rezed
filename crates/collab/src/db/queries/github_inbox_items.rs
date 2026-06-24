use super::*;
use anyhow::{Context as _, anyhow};
use cloud_api_types::{
    GitHubActivityKind, GitHubActivitySyncBatch, GitHubInboxItem, GitHubInboxItemsResponse,
};

impl Database {
    pub async fn sync_github_inbox_items(
        &self,
        user_id: UserId,
        batch: GitHubActivitySyncBatch,
    ) -> Result<usize> {
        let item_count = batch.items.len();

        self.transaction(|tx| {
            let items = batch.items.clone();
            async move {
                if items.is_empty() {
                    return Ok(0);
                }

                let synced_at = chrono::Utc::now().naive_utc();
                let rows = items
                    .into_iter()
                    .map(|item| {
                        let labels_json = serde_json::to_string(&item.labels)?;
                        let number = item
                            .number
                            .map(i64::try_from)
                            .transpose()
                            .context("GitHub issue or pull request number exceeds i64")?;
                        let workflow_run_id = item
                            .workflow_run_id
                            .map(i64::try_from)
                            .transpose()
                            .context("GitHub workflow run id exceeds i64")?;
                        Ok(github_inbox_item::ActiveModel {
                            user_id: ActiveValue::Set(user_id),
                            source_id: ActiveValue::Set(item.source_id),
                            kind: ActiveValue::Set(github_activity_kind(&item.kind).to_string()),
                            repository_name_with_owner: ActiveValue::Set(
                                item.repository_name_with_owner,
                            ),
                            title: ActiveValue::Set(item.title),
                            body: ActiveValue::Set(item.body),
                            author_login: ActiveValue::Set(item.author_login),
                            labels_json: ActiveValue::Set(labels_json),
                            url: ActiveValue::Set(item.url),
                            number: ActiveValue::Set(number),
                            state: ActiveValue::Set(item.state),
                            draft: ActiveValue::Set(item.draft),
                            updated_at: ActiveValue::Set(item.updated_at),
                            workflow_run_id: ActiveValue::Set(workflow_run_id),
                            workflow_status: ActiveValue::Set(item.workflow_status),
                            workflow_conclusion: ActiveValue::Set(item.workflow_conclusion),
                            workflow_event: ActiveValue::Set(item.workflow_event),
                            workflow_head_branch: ActiveValue::Set(item.workflow_head_branch),
                            workflow_head_sha: ActiveValue::Set(item.workflow_head_sha),
                            synced_at: ActiveValue::Set(synced_at.into()),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;

                github_inbox_item::Entity::insert_many(rows)
                    .on_conflict(
                        OnConflict::columns([
                            github_inbox_item::Column::UserId,
                            github_inbox_item::Column::SourceId,
                        ])
                        .update_columns([
                            github_inbox_item::Column::Kind,
                            github_inbox_item::Column::RepositoryNameWithOwner,
                            github_inbox_item::Column::Title,
                            github_inbox_item::Column::Body,
                            github_inbox_item::Column::AuthorLogin,
                            github_inbox_item::Column::LabelsJson,
                            github_inbox_item::Column::Url,
                            github_inbox_item::Column::Number,
                            github_inbox_item::Column::State,
                            github_inbox_item::Column::Draft,
                            github_inbox_item::Column::UpdatedAt,
                            github_inbox_item::Column::WorkflowRunId,
                            github_inbox_item::Column::WorkflowStatus,
                            github_inbox_item::Column::WorkflowConclusion,
                            github_inbox_item::Column::WorkflowEvent,
                            github_inbox_item::Column::WorkflowHeadBranch,
                            github_inbox_item::Column::WorkflowHeadSha,
                            github_inbox_item::Column::SyncedAt,
                        ])
                        .to_owned(),
                    )
                    .exec_without_returning(&*tx)
                    .await?;

                Ok(item_count)
            }
        })
        .await
    }

    #[cfg(feature = "test-support")]
    pub async fn get_github_inbox_items_for_test(
        &self,
        user_id: UserId,
    ) -> Result<Vec<github_inbox_item::Model>> {
        self.transaction(|tx| async move {
            Ok(github_inbox_item::Entity::find()
                .filter(github_inbox_item::Column::UserId.eq(user_id))
                .order_by_asc(github_inbox_item::Column::SourceId)
                .all(&*tx)
                .await?)
        })
        .await
    }

    pub async fn get_github_inbox_items(
        &self,
        user_id: UserId,
        limit: usize,
    ) -> Result<GitHubInboxItemsResponse> {
        self.transaction(|tx| async move {
            let rows = github_inbox_item::Entity::find()
                .filter(github_inbox_item::Column::UserId.eq(user_id))
                .order_by_desc(github_inbox_item::Column::UpdatedAt)
                .order_by_desc(github_inbox_item::Column::SourceId)
                .limit(limit as u64)
                .all(&*tx)
                .await?;
            let items = rows
                .into_iter()
                .map(github_inbox_item_to_dto)
                .collect::<Result<Vec<_>>>()?;

            Ok(GitHubInboxItemsResponse { items })
        })
        .await
    }
}

fn github_activity_kind(kind: &GitHubActivityKind) -> &'static str {
    match kind {
        GitHubActivityKind::Issue => "issue",
        GitHubActivityKind::PullRequest => "pull_request",
        GitHubActivityKind::WorkflowRun => "workflow_run",
    }
}

fn github_activity_kind_from_db(kind: &str) -> Result<GitHubActivityKind> {
    match kind {
        "issue" => Ok(GitHubActivityKind::Issue),
        "pull_request" => Ok(GitHubActivityKind::PullRequest),
        "workflow_run" => Ok(GitHubActivityKind::WorkflowRun),
        other => Err(anyhow!("unknown GitHub inbox item kind: {other}").into()),
    }
}

fn github_inbox_item_to_dto(row: github_inbox_item::Model) -> Result<GitHubInboxItem> {
    let labels = serde_json::from_str(&row.labels_json)
        .context("failed to parse GitHub inbox item labels")?;
    let number = row
        .number
        .map(u64::try_from)
        .transpose()
        .context("GitHub inbox item number is negative")?;
    let workflow_run_id = row
        .workflow_run_id
        .map(u64::try_from)
        .transpose()
        .context("GitHub inbox workflow run id is negative")?;

    Ok(GitHubInboxItem {
        source_id: row.source_id,
        kind: github_activity_kind_from_db(&row.kind)?,
        repository_name_with_owner: row.repository_name_with_owner,
        title: row.title,
        body: row.body,
        author_login: row.author_login,
        labels,
        url: row.url,
        number,
        state: row.state,
        draft: row.draft,
        updated_at: row.updated_at,
        workflow_run_id,
        workflow_status: row.workflow_status,
        workflow_conclusion: row.workflow_conclusion,
        workflow_event: row.workflow_event,
        workflow_head_branch: row.workflow_head_branch,
        workflow_head_sha: row.workflow_head_sha,
    })
}
