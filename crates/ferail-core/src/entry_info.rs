//! Platform-neutral "Get Info" model.
//!
//! [`EntryInfo`] is the whole record the Get Info popup renders for one
//! filesystem object — a file, a folder, or a volume. It is built off the
//! UI thread by the host's platform gather code (which calls the native
//! crates) and consumed read-only by the renderer, in keeping with the
//! Prime Directive: paint never touches I/O and never formats.
//!
//! Each platform fills the subset of fields it can read; the missing ones
//! are simply absent rows. Editable values carry enough structured state
//! for the UI to render a control and emit an [`EntryInfoEdit`] back to the
//! native layer. This crate has zero platform and zero UI deps — the model
//! is data only, plus small pure helpers.

use crate::commands::TagColor;
use crate::msgid;

/// What an [`EntryInfo`] describes. Drives which sections make sense and how
/// the size row behaves (a file knows its size; a folder/volume calculates).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InfoTarget {
    #[default]
    File,
    Folder,
    Volume,
}

/// The full Get Info record for one object.
#[derive(Clone, Debug, Default)]
pub struct EntryInfo {
    /// Display name (file/folder/volume name). Editable via the Name row.
    pub name: String,
    /// Friendly kind string, e.g. "Folder", "PNG image", "Volume".
    pub kind: String,
    /// What this record is — file, folder, or volume.
    pub target: InfoTarget,
    /// Ordered, titled groups of rows. Render order is this order.
    pub sections: Vec<InfoSection>,
}

impl EntryInfo {
    /// Replace the first `Size` row's value in place. Used when an
    /// on-demand recursive "Calculate" finishes and streams the total back
    /// into an already-open record.
    pub fn set_size_value(&mut self, value: SizeValue) {
        for section in &mut self.sections {
            for row in &mut section.rows {
                if matches!(row.value, InfoValue::Size(_)) {
                    row.value = InfoValue::Size(value);
                    return;
                }
            }
        }
    }

    /// True when the (first) Size row is still awaiting an on-demand
    /// calculation — i.e. a folder/volume whose total we don't have yet.
    pub fn size_is_calculable(&self) -> bool {
        self.sections
            .iter()
            .flat_map(|s| &s.rows)
            .any(|r| matches!(r.value, InfoValue::Size(SizeValue::Calculable)))
    }

    /// Find the first row carrying an editable toggle for `attr`, if any.
    /// Used by edit round-trips to flip the displayed state optimistically.
    pub fn toggle(&self, attr: Attr) -> Option<bool> {
        self.sections
            .iter()
            .flat_map(|s| &s.rows)
            .find_map(|r| match &r.value {
                InfoValue::Toggle { on, attr: a } if *a == attr => Some(*on),
                _ => None,
            })
    }
}

/// A titled group of rows (e.g. "General", "Permissions", "Info").
#[derive(Clone, Debug)]
pub struct InfoSection {
    pub title: String,
    pub rows: Vec<InfoRow>,
}

impl InfoSection {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
        }
    }

    /// Push a row and return self, for fluent assembly in the gather code.
    pub fn row(mut self, label: impl Into<String>, value: InfoValue) -> Self {
        self.rows.push(InfoRow {
            label: label.into(),
            value,
        });
        self
    }

    /// Push a plain read-only text row only when `value` is non-empty.
    /// Keeps absent facts from rendering as blank rows.
    pub fn text_if(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.is_empty() {
            self.rows.push(InfoRow {
                label: label.into(),
                value: InfoValue::Text(value),
            });
        }
        self
    }
}

/// One labelled row.
#[derive(Clone, Debug)]
pub struct InfoRow {
    pub label: String,
    pub value: InfoValue,
}

/// The value side of a row. Read-only variants carry a pre-formatted string;
/// editable variants carry structured state plus the identity the UI needs to
/// emit the matching [`EntryInfoEdit`].
#[derive(Clone, Debug)]
pub enum InfoValue {
    /// Plain, already-formatted, read-only text.
    Text(String),
    /// A boolean attribute the user can toggle (Locked, Invisible, …).
    Toggle { on: bool, attr: Attr },
    /// The editable display name (rename).
    Name(String),
    /// Color labels (the 7 canonical Finder colors) plus any free-form tags.
    Tags {
        colors: Vec<TagColor>,
        custom: Vec<String>,
    },
    /// POSIX owner/group/permission matrix.
    Permissions(PermMatrix),
    /// A size that may need an on-demand recursive scan (folder/volume).
    Size(SizeValue),
}

/// A togglable boolean attribute. The host routes each one to the right native
/// writer — some are BSD flags (`chflags`), some are Finder/NSURL keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attr {
    /// `UF_IMMUTABLE` — Finder "Locked".
    Locked,
    /// `UF_HIDDEN` — Finder "Invisible".
    Invisible,
    /// `NSURLHasHiddenExtensionKey` — Finder "Hide extension".
    HiddenExtension,
    /// Stationery pad bit (Finder info).
    Stationery,
}

impl Attr {
    /// Stable label for the toggle row — a msgid; translate at the display
    /// site with `ferail_core::i18n::tr_raw`.
    pub fn label(self) -> &'static str {
        match self {
            Attr::Locked => msgid!("Locked"),
            Attr::Invisible => msgid!("Invisible"),
            Attr::HiddenExtension => msgid!("Hide extension"),
            Attr::Stationery => msgid!("Stationery pad"),
        }
    }
}

/// Read/write/execute triple for one POSIX class.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PermBits {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PermBits {
    /// Build from a 3-bit nibble (e.g. the owner triple of an octal mode).
    pub fn from_triple(bits: u32) -> Self {
        Self {
            read: bits & 0b100 != 0,
            write: bits & 0b010 != 0,
            execute: bits & 0b001 != 0,
        }
    }

    pub fn to_triple(self) -> u32 {
        (self.read as u32) << 2 | (self.write as u32) << 1 | (self.execute as u32)
    }

    /// "rwx" / "r-x" / "---" style fragment.
    pub fn symbolic(self) -> String {
        format!(
            "{}{}{}",
            if self.read { 'r' } else { '-' },
            if self.write { 'w' } else { '-' },
            if self.execute { 'x' } else { '-' },
        )
    }
}

/// POSIX ownership and permission state for a path.
#[derive(Clone, Debug, Default)]
pub struct PermMatrix {
    pub owner_name: String,
    pub group_name: String,
    pub owner: PermBits,
    pub group: PermBits,
    pub other: PermBits,
    /// Leading char of the symbolic string: 'd' dir, 'l' symlink, '-' file.
    pub kind_char: char,
    /// Full lower-12 mode bits, so writers preserve setuid/setgid/sticky.
    pub raw_mode: u32,
}

impl PermMatrix {
    /// Reconstruct the rwx 9 bits from the three triples, preserving the
    /// high bits (setuid/setgid/sticky) of `raw_mode`. This is what a
    /// permission edit hands to `chmod`.
    pub fn to_mode(&self) -> u32 {
        let rwx =
            self.owner.to_triple() << 6 | self.group.to_triple() << 3 | self.other.to_triple();
        (self.raw_mode & 0o7000) | rwx
    }

    /// Octal string of the rwx bits, e.g. "700".
    pub fn octal(&self) -> String {
        let rwx =
            self.owner.to_triple() << 6 | self.group.to_triple() << 3 | self.other.to_triple();
        format!("{:03o}", rwx)
    }

    /// Finder-style "drwx------ (700)" string.
    pub fn symbolic(&self) -> String {
        format!(
            "{}{}{}{} ({})",
            self.kind_char,
            self.owner.symbolic(),
            self.group.symbolic(),
            self.other.symbolic(),
            self.octal(),
        )
    }
}

/// A size value that may be unknown until the user asks for it.
#[derive(Clone, Debug)]
pub enum SizeValue {
    /// Known byte count, pre-formatted plus raw for the on-disk/“x bytes” line.
    /// `refreshable` is true for a folder/volume total that was reused from a
    /// cache and can be recomputed (the UI shows a refresh affordance); false
    /// for a file's own, always-current size.
    Known {
        bytes: u64,
        display: String,
        refreshable: bool,
    },
    /// Folder/volume size not computed yet; the UI shows a "Calculate" button.
    Calculable,
    /// A recursive scan is in flight.
    Calculating,
}

/// An edit emitted by the UI and applied by the host's native writers. The
/// target path/identity is held by the view, not the edit.
#[derive(Clone, Debug)]
pub enum EntryInfoEdit {
    SetToggle {
        attr: Attr,
        on: bool,
    },
    Rename(String),
    SetTags {
        colors: Vec<TagColor>,
        custom: Vec<String>,
    },
    SetPermissions(PermMatrix),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_triple_round_trips() {
        for bits in 0..8u32 {
            assert_eq!(PermBits::from_triple(bits).to_triple(), bits);
        }
    }

    #[test]
    fn mode_round_trips_and_preserves_high_bits() {
        // 0o4755 = setuid + rwxr-xr-x.
        let m = PermMatrix {
            owner: PermBits::from_triple(7),
            group: PermBits::from_triple(5),
            other: PermBits::from_triple(5),
            raw_mode: 0o4755,
            kind_char: '-',
            ..Default::default()
        };
        assert_eq!(m.octal(), "755");
        assert_eq!(m.to_mode(), 0o4755);
        assert_eq!(m.symbolic(), "-rwxr-xr-x (755)");
    }

    #[test]
    fn symbolic_dir_700() {
        let m = PermMatrix {
            owner: PermBits::from_triple(7),
            group: PermBits::default(),
            other: PermBits::default(),
            raw_mode: 0o700,
            kind_char: 'd',
            ..Default::default()
        };
        assert_eq!(m.symbolic(), "drwx------ (700)");
    }

    #[test]
    fn toggle_lookup_finds_attr() {
        let info = EntryInfo {
            sections: vec![InfoSection {
                title: "Attributes".into(),
                rows: vec![InfoRow {
                    label: "Locked".into(),
                    value: InfoValue::Toggle {
                        on: true,
                        attr: Attr::Locked,
                    },
                }],
            }],
            ..Default::default()
        };
        assert_eq!(info.toggle(Attr::Locked), Some(true));
        assert_eq!(info.toggle(Attr::Invisible), None);
    }
}
