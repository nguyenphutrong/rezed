use crate::git_panel::{GitPanel, github_pull_request_patch_text, open_output};
use editor::{Editor, EditorElement, EditorStyle};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Styled, TextStyle,
    WeakEntity, Window, relative, rems,
};
use http_client::github::GitHubPullRequest;
use language::Buffer;
use settings::Settings;
use theme_settings::ThemeSettings;
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
    body_editor: Entity<Editor>,
    checking_out: bool,
    viewing_changes: bool,
    focus_handle: FocusHandle,
}

impl GitHubPullRequestView {
    pub(crate) fn new(
        repo_name_with_owner: SharedString,
        pull: GitHubPullRequest,
        git_panel: WeakEntity<GitPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let body_editor = Self::new_body_editor(&pull, window, cx);
        Self {
            repo_name_with_owner,
            pull,
            git_panel,
            body_editor,
            checking_out: false,
            viewing_changes: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub(crate) fn set_pull_request(
        &mut self,
        repo_name_with_owner: SharedString,
        pull: GitHubPullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.repo_name_with_owner = repo_name_with_owner;
        self.pull = pull;
        self.checking_out = false;
        self.viewing_changes = false;
        self.body_editor = Self::new_body_editor(&self.pull, window, cx);
        cx.notify();
    }

    fn new_body_editor(
        pull: &GitHubPullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        let body = github_pull_request_body_text(pull);
        let buffer = cx.new(|cx| Buffer::local(body.as_ref(), cx));
        buffer.update(cx, |buffer, cx| {
            buffer.set_capability(language::Capability::ReadOnly, cx);
        });
        cx.new(|cx| {
            let mut editor = Editor::for_buffer(buffer, None, window, cx);
            editor.set_read_only(true);
            editor
        })
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
                    operation.http_client,
                )
                .await?;
                let patch_text = github_pull_request_patch_text(
                    &operation.repo_name_with_owner,
                    operation.pull_number,
                    &files,
                );

                operation.workspace.update_in(cx, |workspace, window, cx| {
                    open_output(
                        format!(
                            "github pr {} #{}",
                            operation.repo_name_with_owner, operation.pull_number
                        ),
                        workspace,
                        &patch_text,
                        window,
                        cx,
                    );
                })?;

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
                            .child(self.render_body(cx)),
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

    fn render_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border.opacity(0.5))
            .p_4()
            .child(Label::new("Summary").size(LabelSize::Large))
            .child(div().min_h_32().child(EditorElement::new(
                &self.body_editor,
                self.body_editor_style(cx),
            )))
    }

    fn body_editor_style(&self, cx: &mut Context<Self>) -> EditorStyle {
        let settings = ThemeSettings::get_global(cx);
        EditorStyle {
            background: cx.theme().colors().editor_background,
            local_player: cx.theme().players().local(),
            text: TextStyle {
                color: cx.theme().colors().text,
                font_family: settings.ui_font.family.clone(),
                font_features: settings.ui_font.features.clone(),
                font_fallbacks: settings.ui_font.fallbacks.clone(),
                font_size: rems(0.875).into(),
                font_weight: settings.ui_font.weight,
                line_height: relative(1.5),
                ..Default::default()
            },
            ..Default::default()
        }
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
    use http_client::github::{GitHubPullRequestBranch, GitHubRepositoryRef, GitHubUser};

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
}
