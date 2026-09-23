//! Which commands apply to which kinds of file.
//!
//! A surface ([`super::MenuSurface`]) says where a menu opens and what it can
//! ever contain. What is right-clicked narrows it further: Extract means
//! nothing on a photo, Show Contents nothing on a text file. That second rule
//! is this table. Each command names the target types it handles; a command
//! the table does not name handles every type.
//!
//! A target's type is decided from what the row already carries (its kind,
//! name and cached content description), the same signals the list uses to
//! pick its icon, so classifying needs no I/O and is safe while a menu is
//! being built.

use ferail_core::commands::CommandId;
use ferail_core::{EntryKind, FileEntry};

use super::ids;
use crate::icons::{FileTypeTint, classify_file};

/// What a menu target is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetType {
    Folder,
    /// A directory shown as one item: an application, an installer, a
    /// document package. macOS only; see `ferail_core::packages`.
    Package,
    /// A file Ferail can browse and extract as an archive, by its name.
    Archive,
    /// A checksum list (`SHA256SUMS`, `*.sha256`, …).
    ChecksumManifest,
    Image,
    Video,
    Audio,
    Document,
    Code,
    DiskImage,
    Executable,
    Other,
}

impl TargetType {
    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of [`TargetType`]s.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TypeSet(u16);

impl TypeSet {
    pub(crate) const EMPTY: Self = Self(0);
    pub(crate) const ALL: Self = Self(u16::MAX);

    pub(crate) const fn of(types: &[TargetType]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < types.len() {
            bits |= types[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub(crate) const fn without(self, types: &[TargetType]) -> Self {
        Self(self.0 & !Self::of(types).0)
    }

    pub(crate) fn insert(&mut self, t: TargetType) {
        self.0 |= t.bit();
    }

    pub(crate) fn contains(self, t: TargetType) -> bool {
        self.0 & t.bit() != 0
    }

    pub(crate) fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// Anything that is not a folder or a package.
const FILES: TypeSet = TypeSet::ALL.without(&[TargetType::Folder, TargetType::Package]);
/// Files a text editor can make sense of. Media, archives, disk images and
/// programs are binary by nature; anything else may be text.
const TEXT_LIKE: TypeSet = FILES.without(&[
    TargetType::Image,
    TargetType::Video,
    TargetType::Audio,
    TargetType::Archive,
    TargetType::DiskImage,
    TargetType::Executable,
]);
/// What a command acting on one directory can take.
const DIRECTORIES: TypeSet = TypeSet::of(&[TargetType::Folder, TargetType::Package]);

/// The type of one row.
pub(crate) fn target_type(entry: &FileEntry) -> TargetType {
    if matches!(entry.kind, EntryKind::Directory) {
        return if ferail_core::packages::is_package_name(&entry.name) {
            TargetType::Package
        } else {
            TargetType::Folder
        };
    }
    if crate::shell::verify::entry_is_manifest(entry) {
        return TargetType::ChecksumManifest;
    }
    if ferail_archive::Format::is_archive_path(&entry.name) {
        return TargetType::Archive;
    }
    match classify_file(&entry.name, &entry.display_magic) {
        FileTypeTint::Image => TargetType::Image,
        FileTypeTint::Video => TargetType::Video,
        FileTypeTint::Audio => TargetType::Audio,
        FileTypeTint::Document => TargetType::Document,
        FileTypeTint::Code => TargetType::Code,
        FileTypeTint::Disk => TargetType::DiskImage,
        FileTypeTint::Executable => TargetType::Executable,
        // An archive format Ferail cannot open (RAR, Zstandard) is, for its
        // commands, just a file.
        FileTypeTint::Archive
        | FileTypeTint::Folder
        | FileTypeTint::Symlink
        | FileTypeTint::Unknown => TargetType::Other,
    }
}

/// The types a menu acts on: the right-clicked row's, and the union over
/// every targeted row.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TargetTypes {
    pub anchor: Option<TargetType>,
    /// Every type among the targets. Conservatively [`TypeSet::ALL`] when the
    /// selection is too large to classify while the menu opens: a command
    /// shown for a set it partly applies to acts on the part it handles.
    pub any: TypeSet,
}

/// Which of the targets a rule looks at.
#[derive(Clone, Copy, Debug)]
enum Scope {
    /// The row that was right-clicked: for commands that act on one item.
    Anchor,
    /// Any targeted row: for commands that act on the matching subset.
    Any,
}

struct Rule {
    id: CommandId,
    scope: Scope,
    handles: TypeSet,
}

const fn rule(id: CommandId, scope: Scope, handles: TypeSet) -> Rule {
    Rule { id, scope, handles }
}

use TargetType as T;

/// The table. A command absent from it handles every type.
const RULES: &[Rule] = &[
    rule(ids::SHOW_CONTENTS, Scope::Anchor, TypeSet::of(&[T::Package, T::Archive])),
    rule(ids::EDIT, Scope::Anchor, TEXT_LIKE),
    rule(ids::EDIT_IN_SYSTEM_EDITOR, Scope::Anchor, TEXT_LIKE),
    rule(ids::EDIT_IMAGE, Scope::Anchor, TypeSet::of(&[T::Image])),
    rule(ids::SLIDESHOW_FROM_HERE, Scope::Anchor, FILES),
    rule(ids::GENERATE_SHA256, Scope::Anchor, FILES),
    rule(ids::VERIFY_CHECKSUMS, Scope::Anchor, TypeSet::of(&[T::ChecksumManifest])),
    rule(ids::OPEN_TERMINAL_HERE, Scope::Anchor, DIRECTORIES),
    rule(ids::TOGGLE_FAVORITE, Scope::Anchor, DIRECTORIES),
    rule(ids::EXTRACT, Scope::Any, TypeSet::of(&[T::Archive])),
    rule(ids::CONVERT_ARCHIVE, Scope::Anchor, TypeSet::of(&[T::Archive])),
    // Any other file may still be an archive under a misleading name (a
    // .docx, a .jar, an extensionless download): the workbench probes its
    // content. A recognised archive already has Show Contents.
    rule(ids::OPEN_AS_ARCHIVE, Scope::Anchor, FILES.without(&[T::Archive])),
];

/// Whether command `id` handles `targets`.
pub(crate) fn handles(id: CommandId, targets: &TargetTypes) -> bool {
    let Some(rule) = RULES.iter().find(|r| r.id == id) else {
        return true;
    };
    match rule.scope {
        Scope::Anchor => targets.anchor.is_some_and(|t| rule.handles.contains(t)),
        Scope::Any => targets.any.intersects(rule.handles),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(t: TargetType) -> TargetTypes {
        let mut any = TypeSet::EMPTY;
        any.insert(t);
        TargetTypes { anchor: Some(t), any }
    }

    #[test]
    fn show_contents_is_for_packages_and_archives_only() {
        assert!(handles(ids::SHOW_CONTENTS, &only(T::Package)));
        assert!(handles(ids::SHOW_CONTENTS, &only(T::Archive)));
        assert!(!handles(ids::SHOW_CONTENTS, &only(T::Folder)));
        assert!(!handles(ids::SHOW_CONTENTS, &only(T::Document)));
    }

    #[test]
    fn open_as_archive_steps_aside_for_a_recognised_archive() {
        assert!(handles(ids::OPEN_AS_ARCHIVE, &only(T::Document)));
        assert!(handles(ids::OPEN_AS_ARCHIVE, &only(T::Other)));
        assert!(!handles(ids::OPEN_AS_ARCHIVE, &only(T::Archive)));
        assert!(!handles(ids::OPEN_AS_ARCHIVE, &only(T::Folder)));
    }

    #[test]
    fn extract_looks_at_every_target_not_just_the_anchor() {
        let mixed = TargetTypes {
            anchor: Some(T::Image),
            any: TypeSet::of(&[T::Image, T::Archive]),
        };
        assert!(handles(ids::EXTRACT, &mixed));
        assert!(!handles(ids::EXTRACT, &only(T::Image)));
    }

    #[test]
    fn the_text_editor_is_not_offered_for_binary_files() {
        assert!(handles(ids::EDIT, &only(T::Code)));
        assert!(handles(ids::EDIT, &only(T::Other)));
        for t in [T::Image, T::Archive, T::Video, T::Executable] {
            assert!(!handles(ids::EDIT, &only(t)), "{t:?}");
            assert!(!handles(ids::EDIT_IN_SYSTEM_EDITOR, &only(t)), "{t:?}");
        }
    }

    #[test]
    fn commands_outside_the_table_handle_everything() {
        for t in [T::Folder, T::Package, T::Image, T::Other] {
            assert!(handles(ids::RENAME, &only(t)));
        }
    }

    #[test]
    fn every_rule_names_a_known_id_once() {
        for (i, r) in RULES.iter().enumerate() {
            assert!(
                RULES[i + 1..].iter().all(|other| other.id != r.id),
                "{} has two rules",
                r.id.0
            );
        }
    }
}
