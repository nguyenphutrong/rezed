use crate::virtual_diff::{
    VirtualDiffEntry, VirtualDiffFile, build_virtual_buffer, build_virtual_buffer_diff,
    diff_excerpt_ranges, insert_diff_excerpts,
};
use anyhow::{Result, anyhow};
use buffer_diff::BufferDiff;
use editor::{Editor, EditorEvent, EditorSettings, SplittableEditor};
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Styled, WeakEntity,
    Window, rems,
};
use http_client::{
    HttpClient,
    github::{GitHubPullRequest, GitHubPullRequestBranch, GitHubPullRequestFile},
};
use language::{Buffer, Capability};
use multi_buffer::{MultiBuffer, PathKey};
use project::{Project, ProjectPath};
use settings::Settings;
use std::{any::TypeId, sync::Arc};
use ui::{Color, Icon, IconName, IconSize, Label, LabelSize, div, h_flex, prelude::*, v_flex};
use util::rel_path::RelPath;
use workspace::{
    Item, ItemHandle, ItemNavHistory, ToolbarItemLocation, Workspace,
    item::{ItemEvent, TabContentParams},
    searchable::SearchableItemHandle,
};

pub(crate) struct GitHubPrDiffView {
    repo_name_with_owner: SharedString,
    pull_number: u64,
    title: SharedString,
    files: Vec<GitHubPrDiffFile>,
    editor: Entity<SplittableEditor>,
    focus_handle: FocusHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitHubPrDiffFile {
    pub filename: SharedString,
    pub old_filename: SharedString,
    pub status: SharedString,
    pub additions: u32,
    pub deletions: u32,
    pub path_key: PathKey,
    pub unsupported_reason: Option<SharedString>,
    pub preview_note: Option<SharedString>,
}

struct GitHubPrDiffEntry {
    file: GitHubPrDiffFile,
    new_buffer: Entity<Buffer>,
    diff: Entity<BufferDiff>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedGitHubPrDiffFile {
    pub filename: String,
    pub old_filename: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub old_text: String,
    pub new_text: String,
    pub unsupported_reason: Option<String>,
    pub preview_note: Option<String>,
}

impl GitHubPrDiffView {
    pub(crate) async fn build(
        repo_name_with_owner: SharedString,
        pull: GitHubPullRequest,
        files: Vec<GitHubPullRequestFile>,
        token: Option<String>,
        http_client: Arc<dyn HttpClient>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        cx: &mut AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        let mut entries = Vec::new();
        let mut diff_files = Vec::new();
        let language_registry = project.read_with(cx, |project, _| project.languages().clone());
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .ok_or_else(|| anyhow!("project has no worktrees"))?;

        let mut parsed_files =
            futures::stream::iter(files.into_iter().enumerate().map(|(index, file)| {
                let repo_name_with_owner = repo_name_with_owner.to_string();
                let pull = pull.clone();
                let token = token.clone();
                let http_client = http_client.clone();
                async move {
                    let parsed = resolve_github_pr_diff_file(
                        &repo_name_with_owner,
                        &pull,
                        file,
                        token.as_deref(),
                        http_client,
                    )
                    .await;
                    (index, parsed)
                }
            }))
            .buffer_unordered(6)
            .collect::<Vec<_>>()
            .await;
        parsed_files.sort_by_key(|(index, _)| *index);

        for (index, parsed) in parsed_files {
            let path_key = path_key_for_file(index, &parsed.filename)?;
            let diff_file = GitHubPrDiffFile {
                filename: parsed.filename.clone().into(),
                old_filename: parsed.old_filename.clone().into(),
                status: parsed.status.clone().into(),
                additions: parsed.additions,
                deletions: parsed.deletions,
                path_key,
                unsupported_reason: parsed.unsupported_reason.clone().map(SharedString::from),
                preview_note: parsed.preview_note.clone().map(SharedString::from),
            };
            diff_files.push(diff_file.clone());

            if parsed.unsupported_reason.is_some() {
                continue;
            }

            let path = RelPath::unix(&parsed.filename)?.into_arc();
            let display_name = path
                .file_name()
                .map(ToString::to_string)
                .unwrap_or_else(|| parsed.filename.clone());
            let file = VirtualDiffFile::new(
                path,
                display_name,
                worktree_id,
                parsed.status == "removed",
                false,
            );
            let new_buffer = build_virtual_buffer(
                parsed.new_text,
                file,
                Capability::ReadOnly,
                &language_registry,
                cx,
            )
            .await?;
            let diff = build_virtual_buffer_diff(
                Some(parsed.old_text),
                &new_buffer,
                &language_registry,
                cx,
            )
            .await?;
            entries.push(GitHubPrDiffEntry {
                file: diff_file,
                new_buffer,
                diff,
            });
        }

        let item = workspace.update_in(cx, |workspace, window, cx| {
            let project = project.clone();
            let workspace_entity = cx.entity();
            let existing = workspace.items_of_type::<Self>(cx).find(|item| {
                let item = item.read(cx);
                item.repo_name_with_owner == repo_name_with_owner && item.pull_number == pull.number
            });

            let item = if let Some(existing) = existing {
                existing.update(cx, |view, cx| {
                    view.rebuild(
                        repo_name_with_owner.clone(),
                        pull.number,
                        pull.title.clone().into(),
                        entries,
                        diff_files,
                        project,
                        workspace_entity,
                        window,
                        cx,
                    );
                });
                existing
            } else {
                cx.new(|cx| {
                    Self::new(
                        repo_name_with_owner.clone(),
                        pull.number,
                        pull.title.clone().into(),
                        entries,
                        diff_files,
                        project,
                        workspace_entity,
                        window,
                        cx,
                    )
                })
            };

            workspace.add_item_to_center(Box::new(item.clone()), window, cx);
            item
        })?;

        item.update_in(cx, |view, window, cx| {
            view.split_if_needed(window, cx);
        })?;

        Ok(item)
    }

    fn new(
        repo_name_with_owner: SharedString,
        pull_number: u64,
        title: SharedString,
        entries: Vec<GitHubPrDiffEntry>,
        files: Vec<GitHubPrDiffFile>,
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editor, files) = build_editor(entries, files, project, workspace, window, cx);
        Self {
            repo_name_with_owner,
            pull_number,
            title,
            files,
            editor,
            focus_handle: cx.focus_handle(),
        }
    }

    fn rebuild(
        &mut self,
        repo_name_with_owner: SharedString,
        pull_number: u64,
        title: SharedString,
        entries: Vec<GitHubPrDiffEntry>,
        files: Vec<GitHubPrDiffFile>,
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (editor, files) = build_editor(entries, files, project, workspace, window, cx);
        self.repo_name_with_owner = repo_name_with_owner;
        self.pull_number = pull_number;
        self.title = title;
        self.files = files;
        self.editor = editor;
        cx.notify();
    }

    fn title(&self) -> SharedString {
        format!("#{} Changes", self.pull_number).into()
    }

    fn split_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            if editor.diff_view_style() == settings::DiffViewStyle::Split {
                editor.split(window, cx);
            }
        });
    }
}

fn build_editor(
    entries: Vec<GitHubPrDiffEntry>,
    files: Vec<GitHubPrDiffFile>,
    project: Entity<Project>,
    workspace: Entity<Workspace>,
    window: &mut Window,
    cx: &mut Context<GitHubPrDiffView>,
) -> (Entity<SplittableEditor>, Vec<GitHubPrDiffFile>) {
    let multibuffer = cx.new(|cx| {
        let mut multibuffer = MultiBuffer::new(Capability::ReadOnly);
        multibuffer.set_all_diff_hunks_expanded(cx);
        multibuffer
    });

    let editor = cx.new(|cx| {
        let editor = SplittableEditor::new(
            EditorSettings::get_global(cx).diff_view_style,
            multibuffer.clone(),
            project,
            workspace,
            window,
            cx,
        );
        editor.rhs_editor().update(cx, |editor, cx| {
            editor.start_temporary_diff_override();
            editor.disable_diagnostics(cx);
            editor.set_expand_all_diff_hunks(cx);
            editor.set_render_diff_hunks_as_unstaged(true, cx);
            editor.set_render_diff_hunk_controls(
                Arc::new(|_, _, _, _, _, _, _, _| gpui::Empty.into_any_element()),
                cx,
            );
        });
        editor.disable_diff_hunk_controls(cx);
        editor.set_render_diff_hunks_as_unstaged(cx);
        editor
    });

    add_entries_to_editor(entries, &editor, window, cx);

    (editor, files)
}

fn add_entries_to_editor(
    entries: Vec<GitHubPrDiffEntry>,
    editor: &Entity<SplittableEditor>,
    window: &mut Window,
    cx: &mut Context<GitHubPrDiffView>,
) {
    for entry in entries {
        let ranges = diff_excerpt_ranges(&entry.new_buffer, &entry.diff, false, cx);
        insert_diff_excerpts(
            editor,
            VirtualDiffEntry {
                path: entry.file.path_key.clone(),
                buffer: entry.new_buffer,
                diff: entry.diff,
            },
            ranges,
            true,
            false,
            window,
            cx,
        );
    }
}

pub(crate) fn parse_github_pr_diff_file(file: &GitHubPullRequestFile) -> ParsedGitHubPrDiffFile {
    let old_filename = file
        .previous_filename
        .clone()
        .unwrap_or_else(|| file.filename.clone());
    let Some(patch) = file.patch.as_deref() else {
        return ParsedGitHubPrDiffFile {
            filename: file.filename.clone(),
            old_filename,
            status: file.status.clone(),
            additions: file.additions,
            deletions: file.deletions,
            old_text: String::new(),
            new_text: String::new(),
            unsupported_reason: Some("Binary file or diff too large to display".to_string()),
            preview_note: None,
        };
    };

    match parse_unified_patch_text(patch) {
        Ok((old_text, new_text)) => ParsedGitHubPrDiffFile {
            filename: file.filename.clone(),
            old_filename,
            status: file.status.clone(),
            additions: file.additions,
            deletions: file.deletions,
            old_text,
            new_text,
            unsupported_reason: None,
            preview_note: None,
        },
        Err(error) => ParsedGitHubPrDiffFile {
            filename: file.filename.clone(),
            old_filename,
            status: file.status.clone(),
            additions: file.additions,
            deletions: file.deletions,
            old_text: String::new(),
            new_text: String::new(),
            unsupported_reason: Some(error.to_string()),
            preview_note: None,
        },
    }
}

async fn resolve_github_pr_diff_file(
    repo_name_with_owner: &str,
    pull: &GitHubPullRequest,
    file: GitHubPullRequestFile,
    token: Option<&str>,
    http_client: Arc<dyn HttpClient>,
) -> ParsedGitHubPrDiffFile {
    match resolve_github_pr_diff_file_content(repo_name_with_owner, pull, &file, token, http_client)
        .await
    {
        Ok(parsed) => parsed,
        Err(error) => {
            let mut parsed = parse_github_pr_diff_file(&file);
            if parsed.unsupported_reason.is_none() {
                parsed.preview_note = Some("Patch preview".to_string());
            } else {
                log::debug!(
                    "Could not fetch full GitHub PR file content for {}: {error:#}",
                    file.filename
                );
            }
            parsed
        }
    }
}

async fn resolve_github_pr_diff_file_content(
    repo_name_with_owner: &str,
    pull: &GitHubPullRequest,
    file: &GitHubPullRequestFile,
    token: Option<&str>,
    http_client: Arc<dyn HttpClient>,
) -> Result<ParsedGitHubPrDiffFile> {
    let old_filename = file
        .previous_filename
        .clone()
        .unwrap_or_else(|| file.filename.clone());
    let base_repo = branch_repo_name(&pull.base, repo_name_with_owner);
    let head_repo = branch_repo_name(&pull.head, repo_name_with_owner);

    let old_text = match file.status.as_str() {
        "added" => String::new(),
        _ => {
            http_client::github::repository_file_content(
                &base_repo,
                &old_filename,
                &pull.base.sha,
                token,
                http_client.clone(),
            )
            .await?
        }
    };

    let new_text = match file.status.as_str() {
        "removed" | "deleted" => String::new(),
        _ => {
            http_client::github::repository_file_content(
                &head_repo,
                &file.filename,
                &pull.head.sha,
                token,
                http_client,
            )
            .await?
        }
    };

    Ok(ParsedGitHubPrDiffFile {
        filename: file.filename.clone(),
        old_filename,
        status: file.status.clone(),
        additions: file.additions,
        deletions: file.deletions,
        old_text,
        new_text,
        unsupported_reason: None,
        preview_note: None,
    })
}

fn branch_repo_name(branch: &GitHubPullRequestBranch, fallback: &str) -> String {
    branch
        .repo
        .as_ref()
        .map(|repo| repo.full_name.clone())
        .unwrap_or_else(|| fallback.to_string())
}

fn parse_unified_patch_text(patch: &str) -> Result<(String, String)> {
    let mut old_text = String::new();
    let mut new_text = String::new();
    let mut saw_hunk = false;

    for line in patch.lines() {
        if line.starts_with("@@") {
            saw_hunk = true;
            continue;
        }
        if line.starts_with("\\ No newline at end of file") {
            continue;
        }
        if line.starts_with("diff --git ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }

        if line.is_empty() {
            continue;
        }
        let (kind, text) = line.split_at(1);
        match kind {
            " " => {
                old_text.push_str(text);
                old_text.push('\n');
                new_text.push_str(text);
                new_text.push('\n');
            }
            "-" => {
                old_text.push_str(text);
                old_text.push('\n');
            }
            "+" => {
                new_text.push_str(text);
                new_text.push('\n');
            }
            _ => return Err(anyhow!("Unsupported GitHub patch line: {line}")),
        }
    }

    if !saw_hunk && !patch.trim().is_empty() {
        return Err(anyhow!("GitHub patch did not contain a hunk"));
    }

    Ok((old_text, new_text))
}

fn path_key_for_file(index: usize, filename: &str) -> Result<PathKey> {
    let rel_path = RelPath::unix(filename)?.into_arc();
    Ok(PathKey::with_sort_prefix(index as u64, rel_path))
}

fn github_pr_diff_totals(files: &[GitHubPrDiffFile]) -> (usize, usize, usize) {
    (
        files.len(),
        files.iter().map(|file| file.additions as usize).sum(),
        files.iter().map(|file| file.deletions as usize).sum(),
    )
}

impl EventEmitter<EditorEvent> for GitHubPrDiffView {}

impl Focusable for GitHubPrDiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for GitHubPrDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, _cx: &App) -> AnyElement {
        Label::new(self.title())
            .color(if params.selected {
                Color::Default
            } else {
                Color::Muted
            })
            .into_any_element()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("{} #{}", self.repo_name_with_owner, self.pull_number).into())
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title()
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("GitHub Pull Request Diff Opened")
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else {
            self.editor.act_as_type(type_id, cx)
        }
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.editor.for_each_project_item(cx, f)
    }

    fn active_project_path(&self, cx: &App) -> Option<ProjectPath> {
        self.editor.read(cx).active_project_path(cx)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            editor.rhs_editor().update(cx, |editor, _| {
                editor.set_nav_history(Some(nav_history));
            })
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn std::any::Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.editor.update(cx, |editor, cx| {
            editor
                .rhs_editor()
                .update(cx, |editor, cx| editor.navigate(data, window, cx))
        })
    }

    fn breadcrumb_location(&self, _: &App) -> ToolbarItemLocation {
        ToolbarItemLocation::PrimaryLeft
    }

    fn breadcrumbs(
        &self,
        cx: &App,
    ) -> Option<(Vec<language::HighlightedText>, Option<gpui::Font>)> {
        self.editor.breadcrumbs(cx)
    }
}

impl Render for GitHubPrDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().colors().editor_background)
            .child(self.render_header(cx))
            .child(
                div()
                    .flex_1()
                    .size_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.editor.clone()),
            )
    }
}

impl GitHubPrDiffView {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (file_count, additions, deletions) = github_pr_diff_totals(&self.files);
        h_flex()
            .w_full()
            .h(rems(2.))
            .flex_none()
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .justify_between()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .child(
                        Label::new(format!("#{}", self.pull_number))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Icon::new(IconName::PullRequest).size(IconSize::Small))
                    .child(Label::new(self.title.clone()).truncate()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(
                        Label::new(self.repo_name_with_owner.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(format!("{file_count} files"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(format!("+{additions}")).color(Color::Success))
                    .child(Label::new(format!("-{deletions}")).color(Color::Error)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use http_client::{AsyncBody, FakeHttpClient, Response};
    use std::sync::{Arc, Mutex};

    fn file(status: &str, patch: Option<&str>) -> GitHubPullRequestFile {
        GitHubPullRequestFile {
            filename: "src/main.rs".to_string(),
            status: status.to_string(),
            previous_filename: None,
            additions: 1,
            deletions: 1,
            changes: 2,
            patch: patch.map(ToString::to_string),
        }
    }

    fn diff_file(index: usize, filename: &str) -> GitHubPrDiffFile {
        GitHubPrDiffFile {
            filename: filename.to_string().into(),
            old_filename: filename.to_string().into(),
            status: "modified".into(),
            additions: index as u32 + 1,
            deletions: index as u32,
            path_key: path_key_for_file(index, filename).unwrap(),
            unsupported_reason: None,
            preview_note: None,
        }
    }

    fn pull() -> GitHubPullRequest {
        GitHubPullRequest {
            number: 7,
            title: "Improve graph".to_string(),
            html_url: "https://github.com/owner/repo/pull/7".to_string(),
            state: "open".to_string(),
            user: http_client::github::GitHubUser {
                login: "octo".to_string(),
            },
            base: GitHubPullRequestBranch {
                ref_name: "main".to_string(),
                sha: "base-sha".to_string(),
                repo: Some(http_client::github::GitHubRepositoryRef {
                    full_name: "owner/repo".to_string(),
                    clone_url: None,
                    ssh_url: None,
                }),
            },
            head: GitHubPullRequestBranch {
                ref_name: "feature".to_string(),
                sha: "head-sha".to_string(),
                repo: Some(http_client::github::GitHubRepositoryRef {
                    full_name: "fork/repo".to_string(),
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
            comments: 0,
            review_comments: 0,
            commits: None,
            changed_files: None,
            additions: None,
            deletions: None,
        }
    }

    fn content_response(text: &str) -> Response<AsyncBody> {
        let content = base64::engine::general_purpose::STANDARD.encode(text);
        Response::builder()
            .status(200)
            .body(AsyncBody::from(format!(
                r#"{{"type":"file","encoding":"base64","content":"{content}"}}"#
            )))
            .unwrap()
    }

    #[test]
    fn test_parse_modified_patch_reconstructs_old_and_new_text() {
        let parsed = parse_github_pr_diff_file(&file(
            "modified",
            Some("@@ -1,3 +1,3 @@\n fn main() {\n-old();\n+new();\n }"),
        ));

        assert_eq!(parsed.old_text, "fn main() {\nold();\n}\n");
        assert_eq!(parsed.new_text, "fn main() {\nnew();\n}\n");
        assert_eq!(parsed.unsupported_reason, None);
    }

    #[test]
    fn test_parse_added_patch_has_empty_old_text() {
        let parsed = parse_github_pr_diff_file(&GitHubPullRequestFile {
            filename: "src/new.rs".to_string(),
            status: "added".to_string(),
            previous_filename: None,
            additions: 2,
            deletions: 0,
            changes: 2,
            patch: Some("@@ -0,0 +1,2 @@\n+one\n+two".to_string()),
        });

        assert_eq!(parsed.old_text, "");
        assert_eq!(parsed.new_text, "one\ntwo\n");
        assert_eq!(parsed.unsupported_reason, None);
    }

    #[test]
    fn test_parse_deleted_patch_has_empty_new_text() {
        let parsed = parse_github_pr_diff_file(&GitHubPullRequestFile {
            filename: "src/old.rs".to_string(),
            status: "removed".to_string(),
            previous_filename: None,
            additions: 0,
            deletions: 2,
            changes: 2,
            patch: Some("@@ -1,2 +0,0 @@\n-one\n-two".to_string()),
        });

        assert_eq!(parsed.old_text, "one\ntwo\n");
        assert_eq!(parsed.new_text, "");
        assert_eq!(parsed.unsupported_reason, None);
    }

    #[test]
    fn test_parse_renamed_patch_uses_previous_filename() {
        let parsed = parse_github_pr_diff_file(&GitHubPullRequestFile {
            filename: "src/new.rs".to_string(),
            status: "renamed".to_string(),
            previous_filename: Some("src/old.rs".to_string()),
            additions: 1,
            deletions: 1,
            changes: 2,
            patch: Some("@@ -1 +1 @@\n-old\n+new".to_string()),
        });

        assert_eq!(parsed.old_filename, "src/old.rs");
        assert_eq!(parsed.filename, "src/new.rs");
        assert_eq!(parsed.old_text, "old\n");
        assert_eq!(parsed.new_text, "new\n");
    }

    #[test]
    fn test_missing_patch_is_unsupported() {
        let parsed = parse_github_pr_diff_file(&file("modified", None));

        assert_eq!(
            parsed.unsupported_reason.as_deref(),
            Some("Binary file or diff too large to display")
        );
    }

    #[test]
    fn test_github_pr_diff_totals_format_header_stats() {
        let files = vec![
            diff_file(0, "src/main.rs"),
            diff_file(1, "src/lib.rs"),
            diff_file(2, "README.md"),
        ];

        assert_eq!(github_pr_diff_totals(&files), (3, 6, 3));
    }

    #[test]
    fn test_resolve_modified_file_uses_full_base_and_head_content() {
        futures::executor::block_on(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let responses = Arc::new(Mutex::new(vec![
                content_response("fn main() {\n    old();\n}\n"),
                content_response("fn main() {\n    new();\n}\n"),
            ]));
            let http = FakeHttpClient::create({
                let requests = requests.clone();
                let responses = responses.clone();
                move |request| {
                    requests.lock().unwrap().push(request.uri().to_string());
                    let response = responses.lock().unwrap().remove(0);
                    async move { Ok(response) }
                }
            });

            let parsed = resolve_github_pr_diff_file_content(
                "owner/repo",
                &pull(),
                &file("modified", Some("@@ -1 +1 @@\n-old\n+new")),
                Some("secret"),
                http,
            )
            .await
            .expect("full file content should resolve");

            assert_eq!(parsed.old_text, "fn main() {\n    old();\n}\n");
            assert_eq!(parsed.new_text, "fn main() {\n    new();\n}\n");
            assert_eq!(parsed.preview_note, None);
            assert_eq!(
                &*requests.lock().unwrap(),
                &[
                    "https://api.github.com/repos/owner/repo/contents/src/main.rs?ref=base-sha",
                    "https://api.github.com/repos/fork/repo/contents/src/main.rs?ref=head-sha",
                ]
            );
        });
    }

    #[test]
    fn test_resolve_renamed_file_uses_previous_filename_for_base() {
        futures::executor::block_on(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let responses = Arc::new(Mutex::new(vec![
                content_response("old name\n"),
                content_response("new name\n"),
            ]));
            let http = FakeHttpClient::create({
                let requests = requests.clone();
                let responses = responses.clone();
                move |request| {
                    requests.lock().unwrap().push(request.uri().to_string());
                    let response = responses.lock().unwrap().remove(0);
                    async move { Ok(response) }
                }
            });

            let parsed = resolve_github_pr_diff_file_content(
                "owner/repo",
                &pull(),
                &GitHubPullRequestFile {
                    filename: "src/new.rs".to_string(),
                    status: "renamed".to_string(),
                    previous_filename: Some("src/old.rs".to_string()),
                    additions: 1,
                    deletions: 1,
                    changes: 2,
                    patch: Some("@@ -1 +1 @@\n-old\n+new".to_string()),
                },
                None,
                http,
            )
            .await
            .expect("renamed file content should resolve");

            assert_eq!(parsed.old_filename, "src/old.rs");
            assert_eq!(parsed.old_text, "old name\n");
            assert_eq!(parsed.new_text, "new name\n");
            assert_eq!(
                &*requests.lock().unwrap(),
                &[
                    "https://api.github.com/repos/owner/repo/contents/src/old.rs?ref=base-sha",
                    "https://api.github.com/repos/fork/repo/contents/src/new.rs?ref=head-sha",
                ]
            );
        });
    }

    #[test]
    fn test_resolve_file_falls_back_to_patch_preview() {
        futures::executor::block_on(async {
            let http = FakeHttpClient::create(|_| async {
                Ok(Response::builder()
                    .status(404)
                    .body(AsyncBody::from("{}"))
                    .unwrap())
            });

            let parsed = resolve_github_pr_diff_file(
                "owner/repo",
                &pull(),
                file("modified", Some("@@ -1 +1 @@\n-old\n+new")),
                None,
                http,
            )
            .await;

            assert_eq!(parsed.old_text, "old\n");
            assert_eq!(parsed.new_text, "new\n");
            assert_eq!(parsed.preview_note.as_deref(), Some("Patch preview"));
        });
    }
}
