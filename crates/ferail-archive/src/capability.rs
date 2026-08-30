//! Per-format capability matrix.
//!
//! Different archive formats support fundamentally different operations, and
//! the workbench UI must reflect that per-format rather than pretending they
//! are uniform. A zip is a random-access, editable, password-capable
//! container; a `.tar.gz` is an append-only compressed stream with no central
//! directory, so it cannot be edited in place; `.7z` we read but do not write
//! in v1. This table is the single source of truth the UI reads to decide
//! which affordances to enable: create/modify controls are gated on it, and a
//! format that cannot be written shows a read-only breadcrumb.
//!
//! Keeping the decision here (data, not scattered `match` arms in view code)
//! means adding a format or lifting a limitation is a one-line change that the
//! whole UI picks up.

use crate::format::Format;

/// What a given [`Format`] can do inside Ferail. Every field answers one
/// concrete UI question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// List the table of contents without extracting. True for every format
    /// we support, browsing is the floor.
    pub can_browse: bool,
    /// Extract all or a cherry-picked subset of entries.
    pub can_extract: bool,
    /// Create a fresh archive of this format from a set of inputs.
    pub can_create: bool,
    /// Add or remove individual entries in an existing archive without
    /// rewriting the whole thing. Requires random access (a central
    /// directory), so this is zip-only today. Tar-family archives are
    /// append-only streams; 7z we do not write yet.
    pub can_edit_in_place: bool,
    /// Encrypt newly created archives with a password. Read-time password
    /// support belongs to the decoder and is independent of this UI choice.
    pub supports_create_password: bool,
    /// Honor a [`crate::CompressionLevel`] on create.
    pub supports_levels: bool,
}

impl Capabilities {
    /// True when entries cannot be added to or removed from an *existing*
    /// archive of this format: the workbench presents it as read-only (its
    /// add/remove controls disabled, a read-only breadcrumb) even if the
    /// format can still be created fresh. Only zip is editable in place;
    /// tar-family and 7z are read-only here despite being creatable.
    pub fn is_read_only(self) -> bool {
        !self.can_edit_in_place
    }
}

impl Format {
    /// This format's capability row. The matrix, in one place.
    pub fn capabilities(self) -> Capabilities {
        match self {
            // The full-featured container: random access, editable, AES
            // password, per-entry level.
            Format::Zip => Capabilities {
                can_browse: true,
                can_extract: true,
                can_create: true,
                can_edit_in_place: true,
                supports_create_password: true,
                supports_levels: true,
            },

            // 7z: read, extract (incl. AES-encrypted), and create fresh via
            // `sevenz-rust`'s writer. No in-place edit (read-only in the
            // workbench). Create-time password/levels are a later addition.
            Format::SevenZ => Capabilities {
                can_browse: true,
                can_extract: true,
                can_create: true,
                can_edit_in_place: false,
                supports_create_password: false,
                supports_levels: false,
            },

            // Tar family: browse, extract, and create-fresh, but no in-place
            // edit (no central directory) and no password. Compressed
            // variants honor a level on the compression stage; plain `.tar`
            // has nothing to compress.
            Format::TarGz | Format::TarBz2 | Format::TarXz => Capabilities {
                can_browse: true,
                can_extract: true,
                can_create: true,
                can_edit_in_place: false,
                supports_create_password: false,
                supports_levels: true,
            },
            Format::Tar => Capabilities {
                can_browse: true,
                can_extract: true,
                can_create: true,
                can_edit_in_place: false,
                supports_create_password: false,
                supports_levels: false,
            },

            // Single compressed members: one logical entry, create-fresh from
            // one input, level on the compressor, no password, no editing.
            Format::Gzip | Format::Bzip2 | Format::Xz => Capabilities {
                can_browse: true,
                can_extract: true,
                can_create: true,
                can_edit_in_place: false,
                supports_create_password: false,
                supports_levels: true,
            },

            // LHA: browse and extract only. `delharc` is a decoder: it has
            // no compressor, so this is the first format in the matrix that
            // cannot be created at all. It is therefore absent from
            // `creatable_multi_file` below, which is what keeps it out of the
            // Create Archive picker.
            Format::Lha => Capabilities {
                can_browse: true,
                can_extract: true,
                can_create: false,
                can_edit_in_place: false,
                supports_create_password: false,
                supports_levels: false,
            },
        }
    }

    /// Formats offered in the "Create Archive" picker: those that can be
    /// created. Single-member compressors are excluded from the multi-file
    /// create flow (they hold one file); they are reachable through a
    /// "Compress" single-file action instead.
    pub fn creatable_multi_file() -> &'static [Format] {
        &[
            Format::Zip,
            Format::SevenZ,
            Format::Tar,
            Format::TarGz,
            Format::TarBz2,
            Format::TarXz,
        ]
    }
}
