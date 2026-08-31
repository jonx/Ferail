use ferail_core::EntryKind;
use gpui::{AppContext as _, Context, Window};
use gpui_component::{WindowExt as _, notification::Notification};

use super::{CreateChecksumFile, Shell, VerifyChecksums, tab::ToolResultSurface};

/// Whether a listing row looks like a checksum manifest.
///
/// Name first (`ferail_fs_native::verify::is_manifest_file_name`), then the
/// sniffed description for a manifest that does not wear a telling name. Both
/// the context menu's availability rule and double-click activation ask this
/// one question, so the menu can never offer Verify on a row the double-click
/// would open, or the reverse.
pub(crate) fn entry_is_manifest(entry: &ferail_core::FileEntry) -> bool {
    ferail_fs_native::verify::is_manifest_file_name(&entry.name)
        || entry.display_magic.contains("checksum")
}

impl Shell {
    pub(crate) fn open_verify_path(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let manifest = path.clone();
        let tasks = self.process.tasks.clone();
        let view = cx.new(|cx| crate::verify_view::VerifyView::new(path, tasks, cx));
        self.active_tab_mut().tool_result = Some(ToolResultSurface::verify(manifest, view));
        cx.notify();
    }

    pub(crate) fn on_create_checksum_file(
        &mut self,
        _: &CreateChecksumFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Create checksum file");
        let root = self.active_tab().current_dir.clone();
        let selected = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        crate::create_checksum::open_dialog(root, selected, window, cx);
    }

    pub(crate) fn on_verify_checksums(
        &mut self,
        _: &VerifyChecksums,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Verify checksums");
        let targets = self.action_entries_visible_order(cx);
        if targets.len() != 1 {
            window.push_notification(
                Notification::info(tr!("Select one checksum manifest to verify.")),
                cx,
            );
            return;
        }
        let Some((_, entry, path)) = targets.into_iter().next() else {
            return;
        };
        if matches!(entry.kind, EntryKind::Directory) {
            window.push_notification(
                Notification::info(tr!("Select one checksum manifest to verify.")),
                cx,
            );
            return;
        }

        self.open_verify_path(path, cx);
    }
}
