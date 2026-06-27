use anyhow::Result;
use buffer_diff::BufferDiff;
use editor::{SplittableEditor, multibuffer_context_lines};
use gpui::{App, AppContext as _, AsyncWindowContext, Context, Entity, Window};
use language::{
    Buffer, Capability, DiskState, File, LanguageRegistry, LineEnding, OffsetRangeExt as _,
    ReplicaId, Rope, TextBuffer,
};
use multi_buffer::PathKey;
use project::WorktreeId;
use settings::DiffViewStyle;
use std::{ops::Range, path::PathBuf, sync::Arc};
use util::{ResultExt, paths::PathStyle, rel_path::RelPath};

pub(crate) struct VirtualDiffFile {
    path: Arc<RelPath>,
    display_name: String,
    worktree_id: WorktreeId,
    is_deleted: bool,
    is_binary: bool,
}

pub(crate) struct VirtualDiffEntry {
    pub(crate) path: PathKey,
    pub(crate) buffer: Entity<Buffer>,
    pub(crate) diff: Entity<BufferDiff>,
}

impl VirtualDiffFile {
    pub(crate) fn new(
        path: Arc<RelPath>,
        display_name: String,
        worktree_id: WorktreeId,
        is_deleted: bool,
        is_binary: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            path,
            display_name,
            worktree_id,
            is_deleted,
            is_binary,
        })
    }
}

impl File for VirtualDiffFile {
    fn as_local(&self) -> Option<&dyn language::LocalFile> {
        None
    }

    fn disk_state(&self) -> DiskState {
        DiskState::Historic {
            was_deleted: self.is_deleted,
        }
    }

    fn path_style(&self, _: &App) -> PathStyle {
        PathStyle::local()
    }

    fn path(&self) -> &Arc<RelPath> {
        &self.path
    }

    fn full_path(&self, _: &App) -> PathBuf {
        self.path.as_std_path().to_path_buf()
    }

    fn file_name<'a>(&'a self, _: &'a App) -> &'a str {
        self.display_name.as_ref()
    }

    fn worktree_id(&self, _: &App) -> WorktreeId {
        self.worktree_id
    }

    fn to_proto(&self, _cx: &App) -> language::proto::File {
        unimplemented!()
    }

    fn is_private(&self) -> bool {
        false
    }

    fn can_open(&self) -> bool {
        !self.is_binary
    }
}

pub(crate) async fn build_virtual_buffer(
    mut text: String,
    file: Arc<dyn File>,
    capability: Capability,
    language_registry: &Arc<LanguageRegistry>,
    cx: &mut AsyncWindowContext,
) -> Result<Entity<Buffer>> {
    let line_ending = LineEnding::detect(&text);
    LineEnding::normalize(&mut text);
    let text = Rope::from(text);
    let language =
        cx.update(|_, cx| language_registry.language_for_file(&file, Some(&text), cx))?;
    let language = if let Some(language) = language {
        language_registry
            .load_language(&language)
            .await
            .ok()
            .and_then(|language| language.log_err())
    } else {
        None
    };

    let buffer = cx.new(|cx| {
        let text_buffer = TextBuffer::new_normalized(
            ReplicaId::LOCAL,
            cx.entity_id().as_non_zero_u64().into(),
            line_ending,
            text,
        );
        let mut buffer = Buffer::build(text_buffer, Some(file), capability);
        buffer.set_language_async(language, cx);
        buffer
    });
    Ok(buffer)
}

pub(crate) async fn build_virtual_buffer_diff(
    mut old_text: Option<String>,
    buffer: &Entity<Buffer>,
    language_registry: &Arc<LanguageRegistry>,
    cx: &mut AsyncWindowContext,
) -> Result<Entity<BufferDiff>> {
    if let Some(old_text) = &mut old_text {
        LineEnding::normalize(old_text);
    }

    let language = cx.update(|_, cx| buffer.read(cx).language().cloned())?;
    let buffer = cx.update(|_, cx| buffer.read(cx).snapshot())?;

    let diff =
        cx.new(|cx| BufferDiff::new(&buffer.text, language, Some(language_registry.clone()), cx));

    diff.update(cx, |diff, cx| {
        diff.set_base_text(
            old_text.map(|old_text| Arc::from(old_text.as_str())),
            buffer.text.clone(),
            cx,
        )
    })
    .await;

    Ok(diff)
}

pub(crate) fn diff_excerpt_ranges(
    buffer: &Entity<Buffer>,
    diff: &Entity<BufferDiff>,
    include_full_buffer_when_unchanged: bool,
    cx: &App,
) -> Vec<Range<language::Point>> {
    let snapshot = buffer.read(cx).snapshot();
    let diff_snapshot = diff.read(cx).snapshot(cx);
    let mut hunks = diff_snapshot.hunks(&snapshot).peekable();
    if include_full_buffer_when_unchanged && hunks.peek().is_none() {
        vec![language::Point::zero()..snapshot.max_point()]
    } else {
        hunks
            .map(|hunk| hunk.buffer_range.to_point(&snapshot))
            .collect::<Vec<_>>()
    }
}

pub(crate) fn insert_diff_excerpts(
    editor: &Entity<SplittableEditor>,
    entry: VirtualDiffEntry,
    ranges: Vec<Range<language::Point>>,
    is_first_batch: bool,
    split_on_first_batch: bool,
    window: &mut Window,
    cx: &mut Context<impl Sized + 'static>,
) {
    let context_lines = multibuffer_context_lines(cx);
    editor.update(cx, |editor, cx| {
        let added_new_excerpt = editor.update_excerpts_for_path(
            entry.path,
            entry.buffer,
            ranges,
            context_lines,
            entry.diff,
            cx,
        );
        if added_new_excerpt
            && is_first_batch
            && split_on_first_batch
            && editor.diff_view_style() == DiffViewStyle::Split
        {
            editor.split(window, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn test_virtual_diff_file_reports_filename_and_path(cx: &mut gpui::TestAppContext) {
        let path = RelPath::unix("app/Console/Commands/CreateFeatureFlagCommand.php")
            .unwrap()
            .into_arc();
        let file = VirtualDiffFile {
            path,
            display_name: "CreateFeatureFlagCommand.php".to_string(),
            worktree_id: WorktreeId::from_usize(7),
            is_deleted: false,
            is_binary: false,
        };

        cx.update(|cx| {
            assert_eq!(file.file_name(cx), "CreateFeatureFlagCommand.php");
            assert_eq!(
                file.path().as_unix_str(),
                "app/Console/Commands/CreateFeatureFlagCommand.php"
            );
            assert_eq!(
                file.full_path(cx),
                PathBuf::from("app/Console/Commands/CreateFeatureFlagCommand.php")
            );
            assert_eq!(file.worktree_id(cx), WorktreeId::from_usize(7));
            assert_eq!(
                file.disk_state(),
                DiskState::Historic { was_deleted: false }
            );
            assert!(file.can_open());
        });
    }

    #[gpui::test]
    async fn test_virtual_diff_binary_file_cannot_open(cx: &mut gpui::TestAppContext) {
        let file = VirtualDiffFile {
            path: RelPath::unix("assets/image.png").unwrap().into_arc(),
            display_name: "image.png".to_string(),
            worktree_id: WorktreeId::from_usize(1),
            is_deleted: false,
            is_binary: true,
        };

        cx.update(|_| {
            assert!(!file.can_open());
        });
    }
}
