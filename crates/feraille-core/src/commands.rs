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

/// Stable identifier for a user-invokable action. Format
/// `"category.action_name"`, lowercase, dot-separated. Use
/// [`find`] to look up the metadata for an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandId(pub &'static str);

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
        title: "Move to Trash",
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
    CommandSpec {
        id: CommandId("file.reveal_in_finder"),
        title: "Reveal in Finder",
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
        shortcuts: &[Shortcut::primary("P")],
    },
    CommandSpec {
        id: CommandId("view.toggle_hidden"),
        title: "Show Hidden Files",
        category: Category::View,
        shortcuts: &[Shortcut::primary_shift(".")],
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
    // Disk usage. Cmd+Shift+D opens (or focuses) the dedicated Disk
    // Usage window. The other two commands are scoped to that window
    // — the dispatcher gates them on focus so they don't shadow
    // similarly-bound actions in the main file pane.
    CommandSpec {
        id: CommandId("view.disk_usage"),
        title: "Disk Usage",
        category: Category::View,
        shortcuts: &[Shortcut::primary_shift("D")],
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
];

pub fn all_commands() -> &'static [CommandSpec] {
    CATALOGUE
}

pub fn find(id: CommandId) -> Option<&'static CommandSpec> {
    CATALOGUE.iter().find(|c| c.id == id)
}
