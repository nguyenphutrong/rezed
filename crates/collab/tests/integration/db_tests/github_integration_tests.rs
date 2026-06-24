use std::sync::Arc;

use cloud_api_types::{GitHubActivityItem, GitHubActivityKind, GitHubActivitySyncBatch};
use collab::db::Database;

use crate::test_both_dbs;

test_both_dbs!(
    test_github_integration_round_trip,
    test_github_integration_round_trip_postgres,
    test_github_integration_round_trip_sqlite
);

async fn test_github_integration_round_trip(db: &Arc<Database>) {
    let user = db.create_user(false).await.unwrap();
    db.upsert_github_integration(
        user.user_id,
        "octocat".to_string(),
        vec!["repo".to_string(), "read:user".to_string()],
        "github-token".to_string(),
        "encryption-secret",
    )
    .await
    .unwrap();

    let encrypted_access_token = db
        .get_encrypted_github_access_token_for_test(user.user_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!encrypted_access_token.contains("github-token"));

    let account = db
        .get_github_integration(user.user_id, "encryption-secret")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.login, "octocat");
    assert_eq!(account.scopes, &["repo", "read:user"]);
    assert_eq!(account.access_token, "github-token");

    db.delete_github_integration(user.user_id).await.unwrap();
    assert!(
        db.get_github_integration(user.user_id, "encryption-secret")
            .await
            .unwrap()
            .is_none()
    );
}

test_both_dbs!(
    test_github_inbox_activity_sync,
    test_github_inbox_activity_sync_postgres,
    test_github_inbox_activity_sync_sqlite
);

async fn test_github_inbox_activity_sync(db: &Arc<Database>) {
    let user = db.create_user(false).await.unwrap();

    let count = db
        .sync_github_inbox_items(
            user.user_id,
            GitHubActivitySyncBatch {
                repository_name_with_owner: "owner/repo".to_string(),
                items: vec![
                    github_activity_item(GitHubActivityKind::Issue, "github:owner/repo:issue:1"),
                    github_activity_item(
                        GitHubActivityKind::WorkflowRun,
                        "github:owner/repo:workflow_run:42",
                    ),
                ],
            },
        )
        .await
        .unwrap();
    assert_eq!(count, 2);

    let items = db
        .get_github_inbox_items_for_test(user.user_id)
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].source_id, "github:owner/repo:issue:1");
    assert_eq!(items[0].kind, "issue");
    assert_eq!(items[0].repository_name_with_owner, "owner/repo");
    assert_eq!(items[0].labels_json, "[\"bug\"]");
    assert_eq!(items[1].kind, "workflow_run");
    assert_eq!(
        items[1].workflow_head_sha.as_deref(),
        Some("0123456789abcdef")
    );

    let mut updated = github_activity_item(GitHubActivityKind::Issue, "github:owner/repo:issue:1");
    updated.title = "Updated issue".to_string();
    updated.state = Some("closed".to_string());
    db.sync_github_inbox_items(
        user.user_id,
        GitHubActivitySyncBatch {
            repository_name_with_owner: "owner/repo".to_string(),
            items: vec![updated],
        },
    )
    .await
    .unwrap();

    let items = db
        .get_github_inbox_items_for_test(user.user_id)
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].title, "Updated issue");
    assert_eq!(items[0].state.as_deref(), Some("closed"));
}

fn github_activity_item(kind: GitHubActivityKind, source_id: &str) -> GitHubActivityItem {
    GitHubActivityItem {
        kind,
        source_id: source_id.to_string(),
        repository_name_with_owner: "owner/repo".to_string(),
        title: "GitHub activity".to_string(),
        body: Some("Body".to_string()),
        author_login: Some("octocat".to_string()),
        labels: vec!["bug".to_string()],
        url: "https://github.com/owner/repo/issues/1".to_string(),
        number: Some(1),
        state: Some("open".to_string()),
        draft: Some(false),
        updated_at: Some("2026-06-25T00:00:00Z".to_string()),
        workflow_run_id: Some(42),
        workflow_status: Some("completed".to_string()),
        workflow_conclusion: Some("success".to_string()),
        workflow_event: Some("push".to_string()),
        workflow_head_branch: Some("main".to_string()),
        workflow_head_sha: Some("0123456789abcdef".to_string()),
    }
}
