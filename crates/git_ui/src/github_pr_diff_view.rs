use crate::file_diff_view::build_buffer_diff;
use anyhow::{Result, anyhow};
use buffer_diff::BufferDiff;
use editor::{
    Editor, EditorEvent, EditorSettings, SelectionEffects, SplittableEditor,
    multibuffer_context_lines, scroll::Autoscroll,
};
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, rems,
};
use http_client::github::{GitHubPullRequest, GitHubPullRequestFile};
use language::{Buffer, Capability, OffsetRangeExt};
use multi_buffer::{MultiBuffer, PathKey};
use project::Project;
use settings::Settings;
use std::{any::TypeId, sync::Arc};
use ui::{Color, Icon, IconName, Label, LabelSize, Tooltip, div, h_flex, prelude::*, v_flex};
use util::rel_path::RelPath;
use workspace::{
    Item, Workspace,
    item::{ItemEvent, TabContentParams},
    searchable::SearchableItemHandle,
};

pub(crate) struct GitHubPrDiffView {
    repo_name_with_owner: SharedString,
    pull_number: u64,
    title: SharedString,
    files: Vec<GitHubPrDiffFile>,
    editor: Entity<SplittableEditor>,
    multibuffer: Entity<MultiBuffer>,
    selected_file: Option<SharedString>,
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
}

impl GitHubPrDiffView {
    pub(crate) async fn build(
        repo_name_with_owner: SharedString,
        pull: GitHubPullRequest,
        files: Vec<GitHubPullRequestFile>,
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        cx: &mut AsyncApp,
    ) -> Result<Entity<Self>> {
        let mut entries = Vec::new();
        let mut diff_files = Vec::new();

        for (index, file) in files.into_iter().enumerate() {
            let parsed = parse_github_pr_diff_file(&file);
            let path_key = path_key_for_file(index, &parsed.filename)?;
            let diff_file = GitHubPrDiffFile {
                filename: parsed.filename.clone().into(),
                old_filename: parsed.old_filename.clone().into(),
                status: parsed.status.clone().into(),
                additions: parsed.additions,
                deletions: parsed.deletions,
                path_key,
                unsupported_reason: parsed.unsupported_reason.clone().map(SharedString::from),
            };
            diff_files.push(diff_file.clone());

            if parsed.unsupported_reason.is_some() {
                continue;
            }

            let old_buffer = cx.new(|cx| {
                let mut buffer = Buffer::local(parsed.old_text.as_str(), cx);
                buffer.set_capability(Capability::ReadOnly, cx);
                buffer
            });
            let new_buffer = cx.new(|cx| {
                let mut buffer = Buffer::local(parsed.new_text.as_str(), cx);
                buffer.set_capability(Capability::ReadOnly, cx);
                buffer
            });
            let diff = build_buffer_diff(&old_buffer, &new_buffer, cx).await?;
            entries.push(GitHubPrDiffEntry {
                file: diff_file,
                new_buffer,
                diff,
            });
        }

        workspace.update_in(cx, |workspace, window, cx| {
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
        })
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
        let (editor, multibuffer, files) =
            build_editor(entries, files, project, workspace, window, cx);
        Self {
            repo_name_with_owner,
            pull_number,
            title,
            files,
            editor,
            multibuffer,
            selected_file: None,
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
        let (editor, multibuffer, files) =
            build_editor(entries, files, project, workspace, window, cx);
        self.repo_name_with_owner = repo_name_with_owner;
        self.pull_number = pull_number;
        self.title = title;
        self.files = files;
        self.editor = editor;
        self.multibuffer = multibuffer;
        self.selected_file = None;
        cx.notify();
    }

    fn title(&self) -> SharedString {
        format!("#{} Changes", self.pull_number).into()
    }

    fn move_to_file(
        &mut self,
        file: GitHubPrDiffFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_file = Some(file.filename.clone());
        if file.unsupported_reason.is_some() {
            cx.notify();
            return;
        }

        if let Some(position) = self
            .multibuffer
            .read(cx)
            .location_for_path(&file.path_key, cx)
        {
            self.editor.update(cx, |editor, cx| {
                editor.rhs_editor().update(cx, |editor, cx| {
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::focused()),
                        window,
                        cx,
                        |selections| {
                            selections.select_ranges([position..position]);
                        },
                    );
                });
            });
        }
        cx.notify();
    }
}

fn build_editor(
    entries: Vec<GitHubPrDiffEntry>,
    files: Vec<GitHubPrDiffFile>,
    project: Entity<Project>,
    workspace: Entity<Workspace>,
    window: &mut Window,
    cx: &mut Context<GitHubPrDiffView>,
) -> (
    Entity<SplittableEditor>,
    Entity<MultiBuffer>,
    Vec<GitHubPrDiffFile>,
) {
    let context_lines = multibuffer_context_lines(cx);
    let multibuffer = cx.new(|cx| {
        let mut multibuffer = MultiBuffer::new(Capability::ReadOnly);
        multibuffer.set_all_diff_hunks_expanded(cx);
        multibuffer
    });

    for entry in entries {
        let snapshot = entry.new_buffer.read(cx).snapshot();
        let diff_snapshot = entry.diff.read(cx).snapshot(cx);
        let ranges = diff_snapshot
            .hunks(&snapshot)
            .map(|hunk| hunk.buffer_range.to_point(&snapshot))
            .collect::<Vec<_>>();
        multibuffer.update(cx, |multibuffer, cx| {
            multibuffer.set_excerpts_for_path(
                entry.file.path_key.clone(),
                entry.new_buffer,
                ranges,
                context_lines,
                cx,
            );
            multibuffer.add_diff(entry.diff, cx);
        });
    }

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

    (editor, multibuffer, files)
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
        },
    }
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

fn status_color(status: &str) -> Color {
    match status {
        "added" => Color::Success,
        "removed" | "deleted" => Color::Error,
        "renamed" | "copied" => Color::Warning,
        _ => Color::Muted,
    }
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
        _: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<SplittableEditor>() {
            Some(self.editor.clone().into())
        } else {
            None
        }
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        None
    }
}

impl Render for GitHubPrDiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .size_full()
            .overflow_hidden()
            .child(self.render_sidebar(window, cx))
            .child(div().flex_1().size_full().child(self.editor.clone()))
    }
}

impl GitHubPrDiffView {
    fn render_sidebar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h_full()
            .w(rems(17.))
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .child(
                v_flex()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border.opacity(0.6))
                    .px_3()
                    .py_2()
                    .child(Label::new(self.repo_name_with_owner.clone()).size(LabelSize::Small))
                    .child(
                        Label::new(format!("#{} {}", self.pull_number, self.title))
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    ),
            )
            .child(
                div()
                    .id("github-pr-diff-files")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(
                        v_flex().children(
                            self.files
                                .iter()
                                .cloned()
                                .map(|file| self.render_file_row(file, cx).into_any_element()),
                        ),
                    ),
            )
    }

    fn render_file_row(&self, file: GitHubPrDiffFile, cx: &mut Context<Self>) -> impl IntoElement {
        let is_selected = self
            .selected_file
            .as_ref()
            .is_some_and(|selected| selected == &file.filename);
        let status = file.status.to_string();
        let unsupported = file.unsupported_reason.clone();
        let file_for_click = file.clone();
        div()
            .id(format!("github-pr-diff-file-{}", file.filename))
            .w_full()
            .px_3()
            .py_2()
            .cursor_pointer()
            .when(is_selected, |this| {
                this.bg(cx.theme().colors().element_selected)
            })
            .hover(|this| this.bg(cx.theme().colors().element_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.move_to_file(file_for_click.clone(), window, cx);
            }))
            .tooltip(Tooltip::text(file.filename.clone()))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Label::new(status.chars().next().unwrap_or('M').to_string())
                                    .size(LabelSize::XSmall)
                                    .color(status_color(status.as_str())),
                            )
                            .child(Label::new(file.filename.clone()).truncate()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Label::new(format!("+{}", file.additions)).color(Color::Success))
                            .child(Label::new(format!("-{}", file.deletions)).color(Color::Error))
                            .when_some(unsupported, |this, reason| {
                                this.child(Label::new(reason).color(Color::Muted).truncate())
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
