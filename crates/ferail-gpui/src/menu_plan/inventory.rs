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

/// The file-list row and icon-grid cell menu, in the order it is built.
const FILE_ROW: &[CommandId] = &[
    ids::OPEN,
    ids::OPEN_IN_NEW_TAB,
    ids::EDIT,
    ids::EDIT_IMAGE,
    ids::EDIT_IN_SYSTEM_EDITOR,
    ids::GET_INFO,
    ids::QUICK_LOOK,
    ids::SLIDESHOW_FROM_HERE,
    ids::REVEAL,
    ids::COPY_PATH,
    ids::GENERATE_SHA256,
    ids::VERIFY_CHECKSUMS,
    ids::CREATE_CHECKSUM_FILE,
    ids::SHOW_LOCK_HOLDERS,
    ids::OPEN_TERMINAL_HERE,
    ids::RENAME,
    ids::BULK_RENAME,
    ids::DUPLICATE,
    ids::MAKE_ALIAS,
    ids::COMPRESS,
    ids::EXTRACT,
    ids::CONVERT_ARCHIVE,
    ids::OPEN_AS_ARCHIVE,
    ids::CLEAR_QUARANTINE,
    ids::TOGGLE_FAVORITE,
    ids::OPEN_WITH,
    ids::TAGS,
    ids::MOVE_TO_TRASH,
    ids::DELETE_IMMEDIATELY,
    #[cfg(windows)]
    ids::WINDOWS_CONTEXT_MENU,
];

/// The empty-space menu, which targets the folder being browsed.
const FILE_BACKGROUND: &[CommandId] = &[
    ids::NEW_FOLDER,
    ids::PASTE,
    ids::SELECT_ALL,
    ids::GET_INFO,
    ids::REVEAL,
    ids::COPY_PATH,
    ids::OPEN_TERMINAL_HERE,
    ids::PIN_TO_FAVORITES,
    ids::REFRESH,
];

pub(crate) fn entries(surface: MenuSurface) -> &'static [CommandId] {
    match surface {
        MenuSurface::FileRow => FILE_ROW,
        MenuSurface::FileBackground => FILE_BACKGROUND,
    }
}

pub(crate) fn lists(surface: MenuSurface, id: CommandId) -> bool {
    entries(surface).contains(&id)
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
        // An id in the spec but not in this build: shown as itself rather
        // than dropped, so a preference from a newer version stays visible and
        // reversible instead of looking like it was lost.
        other => SharedString::from(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{FILE_BACKGROUND, FILE_ROW, entries, label, lists};
    use crate::menu_plan::{MenuSurface, ids, prefs::ALWAYS_VISIBLE};

    #[test]
    fn every_surface_lists_something_and_lists_it_once() {
        for surface in MenuSurface::ALL {
            let list = entries(surface);
            assert!(!list.is_empty(), "{} lists nothing", surface.key());
            let mut seen = Vec::new();
            for id in list {
                assert!(
                    !seen.contains(&id.0),
                    "{} lists {} twice",
                    surface.key(),
                    id.0
                );
                seen.push(id.0);
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
                let label = label(*id);
                assert!(!label.is_empty(), "{} has no label", id.0);
                // The fallback returns the raw id: fine for an unknown id from
                // a newer build, wrong for one this build lists itself.
                assert_ne!(label.as_ref(), id.0, "{} falls through to its id", id.0);
            }
        }
    }

    #[test]
    fn the_two_menus_are_not_the_same_menu() {
        assert_ne!(FILE_ROW.len(), 0);
        assert_ne!(FILE_BACKGROUND.len(), 0);
        assert!(FILE_ROW.contains(&ids::MOVE_TO_TRASH));
        assert!(!FILE_BACKGROUND.contains(&ids::MOVE_TO_TRASH));
        // Get Info and Copy Path are in both, which is the whole reason a
        // preference is keyed on (surface, command) and not on command alone.
        assert!(FILE_ROW.contains(&ids::GET_INFO) && FILE_BACKGROUND.contains(&ids::GET_INFO));
    }
}
