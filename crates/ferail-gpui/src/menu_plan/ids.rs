//! Stable ids for menu entries, in one table.
//!
//! An id is what a user's preference is stored against, so it outlives
//! renames, reorderings and rewrites of the code that builds the menu. Two
//! rules keep it honest:
//!
//! - **Reuse the catalogue id whenever the entry is a catalogue command.**
//!   `file.rename` in the menu is the same command as `file.rename` in the
//!   palette and in the shortcuts page, and nothing good comes of them
//!   disagreeing.
//! - **Mint under the same namespace when it is not**, and say why here. The
//!   entries below exist only as menu items today: giving each a `CommandSpec`
//!   would put it in the Cmd+K palette, where most would sit disabled because
//!   they need a right-clicked target to mean anything. When one grows a real
//!   catalogue entry, delete the constant here and point at the catalogue's.
//!
//! `menu_only_ids_are_deliberate` in the tests below pins that split: an id
//! that is neither a catalogue command nor listed as menu-only fails the
//! build's tests, so a typo cannot silently mint a third kind of id.

use ferail_core::commands::CommandId;

/// Ids that exist in `ferail_core::commands`. Written out rather than
/// imported because the catalogue exposes specs, not per-command constants.
pub(crate) const OPEN: CommandId = CommandId("file.open");
pub(crate) const OPEN_IN_NEW_TAB: CommandId = CommandId("file.open_in_new_tab");
pub(crate) const EDIT: CommandId = CommandId("file.edit");
pub(crate) const EDIT_IMAGE: CommandId = CommandId("file.edit_image");
pub(crate) const GET_INFO: CommandId = CommandId("file.get_info");
pub(crate) const QUICK_LOOK: CommandId = CommandId("file.quick_look");
pub(crate) const REVEAL: CommandId = CommandId("file.reveal_in_finder");
pub(crate) const COPY_PATH: CommandId = CommandId("file.copy_path");
pub(crate) const GENERATE_SHA256: CommandId = CommandId("file.generate_sha256");
pub(crate) const VERIFY_CHECKSUMS: CommandId = CommandId("file.verify_checksums");
pub(crate) const CREATE_CHECKSUM_FILE: CommandId = CommandId("file.create_checksum_file");
pub(crate) const OPEN_TERMINAL_HERE: CommandId = CommandId("file.open_terminal_here");
pub(crate) const RENAME: CommandId = CommandId("file.rename");
pub(crate) const DUPLICATE: CommandId = CommandId("file.duplicate");
pub(crate) const MAKE_ALIAS: CommandId = CommandId("file.make_alias");
pub(crate) const COMPRESS: CommandId = CommandId("file.compress");
pub(crate) const EXTRACT: CommandId = CommandId("file.extract");
pub(crate) const CLEAR_QUARANTINE: CommandId = CommandId("file.clear_quarantine");
pub(crate) const OPEN_WITH: CommandId = CommandId("file.open_with_app");
pub(crate) const TAGS: CommandId = CommandId("file.set_tag");
pub(crate) const MOVE_TO_TRASH: CommandId = CommandId("file.move_to_trash");
pub(crate) const DELETE_IMMEDIATELY: CommandId = CommandId("file.delete_immediately");
pub(crate) const NEW_FOLDER: CommandId = CommandId("file.new_folder");
pub(crate) const PASTE: CommandId = CommandId("file.paste");
pub(crate) const REFRESH: CommandId = CommandId("file.refresh");
pub(crate) const PIN_TO_FAVORITES: CommandId = CommandId("file.pin_to_favorites");

/// The system-editor escape hatch next to the built-in **Edit**. Its label is
/// per-platform ("Edit in TextEdit" / "Edit in Notepad"), which is exactly why
/// it is one id and not three.
pub(crate) const EDIT_IN_SYSTEM_EDITOR: CommandId = CommandId("file.edit_in_system_editor");

/// Starts the viewer slideshow anchored on the clicked file. Meaningless
/// without an anchor row, so it has no palette equivalent.
pub(crate) const SLIDESHOW_FROM_HERE: CommandId = CommandId("file.slideshow_from_here");

/// Names the processes holding the resolved targets open. Windows-only in
/// practice (the lookup is stubbed elsewhere), and target-bound.
pub(crate) const SHOW_LOCK_HOLDERS: CommandId = CommandId("file.show_lock_holders");

/// Pattern rename over the whole resolved set: the multi-selection twin of
/// `file.rename`, and a distinct entry because it appears alongside it.
pub(crate) const BULK_RENAME: CommandId = CommandId("file.bulk_rename");

/// Re-encode one archive into another format. Archive-anchored.
pub(crate) const CONVERT_ARCHIVE: CommandId = CommandId("file.convert_archive");

/// Browse any single file as an archive, deliberately broader than Extract
/// because the content probe happens off-thread afterwards.
pub(crate) const OPEN_AS_ARCHIVE: CommandId = CommandId("file.open_as_archive");

/// One entry that flips its label between Add to and Remove from Favorites.
/// The catalogue has both `file.pin_to_favorites` and
/// `file.remove_from_favorites`; keying the menu entry on either would make a
/// user's preference apply to half the states it can be in, so the toggle gets
/// an id of its own.
pub(crate) const TOGGLE_FAVORITE: CommandId = CommandId("file.toggle_favorite");

/// Hands the selection to the Windows Shell's own menu.
///
/// Declared on every platform though only Windows builds the entry: the id
/// table is the reference for what a preference can name, and a table with
/// holes in it per platform is a table nobody can check.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const WINDOWS_CONTEXT_MENU: CommandId = CommandId("file.windows_context_menu");

/// Select every row in the current listing.
pub(crate) const SELECT_ALL: CommandId = CommandId("selection.select_all");

/// Put a trashed item back where it came from.
pub(crate) const RESTORE_FROM_TRASH: CommandId = CommandId("file.restore_from_trash");

/// Empty the trash. A catalogue command, unlike the one above.
pub(crate) const EMPTY_TRASH: CommandId = CommandId("file.empty_trash");

/// Every id above that is deliberately not a catalogue command.
#[cfg(test)]
const MENU_ONLY: [CommandId; 10] = [
    EDIT_IN_SYSTEM_EDITOR,
    SLIDESHOW_FROM_HERE,
    SHOW_LOCK_HOLDERS,
    BULK_RENAME,
    CONVERT_ARCHIVE,
    OPEN_AS_ARCHIVE,
    TOGGLE_FAVORITE,
    WINDOWS_CONTEXT_MENU,
    SELECT_ALL,
    RESTORE_FROM_TRASH,
];

/// Every id this module declares, for the checks below.
#[cfg(test)]
const ALL: [CommandId; 37] = [
    OPEN,
    OPEN_IN_NEW_TAB,
    EDIT,
    EDIT_IMAGE,
    GET_INFO,
    QUICK_LOOK,
    REVEAL,
    COPY_PATH,
    GENERATE_SHA256,
    VERIFY_CHECKSUMS,
    CREATE_CHECKSUM_FILE,
    OPEN_TERMINAL_HERE,
    RENAME,
    DUPLICATE,
    MAKE_ALIAS,
    COMPRESS,
    EXTRACT,
    CLEAR_QUARANTINE,
    OPEN_WITH,
    TAGS,
    MOVE_TO_TRASH,
    DELETE_IMMEDIATELY,
    NEW_FOLDER,
    PASTE,
    REFRESH,
    PIN_TO_FAVORITES,
    EDIT_IN_SYSTEM_EDITOR,
    SLIDESHOW_FROM_HERE,
    SHOW_LOCK_HOLDERS,
    BULK_RENAME,
    CONVERT_ARCHIVE,
    OPEN_AS_ARCHIVE,
    TOGGLE_FAVORITE,
    WINDOWS_CONTEXT_MENU,
    SELECT_ALL,
    RESTORE_FROM_TRASH,
    EMPTY_TRASH,
];

#[cfg(test)]
mod tests {
    use super::{ALL, MENU_ONLY};

    #[test]
    fn menu_only_ids_are_deliberate() {
        for id in ALL {
            let in_catalogue = ferail_core::commands::find(id).is_some();
            let menu_only = MENU_ONLY.contains(&id);
            assert!(
                in_catalogue != menu_only,
                "{} is {}: an id must be either a catalogue command or a \
                 declared menu-only entry, never both and never neither",
                id.0,
                if in_catalogue {
                    "both a catalogue command and listed as menu-only"
                } else {
                    "neither a catalogue command nor listed as menu-only \
                     (typo, or a new entry that needs a line in MENU_ONLY)"
                }
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut seen: Vec<&'static str> = Vec::new();
        for id in ALL {
            assert!(!seen.contains(&id.0), "{} is declared twice", id.0);
            seen.push(id.0);
        }
    }

    #[test]
    fn ids_stay_in_the_catalogue_namespaces() {
        // Not decoration: the persisted preference is a flat string map, and
        // an unnamespaced id would collide the first time two surfaces or two
        // subsystems picked the same short name.
        for id in ALL {
            assert!(
                id.0.contains('.'),
                "{} needs a namespace prefix such as `file.`",
                id.0
            );
        }
    }
}
