//! Built-in lightweight text editor (docs/features/TEXT_EDITOR.md).
//!
//! One small standalone window per file, deliberately not an IDE: no tabs,
//! no project model, no LSP. The whole file is read off-thread (with a size
//! guard), edited in gpui-component's `Editor` widget (which brings undo,
//! find, and tree-sitter highlighting by extension), and Cmd+S writes it
//! back off-thread. Closing with unsaved changes prompts Save / Don't Save /
//! Cancel, both from the window's close button and from Esc / Cmd+W.
//!
//! Find and replace come from the widget (Cmd+F / Cmd+Shift+F, Ctrl+F /
//! Ctrl+H): this module only makes them reachable from the toolbar and keeps
//! Esc from closing the window out from under an open search panel. Reload,
//! soft wrap and line numbers are ours.
//!
//! Saving writes the full text to a unique hidden sibling first, so the
//! bytes are durably on disk before the original is touched, then rewrites
//! the original **in place** (same inode, so Finder tags, permissions, and
//! creation date survive) and removes the sibling. If the in-place write
//! fails midway, the sibling is left behind and the error toast names it as
//! the recovery copy.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable as _, Root, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, InputEvent, Position, RopeExt as _},
    menu::DropdownMenu as _,
    notification::Notification,
    v_flex,
};

use crate::shell::Shell;
use crate::text::TextScale as _;

/// Key-binding context for the editor window: Cmd+S saves, Esc / Cmd+W
/// close (through the unsaved-changes guard). Bound in
/// `keymap::install_extras`.
pub const TEXT_EDITOR_CONTEXT: &str = "TextEditor";

actions!(
    text_editor,
    [
        EditorSave,
        EditorDismiss,
        EditorZoomIn,
        EditorZoomOut,
        EditorZoomReset,
        EditorRevealFile,
        EditorReload,
        EditorToggleWrap,
        EditorToggleLineNumbers,
    ]
);

/// Editor font size at 100 %, and the zoom bounds around it. The step is
/// the viewer's, so a zoom gesture feels the same everywhere.
const BASE_FONT_PX: f32 = 13.0;
const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 4.0;
const ZOOM_STEP: f32 = 1.15;

fn command_tooltip(label: SharedString, mac: &str, other: &str) -> SharedString {
    format!(
        "{label} ({})",
        if cfg!(target_os = "macos") {
            mac
        } else {
            other
        }
    )
    .into()
}

/// Refuse files past this size. The widget is comfortable to ~50K lines,
/// and "fast and simple" stops being either on a huge file. The system
/// editor entry in the context menu covers the rest.
const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;
/// Same guard expressed in lines, for files made of very short lines.
const MAX_EDIT_LINES: usize = 100_000;

/// Number of editor windows currently open. Drives the spiral cascade,
/// exactly like Get Info's (see [`crate::window_cascade`]).
static OPEN_TEXT_EDITOR_WINDOWS: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct CascadeGuard {
    slot: usize,
}

impl CascadeGuard {
    fn claim() -> Self {
        let slot = OPEN_TEXT_EDITOR_WINDOWS.fetch_add(1, Ordering::Relaxed);
        CascadeGuard { slot }
    }
}

impl Drop for CascadeGuard {
    fn drop(&mut self) {
        OPEN_TEXT_EDITOR_WINDOWS.fetch_sub(1, Ordering::Relaxed);
    }
}

enum LoadState {
    Loading,
    Ready,
    /// The file exists but the editor deliberately refuses it (too large,
    /// or not UTF-8 text). The message says why; the file is untouched.
    Refused(SharedString),
    Failed(SharedString),
}

/// What the background reader hands back.
enum ReadOutcome {
    Text {
        text: String,
        had_crlf: bool,
        had_bom: bool,
    },
    TooLarge(u64),
    NotText,
    Failed(String),
}

/// Open a standalone editor window for `path`. `name` is the display leaf
/// (from the selected row); `shell` lets a successful save refresh the
/// containing directory's listing.
pub fn open(
    path: PathBuf,
    name: String,
    shell: WeakEntity<Shell>,
    origin_tab: crate::shell::TabId,
    cx: &mut App,
) {
    let cascade = CascadeGuard::claim();
    let shown = crate::private_mode::present_leaf_str(&name, false);
    let title: SharedString = tr!("Edit: {name}", name = shown.clone());
    let window_size = size(px(780.0), px(620.0));
    let opts = WindowOptions {
        window_bounds: Some(crate::window_cascade::cascaded_bounds(
            cascade.slot,
            window_size,
            cx,
        )),
        titlebar: Some(TitlebarOptions {
            title: Some(title.clone()),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let handle = cx.open_window(opts, move |window, cx| {
        crate::boot::install_dev_window_callback_cleanup(window, cx);
        let view = cx.new(|cx| {
            TextEditorView::new(
                path,
                name,
                Some(shell),
                Some(origin_tab),
                Some(cascade),
                window,
                cx,
            )
        });
        // The OS close button must honour the unsaved-changes guard too;
        // programmatic closes (`remove_window`) bypass this callback, which
        // is exactly what Save-then-close and Don't-Save rely on.
        let weak = view.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |view, cx| view.platform_should_close(window, cx))
                .unwrap_or(true)
        });
        // The rich editor owns a more-specific key context and can consume
        // Escape before the parent view sees EditorDismiss. Intercept the raw
        // key for this native window so Escape reaches the guarded close path.
        // Escape must reach the search panel first. Closing it here rather
        // than letting the widget do it keeps the behaviour identical whether
        // focus sits in the query field or back in the text.
        let target_window = window.window_handle();
        let escape_view = view.downgrade();
        let escape_subscription = cx.intercept_keystrokes(move |event, window, app| {
            if event.keystroke.key != "escape"
                || window.window_handle() != target_window
                || window.has_active_dialog(app)
            {
                return;
            }
            let _ = escape_view.update(app, |view, cx| {
                if view.search_open(cx) {
                    view.editor.update(cx, |state, cx| state.close_search(cx));
                } else {
                    view.request_dismiss(window, cx);
                }
            });
            app.stop_propagation();
        });
        view.update(cx, |view, _| {
            view._escape_subscription = Some(escape_subscription)
        });
        cx.new(|cx| Root::new(view, window, cx))
    });
    if let Ok(handle) = handle {
        crate::process_state::process_state(cx)
            .register_aux_window(handle.into(), title.to_string());
        crate::boot::refresh_window_menu(cx);
    }
}

pub struct TextEditorView {
    path: PathBuf,
    name: String,
    editor: Entity<EditorState>,
    load: LoadState,
    dirty: bool,
    saving: bool,
    /// A re-read of the file is in flight (the Reload command).
    reloading: bool,
    /// View toggles the widget owns but does not expose a getter for, so we
    /// keep the authoritative copy here and push it down on change. Both
    /// match `EditorState`'s own defaults.
    soft_wrap: bool,
    line_numbers: bool,
    /// Text zoom factor. Scales the editor's font size only; the rest of
    /// the chrome follows the app-wide `ui_scale` as usual.
    zoom: f32,
    /// Line-ending / BOM shape observed at load, restored verbatim on save
    /// so a CRLF file stays CRLF and a BOM'd file keeps its BOM.
    had_crlf: bool,
    had_bom: bool,
    /// Set when the user chose "Don't Save", so the close that follows
    /// doesn't re-prompt.
    allow_close: bool,
    did_focus: bool,
    /// Last OS window title pushed, to avoid re-setting it every frame.
    last_title: String,
    focus_handle: FocusHandle,
    shell: Option<WeakEntity<Shell>>,
    origin_tab: Option<crate::shell::TabId>,
    _escape_subscription: Option<Subscription>,
    _cascade: Option<CascadeGuard>,
}

impl TextEditorView {
    pub(crate) fn new(
        path: PathBuf,
        name: String,
        shell: Option<WeakEntity<Shell>>,
        origin_tab: Option<crate::shell::TabId>,
        cascade: Option<CascadeGuard>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Extension-only language pick: no content sniffing here (prime
        // directive: constructors run on the UI thread). Unknown extensions
        // fall back to plain text inside the widget.
        let language = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_else(|| "text".to_string());
        let editor = cx.new(|cx| EditorState::new(window, cx).language(language));
        cx.subscribe(&editor, |this: &mut Self, _, ev, cx| {
            if matches!(ev, InputEvent::Change)
                && matches!(this.load, LoadState::Ready)
                && !this.dirty
            {
                this.dirty = true;
                cx.notify();
            }
        })
        .detach();
        // `InputEvent` fires on text change only, but the status bar reports
        // the caret, which moves without an edit. Observing the entity catches
        // those; the editor repaints on those frames anyway.
        cx.observe(&editor, |_, _, cx| cx.notify()).detach();

        // Read the whole file on the background executor, then apply under
        // the window so `set_value` can run. The window handle re-entry is
        // the same shape `confirm_fanout` uses.
        let handle = window.window_handle();
        let load_path = path.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { read_for_edit(&load_path) })
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view: &mut Self, cx| {
                    view.apply_load(outcome, None, window, cx);
                });
            });
        })
        .detach();

        Self {
            path,
            name,
            editor,
            load: LoadState::Loading,
            dirty: false,
            saving: false,
            reloading: false,
            soft_wrap: true,
            line_numbers: true,
            zoom: 1.0,
            had_crlf: false,
            had_bom: false,
            allow_close: false,
            did_focus: false,
            last_title: String::new(),
            focus_handle: cx.focus_handle(),
            shell,
            origin_tab,
            _escape_subscription: None,
            _cascade: cascade,
        }
    }

    /// Install a freshly read file. `restore_cursor` carries the caret
    /// position from before a reload, so re-reading a file does not throw the
    /// user back to line 1.
    fn apply_load(
        &mut self,
        outcome: ReadOutcome,
        restore_cursor: Option<Position>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // This read replaces the buffer, so nothing is pending a save. The
        // failing arms need it too: a reload of a vanished file leaves no
        // editor to save from, and a stale `dirty` would trap the close
        // behind a Save button that can do nothing.
        self.dirty = false;
        match outcome {
            ReadOutcome::Text {
                text,
                had_crlf,
                had_bom,
            } => {
                self.had_crlf = had_crlf;
                self.had_bom = had_bom;
                // `set_value` suppresses Change events and clears undo, so
                // the initial fill neither marks the file dirty nor becomes
                // an undoable step.
                self.editor
                    .update(cx, |state, cx| state.set_value(text, window, cx));
                if let Some(position) = restore_cursor {
                    self.editor.update(cx, |state, cx| {
                        // The file may have shrunk under us; the column
                        // clamps itself, the line does not.
                        let last = state.text().lines_len().saturating_sub(1) as u32;
                        state.set_cursor_position(
                            Position {
                                line: position.line.min(last),
                                character: position.character,
                            },
                            window,
                            cx,
                        );
                    });
                }
                self.load = LoadState::Ready;
            }
            ReadOutcome::TooLarge(bytes) => {
                self.load = LoadState::Refused(tr!(
                    "This file is {size}, too large for the built-in editor.",
                    size = ferail_fs_native::humanize_bytes(bytes)
                ));
            }
            ReadOutcome::NotText => {
                self.load = LoadState::Refused(tr!(
                    "This file doesn't look like plain text, so it can't be edited here."
                ));
            }
            ReadOutcome::Failed(error) => {
                self.load =
                    LoadState::Failed(tr!("Could not read the file: {error}", error = error));
            }
        }
        cx.notify();
    }

    fn on_save(&mut self, _: &EditorSave, window: &mut Window, cx: &mut Context<Self>) {
        self.save(false, window, cx);
    }

    /// Snapshot the text and write it off-thread. `close_after` closes the
    /// window once the write lands (the Save path of the unsaved-changes
    /// prompt). Edits made while the write is in flight keep the file dirty.
    fn save(&mut self, close_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !matches!(self.load, LoadState::Ready) {
            return;
        }
        self.saving = true;
        cx.notify();
        let text = self.editor.read(cx).value();
        let saved_text = text.clone();
        let path = self.path.clone();
        let (had_crlf, had_bom) = (self.had_crlf, self.had_bom);
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { write_for_edit(&path, &text, had_crlf, had_bom) })
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view: &mut Self, cx| {
                    view.saving = false;
                    match result {
                        Ok(()) => {
                            view.dirty = view.editor.read(cx).value() != saved_text;
                            if let (Some(shell), Some(dir)) =
                                (view.shell.as_ref(), view.path.parent())
                            {
                                let dir = dir.to_path_buf();
                                let _ = shell.update(cx, |shell, cx| {
                                    shell.reload_tabs_matching_paths(&[dir], cx);
                                });
                            }
                            if close_after && !view.dirty {
                                view.allow_close = true;
                                window.remove_window();
                                return;
                            }
                        }
                        Err(error) => {
                            window.push_notification(
                                Notification::error(tr!(
                                    "Could not save {name}: {error}",
                                    name = crate::private_mode::present_leaf_str(&view.name, false),
                                    error = error
                                )),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// True while the widget's find/replace panel is showing.
    fn search_open(&self, cx: &App) -> bool {
        self.editor.read(cx).search_session().open
    }

    /// Open the widget's own find (or find-and-replace) panel. The keyboard
    /// route (Cmd+F / Cmd+Shift+F, Ctrl+F / Ctrl+H) is gpui-component's; this
    /// is the toolbar route to the same panel.
    fn open_search(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        if !matches!(self.load, LoadState::Ready) {
            return;
        }
        self.editor
            .update(cx, |state, cx| state.open_search(replace_mode, cx));
        cx.notify();
    }

    fn on_reload(&mut self, _: &EditorReload, window: &mut Window, cx: &mut Context<Self>) {
        self.request_reload(window, cx);
    }

    /// Re-read the file from disk. With unsaved edits this asks first: a
    /// reload throws them away and there is no undo across the swap.
    fn request_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.reloading || matches!(self.load, LoadState::Loading) {
            return;
        }
        if self.dirty {
            self.prompt_reload(window, cx);
        } else {
            self.reload(window, cx);
        }
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reloading = true;
        cx.notify();
        let cursor = matches!(self.load, LoadState::Ready)
            .then(|| self.editor.read(cx).cursor_position());
        let path = self.path.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { read_for_edit(&path) })
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view: &mut Self, cx| {
                    view.reloading = false;
                    view.apply_load(outcome, cursor, window, cx);
                });
            });
        })
        .detach();
    }

    fn on_toggle_wrap(&mut self, _: &EditorToggleWrap, window: &mut Window, cx: &mut Context<Self>) {
        self.soft_wrap = !self.soft_wrap;
        let wrap = self.soft_wrap;
        self.editor
            .update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        cx.notify();
    }

    fn on_toggle_line_numbers(
        &mut self,
        _: &EditorToggleLineNumbers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.line_numbers = !self.line_numbers;
        let on = self.line_numbers;
        self.editor
            .update(cx, |state, cx| state.set_line_number(on, window, cx));
        cx.notify();
    }

    fn on_dismiss(&mut self, _: &EditorDismiss, window: &mut Window, cx: &mut Context<Self>) {
        self.request_dismiss(window, cx);
    }

    fn request_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty && !self.allow_close {
            self.prompt_unsaved(window, cx);
        } else {
            window.remove_window();
        }
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        cx.notify();
    }

    fn on_zoom_in(&mut self, _: &EditorZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &EditorZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0 / ZOOM_STEP, cx);
    }

    fn on_zoom_reset(&mut self, _: &EditorZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        cx.notify();
    }

    /// Select this file back in a Ferail browsing window, so the user can
    /// get to its folder, siblings, Get Info, and so on without hunting
    /// for it. Returns to the exact source tab when it still exists, with
    /// the ordinary reveal policy as a fallback.
    fn on_reveal_file(&mut self, _: &EditorRevealFile, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(shell), Some(tab_id)) = (&self.shell, self.origin_tab) {
            crate::shell::reselect_path_in_origin(cx, shell, tab_id, self.path.clone());
        } else {
            crate::shell::reveal_path_in_app(cx, self.path.clone());
        }
    }

    /// Icon toolbar under the title bar. Same shape as the viewer's: one
    /// dense row of icon buttons with tooltips, no labels.
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ready = matches!(self.load, LoadState::Ready);
        let zoom_label = format!("{:.0}%", self.zoom * 100.0);
        let border = cx.theme().border;
        let separator = move || div().w(px(1.0)).h(px(20.0)).bg(border);
        let view_focus = self.focus_handle.clone();
        let (soft_wrap, line_numbers) = (self.soft_wrap, self.line_numbers);
        h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(border)
            .child(
                Button::new("editor-save")
                    .icon(gpui_component::Icon::empty().path("icons/save.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Save"), "⌘S", "Ctrl+S"))
                    .disabled(!ready || self.saving)
                    .on_click(cx.listener(|view, _, window, cx| view.save(false, window, cx))),
            )
            .child(
                Button::new("editor-reload")
                    .icon(gpui_component::Icon::empty().path("icons/nav/refresh.svg"))
                    .small()
                    .tooltip(tr!("Reload from Disk"))
                    .disabled(!ready || self.saving || self.reloading)
                    .on_click(cx.listener(|view, _, window, cx| view.request_reload(window, cx))),
            )
            .child(
                Button::new("editor-reveal")
                    .icon(gpui_component::Icon::empty().path("icons/locate-fixed.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Show in Ferail"), "⌘R", "Ctrl+R"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.on_reveal_file(&EditorRevealFile, window, cx)
                    })),
            )
            .child(separator())
            .child(
                Button::new("editor-find")
                    .icon(gpui_component::Icon::empty().path("icons/search.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Find"), "⌘F", "Ctrl+F"))
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, _, cx| view.open_search(false, cx))),
            )
            .child(
                Button::new("editor-replace")
                    .icon(gpui_component::Icon::empty().path("icons/replace.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Find and Replace"), "⌘⇧F", "Ctrl+H"))
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, _, cx| view.open_search(true, cx))),
            )
            .child(separator())
            .child(
                Button::new("editor-zoom-out")
                    .icon(gpui_component::Icon::empty().path("icons/minus.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Smaller Text"), "⌘−", "Ctrl+−"))
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, _, cx| view.zoom_by(1.0 / ZOOM_STEP, cx))),
            )
            .child(
                div()
                    .id("editor-zoom-reset")
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(44.0))
                    .text_center()
                    .cursor_pointer()
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(command_tooltip(
                            tr!("Reset Text Size"),
                            "⌘0",
                            "Ctrl+0",
                        ))
                        .build(window, cx)
                    })
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.zoom = 1.0;
                        cx.notify();
                    }))
                    .child(zoom_label),
            )
            .child(
                Button::new("editor-zoom-in")
                    .icon(gpui_component::Icon::empty().path("icons/plus.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Bigger Text"), "⌘+", "Ctrl++"))
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, _, cx| view.zoom_by(ZOOM_STEP, cx))),
            )
            .child(separator())
            // Display toggles, not commands: a checkable menu shows their
            // state where an icon button could not.
            .child(
                Button::new("editor-view-menu")
                    .icon(gpui_component::Icon::empty().path("icons/ellipsis.svg"))
                    .small()
                    .tooltip(tr!("More"))
                    .disabled(!ready)
                    .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _window, _cx| {
                        menu.action_context(view_focus.clone())
                            .menu_with_check(
                                tr!("Wrap Lines"),
                                soft_wrap,
                                Box::new(EditorToggleWrap),
                            )
                            .menu_with_check(
                                tr!("Line Numbers"),
                                line_numbers,
                                Box::new(EditorToggleLineNumbers),
                            )
                    }),
            )
            .child(div().flex_1())
            .when(self.dirty, |bar| {
                bar.child(
                    div()
                        .text_scale_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("Unsaved")),
                )
            })
    }

    /// Footer strip: caret position, line count, encoding, line endings.
    /// All derived from state already in memory, so it costs a rope lookup
    /// and no I/O. Line and column are coordinates, not counts, so they are
    /// not digit-grouped; the line total is, through `trn!`. No file size:
    /// it goes stale on the first keystroke and re-reading it here would be
    /// a filesystem call on the UI thread.
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ready = matches!(self.load, LoadState::Ready);
        let editor = self.editor.read(cx);
        let position = editor.cursor_position();
        let lines = editor.text().lines_len();
        let text = if ready {
            [
                tr!(
                    "Line {line}, Column {column}",
                    line = position.line + 1,
                    column = position.character + 1
                )
                .to_string(),
                trn!("{n} line", "{n} lines", lines).to_string(),
                // Encoding and line-ending names are identifiers, not prose:
                // they read the same in every language.
                if self.had_bom { "UTF-8 (BOM)" } else { "UTF-8" }.to_string(),
                if self.had_crlf { "CRLF" } else { "LF" }.to_string(),
            ]
            .join(" · ")
        } else {
            String::new()
        };
        h_flex()
            .flex_none()
            .items_center()
            .justify_end()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(text),
            )
    }

    /// The OS close-button path: `true` lets the window close.
    fn platform_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.allow_close || !self.dirty {
            return true;
        }
        self.prompt_unsaved(window, cx);
        false
    }

    fn prompt_unsaved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let weak = cx.weak_entity();
        let name = crate::private_mode::present_leaf_str(&self.name, false);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let weak_save = weak.clone();
            let weak_discard = weak.clone();
            dialog
                .title(tr!("Unsaved Changes"))
                .child(div().text_scale_sm().child(tr!(
                    "\u{201C}{name}\u{201D} has changes that haven't been saved.",
                    name = name.clone()
                )))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("editor-dont-save")
                                .label(tr!("Don't Save"))
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak_discard.update(cx, |view, _| {
                                        view.allow_close = true;
                                    });
                                    window.remove_window();
                                }),
                        )
                        .child(
                            Button::new("editor-close-cancel")
                                .label(tr!("Cancel"))
                                .small()
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("editor-save-close")
                                .label(tr!("Save"))
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak_save.update(cx, |view, cx| {
                                        view.save(true, window, cx);
                                    });
                                }),
                        ),
                )
        });
    }

    /// Reload with unsaved edits: say plainly that they go away, because the
    /// swap is not undoable.
    fn prompt_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let weak = cx.weak_entity();
        let name = crate::private_mode::present_leaf_str(&self.name, false);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let weak_reload = weak.clone();
            dialog
                .title(tr!("Reload from Disk"))
                .child(div().text_scale_sm().child(tr!(
                    "Reloading \u{201C}{name}\u{201D} discards your unsaved changes.",
                    name = name.clone()
                )))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("editor-reload-cancel")
                                .label(tr!("Cancel"))
                                .small()
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("editor-reload-confirm")
                                .label(tr!("Reload"))
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak_reload.update(cx, |view, cx| {
                                        view.reload(window, cx);
                                    });
                                }),
                        ),
                )
        });
    }

    /// Fallback for a refused file: hand it to the platform text editor,
    /// same off-thread call as the context menu's system-editor entry.
    fn open_externally(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let window = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::platform_shell::edit_text_file(&path) })
                .await;
            if let Err(error) = result {
                let _ = window.update(cx, |_, window, cx| {
                    window.push_notification(
                        Notification::error(tr!(
                            "Could not open text editor: {error}",
                            error = error
                        )),
                        cx,
                    );
                });
            }
        })
        .detach();
    }
}

impl Focusable for TextEditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextEditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let private = crate::private_mode::enabled();
        if !private {
            let base_title = tr!("Edit: {name}", name = self.name.clone());
            let title = if self.dirty {
                format!("\u{2022} {base_title}")
            } else {
                base_title.to_string()
            };
            if title != self.last_title {
                window.set_window_title(&title);
                self.last_title = title;
            }
        } else {
            // Private Mode owns the caption; forget the last title so exit
            // repaints ours on the next frame.
            self.last_title.clear();
        }
        let muted = cx.theme().muted_foreground;

        let body = if private {
            // Fail-closed: the file's text is user content, so blank the whole
            // stage, keep the window chrome (same stance as the viewer).
            div().flex_1().into_any_element()
        } else {
            match &self.load {
                LoadState::Loading => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_scale_sm()
                            .text_color(muted)
                            .child(tr!("Loading…")),
                    )
                    .into_any_element(),
                LoadState::Ready => {
                    if !self.did_focus {
                        self.did_focus = true;
                        self.editor.update(cx, |state, cx| state.focus(window, cx));
                    }
                    div()
                        .flex_1()
                        .min_h_0()
                        .p_2()
                        .child(
                            Editor::new(&self.editor)
                                .h(relative(1.))
                                .bordered(false)
                                .text_size(px(BASE_FONT_PX * self.zoom)),
                        )
                        .into_any_element()
                }
                LoadState::Refused(msg) | LoadState::Failed(msg) => {
                    if !self.did_focus {
                        self.did_focus = true;
                        window.focus(&self.focus_handle, cx);
                    }
                    let external_label = if cfg!(target_os = "macos") {
                        tr!("Edit in TextEdit")
                    } else if cfg!(windows) {
                        tr!("Edit in Notepad")
                    } else {
                        tr!("Edit in Text Editor")
                    };
                    let refused = matches!(self.load, LoadState::Refused(_));
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(
                            div()
                                .text_scale_sm()
                                .text_color(muted)
                                .max_w(px(420.))
                                .text_center()
                                .child(msg.clone()),
                        )
                        .when(refused, |this| {
                            this.child(
                                Button::new("editor-open-external")
                                    .label(external_label)
                                    .small()
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.open_externally(window, cx);
                                    })),
                            )
                        })
                        .into_any_element()
                }
            }
        };

        let content = v_flex()
            .key_context(TEXT_EDITOR_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_reveal_file))
            .on_action(cx.listener(Self::on_reload))
            .on_action(cx.listener(Self::on_toggle_wrap))
            .on_action(cx.listener(Self::on_toggle_line_numbers))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(!private, |this| this.child(self.toolbar(cx)))
            .child(body)
            // Private Mode blanks the stage, so the footer goes with it: the
            // caret position and line count describe the file's content.
            .when(!private, |this| this.child(self.status_bar(cx)))
            // This window's own Root holds dialog/notification state but
            // doesn't render the layers; do it here so the unsaved-changes
            // dialog and save-error toasts appear.
            .children(Root::render_dialog_layer(window, cx))
            .when(!crate::private_mode::enabled(), |this| {
                this.children(Root::render_notification_layer(window, cx))
            })
            .into_any_element();
        crate::private_mode::protect(content, cx)
    }
}

/// Read `path` for editing: whole file, bounded, UTF-8 only. Blocking:
/// background executor only.
fn read_for_edit(path: &Path) -> ReadOutcome {
    ferail_core::path_guard::assert_off_ui_thread("text_editor read");
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ReadOutcome::Failed(e.to_string()),
    };
    if meta.len() > MAX_EDIT_BYTES {
        return ReadOutcome::TooLarge(meta.len());
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ReadOutcome::Failed(e.to_string()),
    };
    let (bytes, had_bom) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => (rest.to_vec(), true),
        None => (bytes, false),
    };
    if bytes.contains(&0) {
        return ReadOutcome::NotText;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return ReadOutcome::NotText;
    };
    if text.bytes().filter(|b| *b == b'\n').count() >= MAX_EDIT_LINES {
        return ReadOutcome::TooLarge(meta.len());
    }
    let had_crlf = text.contains("\r\n");
    let text = if had_crlf {
        text.replace("\r\n", "\n")
    } else {
        text
    };
    ReadOutcome::Text {
        text,
        had_crlf,
        had_bom,
    }
}

/// Write the edited text back. Blocking: background executor only.
///
/// Two steps: (1) the full serialized text goes to a unique hidden sibling
/// (durable before the original is touched), (2) the original is rewritten
/// **in place**, same inode, so Finder tags, ACLs, permissions, and the
/// creation date all survive, which a rename-over would silently drop,
/// then the sibling is removed. If step 2 fails the sibling stays behind
/// and the error names it as the recovery copy.
fn write_for_edit(path: &Path, text: &str, had_crlf: bool, had_bom: bool) -> Result<(), String> {
    ferail_core::path_guard::assert_off_ui_thread("text_editor write");
    let mut bytes: Vec<u8> = Vec::with_capacity(text.len() + 3);
    if had_bom {
        bytes.extend_from_slice(b"\xEF\xBB\xBF");
    }
    if had_crlf {
        bytes.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
    } else {
        bytes.extend_from_slice(text.as_bytes());
    }

    match crate::safe_write::write_bytes_in_place(path, &bytes, "edit") {
        Ok(()) => Ok(()),
        Err(fail) => Err(match fail.backup {
            Some(backup) => tr!(
                "{error}. Your text was preserved in {backup}.",
                error = fail.error,
                backup = backup.display()
            )
            .to_string(),
            None => fail.error,
        }),
    }
}

#[cfg(test)]
mod tests {
    // Deliberately no `use super::*`: that would pull in `gpui::*`, whose
    // `test` attribute macro shadows the built-in `#[test]`.
    use super::{ReadOutcome, read_for_edit, write_for_edit};

    #[test]
    fn read_write_round_trip_preserves_crlf_and_bom() {
        let dir = std::env::temp_dir().join(format!("ferail-edit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("crlf.txt");
        std::fs::write(&path, b"\xEF\xBB\xBFline one\r\nline two\r\n").unwrap();

        let ReadOutcome::Text {
            text,
            had_crlf,
            had_bom,
        } = read_for_edit(&path)
        else {
            panic!("expected text");
        };
        assert!(had_crlf);
        assert!(had_bom);
        assert_eq!(text, "line one\nline two\n");

        let edited = "line one\nline 2\n";
        write_for_edit(&path, edited, had_crlf, had_bom).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"\xEF\xBB\xBFline one\r\nline 2\r\n"
        );
        // No leftover temp sibling after a clean save.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("ferail-edit"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_refuses_binary_and_oversize() {
        let dir = std::env::temp_dir().join(format!("ferail-edit-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("bin.dat");
        std::fs::write(&bin, [0u8, 159, 146, 150]).unwrap();
        assert!(matches!(read_for_edit(&bin), ReadOutcome::NotText));

        let missing = dir.join("nope.txt");
        assert!(matches!(read_for_edit(&missing), ReadOutcome::Failed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
