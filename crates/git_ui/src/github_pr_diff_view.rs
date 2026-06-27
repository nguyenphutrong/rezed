use crate::virtual_diff::{
    VirtualDiffEntry, VirtualDiffFile, build_virtual_buffer, build_virtual_buffer_diff,
    diff_excerpt_ranges, insert_diff_excerpts,
};
use anyhow::{Result, anyhow};
use buffer_diff::BufferDiff;
use editor::{
    Editor, EditorEvent, EditorSettings, SelectionEffects, SplittableEditor, scroll::Autoscroll,
};
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle,
    WeakEntity, Window, px, rems, uniform_list,
};
use http_client::{
    HttpClient,
    github::{GitHubPullRequest, GitHubPullRequestBranch, GitHubPullRequestFile},
};
use language::{Buffer, Capability};
use multi_buffer::{MultiBuffer, PathKey};
use project::{Project, ProjectPath};
use settings::Settings;
use std::{
    any::TypeId,
    collections::{BTreeMap, HashMap},
    rc::Rc,
    sync::Arc,
};
use ui::{
    Button, ButtonSize, ButtonStyle, Color, Icon, IconName, IconSize, Label, LabelSize, Tooltip,
    div, h_flex, prelude::*, v_flex,
};
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
    multibuffer: Entity<MultiBuffer>,
    selected_file: Option<SharedString>,
    files_view_mode: GitHubPrDiffFilesViewMode,
    expanded_dirs: HashMap<String, bool>,
    files_scroll_handle: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _editor_subscription: Subscription,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GitHubPrDiffFilesViewMode {
    List,
    #[default]
    Tree,
}

impl GitHubPrDiffFilesViewMode {
    fn is_tree(self) -> bool {
        matches!(self, Self::Tree)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GitHubPrDiffTreeEntry {
    Directory(GitHubPrDiffDirectoryEntry),
    File(GitHubPrDiffTreeFileEntry),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitHubPrDiffTreeFileEntry {
    file: GitHubPrDiffFile,
    depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitHubPrDiffDirectoryEntry {
    path: String,
    name: SharedString,
    depth: usize,
    expanded: bool,
}

#[derive(Default)]
struct GitHubPrDiffTreeNode {
    name: SharedString,
    path: Option<String>,
    children: BTreeMap<String, GitHubPrDiffTreeNode>,
    files: Vec<GitHubPrDiffFile>,
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
        let (editor, multibuffer, files, editor_subscription) =
            build_editor(entries, files, project, workspace, window, cx);
        Self {
            repo_name_with_owner,
            pull_number,
            title,
            files,
            editor,
            multibuffer,
            selected_file: None,
            files_view_mode: GitHubPrDiffFilesViewMode::Tree,
            expanded_dirs: HashMap::default(),
            files_scroll_handle: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _editor_subscription: editor_subscription,
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
        let (editor, multibuffer, files, editor_subscription) =
            build_editor(entries, files, project, workspace, window, cx);
        self.repo_name_with_owner = repo_name_with_owner;
        self.pull_number = pull_number;
        self.title = title;
        self.files = files;
        self.editor = editor;
        self.multibuffer = multibuffer;
        self.selected_file = None;
        self.expanded_dirs.clear();
        self._editor_subscription = editor_subscription;
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

    fn handle_editor_event(
        &mut self,
        _: &Entity<SplittableEditor>,
        event: &EditorEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorEvent::ScrollPositionChanged { .. }
            | EditorEvent::SelectionsChanged { local: true } => {
                self.sync_selected_file_to_editor(cx);
            }
            _ => {}
        }
    }

    fn sync_selected_file_to_editor(&mut self, cx: &mut Context<Self>) {
        let Some(project_path) = self.active_project_path(cx) else {
            return;
        };
        let path = project_path.path.as_unix_str();
        let Some(filename) = selected_filename_for_path(&self.files, path) else {
            return;
        };
        if self
            .selected_file
            .as_ref()
            .is_some_and(|selected| selected == &filename)
        {
            return;
        }

        self.selected_file = Some(filename.clone());
        if self.files_view_mode.is_tree() {
            self.expand_parent_dirs(path);
        }
        if let Some(index) = self.visible_file_index(&filename) {
            self.files_scroll_handle
                .scroll_to_item(index, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn expand_parent_dirs(&mut self, filename: &str) {
        let mut path = String::new();
        let mut components = filename.split('/').peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(component);
            self.expanded_dirs.insert(path.clone(), true);
        }
    }

    fn visible_file_index(&self, filename: &SharedString) -> Option<usize> {
        if self.files_view_mode.is_tree() {
            build_github_pr_diff_tree_entries(self.files.clone(), &self.expanded_dirs)
                .into_iter()
                .position(|entry| {
                    matches!(entry, GitHubPrDiffTreeEntry::File(file) if file.file.filename == *filename)
                })
        } else {
            self.files
                .iter()
                .position(|file| file.filename == *filename)
        }
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
    Subscription,
) {
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
    let editor_subscription =
        cx.subscribe_in(&editor, window, GitHubPrDiffView::handle_editor_event);

    (editor, multibuffer, files, editor_subscription)
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

fn selected_filename_for_path(files: &[GitHubPrDiffFile], path: &str) -> Option<SharedString> {
    files
        .iter()
        .find(|file| file.filename.as_ref() == path)
        .map(|file| file.filename.clone())
}

fn build_github_pr_diff_tree_entries(
    mut files: Vec<GitHubPrDiffFile>,
    expanded_dirs: &HashMap<String, bool>,
) -> Vec<GitHubPrDiffTreeEntry> {
    files.sort_by(|left, right| left.filename.cmp(&right.filename));

    let mut root = GitHubPrDiffTreeNode::default();
    for file in files {
        let filename = file.filename.to_string();
        let components = filename
            .split('/')
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        if components.is_empty() {
            root.files.push(file);
            continue;
        }

        let mut current = &mut root;
        let mut current_path = String::new();
        for (index, component) in components.iter().enumerate() {
            if index == components.len() - 1 {
                current.files.push(file.clone());
            } else {
                if !current_path.is_empty() {
                    current_path.push('/');
                }
                current_path.push_str(component);
                let component = component.to_string();
                current = current
                    .children
                    .entry(component.clone())
                    .or_insert_with(|| GitHubPrDiffTreeNode {
                        name: component.into(),
                        path: Some(current_path.clone()),
                        ..Default::default()
                    });
            }
        }
    }

    flatten_github_pr_diff_tree(&root, 0, expanded_dirs)
}

fn flatten_github_pr_diff_tree(
    node: &GitHubPrDiffTreeNode,
    depth: usize,
    expanded_dirs: &HashMap<String, bool>,
) -> Vec<GitHubPrDiffTreeEntry> {
    let mut entries = Vec::new();

    for child in node.children.values() {
        let (terminal, name) = compact_github_pr_diff_directory_chain(child);
        let Some(path) = terminal.path.clone().or_else(|| child.path.clone()) else {
            continue;
        };
        let expanded = *expanded_dirs.get(&path).unwrap_or(&true);
        let child_entries = flatten_github_pr_diff_tree(terminal, depth + 1, expanded_dirs);

        entries.push(GitHubPrDiffTreeEntry::Directory(
            GitHubPrDiffDirectoryEntry {
                path,
                name,
                depth,
                expanded,
            },
        ));

        if expanded {
            entries.extend(child_entries);
        }
    }

    entries.extend(
        node.files
            .iter()
            .cloned()
            .map(|file| GitHubPrDiffTreeEntry::File(GitHubPrDiffTreeFileEntry { file, depth })),
    );
    entries
}

fn compact_github_pr_diff_directory_chain(
    mut node: &GitHubPrDiffTreeNode,
) -> (&GitHubPrDiffTreeNode, SharedString) {
    let mut parts = vec![node.name.clone()];
    while node.files.is_empty() && node.children.len() == 1 {
        let Some(child) = node.children.values().next() else {
            continue;
        };
        if child.path.is_none() {
            break;
        }
        parts.push(child.name.clone());
        node = child;
    }
    (node, parts.join("/").into())
}

fn filename_basename(filename: &str) -> &str {
    filename.rsplit('/').next().unwrap_or(filename)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().colors().editor_background)
            .child(self.render_header(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_sidebar(window, cx))
                    .child(div().flex_1().size_full().child(self.editor.clone())),
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

    fn render_sidebar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_tree_view = self.files_view_mode.is_tree();
        let flat_entries = Rc::new(self.files.clone());
        let tree_entries = Rc::new(if is_tree_view {
            build_github_pr_diff_tree_entries(self.files.clone(), &self.expanded_dirs)
        } else {
            Vec::new()
        });
        let entry_count = if is_tree_view {
            tree_entries.len()
        } else {
            flat_entries.len()
        };
        let this = cx.weak_entity();

        v_flex()
            .h_full()
            .w(rems(17.))
            .flex_none()
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border.opacity(0.6))
                    .px_3()
                    .py_2()
                    .child(
                        Label::new("Files")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("github-pr-diff-list-view", "List")
                                    .size(ButtonSize::Compact)
                                    .style(ButtonStyle::Subtle)
                                    .toggle_state(!is_tree_view)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.files_view_mode = GitHubPrDiffFilesViewMode::List;
                                        if let Some(selected) = this.selected_file.clone()
                                            && let Some(index) = this.visible_file_index(&selected)
                                        {
                                            this.files_scroll_handle
                                                .scroll_to_item(index, ScrollStrategy::Nearest);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("github-pr-diff-tree-view", "Tree")
                                    .size(ButtonSize::Compact)
                                    .style(ButtonStyle::Subtle)
                                    .toggle_state(is_tree_view)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.files_view_mode = GitHubPrDiffFilesViewMode::Tree;
                                        if let Some(selected) = this.selected_file.clone() {
                                            this.expand_parent_dirs(&selected);
                                            if let Some(index) = this.visible_file_index(&selected)
                                            {
                                                this.files_scroll_handle
                                                    .scroll_to_item(index, ScrollStrategy::Nearest);
                                            }
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("github-pr-diff-files")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "github-pr-diff-files-list",
                            entry_count,
                            move |range, _window, cx| {
                                range
                                    .map(|index| {
                                        if is_tree_view {
                                            match &tree_entries[index] {
                                                GitHubPrDiffTreeEntry::Directory(entry) => {
                                                    render_directory_row(
                                                        index,
                                                        entry.clone(),
                                                        this.clone(),
                                                        cx,
                                                    )
                                                }
                                                GitHubPrDiffTreeEntry::File(entry) => {
                                                    render_file_row(
                                                        entry.file.clone(),
                                                        entry.depth,
                                                        true,
                                                        this.clone(),
                                                        cx,
                                                    )
                                                }
                                            }
                                        } else {
                                            render_file_row(
                                                flat_entries[index].clone(),
                                                0,
                                                false,
                                                this.clone(),
                                                cx,
                                            )
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .size_full()
                        .flex_grow_1()
                        .track_scroll(&self.files_scroll_handle),
                    ),
            )
    }
}

fn render_directory_row(
    index: usize,
    entry: GitHubPrDiffDirectoryEntry,
    view: WeakEntity<GitHubPrDiffView>,
    cx: &mut App,
) -> AnyElement {
    const TREE_INDENT: f32 = 12.0;
    let path = entry.path.clone();
    let expanded = entry.expanded;
    div()
        .id(format!("github-pr-diff-dir-{index}"))
        .w_full()
        .px_3()
        .py_1()
        .cursor_pointer()
        .hover(|this| this.bg(cx.theme().colors().element_hover))
        .on_click(move |_, _, cx| {
            view.update(cx, |view, cx| {
                view.expanded_dirs.insert(path.clone(), !expanded);
                if let Some(selected) = view.selected_file.clone()
                    && let Some(index) = view.visible_file_index(&selected)
                {
                    view.files_scroll_handle
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                }
                cx.notify();
            })
            .ok();
        })
        .child(
            h_flex()
                .min_w_0()
                .gap_1()
                .pl(px(entry.depth as f32 * TREE_INDENT))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(IconSize::Small)
                    .color(Color::Muted),
                )
                .child(
                    Icon::new(if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    })
                    .size(IconSize::Small)
                    .color(Color::Muted),
                )
                .child(
                    Label::new(entry.name)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                ),
        )
        .into_any_element()
}

fn render_file_row(
    file: GitHubPrDiffFile,
    depth: usize,
    tree_view: bool,
    view: WeakEntity<GitHubPrDiffView>,
    cx: &mut App,
) -> AnyElement {
    const TREE_INDENT: f32 = 12.0;
    let is_selected = view
        .read_with(cx, |view, _| {
            view.selected_file
                .as_ref()
                .is_some_and(|selected| selected == &file.filename)
        })
        .unwrap_or(false);
    let status = file.status.to_string();
    let unsupported = file.unsupported_reason.clone();
    let preview_note = file.preview_note.clone();
    let label = if tree_view {
        filename_basename(&file.filename).to_string()
    } else {
        file.filename.to_string()
    };
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
        .on_click(move |_, window, cx| {
            view.update(cx, |view, cx| {
                view.move_to_file(file_for_click.clone(), window, cx);
            })
            .ok();
        })
        .tooltip(Tooltip::text(file.filename.clone()))
        .child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .when(tree_view, |this| this.pl(px(depth as f32 * TREE_INDENT)))
                        .child(
                            Label::new(status.chars().next().unwrap_or('M').to_string())
                                .size(LabelSize::XSmall)
                                .color(status_color(status.as_str())),
                        )
                        .child(Label::new(label).truncate()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .when(tree_view, |this| {
                            this.pl(px(depth as f32 * TREE_INDENT + TREE_INDENT))
                        })
                        .child(Label::new(format!("+{}", file.additions)).color(Color::Success))
                        .child(Label::new(format!("-{}", file.deletions)).color(Color::Error))
                        .when_some(unsupported, |this, reason| {
                            this.child(Label::new(reason).color(Color::Muted).truncate())
                        })
                        .when_some(preview_note, |this, note| {
                            this.child(Label::new(note).color(Color::Muted).truncate())
                        }),
                ),
        )
        .into_any_element()
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
    fn test_selected_filename_for_active_project_path() {
        let files = vec![
            diff_file(0, "src/main.rs"),
            diff_file(1, "crates/git_ui/src/github_pr_diff_view.rs"),
        ];

        assert_eq!(
            selected_filename_for_path(&files, "crates/git_ui/src/github_pr_diff_view.rs")
                .as_deref(),
            Some("crates/git_ui/src/github_pr_diff_view.rs")
        );
        assert_eq!(selected_filename_for_path(&files, "missing.rs"), None);
    }

    #[test]
    fn test_tree_builder_groups_nested_files_and_compacts_directories() {
        let entries = build_github_pr_diff_tree_entries(
            vec![
                diff_file(0, "crates/intl-lens-extension/extension.toml"),
                diff_file(1, "crates/intl-lens/src/i18n/store.rs"),
                diff_file(2, "README.md"),
            ],
            &HashMap::default(),
        );

        assert!(entries.iter().any(|entry| {
            matches!(
                entry,
                GitHubPrDiffTreeEntry::Directory(directory)
                    if directory.name.as_ref() == "intl-lens/src/i18n"
            )
        }));
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(entry, GitHubPrDiffTreeEntry::File(_)))
                .count(),
            3
        );
    }

    #[test]
    fn test_tree_builder_preserves_file_path_key() {
        let file = diff_file(4, "crates/git_ui/src/github_pr_diff_view.rs");
        let path_key = file.path_key.clone();
        let entries = build_github_pr_diff_tree_entries(vec![file], &HashMap::default());
        let tree_file = entries
            .iter()
            .find_map(|entry| match entry {
                GitHubPrDiffTreeEntry::File(file) => Some(&file.file),
                GitHubPrDiffTreeEntry::Directory(_) => None,
            })
            .expect("tree should contain file entry");

        assert_eq!(tree_file.path_key, path_key);
    }

    #[test]
    fn test_tree_builder_respects_collapsed_directories() {
        let mut expanded_dirs = HashMap::default();
        expanded_dirs.insert("crates".to_string(), false);

        let entries = build_github_pr_diff_tree_entries(
            vec![
                diff_file(0, "crates/git_ui/src/github_pr_diff_view.rs"),
                diff_file(1, "crates/http_client/src/github.rs"),
            ],
            &expanded_dirs,
        );

        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            GitHubPrDiffTreeEntry::Directory(directory)
                if directory.path == "crates" && !directory.expanded
        ));
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
