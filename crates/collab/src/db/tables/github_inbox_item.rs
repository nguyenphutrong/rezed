use crate::db::UserId;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "github_inbox_items")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: UserId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub source_id: String,
    pub kind: String,
    pub repository_name_with_owner: String,
    pub title: String,
    pub body: Option<String>,
    pub author_login: Option<String>,
    pub labels_json: String,
    pub url: String,
    pub number: Option<i64>,
    pub state: Option<String>,
    pub draft: Option<bool>,
    pub updated_at: Option<String>,
    pub workflow_run_id: Option<i64>,
    pub workflow_status: Option<String>,
    pub workflow_conclusion: Option<String>,
    pub workflow_event: Option<String>,
    pub workflow_head_branch: Option<String>,
    pub workflow_head_sha: Option<String>,
    pub synced_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
