use crate::{git_panel::GitPanel, github_pr_diff_view::GitHubPrDiffView};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, WeakEntity,
    Window,
};
use http_client::{
    HttpClient,
    github::{
        GitHubCheckRun, GitHubCombinedStatus, GitHubCommitStatus, GitHubIssueComment,
        GitHubPullRequest, GitHubPullRequestCommit, GitHubPullRequestReview,
        GitHubPullRequestReviewComment,
    },
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use time_format::{TimestampFormat, format_localized_timestamp};
use ui::{
    Button, ButtonSize, ButtonStyle, Color, Icon, IconName, IconSize, Label, LabelSize, div,
    h_flex, prelude::*, v_flex,
};
use workspace::item::{Item, TabContentParams};
use workspace::notifications::DetachAndPromptErr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitHubPullRequestRowMeta {
    pub title: SharedString,
    pub metadata: SharedString,
}

pub(crate) struct GitHubPullRequestView {
    repo_name_with_owner: SharedString,
    pull: GitHubPullRequest,
    git_panel: WeakEntity<GitPanel>,
    body_markdown: Entity<Markdown>,
    review_detail: GitHubPullRequestReviewDetailState,
    review_detail_task: Option<gpui::Task<()>>,
    checking_out: bool,
    viewing_changes: bool,
    focus_handle: FocusHandle,
}

enum GitHubPullRequestReviewDetailState {
    NotLoaded,
    Loading,
    Loaded(GitHubPullRequestReviewDetail),
    Error(SharedString),
}

struct GitHubPullRequestReviewDetail {
    timeline: Vec<GitHubPullRequestTimelineItem>,
    review_summary: GitHubPullRequestReviewSummary,
    checks_summary: GitHubPullRequestChecksSummary,
    errors: Vec<SharedString>,
}

#[derive(Default)]
struct GitHubPullRequestReviewSummary {
    approved: usize,
    changes_requested: usize,
    commented: usize,
    pending_reviewers: usize,
}

struct GitHubPullRequestChecksSummary {
    overall: GitHubPullRequestCheckState,
    items: Vec<GitHubPullRequestCheckItem>,
}

struct GitHubPullRequestCheckItem {
    name: SharedString,
    state: GitHubPullRequestCheckState,
    description: Option<SharedString>,
    url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitHubPullRequestCheckState {
    Passed,
    Failed,
    Pending,
    Skipped,
    Missing,
}

struct GitHubPullRequestTimelineItem {
    kind: GitHubPullRequestTimelineItemKind,
    author: SharedString,
    timestamp: Option<String>,
    title: SharedString,
    metadata: Option<SharedString>,
    markdown: Option<Entity<Markdown>>,
    url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitHubPullRequestTimelineItemKind {
    Commit,
    Comment,
    Review,
    ReviewComment,
}

struct GitHubPullRequestReviewDetailLoad {
    commits: Result<Vec<GitHubPullRequestCommit>, String>,
    issue_comments: Result<Vec<GitHubIssueComment>, String>,
    reviews: Result<Vec<GitHubPullRequestReview>, String>,
    review_comments: Result<Vec<GitHubPullRequestReviewComment>, String>,
    check_runs: Result<Vec<GitHubCheckRun>, String>,
    combined_status: Result<GitHubCombinedStatus, String>,
}

impl GitHubPullRequestView {
    pub(crate) fn new(
        repo_name_with_owner: SharedString,
        pull: GitHubPullRequest,
        git_panel: WeakEntity<GitPanel>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let body_markdown = Self::new_body_markdown(&pull, cx);
        Self {
            repo_name_with_owner,
            pull,
            git_panel,
            body_markdown,
            review_detail: GitHubPullRequestReviewDetailState::NotLoaded,
            review_detail_task: None,
            checking_out: false,
            viewing_changes: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub(crate) fn set_pull_request(
        &mut self,
        repo_name_with_owner: SharedString,
        pull: GitHubPullRequest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.repo_name_with_owner = repo_name_with_owner;
        self.pull = pull;
        self.checking_out = false;
        self.viewing_changes = false;
        self.body_markdown = Self::new_body_markdown(&self.pull, cx);
        self.review_detail = GitHubPullRequestReviewDetailState::NotLoaded;
        self.review_detail_task = None;
        cx.notify();
    }

    fn new_body_markdown(pull: &GitHubPullRequest, cx: &mut Context<Self>) -> Entity<Markdown> {
        let body = github_pull_request_body_text(pull);
        cx.new(|cx| Markdown::new(body, None, None, cx))
    }

    pub(crate) fn load_review_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.review_detail,
            GitHubPullRequestReviewDetailState::Loading
        ) {
            return;
        }

        let repo_name = self.repo_name_with_owner.to_string();
        let pull = self.pull.clone();
        let http_client = cx.http_client();
        self.review_detail = GitHubPullRequestReviewDetailState::Loading;
        self.review_detail_task = Some(cx.spawn_in(window, async move |this, cx| {
            let token = GitPanel::github_token(cx).await.token;
            let detail = load_github_pull_request_review_detail(
                &repo_name,
                &pull,
                token.as_deref(),
                http_client,
            )
            .await;

            this.update(cx, |this, cx| {
                this.review_detail_task = None;
                match detail {
                    Ok(detail) => {
                        this.review_detail = GitHubPullRequestReviewDetailState::Loaded(
                            this.build_review_detail(detail, cx),
                        );
                    }
                    Err(error) => {
                        this.review_detail =
                            GitHubPullRequestReviewDetailState::Error(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn checkout_pull_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo_name = self.repo_name_with_owner.clone();
        let pull = self.pull.clone();
        let pull_number = pull.number;
        let operation = self
            .git_panel
            .update(cx, |git_panel, cx| {
                git_panel.prepare_github_pull_request_checkout(pull, window, cx)
            })
            .ok()
            .flatten();
        let Some(operation) = operation else {
            return;
        };

        self.checking_out = true;
        cx.notify();

        let repo_name_for_task = repo_name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let remote_branch = format!("{}/{}", operation.remote_name, operation.local_branch);
            let fetch = operation.repository.update(cx, |repository, cx| {
                repository.fetch_refspec(
                    operation.remote_name,
                    operation.refspec,
                    operation.askpass,
                    cx,
                )
            });
            let result = async {
                fetch.await??;

                let checkout = operation
                    .repository
                    .update(cx, |repository, _| repository.change_branch(remote_branch));
                checkout.await??;

                anyhow::Ok(())
            }
            .await;

            this.update(cx, |this, cx| {
                this.checking_out = false;
                cx.notify();
            })
            .ok();

            result?;

            telemetry::event!(
                "GitHub Pull Request Checkout Finished",
                repo = repo_name_for_task.as_ref(),
                pull_request = pull_number
            );

            anyhow::Ok(())
        })
        .detach_and_prompt_err(
            "Failed to checkout pull request",
            window,
            cx,
            |error, _, _| Some(error.to_string()),
        );

        telemetry::event!(
            "GitHub Pull Request Checkout Started",
            repo = repo_name.as_ref(),
            pull_request = pull_number
        );
    }

    fn view_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repo_name = self.repo_name_with_owner.clone();
        let pull = self.pull.clone();
        let operation = self
            .git_panel
            .update(cx, |git_panel, cx| {
                git_panel.prepare_github_pull_request_changes(repo_name, pull, cx)
            })
            .ok()
            .flatten();
        let Some(operation) = operation else {
            return;
        };

        self.viewing_changes = true;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let token = GitPanel::github_token(cx).await.token;
            let result = async {
                let files = http_client::github::pull_request_files(
                    &operation.repo_name_with_owner,
                    operation.pull_number,
                    token.as_deref(),
                    operation.http_client.clone(),
                )
                .await?;
                let project = operation
                    .workspace
                    .update(cx, |workspace, _| workspace.project().clone());
                GitHubPrDiffView::build(
                    operation.repo_name_with_owner.into(),
                    operation.pull,
                    files,
                    token,
                    operation.http_client,
                    project,
                    operation.workspace.downgrade(),
                    cx,
                )
                .await?;

                anyhow::Ok(())
            }
            .await;

            this.update(cx, |this, cx| {
                this.viewing_changes = false;
                cx.notify();
            })
            .ok();

            result
        })
        .detach_and_prompt_err(
            "Failed to view pull request changes",
            window,
            cx,
            |error, _, _| Some(error.to_string()),
        );
    }
}

pub(crate) fn github_pull_request_row_meta(
    repo_name_with_owner: &str,
    pull: &GitHubPullRequest,
) -> GitHubPullRequestRowMeta {
    let updated = pull
        .updated_at
        .as_deref()
        .and_then(github_format_timestamp)
        .unwrap_or_else(|| "unknown".to_string());

    GitHubPullRequestRowMeta {
        title: pull.title.clone().into(),
        metadata: format!(
            "{} #{} · @{} · {}",
            repo_name_with_owner, pull.number, pull.user.login, updated
        )
        .into(),
    }
}

pub(crate) fn github_pull_request_state_label(pull: &GitHubPullRequest) -> &'static str {
    if pull.draft {
        "Draft"
    } else {
        match pull.state.as_str() {
            "closed" => "Closed",
            "open" => "Open",
            _ => "Unknown",
        }
    }
}

pub(crate) fn github_pull_request_diff_boxes(additions: u32, deletions: u32) -> (usize, usize) {
    const BOX_COUNT: usize = 5;
    let total = additions + deletions;
    if total == 0 {
        return (0, 0);
    }

    let added = ((additions as f32 / total as f32) * BOX_COUNT as f32).round() as usize;
    let added = added.min(BOX_COUNT);
    (added, BOX_COUNT - added)
}

pub(crate) fn github_pull_request_body_text(pull: &GitHubPullRequest) -> SharedString {
    pull.body
        .as_deref()
        .filter(|body| !body.trim().is_empty())
        .unwrap_or("No description provided.")
        .to_string()
        .into()
}

pub(crate) fn github_format_timestamp(timestamp: &str) -> Option<String> {
    let timestamp = OffsetDateTime::parse(timestamp, &Rfc3339).ok()?;
    let timezone = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    Some(format_localized_timestamp(
        timestamp,
        OffsetDateTime::now_utc(),
        timezone,
        TimestampFormat::Relative,
    ))
}

async fn load_github_pull_request_review_detail(
    repo_name_with_owner: &str,
    pull: &GitHubPullRequest,
    token: Option<&str>,
    http_client: Arc<dyn HttpClient>,
) -> anyhow::Result<GitHubPullRequestReviewDetailLoad> {
    let commits = http_client::github::pull_request_commits(
        repo_name_with_owner,
        pull.number,
        token,
        http_client.clone(),
    )
    .await
    .map_err(|error| error.to_string());
    let issue_comments = http_client::github::pull_request_issue_comments(
        repo_name_with_owner,
        pull.number,
        token,
        http_client.clone(),
    )
    .await
    .map_err(|error| error.to_string());
    let reviews = http_client::github::pull_request_reviews(
        repo_name_with_owner,
        pull.number,
        token,
        http_client.clone(),
    )
    .await
    .map_err(|error| error.to_string());
    let review_comments = http_client::github::pull_request_review_comments(
        repo_name_with_owner,
        pull.number,
        token,
        http_client.clone(),
    )
    .await
    .map_err(|error| error.to_string());
    let check_runs = http_client::github::commit_check_runs(
        repo_name_with_owner,
        &pull.head.sha,
        token,
        http_client.clone(),
    )
    .await
    .map_err(|error| error.to_string());
    let combined_status = http_client::github::commit_status(
        repo_name_with_owner,
        &pull.head.sha,
        token,
        http_client,
    )
    .await
    .map_err(|error| error.to_string());

    Ok(GitHubPullRequestReviewDetailLoad {
        commits,
        issue_comments,
        reviews,
        review_comments,
        check_runs,
        combined_status,
    })
}

impl GitHubPullRequestView {
    fn build_review_detail(
        &self,
        detail: GitHubPullRequestReviewDetailLoad,
        cx: &mut Context<Self>,
    ) -> GitHubPullRequestReviewDetail {
        let mut errors = Vec::new();
        let mut timeline = Vec::new();

        match detail.commits {
            Ok(commits) => timeline.extend(commits.into_iter().map(github_pr_commit_timeline_item)),
            Err(error) => errors.push(format!("Commits: {error}").into()),
        }

        match detail.issue_comments {
            Ok(comments) => timeline.extend(
                comments
                    .into_iter()
                    .map(|comment| github_pr_issue_comment_timeline_item(comment, cx)),
            ),
            Err(error) => errors.push(format!("Comments: {error}").into()),
        }

        let reviews = match detail.reviews {
            Ok(reviews) => {
                timeline.extend(
                    reviews
                        .iter()
                        .cloned()
                        .map(|review| github_pr_review_timeline_item(review, cx)),
                );
                reviews
            }
            Err(error) => {
                errors.push(format!("Reviews: {error}").into());
                Vec::new()
            }
        };

        match detail.review_comments {
            Ok(comments) => timeline.extend(
                comments
                    .into_iter()
                    .map(|comment| github_pr_review_comment_timeline_item(comment, cx)),
            ),
            Err(error) => errors.push(format!("Review comments: {error}").into()),
        }

        let check_runs = match detail.check_runs {
            Ok(check_runs) => check_runs,
            Err(error) => {
                errors.push(format!("Check runs: {error}").into());
                Vec::new()
            }
        };
        let combined_status = match detail.combined_status {
            Ok(status) => Some(status),
            Err(error) => {
                errors.push(format!("Commit status: {error}").into());
                None
            }
        };

        timeline.sort_by(|left, right| {
            left.timestamp
                .as_deref()
                .unwrap_or_default()
                .cmp(right.timestamp.as_deref().unwrap_or_default())
        });

        GitHubPullRequestReviewDetail {
            timeline,
            review_summary: github_pr_review_summary(&reviews, self.pull.requested_reviewers.len()),
            checks_summary: github_pr_checks_summary(check_runs, combined_status),
            errors,
        }
    }
}

fn markdown_entity(text: String, cx: &mut Context<GitHubPullRequestView>) -> Entity<Markdown> {
    cx.new(|cx| Markdown::new(text.into(), None, None, cx))
}

fn github_pr_commit_timeline_item(
    commit: GitHubPullRequestCommit,
) -> GitHubPullRequestTimelineItem {
    let title = commit
        .commit
        .message
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let author = commit
        .author
        .map(|author| author.login)
        .or_else(|| {
            commit
                .commit
                .author
                .as_ref()
                .and_then(|author| author.name.clone())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let short_sha = commit
        .sha
        .get(0..7)
        .unwrap_or(commit.sha.as_str())
        .to_string();

    GitHubPullRequestTimelineItem {
        kind: GitHubPullRequestTimelineItemKind::Commit,
        author: author.into(),
        timestamp: commit.commit.author.and_then(|author| author.date),
        title: title.into(),
        metadata: Some(short_sha.into()),
        markdown: None,
        url: Some(commit.html_url),
    }
}

fn github_pr_issue_comment_timeline_item(
    comment: GitHubIssueComment,
    cx: &mut Context<GitHubPullRequestView>,
) -> GitHubPullRequestTimelineItem {
    GitHubPullRequestTimelineItem {
        kind: GitHubPullRequestTimelineItemKind::Comment,
        author: comment.user.login.into(),
        timestamp: Some(comment.created_at),
        title: "Commented".into(),
        metadata: None,
        markdown: Some(markdown_entity(comment.body, cx)),
        url: Some(comment.html_url),
    }
}

fn github_pr_review_timeline_item(
    review: GitHubPullRequestReview,
    cx: &mut Context<GitHubPullRequestView>,
) -> GitHubPullRequestTimelineItem {
    let title = github_pr_review_state_label(&review.state).to_string();
    let markdown = review
        .body
        .filter(|body| !body.trim().is_empty())
        .map(|body| markdown_entity(body, cx));

    GitHubPullRequestTimelineItem {
        kind: GitHubPullRequestTimelineItemKind::Review,
        author: review.user.login.into(),
        timestamp: review.submitted_at,
        title: title.into(),
        metadata: Some(review.state.into()),
        markdown,
        url: review.html_url,
    }
}

fn github_pr_review_comment_timeline_item(
    comment: GitHubPullRequestReviewComment,
    cx: &mut Context<GitHubPullRequestView>,
) -> GitHubPullRequestTimelineItem {
    let line = comment
        .line
        .or(comment.original_line)
        .map(|line| format!(":{}", line))
        .unwrap_or_default();
    GitHubPullRequestTimelineItem {
        kind: GitHubPullRequestTimelineItemKind::ReviewComment,
        author: comment.user.login.into(),
        timestamp: Some(comment.created_at),
        title: "Review comment".into(),
        metadata: Some(format!("{}{}", comment.path, line).into()),
        markdown: Some(markdown_entity(comment.body, cx)),
        url: Some(comment.html_url),
    }
}

fn github_pr_review_state_label(state: &str) -> &'static str {
    match state {
        "APPROVED" => "Approved",
        "CHANGES_REQUESTED" => "Requested changes",
        "COMMENTED" => "Reviewed",
        "DISMISSED" => "Review dismissed",
        "PENDING" => "Review pending",
        _ => "Reviewed",
    }
}

fn github_pr_review_summary(
    reviews: &[GitHubPullRequestReview],
    pending_reviewers: usize,
) -> GitHubPullRequestReviewSummary {
    let mut summary = GitHubPullRequestReviewSummary {
        pending_reviewers,
        ..Default::default()
    };
    for review in reviews {
        match review.state.as_str() {
            "APPROVED" => summary.approved += 1,
            "CHANGES_REQUESTED" => summary.changes_requested += 1,
            "COMMENTED" => summary.commented += 1,
            _ => {}
        }
    }
    summary
}

fn github_pr_checks_summary(
    check_runs: Vec<GitHubCheckRun>,
    combined_status: Option<GitHubCombinedStatus>,
) -> GitHubPullRequestChecksSummary {
    let mut items = Vec::new();
    items.extend(check_runs.into_iter().map(|run| {
        let state = check_run_state(&run);
        GitHubPullRequestCheckItem {
            name: run.name.into(),
            state,
            description: run.conclusion.or(Some(run.status)).map(SharedString::from),
            url: Some(run.html_url),
        }
    }));

    if let Some(status) = combined_status {
        items.extend(status.statuses.into_iter().map(|status| {
            let state = commit_status_state(&status);
            GitHubPullRequestCheckItem {
                name: status.context.into(),
                state,
                description: status.description.map(SharedString::from),
                url: status.target_url,
            }
        }));
        if items.is_empty() && status.total_count > 0 {
            items.push(GitHubPullRequestCheckItem {
                name: "Commit status".into(),
                state: combined_status_state(&status.state),
                description: Some(status.state.into()),
                url: None,
            });
        }
    }

    let overall = checks_overall_state(&items);
    GitHubPullRequestChecksSummary { overall, items }
}

fn check_run_state(run: &GitHubCheckRun) -> GitHubPullRequestCheckState {
    if run.status != "completed" {
        return GitHubPullRequestCheckState::Pending;
    }
    match run.conclusion.as_deref() {
        Some("success") | Some("neutral") => GitHubPullRequestCheckState::Passed,
        Some("skipped") => GitHubPullRequestCheckState::Skipped,
        Some("cancelled") | Some("timed_out") | Some("failure") | Some("action_required") => {
            GitHubPullRequestCheckState::Failed
        }
        _ => GitHubPullRequestCheckState::Pending,
    }
}

fn commit_status_state(status: &GitHubCommitStatus) -> GitHubPullRequestCheckState {
    combined_status_state(&status.state)
}

fn combined_status_state(state: &str) -> GitHubPullRequestCheckState {
    match state {
        "success" => GitHubPullRequestCheckState::Passed,
        "failure" | "error" => GitHubPullRequestCheckState::Failed,
        "pending" => GitHubPullRequestCheckState::Pending,
        _ => GitHubPullRequestCheckState::Missing,
    }
}

fn checks_overall_state(items: &[GitHubPullRequestCheckItem]) -> GitHubPullRequestCheckState {
    if items.is_empty() {
        return GitHubPullRequestCheckState::Missing;
    }
    if items
        .iter()
        .any(|item| item.state == GitHubPullRequestCheckState::Failed)
    {
        return GitHubPullRequestCheckState::Failed;
    }
    if items
        .iter()
        .any(|item| item.state == GitHubPullRequestCheckState::Pending)
    {
        return GitHubPullRequestCheckState::Pending;
    }
    if items
        .iter()
        .all(|item| item.state == GitHubPullRequestCheckState::Skipped)
    {
        return GitHubPullRequestCheckState::Skipped;
    }
    GitHubPullRequestCheckState::Passed
}

fn check_state_label(state: GitHubPullRequestCheckState) -> &'static str {
    match state {
        GitHubPullRequestCheckState::Passed => "Passed",
        GitHubPullRequestCheckState::Failed => "Failed",
        GitHubPullRequestCheckState::Pending => "Pending",
        GitHubPullRequestCheckState::Skipped => "Skipped",
        GitHubPullRequestCheckState::Missing => "Missing",
    }
}

fn check_state_color(state: GitHubPullRequestCheckState) -> Color {
    match state {
        GitHubPullRequestCheckState::Passed => Color::Success,
        GitHubPullRequestCheckState::Failed => Color::Error,
        GitHubPullRequestCheckState::Pending => Color::Warning,
        GitHubPullRequestCheckState::Skipped | GitHubPullRequestCheckState::Missing => Color::Muted,
    }
}

impl Render for GitHubPullRequestView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pull = &self.pull;
        let row_meta = github_pull_request_row_meta(&self.repo_name_with_owner, pull);
        let state_label = github_pull_request_state_label(pull);
        let state_color = if pull.draft {
            Color::Warning
        } else if pull.state == "open" {
            Color::Success
        } else {
            Color::Muted
        };
        let created = pull
            .created_at
            .as_deref()
            .and_then(github_format_timestamp)
            .unwrap_or_else(|| "unknown".to_string());
        let updated = pull
            .updated_at
            .as_deref()
            .and_then(github_format_timestamp)
            .unwrap_or_else(|| "unknown".to_string());
        let commits = pull.commits.unwrap_or_default();
        let changed_files = pull.changed_files.unwrap_or_default();
        let additions = pull.additions.unwrap_or_default();
        let deletions = pull.deletions.unwrap_or_default();
        let (added_boxes, deleted_boxes) = github_pull_request_diff_boxes(additions, deletions);

        v_flex()
            .id("github-pr-detail")
            .size_full()
            .overflow_y_scroll()
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .items_start()
                    .gap_8()
                    .p_8()
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_6()
                            .child(self.render_header(row_meta, state_label, state_color, cx))
                            .child(self.render_stats_bar(
                                commits,
                                changed_files,
                                additions,
                                deletions,
                                added_boxes,
                                deleted_boxes,
                                window,
                                cx,
                            ))
                            .child(self.render_body(window, cx))
                            .child(self.render_review_detail(window, cx)),
                    )
                    .child(self.render_sidebar(created, updated, cx)),
            )
    }
}

impl GitHubPullRequestView {
    fn render_header(
        &self,
        row_meta: GitHubPullRequestRowMeta,
        state_label: &'static str,
        state_color: Color,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Label::new("Pull Requests")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new("/").size(LabelSize::Small).color(Color::Muted))
                    .child(
                        Label::new(self.repo_name_with_owner.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new("/").size(LabelSize::Small).color(Color::Muted))
                    .child(
                        Label::new(format!("#{}", self.pull.number))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::PullRequest)
                            .size(IconSize::Medium)
                            .color(state_color),
                    )
                    .child(Label::new(row_meta.title).size(LabelSize::Large).truncate()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(self.render_pill(state_label, state_color, cx))
                    .child(Label::new(format!("@{}", self.pull.user.login)).color(Color::Default))
                    .child(Label::new("wants to merge into").color(Color::Muted))
                    .child(self.render_ref_pill(self.pull.base.ref_name.as_str(), cx))
                    .child(Label::new("from").color(Color::Muted))
                    .child(self.render_ref_pill(self.pull.head.ref_name.as_str(), cx)),
            )
    }

    fn render_stats_bar(
        &self,
        commits: u32,
        changed_files: u32,
        additions: u32,
        deletions: u32,
        added_boxes: usize,
        deleted_boxes: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .justify_between()
            .gap_3()
            .rounded_lg()
            .bg(cx.theme().colors().element_background)
            .border_1()
            .border_color(cx.theme().colors().border.opacity(0.5))
            .px_4()
            .py_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(self.render_stat(IconName::GitCommit, commits, "commits"))
                    .child(self.render_stat(IconName::File, changed_files, "files changed"))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Label::new(format!("+{additions}")).color(Color::Success))
                            .child(Label::new(format!("-{deletions}")).color(Color::Error))
                            .child(self.render_diff_boxes(added_boxes, deleted_boxes, cx)),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("github-pr-checkout", "Checkout PR")
                            .style(ButtonStyle::Filled)
                            .size(ButtonSize::Compact)
                            .loading(self.checking_out)
                            .disabled(self.checking_out)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.checkout_pull_request(window, cx);
                            })),
                    )
                    .child(
                        Button::new("github-pr-view-changes", "View Changes")
                            .size(ButtonSize::Compact)
                            .loading(self.viewing_changes)
                            .disabled(self.viewing_changes)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.view_changes(window, cx);
                            })),
                    )
                    .child(
                        Button::new("github-pr-open-browser", "View on GitHub")
                            .size(ButtonSize::Compact)
                            .start_icon(Icon::new(IconName::Github).size(IconSize::Small))
                            .end_icon(Icon::new(IconName::ArrowUpRight).size(IconSize::Small))
                            .on_click({
                                let url = self.pull.html_url.clone();
                                move |_, _, cx| cx.open_url(&url)
                            }),
                    ),
            )
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border.opacity(0.5))
            .p_4()
            .child(Label::new("Summary").size(LabelSize::Large))
            .child(div().min_h_16().child(MarkdownElement::new(
                self.body_markdown.clone(),
                self.markdown_style(window, cx),
            )))
    }

    fn markdown_style(&self, window: &Window, cx: &mut Context<Self>) -> MarkdownStyle {
        MarkdownStyle::themed(MarkdownFont::Editor, window, cx)
    }

    fn render_review_detail(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        match &self.review_detail {
            GitHubPullRequestReviewDetailState::NotLoaded
            | GitHubPullRequestReviewDetailState::Loading => v_flex()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().colors().border.opacity(0.5))
                .p_4()
                .child(Label::new("Loading pull request review details...").color(Color::Muted))
                .into_any_element(),
            GitHubPullRequestReviewDetailState::Error(error) => v_flex()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().colors().border.opacity(0.5))
                .p_4()
                .child(Label::new("Failed to load pull request review details"))
                .child(Label::new(error.clone()).color(Color::Error))
                .child(
                    Button::new("github-pr-review-detail-retry", "Retry")
                        .size(ButtonSize::Compact)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.load_review_detail(window, cx);
                        })),
                )
                .into_any_element(),
            GitHubPullRequestReviewDetailState::Loaded(detail) => v_flex()
                .gap_6()
                .when(!detail.errors.is_empty(), |this| {
                    this.child(
                        v_flex()
                            .gap_2()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().status().warning_border)
                            .bg(cx.theme().status().warning_background.opacity(0.4))
                            .p_4()
                            .child(Label::new("Some GitHub data could not be loaded"))
                            .children(detail.errors.iter().map(|error| {
                                Label::new(error.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                            })),
                    )
                })
                .child(self.render_activity_section(detail, window, cx))
                .child(self.render_review_section(&detail.review_summary, cx))
                .child(self.render_checks_section(&detail.checks_summary, cx))
                .into_any_element(),
        }
    }

    fn render_activity_section(
        &self,
        detail: &GitHubPullRequestReviewDetail,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let markdown_style = self.markdown_style(window, cx);
        v_flex()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border.opacity(0.5))
            .p_4()
            .child(
                h_flex()
                    .justify_between()
                    .child(Label::new("Activity").size(LabelSize::Large))
                    .child(Label::new(detail.timeline.len().to_string()).color(Color::Muted)),
            )
            .when(detail.timeline.is_empty(), |this| {
                this.child(Label::new("No activity loaded.").color(Color::Muted))
            })
            .children(
                detail
                    .timeline
                    .iter()
                    .map(|item| self.render_timeline_item(item, markdown_style.clone(), cx)),
            )
    }

    fn render_timeline_item(
        &self,
        item: &GitHubPullRequestTimelineItem,
        markdown_style: MarkdownStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let icon = match item.kind {
            GitHubPullRequestTimelineItemKind::Commit => IconName::GitCommit,
            GitHubPullRequestTimelineItemKind::Comment => IconName::Chat,
            GitHubPullRequestTimelineItemKind::Review => IconName::Eye,
            GitHubPullRequestTimelineItemKind::ReviewComment => IconName::File,
        };
        let timestamp = item
            .timestamp
            .as_deref()
            .and_then(github_format_timestamp)
            .unwrap_or_else(|| "unknown".to_string());

        v_flex()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().colors().border.opacity(0.4))
            .pt_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .child(Label::new(item.title.clone()).truncate())
                            .when_some(item.metadata.clone(), |this, metadata| {
                                this.child(
                                    Label::new(metadata)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .child(Label::new(format!("@{}", item.author)).color(Color::Muted))
                    .child(Label::new(timestamp).color(Color::Muted))
                    .when_some(item.url.clone(), |this, url| {
                        this.child(
                            Button::new(format!("github-pr-timeline-open-{url}"), "Open")
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Subtle)
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .when_some(item.markdown.clone(), |this, markdown| {
                this.child(
                    div()
                        .pl_6()
                        .child(MarkdownElement::new(markdown, markdown_style)),
                )
            })
            .into_any_element()
    }

    fn render_review_section(
        &self,
        summary: &GitHubPullRequestReviewSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = if summary.changes_requested > 0 {
            "Changes requested"
        } else if summary.approved > 0 {
            "Approved"
        } else if summary.pending_reviewers > 0 {
            "Review required"
        } else {
            "No reviewers added"
        };
        let color = if summary.changes_requested > 0 {
            Color::Error
        } else if summary.approved > 0 {
            Color::Success
        } else if summary.pending_reviewers > 0 {
            Color::Warning
        } else {
            Color::Muted
        };

        v_flex()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border.opacity(0.5))
            .p_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Eye).size(IconSize::Small).color(color))
                    .child(Label::new(title).size(LabelSize::Large)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(self.render_review_count("Approved", summary.approved))
                    .child(self.render_review_count("Changes requested", summary.changes_requested))
                    .child(self.render_review_count("Commented", summary.commented))
                    .child(self.render_review_count("Pending", summary.pending_reviewers)),
            )
    }

    fn render_review_count(&self, label: &'static str, count: usize) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Label::new(count.to_string()))
            .child(Label::new(label).color(Color::Muted))
    }

    fn render_checks_section(
        &self,
        summary: &GitHubPullRequestChecksSummary,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = match summary.overall {
            GitHubPullRequestCheckState::Passed => "All checks have passed",
            GitHubPullRequestCheckState::Failed => "Some checks failed",
            GitHubPullRequestCheckState::Pending => "Checks are pending",
            GitHubPullRequestCheckState::Skipped => "Checks skipped",
            GitHubPullRequestCheckState::Missing => "No checks found",
        };
        let color = check_state_color(summary.overall);

        v_flex()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border.opacity(0.5))
            .p_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Check)
                            .size(IconSize::Small)
                            .color(color),
                    )
                    .child(Label::new(title).size(LabelSize::Large)),
            )
            .when(summary.items.is_empty(), |this| {
                this.child(
                    Label::new("No check runs or commit statuses found.").color(Color::Muted),
                )
            })
            .children(summary.items.iter().map(|item| {
                h_flex()
                    .justify_between()
                    .gap_3()
                    .border_t_1()
                    .border_color(cx.theme().colors().border.opacity(0.4))
                    .pt_2()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(
                                Icon::new(IconName::Check)
                                    .size(IconSize::Small)
                                    .color(check_state_color(item.state)),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(Label::new(item.name.clone()).truncate())
                                    .when_some(item.description.clone(), |this, description| {
                                        this.child(
                                            Label::new(description)
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .truncate(),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        Label::new(check_state_label(item.state))
                            .color(check_state_color(item.state)),
                    )
                    .when_some(item.url.clone(), |this, url| {
                        this.child(
                            Button::new(format!("github-pr-check-open-{url}"), "Open")
                                .size(ButtonSize::Compact)
                                .style(ButtonStyle::Subtle)
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    })
            }))
    }

    fn render_sidebar(
        &self,
        created: String,
        updated: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_64()
            .gap_5()
            .child(self.render_label_section(cx))
            .child(self.render_user_section("Reviewers", &self.pull.requested_reviewers, cx))
            .child(self.render_participants(cx))
            .child(self.render_details(created, updated, cx))
    }

    fn render_label_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(self.render_sidebar_heading("Labels"))
            .when(self.pull.labels.is_empty(), |this| {
                this.child(
                    Label::new("No labels")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(self.pull.labels.iter().map(|label| {
                        div()
                            .rounded_md()
                            .bg(cx.theme().colors().element_background)
                            .px_2()
                            .py_0p5()
                            .child(Label::new(label.name.clone()).size(LabelSize::Small))
                    })),
            )
    }

    fn render_user_section(
        &self,
        title: &'static str,
        users: &[http_client::github::GitHubUser],
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(self.render_sidebar_heading(title))
            .when(users.is_empty(), |this| {
                this.child(
                    Label::new(format!("No {}", title.to_lowercase()))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .children(users.iter().map(|user| {
                Label::new(format!("@{}", user.login))
                    .size(LabelSize::Small)
                    .color(Color::Default)
            }))
    }

    fn render_participants(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(self.render_sidebar_heading("Participants"))
            .child(Label::new(format!("@{}", self.pull.user.login)).size(LabelSize::Small))
    }

    fn render_details(
        &self,
        created: String,
        updated: String,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(self.render_sidebar_heading("Details"))
            .child(self.render_detail_row("Created", created))
            .child(self.render_detail_row("Updated", updated))
            .child(self.render_detail_row("Comments", self.pull.comments.to_string()))
            .child(self.render_detail_row("Review comments", self.pull.review_comments.to_string()))
    }

    fn render_sidebar_heading(&self, title: &'static str) -> impl IntoElement {
        Label::new(title)
            .size(LabelSize::Small)
            .color(Color::Muted)
            .weight(FontWeight::BOLD)
    }

    fn render_detail_row(&self, label: &'static str, value: String) -> impl IntoElement {
        h_flex()
            .justify_between()
            .gap_3()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(
                Label::new(value)
                    .size(LabelSize::Small)
                    .color(Color::Default),
            )
    }

    fn render_stat(&self, icon: IconName, count: u32, label: &'static str) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
            .child(Label::new(count.to_string()).color(Color::Default))
            .child(Label::new(label).color(Color::Muted))
    }

    fn render_pill(
        &self,
        label: &'static str,
        color: Color,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .rounded_md()
            .bg(cx.theme().colors().element_background)
            .px_2()
            .py_0p5()
            .child(Label::new(label).size(LabelSize::Small).color(color))
    }

    fn render_ref_pill(&self, label: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .rounded_md()
            .bg(cx.theme().colors().element_background)
            .px_2()
            .py_0p5()
            .child(
                Label::new(label.to_string())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }

    fn render_diff_boxes(
        &self,
        added_boxes: usize,
        deleted_boxes: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted_boxes = 5usize.saturating_sub(added_boxes + deleted_boxes);
        h_flex()
            .gap_0p5()
            .children(
                (0..added_boxes)
                    .map(|_| div().size_2().rounded_sm().bg(cx.theme().status().created)),
            )
            .children(
                (0..deleted_boxes)
                    .map(|_| div().size_2().rounded_sm().bg(cx.theme().status().deleted)),
            )
            .children((0..muted_boxes).map(|_| {
                div()
                    .size_2()
                    .rounded_sm()
                    .bg(cx.theme().colors().text_muted.opacity(0.3))
            }))
    }
}

impl Focusable for GitHubPullRequestView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<()> for GitHubPullRequestView {}

impl Item for GitHubPullRequestView {
    type Event = ();

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::PullRequest).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        format!("#{} {}", self.pull.number, self.pull.title).into()
    }

    fn tab_content(
        &self,
        params: TabContentParams,
        _window: &Window,
        cx: &App,
    ) -> gpui::AnyElement {
        Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(self.pull.html_url.clone().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::github::{
        GitHubCheckRun, GitHubCombinedStatus, GitHubCommitStatus, GitHubPullRequestBranch,
        GitHubPullRequestReview, GitHubRepositoryRef, GitHubUser,
    };

    fn pull_request() -> GitHubPullRequest {
        GitHubPullRequest {
            number: 42,
            title: "Improve GitHub PR UI".to_string(),
            html_url: "https://github.com/owner/repo/pull/42".to_string(),
            state: "open".to_string(),
            user: GitHubUser {
                login: "octo".to_string(),
            },
            base: GitHubPullRequestBranch {
                ref_name: "main".to_string(),
                sha: "base-sha".to_string(),
                repo: Some(GitHubRepositoryRef {
                    full_name: "owner/repo".to_string(),
                    clone_url: None,
                    ssh_url: None,
                }),
            },
            head: GitHubPullRequestBranch {
                ref_name: "feature/pr-ui".to_string(),
                sha: "head-sha".to_string(),
                repo: Some(GitHubRepositoryRef {
                    full_name: "owner/repo".to_string(),
                    clone_url: None,
                    ssh_url: None,
                }),
            },
            draft: false,
            updated_at: None,
            created_at: None,
            labels: Vec::new(),
            body: None,
            requested_reviewers: Vec::new(),
            comments: 3,
            review_comments: 2,
            commits: Some(4),
            changed_files: Some(5),
            additions: Some(12),
            deletions: Some(4),
        }
    }

    #[test]
    fn test_github_pull_request_row_meta_is_compact() {
        let pull = pull_request();
        let row = github_pull_request_row_meta("owner/repo", &pull);

        assert_eq!(row.title.as_ref(), "Improve GitHub PR UI");
        assert_eq!(row.metadata.as_ref(), "owner/repo #42 · @octo · unknown");
    }

    #[test]
    fn test_github_pull_request_state_label_prefers_draft() {
        let mut pull = pull_request();
        assert_eq!(github_pull_request_state_label(&pull), "Open");

        pull.draft = true;
        assert_eq!(github_pull_request_state_label(&pull), "Draft");

        pull.draft = false;
        pull.state = "closed".to_string();
        assert_eq!(github_pull_request_state_label(&pull), "Closed");
    }

    #[test]
    fn test_github_pull_request_diff_boxes_splits_additions_and_deletions() {
        assert_eq!(github_pull_request_diff_boxes(0, 0), (0, 0));
        assert_eq!(github_pull_request_diff_boxes(12, 4), (4, 1));
        assert_eq!(github_pull_request_diff_boxes(0, 7), (0, 5));
    }

    #[test]
    fn test_github_pull_request_review_summary_counts_states() {
        let reviews = vec![
            review("APPROVED"),
            review("CHANGES_REQUESTED"),
            review("COMMENTED"),
            review("DISMISSED"),
        ];

        let summary = github_pr_review_summary(&reviews, 2);

        assert_eq!(summary.approved, 1);
        assert_eq!(summary.changes_requested, 1);
        assert_eq!(summary.commented, 1);
        assert_eq!(summary.pending_reviewers, 2);
    }

    #[test]
    fn test_github_pull_request_checks_summary_prefers_failures() {
        let summary = github_pr_checks_summary(
            vec![
                check_run("unit", "completed", Some("success")),
                check_run("lint", "completed", Some("failure")),
            ],
            Some(GitHubCombinedStatus {
                state: "success".to_string(),
                total_count: 1,
                statuses: vec![GitHubCommitStatus {
                    context: "legacy-ci".to_string(),
                    state: "success".to_string(),
                    description: Some("passed".to_string()),
                    target_url: None,
                    updated_at: None,
                }],
            }),
        );

        assert_eq!(summary.overall, GitHubPullRequestCheckState::Failed);
        assert_eq!(summary.items.len(), 3);
    }

    #[test]
    fn test_github_pull_request_checks_summary_handles_missing_checks() {
        let summary = github_pr_checks_summary(Vec::new(), None);

        assert_eq!(summary.overall, GitHubPullRequestCheckState::Missing);
        assert!(summary.items.is_empty());
    }

    fn review(state: &str) -> GitHubPullRequestReview {
        GitHubPullRequestReview {
            id: 1,
            html_url: None,
            user: GitHubUser {
                login: "reviewer".to_string(),
            },
            state: state.to_string(),
            body: None,
            submitted_at: None,
        }
    }

    fn check_run(name: &str, status: &str, conclusion: Option<&str>) -> GitHubCheckRun {
        GitHubCheckRun {
            id: 1,
            name: name.to_string(),
            html_url: format!("https://github.com/owner/repo/actions/runs/{name}"),
            status: status.to_string(),
            conclusion: conclusion.map(ToString::to_string),
            started_at: None,
            completed_at: None,
        }
    }
}
