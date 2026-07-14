//! Get Info popup: the cross-crate composition that builds a
//! [`feraille_core::entry_info::EntryInfo`] for one path, plus the modal
//! view that renders it.
//!
//! Composition lives here (not in a domain crate) because it is the one
//! place that legitimately touches every layer at once: POSIX stat
//! (`feraille-fs-native`), AppKit resource values (`feraille-shell-mac`),
//! volume info, magic, and tags. The gather runs on the background executor
//! — never the paint path — and the result is a fully-formatted, neutral
//! record the view paints without any further I/O.
//!
//! The popup is a gpui-component `Dialog` hosting this view, so ESC /
//! overlay-click / focus-trap come for free (same primitive as the About
//! box). Editing is layered on top in a later pass; today the panel reads.

use crate::text::TextScale as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use feraille_core::commands::TagColor;
use feraille_core::entry_info::{
    Attr, EntryInfo, InfoSection, InfoTarget, InfoValue, PermBits, PermMatrix, SizeValue,
};
use feraille_core::name_hazards::{self, HazardKind};
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, Sizable, WindowExt as _, button::Button, checkbox::Checkbox, h_flex,
    notification::Notification, tooltip::Tooltip, v_flex,
};

use crate::file_list::tag_color_rgba;
use crate::shell::Shell;

/// Key-binding context for the standalone Get Info window — Esc dismisses it
/// (bound in `keymap::install_extras`). The embedded-in-preview instance
/// doesn't set this context, so Esc there belongs to the shell.
pub const ENTRY_INFO_CONTEXT: &str = "GetInfo";

actions!(entry_info, [EntryInfoDismiss]);

/// The seven canonical Finder colors, in the order the swatch row shows them.
const TAG_COLORS: [TagColor; 7] = [
    TagColor::Red,
    TagColor::Orange,
    TagColor::Yellow,
    TagColor::Green,
    TagColor::Blue,
    TagColor::Purple,
    TagColor::Gray,
];

/// Number of standalone Get Info windows currently open. Drives the spiral
/// cascade (see [`crate::window_cascade`]) so a fan-out over many files
/// spreads its windows out instead of stacking them on the same centred
/// spot. A [`CascadeGuard`] held by each window keeps this in sync — once
/// they all close, the next one re-centres.
static OPEN_GET_INFO_WINDOWS: AtomicUsize = AtomicUsize::new(0);

/// Claims a cascade slot on construction and releases it on drop (the
/// owning [`EntryInfoView`] drops when its window closes). The held `slot`
/// is the number of windows already open, i.e. this window's spiral index.
struct CascadeGuard {
    slot: usize,
}

impl CascadeGuard {
    fn claim() -> Self {
        let slot = OPEN_GET_INFO_WINDOWS.fetch_add(1, Ordering::Relaxed);
        CascadeGuard { slot }
    }
}

impl Drop for CascadeGuard {
    fn drop(&mut self) {
        OPEN_GET_INFO_WINDOWS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Open a Get Info window for `path`. A standalone, resizable, movable OS
/// window — not tied to the main window — so several can be open at once for
/// different files. Opening more than one (e.g. Get Info over a selection)
/// cascades them along a spiral so they don't stack. `name`/`target` are the
/// caller's best guess (from the selected row) for the loading header; the
/// background gather recomputes them. `shell` lets edits reload the
/// affected directory.
pub fn open(
    path: PathBuf,
    name: String,
    target: InfoTarget,
    known_size: Option<u64>,
    shell: WeakEntity<Shell>,
    cx: &mut App,
) {
    let title: SharedString = format!("Get Info \u{2014} {name}").into();
    // Claim the next spiral slot; the guard rides along in the view and
    // releases the slot when the window closes.
    let cascade = CascadeGuard::claim();
    let window_size = size(px(420.0), px(680.0));
    let opts = WindowOptions {
        window_bounds: Some(crate::window_cascade::cascaded_bounds(
            cascade.slot,
            window_size,
            cx,
        )),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..Default::default()
        }),
        ..Default::default()
    };
    let _ = cx.open_window(opts, move |window, cx| {
        let view = cx.new(|cx| {
            EntryInfoView::new(path, name, target, known_size, shell, Some(cascade), cx)
        });
        cx.new(|cx| Root::new(view, window, cx))
    });
}

/// Classify a path for the size/volume behavior. Touches the filesystem
/// (`is_dir`) — only ever called on the gather worker.
fn classify(path: &Path) -> InfoTarget {
    if path == Path::new("/") || path.parent() == Some(Path::new("/Volumes")) {
        InfoTarget::Volume
    } else if path.is_dir() {
        InfoTarget::Folder
    } else {
        InfoTarget::File
    }
}

/// Display name: volume's localized name when it's a volume, else the file
/// name, else the path itself.
fn display_name(path: &Path, target: InfoTarget, vol_name: Option<&str>) -> String {
    if target == InfoTarget::Volume {
        if let Some(n) = vol_name {
            return n.to_string();
        }
    }
    path.file_name()
        .and_then(|s| s.to_str())
        // Same Finder-parity leaf swap the file list uses (macOS `:` → `/`),
        // so Get Info's title and its deceptive-name analysis see the name the
        // user actually reads.
        .map(|n| feraille_fs_native::paths::display_leaf(n).into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Build the full Get Info record. Runs on the background executor: every
/// call here is a native read, none of it is allowed on the paint path.
/// `known_size` is the caller's already-computed recursive size for a
/// folder/volume (from the file list's Size column) — reused so we don't
/// rescan, shown with a refresh affordance.
pub fn gather(path: &Path, known_size: Option<u64>) -> EntryInfo {
    use feraille_fs_native as fsn;
    // Routes through the per-OS shell alias so Get Info builds on every
    // platform; on macOS this *is* `feraille_shell_mac`, so behaviour is
    // unchanged. read_shell_info / read_canonical_tags / open_with_candidates
    // are real on macOS and graceful no-ops on win32/linux.
    use crate::platform_shell as shell;

    let target = classify(path);
    let stat = fsn::stat_info::read_stat_info(path);
    let sh = shell::read_shell_info(path);
    let vol = fsn::volume_info_for_path(path);
    let colors = shell::read_canonical_tags(path);
    let default_app = shell::open_with_candidates(path)
        .into_iter()
        .find(|c| c.is_default)
        .map(|c| c.name);

    let name = display_name(path, target, vol.as_ref().map(|v| v.name.as_str()));
    let kind = sh
        .kind
        .clone()
        .or_else(|| fsn::detect_magic(path).map(str::to_string))
        .unwrap_or_else(|| match target {
            InfoTarget::Volume => "Volume".into(),
            InfoTarget::Folder => "Folder".into(),
            InfoTarget::File => "Document".into(),
        });

    let fmt_date = fsn::stat_info::format_local_datetime;

    // ---- General ----
    let mut general = InfoSection::new("General").text_if("Kind", kind.clone());
    if let Some(uti) = sh.uti.clone() {
        general = general.text_if("Type", uti);
    }
    match target {
        InfoTarget::File => {
            let bytes = stat.as_ref().map(|s| s.size).unwrap_or(0);
            general = general.row(
                "Size",
                InfoValue::Size(SizeValue::Known {
                    bytes,
                    display: fsn::humanize_bytes(bytes),
                    refreshable: false,
                }),
            );
        }
        InfoTarget::Folder => {
            // Reuse the file list's recursive size when it already has one;
            // otherwise offer to compute it on demand.
            let size = match known_size {
                Some(b) if b > 0 => SizeValue::Known {
                    bytes: b,
                    display: fsn::humanize_bytes(b),
                    refreshable: true,
                },
                _ => SizeValue::Calculable,
            };
            general = general.row("Size", InfoValue::Size(size));
        }
        InfoTarget::Volume => {}
    }
    if let Some(s) = &stat {
        if let Some(c) = s.created_unix {
            general = general.text_if("Created", fmt_date(c));
        }
        general = general.text_if("Modified", fmt_date(s.modified_unix));
        if let Some(a) = s.accessed_unix {
            general = general.text_if("Last opened", fmt_date(a));
        }
    }
    if let Some(added) = sh.added_unix {
        general = general.text_if("Added", fmt_date(added));
    }
    if let Some(app) = default_app {
        general = general.text_if("Application", app);
    }
    general = general.text_if("Where", feraille_fs_native::paths::display_path(path));

    // ---- Attributes ----
    let mut attributes = InfoSection::new("Attributes");
    if let Some(s) = &stat {
        attributes = attributes
            .row(
                Attr::Locked.label(),
                InfoValue::Toggle {
                    on: s.is_locked,
                    attr: Attr::Locked,
                },
            )
            .row(
                Attr::Invisible.label(),
                InfoValue::Toggle {
                    on: s.is_invisible,
                    attr: Attr::Invisible,
                },
            );
    }
    if let Some(he) = sh.hidden_extension {
        attributes = attributes.row(
            Attr::HiddenExtension.label(),
            InfoValue::Toggle {
                on: he,
                attr: Attr::HiddenExtension,
            },
        );
    }
    attributes = attributes.row(
        "Tags",
        InfoValue::Tags {
            colors,
            custom: Vec::new(),
        },
    );

    // ---- Ownership & Permissions ----
    let mut permissions = InfoSection::new("Ownership & Permissions");
    if let Some(s) = &stat {
        let mode = s.mode & 0o7777;
        permissions = permissions
            .text_if("Owner", s.owner_name.clone())
            .text_if("Group", s.group_name.clone())
            .row(
                "Permissions",
                InfoValue::Permissions(PermMatrix {
                    owner_name: s.owner_name.clone(),
                    group_name: s.group_name.clone(),
                    owner: PermBits::from_triple((mode >> 6) & 0b111),
                    group: PermBits::from_triple((mode >> 3) & 0b111),
                    other: PermBits::from_triple(mode & 0b111),
                    kind_char: s.kind_char(),
                    raw_mode: mode,
                }),
            );
    }

    // ---- Volume ----
    let mut volume = InfoSection::new("Volume");
    if let Some(v) = &vol {
        volume = volume.text_if("Volume", v.name.clone());
        if let Some(t) = v.total_bytes {
            volume = volume.text_if("Capacity", fsn::humanize_bytes(t));
        }
        if let Some(a) = v.available_bytes {
            volume = volume.text_if("Available", fsn::humanize_bytes(a));
        }
        if let Some(f) = v.format.clone() {
            volume = volume.text_if("Format", f);
        }
        volume = volume.text_if("Mount point", v.path.display().to_string());
        if let Some(d) = v.bsd_device.clone() {
            volume = volume.text_if("Device", d);
        }
    }

    let sections = [general, attributes, permissions, volume]
        .into_iter()
        .filter(|s| !s.rows.is_empty())
        .collect();

    EntryInfo {
        name,
        kind,
        target,
        sections,
    }
}

enum GatherState {
    Loading,
    Ready(EntryInfo),
}

/// The modal's content view. Owns the target path and the gathered record;
/// gathers on construction and re-renders when it lands. Edits write
/// through the native crates, then re-gather so the panel shows truth.
pub struct EntryInfoView {
    path: PathBuf,
    name: String,
    kind: String,
    state: GatherState,
    /// Cancel flag for an in-flight recursive "Calculate".
    size_cancel: Option<Arc<AtomicBool>>,
    /// Reloads the affected directory and hosts notifications after edits.
    shell: WeakEntity<Shell>,
    /// Embedded in the preview pane (no name header, no own scroll) vs.
    /// standalone in the popup (header + scrollable body).
    embedded: bool,
    /// Scroll position for the popup's body.
    scroll: ScrollHandle,
    /// The file list's already-computed recursive size for a folder, reused
    /// so Get Info doesn't rescan (shown with a refresh affordance).
    known_size: Option<u64>,
    /// Focus target for the standalone window so Esc (and key dispatch) has a
    /// home. Unused in embedded mode.
    focus_handle: FocusHandle,
    /// One-shot guard: grab focus on the window's first paint so Esc works
    /// immediately, before any control is clicked.
    did_focus: bool,
    /// Holds this window's spiral-cascade slot for its lifetime; dropping it
    /// (on window close) frees the slot. `None` for the embedded preview,
    /// which isn't a standalone window. Kept only for its `Drop`.
    _cascade: Option<CascadeGuard>,
}

impl EntryInfoView {
    fn new(
        path: PathBuf,
        name: String,
        target: InfoTarget,
        known_size: Option<u64>,
        shell: WeakEntity<Shell>,
        cascade: Option<CascadeGuard>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(path, name, target, known_size, shell, false, cascade, cx)
    }

    /// Construct for embedding in the preview pane: section rows only, no
    /// name header (the preview already shows the name) and no own scroll
    /// (the preview pane scrolls).
    pub(crate) fn new_embedded(
        path: PathBuf,
        name: String,
        target: InfoTarget,
        known_size: Option<u64>,
        shell: WeakEntity<Shell>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(path, name, target, known_size, shell, true, None, cx)
    }

    // The info-row builder genuinely needs each of these inputs.
    #[allow(clippy::too_many_arguments)]
    fn build(
        path: PathBuf,
        name: String,
        _target: InfoTarget,
        known_size: Option<u64>,
        shell: WeakEntity<Shell>,
        embedded: bool,
        cascade: Option<CascadeGuard>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            path,
            name,
            kind: String::new(),
            state: GatherState::Loading,
            size_cancel: None,
            shell,
            embedded,
            scroll: ScrollHandle::new(),
            known_size,
            focus_handle: cx.focus_handle(),
            did_focus: false,
            _cascade: cascade,
        };
        this.refresh(cx);
        this
    }

    /// Re-point an embedded view at a new selection without rebuilding the
    /// entity. Cancels any in-flight size scan and re-gathers.
    pub(crate) fn retarget(
        &mut self,
        path: PathBuf,
        name: String,
        known_size: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.path == path {
            // Same entry — but a folder size may have just landed from the
            // async worker after we first showed "Calculate". Upgrade in
            // place rather than rescanning.
            if self.known_size != known_size {
                self.known_size = known_size;
                if let (Some(b), GatherState::Ready(info)) = (known_size, &mut self.state) {
                    if b > 0 && info.size_is_calculable() {
                        info.set_size_value(SizeValue::Known {
                            bytes: b,
                            display: feraille_fs_native::humanize_bytes(b),
                            refreshable: true,
                        });
                        cx.notify();
                    }
                }
            }
            return;
        }
        if let Some(c) = self.size_cancel.take() {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.path = path;
        self.name = name;
        self.kind = String::new();
        self.known_size = known_size;
        self.state = GatherState::Loading;
        self.refresh(cx);
    }

    /// (Re-)gather the record on the background executor and apply it.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let gather_path = self.path.clone();
        let apply_path = gather_path.clone();
        let known_size = self.known_size;
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async move { gather(&gather_path, known_size) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Staleness guard: the panel may have been retargeted
                // (preview-pane embedded mode) while a slow gather —
                // e.g. a network mount — was in flight. Applying it
                // would show file A's size/permissions under file B.
                if this.path != apply_path {
                    return;
                }
                this.name = info.name.clone();
                this.kind = info.kind.clone();
                this.state = GatherState::Ready(info);
                cx.notify();
            });
        })
        .detach();
    }

    /// Apply an edit result: surface failures as a toast, and on success
    /// reload the file list for the affected directory and re-gather so the
    /// panel reflects the new state. Success is silent — the refreshed rows
    /// are the feedback.
    fn after_write(
        &mut self,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(()) => {
                if let (Some(parent), Some(shell)) = (self.path.parent(), self.shell.upgrade()) {
                    let dir = parent.to_path_buf();
                    shell.update(cx, |s, cx| s.reload_tabs_matching_paths(&[dir], cx));
                }
                self.refresh(cx);
            }
            Err(e) => window.push_notification(Notification::error(e), cx),
        }
    }

    /// Kick a recursive size scan for a folder/volume's "Calculate" button.
    /// Read-only work (no mutation) — streams the total back into the open
    /// record when it finishes.
    fn calculate_size(&mut self, cx: &mut Context<Self>) {
        if let GatherState::Ready(info) = &mut self.state {
            info.set_size_value(SizeValue::Calculating);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.size_cancel = Some(cancel.clone());
        let path = self.path.clone();
        let apply_path = path.clone();
        let apply_cancel = cancel.clone();
        // Share the file list's folder-size cache so a folder already
        // sized in the Size column answers instantly, and a value
        // computed here feeds that column too (one source of truth,
        // one invalidation path — see docs/features/FRESHNESS.md).
        let db = crate::process_state::process_state(cx).db_snapshot();
        cx.spawn(async move |this, cx| {
            let bytes = cx
                .background_executor()
                .spawn(async move {
                    crate::folder_sizes::folder_size_cached(&path, db.as_ref(), &cancel)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Drop stale/cancelled results: a cancelled walk
                // returns an invalid partial sum, and a retarget swaps
                // both the path and the cancel handle — presenting
                // either as a confident size would be wrong (and
                // would clobber the newer scan's cancel handle).
                if this.path != apply_path
                    || apply_cancel.load(std::sync::atomic::Ordering::Relaxed)
                    || !this
                        .size_cancel
                        .as_ref()
                        .is_some_and(|c| Arc::ptr_eq(c, &apply_cancel))
                {
                    return;
                }
                this.known_size = Some(bytes);
                if let GatherState::Ready(info) = &mut this.state {
                    info.set_size_value(SizeValue::Known {
                        bytes,
                        display: feraille_fs_native::humanize_bytes(bytes),
                        refreshable: true,
                    });
                }
                this.size_cancel = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Run a native write on the background executor, then route its
    /// result through [`Self::after_write`] with the window available
    /// for the failure toast. `chflags`/`chmod`/xattr writes are
    /// filesystem I/O — inline in a click listener they'd block the UI
    /// for the full mount timeout on a dead network volume or a
    /// dataless placeholder (Prime Directive).
    fn spawn_write(
        &mut self,
        write: impl FnOnce() -> Result<(), String> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { write() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.after_write(result, window, cx);
            });
        })
        .detach();
    }

    /// Flip a boolean attribute (Locked / Invisible / Hide extension) via
    /// the matching native writer, then refresh.
    fn apply_toggle(&mut self, attr: Attr, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        use feraille_fs_native::stat_info;
        let path = self.path.clone();
        self.spawn_write(
            move || match attr {
                Attr::Locked => stat_info::set_locked(&path, on),
                Attr::Invisible => stat_info::set_invisible(&path, on),
                Attr::HiddenExtension => crate::platform_shell::set_hidden_extension(&path, on),
                Attr::Stationery => Err("Stationery editing is not supported yet".into()),
            },
            window,
            cx,
        );
    }

    /// Add or remove a Finder color label, preserving other tags.
    fn toggle_color(&mut self, color: TagColor, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.spawn_write(
            move || crate::platform_shell::toggle_tag(&path, color),
            window,
            cx,
        );
    }

    /// Rewrite the permission mode after a single rwx box flipped. Unix only —
    /// Windows shows a read-only summary instead of the editable rwx grid.
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    fn apply_permissions(&mut self, mode: u32, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        self.spawn_write(
            move || feraille_fs_native::stat_info::set_permissions(&path, mode),
            window,
            cx,
        );
    }
}

impl EntryInfoView {
    /// Esc — close the standalone Get Info window.
    fn on_dismiss(&mut self, _: &EntryInfoDismiss, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }
}

impl Focusable for EntryInfoView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for EntryInfoView {
    fn drop(&mut self) {
        // If a recursive size scan is still running when the popup closes,
        // tell it to stop — its result has nowhere to land.
        if let Some(c) = &self.size_cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Render for EntryInfoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let sections = match &self.state {
            GatherState::Loading => v_flex().child(
                div()
                    .text_scale_sm()
                    .text_color(muted)
                    .child("Gathering details\u{2026}"),
            ),
            GatherState::Ready(info) => {
                let mut col = v_flex().gap_3();
                let mut row_ix = 0usize;
                for section in &info.sections {
                    col = col.child(self.render_section(section, &mut row_ix, muted, cx));
                }
                col
            }
        };

        if self.embedded {
            // Preview pane provides the name header and the scroll; just
            // emit the section rows.
            return v_flex().gap_3().child(sections).into_any_element();
        }

        // Standalone window: a fixed name/kind header above a body that
        // fills the window and scrolls, so the record stays usable at any
        // window size (resizable + movable + multiple instances).
        //
        // Grab focus on first paint so Esc (bound to EntryInfoDismiss in this
        // window's key context) dismisses the window right away.
        if !self.did_focus {
            self.did_focus = true;
            window.focus(&self.focus_handle, cx);
        }
        let mut header = v_flex().gap_0p5().child(
            div()
                .text_scale_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(name_hazard_element(&self.name, "popup-name")),
        );
        if let Some(warn) = name_hazard_warning(&self.name) {
            header = header.child(
                div()
                    .text_scale_xs()
                    .text_color(gpui::rgb(0xC2410C))
                    .child(format!("\u{26A0} {warn}")),
            );
        }
        let header = header.child(div().text_scale_xs().text_color(muted).child(self.kind.clone()));

        v_flex()
            .key_context(ENTRY_INFO_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_dismiss))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().flex_none().px_4().pt_4().pb_2().child(header))
            .child(
                div()
                    .id("entry-info-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_4()
                    .pb_4()
                    .child(sections),
            )
            // This window's own Root holds the notification state but doesn't
            // render the layer — do it here so edit-error toasts appear.
            .children(Root::render_notification_layer(window, cx))
            .into_any_element()
    }
}

impl EntryInfoView {
    fn render_section(
        &self,
        section: &InfoSection,
        row_ix: &mut usize,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut block = v_flex().gap_1().child(
            div()
                .text_scale_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(section.title.clone()),
        );
        for row in &section.rows {
            let ix = *row_ix;
            *row_ix += 1;
            block = block.child(self.render_row(&row.label, &row.value, ix, muted, cx));
        }
        block
    }

    fn render_row(
        &self,
        label: &str,
        value: &InfoValue,
        ix: usize,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .gap_2()
            .items_start()
            .child(
                div()
                    .w(px(96.0))
                    .flex_none()
                    .text_scale_xs()
                    .text_color(muted)
                    .text_right()
                    .child(label.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .text_scale_xs()
                    .child(self.render_value(value, ix, cx)),
            )
    }

    fn render_value(&self, value: &InfoValue, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match value {
            InfoValue::Text(s) | InfoValue::Name(s) => div().child(s.clone()).into_any_element(),
            InfoValue::Toggle { on, attr } => {
                let attr = *attr;
                Checkbox::new(ElementId::Name(format!("entry-info-tog-{ix}").into()))
                    .xsmall()
                    .checked(*on)
                    .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                        this.apply_toggle(attr, *checked, window, cx);
                    }))
                    .into_any_element()
            }
            InfoValue::Tags { colors, custom } => {
                // All seven canonical swatches; the active ones are ringed.
                // Click toggles that label on the file.
                let active: std::collections::HashSet<TagColor> = colors.iter().copied().collect();
                let mut row = h_flex().gap_1p5().items_center().flex_wrap();
                for c in TAG_COLORS {
                    let rgba = tag_color_rgba(c);
                    let is_on = active.contains(&c);
                    row = row.child(
                        div()
                            .id(ElementId::Name(format!("entry-info-tag-{c:?}").into()))
                            .w(px(15.0))
                            .h(px(15.0))
                            .rounded_full()
                            .bg(rgba)
                            .border_2()
                            .border_color(if is_on {
                                cx.theme().foreground
                            } else {
                                cx.theme().border
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_color(c, window, cx);
                            })),
                    );
                }
                for name in custom {
                    row = row.child(div().child(name.clone()));
                }
                row.into_any_element()
            }
            InfoValue::Permissions(m) => self.render_permissions(m, cx),
            InfoValue::Size(size) => match size {
                SizeValue::Known {
                    display,
                    refreshable,
                    ..
                } => {
                    let row = h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().child(display.clone()));
                    if *refreshable {
                        // A cached folder/volume total — let the user recompute.
                        row.child(
                            Button::new("entry-info-recalc-size")
                                .label("\u{21BB}")
                                .xsmall()
                                .tooltip("Recalculate size")
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.calculate_size(cx)),
                                ),
                        )
                        .into_any_element()
                    } else {
                        row.into_any_element()
                    }
                }
                SizeValue::Calculating => div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Calculating\u{2026}")
                    .into_any_element(),
                SizeValue::Calculable => Button::new("entry-info-calc-size")
                    .label("Calculate")
                    .xsmall()
                    .on_click(cx.listener(|this, _, _window, cx| this.calculate_size(cx)))
                    .into_any_element(),
            },
        }
    }

    /// Editable 3×3 read/write/execute grid (owner / group / other), plus
    /// the octal readout. Each box rewrites the whole mode via `chmod`.
    fn render_permissions(&self, m: &PermMatrix, cx: &mut Context<Self>) -> AnyElement {
        // Windows files are governed by NTFS ACLs, not Unix owner/group/other
        // rwx bits, so the synthesized 3×3 grid + octal would only mislead.
        // Surface the one concept that maps cleanly — writable vs read-only —
        // and leave the editable read-only/hidden toggles to the Attributes
        // section above.
        #[cfg(target_os = "windows")]
        {
            let label = if m.owner.write { "Read & write" } else { "Read-only" };
            return div()
                .text_scale_sm()
                .text_color(cx.theme().foreground)
                .child(SharedString::from(label))
                .into_any_element();
        }

        #[cfg(not(target_os = "windows"))]
        {
        let classes: [(&str, PermBits); 3] =
            [("Owner", m.owner), ("Group", m.group), ("Other", m.other)];
        let mut grid = v_flex().gap_0p5();
        for (ci, (label, bits)) in classes.into_iter().enumerate() {
            let mut row = h_flex().gap_2().items_center().child(
                div()
                    .w(px(44.0))
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            );
            // bit 2 = read, 1 = write, 0 = execute.
            for (label, bit, on) in [
                ("r", 2u32, bits.read),
                ("w", 1, bits.write),
                ("x", 0, bits.execute),
            ] {
                let base = m.clone();
                row = row.child(
                    Checkbox::new(ElementId::Name(format!("perm-{ci}-{bit}").into()))
                        // Match the dense token-xs labels around it; the
                        // component default (Medium) renders the r/w/x
                        // letters oversized.
                        .xsmall()
                        .label(label)
                        .checked(on)
                        .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                            let mut next = base.clone();
                            let triple = match ci {
                                0 => &mut next.owner,
                                1 => &mut next.group,
                                _ => &mut next.other,
                            };
                            match bit {
                                2 => triple.read = *checked,
                                1 => triple.write = *checked,
                                _ => triple.execute = *checked,
                            }
                            this.apply_permissions(next.to_mode(), window, cx);
                        })),
                );
            }
            grid = grid.child(row);
        }
        grid.child(
            div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(m.symbolic()),
        )
        .into_any_element()
        }
    }
}

/// Render a filename with deceptive characters highlighted: leading/trailing
/// or unusual whitespace, zero-width / control / bidi characters, and
/// homoglyphs. Invisible characters are shown via a visible stand-in; each
/// flagged span carries a tooltip naming the hazard. `id_prefix` keeps the
/// per-span element ids unique when the name renders in more than one place.
pub(crate) fn name_hazard_element(name: &str, id_prefix: impl Into<SharedString>) -> AnyElement {
    let id_prefix = id_prefix.into();
    let segments = name_hazards::analyze(name);
    if segments.iter().all(|s| s.hazard.is_none()) {
        return div().child(name.to_string()).into_any_element();
    }
    let mut row = h_flex().flex_wrap().items_center();
    for (i, seg) in segments.into_iter().enumerate() {
        match seg.hazard {
            None => row = row.child(div().child(seg.text)),
            Some(kind) => {
                // Whitespace tricks are amber; reordering / invisible /
                // look-alike characters are the dangerous red.
                let amber = matches!(
                    kind,
                    HazardKind::LeadingSpace
                        | HazardKind::TrailingSpace
                        | HazardKind::UnusualWhitespace
                );
                let bg = if amber { rgb(0xF59E0B) } else { rgb(0xDC2626) };
                let shown = seg.render.unwrap_or(seg.text);
                let label: SharedString = seg
                    .label
                    .unwrap_or_else(|| kind.summary().to_string())
                    .into();
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("{id_prefix}-haz-{i}").into()))
                        .px_0p5()
                        .rounded_sm()
                        .bg(bg)
                        .text_color(gpui::white())
                        .child(shown)
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx)),
                );
            }
        }
    }
    row.into_any_element()
}

/// A one-line summary of the hazard kinds present in `name`, or `None` if the
/// name is clean. Shown under the name as a warning.
pub(crate) fn name_hazard_warning(name: &str) -> Option<String> {
    let mut kinds: Vec<&'static str> = name_hazards::analyze(name)
        .iter()
        .filter_map(|s| s.hazard.map(|k| k.summary()))
        .collect();
    if kinds.is_empty() {
        return None;
    }
    kinds.dedup();
    kinds.sort_unstable();
    kinds.dedup();
    Some(format!("Deceptive name: {}", kinds.join(", ")))
}
