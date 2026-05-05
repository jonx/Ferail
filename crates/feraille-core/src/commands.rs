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
    Window,
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
        Self {
            key,
            primary: true,
            shift: false,
            alt: false,
        }
    }
    pub const fn primary_shift(key: &'static str) -> Self {
        Self {
            key,
            primary: true,
            shift: true,
            alt: false,
        }
    }
}

pub struct CommandSpec {
    pub id: CommandId,
    pub title: &'static str,
    pub category: Category,
    pub default_shortcut: Option<Shortcut>,
}

const CATALOGUE: &[CommandSpec] = &[
    // App
    CommandSpec {
        id: CommandId("app.about"),
        title: "About Feraille",
        category: Category::App,
        default_shortcut: None,
    },
    CommandSpec {
        id: CommandId("app.settings"),
        title: "Settings…",
        category: Category::App,
        default_shortcut: Some(Shortcut::primary(",")),
    },
    // File
    CommandSpec {
        id: CommandId("file.new_tab"),
        title: "New Tab",
        category: Category::File,
        default_shortcut: Some(Shortcut::primary("T")),
    },
    CommandSpec {
        id: CommandId("file.get_info"),
        title: "Get Info",
        category: Category::File,
        default_shortcut: Some(Shortcut::primary("I")),
    },
    // View
    CommandSpec {
        id: CommandId("view.toggle_hidden"),
        title: "Show Hidden Files",
        category: Category::View,
        default_shortcut: Some(Shortcut::primary_shift(".")),
    },
    // Go
    CommandSpec {
        id: CommandId("go.back"),
        title: "Back",
        category: Category::Go,
        default_shortcut: Some(Shortcut::primary("[")),
    },
    CommandSpec {
        id: CommandId("go.forward"),
        title: "Forward",
        category: Category::Go,
        default_shortcut: Some(Shortcut::primary("]")),
    },
    CommandSpec {
        id: CommandId("go.parent"),
        title: "Enclosing Folder",
        category: Category::Go,
        default_shortcut: Some(Shortcut::primary("Up")),
    },
    CommandSpec {
        id: CommandId("go.home"),
        title: "Home",
        category: Category::Go,
        default_shortcut: Some(Shortcut::primary_shift("H")),
    },
];

pub fn all_commands() -> &'static [CommandSpec] {
    CATALOGUE
}

pub fn find(id: CommandId) -> Option<&'static CommandSpec> {
    CATALOGUE.iter().find(|c| c.id == id)
}
