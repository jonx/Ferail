//! What each customizable menu can contain, for the settings UI.
//!
//! A menu is planned against a live right-click: which entries appear depends
//! on what was clicked. The settings UI has no right-click to work from, so it
//! needs the full set a surface can ever show, in menu order. That list is
//! here.
//!
//! Two lists could drift apart, so they are checked against each other rather
//! than trusted: `MenuPlan::render` asserts in debug builds that every entry it
//! is about to draw is listed here for its surface. Adding an entry to a menu
//! and forgetting this file then fails on the first right-click in a dev build,
//! which is the earliest anyone could notice.
//!
//! Labels come from the command catalogue wherever the id is a catalogue
//! command, so the settings list says what the palette and the shortcuts page
//! say. Menu-only ids carry their own label here, which is the one place they
//! are written down for display.

use gpui::SharedString;

use ferail_core::commands::CommandId;

use super::{MenuSurface, ids};

/// One position in a built-in menu: an entry, or the separator the menu is
/// written with.
///
/// The separators belong here rather than only in the builder because they are
/// part of what a user starts from. An editor that showed the entries without
/// them would silently drop every group boundary the first time anyone moved
/// anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Item {
    Entry(CommandId),
    Separator,
}

impl Item {
    pub(crate) fn id(self) -> Option<CommandId> {
        match self {
            Self::Entry(id) => Some(id),
            Self::Separator => None,
        }
    }
}

use Item::Entry as E;
use Item::Separator as S;

/// The file-list row and icon-grid cell menu, in the order it is built.
const FILE_ROW: &[Item] = &[
    E(ids::OPEN),
    E(ids::OPEN_IN_NEW_TAB),
    E(ids::EDIT),
    E(ids::EDIT_IMAGE),
    E(ids::EDIT_IN_SYSTEM_EDITOR),
    S,
    E(ids::GET_INFO),
    E(ids::QUICK_LOOK),
    E(ids::SLIDESHOW_FROM_HERE),
    S,
    E(ids::REVEAL),
    E(ids::COPY_PATH),
    E(ids::GENERATE_SHA256),
    E(ids::VERIFY_CHECKSUMS),
    E(ids::CREATE_CHECKSUM_FILE),
    E(ids::SHOW_LOCK_HOLDERS),
    E(ids::OPEN_TERMINAL_HERE),
    S,
    E(ids::RENAME),
    E(ids::BULK_RENAME),
    E(ids::DUPLICATE),
    E(ids::MAKE_ALIAS),
    E(ids::COMPRESS),
    E(ids::EXTRACT),
    E(ids::CONVERT_ARCHIVE),
    E(ids::OPEN_AS_ARCHIVE),
    S,
    E(ids::CLEAR_QUARANTINE),
    S,
    E(ids::TOGGLE_FAVORITE),
    E(ids::OPEN_WITH),
    E(ids::TAGS),
    S,
    E(ids::MOVE_TO_TRASH),
    E(ids::DELETE_IMMEDIATELY),
    #[cfg(windows)]
    S,
    #[cfg(windows)]
    E(ids::WINDOWS_CONTEXT_MENU),
];

/// The empty-space menu, which targets the folder being browsed.
const FILE_BACKGROUND: &[Item] = &[
    E(ids::NEW_FOLDER),
    S,
    E(ids::PASTE),
    E(ids::SELECT_ALL),
    S,
    E(ids::GET_INFO),
    E(ids::REVEAL),
    E(ids::COPY_PATH),
    E(ids::OPEN_TERMINAL_HERE),
    S,
    E(ids::PIN_TO_FAVORITES),
    S,
    E(ids::REFRESH),
];

/// A row in a trash folder.
///
/// Deliberately short. Most of the file menu is meaningless on something the
/// user already threw away: renaming, duplicating, compressing, tagging or
/// favouriting a deleted item, and "Move to Trash" on an item that is in the
/// trash. What is left is looking at it, finding out what it is, putting it
/// back, and getting rid of it for good.
const TRASH_ROW: &[Item] = &[
    E(ids::OPEN),
    E(ids::OPEN_IN_NEW_TAB),
    S,
    E(ids::RESTORE_FROM_TRASH),
    S,
    E(ids::GET_INFO),
    E(ids::QUICK_LOOK),
    E(ids::REVEAL),
    E(ids::COPY_PATH),
    S,
    E(ids::DELETE_IMMEDIATELY),
    E(ids::EMPTY_TRASH),
];

/// Empty space in a trash folder. No New Folder, no Paste: the trash is not
/// somewhere you put things on purpose.
const TRASH_BACKGROUND: &[Item] = &[
    E(ids::SELECT_ALL),
    S,
    E(ids::REVEAL),
    E(ids::COPY_PATH),
    S,
    E(ids::EMPTY_TRASH),
    E(ids::REFRESH),
];

/// Everything a surface can show, separators included, in menu order.
pub(crate) fn items(surface: MenuSurface) -> &'static [Item] {
    match surface {
        MenuSurface::FileRow => FILE_ROW,
        MenuSurface::FileBackground => FILE_BACKGROUND,
        MenuSurface::TrashRow => TRASH_ROW,
        MenuSurface::TrashBackground => TRASH_BACKGROUND,
    }
}

/// The commands a surface can show, without the separators.
pub(crate) fn entries(surface: MenuSurface) -> impl Iterator<Item = CommandId> {
    items(surface).iter().filter_map(|item| item.id())
}

pub(crate) fn lists(surface: MenuSurface, id: CommandId) -> bool {
    entries(surface).any(|known| known == id)
}

/// Display name for the settings list.
///
/// The catalogue's title wins whenever there is one, so a command is called
/// the same thing here, in the palette and in the shortcuts page. Only the
/// menu-only ids need an answer of their own.
pub(crate) fn label(id: CommandId) -> SharedString {
    if let Some(spec) = ferail_core::commands::find(id) {
        return crate::i18n::tr_static(spec.title);
    }
    match id.0 {
        // Deliberately the neutral name rather than the per-platform label the
        // menu shows ("Edit in TextEdit" / "Edit in Notepad"): one preference
        // governs the entry on every platform.
        "file.edit_in_system_editor" => tr!("Edit in the system text editor"),
        "file.slideshow_from_here" => tr!("Slideshow from Here"),
        "file.show_lock_holders" => tr!("What’s Locking This?"),
        "file.bulk_rename" => tr!("Rename Multiple Items…"),
        "file.convert_archive" => tr!("Convert Archive…"),
        "file.open_as_archive" => tr!("Open as Archive"),
        "file.toggle_favorite" => tr!("Add to / Remove from Favorites"),
        "file.windows_context_menu" => tr!("More options from Windows…"),
        "selection.select_all" => tr!("Select All"),
        "file.restore_from_trash" => tr!("Put Back"),
        // An id in the spec but not in this build: shown as itself rather
        // than dropped, so a preference from a newer version stays visible and
        // reversible instead of looking like it was lost.
        other => SharedString::from(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{entries, items, label, lists, Item};
    use crate::menu_plan::{ids, prefs::ALWAYS_VISIBLE, MenuSurface};

    #[test]
    fn every_surface_lists_something_and_lists_it_once() {
        for surface in MenuSurface::ALL {
            let list: Vec<_> = entries(surface).collect();
            assert!(!list.is_empty(), "{} lists nothing", surface.key());
            let mut seen = Vec::new();
            for id in list {
                assert!(!seen.contains(&id.0), "{} lists {} twice", surface.key(), id.0);
                seen.push(id.0);
            }
        }
    }

    #[test]
    fn built_in_separators_never_bookend_a_menu_or_run_two_deep() {
        // The inventory is what an editor hands the user as a starting point,
        // so a stray separator here would be one they have to clean up.
        for surface in MenuSurface::ALL {
            let list = items(surface);
            assert!(!list.first().is_some_and(|item| *item == Item::Separator));
            assert!(!list.last().is_some_and(|item| *item == Item::Separator));
            for pair in list.windows(2) {
                assert!(
                    pair != [Item::Separator, Item::Separator],
                    "{} has a doubled separator",
                    surface.key()
                );
            }
        }
    }

    #[test]
    fn the_never_hidden_entries_are_actually_in_a_menu() {
        // A floor naming an entry no menu has would protect nothing while
        // reading as though it protected something.
        for id in ALWAYS_VISIBLE {
            assert!(
                MenuSurface::ALL.iter().any(|surface| lists(*surface, id)),
                "{} is protected but appears in no menu",
                id.0
            );
        }
    }

    #[test]
    fn every_listed_entry_has_a_display_name() {
        for surface in MenuSurface::ALL {
            for id in entries(surface) {
                let label = label(id);
                assert!(!label.is_empty(), "{} has no label", id.0);
                // The fallback returns the raw id: fine for an unknown id from
                // a newer build, wrong for one this build lists itself.
                assert_ne!(label.as_ref(), id.0, "{} falls through to its id", id.0);
            }
        }
    }

    #[test]
    fn the_two_menus_are_not_the_same_menu() {
        assert!(lists(MenuSurface::FileRow, ids::MOVE_TO_TRASH));
        assert!(!lists(MenuSurface::FileBackground, ids::MOVE_TO_TRASH));
        // Get Info and Copy Path are in both, which is the whole reason a
        // preference is keyed on (surface, command) and not on command alone.
        assert!(lists(MenuSurface::FileRow, ids::GET_INFO));
        assert!(lists(MenuSurface::FileBackground, ids::GET_INFO));
    }
}
