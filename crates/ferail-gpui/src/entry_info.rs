//! Get Info popup: the cross-crate composition that builds a
//! [`ferail_core::entry_info::EntryInfo`] for one path, plus the modal
//! view that renders it.
//!
//! Composition lives here (not in a domain crate) because it is the one
//! place that legitimately touches every layer at once: POSIX stat
//! (`ferail-fs-native`), AppKit resource values (`ferail-shell-mac`),
//! volume info, magic, and tags. The gather runs on the background executor
//! — never the paint path — and the result is a fully-formatted, neutral
//! record the view paints without any further I/O.
//!
//! The popup is a gpui-component `Dialog` hosting this view, so ESC /
//! overlay-click / focus-trap come for free (same primitive as the About
//! box). Editing is layered on top in a later pass; today the panel reads.

use crate::text::{TextScale as _, TruncateMiddle as _, elide_label};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ferail_core::commands::TagColor;
use ferail_core::entry_info::{
    Attr, EntryInfo, InfoSection, InfoTarget, InfoValue, PermBits, PermMatrix, SizeValue,
    TimestampKind,
};
use ferail_core::name_hazards::{self, HazardKind};
#[cfg(windows)]
use ferail_core::platform_properties::{
    PlatformProperties, PlatformPropertiesProvider as _, PlatformPropertyValue,
};
#[cfg(windows)]
use ferail_core::platform_shortcuts::{ShortcutInfo, ShortcutResolver as _, ShortcutTarget};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, Sizable, WindowExt as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputState},
    notification::Notification,
    tooltip::Tooltip,
    v_flex,
};

use crate::file_list::tag_color_rgba;
use crate::shell::Shell;

/// Key-binding context for the standalone Get Info window — Esc dismisses it
/// (bound in `keymap::install_extras`). The embedded-in-preview instance
/// doesn't set this context, so Esc there belongs to the shell.
pub const ENTRY_INFO_CONTEXT: &str = "GetInfo";

#[derive(Clone, Copy)]
pub struct EntryInfoIdentity {
    pub node: ferail_core::NodeId,
    pub revision: ferail_core::revision_cache::FileRevision,
}

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
    open_impl(path, name, target, known_size, None, shell, cx);
}

pub fn open_identified(
    path: PathBuf,
    name: String,
    target: InfoTarget,
    known_size: Option<u64>,
    identity: EntryInfoIdentity,
    shell: WeakEntity<Shell>,
    cx: &mut App,
) {
    open_impl(path, name, target, known_size, Some(identity), shell, cx);
}

fn open_impl(
    path: PathBuf,
    name: String,
    target: InfoTarget,
    known_size: Option<u64>,
    identity: Option<EntryInfoIdentity>,
    shell: WeakEntity<Shell>,
    cx: &mut App,
) {
    let title: SharedString = tr!(
        "Get Info \u{2014} {name}",
        name = crate::private_mode::present_leaf_str(&name, false)
    );
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
            title: Some(title.clone()),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let handle = cx.open_window(opts, move |window, cx| {
        crate::boot::install_dev_window_callback_cleanup(window, cx);
        let view = cx.new(|cx| {
            EntryInfoView::new(
                path,
                name,
                target,
                known_size,
                identity,
                shell,
                Some(cascade),
                cx,
            )
        });
        cx.new(|cx| Root::new(view, window, cx))
    });
    if let Ok(handle) = handle {
        crate::process_state::process_state(cx)
            .register_aux_window(handle.into(), title.to_string());
        crate::boot::refresh_window_menu(cx);
    }
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
        .map(|n| ferail_fs_native::paths::display_leaf(n).into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Build the full Get Info record. Runs on the background executor: every
/// call here is a native read, none of it is allowed on the paint path.
/// `known_size` is the caller's already-computed recursive size for a
/// folder/volume (from the file list's Size column) — reused so we don't
/// rescan, shown with a refresh affordance.
pub fn gather(path: &Path, known_size: Option<u64>) -> EntryInfo {
    use ferail_fs_native as fsn;
    // Routes through the per-OS shell alias so Get Info builds on every
    // platform; on macOS this *is* `ferail_shell_mac`, so behaviour is
    // unchanged. read_shell_info / read_canonical_tags / open_with_candidates
    // are real on macOS and graceful no-ops on win32/linux.
    use crate::platform_shell as shell;

    let target = classify(path);
    let stat = fsn::stat_info::read_stat_info(path);
    let sh = shell::read_shell_info(path);
    let vol = fsn::volume_info_for_path(path);
    let colors = if shell::SUPPORTS_TAGS {
        shell::read_canonical_tags(path)
    } else {
        Vec::new()
    };
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
            InfoTarget::Volume => tr!("Volume").to_string(),
            InfoTarget::Folder => tr!("Folder").to_string(),
            InfoTarget::File => tr!("Document").to_string(),
        });

    let fmt_date = fsn::stat_info::format_local_datetime;

    // ---- General ----
    let mut general =
        InfoSection::new(tr!("General").to_string()).text_if(tr!("Kind").to_string(), kind.clone());
    if let Some(uti) = sh.uti.clone() {
        general = general.text_if(tr!("Type").to_string(), uti);
    }
    match target {
        InfoTarget::File => {
            let bytes = stat.as_ref().map(|s| s.size).unwrap_or(0);
            general = general.row(
                tr!("Size").to_string(),
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
            general = general.row(tr!("Size").to_string(), InfoValue::Size(size));
        }
        InfoTarget::Volume => {}
    }
    if let Some(s) = &stat {
        if let Some(c) = s.created_unix {
            general = general.row(
                tr!("Created").to_string(),
                InfoValue::Timestamp {
                    unix: c,
                    display: fmt_date(c),
                    kind: TimestampKind::Created,
                    editable: cfg!(windows) && !s.is_symlink,
                },
            );
        }
        general = general.row(
            tr!("Modified").to_string(),
            InfoValue::Timestamp {
                unix: s.modified_unix,
                display: fmt_date(s.modified_unix),
                kind: TimestampKind::Modified,
                editable: cfg!(any(unix, windows)) && !cfg!(target_os = "aros") && !s.is_symlink,
            },
        );
        if let Some(a) = s.accessed_unix {
            general = general.row(
                tr!("Last opened").to_string(),
                InfoValue::Timestamp {
                    unix: a,
                    display: fmt_date(a),
                    kind: TimestampKind::Accessed,
                    editable: cfg!(any(unix, windows))
                        && !cfg!(target_os = "aros")
                        && !s.is_symlink,
                },
            );
        }
    }
    if let Some(added) = sh.added_unix {
        general = general.row(
            tr!("Added").to_string(),
            InfoValue::Timestamp {
                unix: added,
                display: fmt_date(added),
                // Non-editable; the kind is never dispatched to a writer.
                kind: TimestampKind::Modified,
                editable: false,
            },
        );
    }
    if let Some(app) = default_app {
        general = general.private_text_if(tr!("Application").to_string(), app);
    }
    general = general.path_if(
        tr!("Where").to_string(),
        ferail_fs_native::paths::display_path(path),
    );

    // ---- Media (audio tags + properties) ----
    // Files only, and only when lofty recognizes the container as audio — the
    // reader returns `None` for everything else, so this section simply doesn't
    // appear for non-media files (the `filter(!rows.is_empty())` below drops an
    // empty section). Reading tags is native I/O, which is fine here: `gather`
    // already runs on the background executor, never the paint path.
    let mut media = InfoSection::new(tr!("Media").to_string());
    if target == InfoTarget::File {
        if let Some(t) = fsn::media::read_media_tags(path) {
            media = media
                .private_text_if(tr!("Title").to_string(), t.title.clone())
                .private_text_if(tr!("Artist").to_string(), t.artist.clone())
                .private_text_if(tr!("Album").to_string(), t.album.clone())
                .private_text_if(tr!("Genre").to_string(), t.genre.clone())
                .private_text_if(
                    tr!("Year").to_string(),
                    t.year.map(|y| y.to_string()).unwrap_or_default(),
                )
                .text_if(tr!("Track").to_string(), t.track_label())
                .text_if(tr!("Disc").to_string(), t.disc_label())
                .text_if(tr!("Duration").to_string(), t.duration_label())
                .text_if(tr!("Format").to_string(), t.codec.clone())
                .text_if(tr!("Channels").to_string(), t.channels_label())
                .text_if(tr!("Sample rate").to_string(), t.sample_rate_label())
                .text_if(tr!("Bit depth").to_string(), t.bit_depth_label())
                .text_if(tr!("Bit rate").to_string(), t.bitrate_label());
        }
    }

    // ---- Image (header dimensions + curated EXIF) ----
    // Same contract as Media above: the reader returns `None` for anything
    // that isn't a readable image, so the section silently doesn't appear.
    // GPS is presence-only by design (WIN-014 privacy treatment) — the
    // coordinates are never parsed, shown, logged, or persisted.
    let mut image = InfoSection::new(tr!("Image").to_string());
    if target == InfoTarget::File {
        if let Some(m) = fsn::image_meta::read_image_meta(path) {
            image = image
                .text_if(tr!("Dimensions").to_string(), m.dimensions_label())
                .private_text_if(tr!("Camera").to_string(), m.camera_label())
                .private_text_if(tr!("Lens").to_string(), m.lens_model.clone())
                .private_text_if(tr!("Date taken").to_string(), m.taken.clone())
                .text_if(tr!("Exposure").to_string(), m.exposure_label());
            // "Normal" is the unremarkable default — only a stored rotation
            // is worth a row (the volume section's read-only precedent).
            if let Some(code) = m.orientation.filter(|&c| c != 1) {
                image = image.text_if(
                    tr!("Orientation").to_string(),
                    orientation_label(code).to_string(),
                );
            }
            if m.gps_present {
                image = image.text_if(
                    tr!("Location").to_string(),
                    tr!("Embedded in photo").to_string(),
                );
            }
        }
    }

    // ---- Attributes ----
    let mut attributes = InfoSection::new(tr!("Attributes").to_string());
    if let Some(s) = &stat {
        attributes = attributes
            .row(
                crate::i18n::tr_static(Attr::Locked.label()).to_string(),
                InfoValue::Toggle {
                    on: s.is_locked,
                    attr: Attr::Locked,
                },
            )
            .row(
                crate::i18n::tr_static(Attr::Invisible.label()).to_string(),
                InfoValue::Toggle {
                    on: s.is_invisible,
                    attr: Attr::Invisible,
                },
            );
    }
    if let Some(he) = sh.hidden_extension {
        attributes = attributes.row(
            crate::i18n::tr_static(Attr::HiddenExtension.label()).to_string(),
            InfoValue::Toggle {
                on: he,
                attr: Attr::HiddenExtension,
            },
        );
    }
    if shell::SUPPORTS_TAGS {
        attributes = attributes.row(
            tr!("Tags").to_string(),
            InfoValue::Tags {
                colors,
                custom: Vec::new(),
            },
        );
    }

    // ---- Ownership & Permissions ----
    let mut permissions = InfoSection::new(tr!("Ownership & Permissions").to_string());
    if let Some(s) = &stat {
        let mode = s.mode & 0o7777;
        permissions = permissions
            .private_text_if(tr!("Owner").to_string(), s.owner_name.clone())
            .private_text_if(tr!("Group").to_string(), s.group_name.clone())
            .row(
                tr!("Permissions").to_string(),
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
    let mut volume = InfoSection::new(tr!("Volume").to_string());
    if let Some(v) = &vol {
        volume = volume.private_text_if(tr!("Volume").to_string(), v.name.clone());
        if let Some(t) = v.total_bytes {
            volume = volume.row(tr!("Capacity").to_string(), InfoValue::Bytes(t));
        }
        if let Some(a) = v.available_bytes {
            volume = volume.row(tr!("Available").to_string(), InfoValue::Bytes(a));
        }
        // Finder parity: Capacity / Available / Used. Used is derived —
        // statfs reports total and free, not a used counter.
        if let (Some(t), Some(a)) = (v.total_bytes, v.available_bytes) {
            volume = volume.row(
                tr!("Used").to_string(),
                InfoValue::Bytes(t.saturating_sub(a)),
            );
        }
        if let Some(f) = v.format.clone() {
            volume = volume.text_if(tr!("Format").to_string(), f);
        }
        // Only worth a row when it constrains the user; a writable
        // volume is the unremarkable default.
        if v.read_only {
            volume = volume.text_if(tr!("Access").to_string(), tr!("Read-only").to_string());
        }
        volume = volume.path_if(tr!("Mount point").to_string(), v.path.display().to_string());
        if let Some(d) = v.bsd_device.clone() {
            volume = volume.private_text_if(tr!("Device").to_string(), d);
        }
    }

    let sections = [general, media, image, attributes, permissions, volume]
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

/// Human wording for a stored EXIF orientation (codes 2–8; 1 = normal is
/// filtered out by the caller). The label describes the correction a viewer
/// applies to display the photo upright — the convention cameras write.
fn orientation_label(code: u16) -> SharedString {
    match code {
        2 => tr!("Flipped horizontally"),
        3 => tr!("Rotated 180\u{00B0}"),
        4 => tr!("Flipped vertically"),
        5 => tr!("Rotated 90\u{00B0} counter-clockwise, flipped"),
        6 => tr!("Rotated 90\u{00B0} clockwise"),
        7 => tr!("Rotated 90\u{00B0} clockwise, flipped"),
        8 => tr!("Rotated 90\u{00B0} counter-clockwise"),
        _ => SharedString::default(),
    }
}

#[cfg(windows)]
fn append_windows_details(
    info: &mut EntryInfo,
    properties: Option<&PlatformProperties>,
    shortcut: Option<&ShortcutInfo>,
) {
    if let Some(properties) = properties {
        for section in &properties.sections {
            let mut output = InfoSection::new(section.title.to_string());
            for property in &section.properties {
                let value = match &property.value {
                    PlatformPropertyValue::Text(value) => value.to_string(),
                    PlatformPropertyValue::TextList(values) => values
                        .iter()
                        .map(AsRef::<str>::as_ref)
                        .collect::<Vec<_>>()
                        .join(", "),
                    PlatformPropertyValue::Boolean(value) => value.to_string(),
                    PlatformPropertyValue::Signed(value) => value.to_string(),
                    PlatformPropertyValue::Unsigned(value) => value.to_string(),
                    PlatformPropertyValue::TimestampUnixMillis(value) => value.to_string(),
                };
                output = output.private_text_if(property.display_name.to_string(), value);
            }
            if !output.rows.is_empty() {
                info.sections.push(output);
            }
        }
    }
    if let Some(shortcut) = shortcut {
        let mut section = InfoSection::new(tr!("Shortcut").to_string());
        match &shortcut.target {
            Ok(ShortcutTarget::FileSystem { path, .. }) => {
                section = section.path_if(
                    tr!("Target").to_string(),
                    ferail_fs_native::paths::display_path(path),
                );
            }
            Ok(ShortcutTarget::Url(url)) => {
                section = section.private_text_if(tr!("Target").to_string(), url.to_string());
            }
            Ok(ShortcutTarget::Platform(_)) => {
                section = section.text_if(
                    tr!("Target").to_string(),
                    tr!("Windows Shell item").to_string(),
                );
            }
            Err(error) => {
                section = section.private_text_if(tr!("Status").to_string(), format!("{error:?}"));
            }
        }
        if !shortcut.arguments.is_empty() {
            section = section.private_text_if(
                tr!("Arguments").to_string(),
                shortcut
                    .arguments
                    .iter()
                    .map(|argument| argument.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        if let Some(directory) = &shortcut.working_directory {
            section = section.path_if(
                tr!("Start in").to_string(),
                ferail_fs_native::paths::display_path(directory),
            );
        }
        if !section.rows.is_empty() {
            info.sections.push(section);
        }
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
    /// Cancels native property/shortcut reads when the embedded view retargets
    /// or a standalone window closes.
    details_cancel: Option<Arc<AtomicBool>>,
    identity: Option<EntryInfoIdentity>,
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
    #[allow(clippy::too_many_arguments)]
    fn new(
        path: PathBuf,
        name: String,
        target: InfoTarget,
        known_size: Option<u64>,
        identity: Option<EntryInfoIdentity>,
        shell: WeakEntity<Shell>,
        cascade: Option<CascadeGuard>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(
            path, name, target, known_size, identity, shell, false, cascade, cx,
        )
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
        Self::build(path, name, target, known_size, None, shell, true, None, cx)
    }

    // The info-row builder genuinely needs each of these inputs.
    #[allow(clippy::too_many_arguments)]
    fn build(
        path: PathBuf,
        name: String,
        _target: InfoTarget,
        known_size: Option<u64>,
        identity: Option<EntryInfoIdentity>,
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
            details_cancel: None,
            identity,
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
                            display: ferail_fs_native::humanize_bytes(b),
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
        if let Some(c) = self.details_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.path = path;
        self.name = name;
        self.kind = String::new();
        self.known_size = known_size;
        self.identity = None;
        self.state = GatherState::Loading;
        self.refresh(cx);
    }

    /// (Re-)gather the record on the background executor and apply it.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.details_cancel.take() {
            previous.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.details_cancel = Some(cancel.clone());
        #[cfg(windows)]
        let worker_cancel = cancel.clone();
        let gather_path = self.path.clone();
        let apply_path = gather_path.clone();
        let known_size = self.known_size;
        #[cfg(windows)]
        let identity = self.identity;
        #[cfg(windows)]
        let (
            properties_provider,
            cached_properties,
            shortcut_resolver,
            cached_shortcut,
            provider_gate,
        ) = {
            let process = crate::process_state::process_state(cx);
            (
                process.properties_provider.clone(),
                identity.and_then(|identity| {
                    process
                        .properties_cache
                        .borrow_mut()
                        .get(identity.node, identity.revision)
                }),
                process.shortcut_resolver.clone(),
                identity.and_then(|identity| {
                    process
                        .shortcut_cache
                        .borrow_mut()
                        .get(identity.node, identity.revision)
                }),
                process.info_provider_gate.clone(),
            )
        };
        cx.spawn(async move |this, cx| {
            #[cfg(not(windows))]
            let info = cx
                .background_executor()
                .spawn(async move { gather(&gather_path, known_size) })
                .await;
            #[cfg(windows)]
            let (info, new_properties, new_shortcut) = cx
                .background_executor()
                .spawn(async move {
                    let mut info = gather(&gather_path, known_size);
                    let Some(_provider_permit) = provider_gate.acquire(&worker_cancel) else {
                        return (info, None, None);
                    };
                    let properties = if let Some(cached) = cached_properties {
                        Some((*cached).clone())
                    } else {
                        properties_provider
                            .read_properties(
                                ferail_core::platform_properties::PlatformPropertiesRequest {
                                    target:
                                        ferail_core::platform_namespace::LocationTarget::FileSystem(
                                            gather_path.clone(),
                                        ),
                                },
                                &worker_cancel,
                            )
                            .ok()
                    };
                    let is_shortcut = gather_path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"));
                    let shortcut = if !is_shortcut {
                        None
                    } else if let Some(cached) = cached_shortcut {
                        Some((*cached).clone())
                    } else if let Some(identity) = identity {
                        match shortcut_resolver.resolve(
                            ferail_core::platform_shortcuts::ShortcutResolveRequest {
                                source: gather_path.clone(),
                                revision: identity.revision,
                            },
                            &worker_cancel,
                        ) {
                            Ok(shortcut) => Some(shortcut),
                            Err(
                                ferail_core::platform_shortcuts::ShortcutFailureKind::Cancelled,
                            ) => None,
                            Err(error) => Some(ShortcutInfo {
                                target: Err(error),
                                arguments: Vec::new(),
                                working_directory: None,
                                icon_location: None,
                            }),
                        }
                    } else {
                        None
                    };
                    append_windows_details(&mut info, properties.as_ref(), shortcut.as_ref());
                    (info, properties, shortcut)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Staleness guard: the panel may have been retargeted
                // (preview-pane embedded mode) while a slow gather —
                // e.g. a network mount — was in flight. Applying it
                // would show file A's size/permissions under file B.
                if this.path != apply_path || cancel.load(Ordering::Relaxed) {
                    return;
                }
                #[cfg(windows)]
                if let Some(identity) = identity {
                    let process = crate::process_state::process_state(cx);
                    if let Some(properties) = new_properties {
                        process.properties_cache.borrow_mut().insert(
                            identity.node,
                            identity.revision,
                            properties,
                        );
                    }
                    if let Some(shortcut) = new_shortcut {
                        process.shortcut_cache.borrow_mut().insert(
                            identity.node,
                            identity.revision,
                            shortcut,
                        );
                    }
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
                #[cfg(windows)]
                if let Some(identity) = self.identity {
                    crate::process_state::process_state(cx)
                        .properties_cache
                        .borrow_mut()
                        .remove(identity.node);
                }
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
                        display: ferail_fs_native::humanize_bytes(bytes),
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
        use ferail_fs_native::stat_info;
        let path = self.path.clone();
        self.spawn_write(
            move || match attr {
                Attr::Locked => stat_info::set_locked(&path, on),
                Attr::Invisible => stat_info::set_invisible(&path, on),
                Attr::HiddenExtension => crate::platform_shell::set_hidden_extension(&path, on),
                Attr::Stationery => Err(tr!("Stationery editing is not supported yet").to_string()),
            },
            window,
            cx,
        );
    }

    /// Open a focused editor for one local filesystem date. The input uses a
    /// stable, locale-independent shape while the read-only row remains
    /// friendly/localized. Validation happens before the dialog closes; the
    /// actual filesystem write still goes through the background worker.
    fn edit_timestamp(
        &mut self,
        kind: TimestampKind,
        unix: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = ferail_fs_native::stat_info::format_editable_local_datetime(unix);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial)
                .placeholder("YYYY-MM-DD HH:MM:SS")
        });
        let input_for_dialog = input.clone();
        let view = cx.entity();
        let field = crate::i18n::tr_static(kind.label()).to_string();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input_for_dialog.clone();
            let view = view.clone();
            dialog
                .title(tr!("Edit {field}", field = field.clone()))
                .child(Input::new(&input).small())
                .on_ok(move |_, window, cx: &mut App| {
                    let raw = input.read(cx).value().trim().to_string();
                    let timestamp = match ferail_fs_native::stat_info::parse_local_datetime(&raw) {
                        Ok(timestamp) => timestamp,
                        Err(_) => {
                            window.push_notification(
                                Notification::error(tr!(
                                    "Enter a valid local date and time as YYYY-MM-DD HH:MM:SS."
                                )),
                                cx,
                            );
                            return false;
                        }
                    };
                    view.update(cx, |this, cx| {
                        this.apply_timestamp(kind, timestamp, window, cx);
                    });
                    true
                })
        });
        window.on_next_frame(move |window, cx| {
            input.read(cx).focus_handle(cx).focus(window, cx);
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        });
    }

    fn apply_timestamp(
        &mut self,
        kind: TimestampKind,
        unix: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.path.clone();
        self.spawn_write(
            move || ferail_fs_native::stat_info::set_timestamp(&path, kind, unix),
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
            move || ferail_fs_native::stat_info::set_permissions(&path, mode),
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
        if let Some(c) = &self.details_cancel {
            c.store(true, Ordering::Relaxed);
        }
    }
}

impl Render for EntryInfoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.embedded {
            window.set_window_title(&tr!(
                "Get Info — {name}",
                name = crate::private_mode::present_leaf_str(&self.name, false)
            ));
        }
        let muted = cx.theme().muted_foreground;

        let sections = match &self.state {
            GatherState::Loading => v_flex().child(
                div()
                    .text_scale_sm()
                    .text_color(muted)
                    .child(tr!("Gathering details\u{2026}")),
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
        let shown_name = crate::private_mode::present_leaf_str(&self.name, false);
        let mut header = v_flex().gap_0p5().child(
            div()
                .text_scale_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(if crate::private_mode::enabled() {
                    SharedString::from(shown_name.clone()).into_any_element()
                } else {
                    name_hazard_element(&shown_name, "popup-name")
                }),
        );
        if !crate::private_mode::enabled()
            && let Some(warn) = name_hazard_warning(&self.name)
        {
            header = header.child(
                div()
                    .text_scale_xs()
                    .text_color(gpui::rgb(0xC2410C))
                    .child(tr!("\u{26A0} {warning}", warning = warn)),
            );
        }
        let header = header.child(
            div()
                .text_scale_xs()
                .text_color(muted)
                .child(self.kind.clone()),
        );

        let content = v_flex()
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
            .when(!crate::private_mode::enabled(), |this| {
                this.children(Root::render_notification_layer(window, cx))
            })
            .into_any_element();
        crate::private_mode::protect(content, cx)
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
            .w_full()
            .min_w_0()
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
                    .min_w_0()
                    .text_scale_xs()
                    .child(self.render_value(value, ix, cx)),
            )
    }

    fn render_value(&self, value: &InfoValue, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match value {
            InfoValue::Text(s) => div().child(s.clone()).into_any_element(),
            InfoValue::PrivateText(s) => div()
                .child(crate::private_mode::present_label(s))
                .into_any_element(),
            InfoValue::Bytes(bytes) => div()
                .child(ferail_fs_native::humanize_bytes(
                    crate::private_mode::present_bytes(ix as u64, *bytes),
                ))
                .into_any_element(),
            InfoValue::Name(s) => div()
                .child(crate::private_mode::present_leaf_str(s, false))
                .into_any_element(),
            InfoValue::Path(path) => {
                let shown = if crate::private_mode::enabled() {
                    crate::private_mode::present_path(Path::new(path))
                } else {
                    path.clone()
                };
                let full: SharedString = shown.clone().into();
                div()
                    .id(ElementId::Name(format!("entry-info-path-{ix}").into()))
                    .w_full()
                    .min_w_0()
                    .truncate_middle()
                    .child(elide_label(&shown, 72))
                    .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                    .into_any_element()
            }
            InfoValue::Timestamp {
                unix,
                display,
                kind,
                editable,
            } => {
                let shown = if crate::private_mode::enabled() {
                    ferail_fs_native::stat_info::format_local_datetime(
                        crate::private_mode::present_timestamp(
                            ix as u64,
                            *unix,
                            ferail_core::now_unix(),
                        ),
                    )
                } else {
                    display.clone()
                };
                let mut row = h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().child(shown));
                if *editable {
                    let (unix, kind) = (*unix, *kind);
                    row = row.child(
                        Button::new(ElementId::Name(format!("entry-info-date-{ix}").into()))
                            .label(tr!("Edit"))
                            .xsmall()
                            .tooltip(tr!("Edit date and time"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_timestamp(kind, unix, window, cx);
                            })),
                    );
                }
                row.into_any_element()
            }
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
                    row = row.child(div().child(crate::private_mode::present_label(name)));
                }
                row.into_any_element()
            }
            InfoValue::Permissions(m) => self.render_permissions(m, cx),
            InfoValue::Size(size) => match size {
                SizeValue::Known {
                    bytes,
                    display,
                    refreshable,
                } => {
                    let shown = if crate::private_mode::enabled() {
                        ferail_fs_native::humanize_bytes(crate::private_mode::present_bytes(
                            ix as u64, *bytes,
                        ))
                    } else {
                        display.clone()
                    };
                    let row = h_flex().gap_1().items_center().child(div().child(shown));
                    if *refreshable {
                        // A cached folder/volume total — let the user recompute.
                        row.child(
                            Button::new("entry-info-recalc-size")
                                .label("\u{21BB}")
                                .xsmall()
                                .tooltip(tr!("Recalculate size"))
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
                    .child(tr!("Calculating\u{2026}"))
                    .into_any_element(),
                SizeValue::Calculable => Button::new("entry-info-calc-size")
                    .label(tr!("Calculate"))
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
            let label = if m.owner.write {
                tr!("Read & write")
            } else {
                tr!("Read-only")
            };
            div()
                .text_scale_sm()
                .text_color(cx.theme().foreground)
                .child(label)
                .into_any_element()
        }

        #[cfg(not(target_os = "windows"))]
        {
            let classes: [(SharedString, PermBits); 3] = [
                (tr!("Owner"), m.owner),
                (tr!("Group"), m.group),
                (tr!("Other"), m.other),
            ];
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct HazardRenderPiece {
    text: String,
    hazard: Option<HazardKind>,
    label: Option<String>,
}

impl HazardRenderPiece {
    fn display_len(&self) -> usize {
        self.text.chars().count()
    }
}

fn hazard_render_pieces(name: &str) -> Vec<HazardRenderPiece> {
    let mut pieces = Vec::new();
    for segment in name_hazards::analyze(name) {
        if let Some(kind) = segment.hazard {
            pieces.push(HazardRenderPiece {
                text: segment.render.unwrap_or(segment.text),
                hazard: Some(kind),
                label: segment.label,
            });
        } else {
            // Plain characters may be cut individually. Hazard stand-ins stay
            // atomic so an elision never shows half of "<U+200B>".
            pieces.extend(segment.text.chars().map(|ch| HazardRenderPiece {
                text: ch.to_string(),
                hazard: None,
                label: None,
            }));
        }
    }
    pieces
}

fn prefix_end(pieces: &[HazardRenderPiece], budget: usize) -> usize {
    let mut used = 0;
    pieces
        .iter()
        .take_while(|piece| {
            let next = used + piece.display_len();
            if next <= budget {
                used = next;
                true
            } else {
                false
            }
        })
        .count()
}

fn suffix_start(pieces: &[HazardRenderPiece], budget: usize) -> usize {
    let mut used = 0;
    let count = pieces
        .iter()
        .rev()
        .take_while(|piece| {
            let next = used + piece.display_len();
            if next <= budget {
                used = next;
                true
            } else {
                false
            }
        })
        .count();
    pieces.len() - count
}

fn centered_range(
    pieces: &[HazardRenderPiece],
    low: usize,
    high: usize,
    budget: usize,
) -> (usize, usize) {
    if low >= high || budget == 0 {
        return (low, low);
    }
    let total: usize = pieces[low..high]
        .iter()
        .map(HazardRenderPiece::display_len)
        .sum();
    let midpoint = total / 2;
    let mut crossed = 0;
    let centre = (low..high)
        .find(|&index| {
            crossed += pieces[index].display_len();
            crossed > midpoint
        })
        .unwrap_or(low);
    let centre_width = pieces[centre].display_len();
    if centre_width > budget {
        return (centre, centre);
    }
    let (mut start, mut end, mut used) = (centre, centre + 1, centre_width);
    loop {
        let mut grew = false;
        if start > low {
            let width = pieces[start - 1].display_len();
            if used + width <= budget {
                start -= 1;
                used += width;
                grew = true;
            }
        }
        if end < high {
            let width = pieces[end].display_len();
            if used + width <= budget {
                end += 1;
                used += width;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    (start, end)
}

fn hidden_ellipsis(hidden: &[HazardRenderPiece]) -> HazardRenderPiece {
    let hazards: Vec<&HazardRenderPiece> = hidden
        .iter()
        .filter(|piece| piece.hazard.is_some())
        .collect();
    let picked = hazards
        .iter()
        .copied()
        .find(|piece| !piece.hazard.is_some_and(is_amber_hazard))
        .or_else(|| hazards.first().copied());
    let Some(picked) = picked else {
        return HazardRenderPiece {
            text: "…".into(),
            hazard: None,
            label: None,
        };
    };
    let mut labels = Vec::new();
    for piece in hazards {
        let label = piece
            .label
            .clone()
            .unwrap_or_else(|| piece.hazard.unwrap().summary().to_string());
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    HazardRenderPiece {
        text: "…".into(),
        hazard: picked.hazard,
        label: Some(labels.join(" · ")),
    }
}

fn elide_hazard_pieces(pieces: &[HazardRenderPiece], max_chars: usize) -> Vec<HazardRenderPiece> {
    let total: usize = pieces.iter().map(HazardRenderPiece::display_len).sum();
    if total <= max_chars || max_chars < 4 {
        return pieces.to_vec();
    }
    if total <= max_chars.saturating_mul(2) {
        let content = max_chars - 1;
        let prefix = prefix_end(pieces, content / 2);
        let suffix = suffix_start(pieces, content - content / 2).max(prefix);
        let mut out = pieces[..prefix].to_vec();
        out.push(hidden_ellipsis(&pieces[prefix..suffix]));
        out.extend_from_slice(&pieces[suffix..]);
        return out;
    }

    let content = max_chars - 2;
    let prefix_budget = content / 3;
    let centre_budget = content / 3;
    let suffix_budget = content - prefix_budget - centre_budget;
    let prefix = prefix_end(pieces, prefix_budget);
    let suffix = suffix_start(pieces, suffix_budget).max(prefix);
    let (centre_start, centre_end) = centered_range(pieces, prefix, suffix, centre_budget);
    if centre_start == centre_end {
        let mut out = pieces[..prefix].to_vec();
        out.push(hidden_ellipsis(&pieces[prefix..suffix]));
        out.extend_from_slice(&pieces[suffix..]);
        return out;
    }
    let mut out = pieces[..prefix].to_vec();
    out.push(hidden_ellipsis(&pieces[prefix..centre_start]));
    out.extend_from_slice(&pieces[centre_start..centre_end]);
    out.push(hidden_ellipsis(&pieces[centre_end..suffix]));
    out.extend_from_slice(&pieces[suffix..]);
    out
}

fn is_amber_hazard(kind: HazardKind) -> bool {
    matches!(
        kind,
        HazardKind::LeadingSpace | HazardKind::TrailingSpace | HazardKind::UnusualWhitespace
    )
}

/// Render a filename with deceptive characters highlighted: leading/trailing
/// or unusual whitespace, zero-width / control / bidi characters, and
/// homoglyphs. Invisible characters are shown via a visible stand-in; each
/// flagged span carries a tooltip naming the hazard. `id_prefix` keeps the
/// per-span element ids unique when the name renders in more than one place.
pub(crate) fn name_hazard_element(name: &str, id_prefix: impl Into<SharedString>) -> AnyElement {
    name_hazard_element_elided(name, id_prefix, usize::MAX)
}

/// Single-line variant for constrained filename cells. Omitted hazardous
/// characters transfer their warning colour and tooltip to the ellipsis, so
/// narrowing a column can never make a deceptive name look clean.
pub(crate) fn name_hazard_element_elided(
    name: &str,
    id_prefix: impl Into<SharedString>,
    max_chars: usize,
) -> AnyElement {
    let id_prefix = id_prefix.into();
    let pieces = hazard_render_pieces(name);
    if pieces.iter().all(|piece| piece.hazard.is_none()) {
        return div()
            .truncate_middle()
            .child(elide_label(name, max_chars))
            .into_any_element();
    }
    let pieces = elide_hazard_pieces(&pieces, max_chars);
    let mut row = h_flex()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .items_center();
    for (i, piece) in pieces.into_iter().enumerate() {
        match piece.hazard {
            None => row = row.child(div().flex_shrink_0().child(piece.text)),
            Some(kind) => {
                let bg = if is_amber_hazard(kind) {
                    rgb(0xF59E0B)
                } else {
                    rgb(0xDC2626)
                };
                let label: SharedString = piece
                    .label
                    .unwrap_or_else(|| kind.summary().to_string())
                    .into();
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("{id_prefix}-haz-{i}").into()))
                        .flex_shrink_0()
                        .px_0p5()
                        .rounded_sm()
                        .bg(bg)
                        .text_color(gpui::white())
                        .child(piece.text)
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
    let kinds: Vec<String> = kinds
        .into_iter()
        .map(|k| crate::i18n::tr_static(k).to_string())
        .collect();
    Some(tr!("Deceptive name: {kinds}", kinds = kinds.join(", ")).to_string())
}

#[cfg(test)]
mod hazard_elision_tests {
    // Do not glob-import the parent's `gpui::*`: GPUI exports its own `test`
    // attribute, which would recursively re-expand Rust's `#[test]` here.
    use super::{HazardKind, elide_hazard_pieces, hazard_render_pieces};

    #[test]
    fn hidden_hazard_colours_the_ellipsis() {
        let pieces = hazard_render_pieces("prefix-long-\u{200b}-suffix-long.txt");
        let shown = elide_hazard_pieces(&pieces, 12);
        assert!(
            shown.iter().any(|piece| {
                piece.text == "…" && piece.hazard == Some(HazardKind::ZeroWidth)
            })
        );
    }

    #[test]
    fn clean_hidden_text_keeps_a_plain_ellipsis() {
        let pieces = hazard_render_pieces("a-very-long-but-clean-filename.txt");
        let shown = elide_hazard_pieces(&pieces, 12);
        assert!(shown.iter().any(|piece| piece.text == "…"));
        assert!(
            shown
                .iter()
                .filter(|piece| piece.text == "…")
                .all(|piece| piece.hazard.is_none())
        );
    }

    #[test]
    fn visible_hazard_stays_atomic() {
        let pieces = hazard_render_pieces("ab\u{200b}cd");
        let shown = elide_hazard_pieces(&pieces, 32);
        let hazard = shown
            .iter()
            .find(|piece| piece.hazard == Some(HazardKind::ZeroWidth))
            .expect("visible zero-width marker");
        assert!(hazard.text.chars().count() > 1);
    }
}
