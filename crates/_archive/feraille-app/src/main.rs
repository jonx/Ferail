//! Feraille — iter-2 final integration.
//!
//! Layout:
//!   header (40)
//!   tabstrip (32)
//!   [filetree (200, splitter-resizable) | breadcrumb (32) / list+scrollbar]
//!   status (24)
//!
//! Per-tab state: current_dir, entries, selection, list_scroll. Shared:
//! one of each control. FileTree replaces the iter-1 Sidebar.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use feraille_controls::primitives::{
    draw::{fill_rounded_rect, paint_card, stroke_rounded_rect, text_y_center},
    panel::ModalPanel,
    progress_strip::{ProgressStrip, ProgressTaskId},
    scrollbar::Scrollbar,
    settings_widgets::{
        compute_settings_row, paint_preview_tile, paint_segmented, paint_settings_row_text,
        paint_sidebar_nav_item, paint_toggle, segmented_hit, toggle_hit, PreviewKind, RowLayout,
        ROW_H_DESCRIBED,
    },
    splitter::Splitter,
    text_input::{TextInput, TextInputEvent, TextInputKey},
    toast::{Toast, ToastKind, ToastStack},
};
use feraille_controls::{
    sort_entries, BreadcrumbBar, BreadcrumbEvent, FileTree, Section, SectionKind, Selection,
    SelectionSet, TabInfo, TabStrip, TabStripEvent, TreeEvent, VirtualizedList,
};
use feraille_core::commands::CommandId;
use feraille_core::{
    AntTrail, EntryKind, EnumerationError, FileEntry, FsBackend, NodeId, QuarantineDetails,
};
use feraille_design::{FontWeight, Theme, Tokens};
use feraille_fs_native::{
    detect_magic, fetch_icon_rgba, fetch_quarantine_info, home_dir, list_volumes, move_to_trash,
    open_with_default, quarantine_details_from, volume_info_for_path, NativeFs,
    DEFAULT_ENUMERATION_BATCH,
};

mod app_prefs;
mod disk_usage_prefs;
mod disk_usage_state;
mod disk_usage_window;
mod obs;
mod screenshot;
mod task_panel;
mod tasks;

use crate::disk_usage_state::DiskUsageState;
use crate::disk_usage_window::DiskUsageWindow;
use crate::tasks::{TaskId, TaskKind, TaskRegistry};

/// Iteration-tagged logging. The first argument is an ID number; lines
/// with `id < obs::LOG_THRESHOLD` are silently dropped. Bump `LOG_THRESHOLD`
/// each iteration to suppress stale diagnostic noise without deleting code.
/// Crash diagnostics (panic hook, startup banner, worker-panic line) bypass
/// these macros and are always printed.
macro_rules! log_info {
    ($id:expr, $($arg:tt)*) => {
        if $id >= $crate::obs::LOG_THRESHOLD {
            $crate::obs::line("info", format_args!($($arg)*))
        }
    };
}
macro_rules! log_warn {
    ($id:expr, $($arg:tt)*) => {
        if $id >= $crate::obs::LOG_THRESHOLD {
            $crate::obs::line("warn", format_args!($($arg)*))
        }
    };
}
macro_rules! log_error {
    ($id:expr, $($arg:tt)*) => {
        if $id >= $crate::obs::LOG_THRESHOLD {
            $crate::obs::line("error", format_args!($($arg)*))
        }
    };
}

use feraille_render::{Bitmap, Point as FPoint, Rect as FRect, Renderer, SoftRenderer, TextStyle};
use std::collections::HashMap;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const DRAG_THRESHOLD_DIPS: f32 = 4.0;
const DRAG_DELAY_MS: u128 = 100;
/// Icons fetched per `IconChunkTick`. Each call to NSWorkspace.iconForFile:
/// is ~1ms on a warm Launch Services cache; 4 keeps a single tick under
/// the 4ms paint frame budget from specs/ux/05-performance.md.
const ICON_CHUNK_SIZE: usize = 4;

/// Which pane currently owns keyboard focus. F6 cycles between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusedPane {
    Tree,
    List,
}

#[derive(Clone, Copy, Debug)]
struct DragWatch {
    start: FPoint,
    when: std::time::Instant,
    row: usize,
}

#[derive(Clone, Debug)]
enum AppEvent {
    MagicBatch {
        generation: u64,
        dir: PathBuf,
        results: Vec<MagicResult>,
    },
    /// Drain a chunk of `App.icon_queue` on the main thread. Posted to the
    /// event loop so input/paint can run between chunks; NSWorkspace is
    /// main-thread-only so we cannot offload the fetch to a worker.
    IconChunkTick { generation: u64 },
    /// User invoked a Feraille-owned command (menu / future command
    /// palette / future remappable shortcut). Dispatched to the
    /// matching App method by id in `user_event`.
    Command(CommandId),
    /// Streaming-enumeration batch: append `entries` to the active
    /// tab if `generation` and `dir` still match the in-flight
    /// listing. Stale batches are dropped at the gate in `user_event`.
    EnumerationBatch {
        generation: u64,
        dir: PathBuf,
        entries: Vec<FileEntry>,
    },
    /// Final marker for a streamed enumeration. Always sent before
    /// the worker exits (even on cancellation, where `error` is
    /// `None`). On hard failure mid-stream, `error` carries the
    /// reason and any rows already delivered remain visible.
    EnumerationDone {
        generation: u64,
        dir: PathBuf,
        error: Option<EnumerationError>,
    },
    /// Tree-pane children for `id` arrived from a worker. Gated on
    /// `App::tree_pending` matching `generation` so a stale result
    /// (superseded by `invalidate_tree` or a re-spawn) is dropped.
    TreeChildrenLoaded {
        generation: u64,
        id: NodeId,
        entries: Vec<FileEntry>,
        error: Option<EnumerationError>,
    },
    /// macOS Appearance flipped. `dark` is the new state. Posted by
    /// the `feraille_shell_mac::start_system_theme_observer` callback;
    /// the user_event arm calls `apply_theme` only when the user's
    /// preference is `System`.
    SystemThemeChanged {
        dark: bool,
    },
    /// macOS quarantine + where-from xattrs read off-thread for files
    /// in `dir`. Stale batches dropped at the gate via `generation`.
    QuarantineBatch {
        generation: u64,
        dir: PathBuf,
        results: Vec<QuarantineResult>,
    },
    /// Inline preview thumbnail finished decoding. Stale ones (e.g.
    /// the user moved the cursor before qlmanage returned) are
    /// dropped at the gate via `generation`.
    PreviewThumbReady {
        generation: u64,
        path: PathBuf,
        mtime_unix: i64,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// `qlmanage -t` failed (non-zero exit, no PNG written, decode
    /// error, …). Recorded in `preview_failed` so we don't retry on
    /// every paint, and so the placeholder text flips from
    /// "Generating preview…" to "No preview" once we know.
    PreviewThumbFailed {
        generation: u64,
        path: PathBuf,
        mtime_unix: i64,
        size_px: u32,
    },
    /// Disk-usage scan posted a batch of facts. Gated on `generation`
    /// matching the DU window's current scan; stale batches dropped.
    DiskUsageBatch {
        generation: u64,
        facts: Vec<feraille_disk_usage::DiskUsageFact>,
    },
    /// Periodic progress tick from the DU worker (~250 ms cadence).
    DiskUsageProgress {
        generation: u64,
        stats: feraille_disk_usage::DiskUsageStats,
    },
    /// Final marker for a DU scan. `error` is `Some` only on hard
    /// failure of the top-level `read_dir`; cancellation arrives with
    /// `error: None`.
    DiskUsageDone {
        generation: u64,
        error: Option<EnumerationError>,
    },
    /// Worker-side file operation (Duplicate, Compress) finished.
    /// Posted from the worker; the UI ends the task, refreshes the
    /// active tab when `dest_dir` matches it, and toasts on failure.
    FileOpComplete {
        op: FileOpKind,
        task_id: TaskId,
        dest_dir: PathBuf,
        result: Result<PathBuf, String>,
    },
}

/// Kind of background file operation we report back via
/// [`AppEvent::FileOpComplete`]. Drives the user-facing toast
/// wording and nothing else.
#[derive(Clone, Copy, Debug)]
enum FileOpKind {
    Duplicate,
    Compress,
}

#[derive(Clone, Debug)]
struct MagicResult {
    name: String,
    mtime_unix: i64,
    label: String,
}

#[derive(Clone, Debug)]
struct QuarantineResult {
    name: String,
    mtime_unix: i64,
    quarantined: bool,
    details: QuarantineDetails,
}

/// State for the Keyboard-Shortcuts overlay. Renders a capped-height
/// modal with a live filter at the top and a scrollable body grouped
/// by command category. The catalogue is the source of truth — adding
/// a `CommandSpec` with shortcuts surfaces here automatically.
pub struct ShortcutsModal {
    /// Live filter input. Matches case-insensitively against the
    /// command title and against each shortcut's rendered glyphs
    /// ("⌘=", "⌘⇧[" etc.).
    pub filter: TextInput,
    /// Vertical scroll offset in DIPs from the top of the content.
    /// Clamped to `[0, max_scroll]` at paint time so resize / filter
    /// changes don't leave it pointing past the end.
    pub scroll_offset: f32,
}

impl ShortcutsModal {
    pub fn new() -> Self {
        Self { filter: TextInput::new(""), scroll_offset: 0.0 }
    }
}

/// Top-level pages in the Settings panel. Each page is its own
/// content layout; the sidebar nav switches between them. Adding a
/// page is: add a variant, give it an entry in
/// `SettingsCategory::ALL`, and add the page layout + paint arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsCategory {
    Appearance,
    Files,
    Layout,
    About,
}

impl SettingsCategory {
    pub const ALL: &'static [SettingsCategory] = &[
        SettingsCategory::Appearance,
        SettingsCategory::Files,
        SettingsCategory::Layout,
        SettingsCategory::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SettingsCategory::Appearance => "Appearance",
            SettingsCategory::Files => "Files",
            SettingsCategory::Layout => "Layout",
            SettingsCategory::About => "About",
        }
    }

    /// Sidebar nav glyph. Empty for now — the bundled font has no
    /// SF Symbols coverage and the partial coverage we tried was
    /// inconsistent (some rendered, some didn't). When the AppKit
    /// SF Symbols renderer lands, paint real icons here.
    pub fn glyph(self) -> &'static str {
        ""
    }
}

/// Snap stops for sidebar width on the Layout page. Aligns with macOS
/// convention of presenting size choices as Narrow/Medium/Wide
/// rather than exposing raw pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarWidthSnap {
    Narrow,
    Medium,
    Wide,
}

impl SidebarWidthSnap {
    pub const ALL: &'static [SidebarWidthSnap] = &[
        SidebarWidthSnap::Narrow,
        SidebarWidthSnap::Medium,
        SidebarWidthSnap::Wide,
    ];

    pub fn px(self) -> f32 {
        match self {
            SidebarWidthSnap::Narrow => 180.0,
            SidebarWidthSnap::Medium => 240.0,
            SidebarWidthSnap::Wide => 360.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SidebarWidthSnap::Narrow => "Narrow",
            SidebarWidthSnap::Medium => "Medium",
            SidebarWidthSnap::Wide => "Wide",
        }
    }

    /// The snap stop closest to `current_px`. None of the rounding
    /// produces a "Custom" state; even if the user dragged the
    /// splitter to an arbitrary value we surface the closest snap
    /// for visual selection. The caller decides whether to show a
    /// "currently N px" subscript when the match isn't exact.
    pub fn nearest(current_px: f32) -> Self {
        let mut best = SidebarWidthSnap::Medium;
        let mut best_d = f32::INFINITY;
        for s in Self::ALL {
            let d = (s.px() - current_px).abs();
            if d < best_d {
                best_d = d;
                best = *s;
            }
        }
        best
    }
}

/// In-app Settings panel — the live UI for the values persisted in
/// [`app_prefs`]. Every change writes through immediately via
/// `App::save_app_prefs`, so closing the modal isn't a "commit"
/// step (matches macOS System Settings convention).
pub struct SettingsModal {
    /// Which page is currently displayed.
    pub category: SettingsCategory,
}

impl SettingsModal {
    pub fn new() -> Self {
        Self {
            category: SettingsCategory::Appearance,
        }
    }
}

/// Computed geometry for the Settings modal. Page-agnostic top-level
/// frame (panel / sidebar / content) plus a page-specific layout
/// enum carrying control rects.
struct SettingsLayout {
    panel: FRect,
    /// Title bar inside the panel that hosts "Settings" + close button.
    titlebar: FRect,
    close_rect: FRect,
    /// Left sidebar rect — fixed width, full content height.
    sidebar_rect: FRect,
    /// Each sidebar nav row, in `SettingsCategory::ALL` order.
    nav_items: Vec<(SettingsCategory, FRect)>,
    /// Right-hand content area (already inset by the content padding).
    content_rect: FRect,
    /// Page-specific control rects.
    page: PageLayout,
}

enum PageLayout {
    Appearance {
        page_title_y: f32,
        card: FRect,
        row: RowLayout,
        /// Three theme preview tiles, left-to-right.
        tiles: [(ThemePreference, FRect); 3],
    },
    Files {
        page_title_y: f32,
        card: FRect,
        row: RowLayout,
        /// Toggle hit zone. The whole row is also clickable.
        toggle: FRect,
    },
    Layout {
        page_title_y: f32,
        card: FRect,
        row: RowLayout,
        /// Strip rect for the Narrow/Medium/Wide segmented control.
        strip: FRect,
        /// Optional subscript "Currently 221 px" position when the
        /// splitter doesn't match any snap exactly.
        subscript_pos: Option<FPoint>,
    },
    About {
        page_title_y: f32,
        card: FRect,
    },
}

/// Hit-test result for the Settings modal.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SettingsHit {
    Inside,
    Close,
    Category(SettingsCategory),
    ThemeTile(ThemePreference),
    ToggleHidden,
    SidebarWidthSnap(SidebarWidthSnap),
}

/// State for the modal rename / new-folder dialog.
pub struct TextDialog {
    pub mode: DialogMode,
    pub input: TextInput,
}

/// State for inline (in-row) rename. Lives until the user hits
/// Enter (commit), Escape (cancel), clicks outside, or scrolls the row
/// off-screen. The row index references the active tab's `entries`
/// at edit-start; it stays stable because we don't refresh the tab
/// while editing.
pub struct InlineRenameState {
    pub row_idx: usize,
    pub original_name: String,
    pub input: TextInput,
}

#[derive(Clone)]
pub enum DialogMode {
    Rename { original_name: String },
    NewFolder,
}

impl DialogMode {
    fn title(&self) -> &'static str {
        match self {
            DialogMode::Rename { .. } => "Rename",
            DialogMode::NewFolder => "New Folder",
        }
    }
}

const SCROLLBAR_W: f32 = 10.0;
/// Default preview-pane width when first shown.
const PREVIEW_W_DEFAULT: f32 = 320.0;
/// Minimum preview width when the user drags the splitter narrow.
/// Below this the panel can't fit a label + value column comfortably.
const PREVIEW_W_MIN: f32 = 220.0;
/// Maximum preview width as an absolute upper bound. The splitter is
/// also clamped against a fraction of the file-pane area at drag time
/// so the file pane never collapses below ~200 DIPs.
const PREVIEW_W_MAX: f32 = 600.0;
const SIDEBAR_DEFAULT: f32 = 220.0;
const SIDEBAR_MIN: f32 = 160.0;
const SIDEBAR_MAX: f32 = 480.0;

/// Embedded dock/app icon. Reused from the Windows predecessor (Ferail)
/// — the colourful folder with "Fe". Set at runtime via NSApplication so
/// `cargo run` builds (no .app bundle) get the real icon in the dock.
const APP_ICON_PNG: &[u8] = include_bytes!("../resources/feraille.png");

/// Project URL opened by the `help.github` command. Update if/when the
/// repo moves; the placeholder below is chosen to be obviously not a
/// real URL so it triggers a fix rather than silently shipping wrong.
const PROJECT_URL: &str = "https://example.invalid/feraille";

/// Walk the command catalogue and try to match the current keystroke
/// against every command's `shortcuts` slice (primary + alternates).
/// Returns the matching `CommandId` or `None`. Single source of truth
/// for keyboard bindings: changing one lives in
/// Coloured-circle glyph for each Finder tag. Used as a leading
/// emoji in the right-click tag rows so the user can scan the
/// colour without us painting a real swatch. AppKit picks up the
/// emoji's native colour rendering — no attributed-string plumbing
/// needed.
fn tag_color_glyph(color: feraille_core::commands::TagColor) -> &'static str {
    use feraille_core::commands::TagColor;
    match color {
        TagColor::Red => "🔴",
        TagColor::Orange => "🟠",
        TagColor::Yellow => "🟡",
        TagColor::Green => "🟢",
        TagColor::Blue => "🔵",
        TagColor::Purple => "🟣",
        TagColor::Gray => "⚪",
    }
}

/// `feraille_core::commands`, not in any parallel match table.
///
/// Cross-platform note: `primary` matches when either `super` (Cmd
/// on macOS) or `control` is held — preserves the keyboard handler's
/// macOS-friendly-but-Linux-tolerant behaviour from before iter-5.9.
fn keystroke_to_command(logical: &Key, mods: ModifiersState) -> Option<CommandId> {
    let primary = mods.super_key() || mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    for spec in feraille_core::commands::all_commands() {
        for sc in spec.shortcuts {
            if sc.primary != primary || sc.shift != shift || sc.alt != alt {
                continue;
            }
            if matches_shortcut_key(sc.key, logical) {
                return Some(spec.id);
            }
        }
    }
    None
}

fn matches_shortcut_key(spec_key: &str, logical: &Key) -> bool {
    match (spec_key, logical) {
        ("F1", Key::Named(NamedKey::F1)) => true,
        ("F2", Key::Named(NamedKey::F2)) => true,
        ("F3", Key::Named(NamedKey::F3)) => true,
        ("F4", Key::Named(NamedKey::F4)) => true,
        ("F5", Key::Named(NamedKey::F5)) => true,
        ("F6", Key::Named(NamedKey::F6)) => true,
        ("F7", Key::Named(NamedKey::F7)) => true,
        ("F8", Key::Named(NamedKey::F8)) => true,
        ("F9", Key::Named(NamedKey::F9)) => true,
        ("F10", Key::Named(NamedKey::F10)) => true,
        ("F11", Key::Named(NamedKey::F11)) => true,
        ("F12", Key::Named(NamedKey::F12)) => true,
        ("Up", Key::Named(NamedKey::ArrowUp)) => true,
        ("Down", Key::Named(NamedKey::ArrowDown)) => true,
        ("Left", Key::Named(NamedKey::ArrowLeft)) => true,
        ("Right", Key::Named(NamedKey::ArrowRight)) => true,
        ("Backspace", Key::Named(NamedKey::Backspace)) => true,
        ("Delete", Key::Named(NamedKey::Delete)) => true,
        ("Tab", Key::Named(NamedKey::Tab)) => true,
        ("Enter", Key::Named(NamedKey::Enter)) => true,
        ("Escape", Key::Named(NamedKey::Escape)) => true,
        ("Home", Key::Named(NamedKey::Home)) => true,
        ("End", Key::Named(NamedKey::End)) => true,
        ("PageUp", Key::Named(NamedKey::PageUp)) => true,
        ("PageDown", Key::Named(NamedKey::PageDown)) => true,
        // Single-character keys: the catalogue's "T" / "[" / "." etc.
        // matches `Key::Character` case-insensitively, so the catalogue
        // can use the visually-natural case ("T") without forcing the
        // user to hold Shift.
        (other, Key::Character(c)) => c.eq_ignore_ascii_case(other),
        _ => false,
    }
}

fn main() -> Result<()> {
    obs::init();
    // `--reset-db <scope>` runs before any GUI work: opens the DB,
    // wipes the requested scope, prints a one-line confirmation,
    // and exits. Designed for support / dev iteration ("my window
    // restored at a weird size, clear UI prefs") without making the
    // user delete the whole file.
    if let Some(code) = handle_reset_db_cli() {
        std::process::exit(code);
    }
    let args = screenshot::parse_args();
    if args.screenshot.is_some() {
        log_info!(56, "headless screenshot path");
        return screenshot::run(args);
    }
    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    app.event_proxy = Some(event_loop.create_proxy());
    // Open the persistent metadata DB before the first navigation
    // commit. Hydrates the Ant Trail from `folder_usage` so heat
    // survives restarts.
    app.open_metadata_db();
    app.start_magic_prefetch();
    app.start_quarantine_prefetch();
    log_info!(56, "event loop starting");
    let result = event_loop.run_app(&mut app);
    if let Err(e) = &result {
        log_error!(56, "event loop returned error: {e}");
    }
    log_info!(56, "event loop exited");
    result?;
    Ok(())
}

pub struct Tab {
    pub current_dir: PathBuf,
    /// Full current-directory listing after hidden-file filtering.
    pub all_entries: Vec<FileEntry>,
    /// Visible entries after applying `filter_text` and sort.
    pub entries: Vec<FileEntry>,
    pub filter_text: String,
    pub selection: Selection,
    pub list_scroll: f32,
    /// `Some` when the last enumeration of `current_dir` failed.
    /// Surfaces in the file pane as an empty-state message — most
    /// commonly the macOS TCC permission prompt for ~/Documents,
    /// ~/Desktop, ~/Downloads when a sandboxed launcher hasn't been
    /// granted access.
    pub error: Option<EnumerationError>,
    /// Per-tab navigation history. `history_index` points at the
    /// current location. `navigate()` truncates forward then pushes;
    /// back/forward only move the index.
    pub history: Vec<PathBuf>,
    pub history_index: usize,
}

impl Tab {
    fn label(&self) -> String {
        self.current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_dir.to_string_lossy().into_owned())
    }
}

pub struct App {
    window: Option<Rc<Window>>,
    sb_context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    renderer: Option<SoftRenderer>,
    /// Disk Usage window — None until first `Cmd+Shift+D`. When the
    /// user closes it (CloseRequested on its WindowId) the field is
    /// reset to None and any in-flight scan is cancelled.
    disk_usage_window: Option<DiskUsageWindow>,
    /// Bumped whenever a disk-usage scan starts. Worker batches carry
    /// this; stale events are dropped on arrival.
    disk_usage_generation: u64,
    /// Set by `spawn_disk_usage_window` from inside `dispatch_command`
    /// (which doesn't have `&ActiveEventLoop`). The next `user_event`
    /// tick drains it via `try_realize_disk_usage_window` and creates
    /// the actual winit window.
    pending_disk_usage_open: Option<PendingDiskUsageOpen>,

    pub fs: Arc<NativeFs>,
    pub tabs: Vec<Tab>,
    pub active: usize,

    pub list: VirtualizedList,
    pub scrollbar: Scrollbar,
    pub splitter: Splitter,
    /// Splitter between file pane and preview pane. `min` / `max` are
    /// updated per drag based on current viewport width since the
    /// allowed range depends on layout.
    pub preview_splitter: Splitter,
    pub tabstrip: TabStrip,
    pub breadcrumb: BreadcrumbBar,
    pub tree: FileTree,
    pub focused_pane: FocusedPane,
    /// Footer progress strip — debounced; visible during long-running
    /// background work (magic prefetch, icon prefetch, enumeration).
    /// Driven by the registry through `begin_task` / `end_task`.
    pub progress: ProgressStrip,
    /// Single live strip token, present whenever `tasks` is non-empty.
    /// One shared strip across all concurrent tasks — the strip is a
    /// summary indicator; details live in the task panel.
    task_strip_token: Option<ProgressTaskId>,
    /// Active background tasks. Mutated only on the UI thread, from
    /// `App::begin_task` / `App::end_task`.
    pub tasks: TaskRegistry,
    /// Whether the task-list popover is open. Toggled by clicking the
    /// status bar while any task is active, dismissed on Escape, click
    /// outside, or when `tasks` becomes empty.
    pub task_panel_open: bool,
    /// In-flight magic-prefetch task. `Some` between `start_magic_prefetch`
    /// and the matching `MagicBatch` event. Stale tasks are ended silently
    /// when generation rolls over.
    magic_task: Option<TaskId>,
    pub splitter_x: f32,
    /// Width of the preview pane in DIPs when it's visible. Persists
    /// across `preview_visible` toggles so reopening the pane restores
    /// the user's last-set width.
    pub preview_width: f32,
    pub show_hidden: bool,
    pub ant_trail: AntTrail,
    /// Paths the user has pinned to the tree's Favorites section. In-
    /// memory for now; SQLite-persisted in iter-6 alongside ant trail.
    pub pinned_paths: Vec<PathBuf>,
    /// `Some(idx)` when the Get-Info panel is open, pointing at an
    /// index in the active tab's entries.
    pub properties_target: Option<usize>,
    /// Modal text dialog for rename / new-folder. `Some` while open.
    pub dialog: Option<TextDialog>,
    /// In-row rename state. `Some` while a list row is being edited.
    pub inline_rename: Option<InlineRenameState>,
    /// Last-painted rect of the inline-rename overlay, used for
    /// click-outside detection. Cleared when editing ends.
    inline_rename_rect: Option<FRect>,
    /// Transient bottom-right notifications. `log_error!` sites that
    /// matter to the user also push into this stack so they're visible
    /// in-app, not just on stderr.
    pub toasts: ToastStack,
    /// Find/filter dialog. While open, text input updates the visible list live.
    pub search: Option<TextInput>,
    /// Keyboard-shortcuts overlay (Cmd+/). Modal-style: capped height,
    /// scrollable body, live filter at top. `None` = closed.
    pub shortcuts_modal: Option<ShortcutsModal>,
    pub settings_modal: Option<SettingsModal>,
    pub preview_visible: bool,
    /// Cache of NSWorkspace-fetched icons keyed by `cache_key_for(entry)`
    /// — extension for files (".rs", ".md"), "DIR"/"SYMLINK"/"FILE" for
    /// the rest. Populated lazily on `prefetch_icons` after each navigate.
    pub icon_cache: HashMap<String, Bitmap>,
    /// Cache of magic-detected types keyed by `(path, mtime_unix)`. Empty
    /// string = "we tried, no match".
    pub magic_cache: HashMap<(PathBuf, i64), String>,
    /// Cache of macOS quarantine reads keyed by `(path, mtime_unix)`.
    /// `None` value = "we read the xattrs and the file is clean";
    /// `Some` carries the display-ready details. Removing/altering the
    /// xattrs touches mtime, which invalidates the entry naturally.
    pub quarantine_cache: HashMap<(PathBuf, i64), Option<QuarantineDetails>>,
    quarantine_generation: u64,
    quarantine_task: Option<TaskId>,
    /// Cache of Quick Look preview bitmaps keyed by `(path, mtime, size_px)`.
    /// `mtime` is the file's last-modified Unix seconds — bumping it
    /// invalidates the entry naturally on file edits. `size_px` is
    /// the longest-edge target we asked qlmanage for.
    /// Persistent metadata store. `None` in headless mode and when
    /// `$HOME` is unset; the rest of the app degrades gracefully
    /// (in-memory caches still work, just don't survive restart).
    pub metadata_db: Option<feraille_meta::MetadataDb>,
    pub preview_cache: HashMap<(PathBuf, i64, u32), Bitmap>,
    /// Inline-text preview cache for files whose extension marks
    /// them textual. Faster and far prettier than qlmanage's
    /// icon-sized text thumbnail. Keyed without a size since we read
    /// up to a fixed byte budget.
    pub preview_text_cache: HashMap<(PathBuf, i64), String>,
    /// Bumped on every selection change. Worker results are dropped
    /// at the gate when stale.
    preview_generation: u64,
    /// Set of `(path, mtime, size_px)` tuples currently being fetched
    /// — guards against duplicate spawns when paint runs while a
    /// fetch is already in flight.
    preview_pending: std::collections::HashSet<(PathBuf, i64, u32)>,
    /// Sentinel set: `qlmanage` already failed for these tuples.
    /// Stops the paint loop from re-spawning the same doomed worker
    /// every frame and lets the placeholder switch from "Generating"
    /// to "No preview".
    preview_failed: std::collections::HashSet<(PathBuf, i64, u32)>,
    event_proxy: Option<EventLoopProxy<AppEvent>>,
    magic_generation: u64,
    /// Pending (key, representative_path) pairs for the in-flight icon
    /// prefetch. Drained `ICON_CHUNK_SIZE` items per `IconChunkTick`.
    icon_queue: Vec<(String, PathBuf)>,
    icon_generation: u64,
    icon_task: Option<TaskId>,
    /// Generation counter for in-flight directory enumerations. Bumped
    /// at every `start_enumeration`; results are gated on equality.
    enumeration_generation: u64,
    /// Cancel flag for the in-flight enumeration worker, or `None` if
    /// idle. Setting it stops the worker after its next batch.
    enumeration_cancel: Option<Arc<AtomicBool>>,
    /// Registry id for the in-flight enumeration task; ended when
    /// superseded by a fresh navigation or when the final batch lands.
    enumeration_task: Option<TaskId>,
    /// Cursor name to preserve across in-flight batches. Set by
    /// `refresh_active_tab` so F5 keeps the cursor on the same file;
    /// `None` for plain navigation (cursor goes to row 0 of the first
    /// arriving batch and follows the user's pick afterwards).
    enumeration_preserve_cursor: Option<String>,
    /// Scroll offset to restore on each batch. Non-zero only via the
    /// refresh path; navigate resets to 0.
    enumeration_preserve_scroll: f32,
    /// `true` between `start_enumeration` and the first `EnumerationBatch`
    /// (or `EnumerationDone` if zero batches arrive). While set, the
    /// previous folder's listing is still painted; the first batch
    /// clears `all_entries` before extending so the swap is atomic.
    /// Removes the empty-frame flash on `goto_path` to a slow folder.
    enumeration_pending_first_batch: bool,
    /// Monotonic counter for tree-pane child loads. Each `spawn_tree_load`
    /// captures the next value; the matching `TreeChildrenLoaded` event
    /// is dropped if `tree_pending[id]` no longer holds it (superseded
    /// by `invalidate_tree` or a follow-up spawn).
    tree_load_generation: u64,
    /// `id -> generation` for tree-pane children loads currently in
    /// flight. Insert on spawn, remove when the matching event applies
    /// or on `invalidate_tree`. Acts as both a dedup key and a
    /// staleness gate.
    tree_pending: HashMap<NodeId, u64>,
    /// Pixel size for in-flight icon fetches; captured once at prefetch
    /// start so chunks stay consistent if the scale factor changes mid-run.
    icon_size_px: u32,
    /// `Some` when the user has mouse-down on a list row; promotes to a
    /// system drag once `(distance > 4 DIPs && time > 100 ms)`.
    drag_watch: Option<DragWatch>,
    pointer_dips: Option<FPoint>,
    modifiers: ModifiersState,

    pub tokens: Tokens,
    /// User-selected theme preference. `System` (default) tracks
    /// macOS Appearance live; `Light` / `Dark` pin a fixed theme.
    /// Persisted later (settings file); for now in-memory only.
    pub theme_preference: ThemePreference,
    /// User-facing UI scale multiplier, applied to every numeric
    /// dimension token (text, space, hit, icon, layout) when
    /// `apply_theme` builds `self.tokens`. 1.0 = baseline. Cmd+= /
    /// Cmd+- bump in 10% steps; Cmd+0 resets. Session-only for now;
    /// the same field will deserialize from a config file when
    /// settings persistence lands (iter-5.11+).
    pub ui_scale: f32,
    /// Last-known macOS Appearance state. Refreshed at startup and
    /// on every `AppEvent::SystemThemeChanged` so we can resolve the
    /// effective theme without re-querying NSApp on every redraw.
    pub system_is_dark: bool,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

/// User-facing theme choice. The effective rendered theme is derived
/// from this plus the cached system Appearance state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePreference {
    Light,
    Dark,
    /// Follow macOS Appearance (live).
    System,
}

impl App {
    /// Build an `App` configured for headless screenshot use. No window
    /// is opened; the caller sets dimensions and applies scripted state
    /// before calling `paint_to`.
    pub fn new_for_headless(theme: Theme) -> Self {
        let mut a = Self::new();
        // Pin the headless theme regardless of system Appearance —
        // screenshots want a deterministic look.
        a.theme_preference = match theme {
            Theme::Light => ThemePreference::Light,
            Theme::Dark => ThemePreference::Dark,
        };
        a.tokens = Tokens::for_theme(theme);
        a
    }

    pub fn set_dimensions(&mut self, width: u32, height: u32, scale: f32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.scale_factor = scale.max(0.5);
    }

    /// Resolve the user's preference + cached system Appearance into a
    /// concrete theme. The renderer paints from this; the menu
    /// checkmark uses the *preference* (so "Match System" stays
    /// checked even when the resolved theme flips).
    pub fn effective_theme(&self) -> Theme {
        match self.theme_preference {
            ThemePreference::Light => Theme::Light,
            ThemePreference::Dark => Theme::Dark,
            ThemePreference::System => {
                if self.system_is_dark {
                    Theme::Dark
                } else {
                    Theme::Light
                }
            }
        }
    }

    /// Snapshot the persisted-pref-relevant fields and write them to
    /// disk. Cheap (small `key=value` text file in the user's app
    /// support dir); call freely from mutator paths. Failures are
    /// swallowed by `app_prefs::save` so a read-only home directory
    /// doesn't break the running session.
    pub fn save_app_prefs(&self) {
        let theme = match self.theme_preference {
            ThemePreference::Light => app_prefs::ThemePref::Light,
            ThemePreference::Dark => app_prefs::ThemePref::Dark,
            ThemePreference::System => app_prefs::ThemePref::System,
        };
        app_prefs::save(app_prefs::AppPrefs {
            theme_preference: Some(theme),
            show_hidden: Some(self.show_hidden),
            sidebar_width: Some(self.splitter_x),
        });
    }

    /// Switch the user's theme preference and immediately re-resolve
    /// the effective theme. Idempotent — same preference is a no-op.
    pub fn set_theme_preference(&mut self, pref: ThemePreference) {
        if self.theme_preference == pref {
            return;
        }
        self.theme_preference = pref;
        self.apply_theme();
        self.save_app_prefs();
    }

    /// Recompute tokens from the current preference + system state +
    /// `ui_scale`, push the matching menu-item checkmarks, and forward
    /// fresh layout/row dimensions to controls that cache them.
    /// Call after every change to inputs (`theme_preference`,
    /// `system_is_dark`, `ui_scale`).
    pub fn apply_theme(&mut self) {
        self.tokens = Tokens::for_theme(self.effective_theme()).scaled(self.ui_scale);
        // Push dimensions to controls that cache layout for hit-test /
        // scroll math between paints.
        self.tree.set_layout(self.tokens.layout);
        self.list.row_height = self.tokens.hit.row;
        self.list.header_h = self.tokens.hit.row;
        feraille_shell_mac::set_command_state(
            CommandId("view.theme_light"),
            self.theme_preference == ThemePreference::Light,
        );
        feraille_shell_mac::set_command_state(
            CommandId("view.theme_dark"),
            self.theme_preference == ThemePreference::Dark,
        );
        feraille_shell_mac::set_command_state(
            CommandId("view.theme_system"),
            self.theme_preference == ThemePreference::System,
        );
    }

    /// UI-scale step. Cmd+= and Cmd+- move by this much; Cmd+0 resets
    /// to 1.0. Chosen so two presses give a clearly different look
    /// without needing fine-grained percentage control.
    pub const UI_SCALE_STEP: f32 = 0.1;

    /// Bump the UI scale by `delta` and re-apply tokens. Clamped by
    /// `feraille_design::UI_SCALE_{MIN,MAX}` inside `Tokens::scaled`.
    pub fn nudge_ui_scale(&mut self, delta: f32) {
        let next = (self.ui_scale + delta)
            .clamp(feraille_design::UI_SCALE_MIN, feraille_design::UI_SCALE_MAX);
        if (next - self.ui_scale).abs() < f32::EPSILON {
            return;
        }
        self.ui_scale = next;
        self.apply_theme();
        log_info!(59, "ui_scale -> {:.2}", self.ui_scale);
    }

    /// Reset UI scale to 1.0.
    pub fn reset_ui_scale(&mut self) {
        if (self.ui_scale - 1.0).abs() < f32::EPSILON {
            return;
        }
        self.ui_scale = 1.0;
        self.apply_theme();
        log_info!(59, "ui_scale -> 1.00 (reset)");
    }

    pub fn new_tab_at(&mut self, path: PathBuf) {
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        let new_index = self.tabs.len();
        self.tabs.push(Tab {
            current_dir: path.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            filter_text: String::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
            error: None,
            history: Vec::new(),
            history_index: 0,
        });
        self.active = new_index;
        self.list.scroll_offset = 0.0;
        self.navigate(path);
        feraille_shell_mac::set_tab_count(self.tabs.len());
    }

    pub fn switch_to_tab(&mut self, idx: usize) {
        self.switch_tab(idx);
        self.sync_window_title();
    }

    pub fn set_splitter(&mut self, x: f32) {
        self.splitter_x = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }

    pub fn set_scroll(&mut self, y: f32) {
        let viewport_h = self.list_inner_rect().size.height;
        let count = self.tabs[self.active].entries.len();
        self.list
            .scroll_by(y - self.list.scroll_offset, count, viewport_h);
    }

    pub fn select_row(&mut self, idx: usize) {
        let count = self.tabs[self.active].entries.len();
        if idx < count {
            self.tabs[self.active].selection.set_cursor(idx);
            let viewport_h = self.list_inner_rect().size.height;
            self.list.ensure_visible(idx, viewport_h);
        }
    }

    pub fn select_name(&mut self, name: &str) {
        let idx = self.tabs[self.active]
            .entries
            .iter()
            .position(|e| e.name == name);
        if let Some(i) = idx {
            self.select_row(i);
        }
    }

    pub fn enter_breadcrumb_edit_mode(&mut self) {
        let path = self.tabs[self.active].current_dir.clone();
        self.breadcrumb.enter_edit_mode(&path);
    }

    /// Copy the cursor entry's full path to the system clipboard.
    pub fn copy_cursor_path(&self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let path = tab.current_dir.join(&entry.name);
        if let Some(s) = path.to_str() {
            feraille_shell_mac::copy_to_clipboard(s);
        }
    }

    /// Multi-selection-aware copy. Single target degrades to the
    /// cursor entry; multi-target joins paths with `\n` so a paste
    /// into a text field gets one path per line.
    pub fn copy_selection_paths(&self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let joined = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        feraille_shell_mac::copy_to_clipboard(&joined);
    }

    /// Multi-selection-aware reveal. Each selected path opens in
    /// Finder via `open -R`. macOS coalesces multiple `open -R` to
    /// the same Finder window.
    pub fn reveal_selection_in_finder(&self) {
        let paths = self.resolve_selected_paths();
        for p in &paths {
            feraille_shell_mac::reveal_in_finder(p);
        }
    }

    /// Multi-selection-aware Trash. Synchronous (Cocoa
    /// `NSWorkspace.recycleURLs:` is fast). Refreshes the listing
    /// once at the end so multi-trash doesn't flicker.
    pub fn trash_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut any_failed = false;
        for p in &paths {
            if let Err(e) = move_to_trash(p) {
                log_error!(60, "move_to_trash({}) failed: {e}", p.display());
                any_failed = true;
            }
        }
        if any_failed {
            self.toast_error("Couldn't move some items to Trash");
        }
        self.refresh_active_tab();
    }

    /// Set the focused pane and propagate to the controls' visual state.
    pub fn set_focused_pane(&mut self, pane: FocusedPane) {
        if self.focused_pane == pane {
            return;
        }
        self.focused_pane = pane;
        self.tree.focused = matches!(pane, FocusedPane::Tree);
        self.list.focused = matches!(pane, FocusedPane::List);
        // Reset type-ahead when leaving the tree.
        if !self.tree.focused {
            self.tree.type_ahead_clear();
        }
    }

    /// F6 cycles between Tree and List. Headerless / non-focusable
    /// panes (breadcrumb, tabstrip) are not in the cycle — they take
    /// focus only via mouse.
    pub fn cycle_focus(&mut self) {
        let next = match self.focused_pane {
            FocusedPane::Tree => FocusedPane::List,
            FocusedPane::List => FocusedPane::Tree,
        };
        self.set_focused_pane(next);
    }

    /// Put the breadcrumb into edit mode pre-filled with the active
    /// tab's current directory.
    pub fn open_breadcrumb_edit(&mut self) {
        let path = self.tabs[self.active].current_dir.clone();
        self.breadcrumb.enter_edit_mode(&path);
    }

    /// Activate the next tab, wrapping at the end. No-op when only one
    /// tab is open.
    pub fn next_tab(&mut self) {
        let n = self.tabs.len();
        if n <= 1 {
            return;
        }
        let next = (self.active + 1) % n;
        self.switch_tab(next);
    }

    /// Activate the previous tab, wrapping at the start. No-op when
    /// only one tab is open.
    pub fn prev_tab(&mut self) {
        let n = self.tabs.len();
        if n <= 1 {
            return;
        }
        let prev = if self.active == 0 {
            n - 1
        } else {
            self.active - 1
        };
        self.switch_tab(prev);
    }

    /// Pin a path to Favorites if not already present, or remove it.
    /// Triggers a `rebuild_tree_sections` so the tree reflects the
    /// change.
    pub fn pin_path(&mut self, path: PathBuf) {
        if !self.pinned_paths.contains(&path) {
            self.pinned_paths.push(path);
            self.rebuild_tree_sections();
        }
    }

    pub fn unpin_path(&mut self, path: &Path) {
        let before = self.pinned_paths.len();
        self.pinned_paths.retain(|p| p != path);
        if self.pinned_paths.len() != before {
            self.rebuild_tree_sections();
        }
    }

    /// Right-click on the tree pane: build the section-appropriate menu
    /// and act on the user's choice. Mirrors `show_context_menu_at` for
    /// the list pane but with different actions.
    fn show_tree_context_menu_at(&mut self, p: FPoint) {
        let Some(target) = self.tree.right_click(self.tree_rect(), p) else {
            return;
        };
        self.set_focused_pane(FocusedPane::Tree);
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let path = match self.fs.path_for(target.id) {
            Some(p) => p,
            None => return,
        };
        let in_favorites = matches!(target.kind, SectionKind::Favorites);
        let mut plan = feraille_shell_mac::MenuPlan::new();
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.open"),
            "Open",
        ));
        // The tree pane only shows folders, so always offer Open
        // in New Tab as the second action.
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.open_in_new_tab"),
            "Open in New Tab",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.reveal_in_finder"),
            "Reveal in Finder",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.quick_look"),
            "Quick Look",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.copy_path"),
            "Copy Path",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        if in_favorites {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.remove_from_favorites"),
                "Remove from Favorites",
            ));
        } else {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.pin_to_favorites"),
                "Pin to Favorites",
            ));
        }
        let pick = feraille_shell_mac::show_context_menu(&window, plan, (p.x, p.y));
        match pick.as_ref().map(|p| p.command.0) {
            Some("file.open") => self.navigate(path.clone()),
            Some("file.open_in_new_tab") => self.new_tab_at(path.clone()),
            Some("file.reveal_in_finder") => feraille_shell_mac::reveal_in_finder(&path),
            Some("file.quick_look") => {
                if let Err(e) = feraille_shell_mac::show_quick_look(&[path.as_path()]) {
                    log_warn!(60, "quick_look failed: {e}");
                }
            }
            Some("file.copy_path") => {
                if let Some(s) = path.to_str() {
                    feraille_shell_mac::copy_to_clipboard(s);
                }
            }
            Some("file.pin_to_favorites") => self.pin_path(path),
            Some("file.remove_from_favorites") => self.unpin_path(&path),
            _ => {}
        }
        self.request_redraw();
    }

    /// Right-click handler: select the row (or expand the click into
    /// the existing multi-selection if the row is already part of
    /// it), then show a context menu at the click location.
    /// Synchronous — blocks the event loop while the menu is open.
    fn show_context_menu_at(&mut self, p: FPoint) {
        let inner = self.list_inner_rect();
        let count = self.tabs[self.active].entries.len();
        let Some(idx) = self.list.index_at(inner, p, count) else {
            // Right-click missed every row → background menu (acts
            // on the *folder*, not on a particular entry).
            self.show_background_context_menu_at(p);
            return;
        };
        // If the right-clicked row is already part of a multi-row
        // selection, leave the selection alone and act on the whole
        // set. Otherwise collapse to just that row. Mirrors Finder.
        let already_selected = matches!(
            &self.tabs[self.active].selection.set,
            SelectionSet::Range { .. } | SelectionSet::Discrete(_)
        ) && self.tabs[self.active].selection.set.contains(idx);
        if !already_selected {
            self.tabs[self.active].selection.set_cursor(idx);
        }
        let n = self.resolve_selected_paths().len().max(1);
        let many = n > 1;

        // Inspect the cursor entry: folders get an "Open in New
        // Tab" row and skip the Open With submenu (Launch Services
        // would just return Finder).
        let cursor_entry: Option<(EntryKind, String)> = self
            .tabs[self.active]
            .selection
            .cursor()
            .and_then(|i| self.tabs[self.active].entries.get(i))
            .map(|e| (e.kind, e.name.clone()));
        let is_folder = matches!(
            cursor_entry.as_ref().map(|(k, _)| *k),
            Some(EntryKind::Directory)
        );

        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let mut plan = feraille_shell_mac::MenuPlan::new();
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.open"),
            if many { "Open First" } else { "Open" },
        ));
        // Folder-only: "Open in New Tab" right after Open, mirroring
        // Finder's primary-action position. Single-target only.
        if is_folder && !many {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.open_in_new_tab"),
                "Open in New Tab",
            ));
        }
        // Open With submenu: files only, single-target. Folders
        // always open in Finder so Launch Services has nothing to
        // offer (and Finder hides Open With on folders too).
        if !many && !is_folder {
            if let Some(primary) = self
                .tabs[self.active]
                .selection
                .cursor()
                .and_then(|i| self.tabs[self.active].entries.get(i))
                .map(|e| self.tabs[self.active].current_dir.join(&e.name))
            {
                let candidates = feraille_shell_mac::open_with_candidates(&primary);
                if !candidates.is_empty() {
                    let mut sub: Vec<feraille_shell_mac::MenuPlanItem> = Vec::new();
                    for c in &candidates {
                        let label = if c.is_default {
                            format!("{} (default)", c.name)
                        } else {
                            c.name.clone()
                        };
                        sub.push(
                            feraille_shell_mac::MenuPlanItem::action_with_payload(
                                CommandId("file.open_with_app"),
                                label,
                                feraille_core::commands::CommandPayload::OpenWithApp {
                                    app_path: c.path.to_string_lossy().into_owned(),
                                },
                            ),
                        );
                    }
                    plan.push(feraille_shell_mac::MenuPlanItem::submenu(
                        "Open With", sub,
                    ));
                }
            }
        }
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.reveal_in_finder"),
            if many {
                format!("Reveal {n} in Finder")
            } else {
                "Reveal in Finder".to_string()
            },
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.get_info"),
            "Get Info",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.quick_look"),
            if many {
                format!("Quick Look {n} Items")
            } else {
                "Quick Look".to_string()
            },
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        // Rename is single-target only — Finder hides it on multi-
        // select rather than rename-N-at-once.
        if !many {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.rename"),
                "Rename",
            ));
        }
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.duplicate"),
            "Duplicate",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.make_alias"),
            "Make Alias",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.compress"),
            if many {
                format!("Compress {n} Items")
            } else {
                // Finder shows the entry name in curly quotes when
                // single-target. Fall back to bare "Compress" if
                // we somehow lost the cursor entry between hit-test
                // and now.
                cursor_entry
                    .as_ref()
                    .map(|(_, name)| format!("Compress \u{201C}{name}\u{201D}"))
                    .unwrap_or_else(|| "Compress".to_string())
            },
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.copy_path"),
            if many {
                format!("Copy {n} Paths")
            } else {
                "Copy Path".to_string()
            },
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.share"),
            "Share…",
        ));
        // Tags row: read from the cursor entry (primary target),
        // toggle applies to the whole selection. Reading is a
        // single Cocoa hop per path — fast enough on the UI thread.
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        let cursor_path = self
            .tabs[self.active]
            .selection
            .cursor()
            .and_then(|i| self.tabs[self.active].entries.get(i))
            .map(|e| self.tabs[self.active].current_dir.join(&e.name));
        let active_colors: Vec<feraille_core::commands::TagColor> =
            cursor_path
                .as_deref()
                .map(feraille_shell_mac::read_canonical_tags)
                .unwrap_or_default();
        for color in feraille_core::commands::TagColor::ALL {
            let is_set = active_colors.contains(&color);
            plan.push(
                feraille_shell_mac::MenuPlanItem::action_with_payload(
                    CommandId("file.set_tag"),
                    format!("{} {}", tag_color_glyph(color), color.name()),
                    feraille_core::commands::CommandPayload::Tag(Some(color)),
                )
                .checked(is_set),
            );
        }
        if !active_colors.is_empty() {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.clear_tags"),
                "Clear Tags",
            ));
        }
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.move_to_trash"),
            if many {
                format!("Move {n} Items to Trash")
            } else {
                "Move to Trash".to_string()
            },
        ));
        // System Services / Quick Actions. AppKit auto-populates
        // this submenu by walking the responder chain to find the
        // anchor installed at startup. We push the resolved
        // selection so the anchor has paths to vend.
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        plan.push(feraille_shell_mac::MenuPlanItem::services_submenu(
            "Services",
        ));
        feraille_shell_mac::set_services_selection(self.resolve_selected_paths());
        let pick = feraille_shell_mac::show_context_menu(&window, plan, (p.x, p.y));
        match pick.as_ref().map(|p| p.command.0) {
            Some("file.open") => self.open_at_cursor(),
            Some("file.open_in_new_tab") => self.open_cursor_in_new_tab(),
            Some("file.reveal_in_finder") => self.reveal_selection_in_finder(),
            Some("file.get_info") => self.toggle_properties(),
            Some("file.quick_look") => self.quick_look_selection(),
            Some("file.rename") => self.start_inline_rename(),
            Some("file.duplicate") => self.duplicate_selection(),
            Some("file.make_alias") => self.make_alias_for_selection(),
            Some("file.compress") => self.compress_selection(),
            Some("file.copy_path") => self.copy_selection_paths(),
            Some("file.share") => self.share_selection(),
            Some("file.open_with_app") => {
                if let Some(feraille_core::commands::CommandPayload::OpenWithApp {
                    app_path,
                }) = pick.as_ref().and_then(|p| p.payload.as_ref())
                {
                    self.open_selection_with(Path::new(app_path));
                }
            }
            Some("file.set_tag") => {
                if let Some(feraille_core::commands::CommandPayload::Tag(Some(color))) =
                    pick.as_ref().and_then(|p| p.payload.as_ref())
                {
                    self.toggle_tag_on_selection(*color);
                }
            }
            Some("file.clear_tags") => self.clear_tags_on_selection(),
            Some("file.move_to_trash") => self.trash_selection(),
            _ => {}
        }
        self.request_redraw();
    }

    /// Right-click on the empty space below the last row in the
    /// list pane — acts on the *current folder* rather than a
    /// particular entry. Mirrors what Finder shows when you right-
    /// click in an empty area of a window.
    fn show_background_context_menu_at(&mut self, p: FPoint) {
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let cur_dir = self.tabs[self.active].current_dir.clone();
        let mut plan = feraille_shell_mac::MenuPlan::new();
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.new_folder"),
            "New Folder",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.reveal_in_finder"),
            "Reveal in Finder",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.refresh"),
            "Refresh",
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        plan.push(
            feraille_shell_mac::MenuPlanItem::action(
                CommandId("view.toggle_hidden"),
                "Show Hidden Files",
            )
            .checked(self.show_hidden),
        );
        let pick = feraille_shell_mac::show_context_menu(&window, plan, (p.x, p.y));
        match pick.as_ref().map(|p| p.command.0) {
            Some("file.new_folder") => self.open_new_folder(),
            Some("file.reveal_in_finder") => feraille_shell_mac::reveal_in_finder(&cur_dir),
            Some("file.refresh") => self.refresh_active_tab(),
            Some("view.toggle_hidden") => self.toggle_hidden(),
            _ => {}
        }
        self.request_redraw();
    }

    /// Open Finder with the cursor entry selected.
    pub fn reveal_cursor_in_finder(&self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let path = tab.current_dir.join(&entry.name);
        feraille_shell_mac::reveal_in_finder(&path);
    }

    pub fn open_rename(&mut self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let name = entry.name.clone();
        self.dialog = Some(TextDialog {
            mode: DialogMode::Rename {
                original_name: name.clone(),
            },
            input: TextInput::new(&name),
        });
    }

    /// Start in-row rename for the cursor row of the active tab. Falls
    /// back silently if no row is selected.
    pub fn start_inline_rename(&mut self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let original_name = entry.name.clone();
        let input = TextInput::new(&original_name);
        self.inline_rename = Some(InlineRenameState {
            row_idx: idx,
            original_name: original_name.clone(),
            input,
        });
        log_info!(
            57,
            "inline rename: editing row {} ('{}')",
            idx,
            original_name
        );
    }

    /// Commit the in-row rename. On filesystem error, the state is
    /// preserved so the user can correct and retry.
    ///
    /// **Whitespace is preserved verbatim.** Filenames legitimately may
    /// contain leading, trailing, or interior spaces; a previous version
    /// silently `.trim()`-ed and lost user intent. Only a literally empty
    /// (zero-length) value is rejected — that always means "user pressed
    /// Enter on a cleared field" and should leave the file alone.
    fn commit_inline_rename(&mut self) {
        let Some(state) = self.inline_rename.take() else {
            return;
        };
        let new_name = state.input.value();
        if new_name.is_empty() || new_name == state.original_name {
            // No-op: empty value or unchanged name.
            self.inline_rename_rect = None;
            return;
        }
        let cur_dir = self.tabs[self.active].current_dir.clone();
        let from = cur_dir.join(&state.original_name);
        let to = cur_dir.join(&new_name);
        if let Err(e) = std::fs::rename(&from, &to) {
            log_error!(
                57,
                "inline rename({}, {}) failed: {e}",
                from.display(),
                to.display()
            );
            self.toast_error(format!("Rename failed: {e}"));
            // Restore so the user can correct and retry.
            self.inline_rename = Some(state);
            return;
        }
        log_info!(
            57,
            "inline rename committed: {} -> {}",
            state.original_name,
            new_name
        );
        self.refresh_active_tab();
        let tab = &mut self.tabs[self.active];
        if let Some(idx) = tab.entries.iter().position(|e| e.name == new_name) {
            tab.selection.set_cursor(idx);
        }
        self.inline_rename_rect = None;
    }

    fn cancel_inline_rename(&mut self) {
        if self.inline_rename.take().is_some() {
            log_info!(57, "inline rename cancelled");
        }
        self.inline_rename_rect = None;
    }

    /// Push a user-facing error toast. Pair with the matching
    /// `log_error!` so the message is also captured on stderr for
    /// crash investigation.
    fn toast_error(&mut self, message: impl Into<String>) {
        self.toasts.push(Toast::new(ToastKind::Error, message));
        self.request_redraw();
    }

    pub fn open_new_folder(&mut self) {
        self.dialog = Some(TextDialog {
            mode: DialogMode::NewFolder,
            input: TextInput::new("untitled folder"),
        });
    }

    pub fn open_search(&mut self) {
        let current = self.tabs[self.active].filter_text.clone();
        self.search = Some(TextInput::new(&current));
    }

    pub fn set_filter_text(&mut self, text: String) {
        if self.tabs[self.active].filter_text == text {
            return;
        }
        self.tabs[self.active].filter_text = text;
        self.rebuild_visible_entries(None, false);
    }

    pub fn set_preview_visible(&mut self, visible: bool) {
        self.preview_visible = visible;
    }

    pub fn toggle_preview(&mut self) {
        self.preview_visible = !self.preview_visible;
    }

    fn close_dialog(&mut self) {
        self.dialog = None;
    }

    fn submit_dialog(&mut self) {
        let Some(d) = self.dialog.take() else { return };
        // Preserve whitespace verbatim — filenames may contain legitimate
        // leading/trailing/internal spaces. Only reject a literally empty
        // value (Enter on a cleared field).
        let value = d.input.value();
        if value.is_empty() {
            return;
        }
        let cur_dir = self.tabs[self.active].current_dir.clone();
        let target_path = cur_dir.join(&value);
        match d.mode {
            DialogMode::Rename { original_name } => {
                if value == original_name {
                    return; // no-op
                }
                let from = cur_dir.join(&original_name);
                if let Err(e) = std::fs::rename(&from, &target_path) {
                    log_error!(
                        57,
                        "rename({}, {}) failed: {e}",
                        from.display(),
                        target_path.display()
                    );
                    self.toast_error(format!("Rename failed: {e}"));
                    return;
                }
                self.refresh_active_tab();
                let tab = &mut self.tabs[self.active];
                if let Some(idx) = tab.entries.iter().position(|e| e.name == value) {
                    tab.selection.set_cursor(idx);
                }
            }
            DialogMode::NewFolder => {
                if let Err(e) = std::fs::create_dir(&target_path) {
                    log_error!(57, "create_dir({}) failed: {e}", target_path.display());
                    self.toast_error(format!("Couldn't create folder: {e}"));
                    return;
                }
                self.refresh_active_tab();
                let tab = &mut self.tabs[self.active];
                if let Some(idx) = tab.entries.iter().position(|e| e.name == value) {
                    tab.selection.set_cursor(idx);
                }
            }
        }
    }

    pub fn toggle_properties(&mut self) {
        if self.properties_target.is_some() {
            self.properties_target = None;
        } else if let Some(idx) = self.tabs[self.active].selection.cursor() {
            if idx < self.tabs[self.active].entries.len() {
                self.properties_target = Some(idx);
            }
        }
    }

    fn close_properties(&mut self) {
        self.properties_target = None;
    }

    /// Layout rects for the Settings modal. Two-column layout: a
    /// fixed-width sidebar of category nav rows on the left, and a
    /// page-specific content area on the right. The page layout
    /// branches on `SettingsCategory`; all dimensions come from
    /// `tokens` so theme / ui-scale changes flow through automatically.
    fn settings_layout(
        &self,
        viewport: feraille_render::Size,
        tokens: &Tokens,
    ) -> SettingsLayout {
        let panel_w: f32 = 760.0;
        let panel_h: f32 = 520.0_f32.min((viewport.height - 80.0).max(360.0));
        // Padding=0 so we can paint the sidebar and content with
        // distinct background fills; content padding is applied
        // inside the content area.
        let (panel, _) = ModalPanel {
            viewport: FRect::new(0.0, 0.0, viewport.width, viewport.height),
            width: panel_w,
            height: panel_h,
            top_offset_fraction: Some(0.10),
            backdrop_alpha: 120,
            padding: 0.0,
        }
        .compute();

        // Titlebar: 44 DIPs tall, full panel width. Hosts the
        // "Settings" label on the left and the close pill on the right.
        let titlebar_h: f32 = 44.0;
        let titlebar = FRect::new(panel.left(), panel.top(), panel.size.width, titlebar_h);
        let close_size: f32 = 22.0;
        let close_rect = FRect::new(
            panel.right() - 14.0 - close_size,
            panel.top() + (titlebar_h - close_size) / 2.0,
            close_size,
            close_size,
        );

        // Below the titlebar: 200 DIP sidebar nav, then content area.
        let nav_w: f32 = 200.0;
        let sidebar_rect = FRect::new(
            panel.left(),
            panel.top() + titlebar_h,
            nav_w,
            panel.size.height - titlebar_h,
        );

        // Sidebar nav rows: top-padded, 36 DIP each, 8 DIP horizontal
        // inset. The first row gets a tiny extra gap above so it
        // doesn't kiss the titlebar.
        let nav_row_h: f32 = 36.0;
        let nav_inset_x: f32 = 8.0;
        let mut ny = sidebar_rect.top() + tokens.space.sm;
        let mut nav_items: Vec<(SettingsCategory, FRect)> = Vec::new();
        for cat in SettingsCategory::ALL {
            nav_items.push((
                *cat,
                FRect::new(
                    sidebar_rect.left() + nav_inset_x,
                    ny,
                    sidebar_rect.size.width - 2.0 * nav_inset_x,
                    nav_row_h,
                ),
            ));
            ny += nav_row_h + 2.0;
        }

        // Content area: everything to the right of the sidebar,
        // inset by `space.xl` on all sides so the card has room to
        // breathe (Ventura uses generous content padding here).
        let content_inset = tokens.space.xl;
        let content_rect = FRect::new(
            sidebar_rect.right() + content_inset,
            sidebar_rect.top() + content_inset,
            panel.right() - sidebar_rect.right() - 2.0 * content_inset,
            sidebar_rect.size.height - 2.0 * content_inset,
        );

        let category = self
            .settings_modal
            .as_ref()
            .map(|m| m.category)
            .unwrap_or(SettingsCategory::Appearance);

        // Page title sits at the top of the content area in text.lg.
        let page_title_y = content_rect.top();
        let card_top = content_rect.top() + tokens.text.lg + tokens.space.lg;

        let page = match category {
            SettingsCategory::Appearance => {
                // Single card containing the theme row. Description
                // is below the title; control slot below the
                // description, full-width, painted as 3 preview tiles.
                let card_h: f32 = 220.0;
                let card = FRect::new(
                    content_rect.left(),
                    card_top,
                    content_rect.size.width,
                    card_h,
                );
                let row = compute_settings_row(
                    FRect::new(
                        card.left(),
                        card.top() + tokens.space.md,
                        card.size.width,
                        ROW_H_DESCRIBED,
                    ),
                    tokens,
                    true,
                    0.0,
                    tokens.space.lg,
                );
                // Tiles area below the row: full card width minus
                // inset, height filling the rest of the card.
                let tiles_top = row.row.bottom() + tokens.space.sm;
                let tiles_area = FRect::new(
                    card.left() + tokens.space.lg,
                    tiles_top,
                    card.size.width - 2.0 * tokens.space.lg,
                    card.bottom() - tokens.space.md - tiles_top,
                );
                let tile_gap = tokens.space.md;
                let tile_w = (tiles_area.size.width - 2.0 * tile_gap) / 3.0;
                let tile_h = tiles_area.size.height;
                let mk_tile = |i: usize, pref: ThemePreference| -> (ThemePreference, FRect) {
                    (
                        pref,
                        FRect::new(
                            tiles_area.left() + (tile_w + tile_gap) * i as f32,
                            tiles_area.top(),
                            tile_w,
                            tile_h,
                        ),
                    )
                };
                let tiles = [
                    mk_tile(0, ThemePreference::Light),
                    mk_tile(1, ThemePreference::Dark),
                    mk_tile(2, ThemePreference::System),
                ];
                PageLayout::Appearance {
                    page_title_y,
                    card,
                    row,
                    tiles,
                }
            }
            SettingsCategory::Files => {
                let card_h: f32 = ROW_H_DESCRIBED + 2.0 * tokens.space.sm;
                let card = FRect::new(
                    content_rect.left(),
                    card_top,
                    content_rect.size.width,
                    card_h,
                );
                let toggle_w =
                    feraille_controls::primitives::settings_widgets::TOGGLE_W;
                let row = compute_settings_row(
                    FRect::new(
                        card.left(),
                        card.top() + tokens.space.sm,
                        card.size.width,
                        ROW_H_DESCRIBED,
                    ),
                    tokens,
                    true,
                    toggle_w,
                    tokens.space.lg,
                );
                let toggle = FRect::new(
                    row.control_slot.left(),
                    row.control_slot.top()
                        + (row.control_slot.size.height
                            - feraille_controls::primitives::settings_widgets::TOGGLE_H)
                            / 2.0,
                    toggle_w,
                    feraille_controls::primitives::settings_widgets::TOGGLE_H,
                );
                PageLayout::Files {
                    page_title_y,
                    card,
                    row,
                    toggle,
                }
            }
            SettingsCategory::Layout => {
                // Card hosts a row with description + a strip below.
                let strip_h: f32 = 30.0;
                let row_h = ROW_H_DESCRIBED;
                let card_h: f32 = row_h
                    + strip_h
                    + tokens.space.md
                    + 2.0 * tokens.space.sm
                    + tokens.text.sm
                    + 6.0;
                let card = FRect::new(
                    content_rect.left(),
                    card_top,
                    content_rect.size.width,
                    card_h,
                );
                let row = compute_settings_row(
                    FRect::new(
                        card.left(),
                        card.top() + tokens.space.sm,
                        card.size.width,
                        row_h,
                    ),
                    tokens,
                    true,
                    0.0,
                    tokens.space.lg,
                );
                let strip_w: f32 = 320.0;
                let strip = FRect::new(
                    card.left() + tokens.space.lg,
                    row.row.bottom() + tokens.space.xs,
                    strip_w.min(card.size.width - 2.0 * tokens.space.lg),
                    strip_h,
                );
                // Subscript only if the current splitter value
                // doesn't match a snap to within 1 px.
                let nearest = SidebarWidthSnap::nearest(self.splitter_x);
                let exact = (self.splitter_x - nearest.px()).abs() <= 1.0;
                let subscript_pos = if exact {
                    None
                } else {
                    Some(FPoint::new(
                        strip.left(),
                        strip.bottom() + tokens.space.xs,
                    ))
                };
                PageLayout::Layout {
                    page_title_y,
                    card,
                    row,
                    strip,
                    subscript_pos,
                }
            }
            SettingsCategory::About => {
                let card_h: f32 = 180.0;
                let card = FRect::new(
                    content_rect.left(),
                    card_top,
                    content_rect.size.width,
                    card_h,
                );
                PageLayout::About {
                    page_title_y,
                    card,
                }
            }
        };

        SettingsLayout {
            panel,
            titlebar,
            close_rect,
            sidebar_rect,
            nav_items,
            content_rect,
            page,
        }
    }

    fn paint_settings(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        if self.settings_modal.is_none() {
            return;
        }
        let layout = self.settings_layout(viewport, tokens);

        // Backdrop dim — paint manually since we asked ModalPanel
        // for zero padding (it would have drawn the panel fill).
        renderer.fill_rect(
            FRect::new(0.0, 0.0, viewport.width, viewport.height),
            feraille_design::Color::rgba(0, 0, 0, 120),
        );

        // Panel surface: rounded `radius.lg` window-style chrome.
        // Drop-shadow approximation: 1-DIP-offset translucent black
        // beneath the panel.
        fill_rounded_rect(
            renderer,
            FRect::new(
                layout.panel.left(),
                layout.panel.top() + 2.0,
                layout.panel.size.width,
                layout.panel.size.height,
            ),
            tokens.radius.lg,
            feraille_design::Color::rgba(0, 0, 0, 50),
        );
        stroke_rounded_rect(
            renderer,
            layout.panel,
            tokens.radius.lg,
            1.0,
            tokens.border.default,
            tokens.bg.layer1,
        );

        // Sidebar surface (subtly inset background).
        fill_rounded_rect(
            renderer,
            layout.sidebar_rect,
            tokens.radius.lg,
            tokens.bg.base,
        );
        // Square off the inner edges so the rounded corners only
        // appear on the panel's outer corners.
        renderer.fill_rect(
            FRect::new(
                layout.sidebar_rect.right() - tokens.radius.lg,
                layout.sidebar_rect.top(),
                tokens.radius.lg,
                layout.sidebar_rect.size.height,
            ),
            tokens.bg.base,
        );

        // Hairline between sidebar and content.
        renderer.fill_rect(
            FRect::new(
                layout.sidebar_rect.right(),
                layout.sidebar_rect.top(),
                1.0,
                layout.sidebar_rect.size.height,
            ),
            tokens.border.subtle,
        );

        // Titlebar text — "Settings" left-aligned, hairline below.
        renderer.draw_text(
            FPoint::new(
                layout.panel.left() + tokens.space.lg,
                text_y_center(layout.titlebar, tokens.text.md),
            ),
            "Settings",
            TextStyle {
                size: tokens.text.md,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        renderer.fill_rect(
            FRect::new(
                layout.titlebar.left(),
                layout.titlebar.bottom(),
                layout.titlebar.size.width,
                1.0,
            ),
            tokens.border.subtle,
        );

        // Close pill — circle of bg.layer3 with a centered "x".
        // Plain ASCII because the font's coverage of multiplication-
        // sign / heavy-cross glyphs is inconsistent.
        let radius = layout.close_rect.size.width / 2.0;
        fill_rounded_rect(
            renderer,
            layout.close_rect,
            radius,
            tokens.bg.layer3,
        );
        let close_style = TextStyle {
            size: tokens.text.md,
            weight: FontWeight::SemiBold,
            color: tokens.fg.primary,
        };
        let m = renderer.measure_text("x", close_style);
        renderer.draw_text(
            FPoint::new(
                layout.close_rect.left() + (layout.close_rect.size.width - m.width) / 2.0,
                text_y_center(layout.close_rect, tokens.text.md),
            ),
            "x",
            close_style,
        );

        // Sidebar nav items.
        let current = self
            .settings_modal
            .as_ref()
            .map(|m| m.category)
            .unwrap_or(SettingsCategory::Appearance);
        for (cat, rect) in &layout.nav_items {
            paint_sidebar_nav_item(
                renderer,
                tokens,
                *rect,
                cat.glyph(),
                cat.title(),
                *cat == current,
                false,
            );
        }

        // Page content.
        self.paint_settings_page(tokens, renderer, &layout);
    }

    /// Paint the per-category content. Called from `paint_settings`
    /// after the chrome (titlebar, sidebar, close) is laid down.
    fn paint_settings_page(
        &self,
        tokens: &Tokens,
        renderer: &mut dyn Renderer,
        layout: &SettingsLayout,
    ) {
        let title_style = TextStyle {
            size: tokens.text.lg,
            weight: FontWeight::SemiBold,
            color: tokens.fg.primary,
        };
        match &layout.page {
            PageLayout::Appearance {
                page_title_y,
                card,
                row,
                tiles,
            } => {
                renderer.draw_text(
                    FPoint::new(layout.content_rect.left(), *page_title_y),
                    "Appearance",
                    title_style,
                );
                paint_card(renderer, tokens, *card);
                paint_settings_row_text(
                    renderer,
                    tokens,
                    row,
                    "Theme",
                    Some("Match the system, or pick a side."),
                );
                for (pref, tile_rect) in tiles {
                    let selected = self.theme_preference == *pref;
                    let (kind, label) = match pref {
                        ThemePreference::Light => (PreviewKind::Light, "Light"),
                        ThemePreference::Dark => (PreviewKind::Dark, "Dark"),
                        ThemePreference::System => (PreviewKind::Auto, "Auto"),
                    };
                    paint_preview_tile(
                        renderer,
                        tokens,
                        *tile_rect,
                        kind,
                        selected,
                        label,
                    );
                }
            }
            PageLayout::Files {
                page_title_y,
                card,
                row,
                toggle,
            } => {
                renderer.draw_text(
                    FPoint::new(layout.content_rect.left(), *page_title_y),
                    "Files",
                    title_style,
                );
                paint_card(renderer, tokens, *card);
                paint_settings_row_text(
                    renderer,
                    tokens,
                    row,
                    "Show hidden files and folders",
                    Some("Display items that start with a dot, like .config and .ssh."),
                );
                paint_toggle(renderer, tokens, *toggle, self.show_hidden);
            }
            PageLayout::Layout {
                page_title_y,
                card,
                row,
                strip,
                subscript_pos,
            } => {
                renderer.draw_text(
                    FPoint::new(layout.content_rect.left(), *page_title_y),
                    "Layout",
                    title_style,
                );
                paint_card(renderer, tokens, *card);
                paint_settings_row_text(
                    renderer,
                    tokens,
                    row,
                    "Sidebar width",
                    Some("How wide the navigation panel appears."),
                );
                let nearest = SidebarWidthSnap::nearest(self.splitter_x);
                let selected_idx = SidebarWidthSnap::ALL
                    .iter()
                    .position(|s| *s == nearest)
                    .unwrap_or(1);
                let labels: Vec<&str> = SidebarWidthSnap::ALL
                    .iter()
                    .map(|s| s.label())
                    .collect();
                paint_segmented(renderer, tokens, *strip, &labels, selected_idx);
                if let Some(pos) = subscript_pos {
                    let txt = format!("Currently {:.0} px", self.splitter_x);
                    renderer.draw_text(
                        *pos,
                        &txt,
                        TextStyle {
                            size: tokens.text.sm,
                            weight: FontWeight::Regular,
                            color: tokens.fg.secondary,
                        },
                    );
                }
            }
            PageLayout::About {
                page_title_y,
                card,
            } => {
                renderer.draw_text(
                    FPoint::new(layout.content_rect.left(), *page_title_y),
                    "About",
                    title_style,
                );
                paint_card(renderer, tokens, *card);
                let inner_x = card.left() + tokens.space.lg;
                let mut y = card.top() + tokens.space.lg;
                renderer.draw_text(
                    FPoint::new(inner_x, y),
                    "Feraille",
                    TextStyle {
                        size: tokens.text.lg,
                        weight: FontWeight::SemiBold,
                        color: tokens.fg.primary,
                    },
                );
                y += tokens.text.lg + tokens.space.xs;
                renderer.draw_text(
                    FPoint::new(inner_x, y),
                    concat!("Version ", env!("CARGO_PKG_VERSION")),
                    TextStyle {
                        size: tokens.text.sm,
                        weight: FontWeight::Regular,
                        color: tokens.fg.secondary,
                    },
                );
                y += tokens.text.sm + tokens.space.md;
                renderer.draw_text(
                    FPoint::new(inner_x, y),
                    "The macOS port of Ferail — a Finder-class file explorer.",
                    TextStyle {
                        size: tokens.text.md,
                        weight: FontWeight::Regular,
                        color: tokens.fg.primary,
                    },
                );
                y += tokens.text.md + tokens.space.md;
                renderer.draw_text(
                    FPoint::new(inner_x, y),
                    "Built for speed, predictability, and a calm UI.",
                    TextStyle {
                        size: tokens.text.sm,
                        weight: FontWeight::Regular,
                        color: tokens.fg.secondary,
                    },
                );
            }
        }

        // Footer band along the bottom of the panel — separator +
        // single line of muted helper text. Lives at the panel level
        // (spans both columns) like the macOS System Settings footer.
        let footer_h = tokens.text.sm + 2.0 * tokens.space.sm;
        let footer_y = layout.panel.bottom() - footer_h;
        renderer.fill_rect(
            FRect::new(
                layout.panel.left(),
                footer_y - 1.0,
                layout.panel.size.width,
                1.0,
            ),
            tokens.border.subtle,
        );
        let footer_style = TextStyle {
            size: tokens.text.xs,
            weight: FontWeight::Regular,
            color: tokens.fg.secondary,
        };
        renderer.draw_text(
            FPoint::new(
                layout.panel.left() + tokens.space.lg,
                footer_y + (footer_h - tokens.text.xs) / 2.0 - 1.0,
            ),
            "Changes save instantly \u{00B7} Press Esc to close",
            footer_style,
        );
    }

    /// Hit-test the Settings modal at point `p`. Returns `None`
    /// when the click is outside the panel (caller dismisses), or
    /// a `SettingsHit` when it's inside.
    fn settings_hit(
        &self,
        p: FPoint,
        viewport: feraille_render::Size,
        tokens: &Tokens,
    ) -> Option<SettingsHit> {
        let layout = self.settings_layout(viewport, tokens);
        if !layout.panel.contains(p) {
            return None;
        }
        if layout.close_rect.contains(p) {
            return Some(SettingsHit::Close);
        }
        for (cat, rect) in &layout.nav_items {
            if rect.contains(p) {
                return Some(SettingsHit::Category(*cat));
            }
        }
        // Page-specific hits.
        match &layout.page {
            PageLayout::Appearance { tiles, .. } => {
                for (pref, tile) in tiles {
                    if tile.contains(p) {
                        return Some(SettingsHit::ThemeTile(*pref));
                    }
                }
            }
            PageLayout::Files { row, toggle, .. } => {
                if toggle_hit(*toggle, p) || row.row.contains(p) {
                    return Some(SettingsHit::ToggleHidden);
                }
            }
            PageLayout::Layout { strip, .. } => {
                if let Some(idx) =
                    segmented_hit(*strip, SidebarWidthSnap::ALL.len(), p)
                {
                    if let Some(snap) = SidebarWidthSnap::ALL.get(idx).copied() {
                        return Some(SettingsHit::SidebarWidthSnap(snap));
                    }
                }
            }
            PageLayout::About { .. } => {}
        }
        Some(SettingsHit::Inside)
    }

    fn paint_dialog(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        let Some(d) = self.dialog.as_ref() else {
            return;
        };
        let (panel, body) = ModalPanel {
            viewport: FRect::new(0.0, 0.0, viewport.width, viewport.height),
            width: 420.0,
            height: 140.0,
            top_offset_fraction: None,
            backdrop_alpha: 90,
            padding: tokens.space.lg,
        }
        .paint(tokens, renderer);
        renderer.draw_text(
            FPoint::new(body.left(), body.top()),
            d.mode.title(),
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        let input_y = body.top() + tokens.text.lg + tokens.space.md;
        let input_rect = FRect::new(body.left(), input_y, body.size.width, 32.0);
        d.input.paint(input_rect, true, tokens, renderer);
        renderer.draw_text(
            FPoint::new(
                body.left(),
                panel.bottom() - tokens.space.lg - tokens.text.xs,
            ),
            "Enter to confirm \u{00B7} Esc to cancel",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );
    }

    fn paint_search(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        let Some(input) = self.search.as_ref() else {
            return;
        };
        let (panel, body) = ModalPanel {
            viewport: FRect::new(0.0, 0.0, viewport.width, viewport.height),
            width: 520.0,
            height: 132.0,
            top_offset_fraction: Some(0.18),
            backdrop_alpha: 70,
            padding: tokens.space.lg,
        }
        .paint(tokens, renderer);
        renderer.draw_text(
            FPoint::new(body.left(), body.top()),
            "Filter",
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        let input_rect = FRect::new(
            body.left(),
            body.top() + tokens.text.lg + tokens.space.md,
            body.size.width,
            32.0,
        );
        input.paint(input_rect, true, tokens, renderer);
        renderer.draw_text(
            FPoint::new(
                body.left(),
                panel.bottom() - tokens.space.lg - tokens.text.xs,
            ),
            "Type to filter current folder \u{00B7} Enter to close \u{00B7} Esc to dismiss",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );
    }

    /// Format one Shortcut as macOS glyphs + key. Shared by the
    /// shortcuts overlay's body and its filter-matching predicate.
    /// Layout of the shortcuts modal — single source for paint and
    /// hit-test. Mirrors the ModalPanel sizing in `paint_shortcuts`.
    fn shortcuts_panel_layout(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
    ) -> (FRect, FRect) {
        let panel_w: f32 = 560.0;
        let panel_h: f32 = 560.0_f32.min((viewport.height - 80.0).max(280.0));
        let pad = tokens.space.lg;
        feraille_controls::primitives::panel::ModalPanel {
            viewport: FRect::new(0.0, 0.0, viewport.width, viewport.height),
            width: panel_w,
            height: panel_h,
            top_offset_fraction: Some(0.10),
            backdrop_alpha: 90,
            padding: pad,
        }
        .compute()
    }

    /// Close-button rect for the shortcuts modal — 24×24 in the
    /// panel's top-right corner with a small inset.
    fn shortcuts_close_rect_from_panel(panel: FRect) -> FRect {
        const SIZE: f32 = 24.0;
        const INSET: f32 = 12.0;
        FRect::new(
            panel.right() - INSET - SIZE,
            panel.top() + INSET,
            SIZE,
            SIZE,
        )
    }

    fn fmt_shortcut(sc: &feraille_core::commands::Shortcut) -> String {
        // Arial doesn't carry the canonical Mac modifier glyphs (⌘ ⌥ ⇧)
        // so they render as missing-glyph boxes. Use word labels —
        // matches the App menu's plain-text style and reads cleanly
        // even for users who don't know the symbols.
        let mut parts: Vec<String> = Vec::new();
        if sc.primary {
            parts.push("Cmd".to_string());
        }
        if sc.alt {
            parts.push("Opt".to_string());
        }
        if sc.shift {
            parts.push("Shift".to_string());
        }
        parts.push(sc.key.to_string());
        parts.join("+")
    }

    fn paint_shortcuts(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        use feraille_core::commands::{all_commands, Category, CommandSpec};

        let Some(modal) = self.shortcuts_modal.as_ref() else {
            return;
        };

        // Panel sizing. Width is fixed; height caps at the smaller of
        // 560 DIP and (viewport - 80 DIP margin) so the modal never
        // bleeds off-screen on short windows.
        let panel_w: f32 = 560.0;
        let panel_h: f32 = 560.0_f32.min((viewport.height - 80.0).max(280.0));
        let pad = tokens.space.lg;

        let (panel, body) = ModalPanel {
            viewport: FRect::new(0.0, 0.0, viewport.width, viewport.height),
            width: panel_w,
            height: panel_h,
            top_offset_fraction: Some(0.10),
            backdrop_alpha: 90,
            padding: pad,
        }
        .paint(tokens, renderer);

        // Title row.
        let title_y = body.top();
        renderer.draw_text(
            FPoint::new(body.left(), title_y),
            "Keyboard Shortcuts",
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );

        // Close button — top-right of the panel chrome. Rendered as
        // an "x" glyph (Arial doesn't carry × cleanly at small sizes)
        // in a 24×24 hit zone. Mouse handler routes a click here to
        // `close_shortcuts_modal`.
        let close_rect = Self::shortcuts_close_rect_from_panel(panel);
        renderer.fill_rect(close_rect, tokens.bg.layer2);
        renderer.stroke_rect(close_rect, 1.0, tokens.border.subtle);
        let close_glyph = "x";
        let close_style = TextStyle {
            size: tokens.text.md,
            weight: FontWeight::SemiBold,
            color: tokens.fg.secondary,
        };
        let metrics = renderer.measure_text(close_glyph, close_style);
        renderer.draw_text(
            FPoint::new(
                close_rect.left() + (close_rect.size.width - metrics.width) / 2.0,
                close_rect.top() + (close_rect.size.height - tokens.text.md) / 2.0 - 1.0,
            ),
            close_glyph,
            close_style,
        );

        // Filter input.
        let input_y = title_y + tokens.text.lg + tokens.space.md;
        let input_rect = FRect::new(body.left(), input_y, body.size.width, 32.0);
        modal.filter.paint(input_rect, true, tokens, renderer);

        // Divider below the input.
        let divider_y = input_rect.bottom() + tokens.space.md;
        renderer.fill_rect(
            FRect::new(body.left(), divider_y, body.size.width, 1.0),
            tokens.border.subtle,
        );

        // Footer hint, anchored to the panel bottom — sized first so the
        // body height calculation knows how much room is left.
        let footer_h = tokens.text.xs;
        let footer_y = panel.bottom() - tokens.space.lg - footer_h;
        renderer.draw_text(
            FPoint::new(body.left(), footer_y),
            "Type to filter \u{00B7} Esc to close",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );

        // Scrollable body area sits between the divider and the footer.
        let body_top = divider_y + tokens.space.md;
        let body_bottom = footer_y - tokens.space.sm;
        let scroll_w = 10.0;
        let body_rect = FRect::new(
            body.left(),
            body_top,
            (body.size.width - scroll_w - 4.0).max(0.0),
            (body_bottom - body_top).max(0.0),
        );
        if body_rect.size.height <= 0.0 {
            return;
        }

        // Filter predicate. Lowercased once outside the loop.
        let filter = modal.filter.value().to_lowercase();
        let filter = filter.trim();
        let row_passes = |spec: &&CommandSpec| -> bool {
            if filter.is_empty() {
                return true;
            }
            if spec.title.to_lowercase().contains(filter) {
                return true;
            }
            spec.shortcuts
                .iter()
                .any(|sc| Self::fmt_shortcut(sc).to_lowercase().contains(filter))
        };

        let categories: [(Category, &str); 8] = [
            (Category::App, "App"),
            (Category::File, "File"),
            (Category::Edit, "Edit"),
            (Category::View, "View"),
            (Category::Go, "Go"),
            (Category::Selection, "Selection"),
            (Category::Window, "Window"),
            (Category::Help, "Help"),
        ];

        // Group → sorted list, dropping empty groups under the current
        // filter so we don't show stranded category headers.
        let mut groups: Vec<(&str, Vec<&CommandSpec>)> = Vec::with_capacity(categories.len());
        for (cat, label) in categories {
            let mut g: Vec<&CommandSpec> = all_commands()
                .iter()
                .filter(|s| s.category == cat && !s.shortcuts.is_empty())
                .filter(row_passes)
                .collect();
            if g.is_empty() {
                continue;
            }
            g.sort_by_key(|s| s.title);
            groups.push((label, g));
        }

        let header_h = tokens.text.sm + 6.0;
        let row_h = tokens.text.md + 10.0;
        let group_gap = tokens.space.sm;
        let mut total_h: f32 = 0.0;
        for (i, (_, g)) in groups.iter().enumerate() {
            if i > 0 {
                total_h += group_gap;
            }
            total_h += header_h;
            total_h += g.len() as f32 * row_h;
        }

        // Clamp scroll to current content. Re-clamping every frame
        // means filter changes that shrink content reset the scroll
        // correctly without an explicit hook.
        let max_scroll = (total_h - body_rect.size.height).max(0.0);
        let scroll = modal.scroll_offset.clamp(0.0, max_scroll);

        // Render body with a clip.
        renderer.push_clip(body_rect);
        if groups.is_empty() {
            renderer.draw_text(
                FPoint::new(body_rect.left(), body_rect.top() + tokens.space.sm),
                "No shortcuts match.",
                TextStyle {
                    size: tokens.text.md,
                    weight: FontWeight::Regular,
                    color: tokens.fg.disabled,
                },
            );
        } else {
            // The shortcut-keys column gets a fixed width so the title
            // column lines up across categories.
            let keys_col_w: f32 = 150.0;
            let title_col_x = body_rect.left() + keys_col_w + tokens.space.md;

            let mut y = body_rect.top() - scroll;
            for (i, (label, g)) in groups.iter().enumerate() {
                if i > 0 {
                    y += group_gap;
                }
                // Category header.
                renderer.draw_text(
                    FPoint::new(body_rect.left(), y),
                    label,
                    TextStyle {
                        size: tokens.text.sm,
                        weight: FontWeight::SemiBold,
                        color: tokens.fg.secondary,
                    },
                );
                y += header_h;
                for spec in g {
                    let keys = spec
                        .shortcuts
                        .iter()
                        .map(Self::fmt_shortcut)
                        .collect::<Vec<_>>()
                        .join("  \u{00B7}  ");
                    renderer.draw_text(
                        FPoint::new(body_rect.left() + tokens.space.sm, y),
                        &keys,
                        TextStyle {
                            size: tokens.text.md,
                            weight: FontWeight::Regular,
                            color: tokens.fg.primary,
                        },
                    );
                    renderer.draw_text(
                        FPoint::new(title_col_x, y),
                        spec.title,
                        TextStyle {
                            size: tokens.text.md,
                            weight: FontWeight::Regular,
                            color: tokens.fg.primary,
                        },
                    );
                    y += row_h;
                }
            }
        }
        renderer.pop_clip();

        // Scrollbar — flush right of the body, full height.
        let track = FRect::new(
            body_rect.right() + 4.0,
            body_rect.top(),
            scroll_w - 2.0,
            body_rect.size.height,
        );
        self.scrollbar.paint(
            track,
            total_h,
            body_rect.size.height,
            scroll,
            tokens,
            renderer,
        );
    }

    fn paint_preview_pane(&self, bounds: FRect, tokens: &Tokens, renderer: &mut dyn Renderer) {
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return;
        }
        // Outer "shelf" — a slightly recessed surface that the cards sit on.
        renderer.fill_rect(bounds, tokens.bg.layer2);
        // Vertical hairline against the file pane (the splitter rule
        // also paints here, but only when visible — keep this so the
        // pane reads as a separated surface even when no splitter is hovered).
        renderer.fill_rect(
            FRect::new(bounds.left(), bounds.top(), 1.0, bounds.size.height),
            tokens.border.subtle,
        );
        renderer.push_clip(bounds);

        // Card layout: outer margin around the cards, gap between them.
        let outer = tokens.space.lg;
        let gap = tokens.space.md;
        let card_inset = tokens.space.lg;
        let card_x = bounds.left() + outer;
        let card_w = (bounds.size.width - outer * 2.0).max(0.0);

        let tab = &self.tabs[self.active];
        let selected = tab.selection.cursor().and_then(|idx| tab.entries.get(idx));

        let Some(entry) = selected else {
            // Empty-state card.
            let empty_h = 96.0;
            let empty_card = FRect::new(card_x, bounds.top() + outer, card_w, empty_h);
            paint_card_chrome(empty_card, tokens, renderer);
            renderer.draw_text(
                FPoint::new(
                    empty_card.left() + card_inset,
                    empty_card.top() + card_inset,
                ),
                "Preview",
                TextStyle {
                    size: tokens.text.lg,
                    weight: FontWeight::SemiBold,
                    color: tokens.fg.primary,
                },
            );
            renderer.draw_text(
                FPoint::new(
                    empty_card.left() + card_inset,
                    empty_card.top() + card_inset + tokens.text.lg + 6.0,
                ),
                "No item selected",
                TextStyle {
                    size: tokens.text.md,
                    weight: FontWeight::Regular,
                    color: tokens.fg.secondary,
                },
            );
            renderer.pop_clip();
            return;
        };

        // Header card: icon + name + kind.
        let icon_size = 32.0;
        let header_h = card_inset * 2.0 + icon_size;
        let header_card = FRect::new(card_x, bounds.top() + outer, card_w, header_h);
        paint_card_chrome(header_card, tokens, renderer);
        let icon_x = header_card.left() + card_inset;
        let icon_y = header_card.top() + card_inset;
        if let Some(bitmap) = self.icon_cache.get(&cache_key_for(entry)) {
            renderer.draw_bitmap(FRect::new(icon_x, icon_y, icon_size, icon_size), bitmap);
        } else {
            renderer.fill_rect(
                FRect::new(icon_x, icon_y, icon_size, icon_size),
                tokens.accent.fill,
            );
        }
        let title_x = icon_x + icon_size + tokens.space.md;
        renderer.draw_text(
            FPoint::new(title_x, icon_y + 1.0),
            &entry.name,
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        renderer.draw_text(
            FPoint::new(title_x, icon_y + tokens.text.lg + 6.0),
            &entry.display_kind,
            TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );

        // Preview thumbnail card — Quick Look render of the selected
        // file. Sized to a 4:3 aspect ratio so portraits and landscape
        // images both look reasonable; the bitmap inside is drawn
        // letterboxed to preserve its real aspect.
        let thumb_card_h = (card_w * 0.72).clamp(160.0, 320.0);
        let thumb_card =
            FRect::new(card_x, header_card.bottom() + gap, card_w, thumb_card_h);
        paint_card_chrome(thumb_card, tokens, renderer);
        let inner = FRect::new(
            thumb_card.left() + card_inset,
            thumb_card.top() + card_inset,
            (thumb_card.size.width - card_inset * 2.0).max(0.0),
            (thumb_card.size.height - card_inset * 2.0).max(0.0),
        );
        let path_for_key = tab.current_dir.join(&entry.name);
        let key = (path_for_key.clone(), entry.mtime_unix, Self::PREVIEW_THUMB_PX);
        let text_key = (path_for_key.clone(), entry.mtime_unix);
        let is_dir = matches!(entry.kind, feraille_core::EntryKind::Directory);
        let text_snippet = if is_dir {
            None
        } else {
            self.preview_text_cache.get(&text_key)
        };

        if let Some(snippet) = text_snippet {
            // Inline text rendering: monospace-feel via fixed line
            // step, clipped to the inner rect. No syntax highlighting
            // in v1 — that's a downstream feature.
            renderer.push_clip(inner);
            let style = TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.primary,
            };
            let line_h = tokens.text.sm + 4.0;
            let mut y = inner.top();
            for raw_line in snippet.lines() {
                if y + line_h > inner.bottom() {
                    break;
                }
                // Truncate per-line at the right edge so wide lines
                // don't paint past the clip into next neighbours.
                let line = if raw_line.len() > 4096 {
                    &raw_line[..4096]
                } else {
                    raw_line
                };
                renderer.draw_text(FPoint::new(inner.left(), y), line, style);
                y += line_h;
            }
            renderer.pop_clip();
        } else if let (false, Some(bm)) = (is_dir, self.preview_cache.get(&key)) {
            // Letterbox: scale-to-fit while preserving aspect.
            let bw = bm.width as f32;
            let bh = bm.height as f32;
            if bw > 0.0 && bh > 0.0 && inner.size.width > 0.0 && inner.size.height > 0.0 {
                let scale = (inner.size.width / bw).min(inner.size.height / bh);
                let draw_w = bw * scale;
                let draw_h = bh * scale;
                let dx = inner.left() + (inner.size.width - draw_w) / 2.0;
                let dy = inner.top() + (inner.size.height - draw_h) / 2.0;
                renderer.draw_bitmap(FRect::new(dx, dy, draw_w, draw_h), bm);
            }
        } else {
            // Placeholder copy: "Generating preview…" while the
            // worker is in flight, "No preview available" for dirs
            // (we don't fetch thumbnails for them) or after a failed
            // qlmanage run that left no entry in the cache.
            let style = TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            };
            let msg = if is_dir {
                "No preview available"
            } else if self.preview_failed.contains(&key) {
                "No preview available"
            } else if self.preview_pending.contains(&key) {
                "Generating preview…"
            } else {
                "Generating preview…"
            };
            let m = renderer.measure_text(msg, style);
            renderer.draw_text(
                FPoint::new(
                    inner.left() + (inner.size.width - m.width) / 2.0,
                    inner.top() + (inner.size.height - tokens.text.sm) / 2.0,
                ),
                msg,
                style,
            );
        }

        // Metadata card: key/value rows. Same source as the Get-Info modal
        // (`paint_properties`) so both panels stay in lockstep — see
        // `info_rows`.
        let rows = info_rows(entry, &tab.current_dir.join(&entry.name));
        let row_step = tokens.text.xs + 5.0 + tokens.text.sm + tokens.space.md;
        let metadata_h = card_inset * 2.0 + (rows.len() as f32) * row_step - tokens.space.md;
        let metadata_card = FRect::new(card_x, thumb_card.bottom() + gap, card_w, metadata_h);
        paint_card_chrome(metadata_card, tokens, renderer);
        let mut y = metadata_card.top() + card_inset;
        for (label, value) in &rows {
            renderer.draw_text(
                FPoint::new(metadata_card.left() + card_inset, y),
                label,
                TextStyle {
                    size: tokens.text.xs,
                    weight: FontWeight::Medium,
                    color: tokens.fg.secondary,
                },
            );
            y += tokens.text.xs + 5.0;
            renderer.draw_text(
                FPoint::new(metadata_card.left() + card_inset, y),
                value,
                TextStyle {
                    size: tokens.text.sm,
                    weight: FontWeight::Regular,
                    color: tokens.fg.primary,
                },
            );
            y += tokens.text.sm + tokens.space.md;
        }

        renderer.pop_clip();
    }

    fn paint_properties(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        let Some(idx) = self.properties_target else {
            return;
        };
        let tab = &self.tabs[self.active];
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };

        // Single source of truth for the inspector fields — see `info_rows`.
        let rows = info_rows(entry, &tab.current_dir.join(&entry.name));

        // Modal height: header (icon + name + divider) + rows + footer.
        let row_step = tokens.text.md * 2.0;
        let header_h = 32.0 + tokens.space.xl + (tokens.space.xl - 4.0);
        let footer_h = tokens.space.xl + tokens.text.xs + 8.0;
        let modal_height =
            (header_h + (rows.len() as f32) * row_step + footer_h).max(380.0);

        let (panel, _body) = ModalPanel {
            viewport: FRect::new(0.0, 0.0, viewport.width, viewport.height),
            width: 480.0,
            height: modal_height,
            top_offset_fraction: None,
            backdrop_alpha: 90,
            padding: tokens.space.xl,
        }
        .paint(tokens, renderer);

        renderer.push_clip(panel);

        let pad = tokens.space.xl;
        let mut y = panel.top() + pad;

        // Title row: large icon (if cached) + name on the right.
        let icon_size = 32.0;
        let icon_x = panel.left() + pad;
        if let Some(bitmap) = self.icon_cache.get(&cache_key_for(entry)) {
            renderer.draw_bitmap(FRect::new(icon_x, y, icon_size, icon_size), bitmap);
        } else {
            renderer.fill_rect(
                FRect::new(icon_x, y, icon_size, icon_size),
                tokens.accent.fill,
            );
        }
        let name_x = icon_x + icon_size + tokens.space.md;
        renderer.draw_text(
            FPoint::new(name_x, y + 2.0),
            &entry.name,
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        renderer.draw_text(
            FPoint::new(name_x, y + tokens.text.lg + 6.0),
            &entry.display_kind,
            TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
        y += icon_size + pad;

        // Divider
        renderer.fill_rect(
            FRect::new(panel.left() + pad, y, panel.size.width - pad * 2.0, 1.0),
            tokens.border.subtle,
        );
        y += pad - 4.0;

        for (label, value) in &rows {
            renderer.draw_text(
                FPoint::new(panel.left() + pad, y),
                label,
                TextStyle {
                    size: tokens.text.sm,
                    weight: FontWeight::Medium,
                    color: tokens.fg.secondary,
                },
            );
            renderer.draw_text(
                FPoint::new(panel.left() + pad + 90.0, y),
                value,
                TextStyle {
                    size: tokens.text.md,
                    weight: FontWeight::Regular,
                    color: tokens.fg.primary,
                },
            );
            y += row_step;
        }

        renderer.pop_clip();

        // Footer hint
        renderer.draw_text(
            FPoint::new(panel.left() + pad, panel.bottom() - tokens.space.xl),
            "Esc to close · Cmd+I again to toggle",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );
    }

    /// Resolve a path → NodeId via the FS (allocating an ID if new).
    pub fn id_for_path(&self, path: &Path) -> NodeId {
        self.fs.id_for_path(path)
    }

    fn new() -> Self {
        let fs = Arc::new(NativeFs::new());
        let home = home_dir();

        // Pull persisted user preferences once at startup. Missing
        // file or unparseable lines fall through to defaults.
        let prefs = app_prefs::load();

        // Seed the tree with Finder-style sections: Recents (top of ant
        // trail) / Favorites (pinned) / Locations (iCloud, Home, root,
        // Trash) / Volumes (other /Volumes mounts).
        let tree = FileTree::new();
        // Default Favorites mirror Finder's out-of-the-box defaults.
        let pinned_paths: Vec<PathBuf> = ["Applications", "Desktop", "Documents", "Downloads"]
            .iter()
            .map(|sub| home.join(sub))
            .filter(|p| p.is_dir())
            .collect();

        let mut breadcrumb = BreadcrumbBar::new();
        breadcrumb.set_path(&home);

        let initial_tab = Tab {
            current_dir: home.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            filter_text: String::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
            error: None,
            history: Vec::new(),
            history_index: 0,
        };

        let mut a = Self {
            window: None,
            sb_context: None,
            surface: None,
            renderer: None,
            disk_usage_window: None,
            disk_usage_generation: 0,
            pending_disk_usage_open: None,
            fs,
            tabs: vec![initial_tab],
            active: 0,
            list: VirtualizedList::new(),
            scrollbar: Scrollbar::new(),
            splitter: Splitter::new(SIDEBAR_MIN, SIDEBAR_MAX),
            // Preview splitter min/max are recomputed per drag from the
            // current viewport — placeholder values here are overwritten
            // before any drag begins.
            preview_splitter: Splitter::new(0.0, 0.0),
            tabstrip: TabStrip::new(),
            breadcrumb,
            tree,
            focused_pane: FocusedPane::List,
            progress: ProgressStrip::new(),
            task_strip_token: None,
            tasks: TaskRegistry::new(),
            task_panel_open: false,
            magic_task: None,
            splitter_x: prefs
                .sidebar_width
                .map(|w| w.clamp(SIDEBAR_MIN, SIDEBAR_MAX))
                .unwrap_or(SIDEBAR_DEFAULT),
            preview_width: PREVIEW_W_DEFAULT,
            show_hidden: prefs.show_hidden.unwrap_or(false),
            ant_trail: AntTrail::new(),
            pinned_paths,
            properties_target: None,
            dialog: None,
            inline_rename: None,
            inline_rename_rect: None,
            toasts: ToastStack::new(),
            search: None,
            shortcuts_modal: None,
            settings_modal: None,
            preview_visible: false,
            icon_cache: HashMap::new(),
            magic_cache: HashMap::new(),
            quarantine_cache: HashMap::new(),
            quarantine_generation: 0,
            quarantine_task: None,
            metadata_db: None,
            preview_cache: HashMap::new(),
            preview_text_cache: HashMap::new(),
            preview_generation: 0,
            preview_pending: std::collections::HashSet::new(),
            preview_failed: std::collections::HashSet::new(),
            event_proxy: None,
            magic_generation: 0,
            icon_queue: Vec::new(),
            icon_generation: 0,
            icon_task: None,
            icon_size_px: 16,
            enumeration_generation: 0,
            enumeration_cancel: None,
            enumeration_task: None,
            enumeration_preserve_cursor: None,
            enumeration_preserve_scroll: 0.0,
            enumeration_pending_first_batch: false,
            tree_load_generation: 0,
            tree_pending: HashMap::new(),
            drag_watch: None,
            pointer_dips: None,
            modifiers: ModifiersState::empty(),
            tokens: Tokens::light(), // overwritten below by apply_theme
            theme_preference: initial_theme_preference(&prefs),
            ui_scale: initial_ui_scale(),
            system_is_dark: feraille_shell_mac::system_is_dark(),
            width: 1,
            height: 1,
            scale_factor: 1.0,
        };
        a.apply_theme();
        a.rebuild_tree_sections();
        a.navigate(home);
        a
    }

    /// Rebuild the tree's Recents/Favorites/Locations/Volumes sections
    /// from current state (`ant_trail` for Recents, `pinned_paths` for
    /// Favorites, fixed Mac roots for Locations, `/Volumes/*` for
    /// Volumes). Cheap (~5 path lookups + a few `is_dir` checks); call
    /// after navigate / pin / volume change.
    fn rebuild_tree_sections(&mut self) {
        let home = home_dir();
        let mut sections: Vec<(Section, Vec<(feraille_core::NodeId, String)>)> = Vec::new();
        // Collected here so we can attach capacity to the relevant
        // tree nodes after `set_sections` runs (which clears prior
        // capacity to handle remount/eject cleanly).
        let mut capacities: Vec<(feraille_core::NodeId, feraille_controls::NodeCapacity)> =
            Vec::new();
        // Paths whose tree-row icons should be the real per-path Finder
        // icon (Macintosh HD, Home, iCloud, Trash, USB / external SSD /
        // network glyphs, custom .VolumeIcon.icns) rather than the shared
        // generic folder bitmap. Drained at the bottom into the icon
        // prefetcher.
        let mut tree_icon_paths: Vec<PathBuf> = Vec::new();

        // 1. Recents — top folders by ant-trail visits, sorted A→Z for
        //    stable slot positions. The selection is by visit count, but
        //    the display order is alphabetical so entries don't jump
        //    around as visit counts shift.
        let mut recent_labels = Vec::new();
        for id in self.ant_trail.most_visited(5) {
            if let Some(path) = self.fs.path_for(id) {
                if !path.is_dir() {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                recent_labels.push((id, name));
            }
        }
        recent_labels.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        let recent_entries: Vec<NodeId> = recent_labels.iter().map(|(id, _)| *id).collect();
        if !recent_entries.is_empty() {
            sections.push((
                Section::new(SectionKind::Recents, Some("RECENTS"), recent_entries),
                recent_labels,
            ));
        }

        // 2. Favorites — pinned paths.
        if !self.pinned_paths.is_empty() {
            let mut entries = Vec::new();
            let mut labels = Vec::new();
            for path in &self.pinned_paths {
                let id = self.fs.id_for_path(path);
                let label = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or(""))
                    .to_string();
                entries.push(id);
                labels.push((id, label));
            }
            sections.push((
                Section::new(SectionKind::Favorites, Some("FAVORITES"), entries),
                labels,
            ));
        }

        // 3. Locations — iCloud Drive (if present), Home, boot volume, Trash.
        let mut entries = Vec::new();
        let mut labels = Vec::new();
        let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        if icloud.is_dir() {
            let id = self.fs.id_for_path(&icloud);
            entries.push(id);
            labels.push((id, "iCloud Drive".to_string()));
            tree_icon_paths.push(icloud);
        }
        let home_id = self.fs.id_for_path(&home);
        let home_label = home
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Home")
            .to_string();
        entries.push(home_id);
        labels.push((home_id, home_label));
        tree_icon_paths.push(home.clone());
        let root = PathBuf::from("/");
        let root_id = self.fs.id_for_path(&root);
        entries.push(root_id);
        tree_icon_paths.push(root.clone());
        // Real volume name from NSURL (cached, doesn't wake disks).
        // Falls back to the conventional default if the lookup fails.
        let root_info = volume_info_for_path(&root);
        let root_label = root_info
            .as_ref()
            .map(|info| info.name.clone())
            .unwrap_or_else(|| "Macintosh HD".to_string());
        labels.push((root_id, root_label));
        if let Some(info) = root_info.as_ref() {
            if let (Some(total), Some(available)) = (info.total_bytes, info.available_bytes) {
                if total > 0 {
                    capacities.push((
                        root_id,
                        feraille_controls::NodeCapacity { total, available },
                    ));
                }
            }
        }
        let trash = home.join(".Trash");
        if trash.is_dir() {
            let id = self.fs.id_for_path(&trash);
            entries.push(id);
            labels.push((id, "Trash".to_string()));
            tree_icon_paths.push(trash);
        }
        sections.push((
            Section::new(SectionKind::Locations, Some("LOCATIONS"), entries),
            labels,
        ));

        // 4. Volumes — non-boot mounts under /Volumes. /Volumes also
        //    contains a firmlink for the boot volume (`/Volumes/<boot>`)
        //    that NSURL resolves to the same VolumeInfo as `/`; filter
        //    it by matching the boot volume's name.
        let boot_name = root_info.as_ref().map(|info| info.name.clone());
        let volumes: Vec<feraille_fs_native::VolumeInfo> = list_volumes()
            .into_iter()
            .filter(|info| match &boot_name {
                Some(b) => &info.name != b,
                None => info.name != "Macintosh HD",
            })
            .collect();
        if !volumes.is_empty() {
            let mut entries = Vec::new();
            let mut labels = Vec::new();
            for info in volumes {
                let id = self.fs.id_for_path(&info.path);
                entries.push(id);
                if let (Some(total), Some(available)) = (info.total_bytes, info.available_bytes) {
                    if total > 0 {
                        capacities.push((id, feraille_controls::NodeCapacity { total, available }));
                    }
                }
                tree_icon_paths.push(info.path);
                labels.push((id, info.name));
            }
            sections.push((
                Section::new(SectionKind::Volumes, Some("VOLUMES"), entries),
                labels,
            ));
        }

        self.tree.set_sections(sections);
        for (id, cap) in capacities {
            self.tree.set_node_capacity(id, Some(cap));
        }

        // Schedule per-path icon fetches for Volumes + Locations rows.
        // The chunked main-thread prefetcher resolves these via
        // NSWorkspace.iconForFile:; until each lands the row falls back
        // to the cached `"DIR"` bitmap (see paint closure in `render`).
        let icon_items: Vec<(String, PathBuf)> = tree_icon_paths
            .into_iter()
            .map(|p| (path_icon_key(&p), p))
            .collect();
        self.enqueue_icon_fetches(icon_items);
    }

    fn viewport_size_dips(&self) -> (f32, f32) {
        (
            (self.width as f32) / self.scale_factor,
            (self.height as f32) / self.scale_factor,
        )
    }

    fn body_top(&self) -> f32 {
        self.tokens.layout.tabstrip
    }

    fn tabstrip_rect(&self) -> FRect {
        let (w, _) = self.viewport_size_dips();
        FRect::new(0.0, 0.0, w, self.tokens.layout.tabstrip)
    }

    fn tree_rect(&self) -> FRect {
        let (_, h) = self.viewport_size_dips();
        FRect::new(
            0.0,
            self.body_top(),
            self.splitter_x,
            (h - self.body_top() - self.tokens.layout.status_bar).max(0.0),
        )
    }

    fn breadcrumb_rect(&self) -> FRect {
        let (w, _) = self.viewport_size_dips();
        FRect::new(
            self.splitter_x,
            self.body_top(),
            (w - self.splitter_x).max(0.0),
            self.tokens.layout.breadcrumb,
        )
    }

    /// Effective preview width given the current viewport: clamped so
    /// the file pane never collapses below ~200 DIPs and the preview
    /// itself respects [`PREVIEW_W_MIN`] / [`PREVIEW_W_MAX`].
    fn effective_preview_width(&self) -> f32 {
        if !self.preview_visible {
            return 0.0;
        }
        let (w, _) = self.viewport_size_dips();
        let available = (w - self.splitter_x).max(0.0);
        let cap_by_pane = (available - 200.0).max(PREVIEW_W_MIN);
        self.preview_width
            .clamp(PREVIEW_W_MIN, PREVIEW_W_MAX.min(cap_by_pane))
    }

    fn list_pane_rect(&self) -> FRect {
        let (w, h) = self.viewport_size_dips();
        let preview_w = self.effective_preview_width();
        FRect::new(
            self.splitter_x,
            self.body_top() + self.tokens.layout.breadcrumb,
            (w - self.splitter_x - preview_w).max(0.0),
            (h - self.body_top() - self.tokens.layout.breadcrumb - self.tokens.layout.status_bar).max(0.0),
        )
    }

    fn preview_rect(&self) -> FRect {
        if !self.preview_visible {
            return FRect::new(0.0, 0.0, 0.0, 0.0);
        }
        let (w, h) = self.viewport_size_dips();
        let preview_w = self.effective_preview_width();
        FRect::new(
            w - preview_w,
            self.body_top() + self.tokens.layout.breadcrumb,
            preview_w,
            (h - self.body_top() - self.tokens.layout.breadcrumb - self.tokens.layout.status_bar).max(0.0),
        )
    }

    /// Absolute x in DIPs of the preview-pane splitter (the line
    /// between file pane and preview). Returns `None` when the preview
    /// pane is hidden.
    fn preview_splitter_x(&self) -> Option<f32> {
        if !self.preview_visible {
            return None;
        }
        let (w, _) = self.viewport_size_dips();
        Some(w - self.effective_preview_width())
    }

    fn header_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        FRect::new(
            pane.left(),
            pane.top(),
            pane.size.width,
            self.list.header_height(),
        )
    }

    fn list_inner_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        let header_h = self.list.header_height();
        FRect::new(
            pane.left(),
            pane.top() + header_h,
            (pane.size.width - SCROLLBAR_W).max(0.0),
            (pane.size.height - header_h).max(0.0),
        )
    }

    fn scrollbar_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        let header_h = self.list.header_height();
        FRect::new(
            pane.right() - SCROLLBAR_W,
            pane.top() + header_h,
            SCROLLBAR_W,
            (pane.size.height - header_h).max(0.0),
        )
    }

    fn splitter_container(&self) -> FRect {
        let (_, h) = self.viewport_size_dips();
        FRect::new(
            0.0,
            self.body_top(),
            self.viewport_size_dips().0,
            (h - self.body_top() - self.tokens.layout.status_bar).max(0.0),
        )
    }

    fn list_content_height(&self) -> f32 {
        self.tabs[self.active].entries.len() as f32 * self.list.row_height
    }

    fn rebuild_visible_entries(&mut self, cursor_name: Option<String>, preserve_scroll: bool) {
        let key = self.list.sort;
        let scroll = self.list.scroll_offset;
        let tab = &mut self.tabs[self.active];
        tab.entries = filter_entries(&tab.all_entries, &tab.filter_text);
        sort_entries(&mut tab.entries, key);
        tab.selection = Selection::new();
        if let Some(name) = cursor_name {
            if let Some(idx) = tab.entries.iter().position(|e| e.name == name) {
                tab.selection.set_cursor(idx);
            } else if !tab.entries.is_empty() {
                tab.selection.set_cursor(0);
            }
        } else if !tab.entries.is_empty() {
            tab.selection.set_cursor(0);
        }
        self.list.scroll_offset = if preserve_scroll { scroll } else { 0.0 };
    }

    pub fn navigate(&mut self, path: PathBuf) {
        // Truncate forward stack and push, unless we're already on the path.
        {
            let tab = &mut self.tabs[self.active];
            if tab.history_index + 1 < tab.history.len() {
                tab.history.truncate(tab.history_index + 1);
            }
            if tab.history.last() != Some(&path) {
                tab.history.push(path.clone());
                tab.history_index = tab.history.len().saturating_sub(1);
            }
        }
        self.goto_path(path.clone());
        // If the DU window is open and following navigation, re-root
        // its scan to the new path.
        self.maybe_follow_disk_usage_navigation(&path);
    }

    /// Single dispatcher for every Feraille-owned command. Both the
    /// menu bar (via `AppEvent::Command`) and the keyboard handler
    /// route here, so adding / renaming / rebinding a shortcut never
    /// needs to update two parallel matches. Unknown ids log and
    /// no-op — useful for catching catalogue/dispatch drift.
    pub fn dispatch_command(&mut self, id: CommandId) {
        log_info!(60, "command: {:?}", id);
        match id.0 {
            "app.about" => feraille_shell_mac::show_about_panel(),
            "app.settings" => self.show_settings(),
            "file.new_tab" => self.new_tab_at(home_dir()),
            "file.close_tab" => self.close_active_tab(),
            "file.new_folder" => self.open_new_folder(),
            "file.get_info" => self.toggle_properties(),
            "file.move_to_trash" => self.delete_at_cursor_to_trash(),
            "file.copy_path" => self.copy_cursor_path(),
            "file.reveal_in_finder" => self.reveal_cursor_in_finder(),
            "file.refresh" => self.refresh_active_tab(),
            "view.search" => self.open_search(),
            "view.edit_breadcrumb" => self.open_breadcrumb_edit(),
            "view.toggle_preview" => self.toggle_preview(),
            "view.toggle_hidden" => self.toggle_hidden(),
            "view.theme_light" => self.set_theme_preference(ThemePreference::Light),
            "view.theme_dark" => self.set_theme_preference(ThemePreference::Dark),
            "view.theme_system" => self.set_theme_preference(ThemePreference::System),
            "view.cycle_focus" => self.cycle_focus(),
            "view.zoom_in" => self.nudge_ui_scale(Self::UI_SCALE_STEP),
            "view.zoom_out" => self.nudge_ui_scale(-Self::UI_SCALE_STEP),
            "view.zoom_reset" => self.reset_ui_scale(),
            "view.disk_usage" => self.open_or_focus_disk_usage(),
            "disk_usage.refresh" => self.refresh_disk_usage(),
            "disk_usage.zoom_out" => self.disk_usage_zoom_out(),
            "disk_usage.toggle_topn" => self.disk_usage_toggle_topn(),
            "disk_usage.toggle_packages" => self.disk_usage_toggle_packages(),
            "disk_usage.toggle_follow_navigation" => self.disk_usage_toggle_follow_navigation(),
            "disk_usage.coloring_category" => {
                self.disk_usage_set_coloring(feraille_controls::TreemapColoring::Category)
            }
            "disk_usage.coloring_age" => {
                self.disk_usage_set_coloring(feraille_controls::TreemapColoring::AgeHeat)
            }
            "disk_usage.coloring_depth" => {
                self.disk_usage_set_coloring(feraille_controls::TreemapColoring::DepthOnly)
            }
            "disk_usage.size_apparent" => {
                self.disk_usage_set_size_mode(feraille_disk_usage::SizeMode::Apparent)
            }
            "disk_usage.size_allocated" => {
                self.disk_usage_set_size_mode(feraille_disk_usage::SizeMode::Allocated)
            }
            "go.back" => self.navigate_back(),
            "go.forward" => self.navigate_forward(),
            "go.parent" => self.navigate_parent(),
            "go.home" => self.navigate(home_dir()),
            // Selection — pane-aware. The handler routes each command
            // to whichever pane currently owns focus. Bare arrow keys /
            // Home / End / PageUp / PageDown / Enter / F2 / Escape only
            // reach this dispatch when no modal text input is active —
            // the keyboard handler intercepts those first.
            "selection.cursor_up" => self.cursor_in_focused_pane(-1),
            "selection.cursor_down" => self.cursor_in_focused_pane(1),
            "selection.cursor_first" => self.cursor_to_edge_in_focused_pane(true),
            "selection.cursor_last" => self.cursor_to_edge_in_focused_pane(false),
            "selection.page_up" => self.cursor_page_in_focused_pane(true),
            "selection.page_down" => self.cursor_page_in_focused_pane(false),
            "selection.activate" => self.activate_in_focused_pane(),
            "selection.start_rename" => match self.focused_pane {
                FocusedPane::List => self.start_inline_rename(),
                FocusedPane::Tree => self.open_rename(),
            },
            "selection.collapse_or_parent" => {
                if matches!(self.focused_pane, FocusedPane::Tree) {
                    let h = self.tree_rect().size.height;
                    self.tree.collapse_or_parent(h);
                }
            }
            "selection.expand_or_first_child" => {
                if matches!(self.focused_pane, FocusedPane::Tree) {
                    let h = self.tree_rect().size.height;
                    if let Some(ev) = self.tree.expand_or_first_child(h) {
                        self.handle_tree_event(ev);
                    }
                }
            }
            "selection.dismiss" => {
                if matches!(self.focused_pane, FocusedPane::Tree) {
                    self.set_focused_pane(FocusedPane::List);
                } else if self.properties_target.is_some() {
                    self.close_properties();
                }
            }
            "window.next_tab" => self.next_tab(),
            "window.prev_tab" => self.prev_tab(),
            "help.github" => feraille_shell_mac::open_url(PROJECT_URL),
            "help.shortcuts" => self.show_help_shortcuts(),
            other => log_warn!(60, "unknown command id: {:?}", other),
        }
        self.request_redraw();
    }

    /// Step the cursor by ±1 in whichever pane has focus. Helper for
    /// `selection.cursor_up` / `selection.cursor_down` dispatch.
    fn cursor_in_focused_pane(&mut self, delta: i64) {
        match self.focused_pane {
            FocusedPane::List => {
                let count = self.tabs[self.active].entries.len();
                let viewport_h = self.list_inner_rect().size.height;
                let sel = &mut self.tabs[self.active].selection;
                sel.move_cursor(delta, count);
                if let Some(idx) = self.tabs[self.active].selection.cursor() {
                    self.list.ensure_visible(idx, viewport_h);
                }
            }
            FocusedPane::Tree => {
                let h = self.tree_rect().size.height;
                self.tree.move_cursor(delta as i32, h);
            }
        }
    }

    /// Jump to first / last entry in the focused pane.
    fn cursor_to_edge_in_focused_pane(&mut self, first: bool) {
        match self.focused_pane {
            FocusedPane::List => {
                let count = self.tabs[self.active].entries.len();
                let viewport_h = self.list_inner_rect().size.height;
                let sel = &mut self.tabs[self.active].selection;
                if first {
                    sel.move_cursor(-(count as i64), count);
                } else {
                    sel.move_cursor(count as i64, count);
                }
                if let Some(idx) = self.tabs[self.active].selection.cursor() {
                    self.list.ensure_visible(idx, viewport_h);
                }
            }
            FocusedPane::Tree => {
                let h = self.tree_rect().size.height;
                if first {
                    self.tree.move_to_first(h);
                } else {
                    self.tree.move_to_last(h);
                }
            }
        }
    }

    /// PageUp / PageDown in the focused pane. Page size derived from
    /// the pane's viewport / row height.
    fn cursor_page_in_focused_pane(&mut self, up: bool) {
        match self.focused_pane {
            FocusedPane::List => {
                let count = self.tabs[self.active].entries.len();
                let viewport_h = self.list_inner_rect().size.height;
                let page = (viewport_h / self.list.row_height) as i64;
                let sel = &mut self.tabs[self.active].selection;
                sel.move_cursor(if up { -page } else { page }, count);
                if let Some(idx) = self.tabs[self.active].selection.cursor() {
                    self.list.ensure_visible(idx, viewport_h);
                }
            }
            FocusedPane::Tree => {
                let h = self.tree_rect().size.height;
                let page = (h / self.tokens.layout.tree_row).max(1.0) as i32;
                self.tree.move_cursor(if up { -page } else { page }, h);
            }
        }
    }

    /// Enter on the focused pane.
    fn activate_in_focused_pane(&mut self) {
        match self.focused_pane {
            FocusedPane::List => self.open_at_cursor(),
            FocusedPane::Tree => {
                if let Some(ev) = self.tree.activate_selected() {
                    self.handle_tree_event(ev);
                }
            }
        }
    }

    /// Move the active tab's `history_index` backward and re-enumerate.
    pub fn navigate_back(&mut self) {
        let path = {
            let tab = &mut self.tabs[self.active];
            if tab.history_index == 0 {
                return;
            }
            tab.history_index -= 1;
            tab.history[tab.history_index].clone()
        };
        self.goto_path(path);
    }

    /// Move the active tab's `history_index` forward and re-enumerate.
    pub fn navigate_forward(&mut self) {
        let path = {
            let tab = &mut self.tabs[self.active];
            if tab.history_index + 1 >= tab.history.len() {
                return;
            }
            tab.history_index += 1;
            tab.history[tab.history_index].clone()
        };
        self.goto_path(path);
    }

    /// Internal: re-enumerate `path` and update view state. Does NOT
    /// touch the history vector; callers manage that.
    fn goto_path(&mut self, path: PathBuf) {
        // Same-folder no-op: clicking the current tree node, hitting
        // Enter on it, or back/forward landing on the same path used to
        // clear all_entries and re-enumerate, producing a visible
        // empty→full flicker. Skip when we're already here AND we
        // actually have the listing in hand.
        //
        // The `all_entries.is_empty()` clause matters at startup:
        // `App::new` constructs the initial tab with `current_dir =
        // home` *before* the first navigate, so without this guard the
        // very first `navigate(home)` would short-circuit, leaving
        // entries — and the icon cache that paint reads from — empty
        // until the user moved off and back.
        //
        // An error state is a signal the user wants to retry; F5
        // explicitly refreshes regardless.
        if self.tabs[self.active].current_dir == path
            && self.tabs[self.active].error.is_none()
            && !self.tabs[self.active].all_entries.is_empty()
        {
            return;
        }
        let id = self.fs.id_for_path(&path);
        self.ant_trail.record(id);
        // Persist the visit. Best-effort — a write failure is logged
        // and dropped; the in-memory trail is still correct.
        if let Some(db) = self.metadata_db.as_ref() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if let Some(p) = path.to_str() {
                if let Err(e) = db.record_folder_visit(p, now) {
                    log_warn!(60, "metadata: record_folder_visit({p}) failed: {e}");
                }
            }
        }
        // Rebuild sections so Recents reflects the latest visit.
        self.rebuild_tree_sections();
        // Hold the previous folder's `all_entries` visible until the
        // first `EnumerationBatch` arrives (or `EnumerationDone` if the
        // listing is empty/errors). Clearing here would paint an empty
        // pane for one frame on slow filesystems; the first-batch swap
        // is atomic. Filter, scroll, and current_dir reset immediately:
        // breadcrumb shows the destination, the held rows just stand in
        // for content for the brief interval before they're replaced.
        let tab = &mut self.tabs[self.active];
        tab.filter_text.clear();
        tab.current_dir = path.clone();
        tab.list_scroll = 0.0;
        self.list.scroll_offset = 0.0;
        self.rebuild_visible_entries(None, false);
        self.breadcrumb.set_path(&path);
        self.reveal_in_tree(&path);
        self.sync_window_title();
        self.start_enumeration(path, None, 0.0);
        // prefetch_icons + start_magic_prefetch fire after the
        // enumeration completes, in `AppEvent::EnumerationDone`. Calling
        // them here would no-op (entries are still empty).
    }

    /// Enumerate children for a tree-pane node off the main thread.
    /// Idempotent on `id`: a duplicate spawn while a prior load is
    /// still pending is a no-op. Headless callers (no event proxy)
    /// fall back to the synchronous path so screenshot output keeps
    /// the expanded ancestors visible.
    fn spawn_tree_load(&mut self, id: NodeId) {
        if self.tree_pending.contains_key(&id) || self.tree.is_loaded(id) {
            return;
        }
        let Some(proxy) = self.event_proxy.clone() else {
            // Headless — synchronous fallback. Mirrors what the
            // `TreeChildrenLoaded` handler does in the live path.
            let mut handle = self.fs.enumerate(id);
            filter_hidden(&mut handle.initial, self.show_hidden);
            self.tree.populate_children(id, &handle.initial);
            return;
        };
        self.tree_load_generation = self.tree_load_generation.wrapping_add(1);
        let generation = self.tree_load_generation;
        self.tree_pending.insert(id, generation);
        let fs = self.fs.clone();
        obs::spawn_logged("tree-load", move || {
            let handle = fs.enumerate(id);
            let _ = proxy.send_event(AppEvent::TreeChildrenLoaded {
                generation,
                id,
                entries: handle.initial,
                error: handle.error,
            });
        });
    }

    /// Drop the tree's cached children for `id` and cancel any
    /// in-flight load (a stale `TreeChildrenLoaded` will fail the
    /// generation gate and be dropped). Use this instead of calling
    /// `tree.invalidate` directly so the cancellation half stays in
    /// step.
    fn invalidate_tree(&mut self, id: NodeId) {
        self.tree_pending.remove(&id);
        self.tree.invalidate(id);
    }

    /// Register a new background task and ensure the status-bar strip
    /// is visible. The strip stays visible while the registry is
    /// non-empty — so two overlapping tasks share one strip rather than
    /// the second one stealing the visual from the first.
    fn begin_task(
        &mut self,
        kind: TaskKind,
        label: impl Into<String>,
        cancellable: bool,
    ) -> TaskId {
        let id = self.tasks.begin(kind, label, cancellable);
        if self.task_strip_token.is_none() {
            self.task_strip_token = Some(self.progress.start_indeterminate());
        }
        id
    }

    /// End a task. Stale ids are silently ignored. When the registry
    /// empties, the shared strip is completed (with fade) and the task
    /// panel auto-closes.
    fn end_task(&mut self, id: TaskId) {
        self.tasks.end(id);
        if self.tasks.is_empty() {
            if let Some(token) = self.task_strip_token.take() {
                self.progress.complete(token);
            }
            self.task_panel_open = false;
        }
    }

    /// User-initiated cancel from the task panel's `[×]` button. Routes
    /// to the per-kind cancel mechanism, then ends the task. The
    /// worker's eventual completion event still arrives, but its
    /// `end_task` call is a no-op via the registry's stale-id rule.
    fn cancel_task(&mut self, id: TaskId) {
        let Some(task) = self.tasks.find(id) else {
            return;
        };
        let kind = task.kind;
        match kind {
            TaskKind::Enumeration => {
                if let Some(flag) = self.enumeration_cancel.take() {
                    flag.store(true, Ordering::Relaxed);
                }
                self.enumeration_task = None;
            }
            TaskKind::IconPrefetch => {
                // Bump the generation to drop any in-flight chunk-tick
                // at the gate; clear the queue so no further work is
                // scheduled.
                self.icon_generation = self.icon_generation.wrapping_add(1);
                self.icon_queue.clear();
                self.icon_task = None;
            }
            TaskKind::MagicPrefetch => {
                // Not cancellable in v1; the panel hides the button for
                // this kind, so this branch is unreachable. Defensive
                // fallthrough drops the registry entry only.
            }
            TaskKind::QuarantinePrefetch => {
                // Same shape as MagicPrefetch — not cancellable in v1.
            }
            TaskKind::DiskUsage => {
                if let Some(du) = self.disk_usage_window.as_mut() {
                    du.state.cancel.store(true, Ordering::Relaxed);
                    du.state.task_id = None;
                }
            }
            TaskKind::FileOp => {
                // Not cancellable in v1: by the time the user clicks
                // cancel, the underlying syscall (`std::fs::copy`,
                // `ditto` subprocess) is mid-flight without a stable
                // interruption point. The task panel hides the
                // button for this kind, so this branch is defensive.
            }
        }
        self.end_task(id);
        self.request_redraw();
    }

    /// Status-bar rect in DIPs. Used for hit-testing clicks that
    /// toggle the task panel.
    fn status_bar_rect(&self) -> FRect {
        let (w, h) = self.viewport_size_dips();
        FRect::new(
            0.0,
            h - self.tokens.layout.status_bar,
            w,
            self.tokens.layout.status_bar,
        )
    }

    /// Spawn a worker that streams the active tab's directory listing
    /// in batches of `DEFAULT_ENUMERATION_BATCH`. Cancels any prior
    /// in-flight enumeration. The `preserve_cursor` and `preserve_scroll`
    /// arguments survive the refresh: F5 sets them so the user's cursor
    /// and scroll position stay stable across batches; navigation
    /// passes `None`/`0.0` so the new listing presents at the top.
    ///
    /// Headless callers (no event proxy, e.g. screenshot CLI) fall
    /// back to the eager path so generated images contain rows.
    fn start_enumeration(
        &mut self,
        path: PathBuf,
        preserve_cursor: Option<String>,
        preserve_scroll: f32,
    ) {
        let id = self.fs.id_for_path(&path);

        let Some(proxy) = self.event_proxy.clone() else {
            // Headless — synchronous fallback so screenshots are stable.
            // Mirrors what AppEvent::EnumerationDone does in the live
            // path: populate, rebuild visible, kick the prefetches.
            let mut handle = self.fs.enumerate(id);
            filter_hidden(&mut handle.initial, self.show_hidden);
            let tab = &mut self.tabs[self.active];
            tab.all_entries = handle.initial;
            tab.error = handle.error;
            self.rebuild_visible_entries(preserve_cursor, preserve_scroll > 0.0);
            if preserve_scroll > 0.0 {
                self.list.scroll_offset = preserve_scroll;
            }
            self.prefetch_icons();
            self.start_magic_prefetch();
            self.start_quarantine_prefetch();
            return;
        };

        // Cancel previous enumeration: flip its flag so the worker
        // exits at the next checkpoint, drop its registry entry.
        if let Some(prev) = self.enumeration_cancel.take() {
            prev.store(true, Ordering::Relaxed);
        }
        // Begin the new task before ending the old one so the strip never
        // momentarily empties (which would trigger a fade-out flicker).
        let new_task = self.begin_task(TaskKind::Enumeration, "Reading folder…", true);
        if let Some(prev) = self.enumeration_task.take() {
            self.end_task(prev);
        }

        self.enumeration_generation = self.enumeration_generation.wrapping_add(1);
        let generation = self.enumeration_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.enumeration_cancel = Some(cancel.clone());
        self.enumeration_task = Some(new_task);
        self.enumeration_preserve_cursor = preserve_cursor;
        self.enumeration_preserve_scroll = preserve_scroll;
        self.enumeration_pending_first_batch = true;

        log_info!(
            59,
            "enumeration: starting (gen={}, dir={})",
            generation,
            path.display()
        );

        let fs = self.fs.clone();
        let dir_for_worker = path.clone();
        let dir_for_done = path;
        obs::spawn_logged("enumerate", move || {
            let proxy_for_done = proxy.clone();
            let dir_for_batches = dir_for_worker.clone();
            let mut on_batch = |batch: Vec<FileEntry>| {
                let _ = proxy.send_event(AppEvent::EnumerationBatch {
                    generation,
                    dir: dir_for_batches.clone(),
                    entries: batch,
                });
            };
            let error = fs.enumerate_streaming(
                &dir_for_worker,
                DEFAULT_ENUMERATION_BATCH,
                &cancel,
                &mut on_batch,
            );
            let _ = proxy_for_done.send_event(AppEvent::EnumerationDone {
                generation,
                dir: dir_for_done,
                error,
            });
        });
    }

    /// Build a deduped queue of icon fetches for the active tab and
    /// dispatch via `enqueue_icon_fetches`.
    /// Open the persistent metadata DB at the default macOS path
    /// (`~/Library/Application Support/Feraille/metadata.db`),
    /// creating the parent directory if needed. Stashes it on the
    /// App and hydrates the in-memory Ant Trail from
    /// `folder_usage`. Best-effort: a DB-open failure logs and
    /// leaves `metadata_db = None`, in which case persistence is
    /// silently disabled (caches still work in-memory).
    fn open_metadata_db(&mut self) {
        let Some(path) = feraille_meta::default_db_path() else {
            log_warn!(60, "metadata: $HOME unset; persistence disabled");
            return;
        };
        if let Err(e) = feraille_meta::ensure_parent_dir(&path) {
            log_warn!(60, "metadata: mkdir failed for {}: {e}", path.display());
            return;
        }
        match feraille_meta::MetadataDb::open(&path) {
            Ok(db) => {
                log_info!(60, "metadata: opened {}", path.display());
                self.metadata_db = Some(db);
                self.hydrate_ant_trail_from_db();
                self.hydrate_layout_from_db();
                self.hydrate_tabs_from_db();
            }
            Err(e) => {
                log_warn!(60, "metadata: open failed for {}: {e}", path.display());
            }
        }
    }

    /// Restore sidebar / preview splitter widths + DU geometry from
    /// the DB. Window size restoration happens later (the winit
    /// window doesn't exist yet at App::new time); see
    /// `apply_persisted_window_size` called from `resumed`.
    fn hydrate_layout_from_db(&mut self) {
        let Some(db) = self.metadata_db.as_ref() else {
            return;
        };
        let layout = match db.load_layout_state() {
            Ok(Some(l)) => l,
            Ok(None) => return,
            Err(e) => {
                log_warn!(60, "metadata: load_layout_state failed: {e}");
                return;
            }
        };
        if layout.sidebar_width > 0 {
            self.splitter_x = (layout.sidebar_width as f32)
                .clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        }
        if layout.preview_width > 0 {
            self.preview_width = layout.preview_width as f32;
        }
        self.preview_visible = layout.preview_visible;
        log_info!(
            60,
            "metadata: restored layout (sidebar={}, preview={}, preview_visible={})",
            self.splitter_x,
            self.preview_width,
            self.preview_visible,
        );
    }

    /// Restore the persisted tab list. Each row's path is mapped to
    /// a `Tab` and the active row's index becomes `self.active`. If
    /// no rows persisted, we keep the home-folder default tab the
    /// constructor created.
    fn hydrate_tabs_from_db(&mut self) {
        let Some(db) = self.metadata_db.as_ref() else {
            return;
        };
        let rows = match db.load_tabs() {
            Ok(r) => r,
            Err(e) => {
                log_warn!(60, "metadata: load_tabs failed: {e}");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let mut new_tabs: Vec<Tab> = Vec::with_capacity(rows.len());
        let mut active_index: usize = 0;
        for (i, row) in rows.iter().enumerate() {
            let path = std::path::PathBuf::from(&row.path);
            new_tabs.push(Tab {
                current_dir: path.clone(),
                all_entries: Vec::new(),
                entries: Vec::new(),
                filter_text: String::new(),
                selection: feraille_controls::Selection::new(),
                list_scroll: row.scroll_offset.max(0.0),
                error: None,
                history: vec![path],
                history_index: 0,
            });
            if row.is_active {
                active_index = i;
            }
        }
        self.tabs = new_tabs;
        self.active = active_index.min(self.tabs.len().saturating_sub(1));
        log_info!(
            60,
            "metadata: restored {} tabs (active = {})",
            self.tabs.len(),
            self.active
        );
    }

    /// Window size live-tracking happens via `WindowEvent::Resized`;
    /// at startup we want to *apply* the saved size to the freshly
    /// created window. Called from `resumed` before the first paint.
    fn apply_persisted_window_size(&mut self) {
        let Some(db) = self.metadata_db.as_ref() else {
            return;
        };
        let Some(state) = db.load_window_state().ok().flatten() else {
            return;
        };
        if state.width <= 0 || state.height <= 0 {
            return;
        }
        if let Some(window) = self.window.as_ref() {
            let logical = winit::dpi::LogicalSize::new(
                state.width as f64,
                state.height as f64,
            );
            let _ = window.request_inner_size(logical);
        }
    }

    /// Snapshot window + layout + tab state to the DB. Called on
    /// `CloseRequested` so the next launch restores. Best-effort:
    /// any sub-call's failure is logged and dropped — the user's
    /// quit shouldn't block on a write error.
    fn save_persistent_state(&self) {
        let Some(db) = self.metadata_db.as_ref() else {
            return;
        };
        // Window: convert physical pixels → logical points. macOS
        // restores by logical size, so persist that shape.
        let logical_w = (self.width as f32 / self.scale_factor).round() as i32;
        let logical_h = (self.height as f32 / self.scale_factor).round() as i32;
        let win = feraille_meta::WindowState {
            width: logical_w.max(1),
            height: logical_h.max(1),
            maximized: false,
        };
        if let Err(e) = db.save_window_state(&win) {
            log_warn!(60, "metadata: save_window_state failed: {e}");
        }

        // Layout: sidebar + preview splitter widths and visibility.
        let layout = feraille_meta::LayoutState {
            sidebar_width: self.splitter_x.round() as i32,
            preview_width: self.preview_width.round() as i32,
            preview_visible: self.preview_visible,
            // DU geometry is iter-8.5 territory; leave at zero so
            // the existing du_window.txt path stays authoritative
            // until the migration commits.
            du_width: 0,
            du_height: 0,
            du_topn_width: 0,
        };
        if let Err(e) = db.save_layout_state(&layout) {
            log_warn!(60, "metadata: save_layout_state failed: {e}");
        }

        // Tabs.
        let tabs: Vec<feraille_meta::TabState> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| feraille_meta::TabState {
                path: t.current_dir.to_string_lossy().into_owned(),
                is_active: i == self.active,
                scroll_offset: t.list_scroll,
                selected_index: t.selection.cursor().map(|i| i as i32).unwrap_or(-1),
                sort_column: 0,
                sort_ascending: true,
            })
            .collect();
        if let Err(e) = db.save_tabs(&tabs) {
            log_warn!(60, "metadata: save_tabs failed: {e}");
        }
    }

    /// Read folder_usage rows out of the DB and rebuild the in-memory
    /// `AntTrail` by mapping each persisted path to a fresh NodeId
    /// via `self.fs.id_for_path`. Idempotent — calling it twice with
    /// no intervening writes produces the same in-memory state.
    fn hydrate_ant_trail_from_db(&mut self) {
        let Some(db) = self.metadata_db.as_ref() else {
            return;
        };
        let entries = match db.load_ant_trail() {
            Ok(e) => e,
            Err(e) => {
                log_warn!(60, "metadata: load_ant_trail failed: {e}");
                return;
            }
        };
        let mut max_hits: u32 = 0;
        for e in &entries {
            let id = self.fs.id_for_path(std::path::Path::new(&e.folder_path));
            for _ in 0..e.hits {
                self.ant_trail.record(id);
            }
            if e.hits > max_hits {
                max_hits = e.hits;
            }
        }
        log_info!(
            60,
            "metadata: hydrated ant_trail: {} folders, max hits = {}",
            entries.len(),
            max_hits
        );
    }

    /// Long edge of the inline preview thumbnail in physical pixels.
    /// Bigger = sharper at the cost of slower fetch. Tuned to match
    /// roughly what fits in the preview pane at 2x without scaling
    /// artifacts when the user resizes.
    const PREVIEW_THUMB_PX: u32 = 512;

    /// Ensure a Quick Look thumbnail is being (or has been) fetched
    /// for `path`/`mtime_unix`. No-op if cached or already in flight.
    /// Spawned off the UI thread; result returns through
    /// `AppEvent::PreviewThumbReady`.
    fn ensure_preview_thumb(&mut self, path: PathBuf, mtime_unix: i64) {
        let size = Self::PREVIEW_THUMB_PX;
        let key = (path.clone(), mtime_unix, size);
        if self.preview_cache.contains_key(&key)
            || self.preview_pending.contains(&key)
            || self.preview_failed.contains(&key)
        {
            return;
        }
        let Some(proxy) = self.event_proxy.clone() else {
            // Headless — no event loop to drive a worker thread.
            // Run synchronously so the screenshot harness still
            // shows the real preview. Same pattern as the
            // streaming-enumeration headless fallback.
            match feraille_shell_mac::fetch_quick_look_thumbnail(&path, size) {
                Some((rgba, w, h)) => {
                    self.preview_cache.insert(key, Bitmap::new(w, h, rgba));
                }
                None => {
                    self.preview_failed.insert(key);
                }
            }
            return;
        };
        self.preview_pending.insert(key.clone());
        let generation = self.preview_generation;
        let size_px = size;
        obs::spawn_logged("preview-thumb", move || {
            match feraille_shell_mac::fetch_quick_look_thumbnail(&path, size_px) {
                Some((rgba, w, h)) => {
                    let _ = proxy.send_event(AppEvent::PreviewThumbReady {
                        generation,
                        path,
                        mtime_unix,
                        rgba,
                        width: w,
                        height: h,
                    });
                }
                None => {
                    let _ = proxy.send_event(AppEvent::PreviewThumbFailed {
                        generation,
                        path,
                        mtime_unix,
                        size_px,
                    });
                }
            }
        });
    }

    /// Resolve the current selection to a path + mtime and ensure a
    /// preview fetch is running for it. Driven from `paint_to` so we
    /// don't have to hook every selection-change site individually.
    fn maybe_kick_preview_thumb(&mut self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        // Skip directories — qlmanage produces a generic folder
        // thumbnail, which the existing icon already shows.
        if matches!(entry.kind, feraille_core::EntryKind::Directory) {
            return;
        }
        let path = tab.current_dir.join(&entry.name);
        let mtime = entry.mtime_unix;

        // Text files: read the head inline. Quick Look would render
        // the contents anyway, and `qlmanage -t` only emits an
        // icon-sized text thumbnail; reading the bytes ourselves
        // produces a real, readable preview.
        if is_text_extension(&path) {
            let key = (path.clone(), mtime);
            if !self.preview_text_cache.contains_key(&key) {
                if let Some(snippet) = read_text_preview(&path) {
                    self.preview_text_cache.insert(key, snippet);
                }
            }
            return;
        }

        self.ensure_preview_thumb(path, mtime);
    }

    fn prefetch_icons(&mut self) {
        let cur_dir = self.tabs[self.active].current_dir.clone();

        // NSWorkspace returns one icon per UTI, so any file with a given
        // extension represents the whole bucket. Dedup by cache key so we
        // don't queue 100 `.rs` paths for the same icon.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut items: Vec<(String, PathBuf)> = Vec::new();
        for entry in &self.tabs[self.active].entries {
            let key = cache_key_for(entry);
            if !seen.insert(key.clone()) {
                continue;
            }
            items.push((key, cur_dir.join(&entry.name)));
        }

        self.enqueue_icon_fetches(items);
    }

    /// Append `(key, path)` icon-fetch jobs to the prefetch queue, skipping
    /// keys already cached or already queued. Posts the first
    /// `IconChunkTick` if the queue was idle. The handler drains
    /// `ICON_CHUNK_SIZE` items per tick on the main thread — NSWorkspace
    /// is main-thread-only — and re-posts until the queue is empty,
    /// yielding to input/paint between chunks.
    ///
    /// Headless callers (no event proxy yet, e.g. screenshot CLI) fall
    /// back to a synchronous drain so generated images still have icons.
    fn enqueue_icon_fetches(&mut self, items: impl IntoIterator<Item = (String, PathBuf)>) {
        let icon_size_px = (16.0 * self.scale_factor).round().max(16.0) as u32;

        // Dedup against both the cache and any items already queued from
        // a prior call (e.g. tree-icon enqueue followed by list prefetch).
        let mut seen: std::collections::HashSet<String> =
            self.icon_queue.iter().map(|(k, _)| k.clone()).collect();
        let mut to_append: Vec<(String, PathBuf)> = Vec::new();
        for (key, path) in items {
            if self.icon_cache.contains_key(&key) || !seen.insert(key.clone()) {
                continue;
            }
            to_append.push((key, path));
        }

        if to_append.is_empty() {
            return;
        }

        let Some(proxy) = self.event_proxy.clone() else {
            // Headless: no event loop to schedule against. Run synchronously
            // so screenshot output is correct.
            for (key, path) in to_append {
                if let Some((rgba, w, h)) = fetch_icon_rgba(&path, icon_size_px) {
                    self.icon_cache.insert(key, Bitmap::new(w, h, rgba));
                }
            }
            return;
        };

        let was_idle = self.icon_queue.is_empty();
        let added = to_append.len();
        self.icon_queue.extend(to_append);
        self.icon_size_px = icon_size_px;

        log_info!(
            56,
            "icon prefetch: +{} keys (queue={}, chunk={}, size={}px)",
            added,
            self.icon_queue.len(),
            ICON_CHUNK_SIZE,
            icon_size_px
        );

        if was_idle {
            self.icon_generation = self.icon_generation.wrapping_add(1);
            let new_task = self.begin_task(TaskKind::IconPrefetch, "Loading icons…", true);
            if let Some(prev) = self.icon_task.take() {
                self.end_task(prev);
            }
            self.icon_task = Some(new_task);
            let _ = proxy.send_event(AppEvent::IconChunkTick {
                generation: self.icon_generation,
            });
        }
    }

    fn cursor_entry_name(&self) -> Option<String> {
        let tab = &self.tabs[self.active];
        tab.selection
            .cursor()
            .and_then(|i| tab.entries.get(i))
            .map(|e| e.name.clone())
    }

    fn start_magic_prefetch(&mut self) {
        const MAGIC_PREFETCH_CAP: usize = 200;

        let cur_dir = self.tabs[self.active].current_dir.clone();
        let cursor_name = self.cursor_entry_name();
        let scroll = self.list.scroll_offset;
        let mut changed = false;

        // Hydrate the in-memory magic_cache from the DB for any
        // entries we don't already have. Cheap: per-row lookup keyed
        // by the unique path index, ~10 µs each for cache hits.
        if let Some(db) = self.metadata_db.as_ref() {
            for entry in self.tabs[self.active].all_entries.iter() {
                if !matches!(entry.kind, EntryKind::File) {
                    continue;
                }
                let path = cur_dir.join(&entry.name);
                let key = (path.clone(), entry.mtime_unix);
                if self.magic_cache.contains_key(&key) {
                    continue;
                }
                let Some(p) = path.to_str() else { continue };
                if let Ok(Some(rec)) = db.get_file(p) {
                    if rec.mtime_unix == entry.mtime_unix {
                        if let Some(label) = rec.magic_label {
                            self.magic_cache.insert(key, label);
                        }
                    }
                }
            }
        }

        for entry in self.tabs[self.active].all_entries.iter_mut() {
            if !matches!(entry.kind, EntryKind::File) || !entry.display_magic.is_empty() {
                continue;
            }
            let key = (cur_dir.join(&entry.name), entry.mtime_unix);
            if let Some(cached) = self.magic_cache.get(&key) {
                entry.display_magic = cached.clone();
                changed = true;
            }
        }

        if changed {
            self.rebuild_visible_entries(cursor_name, true);
            self.list.scroll_offset = scroll;
        }

        let Some(proxy) = self.event_proxy.clone() else {
            return;
        };

        self.magic_generation = self.magic_generation.wrapping_add(1);
        let generation = self.magic_generation;
        let candidates: Vec<(String, i64, PathBuf)> = self.tabs[self.active]
            .all_entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::File))
            .filter(|entry| entry.display_magic.is_empty())
            .filter_map(|entry| {
                let path = cur_dir.join(&entry.name);
                if self
                    .magic_cache
                    .contains_key(&(path.clone(), entry.mtime_unix))
                {
                    None
                } else {
                    Some((entry.name.clone(), entry.mtime_unix, path))
                }
            })
            .take(MAGIC_PREFETCH_CAP)
            .collect();

        if candidates.is_empty() {
            return;
        }

        // If a previous prefetch is still in flight, end its registry
        // entry (its `MagicBatch` may still arrive but the generation
        // gate will drop it).
        let new_task = self.begin_task(TaskKind::MagicPrefetch, "Indexing files…", false);
        if let Some(prev) = self.magic_task.take() {
            self.end_task(prev);
        }
        self.magic_task = Some(new_task);

        log_info!(
            56,
            "magic prefetch: {} candidates (gen={})",
            candidates.len(),
            generation
        );

        obs::spawn_logged("magic-prefetch", move || {
            let results = candidates
                .into_iter()
                .map(|(name, mtime_unix, path)| MagicResult {
                    name,
                    mtime_unix,
                    label: detect_magic(&path).unwrap_or_default().to_string(),
                })
                .collect();
            let _ = proxy.send_event(AppEvent::MagicBatch {
                generation,
                dir: cur_dir,
                results,
            });
        });
    }

    /// Spawn a worker to read macOS quarantine + where-from xattrs for
    /// up-to-`QUARANTINE_PREFETCH_CAP` files in the active tab. Mirrors
    /// `start_magic_prefetch` exactly: cache by `(path, mtime)`, post a
    /// `QuarantineBatch` user event, gate stale results on `generation`.
    /// On non-macOS this is a no-op (the worker just returns empty info).
    fn start_quarantine_prefetch(&mut self) {
        const QUARANTINE_PREFETCH_CAP: usize = 200;

        let cur_dir = self.tabs[self.active].current_dir.clone();
        let cursor_name = self.cursor_entry_name();
        let scroll = self.list.scroll_offset;
        let mut changed = false;

        // Hydrate the in-memory quarantine_cache from the DB for any
        // entries we don't already have. Mirrors start_magic_prefetch.
        if let Some(db) = self.metadata_db.as_ref() {
            for entry in self.tabs[self.active].all_entries.iter() {
                if !matches!(entry.kind, EntryKind::File) {
                    continue;
                }
                let path = cur_dir.join(&entry.name);
                let key = (path.clone(), entry.mtime_unix);
                if self.quarantine_cache.contains_key(&key) {
                    continue;
                }
                let Some(p) = path.to_str() else { continue };
                if let Ok(Some(rec)) = db.get_file(p) {
                    if rec.mtime_unix != entry.mtime_unix {
                        continue;
                    }
                    let Some(quarantined) = rec.quarantined else {
                        continue;
                    };
                    let cached = if quarantined {
                        Some(QuarantineDetails {
                            agent: rec.quarantine_agent,
                            downloaded_iso: rec.quarantine_iso,
                            where_from: rec
                                .quarantine_where_from
                                .map(|s| {
                                    s.split('\n')
                                        .filter(|x| !x.is_empty())
                                        .map(|x| x.to_string())
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    } else {
                        None
                    };
                    self.quarantine_cache.insert(key, cached);
                }
            }
        }

        for entry in self.tabs[self.active].all_entries.iter_mut() {
            if !matches!(entry.kind, EntryKind::File) || entry.quarantine.is_some() {
                continue;
            }
            let key = (cur_dir.join(&entry.name), entry.mtime_unix);
            if let Some(cached) = self.quarantine_cache.get(&key) {
                match cached {
                    Some(details) => {
                        entry.is_quarantined = true;
                        entry.quarantine = Some(details.clone());
                    }
                    None => {
                        entry.is_quarantined = false;
                        entry.quarantine = Some(QuarantineDetails::default());
                    }
                }
                changed = true;
            }
        }

        if changed {
            self.rebuild_visible_entries(cursor_name, true);
            self.list.scroll_offset = scroll;
        }

        self.quarantine_generation = self.quarantine_generation.wrapping_add(1);
        let generation = self.quarantine_generation;
        let candidates: Vec<(String, i64, PathBuf)> = self.tabs[self.active]
            .all_entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::File))
            .filter(|entry| entry.quarantine.is_none())
            .filter_map(|entry| {
                let path = cur_dir.join(&entry.name);
                if self
                    .quarantine_cache
                    .contains_key(&(path.clone(), entry.mtime_unix))
                {
                    None
                } else {
                    Some((entry.name.clone(), entry.mtime_unix, path))
                }
            })
            .take(QUARANTINE_PREFETCH_CAP)
            .collect();

        if candidates.is_empty() {
            return;
        }

        // Headless / screenshot path: no event proxy means there's no
        // event loop to post a `QuarantineBatch` back to. Read xattrs
        // synchronously and apply in-place so generated images contain
        // the dot. xattr reads are cheap; the screenshot CLI already
        // takes the same shape for enumeration.
        let Some(proxy) = self.event_proxy.clone() else {
            let cursor_name = self.cursor_entry_name();
            let scroll = self.list.scroll_offset;
            let tab = &mut self.tabs[self.active];
            let mut changed = false;
            for (name, mtime_unix, path) in candidates {
                let info = fetch_quarantine_info(&path);
                let key = (path.clone(), mtime_unix);
                let details = quarantine_details_from(&info);
                self.quarantine_cache.insert(
                    key,
                    if info.quarantined { Some(details.clone()) } else { None },
                );
                if let Some(entry) = tab
                    .all_entries
                    .iter_mut()
                    .find(|e| e.name == name && e.mtime_unix == mtime_unix)
                {
                    entry.is_quarantined = info.quarantined;
                    entry.quarantine = Some(if info.quarantined {
                        details
                    } else {
                        QuarantineDetails::default()
                    });
                    changed = true;
                }
            }
            if changed {
                self.rebuild_visible_entries(cursor_name, true);
                self.list.scroll_offset = scroll;
            }
            return;
        };

        let new_task = self.begin_task(TaskKind::QuarantinePrefetch, "Reading xattrs…", false);
        if let Some(prev) = self.quarantine_task.take() {
            self.end_task(prev);
        }
        self.quarantine_task = Some(new_task);

        log_info!(
            60,
            "quarantine prefetch: {} candidates (gen={})",
            candidates.len(),
            generation
        );

        obs::spawn_logged("quarantine-prefetch", move || {
            let results: Vec<QuarantineResult> = candidates
                .into_iter()
                .map(|(name, mtime_unix, path)| {
                    let info = fetch_quarantine_info(&path);
                    QuarantineResult {
                        name,
                        mtime_unix,
                        quarantined: info.quarantined,
                        details: quarantine_details_from(&info),
                    }
                })
                .collect();
            let _ = proxy.send_event(AppEvent::QuarantineBatch {
                generation,
                dir: cur_dir,
                results,
            });
        });
    }

    fn sync_window_title(&self) {
        let Some(w) = &self.window else { return };
        let path = &self.tabs[self.active].current_dir;
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("Feraille"));
        w.set_title(&format!("{} \u{2014} Feraille", label));
    }

    /// Re-enumerate the active tab without resetting scroll, preserving
    /// the cursor on the same entry name when possible. Used by F5 and
    /// after side-effects (Trash, future copy/move).
    #[allow(clippy::collapsible_else_if)]
    pub fn refresh_active_tab(&mut self) {
        let cursor_name = {
            let t = &self.tabs[self.active];
            t.selection
                .cursor()
                .and_then(|i| t.entries.get(i))
                .map(|e| e.name.clone())
        };
        let scroll = self.list.scroll_offset;
        let path = self.tabs[self.active].current_dir.clone();
        let id = self.fs.id_for_path(&path);
        // Hold the existing rows visible; the first arriving batch
        // (or zero-batch `EnumerationDone`) does the swap. Avoids the
        // empty-then-fill flash on F5 over a slow filesystem.
        // Tree might have a stale view of the current folder's contents.
        // Mark unloaded so a future expand re-enumerates; cancels any
        // in-flight tree load for this id.
        self.invalidate_tree(id);
        self.start_enumeration(path, cursor_name, scroll);
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh_active_tab();
        self.save_app_prefs();
    }

    /// Open the cursor entry (a folder, in practice — the menu
    /// item only shows on directories) in a new tab in the same
    /// window. No-op if the cursor entry isn't a folder we can
    /// resolve.
    pub fn open_cursor_in_new_tab(&mut self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        if !matches!(entry.kind, EntryKind::Directory) {
            return;
        }
        let path = tab.current_dir.join(&entry.name);
        self.new_tab_at(path);
    }

    /// Open every entry in the resolved selection with the app at
    /// `app_path`. Best-effort: failures log + toast but don't
    /// abort the rest of the batch.
    pub fn open_selection_with(&mut self, app_path: &Path) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut any_failed = false;
        for p in &paths {
            if let Err(e) = feraille_shell_mac::open_with_app(p, app_path) {
                log_warn!(
                    60,
                    "open_with_app({}, {}) failed: {e}",
                    p.display(),
                    app_path.display()
                );
                any_failed = true;
            }
        }
        if any_failed {
            self.toast_error("Couldn't open with that app");
        }
    }

    /// Pop the system Share picker (`NSSharingServicePicker`)
    /// anchored to the main window. The picker handles the rest —
    /// Mail, Messages, AirDrop, etc.
    pub fn share_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        if let Err(e) = feraille_shell_mac::show_share_picker(&window, &refs) {
            log_warn!(60, "share picker failed: {e}");
            self.toast_error(format!("Couldn't show Share picker: {e}"));
        }
    }

    /// Toggle a Finder colour tag on every entry in the resolved
    /// selection. Synchronous — each tag is one Cocoa hop, fast
    /// enough on the UI thread for typical selection sizes.
    pub fn toggle_tag_on_selection(&mut self, color: feraille_core::commands::TagColor) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut any_failed = false;
        for p in &paths {
            if let Err(e) = feraille_shell_mac::toggle_tag(p, color) {
                log_warn!(60, "toggle_tag({}, {:?}) failed: {e}", p.display(), color);
                any_failed = true;
            }
        }
        if any_failed {
            self.toast_error("Couldn't set tag on some items");
        }
    }

    /// Strip every tag from every entry in the resolved selection.
    pub fn clear_tags_on_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut any_failed = false;
        for p in &paths {
            if let Err(e) = feraille_shell_mac::clear_tags(p) {
                log_warn!(60, "clear_tags({}) failed: {e}", p.display());
                any_failed = true;
            }
        }
        if any_failed {
            self.toast_error("Couldn't clear tags on some items");
        }
    }

    /// Show Quick Look on the resolved selection. Falls back to the
    /// cursor entry if nothing else is selected. No-op on empty
    /// selection. Quick Look is `qlmanage -p`: spawns and detaches.
    pub fn quick_look_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        if let Err(e) = feraille_shell_mac::show_quick_look(&refs) {
            log_warn!(60, "quick_look failed: {e}");
            self.toast_error(format!("Quick Look failed: {e}"));
        }
    }

    /// Make a Finder alias for each selected entry. Synchronous on
    /// the calling thread — bookmark-data creation is a single Cocoa
    /// call per path, fast enough not to need a worker. Refreshes
    /// the tab on success so the new alias file appears.
    pub fn make_alias_for_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut any_ok = false;
        for p in &paths {
            match feraille_shell_mac::make_alias(p) {
                Ok(_) => any_ok = true,
                Err(e) => {
                    log_warn!(60, "make_alias({}) failed: {e}", p.display());
                    self.toast_error(format!("Couldn't make alias: {e}"));
                }
            }
        }
        if any_ok {
            self.refresh_active_tab();
        }
    }

    /// Duplicate each selected entry on a worker. Refreshes the
    /// active tab on completion via [`AppEvent::FileOpComplete`].
    /// Opens an entry in the Tasks panel so the user can see
    /// progress for slow folder copies.
    pub fn duplicate_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let dest_dir = self.tabs[self.active].current_dir.clone();
        let Some(proxy) = self.event_proxy.clone() else {
            // Headless fallback: synchronous so screenshot tests
            // see the new file.
            for p in &paths {
                let _ = feraille_shell_mac::duplicate_path(p);
            }
            self.refresh_active_tab();
            return;
        };
        let label = if paths.len() == 1 {
            format!(
                "Duplicating {}",
                paths[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
            )
        } else {
            format!("Duplicating {} items", paths.len())
        };
        let task_id = self.begin_task(TaskKind::FileOp, label, false);
        let paths_for_worker = paths.clone();
        let dest_dir_for_worker = dest_dir.clone();
        obs::spawn_logged("file-op-duplicate", move || {
            let mut last: Result<PathBuf, String> = Err("no files".into());
            for src in &paths_for_worker {
                last = feraille_shell_mac::duplicate_path(src);
                if let Err(ref e) = last {
                    let _ = proxy.send_event(AppEvent::FileOpComplete {
                        op: FileOpKind::Duplicate,
                        task_id,
                        dest_dir: dest_dir_for_worker.clone(),
                        result: Err(e.clone()),
                    });
                    return;
                }
            }
            let _ = proxy.send_event(AppEvent::FileOpComplete {
                op: FileOpKind::Duplicate,
                task_id,
                dest_dir: dest_dir_for_worker,
                result: last,
            });
        });
    }

    /// Compress the resolved selection into a single .zip via
    /// `/usr/bin/ditto` on a worker. Refreshes the active tab on
    /// completion. Single source → `Foo.zip`; multiple sources →
    /// `Archive.zip`. Matches Finder.
    pub fn compress_selection(&mut self) {
        let paths = self.resolve_selected_paths();
        if paths.is_empty() {
            return;
        }
        let dest_dir = self.tabs[self.active].current_dir.clone();
        let Some(proxy) = self.event_proxy.clone() else {
            let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            let _ = feraille_shell_mac::compress_paths(&refs);
            self.refresh_active_tab();
            return;
        };
        let label = if paths.len() == 1 {
            format!(
                "Compressing {}",
                paths[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
            )
        } else {
            format!("Compressing {} items", paths.len())
        };
        let task_id = self.begin_task(TaskKind::FileOp, label, false);
        let paths_for_worker = paths.clone();
        let dest_dir_for_worker = dest_dir.clone();
        obs::spawn_logged("file-op-compress", move || {
            let refs: Vec<&Path> = paths_for_worker.iter().map(PathBuf::as_path).collect();
            let result = feraille_shell_mac::compress_paths(&refs);
            let _ = proxy.send_event(AppEvent::FileOpComplete {
                op: FileOpKind::Compress,
                task_id,
                dest_dir: dest_dir_for_worker,
                result,
            });
        });
    }

    /// Resolve the *active* paths the right-click context menu
    /// should act on. Honours the SelectionSet — a multi-row
    /// selection acts on every selected row; otherwise falls back
    /// to the cursor entry. Empty when no row is focused.
    fn resolve_selected_paths(&self) -> Vec<PathBuf> {
        let tab = &self.tabs[self.active];
        let cur = &tab.current_dir;
        let mut indices: Vec<usize> = Vec::new();
        match &tab.selection.set {
            SelectionSet::None => {
                if let Some(c) = tab.selection.cursor() {
                    indices.push(c);
                }
            }
            SelectionSet::Single(i) => indices.push(*i),
            SelectionSet::Range { from, to } => {
                indices.extend(*from..=*to);
            }
            SelectionSet::Discrete(set) => {
                indices.extend(set.iter().copied());
            }
        }
        indices
            .into_iter()
            .filter_map(|i| tab.entries.get(i).map(|e| cur.join(&e.name)))
            .collect()
    }

    pub fn cycle_sort(&mut self, column: feraille_controls::ColumnId) {
        self.list.toggle_sort(column);
        self.rebuild_visible_entries(None, false);
    }

    pub fn delete_at_cursor_to_trash(&mut self) {
        let (cur_dir, name) = {
            let t = &self.tabs[self.active];
            let Some(idx) = t.selection.cursor() else {
                return;
            };
            let Some(entry) = t.entries.get(idx) else {
                return;
            };
            (t.current_dir.clone(), entry.name.clone())
        };
        let target = cur_dir.join(&name);
        match move_to_trash(&target) {
            Ok(_) => self.refresh_active_tab(),
            Err(e) => {
                log_error!(
                    57,
                    "move_to_trash({}) failed: {e} — file remains on disk",
                    target.display()
                );
                self.toast_error(format!("Couldn't move to Trash: {e}"));
            }
        }
    }

    /// Walk the tree from the appropriate root down to `path`, expanding
    /// each ancestor. Children are only re-enumerated for ancestors that
    /// haven't been loaded yet — this is the perf fix that keeps tree
    /// re-reveals (e.g. clicking a folder a second time) instant. Auto-
    /// scrolls the tree to keep the target visible.
    pub fn reveal_in_tree(&mut self, path: &Path) {
        let home = home_dir();
        let mut current = if path.starts_with(&home) {
            home
        } else if path.starts_with("/Volumes") {
            let mut comps = path.components();
            let mut acc = PathBuf::new();
            for _ in 0..3 {
                if let Some(c) = comps.next() {
                    acc.push(c.as_os_str());
                } else {
                    break;
                }
            }
            acc
        } else {
            self.tree.select(self.fs.id_for_path(path));
            return;
        };
        loop {
            let id = self.fs.id_for_path(&current);
            if current != path {
                if self.tree.is_loaded(id) {
                    // Cached — just mark expanded; don't touch the FS.
                    self.tree.ensure_expanded(id);
                } else {
                    // Off-main-thread enumeration; the
                    // `TreeChildrenLoaded` handler populates and
                    // expands when the worker returns. The reveal
                    // walk continues without waiting — deeper
                    // ancestors get spawned in parallel and the user
                    // sees the chain expand progressively.
                    self.spawn_tree_load(id);
                }
            }
            if current == path {
                self.tree.select(id);
                let viewport_h = self.tree_rect().size.height;
                self.tree.ensure_visible(id, viewport_h);
                break;
            }
            let rel = match path.strip_prefix(&current) {
                Ok(r) => r,
                Err(_) => {
                    self.tree.select(id);
                    break;
                }
            };
            let next = match rel.components().next() {
                Some(c) => c,
                None => {
                    self.tree.select(id);
                    break;
                }
            };
            current.push(next.as_os_str());
            if !current.is_dir() {
                break;
            }
        }
    }

    fn open_at_cursor(&mut self) {
        let (cur_dir, kind, name) = {
            let t = &self.tabs[self.active];
            let Some(idx) = t.selection.cursor() else {
                return;
            };
            let Some(entry) = t.entries.get(idx) else {
                return;
            };
            (t.current_dir.clone(), entry.kind, entry.name.clone())
        };
        let path = cur_dir.join(&name);
        match kind {
            EntryKind::Directory => self.navigate(path),
            EntryKind::File | EntryKind::Symlink => {
                if let Err(e) = open_with_default(&path) {
                    log_error!(57, "open_with_default({}) failed: {e}", path.display());
                    self.toast_error(format!("Couldn't open: {e}"));
                }
            }
        }
    }

    pub fn navigate_parent(&mut self) {
        let parent = self.tabs[self.active]
            .current_dir
            .parent()
            .map(Path::to_path_buf);
        if let Some(p) = parent {
            self.navigate(p);
        }
    }

    fn switch_tab(&mut self, new: usize) {
        if new == self.active || new >= self.tabs.len() {
            return;
        }
        // Save list scroll to outgoing tab.
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        self.active = new;
        self.list.scroll_offset = self.tabs[new].list_scroll;
        let path = self.tabs[new].current_dir.clone();
        self.breadcrumb.set_path(&path);
        let id = self.fs.id_for_path(&path);
        self.tree.select(id);
        self.sync_window_title();
    }

    fn new_tab(&mut self) {
        let path = self.tabs[self.active].current_dir.clone();
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        let new_index = self.tabs.len();
        self.tabs.push(Tab {
            current_dir: path.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            filter_text: String::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
            error: None,
            history: Vec::new(),
            history_index: 0,
        });
        self.active = new_index;
        self.list.scroll_offset = 0.0;
        self.navigate(path);
        feraille_shell_mac::set_tab_count(self.tabs.len());
    }

    fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if idx < self.active {
            self.active -= 1;
        }
        self.list.scroll_offset = self.tabs[self.active].list_scroll;
        let path = self.tabs[self.active].current_dir.clone();
        self.breadcrumb.set_path(&path);
        let id = self.fs.id_for_path(&path);
        self.tree.select(id);
        feraille_shell_mac::set_tab_count(self.tabs.len());
    }

    /// Wrapper used by the catalogue's `file.close_tab` command. Closes
    /// the active tab; the underlying [`close_tab`] no-ops at one tab,
    /// at which point AppKit's `validateMenuItem:` greys out the menu
    /// entry and Cmd+W falls through to Close Window.
    pub fn close_active_tab(&mut self) {
        self.close_tab(self.active);
    }

    /// Open the in-app Settings modal. Idempotent: re-opening
    /// closes the existing instance first so any in-flight slider
    /// drag is reset.
    pub fn show_settings(&mut self) {
        self.settings_modal = Some(SettingsModal::new());
    }

    pub fn close_settings(&mut self) {
        self.settings_modal = None;
    }

    /// Open the in-app Keyboard-Shortcuts overlay. Closes any previous
    /// instance so the filter resets on each invocation. Renders next
    /// frame; see `paint_shortcuts`.
    pub fn show_help_shortcuts(&mut self) {
        self.shortcuts_modal = Some(ShortcutsModal::new());
    }

    pub fn close_shortcuts_modal(&mut self) {
        self.shortcuts_modal = None;
    }

    fn handle_tree_event(&mut self, ev: TreeEvent) {
        match ev {
            TreeEvent::Activate(id) => {
                if let Some(path) = self.fs.path_for(id) {
                    self.navigate(path);
                }
            }
            TreeEvent::ExpandRequested(id) => {
                self.spawn_tree_load(id);
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Pure paint into a renderer. No window/surface dependency. Used by
    /// both the GUI present path and the headless screenshot path.
    pub fn paint_to(&mut self, renderer: &mut dyn Renderer) {
        // Kick the preview-thumbnail worker for the current selection
        // before paint runs (paint itself is read-only). Cheap when
        // cached or already in flight.
        if self.preview_visible {
            self.maybe_kick_preview_thumb();
        }

        let tabstrip_rect = self.tabstrip_rect();
        let tree_rect = self.tree_rect();
        let breadcrumb_rect = self.breadcrumb_rect();
        let header_rect = self.header_rect();
        let list_inner = self.list_inner_rect();
        let scrollbar_rect = self.scrollbar_rect();
        let preview_rect = self.preview_rect();
        let splitter_container = self.splitter_container();
        let content_h = self.list_content_height();
        let status_text = format_status(&self.tabs[self.active], &self.tasks);
        let tab_infos: Vec<TabInfo> = self
            .tabs
            .iter()
            .map(|t| TabInfo { label: t.label() })
            .collect();
        let active = self.active;
        let splitter_x = self.splitter_x;
        let viewport = renderer.viewport();
        let tokens = &self.tokens;

        // Window bg
        renderer.fill_rect(
            FRect::new(0.0, 0.0, viewport.width, viewport.height),
            tokens.bg.base,
        );

        // Tabstrip — topmost element (the OS title bar sits above us).
        self.tabstrip
            .paint(tabstrip_rect, &tab_infos, active, tokens, renderer);

        // Tree pane — paint with ant-trail heat overlay. Volumes /
        // Locations rows resolve to their per-path Finder icon (via
        // `path_icon_key`); every other row uses the shared `"DIR"`
        // bitmap. Per-path lookups fall back to `"DIR"` while the
        // chunked prefetcher catches up, avoiding an accent-rectangle
        // flash on first paint.
        let trail = &self.ant_trail;
        let icon_cache = &self.icon_cache;
        let tree_ref = &self.tree;
        let fs_ref = &self.fs;
        self.tree.paint(
            tree_rect,
            tokens,
            renderer,
            |id| trail.heat(id),
            |id| match tree_ref.section_kind_for(id) {
                Some(SectionKind::Volumes) | Some(SectionKind::Locations) => fs_ref
                    .path_for(id)
                    .and_then(|p| icon_cache.get(&path_icon_key(&p)))
                    .or_else(|| icon_cache.get("DIR")),
                _ => icon_cache.get("DIR"),
            },
        );

        // Breadcrumb
        self.breadcrumb.paint(breadcrumb_rect, tokens, renderer);

        // Column header (above list, below breadcrumb)
        self.list.paint_header(header_rect, tokens, renderer);

        // List + scrollbar
        let tab = &self.tabs[self.active];
        let icon_cache = &self.icon_cache;
        self.list.paint(
            list_inner,
            &tab.entries,
            &tab.selection,
            |entry| icon_cache.get(&cache_key_for(entry)),
            tokens,
            renderer,
        );
        self.scrollbar.paint(
            scrollbar_rect,
            content_h,
            list_inner.size.height,
            self.list.scroll_offset,
            tokens,
            renderer,
        );
        // If enumeration failed (TCC permission denied / not found / I/O),
        // overlay a centered explanation panel so the user understands
        // why the list is empty.
        if let Some(err) = self.tabs[self.active].error.clone() {
            paint_empty_state(
                list_inner,
                &err,
                &self.tabs[self.active].current_dir,
                tokens,
                renderer,
            );
        }

        // Inline rename overlay — anchored to the row's name column.
        // The editor box hugs the text width so there's no big empty
        // span after the name that visually reads as trailing space.
        // Auto-cancel if the row scrolled offscreen.
        let inline_rect = self
            .inline_rename
            .as_ref()
            .and_then(|state| self.list.row_name_rect(list_inner, state.row_idx));
        match (inline_rect, self.inline_rename.as_ref()) {
            (Some(name_rect), Some(state)) => {
                let measured = state.input.measured_width(tokens.text.md);
                // 8 DIPs of left padding from TextInput::paint, plus
                // a little breathing room on the right for the caret.
                let snug_w = (measured + 24.0).clamp(80.0, name_rect.size.width);
                let edit_rect = FRect::new(
                    name_rect.left(),
                    name_rect.top(),
                    snug_w,
                    name_rect.size.height,
                );
                state.input.paint(edit_rect, true, tokens, renderer);
                self.inline_rename_rect = Some(edit_rect);
            }
            (None, Some(_)) => {
                // Row scrolled offscreen — auto-cancel.
                self.inline_rename = None;
                self.inline_rename_rect = None;
            }
            _ => {}
        }

        if self.preview_visible {
            self.paint_preview_pane(preview_rect, tokens, renderer);
        }

        // Splitter (sidebar | file pane).
        self.splitter
            .paint(splitter_x, splitter_container, tokens, renderer);
        // Splitter (file pane | preview pane), when the preview is shown.
        if let Some(px) = self.preview_splitter_x() {
            self.preview_splitter
                .paint(px, splitter_container, tokens, renderer);
        }

        // Properties panel — overlay over everything else when open.
        if self.properties_target.is_some() {
            self.paint_properties(tokens, viewport, renderer);
        }
        // Search/filter overlay.
        if self.search.is_some() {
            self.paint_search(tokens, viewport, renderer);
        }
        // Modal text dialog (rename / new-folder) above everything.
        if self.dialog.is_some() {
            self.paint_dialog(tokens, viewport, renderer);
        }
        // Keyboard-shortcuts overlay. Sits above the dialog layer so
        // pressing Cmd+/ during a rename still surfaces it.
        if self.shortcuts_modal.is_some() {
            self.paint_shortcuts(tokens, viewport, renderer);
        }
        if self.settings_modal.is_some() {
            self.paint_settings(tokens, viewport, renderer);
        }

        // Toasts — bottom-right of the file pane area, above the status
        // bar. Pruned at the start of the frame so expired entries don't
        // burn a paint cycle.
        let toast_now = std::time::Instant::now();
        self.toasts.prune(toast_now);
        if !self.toasts.is_empty() {
            let toast_area = FRect::new(
                splitter_x,
                breadcrumb_rect.bottom(),
                viewport.width - splitter_x,
                viewport.height - breadcrumb_rect.bottom() - tokens.layout.status_bar,
            );
            self.toasts.paint(toast_area, tokens, renderer);
        }

        // Status
        let status = FRect::new(0.0, viewport.height - tokens.layout.status_bar, viewport.width, tokens.layout.status_bar);
        renderer.fill_rect(status, tokens.bg.layer2);
        renderer.fill_rect(
            FRect::new(0.0, viewport.height - tokens.layout.status_bar, viewport.width, 1.0),
            tokens.border.subtle,
        );
        // Progress strip overlays the top edge of the status bar.
        let now = std::time::Instant::now();
        self.progress.paint(
            FRect::new(0.0, viewport.height - tokens.layout.status_bar, viewport.width, 2.0),
            now,
            tokens,
            renderer,
        );
        renderer.draw_text(
            FPoint::new(
                tokens.space.md,
                viewport.height - tokens.layout.status_bar + (tokens.layout.status_bar - tokens.text.xs) / 2.0 - 1.0,
            ),
            &status_text,
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );

        // Task popover — paints last so it sits above everything else.
        // Only visible when the user has clicked the status bar; auto-
        // hides when the registry empties (handled in `end_task`).
        if self.task_panel_open && !self.tasks.is_empty() {
            let vp = FRect::new(0.0, 0.0, viewport.width, viewport.height);
            task_panel::paint(vp, &self.tasks, tokens, renderer);
        }
    }

    fn render(&mut self) {
        // Take the renderer + surface out of self so we can borrow self mutably
        // during paint without aliasing.
        let Some(mut renderer) = self.renderer.take() else {
            return;
        };
        let Some(mut surface) = self.surface.take() else {
            self.renderer = Some(renderer);
            return;
        };
        let Some(w_nz) = NonZeroU32::new(self.width) else {
            self.renderer = Some(renderer);
            self.surface = Some(surface);
            return;
        };
        let Some(h_nz) = NonZeroU32::new(self.height) else {
            self.renderer = Some(renderer);
            self.surface = Some(surface);
            return;
        };
        if surface.resize(w_nz, h_nz).is_err() {
            self.renderer = Some(renderer);
            self.surface = Some(surface);
            return;
        }
        self.paint_to(&mut renderer);
        let pixels = renderer.pixels();
        if let Ok(mut buffer) = surface.buffer_mut() {
            for (dst, src) in buffer.iter_mut().zip(pixels.iter()) {
                *dst = *src & 0x00FF_FFFF;
            }
            let _ = buffer.present();
        }
        self.renderer = Some(renderer);
        self.surface = Some(surface);
        // Drive progress-strip animation: if the strip is still active,
        // request another redraw so the comet keeps moving. Self-driven
        // via winit's redraw queue rather than a timer thread.
        if self
            .progress
            .next_wakeup(std::time::Instant::now())
            .is_some()
        {
            self.request_redraw();
        }
    }

    // ---- Disk Usage window: open/focus, refresh, zoom-out, worker spawn ----

    /// Cmd+Shift+D handler. Opens the DU window if not already open;
    /// otherwise just focuses it. The scan is rooted at the active
    /// tab's current directory and runs to completion in the
    /// background; closing the window cancels it.
    fn open_or_focus_disk_usage(&mut self) {
        let root = self.tabs[self.active].current_dir.clone();
        if let Some(du) = self.disk_usage_window.as_ref() {
            // If the window is already open and rooted at the same
            // path, just focus it. If it's open but on a different
            // path, swap it for a fresh scan rooted at the new path.
            if du.state.root_path == root {
                du.window.focus_window();
                return;
            }
            self.close_disk_usage_window();
        }
        // Default to package-as-leaf for a fresh window. The user can
        // toggle once the window is open.
        self.spawn_disk_usage_window(root, false);
    }

    /// Cancel-and-restart for the active DU window. Preserves the
    /// existing window's `descend_packages` setting. No-op when closed.
    fn refresh_disk_usage(&mut self) {
        let Some(du) = self.disk_usage_window.as_ref() else {
            return;
        };
        let root = du.state.root_path.clone();
        let descend = du.state.descend_packages;
        self.close_disk_usage_window();
        self.spawn_disk_usage_window(root, descend);
    }

    /// Pop one level off the DU window's zoom path.
    fn disk_usage_zoom_out(&mut self) {
        if let Some(du) = self.disk_usage_window.as_mut() {
            du.zoom_out();
            du.window.request_redraw();
        }
    }

    /// Toggle the Top-N largest files panel in the DU window.
    fn disk_usage_toggle_topn(&mut self) {
        if let Some(du) = self.disk_usage_window.as_mut() {
            du.state.topn_visible = !du.state.topn_visible;
            du.state.invalidate_layout();
            let on = du.state.topn_visible;
            du.window.request_redraw();
            feraille_shell_mac::set_command_state(
                CommandId("disk_usage.toggle_topn"),
                on,
            );
        }
    }

    /// Push the live DU window settings into the menu's checkmark
    /// columns. Called on window open and whenever a setting flips
    /// from elsewhere (e.g. close-and-restart for refresh / toggle
    /// packages).
    fn sync_disk_usage_menu_state(&self) {
        let (topn, packages, follow) = match self.disk_usage_window.as_ref() {
            Some(du) => (
                du.state.topn_visible,
                du.state.descend_packages,
                du.state.follow_navigation,
            ),
            None => (false, false, false),
        };
        feraille_shell_mac::set_command_state(CommandId("disk_usage.toggle_topn"), topn);
        feraille_shell_mac::set_command_state(CommandId("disk_usage.toggle_packages"), packages);
        feraille_shell_mac::set_command_state(
            CommandId("disk_usage.toggle_follow_navigation"),
            follow,
        );
    }

    fn disk_usage_set_size_mode(&mut self, mode: feraille_disk_usage::SizeMode) {
        if let Some(du) = self.disk_usage_window.as_mut() {
            if du.state.size_mode != mode {
                du.state.size_mode = mode;
                du.state.invalidate_layout();
                du.state.rebuild_topn();
                du.window.request_redraw();
            }
        }
        feraille_shell_mac::set_command_state(
            CommandId("disk_usage.size_apparent"),
            mode == feraille_disk_usage::SizeMode::Apparent,
        );
        feraille_shell_mac::set_command_state(
            CommandId("disk_usage.size_allocated"),
            mode == feraille_disk_usage::SizeMode::Allocated,
        );
    }

    fn disk_usage_set_coloring(&mut self, coloring: feraille_controls::TreemapColoring) {
        if let Some(du) = self.disk_usage_window.as_mut() {
            du.state.coloring = coloring;
            du.window.request_redraw();
        }
        // Update the menu radios.
        let map = [
            (
                "disk_usage.coloring_category",
                feraille_controls::TreemapColoring::Category,
            ),
            (
                "disk_usage.coloring_age",
                feraille_controls::TreemapColoring::AgeHeat,
            ),
            (
                "disk_usage.coloring_depth",
                feraille_controls::TreemapColoring::DepthOnly,
            ),
        ];
        for (id, mode) in map {
            feraille_shell_mac::set_command_state(CommandId(id), mode == coloring);
        }
    }

    fn disk_usage_toggle_follow_navigation(&mut self) {
        let Some(du) = self.disk_usage_window.as_mut() else {
            return;
        };
        du.state.follow_navigation = !du.state.follow_navigation;
        let on = du.state.follow_navigation;
        feraille_shell_mac::set_command_state(
            CommandId("disk_usage.toggle_follow_navigation"),
            on,
        );
    }

    /// Called from `navigate` after the active tab's `current_dir`
    /// settles. If the DU window is open, the user opted into
    /// `follow_navigation`, and the new path differs from the DU
    /// window's current root, kick a fresh scan rooted at the new
    /// path. Preserves the descend-packages and follow-navigation
    /// settings.
    fn maybe_follow_disk_usage_navigation(&mut self, new_root: &Path) {
        let Some(du) = self.disk_usage_window.as_ref() else {
            return;
        };
        if !du.state.follow_navigation {
            return;
        }
        let canonical_new = std::fs::canonicalize(new_root).unwrap_or_else(|_| new_root.to_path_buf());
        if du.state.root_path == canonical_new {
            return;
        }
        let descend = du.state.descend_packages;
        self.close_disk_usage_window();
        self.spawn_disk_usage_window(canonical_new, descend);
    }

    /// Toggle whether macOS packages (`.app`, `.bundle`, …) are
    /// descended into during the scan. Triggers a re-scan because the
    /// fact stream produced under the two settings is structurally
    /// different.
    fn disk_usage_toggle_packages(&mut self) {
        let Some(du) = self.disk_usage_window.as_ref() else {
            return;
        };
        let root = du.state.root_path.clone();
        let new_descend = !du.state.descend_packages;
        self.close_disk_usage_window();
        self.spawn_disk_usage_window(root, new_descend);
        feraille_shell_mac::set_command_state(
            CommandId("disk_usage.toggle_packages"),
            new_descend,
        );
    }

    /// Right-click in the DU window. Builds an NSMenu rooted at the
    /// rect under the cursor (or the current selection if the click
    /// landed outside any rect), then dispatches the chosen action.
    fn show_disk_usage_context_menu(&mut self) {
        // Capture state up-front so we can drop the window borrow
        // before the synchronous menu pops (the menu blocks the
        // calling thread until the user dismisses it).
        let Some(du) = self.disk_usage_window.as_ref() else {
            return;
        };
        let Some(cursor) = du.pointer_dips else { return };
        let target_id = match du.hit_at(cursor) {
            crate::disk_usage_window::DuHit::TreemapNode(id)
            | crate::disk_usage_window::DuHit::TopNRow(id) => Some(id),
            _ => du.state.selection.iter().next().copied(),
        };
        let Some(target_id) = target_id else { return };

        // If the right-clicked node is already part of a multi-
        // selection, the menu acts on the whole set. Otherwise the
        // selection collapses to just the clicked node (matches
        // Finder's behaviour).
        let already_selected = du.state.selection.contains(&target_id);
        let mut targets: Vec<NodeId> = if already_selected && du.state.selection.len() > 1 {
            du.state.selection.iter().copied().collect()
        } else {
            vec![target_id]
        };
        // Stable order: target_id first, then the rest by NodeId.
        targets.sort_by(|a, b| {
            if *a == target_id {
                std::cmp::Ordering::Less
            } else if *b == target_id {
                std::cmp::Ordering::Greater
            } else {
                a.cmp(b)
            }
        });

        // Resolve paths + kinds. Drop nodes whose path the FS can't
        // resolve (shouldn't happen for live trees but guards
        // against stale state).
        let resolved: Vec<(NodeId, PathBuf, bool)> = targets
            .iter()
            .filter_map(|id| {
                let path = self.fs.path_for(*id)?;
                let is_container = matches!(
                    du.state.tree.nodes.get(id).map(|n| n.kind),
                    Some(feraille_disk_usage::NodeKind::Container)
                ) && du
                    .state
                    .tree
                    .containers
                    .get(id)
                    .map(|m| !m.is_empty())
                    .unwrap_or(false);
                Some((*id, path, is_container))
            })
            .collect();
        if resolved.is_empty() {
            return;
        }
        let primary_path = resolved[0].1.clone();
        let primary_is_container = resolved[0].2;
        let many = resolved.len() > 1;

        let cursor_pair = (cursor.x, cursor.y);
        let window_handle = du.window.clone();

        // Build the menu plan. Single-target shows verbs as-is;
        // multi-target swaps to "Reveal N in Finder / Move N Items
        // to Trash". Zoom-into is only offered for a single non-
        // empty container.
        let mut plan = feraille_shell_mac::MenuPlan::new();
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.open"),
            if many { "Open First" } else { "Open" },
        ));
        // Treemap containers are always folders. Single-target
        // gets "Open in New Tab" in the same primary-action slot
        // Finder uses for folder menus.
        if !many && primary_is_container {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.open_in_new_tab"),
                "Open in New Tab",
            ));
        }
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.reveal_in_finder"),
            if many {
                format!("Reveal {} in Finder", resolved.len())
            } else {
                "Reveal in Finder".to_string()
            },
        ));
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.copy_path"),
            if many {
                format!("Copy {} Paths", resolved.len())
            } else {
                "Copy Path".to_string()
            },
        ));
        if !many {
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("file.quick_look"),
                "Quick Look",
            ));
        }
        if !many && primary_is_container {
            plan.push(feraille_shell_mac::MenuPlanItem::separator());
            plan.push(feraille_shell_mac::MenuPlanItem::action(
                CommandId("disk_usage.zoom_into"),
                "Zoom into",
            ));
        }
        plan.push(feraille_shell_mac::MenuPlanItem::separator());
        plan.push(feraille_shell_mac::MenuPlanItem::action(
            CommandId("file.move_to_trash"),
            if many {
                format!("Move {} Items to Trash", resolved.len())
            } else {
                "Move to Trash".to_string()
            },
        ));

        // Promote the right-click target to selection (or grow the
        // existing selection to include it) before we show the menu,
        // so the highlight stays visible while the menu is up.
        if let Some(du) = self.disk_usage_window.as_mut() {
            if !already_selected {
                du.state.selection.clear();
                du.state.selection.insert(target_id);
            }
            du.window.request_redraw();
        }

        let pick = feraille_shell_mac::show_context_menu(&window_handle, plan, cursor_pair);
        match pick.as_ref().map(|p| p.command.0) {
            Some("file.open") => {
                // Single primary path on multi-select too: "Open First".
                if let Err(e) = feraille_fs_native::open_with_default(&primary_path) {
                    log_warn!(
                        60,
                        "disk usage: open failed for {}: {e}",
                        primary_path.display()
                    );
                }
            }
            Some("file.open_in_new_tab") => {
                self.new_tab_at(primary_path.clone());
            }
            Some("file.reveal_in_finder") => {
                for (_, path, _) in &resolved {
                    feraille_shell_mac::reveal_in_finder(path);
                }
            }
            Some("file.copy_path") => {
                let joined = resolved
                    .iter()
                    .map(|(_, p, _)| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                feraille_shell_mac::copy_to_clipboard(&joined);
            }
            Some("file.quick_look") => {
                if let Err(e) = feraille_shell_mac::show_quick_look(&[primary_path.as_path()]) {
                    log_warn!(60, "quick_look failed: {e}");
                }
            }
            Some("disk_usage.zoom_into") => {
                if !many && primary_is_container {
                    if let Some(du) = self.disk_usage_window.as_mut() {
                        du.drilldown(target_id);
                        du.window.request_redraw();
                    }
                }
            }
            Some("file.move_to_trash") => {
                for (id, path, _) in &resolved {
                    self.disk_usage_trash_node(*id, path);
                }
            }
            _ => {}
        }
    }

    /// Move-to-Trash from the DU context menu. On success, surgically
    /// drop the affected subtree from the tree and rebuild Top-N so
    /// the visualization updates without re-scanning. On failure, log
    /// a warning — a toast surface for the DU window can come in a
    /// later iter.
    fn disk_usage_trash_node(&mut self, node_id: NodeId, path: &Path) {
        match feraille_fs_native::move_to_trash(path) {
            Ok(()) => {
                if let Some(du) = self.disk_usage_window.as_mut() {
                    du.state.tree.remove_subtree(node_id);
                    du.state.selection.remove(&node_id);
                    if du.state.hovered == Some(node_id) {
                        du.state.hovered = None;
                    }
                    du.state.zoom_path.retain(|n| *n != node_id);
                    du.state.invalidate_layout();
                    du.state.rebuild_topn();
                    du.window.request_redraw();
                }
                log_info!(60, "disk usage: trashed {}", path.display());
            }
            Err(e) => {
                log_warn!(
                    60,
                    "disk usage: move_to_trash failed for {}: {e}",
                    path.display()
                );
                if let Some(du) = self.disk_usage_window.as_mut() {
                    use feraille_controls::primitives::toast::{Toast, ToastKind};
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_else(|| path.to_str().unwrap_or("<path>"));
                    du.state.toasts.push(Toast::new(
                        ToastKind::Error,
                        format!("Couldn't move {name} to Trash: {e}"),
                    ));
                    du.window.request_redraw();
                }
            }
        }
    }

    /// Tear down the DU window: cancel the in-flight scan, drop the
    /// task entry, drop the window. Idempotent.
    fn close_disk_usage_window(&mut self) {
        if let Some(mut du) = self.disk_usage_window.take() {
            // Persist geometry before tearing down. Writes go to
            // BOTH the legacy `du_window.txt` (so older builds keep
            // working) and the new metadata DB's `layout_state.du_*`
            // columns. Reads prefer the DB; the txt file is a
            // fallback during the migration window.
            let inner = du.window.inner_size();
            let scale = du.scale_factor.max(0.01);
            let width_dips = (inner.width as f32 / scale) as u32;
            let height_dips = (inner.height as f32 / scale) as u32;
            disk_usage_prefs::save(disk_usage_prefs::DuWindowGeometry {
                width: Some(width_dips.max(320)),
                height: Some(height_dips.max(240)),
                topn_width: Some(du.state.topn_width_dips),
            });
            if let Some(db) = self.metadata_db.as_ref() {
                // Read the current layout row, mutate just the du_*
                // fields, and write back. Avoids stomping the
                // sidebar / preview widths the main-window quit
                // path also writes.
                let mut layout = db.load_layout_state().ok().flatten().unwrap_or_default();
                layout.du_width = width_dips.max(320) as i32;
                layout.du_height = height_dips.max(240) as i32;
                layout.du_topn_width = du.state.topn_width_dips.round() as i32;
                let _ = db.save_layout_state(&layout);
            }

            du.state.cancel.store(true, Ordering::Relaxed);
            if let Some(id) = du.state.task_id.take() {
                self.tasks.end(id);
            }
        }
        // Also clear any pending-but-unbuilt request so the next
        // `view.disk_usage` press starts fresh.
        if let Some(p) = self.pending_disk_usage_open.take() {
            p.cancel.store(true, Ordering::Relaxed);
            self.tasks.end(p.task_id);
        }
        self.sync_disk_usage_menu_state();
    }

    /// Allocate the DU window + spawn the worker. Stores the window
    /// in `self.disk_usage_window`. Bumps `disk_usage_generation` so
    /// any in-flight events from a prior scan are dropped.
    fn spawn_disk_usage_window(&mut self, root: std::path::PathBuf, descend_packages: bool) {
        let Some(proxy) = self.event_proxy.clone() else {
            // Headless: no event loop to drive the window. The
            // screenshot harness exercises the static path instead.
            log_warn!(60, "disk_usage: no event proxy; window not created");
            return;
        };

        // Resolve the root: canonicalize to a stable path the worker
        // will report facts under, then assign a NodeId.
        let canonical = std::fs::canonicalize(&root).unwrap_or(root.clone());
        let root_id = self.fs.id_for_path(&canonical);
        self.disk_usage_generation = self.disk_usage_generation.wrapping_add(1);
        let generation = self.disk_usage_generation;

        // Spawn the winit window. We can't reach the active event
        // loop from here, so we defer creation until the next
        // `resumed`/`window_event` tick by deferring through the
        // proxy. Practical workaround: open from the same call by
        // borrowing the existing event loop's Window factory. winit
        // exposes `event_loop.create_window` only inside event
        // handlers; since `dispatch_command` is invoked from
        // `user_event` which has `&ActiveEventLoop`, we'd need to
        // thread it through. For iter-6.2 we take a simpler path:
        // create the window inside the next `user_event` by posting
        // a deferred command. But `dispatch_command` IS reachable
        // from `user_event` already; the cleanest route is to defer
        // the actual `create_window` call via `proxy.send_event` to
        // a dedicated `AppEvent::OpenDiskUsageWindow`. To keep iter
        // scope tight, we reuse the existing main window's softbuffer
        // context for now and skip multi-window creation in this
        // step.
        //
        // Below: kick off the worker; the window itself is built
        // lazily in the first `resumed`-style touch. The state is
        // stashed on App so the next `user_event` tick (or a
        // dedicated `Open` arm) can pick it up.

        let mut state = DiskUsageState::new(canonical.clone(), root_id, generation);
        state.descend_packages = descend_packages;
        let cancel_for_worker = state.cancel.clone();
        let task_label = format!("Analyzing {}…", canonical.display());
        let task_id = self.begin_task(TaskKind::DiskUsage, task_label, true);

        // We need the actual window built from inside an event-loop
        // callback. Stash the pending state on App and post an
        // event so the next `user_event` tick can build the window
        // with access to the live `ActiveEventLoop`. iter-6.2
        // workaround: create the window structure during `resumed`
        // *if* a pending DU root has been requested. The DU window's
        // softbuffer surface still needs an active event loop, so we
        // queue the request and act on it in the next event-loop
        // turn.
        self.pending_disk_usage_open = Some(PendingDiskUsageOpen {
            state,
            task_id,
            generation,
            cancel: cancel_for_worker.clone(),
            root_path: canonical.clone(),
        });

        // Spawn the worker right now — it doesn't depend on the
        // window existing. Facts queue up; once the window is built
        // and routed by WindowId, the existing user_event arms apply
        // them.
        let fs = self.fs.clone();
        let proxy_clone = proxy.clone();
        obs::spawn_logged("disk-usage-scan", move || {
            let proxy_a = proxy_clone.clone();
            let proxy_b = proxy_clone.clone();
            let err = fs.scan_disk_usage(
                &canonical,
                feraille_fs_native::DEFAULT_DU_BATCH,
                &cancel_for_worker,
                descend_packages,
                move |facts| {
                    let _ = proxy_a.send_event(AppEvent::DiskUsageBatch {
                        generation,
                        facts,
                    });
                },
                move |stats| {
                    let _ = proxy_b.send_event(AppEvent::DiskUsageProgress {
                        generation,
                        stats,
                    });
                },
            );
            let _ = proxy_clone.send_event(AppEvent::DiskUsageDone { generation, error: err });
        });
    }

    /// Drain any pending DU window request and create the actual
    /// `winit::Window` + softbuffer surface + soft renderer. Must be
    /// called from within an event-loop callback that supplies
    /// `&ActiveEventLoop`. No-op when nothing is pending or the
    /// window already exists.
    fn try_realize_disk_usage_window(&mut self, event_loop: &ActiveEventLoop) {
        let Some(pending) = self.pending_disk_usage_open.take() else {
            return;
        };
        if self.disk_usage_window.is_some() {
            // A previous request landed before this one; cancel the
            // newer one's pending state to keep things consistent.
            pending.cancel.store(true, Ordering::Relaxed);
            self.tasks.end(pending.task_id);
            return;
        }

        let title = format!("Disk Usage — {}", pending.root_path.display());
        // Prefer the metadata DB's layout_state.du_*; fall back to
        // the legacy `du_window.txt` for users on the migration
        // boundary; finally fall back to compile-time defaults.
        let saved_txt = disk_usage_prefs::load();
        let saved_db = self
            .metadata_db
            .as_ref()
            .and_then(|db| db.load_layout_state().ok().flatten());
        let initial_w = saved_db
            .as_ref()
            .filter(|l| l.du_width > 0)
            .map(|l| l.du_width as f64)
            .or_else(|| saved_txt.width.map(|w| w as f64))
            .unwrap_or(1100.0);
        let initial_h = saved_db
            .as_ref()
            .filter(|l| l.du_height > 0)
            .map(|l| l.du_height as f64)
            .or_else(|| saved_txt.height.map(|h| h as f64))
            .unwrap_or(720.0);
        let initial_topn_width: f32 = saved_db
            .as_ref()
            .filter(|l| l.du_topn_width > 0)
            .map(|l| l.du_topn_width as f32)
            .or(saved_txt.topn_width)
            .unwrap_or(280.0);
        let _ = initial_topn_width; // wired into state below if non-zero
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(initial_w, initial_h));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                log_error!(60, "create_window for disk usage failed: {e}");
                pending.cancel.store(true, Ordering::Relaxed);
                self.tasks.end(pending.task_id);
                return;
            }
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                log_error!(60, "softbuffer Context for disk usage failed: {e}");
                pending.cancel.store(true, Ordering::Relaxed);
                self.tasks.end(pending.task_id);
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                log_error!(60, "softbuffer Surface for disk usage failed: {e}");
                pending.cancel.store(true, Ordering::Relaxed);
                self.tasks.end(pending.task_id);
                return;
            }
        };

        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let width_px = size.width.max(1);
        let height_px = size.height.max(1);
        let font_bytes = match load_default_font() {
            Ok(b) => b,
            Err(e) => {
                log_error!(60, "load_default_font for disk usage failed: {e:#}");
                pending.cancel.store(true, Ordering::Relaxed);
                self.tasks.end(pending.task_id);
                return;
            }
        };
        let renderer = SoftRenderer::new(width_px, height_px, scale, font_bytes);

        let mut state = pending.state;
        // Adopt the same task_id we already registered so the panel's
        // cancel button continues to work after realization.
        state.task_id = Some(pending.task_id);
        // Restore the previously-saved Top-N panel width — DB wins
        // over the legacy txt file via `initial_topn_width` above.
        if initial_topn_width > 0.0 {
            state.topn_width_dips = initial_topn_width;
        } else if let Some(tw) = saved_txt.topn_width {
            state.topn_width_dips = tw;
        }

        // Volume capacity snapshot for the header strip — best effort.
        // For folders that aren't volume roots, NSURL still resolves
        // to the containing volume; we walk up the path until we find
        // one. None on non-macOS or when the lookup fails.
        state.volume = lookup_volume_for_path(&state.root_path);

        log_info!(
            60,
            "disk usage window: {}x{} @{:.2}x for {}",
            width_px, height_px, scale, state.root_path.display()
        );

        self.disk_usage_window = Some(DiskUsageWindow::new(
            window,
            context,
            surface,
            renderer,
            width_px,
            height_px,
            scale,
            state,
        ));
        self.sync_disk_usage_menu_state();
    }

    /// Paint the DU window if it exists and present the buffer.
    fn paint_disk_usage_window(&mut self) {
        let tokens = self.tokens.clone();
        if let Some(du) = self.disk_usage_window.as_mut() {
            du.paint(&tokens);
            du.present();
        }
    }

    /// Handle a winit window event that's been routed to the DU
    /// window. Mirrors the main window's handler, scoped to what the
    /// DU view actually needs in iter-6.2: close, resize, scale,
    /// cursor-move (hover), click (selection + drilldown), Backspace
    /// (zoom-out), and redraw.
    fn handle_disk_usage_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.close_disk_usage_window();
            }
            WindowEvent::Resized(size) => {
                if let Some(du) = self.disk_usage_window.as_mut() {
                    du.handle_resize(size);
                    du.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(du) = self.disk_usage_window.as_mut() {
                    du.handle_scale_factor(scale_factor as f32);
                    du.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved {
                position: PhysicalPosition { x, y },
                ..
            } => {
                let Some(du) = self.disk_usage_window.as_mut() else {
                    return;
                };
                let p = FPoint::new(
                    (x as f32) / du.scale_factor,
                    (y as f32) / du.scale_factor,
                );
                du.pointer_dips = Some(p);
                let mut redraw = false;

                // If a splitter drag is in progress, route to it.
                if du.splitter_dragging() {
                    if du.update_splitter_drag(p) {
                        redraw = true;
                    }
                    if du.update_splitter_hover(Some(p)) {
                        redraw = true;
                    }
                    if redraw {
                        du.window.request_redraw();
                    }
                    return;
                }

                // Splitter hover affordance — faint fill, handle dots,
                // thicker rule. Matches the main window's sidebar /
                // preview splitters so the visual is consistent.
                if du.update_splitter_hover(Some(p)) {
                    redraw = true;
                }

                // Refresh-button visual state.
                let over_btn = matches!(
                    du.hit_at(p),
                    crate::disk_usage_window::DuHit::RefreshButton
                );
                let new_btn = match (du.refresh_button, over_btn) {
                    (crate::disk_usage_window::ButtonState::Pressed, true) => {
                        crate::disk_usage_window::ButtonState::Pressed
                    }
                    (crate::disk_usage_window::ButtonState::Pressed, false) => {
                        crate::disk_usage_window::ButtonState::Idle
                    }
                    (_, true) => crate::disk_usage_window::ButtonState::Hover,
                    (_, false) => crate::disk_usage_window::ButtonState::Idle,
                };
                if du.refresh_button != new_btn {
                    du.refresh_button = new_btn;
                    redraw = true;
                }

                let new_hover = du.hover_node();
                if du.state.hovered != new_hover {
                    du.state.hovered = new_hover;
                    redraw = true;
                }
                if redraw {
                    du.window.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(du) = self.disk_usage_window.as_mut() {
                    du.pointer_dips = None;
                    let mut redraw = false;
                    if du.update_splitter_hover(None) {
                        redraw = true;
                    }
                    if du.state.hovered.take().is_some() {
                        redraw = true;
                    }
                    if du.refresh_button != crate::disk_usage_window::ButtonState::Idle {
                        du.refresh_button = crate::disk_usage_window::ButtonState::Idle;
                        redraw = true;
                    }
                    if redraw {
                        du.window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let cmd = self.modifiers.super_key();
                // Compute the hit + act in two phases so we can drop
                // the &mut borrow on `disk_usage_window` before
                // calling App-level methods that re-borrow it.
                let hit = {
                    let Some(du) = self.disk_usage_window.as_ref() else {
                        return;
                    };
                    let Some(p) = du.pointer_dips else { return };
                    (du.hit_at(p), p)
                };
                match hit.0 {
                    crate::disk_usage_window::DuHit::None => {}
                    crate::disk_usage_window::DuHit::RefreshButton => {
                        // Press marks Pressed; the action fires on
                        // release inside the button rect (canonical
                        // macOS button behaviour, lets the user drag
                        // off to cancel).
                        if let Some(du) = self.disk_usage_window.as_mut() {
                            du.refresh_button = crate::disk_usage_window::ButtonState::Pressed;
                            du.window.request_redraw();
                        }
                    }
                    crate::disk_usage_window::DuHit::Splitter => {
                        if let Some(du) = self.disk_usage_window.as_mut() {
                            du.begin_splitter_drag(hit.1);
                        }
                    }
                    crate::disk_usage_window::DuHit::LegendChip(filter) => {
                        if let Some(du) = self.disk_usage_window.as_mut() {
                            // Clicking the active chip clears the
                            // filter; clicking another sets it.
                            du.state.category_filter = if du.state.category_filter == filter {
                                None
                            } else {
                                filter
                            };
                            du.window.request_redraw();
                        }
                    }
                    crate::disk_usage_window::DuHit::TopNSortHeader(key) => {
                        if let Some(du) = self.disk_usage_window.as_mut() {
                            du.state.topn_sort = key;
                            du.state.rebuild_topn();
                            du.state.topn_scroll_offset = 0.0;
                            du.window.request_redraw();
                        }
                    }
                    crate::disk_usage_window::DuHit::TreemapNode(node_id)
                    | crate::disk_usage_window::DuHit::TopNRow(node_id) => {
                        if let Some(du) = self.disk_usage_window.as_mut() {
                            if cmd {
                                if !du.state.selection.insert(node_id) {
                                    du.state.selection.remove(&node_id);
                                }
                            } else {
                                du.state.selection.clear();
                                du.state.selection.insert(node_id);
                            }
                            du.window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let mut fire_refresh = false;
                if let Some(du) = self.disk_usage_window.as_mut() {
                    if du.splitter_dragging() {
                        du.end_splitter_drag();
                        du.window.request_redraw();
                    }
                    // Refresh-button release: fire only if the press
                    // started here AND the cursor is still over the
                    // button; otherwise just reset the visual state.
                    if du.refresh_button == crate::disk_usage_window::ButtonState::Pressed {
                        let still_over = du
                            .pointer_dips
                            .map(|p| {
                                matches!(
                                    du.hit_at(p),
                                    crate::disk_usage_window::DuHit::RefreshButton
                                )
                            })
                            .unwrap_or(false);
                        du.refresh_button = if still_over {
                            crate::disk_usage_window::ButtonState::Hover
                        } else {
                            crate::disk_usage_window::ButtonState::Idle
                        };
                        du.window.request_redraw();
                        fire_refresh = still_over;
                    }
                }
                if fire_refresh {
                    self.refresh_disk_usage();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.show_disk_usage_context_menu();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Scroll the Top-N panel when the cursor's over it.
                let Some(du) = self.disk_usage_window.as_mut() else {
                    return;
                };
                let Some(p) = du.pointer_dips else { return };
                let Some(topn) = du.topn_pane() else { return };
                if !topn.contains(p) || p.y < topn.top() + crate::disk_usage_window::TOPN_HEADER_H {
                    return;
                }
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * crate::disk_usage_window::TOPN_ROW_H,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32),
                };
                let rows_h = (topn.size.height - crate::disk_usage_window::TOPN_HEADER_H).max(0.0);
                let content_h =
                    du.state.topn_files.len() as f32 * crate::disk_usage_window::TOPN_ROW_H;
                let max_offset = (content_h - rows_h).max(0.0);
                let new = (du.state.topn_scroll_offset + dy).clamp(0.0, max_offset);
                if (new - du.state.topn_scroll_offset).abs() > 0.5 {
                    du.state.topn_scroll_offset = new;
                    du.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    state: ElementState::Pressed,
                    logical_key,
                    ..
                },
                ..
            } => {
                let Some(du) = self.disk_usage_window.as_mut() else {
                    return;
                };
                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if !du.state.zoom_path.is_empty() {
                            du.zoom_out();
                            du.window.request_redraw();
                        }
                    }
                    Key::Named(NamedKey::Enter) => {
                        // Drilldown into the selected node, if it has children.
                        if let Some(&id) = du.state.selection.iter().next() {
                            du.drilldown(id);
                            du.window.request_redraw();
                        }
                    }
                    Key::Named(NamedKey::Escape) => {
                        if !du.state.selection.is_empty() {
                            du.state.selection.clear();
                            du.window.request_redraw();
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::RedrawRequested => {
                self.paint_disk_usage_window();
            }
            _ => {}
        }
    }
}

/// Deferred state captured by `spawn_disk_usage_window` so the next
/// `resumed`/`user_event` tick (which has `&ActiveEventLoop`) can
/// create the actual `winit::Window` and graft it onto App. This
/// keeps the dispatch handler from needing to reach into winit
/// internals from outside an event-loop callback.
struct PendingDiskUsageOpen {
    state: DiskUsageState,
    task_id: TaskId,
    #[allow(dead_code)] // captured for future use; the worker already has its own clone
    generation: u64,
    #[allow(dead_code)]
    cancel: Arc<std::sync::atomic::AtomicBool>,
    #[allow(dead_code)]
    root_path: std::path::PathBuf,
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Feraille")
            .with_inner_size(LogicalSize::new(1180.0, 760.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                log_error!(56, "create_window failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                log_error!(56, "softbuffer Context::new failed: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                log_error!(56, "softbuffer Surface::new failed: {e}");
                event_loop.exit();
                return;
            }
        };
        // Apply native chrome (transparent titlebar + traffic-light handling
        // on macOS; no-op elsewhere). Returned value is the leading-edge
        // inset to reserve in the tabstrip.
        self.tabstrip.inset_left = feraille_shell_mac::apply_native_chrome(&window);

        // Splice the Services-vending responder into the window's
        // chain so the right-click "Services" submenu can auto-
        // populate (Quick Actions ride this same path on macOS).
        feraille_shell_mac::install_services_anchor(&window);

        // Replace the dock/About icon. Must run after winit has built
        // NSApplication (i.e. not from main()), hence here in resumed().
        let icon_result = feraille_shell_mac::set_app_icon_from_png_bytes(APP_ICON_PNG);
        log_info!(56, "set_app_icon: {:?}", icon_result);

        // Install the app menu bar (App / File / Edit / View / Go /
        // Window submenus, About panel options). Idempotent if
        // resumed() ever fires twice.
        feraille_shell_mac::install_app_menu(
            "Feraille",
            "The file explorer that runs wild",
            env!("CARGO_PKG_VERSION"),
            "© 2026 Feraille Project · MIT OR Apache-2.0",
        );

        // Bridge menu-driven commands back into our event loop. The
        // closure runs on the main thread (AppKit guarantees menu
        // dispatch happens there); it just forwards the CommandId via
        // the proxy and lets `user_event` do the actual work.
        if let Some(proxy) = self.event_proxy.clone() {
            feraille_shell_mac::register_command_callback(Some(Box::new(move |id| {
                let _ = proxy.send_event(AppEvent::Command(id));
            })));
        }

        // Seed the menu's tab-count snapshot so file.close_tab is
        // initially correctly enabled/disabled. Subsequent open/close
        // calls update it directly.
        feraille_shell_mac::set_tab_count(self.tabs.len());

        // Push the initial theme-command checkmarks now that the
        // menu has been built. (App::new ran apply_theme before the
        // menu existed, so the first set_command_state calls were
        // discarded — repeat them here.)
        self.apply_theme();

        // Subscribe to live macOS Appearance changes. The callback
        // fires on the main thread; dispatch back through the proxy
        // so the rest of the work happens in `user_event`.
        if let Some(proxy) = self.event_proxy.clone() {
            feraille_shell_mac::start_system_theme_observer(Box::new(move |dark| {
                let _ = proxy.send_event(AppEvent::SystemThemeChanged { dark });
            }));
        }

        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        self.width = size.width.max(1);
        self.height = size.height.max(1);
        self.scale_factor = scale;
        let font_bytes = match load_default_font() {
            Ok(b) => b,
            Err(e) => {
                log_error!(56, "load_default_font failed: {e:#}");
                event_loop.exit();
                return;
            }
        };
        log_info!(
            56,
            "window resumed: {}x{} @{:.2}x scale, font loaded ({} KiB)",
            self.width.max(1),
            self.height.max(1),
            scale,
            font_bytes.len() / 1024,
        );
        self.window = Some(window);
        self.sb_context = Some(context);
        self.surface = Some(surface);
        self.renderer = Some(SoftRenderer::new(
            self.width,
            self.height,
            scale,
            font_bytes,
        ));
        // Apply previously-saved window size now that the winit
        // window exists. The actual resize fires asynchronously and
        // arrives as a `WindowEvent::Resized` which updates
        // `self.width/height` + the renderer.
        self.apply_persisted_window_size();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        // Drain any pending DU window-open request first so a
        // `Cmd+Shift+D` press realizes the window on the very same
        // event-loop turn that posted the command.
        self.try_realize_disk_usage_window(event_loop);
        match event {
            AppEvent::MagicBatch {
                generation,
                dir,
                results,
            } => {
                if generation != self.magic_generation || self.tabs[self.active].current_dir != dir
                {
                    log_info!(
                        56,
                        "magic batch dropped (stale gen={} != current={})",
                        generation,
                        self.magic_generation
                    );
                    return;
                }
                log_info!(
                    56,
                    "magic batch applied: {} results (gen={})",
                    results.len(),
                    generation
                );
                if let Some(id) = self.magic_task.take() {
                    self.end_task(id);
                }

                let cursor_name = self.cursor_entry_name();
                let scroll = self.list.scroll_offset;
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // Snapshot the size before mutably borrowing the tab,
                // since the DB write needs path/size/mtime.
                let mut writes: Vec<(String, i64, u64, String)> = Vec::new();
                let tab = &mut self.tabs[self.active];
                let mut changed = false;
                for result in results {
                    let path = dir.join(&result.name);
                    let key = (path.clone(), result.mtime_unix);
                    self.magic_cache.insert(key, result.label.clone());
                    let size = tab
                        .all_entries
                        .iter()
                        .find(|e| e.name == result.name && e.mtime_unix == result.mtime_unix)
                        .map(|e| e.size)
                        .unwrap_or(0);
                    if let Some(p) = path.to_str() {
                        writes.push((
                            p.to_string(),
                            result.mtime_unix,
                            size,
                            result.label.clone(),
                        ));
                    }
                    if let Some(entry) = tab.all_entries.iter_mut().find(|entry| {
                        entry.name == result.name && entry.mtime_unix == result.mtime_unix
                    }) {
                        if entry.display_magic != result.label {
                            entry.display_magic = result.label;
                            changed = true;
                        }
                    }
                }
                // Persist after the tab borrow is released.
                if let Some(db) = self.metadata_db.as_ref() {
                    for (path, mtime, size, label) in writes {
                        let _ = db.upsert_file(&feraille_meta::FileMetaRecord {
                            path,
                            mtime_unix: mtime,
                            size,
                            magic_label: Some(label),
                            partial_hash: None,
                            full_hash: None,
                            mime: None,
                            quarantined: None,
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
                            indexed_at_unix: now_unix,
                        });
                    }
                }

                if changed {
                    self.rebuild_visible_entries(cursor_name, true);
                    self.list.scroll_offset = scroll;
                    self.request_redraw();
                }
            }
            AppEvent::QuarantineBatch {
                generation,
                dir,
                results,
            } => {
                if generation != self.quarantine_generation
                    || self.tabs[self.active].current_dir != dir
                {
                    log_info!(
                        60,
                        "quarantine batch dropped (stale gen={} != current={})",
                        generation,
                        self.quarantine_generation
                    );
                    return;
                }
                log_info!(
                    60,
                    "quarantine batch applied: {} results (gen={})",
                    results.len(),
                    generation
                );
                if let Some(id) = self.quarantine_task.take() {
                    self.end_task(id);
                }

                let cursor_name = self.cursor_entry_name();
                let scroll = self.list.scroll_offset;
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // Snapshot DB writes before borrowing the tab mutably
                // so the persistence pass doesn't fight borrow rules.
                let mut writes: Vec<(String, i64, u64, bool, Option<QuarantineDetails>)> =
                    Vec::new();
                let tab = &mut self.tabs[self.active];
                let mut changed = false;
                for result in results {
                    let path = dir.join(&result.name);
                    let key = (path.clone(), result.mtime_unix);
                    let cached_details = if result.quarantined {
                        Some(result.details.clone())
                    } else {
                        None
                    };
                    self.quarantine_cache
                        .insert(key, cached_details.clone());
                    let size = tab
                        .all_entries
                        .iter()
                        .find(|e| e.name == result.name && e.mtime_unix == result.mtime_unix)
                        .map(|e| e.size)
                        .unwrap_or(0);
                    if let Some(p) = path.to_str() {
                        writes.push((
                            p.to_string(),
                            result.mtime_unix,
                            size,
                            result.quarantined,
                            cached_details,
                        ));
                    }
                    if let Some(entry) = tab.all_entries.iter_mut().find(|entry| {
                        entry.name == result.name && entry.mtime_unix == result.mtime_unix
                    }) {
                        let new_flag = result.quarantined;
                        let new_details = if result.quarantined {
                            Some(result.details)
                        } else {
                            Some(QuarantineDetails::default())
                        };
                        if entry.is_quarantined != new_flag
                            || entry.quarantine.as_ref() != new_details.as_ref()
                        {
                            entry.is_quarantined = new_flag;
                            entry.quarantine = new_details;
                            changed = true;
                        }
                    }
                }
                if let Some(db) = self.metadata_db.as_ref() {
                    for (path, mtime, size, quarantined, details) in writes {
                        let where_from = details.as_ref().map(|d| d.where_from.join("\n"));
                        let agent = details.as_ref().and_then(|d| d.agent.clone());
                        let iso = details.as_ref().and_then(|d| d.downloaded_iso.clone());
                        let _ = db.upsert_file(&feraille_meta::FileMetaRecord {
                            path,
                            mtime_unix: mtime,
                            size,
                            magic_label: None,
                            partial_hash: None,
                            full_hash: None,
                            mime: None,
                            quarantined: Some(quarantined),
                            quarantine_agent: agent,
                            quarantine_iso: iso,
                            quarantine_where_from: where_from,
                            indexed_at_unix: now_unix,
                        });
                    }
                }

                if changed {
                    self.rebuild_visible_entries(cursor_name, true);
                    self.list.scroll_offset = scroll;
                    self.request_redraw();
                }
            }
            AppEvent::IconChunkTick { generation } => {
                if generation != self.icon_generation {
                    return;
                }
                let mut redraw = false;
                for _ in 0..ICON_CHUNK_SIZE {
                    let Some((key, path)) = self.icon_queue.pop() else {
                        break;
                    };
                    if self.icon_cache.contains_key(&key) {
                        continue;
                    }
                    if let Some((rgba, w, h)) = fetch_icon_rgba(&path, self.icon_size_px) {
                        self.icon_cache.insert(key, Bitmap::new(w, h, rgba));
                        redraw = true;
                    }
                }
                if redraw {
                    self.request_redraw();
                }
                if self.icon_queue.is_empty() {
                    if let Some(id) = self.icon_task.take() {
                        self.end_task(id);
                        log_info!(56, "icon prefetch: complete (gen={})", self.icon_generation);
                    }
                } else if let Some(proxy) = self.event_proxy.clone() {
                    let _ = proxy.send_event(AppEvent::IconChunkTick {
                        generation: self.icon_generation,
                    });
                }
            }
            AppEvent::Command(id) => self.dispatch_command(id),
            AppEvent::EnumerationBatch {
                generation,
                dir,
                mut entries,
            } => {
                if generation != self.enumeration_generation
                    || self.tabs[self.active].current_dir != dir
                {
                    return;
                }
                filter_hidden(&mut entries, self.show_hidden);
                if self.enumeration_pending_first_batch {
                    // First batch for this generation — swap out the
                    // held previous-folder rows. Single-shot so later
                    // batches in the same enumeration just append.
                    let tab = &mut self.tabs[self.active];
                    tab.all_entries.clear();
                    tab.error = None;
                    self.enumeration_pending_first_batch = false;
                }
                let tab = &mut self.tabs[self.active];
                tab.all_entries.extend(entries);
                let cursor_name = self
                    .enumeration_preserve_cursor
                    .clone()
                    .or_else(|| self.cursor_entry_name());
                let preserve_scroll = self.enumeration_preserve_scroll > 0.0;
                let saved_scroll = self.enumeration_preserve_scroll;
                self.rebuild_visible_entries(cursor_name, preserve_scroll);
                if preserve_scroll {
                    self.list.scroll_offset = saved_scroll;
                }
                self.request_redraw();
            }
            AppEvent::EnumerationDone {
                generation,
                dir,
                error,
            } => {
                if generation != self.enumeration_generation {
                    log_info!(
                        59,
                        "enumeration done dropped (stale gen={} != {})",
                        generation,
                        self.enumeration_generation
                    );
                    return;
                }
                if self.tabs[self.active].current_dir != dir {
                    return;
                }
                if let Some(id) = self.enumeration_task.take() {
                    self.end_task(id);
                }
                self.enumeration_cancel = None;
                self.enumeration_preserve_cursor = None;
                self.enumeration_preserve_scroll = 0.0;
                if self.enumeration_pending_first_batch {
                    // Zero batches arrived — empty folder or an error
                    // before any rows. Drop the held previous-folder
                    // rows now so paint reflects the new (empty) state.
                    let tab = &mut self.tabs[self.active];
                    tab.all_entries.clear();
                    tab.error = None;
                    self.enumeration_pending_first_batch = false;
                    self.rebuild_visible_entries(None, false);
                }
                let entry_count = self.tabs[self.active].all_entries.len();
                if let Some(err) = error {
                    log_warn!(
                        59,
                        "enumeration error after {} rows: {:?}",
                        entry_count,
                        err
                    );
                    self.tabs[self.active].error = Some(err.clone());
                    if entry_count == 0 {
                        // No rows arrived — empty-state panel takes over;
                        // toast would be redundant.
                    } else {
                        self.toast_error(format!("Listing error: {err:?}"));
                    }
                } else {
                    log_info!(
                        59,
                        "enumeration done: {} rows (gen={})",
                        entry_count,
                        generation
                    );
                }
                // Both prefetches read the populated listing; fire now
                // that the enumeration has settled.
                self.prefetch_icons();
                self.start_magic_prefetch();
                self.start_quarantine_prefetch();
                self.request_redraw();
            }
            AppEvent::TreeChildrenLoaded {
                generation,
                id,
                mut entries,
                error,
            } => {
                if self.tree_pending.get(&id) != Some(&generation) {
                    return;
                }
                self.tree_pending.remove(&id);
                if let Some(err) = error {
                    log_warn!(59, "tree-load error for id={:?}: {:?}", id, err);
                    return;
                }
                filter_hidden(&mut entries, self.show_hidden);
                self.tree.populate_children(id, &entries);
                // The reveal target may have just become visible (this
                // batch was for one of its ancestors). Cheap if it's
                // not — `ensure_visible` no-ops when the id isn't in
                // the visible row list.
                if let Some(selected) = self.tree.selected {
                    let viewport_h = self.tree_rect().size.height;
                    self.tree.ensure_visible(selected, viewport_h);
                }
                self.request_redraw();
            }
            AppEvent::SystemThemeChanged { dark } => {
                if self.system_is_dark == dark {
                    return;
                }
                self.system_is_dark = dark;
                // Only repaint when we're actually following the
                // system. Pinned Light/Dark users still get the
                // cached state updated for when they next switch
                // back to System.
                if self.theme_preference == ThemePreference::System {
                    self.apply_theme();
                    self.request_redraw();
                }
            }
            AppEvent::PreviewThumbReady {
                generation,
                path,
                mtime_unix,
                rgba,
                width,
                height,
            } => {
                if generation != self.preview_generation {
                    return;
                }
                // The cache key uses the requested size (longest edge
                // we asked qlmanage for), not the actual delivered
                // dimensions — they don't always match because
                // qlmanage scales to fit the type's native ratio.
                let key = (path.clone(), mtime_unix, Self::PREVIEW_THUMB_PX);
                self.preview_pending.remove(&key);
                self.preview_failed.remove(&key);
                self.preview_cache.insert(key, Bitmap::new(width, height, rgba));
                self.request_redraw();
            }
            AppEvent::PreviewThumbFailed {
                generation,
                path,
                mtime_unix,
                size_px,
            } => {
                if generation != self.preview_generation {
                    return;
                }
                let key = (path, mtime_unix, size_px);
                self.preview_pending.remove(&key);
                self.preview_failed.insert(key);
                self.request_redraw();
            }
            AppEvent::DiskUsageBatch { generation, facts } => {
                let Some(du) = self.disk_usage_window.as_mut() else {
                    return;
                };
                if generation != du.state.generation {
                    return;
                }
                du.apply_batch(&facts);
                du.window.request_redraw();
            }
            AppEvent::DiskUsageProgress { generation, stats } => {
                let Some(du) = self.disk_usage_window.as_mut() else {
                    return;
                };
                if generation != du.state.generation {
                    return;
                }
                du.state.stats = stats;
                du.window.request_redraw();
            }
            AppEvent::DiskUsageDone { generation, error } => {
                let Some(du) = self.disk_usage_window.as_mut() else {
                    return;
                };
                if generation != du.state.generation {
                    return;
                }
                du.mark_complete();
                du.state.error = error;
                if let Some(id) = du.state.task_id.take() {
                    self.end_task(id);
                }
                if let Some(du) = self.disk_usage_window.as_ref() {
                    du.window.request_redraw();
                }
            }
            AppEvent::FileOpComplete {
                op,
                task_id,
                dest_dir,
                result,
            } => {
                self.end_task(task_id);
                match result {
                    Ok(_) => {
                        // Only refresh if the op landed in the
                        // currently-shown directory; otherwise the
                        // user has navigated and a refresh would
                        // surprise them.
                        if self.tabs[self.active].current_dir == dest_dir {
                            self.refresh_active_tab();
                        }
                    }
                    Err(e) => {
                        let verb = match op {
                            FileOpKind::Duplicate => "duplicate",
                            FileOpKind::Compress => "compress",
                        };
                        log_warn!(60, "file op {verb} failed: {e}");
                        self.toast_error(format!("Couldn't {verb}: {e}"));
                    }
                }
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Drain any pending DU window-open request first.
        self.try_realize_disk_usage_window(event_loop);

        // Route to the disk-usage window when the event targets it.
        if let Some(du) = self.disk_usage_window.as_mut() {
            if du.window.id() == id {
                self.handle_disk_usage_window_event(event);
                return;
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                // Snapshot state to the metadata DB before tearing
                // down so the next launch can restore. Best-effort.
                self.save_persistent_state();
                event_loop.exit();
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                self.width = width.max(1);
                self.height = height.max(1);
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(self.width, self.height);
                }
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor as f32;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_scale_factor(self.scale_factor);
                }
                self.request_redraw();
            }
            WindowEvent::CursorMoved {
                position: PhysicalPosition { x, y },
                ..
            } => {
                let p = FPoint::new(
                    (x as f32) / self.scale_factor,
                    (y as f32) / self.scale_factor,
                );
                self.pointer_dips = Some(p);
                let mut redraw = false;

                // Drag-out: promote the watch into a system drag once
                // the cursor has moved past the threshold and the delay
                // has elapsed. Once kicked, the OS owns the drag visual
                // until the user drops or cancels.
                if let Some(watch) = self.drag_watch {
                    let dx = p.x - watch.start.x;
                    let dy = p.y - watch.start.y;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist > DRAG_THRESHOLD_DIPS
                        && watch.when.elapsed().as_millis() > DRAG_DELAY_MS
                    {
                        self.drag_watch = None;
                        if let Some(entry) = self.tabs[self.active].entries.get(watch.row).cloned()
                        {
                            let path = self.tabs[self.active].current_dir.join(&entry.name);
                            obs::breadcrumb(format_args!(
                                "drag promote row={} name={} path={} dist={:.1} elapsed_ms={}",
                                watch.row,
                                entry.name,
                                path.display(),
                                dist,
                                watch.when.elapsed().as_millis(),
                            ));
                            if let Some(window) = &self.window {
                                let kicked =
                                    feraille_shell_mac::begin_drag(window, &[path.as_path()]);
                                obs::breadcrumb(format_args!(
                                    "drag begin_drag returned kicked={} path={}",
                                    kicked,
                                    path.display(),
                                ));
                            } else {
                                obs::breadcrumb(format_args!(
                                    "drag promote aborted: no window for path={}",
                                    path.display(),
                                ));
                            }
                        }
                    }
                }

                if self.scrollbar.is_dragging() {
                    if let Some(off) = self.scrollbar.scroll_offset_for_drag(
                        self.scrollbar_rect(),
                        p.y,
                        self.list_content_height(),
                        self.list_inner_rect().size.height,
                    ) {
                        self.list.scroll_offset = off;
                        redraw = true;
                    }
                } else if self.splitter.is_dragging() {
                    if let Some(pos) = self.splitter.position_for_drag(p) {
                        self.splitter_x = pos;
                        redraw = true;
                    }
                } else if self.preview_splitter.is_dragging() {
                    if let Some(pos) = self.preview_splitter.position_for_drag(p) {
                        let (w, _) = self.viewport_size_dips();
                        self.preview_width = (w - pos).clamp(PREVIEW_W_MIN, PREVIEW_W_MAX);
                        redraw = true;
                    }
                } else {
                    if self.tabstrip.update_hover(
                        self.tabstrip_rect(),
                        &self
                            .tabs
                            .iter()
                            .map(|t| TabInfo { label: t.label() })
                            .collect::<Vec<_>>(),
                        Some(p),
                    ) {
                        redraw = true;
                    }
                    if self
                        .breadcrumb
                        .update_hover(self.breadcrumb_rect(), Some(p))
                    {
                        redraw = true;
                    }
                    if self.tree.update_hover(self.tree_rect(), Some(p)) {
                        redraw = true;
                    }
                    let count = self.tabs[self.active].entries.len();
                    if self
                        .list
                        .update_hover(self.list_inner_rect(), Some(p), count)
                    {
                        redraw = true;
                    }
                    if self.list.update_header_hover(self.header_rect(), Some(p)) {
                        redraw = true;
                    }
                    // Splitter hover affordance.
                    let container = self.splitter_container();
                    if self
                        .splitter
                        .update_hover(self.splitter_x, container, Some(p))
                    {
                        redraw = true;
                    }
                    if let Some(px) = self.preview_splitter_x() {
                        if self.preview_splitter.update_hover(px, container, Some(p)) {
                            redraw = true;
                        }
                    }
                }
                if redraw {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_dips = None;
                self.breadcrumb.update_hover(self.breadcrumb_rect(), None);
                self.tree.update_hover(self.tree_rect(), None);
                let count = self.tabs[self.active].entries.len();
                self.list.update_hover(self.list_inner_rect(), None, count);
                self.list.update_header_hover(self.header_rect(), None);
                let container = self.splitter_container();
                self.splitter.update_hover(self.splitter_x, container, None);
                if let Some(px) = self.preview_splitter_x() {
                    self.preview_splitter.update_hover(px, container, None);
                }
                self.request_redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(p) = self.pointer_dips else { return };

                // Keyboard-shortcuts modal — topmost when open. Click
                // on the close button dismisses; click anywhere else
                // (including outside the panel) is swallowed so the
                // overlay behaves as a true modal.
                if self.shortcuts_modal.is_some() {
                    let (vp_w, vp_h) = self.viewport_size_dips();
                    let (panel, _body) = self.shortcuts_panel_layout(
                        &self.tokens,
                        feraille_render::Size::new(vp_w, vp_h),
                    );
                    let close_rect = Self::shortcuts_close_rect_from_panel(panel);
                    if close_rect.contains(p) {
                        self.close_shortcuts_modal();
                        self.request_redraw();
                    }
                    return;
                }

                // Settings modal — same topmost behaviour. Hits route
                // to the matching control; outside-panel clicks dismiss.
                if self.settings_modal.is_some() {
                    let (vp_w, vp_h) = self.viewport_size_dips();
                    let viewport = feraille_render::Size::new(vp_w, vp_h);
                    let tokens = self.tokens.clone();
                    match self.settings_hit(p, viewport, &tokens) {
                        None => {
                            self.close_settings();
                        }
                        Some(SettingsHit::Close) => {
                            self.close_settings();
                        }
                        Some(SettingsHit::Category(cat)) => {
                            if let Some(m) = self.settings_modal.as_mut() {
                                m.category = cat;
                            }
                        }
                        Some(SettingsHit::ThemeTile(pref)) => {
                            self.set_theme_preference(pref);
                        }
                        Some(SettingsHit::ToggleHidden) => {
                            self.toggle_hidden();
                        }
                        Some(SettingsHit::SidebarWidthSnap(snap)) => {
                            self.splitter_x = snap.px();
                            self.save_app_prefs();
                        }
                        Some(SettingsHit::Inside) => {}
                    }
                    self.request_redraw();
                    return;
                }

                // Task panel — when open, it's the topmost popover.
                // Hits inside route to cancel or are swallowed; hits
                // outside close the panel without bleeding through to
                // whatever was underneath.
                if self.task_panel_open {
                    let (vp_w, vp_h) = self.viewport_size_dips();
                    let vp_rect = FRect::new(0.0, 0.0, vp_w, vp_h);
                    match task_panel::hit_test(vp_rect, &self.tasks, p) {
                        task_panel::HitTest::Cancel(id) => {
                            self.cancel_task(id);
                            return;
                        }
                        task_panel::HitTest::Background => {
                            self.request_redraw();
                            return;
                        }
                        task_panel::HitTest::Outside => {
                            self.task_panel_open = false;
                            self.request_redraw();
                            // Fall through so the click can land on
                            // whatever it was actually targeting (e.g.
                            // a folder row).
                        }
                    }
                }

                // Status bar — clicking anywhere on the row toggles the
                // task panel, but only when at least one task is in
                // flight. Otherwise the click falls through (today,
                // status is just informational).
                if !self.tasks.is_empty() && self.status_bar_rect().contains(p) {
                    self.task_panel_open = !self.task_panel_open;
                    self.request_redraw();
                    return;
                }

                // Inline rename — click inside the editor: stay editing
                // (consume so the click doesn't fall through to selection).
                // Click outside: commit. Either way, the click is consumed
                // for v1 — Finder commits and lets the click through, but
                // that complicates "click another row to edit it" which
                // we'd rather defer.
                if self.inline_rename.is_some() {
                    let inside = self
                        .inline_rename_rect
                        .map(|r| r.contains(p))
                        .unwrap_or(false);
                    if inside {
                        self.request_redraw();
                        return;
                    }
                    self.commit_inline_rename();
                    self.request_redraw();
                    return;
                }

                // Properties panel — when open, any click closes it.
                // (Iter-3.10 can refine to "click-outside-only" if useful.)
                if self.properties_target.is_some() {
                    let _ = p;
                    self.close_properties();
                    self.request_redraw();
                    return;
                }

                // Splitter (highest priority — narrow but layered above).
                if self
                    .splitter
                    .begin_drag_at(self.splitter_x, self.splitter_container(), p)
                {
                    self.request_redraw();
                    return;
                }
                // Preview-pane splitter — only present when preview is
                // visible. Update min/max from current viewport so the
                // pane can't collapse the file pane below ~200 DIPs.
                if let Some(px) = self.preview_splitter_x() {
                    let (w, _) = self.viewport_size_dips();
                    self.preview_splitter.min = (w - PREVIEW_W_MAX).max(self.splitter_x + 200.0);
                    self.preview_splitter.max = w - PREVIEW_W_MIN;
                    if self
                        .preview_splitter
                        .begin_drag_at(px, self.splitter_container(), p)
                    {
                        self.request_redraw();
                        return;
                    }
                }
                // Column header — toggle sort.
                if let Some(id) = self.list.header_column_at(self.header_rect(), p) {
                    self.cycle_sort(id);
                    self.request_redraw();
                    return;
                }
                // Scrollbar thumb.
                let inner = self.list_inner_rect();
                if self.scrollbar.begin_drag_at(
                    self.scrollbar_rect(),
                    p,
                    self.list_content_height(),
                    inner.size.height,
                    self.list.scroll_offset,
                ) {
                    self.request_redraw();
                    return;
                }
                // Tabstrip.
                let tab_infos: Vec<TabInfo> = self
                    .tabs
                    .iter()
                    .map(|t| TabInfo { label: t.label() })
                    .collect();
                if let Some(ev) = self.tabstrip.click(self.tabstrip_rect(), &tab_infos, p) {
                    match ev {
                        TabStripEvent::Activate(i) => self.switch_tab(i),
                        TabStripEvent::Close(i) => self.close_tab(i),
                        TabStripEvent::New => self.new_tab(),
                    }
                    self.request_redraw();
                    return;
                }
                // Breadcrumb.
                if let Some(BreadcrumbEvent::Navigate(path)) =
                    self.breadcrumb.click(self.breadcrumb_rect(), p)
                {
                    self.navigate(path);
                    self.request_redraw();
                    return;
                }
                // Tree — `Some(events)` even if empty signals "I handled
                // this click; redraw" (e.g. fold of cached expanded folder
                // mutates state but emits no event). `None` = missed.
                if let Some(tree_events) = self.tree.click(self.tree_rect(), p) {
                    self.set_focused_pane(FocusedPane::Tree);
                    for ev in tree_events {
                        self.handle_tree_event(ev);
                    }
                    self.request_redraw();
                    return;
                }
                // List click.
                if let Some(idx) =
                    self.list
                        .index_at(inner, p, self.tabs[self.active].entries.len())
                {
                    self.set_focused_pane(FocusedPane::List);
                    self.tabs[self.active].selection.set_cursor(idx);
                    if let Some(entry) = self.tabs[self.active].entries.get(idx) {
                        obs::breadcrumb(format_args!(
                            "drag armed row={} name={} start=({:.1},{:.1})",
                            idx, entry.name, p.x, p.y,
                        ));
                    } else {
                        obs::breadcrumb(format_args!(
                            "drag armed row={} start=({:.1},{:.1})",
                            idx, p.x, p.y,
                        ));
                    }
                    // Arm drag-out: a small motion + delay after mouse-down
                    // on a row promotes to a system drag.
                    self.drag_watch = Some(DragWatch {
                        start: p,
                        when: std::time::Instant::now(),
                        row: idx,
                    });
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let Some(p) = self.pointer_dips else { return };
                if self.tree_rect().contains(p) {
                    self.show_tree_context_menu_at(p);
                } else {
                    self.show_context_menu_at(p);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.scrollbar.is_dragging() {
                    self.scrollbar.end_drag();
                    self.request_redraw();
                }
                if self.splitter.is_dragging() {
                    self.splitter.end_drag();
                    // Persist the new sidebar width once the drag
                    // settles; intermediate frames don't churn the
                    // prefs file.
                    self.save_app_prefs();
                    self.request_redraw();
                }
                if self.preview_splitter.is_dragging() {
                    self.preview_splitter.end_drag();
                    self.request_redraw();
                }
                if self.drag_watch.is_some() {
                    obs::breadcrumb(format_args!("drag watch cleared on mouse release"));
                }
                self.drag_watch = None;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // PixelDelta arrives from trackpad / high-precision wheels in
                // logical pixels — pass through. LineDelta is coarse "lines"
                // from a notched wheel; multiply by ~2 rows. Sign: positive
                // delta from winit means scroll forward / content-up; we
                // invert because `scroll_offset` is "DIPs below origin",
                // increasing as content moves up.
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 56.0,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
                };
                // Shortcuts overlay eats wheel input while open — it's
                // a modal, scrolling under it would feel disconnected.
                if let Some(modal) = self.shortcuts_modal.as_mut() {
                    modal.scroll_offset = (modal.scroll_offset + dy).max(0.0);
                    // The upper clamp is applied at paint time once we
                    // know `total_h`, so values past the end snap back
                    // on the next frame.
                    self.request_redraw();
                    return;
                }
                // Route to whichever pane the pointer is over. macOS Finder
                // does the same — scrolling over the sidebar scrolls the
                // sidebar, not the file pane. If we don't know where the
                // pointer is (e.g. the trackpad fired with no prior
                // CursorMoved), fall back to the file list.
                let target = match self.pointer_dips {
                    Some(p) if self.tree_rect().contains(p) => ScrollTarget::Tree,
                    Some(p) if self.list_pane_rect().contains(p) => ScrollTarget::List,
                    Some(_) => ScrollTarget::None,
                    None => ScrollTarget::List,
                };
                match target {
                    ScrollTarget::Tree => {
                        let viewport_h = self.tree_rect().size.height;
                        self.tree.scroll_by(dy, viewport_h);
                        self.request_redraw();
                    }
                    ScrollTarget::List => {
                        let count = self.tabs[self.active].entries.len();
                        let viewport_h = self.list_inner_rect().size.height;
                        self.list.scroll_by(dy, count, viewport_h);
                        self.request_redraw();
                    }
                    ScrollTarget::None => {}
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                // Inline rename — route input to its TextInput. Submit on
                // Enter, cancel on Escape; everything else (printable text,
                // arrow keys, backspace) goes through the input.
                if self.inline_rename.is_some() {
                    if let Key::Named(named) = &logical_key {
                        if let Some(tk) = map_named_to_textinput(*named) {
                            let event = self
                                .inline_rename
                                .as_mut()
                                .expect("checked")
                                .input
                                .handle_key(tk);
                            match event {
                                Some(TextInputEvent::Submit(_)) => self.commit_inline_rename(),
                                Some(TextInputEvent::Cancel) => self.cancel_inline_rename(),
                                None => {}
                            }
                            self.request_redraw();
                            return;
                        }
                    }
                    if let Some(t) = text.as_deref() {
                        self.inline_rename
                            .as_mut()
                            .expect("checked")
                            .input
                            .handle_text(t);
                        self.request_redraw();
                    }
                    return;
                }

                // Modal text dialog — route all input there, swallow others.
                if let Some(d) = self.dialog.as_mut() {
                    if let Key::Named(named) = &logical_key {
                        if let Some(tk) = map_named_to_textinput(*named) {
                            match d.input.handle_key(tk) {
                                Some(TextInputEvent::Submit(_)) => self.submit_dialog(),
                                Some(TextInputEvent::Cancel) => self.close_dialog(),
                                None => {}
                            }
                            self.request_redraw();
                            return;
                        }
                    }
                    if let Some(t) = text.as_deref() {
                        d.input.handle_text(t);
                        self.request_redraw();
                    }
                    return;
                }

                // Keyboard-shortcuts overlay — eat all input while
                // Settings modal — Escape closes; everything else
                // is swallowed so a stray keystroke can't flow into
                // the underlying file pane.
                if self.settings_modal.is_some() {
                    if let Key::Named(winit::keyboard::NamedKey::Escape) = &logical_key {
                        self.close_settings();
                        self.request_redraw();
                    }
                    return;
                }

                // open. Escape closes, Enter does nothing (no submit
                // semantics here), every other key feeds the filter.
                if self.shortcuts_modal.is_some() {
                    if let Some(modal) = self.shortcuts_modal.as_mut() {
                        if let Key::Named(named) = &logical_key {
                            if let Some(tk) = map_named_to_textinput(*named) {
                                match modal.filter.handle_key(tk) {
                                    Some(TextInputEvent::Cancel) => {
                                        self.shortcuts_modal = None;
                                    }
                                    Some(TextInputEvent::Submit(_)) => {
                                        // No-op: there's nothing to
                                        // commit. Keep the overlay
                                        // open; user can keep typing
                                        // or hit Esc.
                                    }
                                    None => {
                                        // Filter changed → snap scroll
                                        // back to the top so the new
                                        // matches are visible.
                                        modal.scroll_offset = 0.0;
                                    }
                                }
                            }
                        } else if let Some(t) = text.as_deref() {
                            modal.filter.handle_text(t);
                            modal.scroll_offset = 0.0;
                        }
                    }
                    self.request_redraw();
                    return;
                }

                // Search/filter dialog — update visible rows live as text changes.
                if self.search.is_some() {
                    let mut close = false;
                    let mut new_value: Option<String> = None;
                    if let Some(input) = self.search.as_mut() {
                        if let Key::Named(named) = &logical_key {
                            if let Some(tk) = map_named_to_textinput(*named) {
                                match input.handle_key(tk) {
                                    Some(TextInputEvent::Submit(s)) => {
                                        new_value = Some(s);
                                        close = true;
                                    }
                                    Some(TextInputEvent::Cancel) => close = true,
                                    None => new_value = Some(input.value()),
                                }
                            }
                        } else if let Some(t) = text.as_deref() {
                            input.handle_text(t);
                            new_value = Some(input.value());
                        }
                    }
                    if let Some(value) = new_value {
                        self.set_filter_text(value);
                    }
                    if close {
                        self.search = None;
                    }
                    self.request_redraw();
                    return;
                }

                // If the breadcrumb is in edit mode, route keyboard there.
                if self.breadcrumb.is_editing() {
                    if let Key::Named(named) = &logical_key {
                        if let Some(tk) = map_named_to_textinput(*named) {
                            if let Some(BreadcrumbEvent::Navigate(path)) =
                                self.breadcrumb.handle_key(tk)
                            {
                                self.navigate(path);
                            }
                            self.request_redraw();
                            return;
                        }
                    }
                    if let Some(t) = text.as_deref() {
                        self.breadcrumb.handle_text(t);
                        self.request_redraw();
                    }
                    return;
                }

                // Task panel — Escape closes it. Sits above the
                // catalogue dispatch so the user can dismiss the panel
                // without firing any escape-bound command (none today,
                // but defensive). Does not stop the underlying tasks.
                if self.task_panel_open && matches!(&logical_key, Key::Named(NamedKey::Escape)) {
                    self.task_panel_open = false;
                    self.request_redraw();
                    return;
                }

                // Single source of truth for keyboard shortcuts:
                // walk `feraille_core::commands::all_commands()` and
                // dispatch the first match. Every Feraille-owned key
                // lives in the catalogue — adding / rebinding /
                // removing happens there, not here. See
                // `keystroke_to_command` above.
                if let Some(id) = keystroke_to_command(&logical_key, self.modifiers) {
                    self.dispatch_command(id);
                    return;
                }

                // Type-ahead: a printable character with no
                // Cmd/Ctrl/Alt held, when the tree pane has focus.
                // Not a shortcut — a text-input mode that runs after
                // every catalogue match has missed. The list pane has
                // no type-ahead today.
                let mods = self.modifiers;
                if !mods.super_key() && !mods.control_key() && !mods.alt_key() {
                    if let Some(t) = text.as_deref() {
                        if let Some(ch) = t.chars().next() {
                            if !ch.is_control()
                                && matches!(self.focused_pane, FocusedPane::Tree)
                            {
                                let tree_h = self.tree_rect().size.height;
                                if self.tree.type_ahead_push(ch, tree_h) {
                                    self.request_redraw();
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }
}

fn map_named_to_textinput(named: NamedKey) -> Option<TextInputKey> {
    Some(match named {
        NamedKey::Backspace => TextInputKey::Backspace,
        NamedKey::Delete => TextInputKey::Delete,
        NamedKey::ArrowLeft => TextInputKey::ArrowLeft,
        NamedKey::ArrowRight => TextInputKey::ArrowRight,
        NamedKey::Home => TextInputKey::Home,
        NamedKey::End => TextInputKey::End,
        NamedKey::Enter => TextInputKey::Enter,
        NamedKey::Escape => TextInputKey::Escape,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum ScrollTarget {
    Tree,
    List,
    None,
}

fn format_iso_date(unix: i64) -> String {
    // Reuse the same approach as feraille-fs-native's humanize: derive
    // Y/M/D from days-since-epoch via Howard Hinnant's algorithm.
    let secs_in_day = unix.rem_euclid(86_400);
    let h = (secs_in_day / 3600) as u32;
    let m = ((secs_in_day % 3600) / 60) as u32;
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02} UTC")
}

/// Canonical key/value rows for the file inspectors. Both the preview
/// pane (Cmd+P) and the Get-Info modal (Cmd+I) call this so they stay
/// in lockstep — adding or renaming a row here updates both surfaces.
/// `full_path` is the absolute path used for the "Where" row.
fn info_rows(entry: &FileEntry, full_path: &Path) -> Vec<(&'static str, String)> {
    let path_str = full_path.to_string_lossy().into_owned();
    let size_text = if matches!(entry.kind, EntryKind::Directory) {
        String::from("—")
    } else if entry.size >= 1024 {
        format!("{} ({} bytes)", entry.display_size, entry.size)
    } else {
        format!("{} bytes", entry.size)
    };
    let magic_text = if entry.display_magic.is_empty() {
        String::from("—")
    } else {
        entry.display_magic.clone()
    };
    let mtime_iso = format_iso_date(entry.mtime_unix);

    let mut rows: Vec<(&'static str, String)> = vec![
        ("Where", path_str),
        ("Kind", entry.display_kind.clone()),
        ("Size", size_text),
        ("Modified", mtime_iso),
        ("Magic", magic_text),
    ];

    if entry.is_quarantined {
        let q_value = match entry.quarantine.as_ref().and_then(|d| d.agent.clone()) {
            Some(agent) => format!("Yes — {agent}"),
            None if entry.quarantine.is_some() => String::from("Yes"),
            None => String::from("Yes — loading…"),
        };
        rows.push(("Quarantined", q_value));
        if let Some(details) = entry.quarantine.as_ref() {
            if let Some(iso) = &details.downloaded_iso {
                rows.push(("Downloaded", iso.clone()));
            }
            if !details.where_from.is_empty() {
                let first = details.where_from.first().cloned().unwrap_or_default();
                let value = if details.where_from.len() > 1 {
                    format!("{first} (+{} more)", details.where_from.len() - 1)
                } else {
                    first
                };
                rows.push(("Downloaded from", value));
            }
        }
    }

    rows
}

/// Paint card chrome — `bg.layer1` fill + 1-DIP `border.default` stroke.
/// Used by the preview pane's section cards. Lifted to a helper so the
/// look stays consistent and is easy to swap later.
fn paint_card_chrome(rect: FRect, tokens: &Tokens, painter: &mut dyn Renderer) {
    painter.fill_rect(rect, tokens.bg.layer1);
    painter.stroke_rect(rect, 1.0, tokens.border.default);
}

fn cache_key_for(entry: &FileEntry) -> String {
    match entry.kind {
        EntryKind::Directory => "DIR".to_string(),
        EntryKind::Symlink => "SYMLINK".to_string(),
        EntryKind::File => match entry.name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => format!(".{}", ext.to_lowercase()),
            _ => "FILE".to_string(),
        },
    }
}

/// Cache key for a per-path tree-row icon (Volumes / Locations sections).
/// macOS returns a distinct icon per mounted volume + per known special
/// folder (Macintosh HD, Home, iCloud Drive, Trash, USB / external / DMG /
/// network glyphs, custom .VolumeIcon.icns); namespace them under `PATH:`
/// so they can't collide with `cache_key_for`'s `DIR` / `.ext` / `FILE` /
/// `SYMLINK` keys.
fn path_icon_key(path: &Path) -> String {
    format!("PATH:{}", path.display())
}

fn filter_hidden(entries: &mut Vec<FileEntry>, show_hidden: bool) {
    if !show_hidden {
        entries.retain(|e| !e.name.starts_with('.'));
    }
}

fn filter_entries(entries: &[FileEntry], filter_text: &str) -> Vec<FileEntry> {
    let query = filter_text.trim().to_lowercase();
    if query.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&query)
                || e.display_kind.to_lowercase().contains(&query)
                || e.display_magic.to_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

/// Centered message in the file pane when `enumerate` failed. Most
/// commonly TCC permission-denied for ~/Documents, ~/Desktop, or
/// ~/Downloads when a sandboxed launcher hasn't been granted access.
fn paint_empty_state(
    rect: FRect,
    error: &EnumerationError,
    path: &Path,
    tokens: &Tokens,
    painter: &mut dyn Renderer,
) {
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return;
    }
    let (heading, body) = match error {
        EnumerationError::PermissionDenied => (
            "macOS denied access to this folder",
            format!(
                "Grant access in System Settings \u{2192} Privacy & Security \u{2192} Files and Folders, \
or run Feraille from outside a sandboxed launcher.\n\nPath: {}",
                path.display()
            ),
        ),
        EnumerationError::NotFound => (
            "This folder is gone",
            format!("It may have been moved or deleted.\n\nPath: {}", path.display()),
        ),
        EnumerationError::Other(msg) => (
            "Couldn't read this folder",
            format!("{msg}\n\nPath: {}", path.display()),
        ),
    };

    let head_style = TextStyle {
        size: tokens.text.lg,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    let body_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.secondary,
    };
    // Wrap body text by hand on each '\n'; further visual wrapping is
    // unnecessary at typical pane widths.
    let body_lines: Vec<&str> = body.split('\n').collect();
    let line_h = tokens.text.sm + 6.0;
    let block_h = tokens.text.lg + 12.0 + body_lines.len() as f32 * line_h;
    let center_y = rect.top() + (rect.size.height - block_h).max(0.0) / 2.0;

    let head_w = painter.measure_text(heading, head_style).width;
    let head_x = rect.left() + (rect.size.width - head_w).max(0.0) / 2.0;
    painter.draw_text(FPoint::new(head_x, center_y), heading, head_style);

    let mut y = center_y + tokens.text.lg + 12.0;
    for line in &body_lines {
        let w = painter.measure_text(line, body_style).width;
        let x = rect.left() + (rect.size.width - w).max(0.0) / 2.0;
        painter.draw_text(FPoint::new(x, y), line, body_style);
        y += line_h;
    }
}

/// Initial theme preference at app startup. Resolution order:
/// `FERAILLE_THEME` env var (regression tooling, screenshots) →
/// persisted `app_prefs::AppPrefs.theme_preference` → `System`.
fn initial_theme_preference(prefs: &app_prefs::AppPrefs) -> ThemePreference {
    if let Some(env) = std::env::var("FERAILLE_THEME").ok() {
        match env.as_str() {
            "dark" => return ThemePreference::Dark,
            "light" => return ThemePreference::Light,
            _ => {}
        }
    }
    match prefs.theme_preference {
        Some(app_prefs::ThemePref::Light) => ThemePreference::Light,
        Some(app_prefs::ThemePref::Dark) => ThemePreference::Dark,
        Some(app_prefs::ThemePref::System) | None => ThemePreference::System,
    }
}

/// Initial `ui_scale`. `FERAILLE_UI_SCALE` env var lets screenshot
/// fixtures and CI render at non-default scales without launching the
/// GUI. Session-only today; when settings persistence ships this is the
/// hook for loading the saved value before falling through to the env
/// var, then to 1.0.
fn initial_ui_scale() -> f32 {
    if let Ok(s) = std::env::var("FERAILLE_UI_SCALE") {
        if let Ok(v) = s.trim().parse::<f32>() {
            return v.clamp(feraille_design::UI_SCALE_MIN, feraille_design::UI_SCALE_MAX);
        }
    }
    1.0
}

fn format_status(tab: &Tab, tasks: &TaskRegistry) -> String {
    let count = tab.entries.len();
    let filter_suffix = if tab.filter_text.trim().is_empty() {
        String::new()
    } else {
        format!("    filter: {}", tab.filter_text.trim())
    };
    let task_suffix = match tasks.len() {
        0 => String::new(),
        1 => match tasks.primary() {
            Some(t) => format!("    \u{00B7} {}", t.label),
            None => String::new(),
        },
        n => format!("    \u{00B7} {} tasks running", n),
    };
    match tab.selection.cursor() {
        Some(i) if i < count => format!(
            "{}    {} of {}{}{}    \u{2191}/\u{2193} navigate · Enter open · Backspace up · Esc quit",
            tab.entries[i].name,
            i + 1,
            count,
            filter_suffix,
            task_suffix
        ),
        _ => format!(
            "{} items{}{}    \u{2191}/\u{2193} navigate · Enter open · Backspace up · Esc quit",
            count, filter_suffix, task_suffix
        ),
    }
}

#[cfg(target_os = "macos")]
fn load_default_font() -> Result<Vec<u8>> {
    let path = "/System/Library/Fonts/Supplemental/Arial.ttf";
    std::fs::read(path).with_context(|| format!("read {path}"))
}

#[cfg(target_os = "windows")]
fn load_default_font() -> Result<Vec<u8>> {
    let path = "C:\\Windows\\Fonts\\segoeui.ttf";
    std::fs::read(path).with_context(|| format!("read {path}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn load_default_font() -> Result<Vec<u8>> {
    anyhow::bail!("no default font path on this OS")
}

/// Recognized text-file extensions for the inline-source preview
/// path. Limited to **unstyled source / log / config** — formats
/// where seeing raw bytes is the right answer. Markdown / HTML /
/// rich text fall through to qlmanage so the user gets the rendered
/// Quick Look preview Finder shows for those types.
fn is_text_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "txt"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "css"
            | "log"
            | "sh"
            | "zsh"
            | "bash"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "go"
            | "rb"
            | "swift"
            | "java"
            | "kt"
            | "ini"
            | "conf"
            | "cfg"
            | "csv"
            | "tsv"
            | "sql"
            | "xml"
    )
}

/// Read up to ~32 KB of `path` as UTF-8 (lossy on errors). Returns
/// `None` only if the open fails. Truncated to a fixed byte budget
/// so a 5 GB log file doesn't OOM the cache.
fn read_text_preview(path: &Path) -> Option<String> {
    use std::io::Read;
    const MAX_BYTES: usize = 32 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = f.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        let take = n.min(MAX_BYTES.saturating_sub(buf.len()));
        buf.extend_from_slice(&chunk[..take]);
        if buf.len() >= MAX_BYTES {
            break;
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// If the command line contains `--reset-db <scope>` (or just
/// `--reset-db` with no value — which we treat as a usage error),
/// open the metadata DB, wipe the requested scope, and return
/// `Some(exit_code)`. Returns `None` when the flag isn't present
/// so `main()` falls through to its normal startup path.
fn handle_reset_db_cli() -> Option<i32> {
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        if arg != "--reset-db" {
            continue;
        }
        let Some(raw) = iter.next() else {
            // Bare `--reset-db` is the "what can I reset?" query —
            // not an error. Print the scopes and exit 0 so users
            // (and `--help`-style discovery) get a clean listing.
            print_reset_db_usage();
            return Some(0);
        };
        // `--reset-db help` / `list` / `-h` also list scopes.
        if matches!(raw.as_str(), "help" | "--help" | "-h" | "list") {
            print_reset_db_usage();
            return Some(0);
        }
        let Some(scope) = feraille_meta::ResetScope::from_cli(&raw) else {
            eprintln!("--reset-db: unknown scope `{raw}`");
            eprintln!();
            print_reset_db_usage();
            return Some(2);
        };
        // Locate + open the DB. Mirrors `App::open_metadata_db` but
        // pre-event-loop so we don't spin up a window.
        let Some(path) = feraille_meta::default_db_path() else {
            eprintln!("--reset-db: $HOME unset; nothing to reset");
            return Some(1);
        };
        if !path.exists() {
            eprintln!("--reset-db: no DB at {} (nothing to do)", path.display());
            return Some(0);
        }
        if let Err(e) = feraille_meta::ensure_parent_dir(&path) {
            eprintln!("--reset-db: mkdir failed for {}: {e}", path.display());
            return Some(1);
        }
        // For `all` it's both faster and cleaner to delete the file
        // outright than to DELETE every table — saves a vacuum step
        // and dodges any stale FK / index state. Other scopes go
        // through the `reset()` API which keeps `db_version` intact.
        if matches!(scope, feraille_meta::ResetScope::All) {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("--reset-db: rm {}: {e}", path.display());
                return Some(1);
            }
            eprintln!("--reset-db: deleted {}", path.display());
            return Some(0);
        }
        let db = match feraille_meta::MetadataDb::open(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("--reset-db: open {}: {e}", path.display());
                return Some(1);
            }
        };
        match db.reset(scope) {
            Ok(()) => {
                eprintln!(
                    "--reset-db {:?}: cleared {} at {}",
                    scope,
                    scope.help_label(),
                    path.display()
                );
                return Some(0);
            }
            Err(e) => {
                eprintln!("--reset-db {:?} failed: {e}", scope);
                return Some(1);
            }
        }
    }
    None
}

fn print_reset_db_usage() {
    use feraille_meta::ResetScope;
    eprintln!("Reset parts of the metadata DB at:");
    if let Some(p) = feraille_meta::default_db_path() {
        eprintln!("  {}", p.display());
    } else {
        eprintln!("  (no DB — $HOME unset)");
    }
    eprintln!();
    eprintln!("Usage:  Feraille --reset-db <scope>");
    eprintln!();
    eprintln!("Available scopes:");
    for scope in [
        ResetScope::All,
        ResetScope::Ui,
        ResetScope::Caches,
        ResetScope::AntTrail,
        ResetScope::Magic,
        ResetScope::Quarantine,
        ResetScope::Favorites,
    ] {
        let name = match scope {
            ResetScope::All => "all",
            ResetScope::Ui => "ui",
            ResetScope::Caches => "caches",
            ResetScope::AntTrail => "ant-trail",
            ResetScope::Magic => "magic",
            ResetScope::Quarantine => "quarantine",
            ResetScope::Favorites => "favorites",
        };
        eprintln!("  {:<12} {}", name, scope.help_label());
    }
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  Feraille --reset-db ui          # forget window size + open tabs");
    eprintln!("  Feraille --reset-db caches      # re-sniff magic + re-walk Ant Trail");
    eprintln!("  Feraille --reset-db all         # nuke the DB file outright");
}

/// Resolve volume info for `path`, walking up to the volume root if
/// `path` itself is a folder rather than a mount point. Maps the
/// platform `VolumeInfo` to our local snapshot so the DU module
/// doesn't need to depend on `feraille-fs-native`'s shape.
fn lookup_volume_for_path(path: &Path) -> Option<crate::disk_usage_state::VolumeSnapshot> {
    let mut current = path.to_path_buf();
    loop {
        if let Some(info) = feraille_fs_native::volume_info_for_path(&current) {
            return Some(crate::disk_usage_state::VolumeSnapshot {
                name: info.name,
                total_bytes: info.total_bytes,
                available_bytes: info.available_bytes,
                is_local: info.is_local,
                is_removable: info.is_removable,
            });
        }
        if !current.pop() {
            return None;
        }
    }
}
