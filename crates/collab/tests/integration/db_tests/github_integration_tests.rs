use std::sync::Arc;

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
