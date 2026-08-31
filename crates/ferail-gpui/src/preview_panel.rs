//! The preview pane, as a host-agnostic component.
//!
//! It used to be ~700 lines of `Shell` methods reading the active tab's
//! selection directly. That made it unusable anywhere else, most visibly, an
//! archive workbench popped out into its own window had no preview, because
//! the pane belonged to the Shell in the *other* window.
//!
//! So the pane no longer decides *what* to show: the host pushes a
//! [`PreviewTarget`] in, and the panel renders it. `Shell` sets the target from
//! its selection; the archive workbench sets it from the entry it staged. The
//! panel owns its own scroll/º sizing state, so two hosts can show two different
//! previews at once without fighting over one set of handles.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, button::ButtonVariants as _, h_flex, v_flex};

use ferail_core::text_encoding::{AnsiColor, AnsiSpan};
use ferail_core::{EntryKind, FileEntry};

use crate::process_state::ProcessState;
use crate::shell::render::{
    PREVIEW_CODE_CHAR_W, PREVIEW_CODE_MAX_W, PREVIEW_CODE_PAD, PREVIEW_CODE_TAB_COLS,
    PREVIEW_MD_MIN_W, PREVIEW_TEXT_MAX_VISUAL_LINES, ResizePreviewThumb, truncated_url_value,
};
use crate::shell::{
    ClearQuarantine, CopyPath, OpenSelected, OpenViewer, RevealInFinder, SHELL_CONTEXT, Shell,
};
use crate::shell::{PREVIEW_THUMB_MAX_H, PREVIEW_THUMB_MIN_H};
use crate::text::TextScale as _;

fn terminal_font_family() -> &'static str {
    if cfg!(target_os = "macos") {
        // Monaco's box/block glyphs are drawn on the full character cell;
        // Menlo leaves visible seams in CP437 artwork at small sizes.
        "Monaco"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

/// Wrap a bounded, self-scrolling preview box in a vertical scrollbar.
///
/// The inner text boxes are capped at `max_h(280)` so a long file cannot bury
/// the Get Info details below them, which means a `.nfo` or a source file
/// usually has more content than the box shows and nothing on screen said so.
/// The bar rides a strip on the right edge instead of covering the box: the
/// scrollbar element takes the hitbox of whatever bounds it is given, and the
/// text underneath has to stay selectable.
fn scrolled_preview_box(body: impl IntoElement, scroll: &ScrollHandle) -> Div {
    div().relative().w_full().child(body).child(
        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(16.0))
            .child(
                gpui_component::scroll::Scrollbar::vertical(scroll)
                    // The theme default fades the bar out when idle, which
                    // is right for a pane you already know scrolls. Here
                    // the bar IS the "there is more below" signal for a
                    // box that stops at 280px, so it stays up. It still
                    // draws nothing when the content fits.
                    .mode(gpui_component::scroll::ScrollbarMode::Always),
            ),
    )
}

fn ansi_color(color: AnsiColor) -> Hsla {
    let rgb = match color {
        AnsiColor::Standard(index) => [
            0x1e1e1e, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
        ][index.min(7) as usize],
        AnsiColor::Bright(index) => [
            0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
        ][index.min(7) as usize],
        AnsiColor::Indexed(index) if index < 8 => {
            return ansi_color(AnsiColor::Standard(index));
        }
        AnsiColor::Indexed(index) if index < 16 => {
            return ansi_color(AnsiColor::Bright(index - 8));
        }
        AnsiColor::Indexed(index) if index < 232 => {
            let value = index - 16;
            let component = |part: u8| if part == 0 { 0 } else { 55 + part as u32 * 40 };
            let red = component(value / 36);
            let green = component((value / 6) % 6);
            let blue = component(value % 6);
            (red << 16) | (green << 8) | blue
        }
        AnsiColor::Indexed(index) => {
            let gray = 8 + (index as u32 - 232) * 10;
            (gray << 16) | (gray << 8) | gray
        }
        AnsiColor::Rgb(red, green, blue) => {
            ((red as u32) << 16) | ((green as u32) << 8) | blue as u32
        }
    };
    gpui::rgb(rgb).into()
}

fn terminal_text(document: &crate::text_preview::TextPreviewDocument) -> StyledText {
    let highlights = document.ansi_spans.iter().map(|AnsiSpan { range, style }| {
        (
            range.clone(),
            HighlightStyle {
                color: style.foreground.map(ansi_color),
                background_color: style.background.map(ansi_color),
                font_weight: style.bold.then_some(FontWeight::BOLD),
                ..HighlightStyle::default()
            },
        )
    });
    StyledText::new(document.text.clone()).with_highlights(highlights)
}

/// What the host wants previewed.
#[derive(Clone, Debug, Default)]
pub enum PreviewTarget {
    /// Nothing selected: the pane shows its empty state.
    #[default]
    None,
    /// A file or folder, with the row it came from (for name, size, kind).
    File {
        path: PathBuf,
        entry: Box<FileEntry>,
    },
    /// A mounted volume, previewed as itself (a sidebar volume click lands
    /// here, since navigating clears the selection).
    Volume { path: PathBuf, name: String },
    /// Content we already hold, with no file behind it: an archive entry we
    /// decoded in memory rather than writing out. Renderers we own (text,
    /// images) take this path, so previewing an archive entry usually touches
    /// no disk at all.
    InMemory {
        name: String,
        size: u64,
        content: PreviewContent,
    },
}

/// Decoded content for [`PreviewTarget::InMemory`].
#[derive(Clone)]
pub enum PreviewContent {
    Text(SharedString),
    Image(std::sync::Arc<RenderImage>),
}

impl std::fmt::Debug for PreviewContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewContent::Text(_) => f.write_str("Text(..)"),
            PreviewContent::Image(_) => f.write_str("Image(..)"),
        }
    }
}

/// A stand-in row for content that has no listing entry of its own: an
/// archive entry staged to a scratch file, labelled with its real name rather
/// than the scratch filename.
pub fn synthetic_entry(path: &std::path::Path, name: &str) -> FileEntry {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    FileEntry {
        id: ferail_core::NodeId::from_raw(1).expect("nonzero"),
        name: name.into(),
        display_name: name.into(),
        name_has_hazards: false,
        kind: EntryKind::File,
        size,
        mtime_unix: 0,
        display_size: ferail_fs_native::humanize_bytes(size).into(),
        display_kind: ferail_core::empty_entry_text(),
        display_magic: ferail_core::empty_entry_text(),
        display_description: ferail_core::empty_entry_text(),
        details_loaded: false,
        is_quarantined: false,
        quarantine: None,
        hidden: false,
        created_unix: None,
        locked: false,
    }
}

/// Raised when the pane's close button is pressed. The host decides what
/// hiding means: the Shell clears `preview_visible`, the archive workbench
/// also switches its own preview toggle off.
pub struct PreviewCloseRequested;

impl EventEmitter<PreviewCloseRequested> for PreviewPanel {}

pub struct PreviewPanel {
    target: PreviewTarget,
    process: Rc<ProcessState>,
    /// Kept for the embedded Get Info view, which reports back to the Shell.
    shell: WeakEntity<Shell>,
    scroll: ScrollHandle,
    text_scroll: ScrollHandle,
    /// Own handle for the folder sidecar box, which is a sibling of the file
    /// text box and never on screen at the same time, but needs a handle of
    /// its own so its scrollbar reports that box's extent, not the file one's.
    sidecar_scroll: ScrollHandle,
    /// Resets every scroll when the target changes.
    scroll_path: Option<PathBuf>,
    thumb_h: f32,
    /// While the thumbnail resize grip is dragged: (pointer y at press,
    /// height at press).
    thumb_drag: Option<(Pixels, f32)>,
    preview_info: Option<Entity<crate::entry_info::EntryInfoView>>,
    /// NFO chosen from a folder's sidecar card. Memory-only and cleared when
    /// the host points the panel at another target.
    sidecar_open: Option<PathBuf>,
}

impl PreviewPanel {
    pub fn new(process: Rc<ProcessState>, shell: WeakEntity<Shell>, thumb_h: f32) -> Self {
        Self {
            target: PreviewTarget::None,
            process,
            shell,
            scroll: ScrollHandle::new(),
            text_scroll: ScrollHandle::new(),
            sidecar_scroll: ScrollHandle::new(),
            scroll_path: None,
            thumb_h,
            thumb_drag: None,
            preview_info: None,
            sidecar_open: None,
        }
    }

    /// Point the pane at something else. Cheap and idempotent: hosts call it
    /// on every selection change.
    pub fn set_target(&mut self, target: PreviewTarget, cx: &mut Context<Self>) {
        let old_path = match &self.target {
            PreviewTarget::File { path, .. } | PreviewTarget::Volume { path, .. } => Some(path),
            _ => None,
        };
        let new_path = match &target {
            PreviewTarget::File { path, .. } | PreviewTarget::Volume { path, .. } => Some(path),
            _ => None,
        };
        if old_path != new_path {
            self.sidecar_open = None;
        }
        self.target = target;
        cx.notify();
    }

    pub fn thumb_h(&self) -> f32 {
        self.thumb_h
    }

    /// Sidecar currently expanded inside a folder card, when it belongs to
    /// `root`. Used by the directory Refresh command to re-request the same
    /// visible document after invalidating its process-memory cache.
    pub fn open_sidecar_under(&self, root: &std::path::Path) -> Option<PathBuf> {
        self.sidecar_open
            .as_ref()
            .filter(|path| path.starts_with(root))
            .cloned()
    }

    /// Body for content we hold in memory.
    fn in_memory_body(
        &mut self,
        name: String,
        size: u64,
        content: PreviewContent,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_scale_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(tr!("Preview")),
            )
            .child(
                gpui_component::button::Button::new("preview-close")
                    .small()
                    .ghost()
                    .icon(gpui_component::Icon::empty().path("icons/close.svg"))
                    .tooltip(tr!("Hide preview"))
                    .on_click(cx.listener(|_, _, _window, cx| {
                        cx.emit(PreviewCloseRequested);
                    })),
            );
        let body = match content {
            PreviewContent::Image(image) => div()
                .w_full()
                .child(img(image).max_w_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            PreviewContent::Text(text) => div()
                .w_full()
                .p_2()
                .rounded_md()
                .bg(theme.muted.opacity(0.4))
                .font_family(theme.mono_font_family.clone())
                .text_scale_xs()
                .child(text)
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .child(header)
            .child(div().text_scale_md().truncate().child(name))
            .child(
                div()
                    .text_scale_xs()
                    .text_color(theme.muted_foreground)
                    .child(ferail_fs_native::humanize_bytes(size)),
            )
            .child(
                div()
                    .id("preview-inmem-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }

    /// Create-or-retarget the embedded Get Info view. Mirrors the Shell method
    /// it replaced; the view still reports back to the Shell, which is why the
    /// panel carries a weak handle.
    fn sync_info(
        &mut self,
        path: PathBuf,
        name: String,
        target: ferail_core::entry_info::InfoTarget,
        known_size: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Entity<crate::entry_info::EntryInfoView> {
        match &self.preview_info {
            Some(view) => {
                let (p, n) = (path.clone(), name.clone());
                view.update(cx, |view, cx| view.retarget(p, n, known_size, cx));
            }
            None => {
                let view = cx.new(|cx| {
                    crate::entry_info::EntryInfoView::new_embedded(
                        path,
                        name,
                        target,
                        known_size,
                        self.shell.clone(),
                        cx,
                    )
                });
                self.preview_info = Some(view);
            }
        }
        self.preview_info.clone().expect("just set")
    }
}

impl Render for PreviewPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.preview(cx)
    }
}

impl PreviewPanel {
    /// Wheel scroll-chaining for the inline text/code box in the preview
    /// pane. The box is a nested scroll inside `preview_scroll`, bounded to
    /// `max_h(280)` on purpose so a long file doesn't bury the Get Info
    /// details below it. Without chaining the wheel drives both scrolls at
    /// once; we want the box to consume the delta and only spill the
    /// remainder into the outer pane.
    ///
    /// `overflow_scroll`'s built-in handler runs just before this one (in the
    /// same bubble pass) and has already added the full wheel delta to
    /// `preview_text_scroll`, unclamped, so `offset()` now sits *past* the
    /// top (positive) or bottom (below `-max_offset`) by exactly the part the
    /// box couldn't use. We forward that residual to `preview_scroll` and
    /// `stop_propagation` so the outer pane's own handler, which would
    /// otherwise apply the *whole* delta and double-scroll, never fires.
    ///
    /// A short file (box not scrollable, `max_offset == 0`) spills the entire
    /// delta straight through, so its box never traps the wheel.
    fn on_preview_text_scroll(
        &mut self,
        _: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let off = self.text_scroll.offset().y;
        let max = self.text_scroll.max_offset().y;
        let residual = if off > px(0.0) {
            off // overshot the top
        } else if off < -max {
            off + max // overshot the bottom
        } else {
            px(0.0) // the box absorbed the whole delta
        };
        if residual != px(0.0) {
            let cur = self.scroll.offset();
            let max_out = self.scroll.max_offset().y;
            let y = (cur.y + residual).clamp(-max_out, px(0.0));
            self.scroll.set_offset(point(cur.x, y));
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// The drag grip under the preview thumbnail box. Dragging it
    /// down/up grows/shrinks the box between `PREVIEW_THUMB_MIN_H`
    /// and `PREVIEW_THUMB_MAX_H`; the height persists via the same
    /// debounced save as the splitter widths. The drag anchor (mouse
    /// y + height at drag start) is snapped in the `on_drag`
    /// constructor; `on_drag_move` then applies the absolute delta,
    /// so the box edge tracks the cursor 1:1, no per-tick
    /// accumulation drift, no dependence on the pane's scroll offset.
    fn preview_thumb_resize_grip(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let weak = cx.weak_entity();
        div()
            .id("preview-thumb-resize")
            .group("preview-thumb-grip")
            .w_full()
            .h(px(9.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_row_resize()
            .child(
                div()
                    .w(px(48.0))
                    .h(px(3.0))
                    .rounded_full()
                    .bg(cx.theme().border)
                    .group_hover("preview-thumb-grip", |this| this.bg(cx.theme().drag_border)),
            )
            .on_drag(ResizePreviewThumb, move |drag, _offset, window, cx| {
                cx.stop_propagation();
                let y = window.mouse_position().y;
                if let Some(shell) = weak.upgrade() {
                    shell.update(cx, |this, _| {
                        this.thumb_drag = Some((y, this.thumb_h));
                    });
                }
                cx.new(|_| drag.clone())
            })
            .on_drag_move(cx.listener(
                |this, e: &DragMoveEvent<ResizePreviewThumb>, _window, cx| {
                    let Some((y0, h0)) = this.thumb_drag else {
                        return;
                    };
                    let h = (h0 + f32::from(e.event.position.y - y0))
                        .clamp(PREVIEW_THUMB_MIN_H, PREVIEW_THUMB_MAX_H);
                    if h != this.thumb_h {
                        this.thumb_h = h;
                        let _ = this.shell.update(cx, |shell, cx| {
                            shell.schedule_splitter_save(cx);
                        });
                        cx.notify();
                    }
                },
            ))
    }

    /// Build the preview pane on the right of the file list. Shows
    /// title / kind / size / modified / full path of the selected
    /// row. Falls back to a neutral empty state when nothing is
    /// selected. Format-specific previews (image, text, PDF) arrive
    /// in a follow-up polish iter.
    fn preview(&mut self, cx: &mut Context<Self>) -> Div {
        use gpui_component::{
            Sizable as _,
            button::{Button, ButtonVariants as _},
            scroll::Scrollbar,
            tooltip::Tooltip,
        };

        // The host decided what to show; the pane just renders it. This
        // used to reach into the active tab's selection, which is exactly what
        // made the pane unusable from any other window.
        // Content held in memory renders directly: no cache lookup (the caches
        // are path-keyed) and no Get Info block (there is no file to stat).
        if let PreviewTarget::InMemory {
            name,
            size,
            content,
        } = &self.target
        {
            let (name, size, content) = (name.clone(), *size, content.clone());
            return self.in_memory_body(name, size, content, cx);
        }
        let (selected, selected_path, volume_target) = match &self.target {
            PreviewTarget::None => (None, None, None),
            PreviewTarget::File { path, entry } => {
                (Some((**entry).clone()), Some(path.clone()), None)
            }
            PreviewTarget::Volume { path, name } => {
                (None, None, Some((path.clone(), name.clone())))
            }
            // Handled above; unreachable here.
            PreviewTarget::InMemory { .. } => (None, None, None),
        };
        let scroll_key = selected_path
            .clone()
            .or_else(|| volume_target.as_ref().map(|(path, _)| path.clone()));
        if self.scroll_path != scroll_key {
            self.scroll_path = scroll_key;
            self.scroll.set_offset(gpui::Point::default());
            self.text_scroll.set_offset(gpui::Point::default());
            self.sidecar_scroll.set_offset(gpui::Point::default());
        }

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_scale_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("Preview")),
            )
            .child(
                Button::new("preview-close")
                    .small()
                    .ghost()
                    .icon(gpui_component::Icon::empty().path("icons/close.svg"))
                    .tooltip(tr!("Hide preview"))
                    .on_click(cx.listener(|_, _, _window, cx| {
                        cx.emit(PreviewCloseRequested);
                    })),
            );

        let body: AnyElement = match (selected, volume_target) {
            // Sidebar volume click: preview the volume itself. The
            // embedded Get Info panel renders the Volume section
            // (capacity, used, format, device) once the background
            // gather lands.
            (None, Some((vol_path, vol_name))) => self.preview_volume_body(vol_path, vol_name, cx),
            (None, None) => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_scale_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("No selection"))
                .into_any_element(),
            (Some(entry), _) => {
                // Same render-safe resolution as `selected_path` above.
                let full_path = selected_path.clone().unwrap_or_default();

                // Keep the embedded Get Info panel pointed at the lead
                // selection (reuses the popup's view in `embedded` mode).
                let info_target = match entry.kind {
                    EntryKind::Directory => ferail_core::entry_info::InfoTarget::Folder,
                    _ => ferail_core::entry_info::InfoTarget::File,
                };
                // Hand the folder's already-computed recursive size (from the
                // Size column) to Get Info so it reuses it, not rescans.
                let known_size = if matches!(entry.kind, EntryKind::Directory) && entry.size > 0 {
                    Some(entry.size)
                } else {
                    None
                };
                let info_view = self.sync_info(
                    full_path.clone(),
                    entry.name.to_string(),
                    info_target,
                    known_size,
                    cx,
                );

                // Quick Look thumbnail (Stage 8 native preview).
                // `preview::request` was kicked off when the row
                // was selected; this just reads whatever the cache
                // has: Loaded shows the bitmap, Pending shows a
                // muted placeholder, Failed shows nothing.
                // Folders have no file preview: show metadata only
                // (no thumbnail/text box). Files get the media block.
                let is_dir = matches!(entry.kind, EntryKind::Directory);
                let private = crate::private_mode::enabled();
                let thumb_state = if is_dir || private {
                    None
                } else {
                    self.process.preview_cache.borrow().get(&full_path)
                };
                let thumb_img = crate::preview::loaded_image(thumb_state.clone());
                // Text/code files render their content inline instead
                // of a thumbnail (docs/features/PREVIEW.md).
                let text_document = if is_dir || private {
                    None
                } else {
                    let text_state = self.process.text_preview_cache.borrow().get(&full_path);
                    crate::text_preview::loaded_document(text_state)
                };

                let mut col = v_flex().gap_3();
                if is_dir && !private {
                    if let Some(state) = self.process.folder_sidecar_cache.borrow().get(&full_path)
                    {
                        match state {
                            crate::sidecar_preview::FolderSidecarsState::Pending => {
                                col = col.child(
                                    div()
                                        .text_scale_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(tr!("Looking for sidecar files…")),
                                );
                            }
                            crate::sidecar_preview::FolderSidecarsState::Ready {
                                hints,
                                truncated,
                            } if !hints.is_empty() => {
                                let mut card = v_flex()
                                    .w_full()
                                    .gap_1()
                                    .p_2()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().secondary.opacity(0.5))
                                    .child(
                                        div()
                                            .text_scale_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(tr!("Sidecar files")),
                                    );
                                for (index, hint) in hints.into_iter().enumerate() {
                                    let name: SharedString = hint.name.clone().into();
                                    let format: SharedString = hint.format.into();
                                    let path = hint.path.clone();
                                    let action = match hint.kind {
                                        crate::sidecar_preview::SidecarKind::Nfo => {
                                            Button::new(("sidecar-preview", index))
                                                .xsmall()
                                                .label(tr!("Preview"))
                                                .on_click(cx.listener(move |panel, _, _, cx| {
                                                    panel.sidecar_open = Some(path.clone());
                                                    if let Some(shell) = panel.shell.upgrade() {
                                                        let requested = path.clone();
                                                        shell.update(cx, |shell, cx| {
                                                            shell
                                                                .process
                                                                .text_preview_cache
                                                                .borrow_mut()
                                                                .invalidate(&requested);
                                                            crate::text_preview::request(
                                                                shell, requested, cx,
                                                            );
                                                        });
                                                    }
                                                    cx.notify();
                                                }))
                                        }
                                        crate::sidecar_preview::SidecarKind::Manifest => {
                                            Button::new(("sidecar-verify", index))
                                                .xsmall()
                                                .label(tr!("Verify"))
                                                .on_click(cx.listener(move |panel, _, _, cx| {
                                                    if let Some(shell) = panel.shell.upgrade() {
                                                        let manifest = path.clone();
                                                        shell.update(cx, |shell, cx| {
                                                            shell.open_verify_path(manifest, cx);
                                                        });
                                                    }
                                                }))
                                        }
                                    };
                                    card = card.child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(div().flex_1().min_w_0().truncate().child(name))
                                            .child(
                                                div()
                                                    .text_scale_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format),
                                            )
                                            .child(action),
                                    );
                                }
                                if truncated {
                                    card = card.child(
                                        div()
                                            .text_scale_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(tr!("More sidecars may be present.")),
                                    );
                                }
                                col = col.child(card);
                            }
                            _ => {}
                        }
                    }
                    if let Some(path) = self.sidecar_open.as_ref() {
                        if let Some(document) = crate::text_preview::loaded_document(
                            self.process.text_preview_cache.borrow().get(path),
                        ) {
                            let mut preview = div()
                                .id("folder-sidecar-text")
                                .w_full()
                                .max_h(px(280.))
                                .overflow_scroll()
                                .track_scroll(&self.sidecar_scroll)
                                .p_2()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().secondary.opacity(0.5))
                                .text_scale_xs()
                                .whitespace_nowrap();
                            preview = if document.terminal_art {
                                preview
                                    .font_family(terminal_font_family())
                                    .line_height(rems(11.0 / 16.0))
                                    .child(terminal_text(&document))
                            } else {
                                preview
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(document.text.clone())
                            };
                            col = col.child(scrolled_preview_box(preview, &self.sidecar_scroll));
                        }
                    }
                }
                if private && !is_dir {
                    // Same blur the list and grid paint, invented from the
                    // session key and never from the file, so a Private Mode
                    // capture still shows what the preview pane is for
                    // (docs/features/PRIVATE_MODE.md).
                    let stand_in = crate::private_thumb::stand_in(entry.id.as_raw());
                    col = col.child(
                        div()
                            .w_full()
                            .h(px(self.thumb_h))
                            .rounded(cx.theme().radius)
                            .overflow_hidden()
                            .bg(cx.theme().secondary.opacity(0.5))
                            .children(stand_in.map(|image| {
                                gpui::img(image)
                                    .w_full()
                                    .h(px(self.thumb_h))
                                    .object_fit(gpui::ObjectFit::Cover)
                            })),
                    );
                } else if let Some(document) = text_document {
                    // Render through gpui-component's TextView:
                    // markdown files format, source files highlight
                    // (the worker already capped this to 500 lines, and
                    // TextView parses off the UI thread). The id is keyed
                    // per file (see below) so selection state can't bleed
                    // across previews.
                    //
                    // A bounded box with its own scroll on BOTH axes:
                    // vertical so a long file doesn't push the Get Info
                    // details far down the pane, horizontal so no-wrap code
                    // lines stay readable.
                    //
                    // Wheel scroll-chaining: `overflow_scroll`'s own handler
                    // applies the delta to `preview_text_scroll` first; the
                    // `on_scroll_wheel` below then forwards only what spilled
                    // past the box's top/bottom to the outer `preview_scroll`,
                    // so a long file scrolls the box, then reveals Get Info,
                    // not both at once. `track_scroll` is what makes the box's
                    // offset readable for that math.
                    let block = div()
                        .id(("preview-text", entry.id.as_raw() as usize))
                        .w_full()
                        .max_h(px(280.0))
                        .overflow_scroll()
                        .track_scroll(&self.text_scroll)
                        .on_scroll_wheel(cx.listener(Self::on_preview_text_scroll))
                        .p_2()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().secondary.opacity(0.5))
                        .text_scale_xs();
                    let block = if document.text.is_empty() {
                        block
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("(empty file)"))
                    } else if document.terminal_art {
                        block
                            .font_family(terminal_font_family())
                            .line_height(rems(11.0 / 16.0))
                            .whitespace_nowrap()
                            .child(terminal_text(&document))
                    } else {
                        let md =
                            crate::text_preview::to_markdown_source(&entry.name, &document.text);
                        // Compact mono in code blocks, and don't wrap:
                        // long lines scroll horizontally in the block
                        // above instead of folding.
                        let style = gpui_component::text::TextViewStyle::default().code_block(
                            gpui::StyleRefinement::default()
                                .text_size(rems(11.0 / 16.0))
                                .whitespace_nowrap(),
                        );
                        // Per-file element id (keyed on the entry id), not
                        // a constant: a TextView keeps internal selection /
                        // scroll state under its id, so a shared id let a
                        // stale text selection bleed onto the next file you
                        // previewed (it looked "already selected" on hover).
                        // A distinct id per file gives each a clean TextView
                        // at the cost of re-parsing on file switch (cheap:
                        // the worker caps content to 500 lines, off-thread).
                        let view = gpui_component::text::TextView::markdown(
                            ("preview-textview", entry.id.as_raw() as usize),
                            SharedString::from(md),
                        )
                        .style(style)
                        // TextView parses large replacements off the UI
                        // thread; max_lines adds a visual-line bound for a
                        // short source whose prose wraps pathologically.
                        .max_lines(PREVIEW_TEXT_MAX_VISUAL_LINES)
                        .selectable(true);
                        // Neither preview kind scrolls horizontally on its own
                        // in the narrow pane, so we give the content a definite
                        // width wider than the box and let the box's
                        // `overflow_scroll` reach the rest. `w_full` keeps a
                        // short file filling the pane rather than sitting in an
                        // over-wide box.
                        //
                        //  - Rendered markdown (`.md`) wraps its prose to the
                        //    container width (gpui-component forces
                        //    `whitespace_normal` on paragraphs), folding every
                        //    sentence into a sliver. A fixed reading column
                        //    (PREVIEW_MD_MIN_W) reads well and scrolls when the
                        //    pane is narrower.
                        //  - Code blocks are `whitespace_nowrap`; they clip
                        //    long lines but don't grow their container, so the
                        //    box has nothing to scroll toward. Size to the
                        //    widest line (estimated from its column count) so
                        //    the box can scroll the full line into view.
                        let is_markdown = matches!(
                            std::path::Path::new(entry.name.as_ref())
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.to_ascii_lowercase())
                                .as_deref(),
                            Some("md" | "markdown" | "mdx")
                        );
                        let min_w = if is_markdown {
                            PREVIEW_MD_MIN_W
                        } else {
                            let cols = document
                                .text
                                .lines()
                                .map(|line| {
                                    line.chars()
                                        .map(|c| if c == '\t' { PREVIEW_CODE_TAB_COLS } else { 1 })
                                        .sum::<usize>()
                                })
                                .max()
                                .unwrap_or(0);
                            (cols as f32 * PREVIEW_CODE_CHAR_W + PREVIEW_CODE_PAD)
                                .min(PREVIEW_CODE_MAX_W)
                        };
                        block.child(div().w_full().min_w(px(min_w)).child(view))
                    };
                    col = col.child(scrolled_preview_box(block, &self.text_scroll));
                } else if let Some(img) = thumb_img {
                    // Clicking the thumbnail opens the big viewer
                    // window (docs/features/VIEWER.md) on the current
                    // folder, same as Cmd+Y. A maximize glyph in the
                    // top-right corner is the discoverability affordance
                    // (only shown here, where a viewer-capable preview
                    // exists) instead of a text caption.
                    //
                    // Box height is user-adjustable via the resize grip
                    // below; the image fills whatever the box allows
                    // (aspect preserved: gpui's img derives its
                    // aspect_ratio from the bitmap's intrinsic size).
                    col = col.child(
                        div()
                            .id("preview-thumb-open")
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w_full()
                            .h(px(self.thumb_h))
                            .p_2()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .cursor_pointer()
                            .hover(|this| this.bg(cx.theme().secondary.opacity(0.8)))
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(OpenViewer), cx)
                            }))
                            .child(gpui::img(img).max_w_full().max_h_full())
                            .child(
                                div()
                                    .absolute()
                                    .top_2()
                                    .right_2()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(22.0))
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().background.opacity(0.75))
                                    .child(
                                        svg()
                                            .path("icons/maximize.svg")
                                            .w(px(13.0))
                                            .h(px(13.0))
                                            .text_color(cx.theme().foreground),
                                    ),
                            ),
                    );
                    col = col.child(self.preview_thumb_resize_grip(cx));
                } else if matches!(thumb_state, Some(crate::preview::PreviewState::Pending)) {
                    col = col.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w_full()
                            .h(px(self.thumb_h))
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .text_scale_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr!("Loading preview\u{2026}")),
                    );
                    col = col.child(self.preview_thumb_resize_grip(cx));
                }

                // Filename header. A clean name truncates with a full-name
                // tooltip; a name with deceptive characters (homoglyphs,
                // bidi overrides, hidden whitespace) renders each hazard
                // highlighted with its own explanatory tooltip instead.
                let name_header = div()
                    .id(("preview-name", entry.id.as_raw() as usize))
                    .text_scale_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground);
                let shown_name: SharedString = crate::private_mode::present_leaf_str(
                    &entry.display_name,
                    matches!(entry.kind, EntryKind::Directory),
                )
                .into();
                let name_header = if entry.name_has_hazards && !private {
                    name_header.child(crate::entry_info::name_hazard_element(
                        &shown_name,
                        "preview-name",
                    ))
                } else {
                    let name_for_tooltip = shown_name.clone();
                    name_header
                        .truncate()
                        .child(shown_name)
                        .tooltip(move |window, cx| {
                            Tooltip::new(name_for_tooltip.clone()).build(window, cx)
                        })
                };
                col = col.child(name_header);

                // The Get Info panel, embedded: the detail rows the
                // preview used to show, now editable and complete. Cmd+I
                // opens the same content as a standalone popup.
                col = col.child(info_view);

                // Quarantine surface: the red mark line, the
                // provenance the prefetch worker read off the xattr /
                // Zone.Identifier record (source URL, referrer, agent
                // + download time), and the clear action. All cached
                // on the entry; zero I/O at render time.
                if entry.is_quarantined {
                    col = col.child(
                        h_flex()
                            .mt_1()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_scale_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(gpui::rgb(0xFF3B30))
                                    .child(tr!("Quarantined \u{00B7} Mark of the Web")),
                            )
                            .child(
                                Button::new("preview-clear-quarantine")
                                    .label(crate::i18n::tr_static(
                                        ferail_core::commands::CLEAR_QUARANTINE_LABEL,
                                    ))
                                    .xsmall()
                                    .outline()
                                    .flex_shrink_0()
                                    .tooltip(tr!("Remove the mark and its \
                                         downloaded-from record",))
                                    .on_click(cx.listener(|_, _, window, cx| {
                                        window.dispatch_action(Box::new(ClearQuarantine), cx);
                                    })),
                            ),
                    );
                    if let Some(q) = &entry.quarantine {
                        // where_from convention (both platforms): the
                        // first URL is the download source, the second
                        // the referring page. Rendered as plain
                        // `text_xs` label/value rows so the provenance
                        // matches the Get Info rows directly above it.
                        // (The gpui-component `DescriptionList` this
                        // used before hardcodes its label at `text_sm`
                        // and lets the value inherit the ambient size;
                        // its `.small()`/`.xsmall()` knob only changes
                        // gap + padding, not font size, so it always
                        // rendered a notch larger than the rest of the
                        // pane.)
                        let muted = cx.theme().muted_foreground;
                        let prov_row = |label: SharedString, value: AnyElement| {
                            v_flex()
                                .gap_0p5()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_scale_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(muted)
                                        .child(label),
                                )
                                .child(div().min_w_0().text_scale_xs().child(value))
                        };
                        let mut prov = v_flex().mt_1p5().gap_2();
                        let mut has_rows = false;
                        if let Some(src) = q.where_from.first() {
                            prov = prov.child(prov_row(
                                tr!("Source"),
                                truncated_url_value("prov-source", src, entry.id),
                            ));
                            has_rows = true;
                        }
                        if let Some(referrer) = q.where_from.get(1) {
                            prov = prov.child(prov_row(
                                tr!("Referrer"),
                                truncated_url_value("prov-referrer", referrer, entry.id),
                            ));
                            has_rows = true;
                        }
                        if q.agent.is_some() || q.downloaded_iso.is_some() {
                            let via = match (&q.agent, &q.downloaded_iso) {
                                (Some(a), Some(t)) => format!("{a} \u{00B7} {t}"),
                                (Some(a), None) => a.clone(),
                                (None, Some(t)) => t.clone(),
                                (None, None) => unreachable!(),
                            };
                            prov = prov.child(prov_row(
                                tr!("Downloaded via"),
                                div().child(SharedString::from(via)).into_any_element(),
                            ));
                            has_rows = true;
                        }
                        if has_rows {
                            col = col.child(prov);
                        }
                    }
                }

                // Action row: icon-only buttons with tooltips that
                // include the keyboard shortcut. No Get Info button here:
                // the preview pane already shows the full Get Info panel,
                // so the icon would just duplicate what's on screen (Cmd+I
                // still opens the detached Get Info window).
                // `tooltip_with_action` pulls the chord from the
                // keymap automatically so each hover reads "Open ⌘O".
                let actions = h_flex()
                    .mt_2()
                    .gap_1()
                    .child(
                        Button::new("preview-open")
                            .icon(gpui_component::Icon::empty().path("icons/external-link.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action(tr!("Open"), &OpenSelected, Some(SHELL_CONTEXT))
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(OpenSelected), cx);
                            })),
                    )
                    .child(
                        Button::new("preview-reveal")
                            .icon(gpui_component::Icon::empty().path("icons/folder-open.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action(
                                crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                                &RevealInFinder,
                                Some(SHELL_CONTEXT),
                            )
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(RevealInFinder), cx);
                            })),
                    )
                    .child(
                        Button::new("preview-copy-path")
                            .icon(gpui_component::Icon::empty().path("icons/copy.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action(tr!("Copy Path"), &CopyPath, Some(SHELL_CONTEXT))
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(CopyPath), cx);
                            })),
                    );
                col = col.child(actions);

                col.into_any_element()
            }
        };

        // Pinned header; the body scrolls when the window is shorter
        // than the thumbnail + metadata + actions stack, with a
        // gpui-component scrollbar overlaid on the pane's right edge
        // (it only shows while the content actually overflows).
        v_flex()
            .size_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(div().px_4().pt_4().pb_3().child(header))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("preview-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .flex()
                            .flex_col()
                            .px_4()
                            .pb_4()
                            .child(body),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(Scrollbar::vertical(&self.scroll)),
                    ),
            )
    }

    /// Preview-pane body for a volume mount root: the volume's display
    /// name over the embedded Get Info panel (same entity the file
    /// preview reuses, retargeted at the mount root). The gather runs on
    /// the background executor; this only points the view at the path.
    fn preview_volume_body(
        &mut self,
        path: PathBuf,
        name: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use gpui_component::tooltip::Tooltip;

        let info_view = self.sync_info(
            path,
            name.clone(),
            ferail_core::entry_info::InfoTarget::Volume,
            None,
            cx,
        );
        let name_for_tooltip = name.clone();
        v_flex()
            .gap_3()
            .child(
                div()
                    .id("preview-volume-name")
                    .text_scale_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .truncate()
                    .child(SharedString::from(name))
                    .tooltip(move |window, cx| {
                        Tooltip::new(SharedString::from(name_for_tooltip.clone())).build(window, cx)
                    }),
            )
            .child(info_view)
            .into_any_element()
    }
}
