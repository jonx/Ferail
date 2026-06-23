//! Command catalogue — the stable identity of every user-invokable
//! action in Feraille.
//!
//! Menus, keyboard shortcuts, the future command palette, and any
//! future scripting / plugin surface all reference commands by
//! [`CommandId`]. Re-binding a shortcut, hiding a menu item, or
//! exposing an action to a script is then a matter of changing a
//! binding — not the command itself.
//!
//! This module is platform-neutral. Shortcuts are described in a
//! neutral DSL (key + modifier flags); shell crates translate.
//!
//! Built-in AppKit / Win32 actions (Hide, Quit, Cut, Copy, Minimize,
//! Zoom, Close…) are deliberately NOT in the catalogue. They ride
//! the platform's responder chain unchanged. Only Feraille-owned
//! behaviour goes here.

/// User-facing label for "show in OS file browser" — Finder on macOS,
/// Explorer on Windows, "File Manager" elsewhere.
#[cfg(target_os = "macos")]
pub const REVEAL_LABEL: &str = "Reveal in Finder";
#[cfg(windows)]
pub const REVEAL_LABEL: &str = "Reveal in Explorer";
#[cfg(not(any(target_os = "macos", windows)))]
pub const REVEAL_LABEL: &str = "Reveal in File Manager";

/// User-facing label for "send to OS trash" — macOS Trash, Windows
/// Recycle Bin.
#[cfg(target_os = "macos")]
pub const TRASH_LABEL: &str = "Move to Trash";
#[cfg(not(target_os = "macos"))]
pub const TRASH_LABEL: &str = "Move to Recycle Bin";

/// User-facing label for "remove the downloaded-from-the-Internet
/// mark and its provenance record" — `com.apple.quarantine` +
/// `kMDItemWhereFroms` on macOS, the `Zone.Identifier` ADS on
/// Windows (where the verb of art is "Unblock").
#[cfg(windows)]
pub const CLEAR_QUARANTINE_LABEL: &str = "Unblock";
#[cfg(not(windows))]
pub const CLEAR_QUARANTINE_LABEL: &str = "Clear Quarantine";

/// Stable identifier for a user-invokable action. Format
/// `"category.action_name"`, lowercase, dot-separated. Use
/// [`find`] to look up the metadata for an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandId(pub &'static str);

/// Optional payload attached to a command when it fires from a
/// menu item that needs to disambiguate (e.g. which tag colour
/// the user picked). Plain `CommandId` actions ride with `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPayload {
    /// `file.set_tag`: which Finder tag the user picked. The
    /// strings are the canonical Finder tag names — macOS resolves
    /// them via Launch Services so the colour swatch shown in
    /// Finder is the system's, not ours. `None` means "clear all".
    Tag(Option<TagColor>),
    /// `file.open_with_app`: filesystem path to the `.app` bundle
    /// the user picked from the Open With submenu. Stored as a
    /// string to keep this enum platform-neutral; the macOS
    /// dispatcher feeds it back into `NSWorkspace.open(at:with:)`.
    OpenWithApp { app_path: String },
}

/// The seven Finder colour tags. Display order matches Finder's
/// own row (red → orange → yellow → green → blue → purple → grey).
/// Names are the system-recognised English tag names; macOS maps
/// them to localised display strings + colour swatches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl TagColor {
    /// All seven, in Finder's display order.
    pub const ALL: [TagColor; 7] = [
        TagColor::Red,
        TagColor::Orange,
        TagColor::Yellow,
        TagColor::Green,
        TagColor::Blue,
        TagColor::Purple,
        TagColor::Gray,
    ];

    /// English tag name as `URLTagNamesKey` understands it. Pass
    /// directly to / from `NSURL.setResourceValue:forKey:`.
    pub fn name(self) -> &'static str {
        match self {
            TagColor::Red => "Red",
            TagColor::Orange => "Orange",
            TagColor::Yellow => "Yellow",
            TagColor::Green => "Green",
            TagColor::Blue => "Blue",
            TagColor::Purple => "Purple",
            TagColor::Gray => "Gray",
        }
    }

    /// `Some(_)` only for the seven canonical names; user-defined
    /// tags collapse to `None` (the menu still shows them as
    /// "set" via the checkmark on the matching colour, though only
    /// when the name happens to match).
    pub fn from_name(s: &str) -> Option<TagColor> {
        match s {
            "Red" => Some(TagColor::Red),
            "Orange" => Some(TagColor::Orange),
            "Yellow" => Some(TagColor::Yellow),
            "Green" => Some(TagColor::Green),
            "Blue" => Some(TagColor::Blue),
            "Purple" => Some(TagColor::Purple),
            "Gray" | "Grey" => Some(TagColor::Gray),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    App,
    File,
    Edit,
    View,
    Go,
    Selection,
    Window,
    Help,
    /// Context-menu-only actions (right-click on a node, background of a
    /// pane, or treemap rect). Not surfaced in the menu bar — the bar
    /// builder filters by category and never asks for this one.
    Context,
}

/// Neutral keyboard shortcut DSL. The shell layer maps `primary` to
/// Cmd on macOS and Ctrl on Linux/Windows. `key` is the display name
/// — single-character keys ("T", "[", ",") for normal letters /
/// punctuation, named keys ("Up", "Down", "F2") for the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub key: &'static str,
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    pub const fn primary(key: &'static str) -> Self {
        Self { key, primary: true, shift: false, alt: false }
    }
    pub const fn primary_shift(key: &'static str) -> Self {
        Self { key, primary: true, shift: true, alt: false }
    }
    pub const fn primary_alt(key: &'static str) -> Self {
        Self { key, primary: true, shift: false, alt: true }
    }
    pub const fn bare(key: &'static str) -> Self {
        Self { key, primary: false, shift: false, alt: false }
    }
    pub const fn alt(key: &'static str) -> Self {
        Self { key, primary: false, shift: false, alt: true }
    }
}

pub struct CommandSpec {
    pub id: CommandId,
    pub title: &'static str,
    pub category: Category,
    /// Zero or more shortcuts. The **first** entry is the canonical
    /// binding shown in menus (AppKit menu items only support one key
    /// equivalent each); every entry — primary or alternate — is
    /// accepted by `keystroke_to_command` and listed in the
    /// Keyboard Shortcuts dialog. Empty slice means menu/palette-only.
    pub shortcuts: &'static [Shortcut],
}

impl CommandSpec {
    /// First (canonical) shortcut, used by the menu builder.
    pub fn primary_shortcut(&self) -> Option<&Shortcut> {
        self.shortcuts.first()
    }
}

/// Preview-pane toggle shortcut. Cmd+P on macOS (the established binding);
/// Ctrl+Shift+P elsewhere, because plain Ctrl+P is the system Print accelerator
/// on Windows/Linux and collides.
#[cfg(target_os = "macos")]
const TOGGLE_PREVIEW_SHORTCUT: Shortcut = Shortcut::primary("P");
#[cfg(not(target_os = "macos"))]
const TOGGLE_PREVIEW_SHORTCUT: Shortcut = Shortcut::primary_shift("P");

const CATALOGUE: &[CommandSpec] = &[
    // App
    CommandSpec {
        id: CommandId("app.about"),
        title: "About Feraille",
        category: Category::App,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("app.settings"),
        title: "Settings…",
        category: Category::App,
        shortcuts: &[Shortcut::primary(",")],
    },
    // File
    CommandSpec {
        id: CommandId("file.new_tab"),
        title: "New Tab",
        category: Category::File,
        shortcuts: &[Shortcut::primary("T")],
    },
    CommandSpec {
        id: CommandId("file.close_tab"),
        title: "Close Tab",
        category: Category::File,
        shortcuts: &[Shortcut::primary("W")],
    },
    CommandSpec {
        id: CommandId("file.reopen_closed_tab"),
        title: "Reopen Closed Tab",
        category: Category::File,
        shortcuts: &[Shortcut::primary_shift("T")],
    },
    CommandSpec {
        id: CommandId("file.new_folder"),
        title: "New Folder",
        category: Category::File,
        shortcuts: &[Shortcut::primary_shift("N")],
    },
    CommandSpec {
        id: CommandId("file.get_info"),
        title: "Get Info",
        category: Category::File,
        shortcuts: &[Shortcut::primary("I")],
    },
    CommandSpec {
        id: CommandId("file.move_to_trash"),
        title: TRASH_LABEL,
        category: Category::File,
        // Cmd+Backspace is the canonical Finder binding; bare Delete
        // is a friendly alternate for users coming from Windows/Linux.
        shortcuts: &[Shortcut::primary("Backspace"), Shortcut::bare("Delete")],
    },
    CommandSpec {
        id: CommandId("file.copy_path"),
        title: "Copy Path",
        category: Category::File,
        shortcuts: &[Shortcut::primary_shift("C")],
    },
    // Copy the whole displayed list (folder contents, duplicate-finder
    // groups, or search results) as newline-joined paths. No default
    // shortcut — reached via the toolbar menu and command palette.
    CommandSpec {
        id: CommandId("file.copy_file_list"),
        title: "Copy File List",
        category: Category::File,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.empty_trash"),
        title: "Empty Trash",
        category: Category::File,
        // Finder's Cmd+Shift+Delete; the keymap installs the chord
        // directly (the Shortcut DSL has no Delete key yet).
        shortcuts: &[],
    },
    // Clipboard file verbs (docs/features/FILE_OPS.md). Copy + Paste +
    // Move-Paste, plus Cut (Cmd+X): a cut marks its items so the next
    // plain Paste moves them (and clears the mark).
    CommandSpec {
        id: CommandId("file.copy"),
        title: "Copy",
        category: Category::File,
        shortcuts: &[Shortcut::primary("C")],
    },
    CommandSpec {
        id: CommandId("file.cut"),
        title: "Cut",
        category: Category::File,
        shortcuts: &[Shortcut::primary("X")],
    },
    CommandSpec {
        id: CommandId("file.paste"),
        title: "Paste",
        category: Category::File,
        shortcuts: &[Shortcut::primary("V")],
    },
    CommandSpec {
        id: CommandId("file.move_paste"),
        title: "Move Items Here",
        category: Category::File,
        shortcuts: &[Shortcut::primary_alt("V")],
    },
    CommandSpec {
        id: CommandId("file.reveal_in_finder"),
        title: REVEAL_LABEL,
        category: Category::File,
        shortcuts: &[Shortcut::primary_alt("R")],
    },
    CommandSpec {
        id: CommandId("file.refresh"),
        title: "Refresh",
        category: Category::File,
        shortcuts: &[Shortcut::bare("F5")],
    },
    // View
    CommandSpec {
        id: CommandId("view.search"),
        title: "Find",
        category: Category::View,
        shortcuts: &[Shortcut::primary("F")],
    },
    CommandSpec {
        id: CommandId("view.edit_breadcrumb"),
        title: "Edit Path",
        category: Category::View,
        shortcuts: &[Shortcut::primary("L")],
    },
    CommandSpec {
        id: CommandId("view.toggle_preview"),
        title: "Show Preview Pane",
        category: Category::View,
        shortcuts: &[TOGGLE_PREVIEW_SHORTCUT],
    },
    CommandSpec {
        id: CommandId("view.open_viewer"),
        title: "Open Viewer",
        category: Category::View,
        shortcuts: &[Shortcut::primary("Y")],
    },
    CommandSpec {
        id: CommandId("view.toggle_hidden"),
        title: "Show Hidden Files",
        category: Category::View,
        shortcuts: &[Shortcut::primary_shift(".")],
    },
    // Toolbar Sort menu. No shortcuts — they live in the sort
    // dropdown + command palette. Re-selecting the active column
    // flips direction.
    CommandSpec {
        id: CommandId("view.sort_name"),
        title: "Sort by Name",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.sort_size"),
        title: "Sort by Size",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.sort_kind"),
        title: "Sort by Kind",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.sort_modified"),
        title: "Sort by Date Modified",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.cycle_focus"),
        title: "Cycle Focus",
        category: Category::View,
        shortcuts: &[Shortcut::bare("F6")],
    },
    CommandSpec {
        id: CommandId("view.zoom_in"),
        title: "Zoom In",
        category: Category::View,
        shortcuts: &[Shortcut::primary("="), Shortcut::primary("+")],
    },
    CommandSpec {
        id: CommandId("view.zoom_out"),
        title: "Zoom Out",
        category: Category::View,
        shortcuts: &[Shortcut::primary("-")],
    },
    CommandSpec {
        id: CommandId("view.zoom_reset"),
        title: "Actual Size",
        category: Category::View,
        shortcuts: &[Shortcut::primary("0")],
    },
    // Tool results. Cmd+Shift+D opens Disk Usage as a tab-local result
    // surface; Search and Duplicate Finder use the same result host.
    // Window-specific Disk Usage commands remain available for the
    // standalone pop-out path.
    CommandSpec {
        id: CommandId("view.disk_usage"),
        title: "Disk Usage",
        category: Category::View,
        shortcuts: &[Shortcut::primary_shift("D")],
    },
    CommandSpec {
        id: CommandId("view.close_results"),
        title: "Close Results",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.open_in_window"),
        title: "Open Disk Usage in Window",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.find_duplicates"),
        title: "Find Duplicates",
        category: Category::View,
        shortcuts: &[Shortcut::primary_shift("U")],
    },
    CommandSpec {
        id: CommandId("disk_usage.refresh"),
        title: "Refresh Disk Usage",
        category: Category::View,
        // Cmd+R is a no-op when no DU window is open, so it's safe
        // to bind globally rather than gating on focus.
        shortcuts: &[Shortcut::primary("R")],
    },
    CommandSpec {
        id: CommandId("disk_usage.zoom_out"),
        title: "Disk Usage: Zoom Out",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.toggle_topn"),
        title: "Largest Files Panel",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.toggle_packages"),
        title: "Descend into Packages",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.toggle_follow_navigation"),
        title: "Follow Tab Navigation",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.coloring_category"),
        title: "Color by File Type",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.coloring_age"),
        title: "Color by Age",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.coloring_depth"),
        title: "Color by Depth Only",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.size_apparent"),
        title: "Size: Apparent",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.size_allocated"),
        title: "Size: Allocated (on disk)",
        category: Category::View,
        shortcuts: &[],
    },
    // The three theme commands are grouped under a "Theme" sub-submenu
    // by `app_menu::build_category_submenu` — title is the submenu's
    // own label, so individual items just say "Light" / "Dark" / etc.
    CommandSpec {
        id: CommandId("view.theme_light"),
        title: "Light",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.theme_dark"),
        title: "Dark",
        category: Category::View,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("view.theme_system"),
        title: "Match System",
        category: Category::View,
        shortcuts: &[],
    },
    // Go
    CommandSpec {
        id: CommandId("go.back"),
        title: "Back",
        category: Category::Go,
        shortcuts: &[Shortcut::primary("["), Shortcut::alt("Left")],
    },
    CommandSpec {
        id: CommandId("go.forward"),
        title: "Forward",
        category: Category::Go,
        shortcuts: &[Shortcut::primary("]"), Shortcut::alt("Right")],
    },
    CommandSpec {
        id: CommandId("go.parent"),
        title: "Enclosing Folder",
        category: Category::Go,
        shortcuts: &[Shortcut::primary("Up"), Shortcut::bare("Backspace")],
    },
    CommandSpec {
        id: CommandId("go.home"),
        title: "Home",
        category: Category::Go,
        shortcuts: &[Shortcut::primary_shift("H")],
    },
    // Selection — pane-aware. The dispatch handler routes to whichever
    // pane currently owns focus (Tree or List). Bare arrow keys /
    // Home / End / PageUp / PageDown / Enter / F2 / Escape only reach
    // these handlers when no modal text input is active (rename,
    // search, dialog, breadcrumb edit) because those intercept first.
    CommandSpec {
        id: CommandId("selection.cursor_up"),
        title: "Move Cursor Up",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("Up")],
    },
    CommandSpec {
        id: CommandId("selection.cursor_down"),
        title: "Move Cursor Down",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("Down")],
    },
    CommandSpec {
        id: CommandId("selection.cursor_first"),
        title: "Move Cursor to Top",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("Home")],
    },
    CommandSpec {
        id: CommandId("selection.cursor_last"),
        title: "Move Cursor to Bottom",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("End")],
    },
    CommandSpec {
        id: CommandId("selection.page_up"),
        title: "Page Up",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("PageUp")],
    },
    CommandSpec {
        id: CommandId("selection.page_down"),
        title: "Page Down",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("PageDown")],
    },
    CommandSpec {
        id: CommandId("selection.activate"),
        title: "Open Selection",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("Enter")],
    },
    CommandSpec {
        id: CommandId("selection.start_rename"),
        title: "Rename Selection",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("F2")],
    },
    CommandSpec {
        id: CommandId("selection.collapse_or_parent"),
        title: "Collapse / Parent",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("Left")],
    },
    CommandSpec {
        id: CommandId("selection.expand_or_first_child"),
        title: "Expand / First Child",
        category: Category::Selection,
        shortcuts: &[Shortcut::bare("Right")],
    },
    CommandSpec {
        id: CommandId("selection.dismiss"),
        title: "Dismiss / Exit",
        category: Category::Selection,
        // Escape: closes Get Info if open, otherwise drops Tree focus,
        // otherwise quits. Quirky semantics handled in dispatch.
        shortcuts: &[Shortcut::bare("Escape")],
    },
    // Window
    CommandSpec {
        id: CommandId("window.new_window"),
        title: "New Window",
        category: Category::Window,
        shortcuts: &[Shortcut::primary("N")],
    },
    CommandSpec {
        id: CommandId("window.close_window"),
        title: "Close Window",
        category: Category::Window,
        shortcuts: &[Shortcut::primary_shift("W")],
    },
    CommandSpec {
        id: CommandId("window.next_tab"),
        title: "Next Tab",
        category: Category::Window,
        shortcuts: &[Shortcut::primary_shift("]")],
    },
    CommandSpec {
        id: CommandId("window.prev_tab"),
        title: "Previous Tab",
        category: Category::Window,
        shortcuts: &[Shortcut::primary_shift("[")],
    },
    // Help
    CommandSpec {
        id: CommandId("help.github"),
        title: "Feraille on GitHub",
        category: Category::Help,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("help.shortcuts"),
        title: "Keyboard Shortcuts",
        category: Category::Help,
        shortcuts: &[Shortcut::primary("/")],
    },
    // Context-menu-only. Not in the menu bar; reachable via right-click
    // dispatch. `selection.activate` is the keyboard equivalent of
    // `file.open` — the two stay separate because the right-click and
    // keyboard paths capture target differently.
    CommandSpec {
        id: CommandId("file.open"),
        title: "Open",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.pin_to_favorites"),
        title: "Pin to Favorites",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.remove_from_favorites"),
        title: "Remove from Favorites",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("disk_usage.zoom_into"),
        title: "Zoom into",
        category: Category::Context,
        shortcuts: &[],
    },
    // Stage B — easy actions reachable from the right-click menu.
    // No keyboard shortcuts yet: Finder's bindings (Space, Cmd+D,
    // Cmd+L) collide with existing Feraille bindings or with text-
    // input contexts. A focus-aware dispatcher can layer them on
    // later without changing the menu surface.
    CommandSpec {
        id: CommandId("file.quick_look"),
        title: "Quick Look",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.rename"),
        title: "Rename",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.duplicate"),
        title: "Duplicate",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.make_alias"),
        title: "Make Alias",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.compress"),
        title: "Compress",
        category: Category::Context,
        shortcuts: &[],
    },
    // Stage C — Finder colour tags. `file.set_tag` fires with a
    // [`CommandPayload::Tag(Some(color))`] for the seven canonical
    // colours; `file.clear_tags` strips every tag in one shot.
    CommandSpec {
        id: CommandId("file.set_tag"),
        title: "Set Tag",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.clear_tags"),
        title: "Clear Tags",
        category: Category::Context,
        shortcuts: &[],
    },
    // Stage D — Open With submenu. Each pick fires `file.open_with_app`
    // with a [`CommandPayload::OpenWithApp`] carrying the chosen
    // app's bundle path.
    CommandSpec {
        id: CommandId("file.open_with_app"),
        title: "Open With",
        category: Category::Context,
        shortcuts: &[],
    },
    CommandSpec {
        id: CommandId("file.share"),
        title: "Share",
        category: Category::Context,
        shortcuts: &[],
    },
    // Folder-only context action: open the right-clicked directory
    // in a new tab in the same window. Mirrors Finder's primary
    // folder-menu action.
    CommandSpec {
        id: CommandId("file.open_in_new_tab"),
        title: "Open in New Tab",
        category: Category::Context,
        shortcuts: &[],
    },
    // Folder-only context action: open a terminal at the right-clicked
    // directory. Reachable from both the file-list and sidebar/tree
    // right-click menus; no keyboard shortcut yet.
    CommandSpec {
        id: CommandId("file.open_terminal_here"),
        title: "Open Terminal Here",
        category: Category::Context,
        shortcuts: &[],
    },
    // Quarantined rows only: strip the Mark-of-the-Web and its
    // provenance record (where-from URLs) from the selected files.
    CommandSpec {
        id: CommandId("file.clear_quarantine"),
        title: CLEAR_QUARANTINE_LABEL,
        category: Category::Context,
        shortcuts: &[],
    },
];

pub fn all_commands() -> &'static [CommandSpec] {
    CATALOGUE
}

pub fn find(id: CommandId) -> Option<&'static CommandSpec> {
    CATALOGUE.iter().find(|c| c.id == id)
}
