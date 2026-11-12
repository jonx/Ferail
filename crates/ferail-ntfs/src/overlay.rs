//! Applying file-system changes to an existing [`CompactNtfsIndex`].
//!
//! The strategy is a rebuild from memory: the changed base records are
//! resolved against the old index, every untouched record and name is copied
//! into a fresh builder, and the result is finished exactly like a scan. That
//! costs one pass over the index (no volume I/O) and reuses the scan's own
//! adjacency, DOS-alias and stale-parent rules, so an updated index cannot
//! diverge from what a fresh scan of the same records would build.
//!
//! Every change is validated against the state it modifies. Whenever a change
//! does not fit (an unknown or reused record, a missing name, a rename that
//! may have moved a DOS alias, a directory removed while names still point
//! into it, an extension record that is absent or belongs elsewhere) the
//! outcome is [`ApplyOutcome::RebuildRequired`]: the caller must read the
//! volume again. The overlay never returns a silently stale index.

use std::collections::{HashMap, HashSet};

use crate::index::{
    namespace_raw, StandardInfo, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
};
use crate::{
    AttributeList, CompactNtfsIndex, FileMeta, FileName, FileRecord, FileReference, IndexBuilder,
    IndexStats, NameLink, NameNamespace, Result,
};

const MAX_NAME_UNITS: usize = 255;
const DOS_NAMESPACE: u8 = 2;

/// One synthetic change. The journal-driven helper uses [`Self::Reread`]
/// for everything because it re-reads each touched record; the name-level
/// kinds exist for callers that only have change metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IndexChange {
    /// A new base record with one name. Sizes start at zero.
    Create {
        file: FileReference,
        parent: FileReference,
        name: Vec<u16>,
        namespace: NameNamespace,
        directory: bool,
        attributes: u32,
        created_ticks: u64,
        modified_ticks: u64,
    },
    /// The base record is no longer in use.
    Delete { file: FileReference },
    /// One name of `file` moved from `(old_parent, old_name)` to
    /// `(new_parent, new_name)`. Covers both rename and move.
    Rename {
        file: FileReference,
        old_parent: FileReference,
        old_name: Vec<u16>,
        new_parent: FileReference,
        new_name: Vec<u16>,
    },
    /// New `$STANDARD_INFORMATION` attributes. A change that would toggle
    /// the reparse or directory nature needs a re-read instead.
    Attributes {
        file: FileReference,
        attributes: u32,
    },
    /// The record must be re-read: `base` is its current parsed state (not
    /// in use means deleted) and `extensions` every extension record its
    /// resident attribute list names.
    Reread {
        base: FileRecord,
        extensions: Vec<FileRecord>,
    },
}

impl IndexChange {
    fn record_number(&self) -> u64 {
        match self {
            Self::Create { file, .. }
            | Self::Delete { file }
            | Self::Rename { file, .. }
            | Self::Attributes { file, .. } => file.record,
            Self::Reread { base, .. } => base.record_number,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebuildReason {
    CreateOverLiveRecord,
    UnknownFile,
    NameNotFound,
    /// The file has a separate DOS 8.3 alias whose new value is unknown.
    DosAliasRename,
    /// An attribute change that toggles reparse or directory nature.
    KindChange,
    InvalidName,
    NotABaseRecord,
    NonResidentAttributeList,
    MissingExtension,
    ForeignExtension,
    /// A directory vanished while names still resolve into it.
    OrphanedChildren,
    /// The index could not be finished (u32 limits).
    IndexLimit,
}

#[must_use]
#[derive(Debug)]
pub enum ApplyOutcome {
    Applied(CompactNtfsIndex),
    RebuildRequired(RebuildReason),
}

#[derive(Clone, Debug)]
struct LinkSpec {
    parent: FileReference,
    namespace: u8,
    name: Vec<u16>,
}

#[derive(Clone, Debug)]
struct FileState {
    meta: FileMeta,
    names: Vec<LinkSpec>,
}

impl CompactNtfsIndex {
    /// Applies `changes` in order and returns the updated index, or
    /// [`ApplyOutcome::RebuildRequired`] when any change cannot be applied
    /// safely. `self` is never modified.
    pub fn apply_changes(&self, changes: &[IndexChange]) -> ApplyOutcome {
        match self.try_apply(changes) {
            Ok(outcome) => outcome,
            Err(_) => ApplyOutcome::RebuildRequired(RebuildReason::IndexLimit),
        }
    }

    fn try_apply(&self, changes: &[IndexChange]) -> Result<ApplyOutcome> {
        let touched: HashSet<u64> = changes.iter().map(IndexChange::record_number).collect();

        // Original state of every touched record, names included.
        let mut touched_files: HashMap<u32, u64> = HashMap::new();
        let mut current: HashMap<u64, Option<FileState>> = HashMap::new();
        let mut original: HashMap<u64, (u16, bool)> = HashMap::new();
        for &record in &touched {
            match self.file_by_record_number(record) {
                Some((file_index, meta)) => {
                    touched_files.insert(file_index, record);
                    original.insert(record, (meta.sequence, meta.is_directory()));
                    current.insert(
                        record,
                        Some(FileState {
                            meta: *meta,
                            names: Vec::new(),
                        }),
                    );
                }
                None => {
                    current.insert(record, None);
                }
            }
        }
        if !touched_files.is_empty() {
            for link in &self.links {
                if let Some(record) = touched_files.get(&link.file_index) {
                    if let Some(Some(state)) = current.get_mut(record) {
                        state.names.push(LinkSpec {
                            parent: link.parent(),
                            namespace: link.namespace,
                            name: self.raw_name(*link).to_vec(),
                        });
                    }
                }
            }
        }

        for change in changes {
            if let Err(reason) = apply_one(&mut current, change) {
                return Ok(ApplyOutcome::RebuildRequired(reason));
            }
        }

        // Directories whose identity ended in this batch. Any surviving name
        // that resolved into one of them before would silently become an
        // orphan: on a real volume its own change must be in the batch too.
        let vanished: HashMap<u64, u16> = original
            .iter()
            .filter(|(_, (_, was_directory))| *was_directory)
            .filter(|(record, (sequence, _))| match current.get(record) {
                Some(Some(state)) => state.meta.sequence != *sequence || !state.meta.is_directory(),
                _ => true,
            })
            .map(|(record, (sequence, _))| (*record, *sequence))
            .collect();
        let orphans =
            |parent: FileReference| vanished.get(&parent.record) == Some(&parent.sequence);

        let mut builder = IndexBuilder::default();
        // finish() recounts these two from the links it is given.
        builder.stats = IndexStats {
            suppressed_dos_aliases: 0,
            stale_or_missing_parent_links: 0,
            ..self.stats
        };
        builder.files.reserve(self.files.len());
        builder.links.reserve(self.links.len());
        builder.name_arena.reserve(self.name_arena.len());
        builder.record_to_file = vec![0; self.record_to_file.len()];

        let mut remap = vec![u32::MAX; self.files.len()];
        for (old_index, meta) in self.files.iter().enumerate() {
            if touched.contains(&meta.record) {
                continue;
            }
            let new_index = builder.files.len() as u32;
            remap[old_index] = new_index;
            builder.files.push(*meta);
            builder.record_to_file[meta.record as usize] = new_index + 1;
        }
        for link in &self.links {
            let new_index = remap[link.file_index as usize];
            if new_index == u32::MAX {
                continue;
            }
            if orphans(link.parent()) {
                return Ok(ApplyOutcome::RebuildRequired(
                    RebuildReason::OrphanedChildren,
                ));
            }
            let name = self.raw_name(*link);
            push_link(&mut builder, new_index, link.parent(), link.namespace, name)?;
        }

        let mut records: Vec<u64> = touched.iter().copied().collect();
        records.sort_unstable();
        for record in records {
            let Some(Some(state)) = current.remove(&record) else {
                continue;
            };
            let slot = usize::try_from(record).map_err(|_| {
                crate::NtfsError::new(crate::ErrorKind::LimitExceeded, record, "record slot")
            })?;
            if slot >= builder.record_to_file.len() {
                builder.record_to_file.resize(slot + 1, 0);
            }
            let new_index = u32::try_from(builder.files.len()).map_err(|_| {
                crate::NtfsError::new(crate::ErrorKind::LimitExceeded, record, "file count")
            })?;
            builder.files.push(state.meta);
            builder.record_to_file[slot] = new_index + 1;
            for name in &state.names {
                if orphans(name.parent) {
                    return Ok(ApplyOutcome::RebuildRequired(
                        RebuildReason::OrphanedChildren,
                    ));
                }
                push_link(
                    &mut builder,
                    new_index,
                    name.parent,
                    name.namespace,
                    &name.name,
                )?;
            }
        }
        Ok(ApplyOutcome::Applied(builder.finish()?))
    }
}

fn push_link(
    builder: &mut IndexBuilder,
    file_index: u32,
    parent: FileReference,
    namespace: u8,
    name: &[u16],
) -> Result<()> {
    let name_offset = u32::try_from(builder.name_arena.len()).map_err(|_| {
        crate::NtfsError::new(
            crate::ErrorKind::LimitExceeded,
            0,
            "UTF-16 name arena exceeds u32",
        )
    })?;
    let name_length = u16::try_from(name.len()).map_err(|_| {
        crate::NtfsError::new(crate::ErrorKind::LimitExceeded, 0, "NTFS name exceeds u16")
    })?;
    if builder.links.len() >= u32::MAX as usize {
        return Err(crate::NtfsError::new(
            crate::ErrorKind::LimitExceeded,
            0,
            "more than u32::MAX filename links",
        ));
    }
    builder.name_arena.extend_from_slice(name);
    builder.links.push(NameLink {
        parent_record: parent.record,
        name_offset,
        file_index,
        parent_sequence: parent.sequence,
        name_length,
        namespace,
        flags: 0,
        _reserved: 0,
    });
    Ok(())
}

fn valid_name(name: &[u16]) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_UNITS && !name.contains(&0)
}

fn live_state(
    current: &mut HashMap<u64, Option<FileState>>,
    file: FileReference,
) -> std::result::Result<&mut FileState, RebuildReason> {
    match current.get_mut(&file.record) {
        Some(Some(state)) if state.meta.sequence == file.sequence => Ok(state),
        _ => Err(RebuildReason::UnknownFile),
    }
}

fn apply_one(
    current: &mut HashMap<u64, Option<FileState>>,
    change: &IndexChange,
) -> std::result::Result<(), RebuildReason> {
    match change {
        IndexChange::Create {
            file,
            parent,
            name,
            namespace,
            directory,
            attributes,
            created_ticks,
            modified_ticks,
        } => {
            if matches!(current.get(&file.record), Some(Some(_))) {
                return Err(RebuildReason::CreateOverLiveRecord);
            }
            if !valid_name(name) {
                return Err(RebuildReason::InvalidName);
            }
            // A reparse point's tag (cloud or link) is not in the change
            // metadata, and the directory bit must agree with the record.
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || (attributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !*directory)
            {
                return Err(RebuildReason::KindChange);
            }
            let meta = FileMeta::from_record_parts(
                file.record,
                file.sequence,
                *directory,
                false,
                false,
                StandardInfo {
                    modified_ticks: *modified_ticks,
                    created_ticks: *created_ticks,
                    attributes: *attributes,
                },
                &[],
                &[],
            );
            current.insert(
                file.record,
                Some(FileState {
                    meta,
                    names: vec![LinkSpec {
                        parent: *parent,
                        namespace: namespace_raw(*namespace),
                        name: name.clone(),
                    }],
                }),
            );
        }
        IndexChange::Delete { file } => {
            live_state(current, *file)?;
            current.insert(file.record, None);
        }
        IndexChange::Rename {
            file,
            old_parent,
            old_name,
            new_parent,
            new_name,
        } => {
            if !valid_name(new_name) {
                return Err(RebuildReason::InvalidName);
            }
            let state = live_state(current, *file)?;
            if state
                .names
                .iter()
                .any(|link| link.namespace == DOS_NAMESPACE)
            {
                return Err(RebuildReason::DosAliasRename);
            }
            let link = state
                .names
                .iter_mut()
                .find(|link| link.parent == *old_parent && link.name == *old_name)
                .ok_or(RebuildReason::NameNotFound)?;
            link.parent = *new_parent;
            link.name = new_name.clone();
        }
        IndexChange::Attributes { file, attributes } => {
            let state = live_state(current, *file)?;
            let reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
            let directory_bit = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
            if reparse != state.meta.is_reparse_point()
                || (directory_bit && !state.meta.is_directory())
            {
                return Err(RebuildReason::KindChange);
            }
            state.meta.attributes = attributes & !FILE_ATTRIBUTE_DIRECTORY;
        }
        IndexChange::Reread { base, extensions } => {
            if base.base_reference.is_some() {
                return Err(RebuildReason::NotABaseRecord);
            }
            if !base.in_use {
                current.insert(base.record_number, None);
                return Ok(());
            }
            let state = reread_state(base, extensions)?;
            current.insert(base.record_number, Some(state));
        }
    }
    Ok(())
}

/// Merges a base record with its extension records using the same rules as
/// [`IndexBuilder`]: the first-extent `$DATA` sizes win, names accumulate,
/// reparse and cloud flags combine.
fn reread_state(
    base: &FileRecord,
    extensions: &[FileRecord],
) -> std::result::Result<FileState, RebuildReason> {
    let base_reference = FileReference {
        record: base.record_number,
        sequence: base.sequence,
    };
    let mut expected: HashSet<FileReference> = HashSet::new();
    for list in &base.attribute_lists {
        match list {
            AttributeList::Resident(entries) => expected.extend(
                entries
                    .iter()
                    .map(|entry| entry.record)
                    .filter(|record| *record != base_reference),
            ),
            AttributeList::NonResident { .. } => {
                return Err(RebuildReason::NonResidentAttributeList)
            }
        }
    }
    let mut supplied: HashSet<FileReference> = HashSet::new();
    for extension in extensions {
        if !extension.in_use || extension.base_reference != Some(base_reference) {
            return Err(RebuildReason::ForeignExtension);
        }
        supplied.insert(FileReference {
            record: extension.record_number,
            sequence: extension.sequence,
        });
    }
    if !expected.is_subset(&supplied) {
        return Err(RebuildReason::MissingExtension);
    }

    let mut data = base.data.clone();
    let mut names: Vec<FileName> = base.names.clone();
    let mut reparse_point = base.reparse_point;
    let mut cloud = base.is_cloud_reparse();
    for extension in extensions {
        data.extend(extension.data.iter().cloned());
        names.extend(extension.names.iter().cloned());
        reparse_point |= extension.reparse_point;
        cloud |= extension.is_cloud_reparse();
    }
    for name in &names {
        if name.name.is_empty() || name.name.len() > usize::from(u16::MAX) {
            return Err(RebuildReason::InvalidName);
        }
    }
    let meta = FileMeta::from_record_parts(
        base.record_number,
        base.sequence,
        base.directory,
        reparse_point,
        cloud,
        StandardInfo::of(base),
        &data,
        &names,
    );
    Ok(FileState {
        meta,
        names: names
            .iter()
            .map(|name| LinkSpec {
                parent: name.parent,
                namespace: namespace_raw(name.namespace),
                name: name.name.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::{name, record, reference};
    use crate::{AttributeListEntry, SearchQuery, TraversalOptions};

    fn raw(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    /// root(5) ── dir(10) ── a.txt(20)
    ///         └─ b.txt(21)
    fn base_index() -> CompactNtfsIndex {
        let root = reference(5, 1);
        let dir = reference(10, 1);
        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        builder
            .push(record(10, 1, true, false, vec![name(root, "dir")], 0))
            .unwrap();
        builder
            .push(record(20, 1, false, false, vec![name(dir, "a.txt")], 100))
            .unwrap();
        builder
            .push(record(21, 1, false, false, vec![name(root, "b.txt")], 200))
            .unwrap();
        builder.finish().unwrap()
    }

    fn applied(outcome: ApplyOutcome) -> CompactNtfsIndex {
        match outcome {
            ApplyOutcome::Applied(index) => index,
            ApplyOutcome::RebuildRequired(reason) => panic!("unexpected rebuild: {reason:?}"),
        }
    }

    fn rebuild(outcome: ApplyOutcome) -> RebuildReason {
        match outcome {
            ApplyOutcome::Applied(_) => panic!("expected rebuild"),
            ApplyOutcome::RebuildRequired(reason) => reason,
        }
    }

    fn all_paths(index: &CompactNtfsIndex) -> Vec<String> {
        let mut rows = Vec::new();
        index
            .search_subtree(
                reference(5, 1),
                &SearchQuery {
                    include_hidden: true,
                    ..SearchQuery::default()
                },
                || false,
                |batch| rows.extend(batch),
            )
            .unwrap();
        let mut paths: Vec<String> = rows
            .iter()
            .map(|row| String::from_utf16_lossy(&row.relative_path))
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn create_adds_a_name_under_its_parent() {
        let index = applied(base_index().apply_changes(&[IndexChange::Create {
            file: reference(30, 4),
            parent: reference(10, 1),
            name: raw("new.txt"),
            namespace: NameNamespace::Win32,
            directory: false,
            attributes: 0x20,
            created_ticks: 9,
            modified_ticks: 10,
        }]));
        assert_eq!(
            all_paths(&index),
            vec!["b.txt", "dir", "dir\\a.txt", "dir\\new.txt"]
        );
        let (_, meta) = index.file(reference(30, 4)).unwrap();
        assert_eq!(meta.created_ticks, 9);
        assert_eq!(meta.logical_bytes, 0);
    }

    #[test]
    fn create_over_a_live_record_requires_rebuild() {
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Create {
            file: reference(21, 2),
            parent: reference(5, 1),
            name: raw("x"),
            namespace: NameNamespace::Win32,
            directory: false,
            attributes: 0,
            created_ticks: 0,
            modified_ticks: 0,
        }]));
        assert_eq!(reason, RebuildReason::CreateOverLiveRecord);
    }

    #[test]
    fn create_of_a_reparse_point_requires_reread() {
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Create {
            file: reference(30, 1),
            parent: reference(5, 1),
            name: raw("link"),
            namespace: NameNamespace::Win32,
            directory: false,
            attributes: FILE_ATTRIBUTE_REPARSE_POINT,
            created_ticks: 0,
            modified_ticks: 0,
        }]));
        assert_eq!(reason, RebuildReason::KindChange);
    }

    #[test]
    fn delete_removes_every_name_and_checks_sequence() {
        let index = applied(base_index().apply_changes(&[IndexChange::Delete {
            file: reference(21, 1),
        }]));
        assert_eq!(all_paths(&index), vec!["dir", "dir\\a.txt"]);
        assert!(index.file_by_record_number(21).is_none());

        let reason = rebuild(base_index().apply_changes(&[IndexChange::Delete {
            file: reference(21, 9),
        }]));
        assert_eq!(reason, RebuildReason::UnknownFile);
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Delete {
            file: reference(99, 1),
        }]));
        assert_eq!(reason, RebuildReason::UnknownFile);
    }

    #[test]
    fn deleting_a_directory_that_still_has_names_requires_rebuild() {
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Delete {
            file: reference(10, 1),
        }]));
        assert_eq!(reason, RebuildReason::OrphanedChildren);

        // Children deleted in the same batch, in either order: fine.
        let index = applied(base_index().apply_changes(&[
            IndexChange::Delete {
                file: reference(10, 1),
            },
            IndexChange::Delete {
                file: reference(20, 1),
            },
        ]));
        assert_eq!(all_paths(&index), vec!["b.txt"]);
    }

    #[test]
    fn rename_and_move_replace_exactly_one_name() {
        let index = applied(base_index().apply_changes(&[IndexChange::Rename {
            file: reference(21, 1),
            old_parent: reference(5, 1),
            old_name: raw("b.txt"),
            new_parent: reference(10, 1),
            new_name: raw("moved.txt"),
        }]));
        assert_eq!(
            all_paths(&index),
            vec!["dir", "dir\\a.txt", "dir\\moved.txt"]
        );

        let reason = rebuild(base_index().apply_changes(&[IndexChange::Rename {
            file: reference(21, 1),
            old_parent: reference(5, 1),
            old_name: raw("B.TXT"),
            new_parent: reference(5, 1),
            new_name: raw("c.txt"),
        }]));
        assert_eq!(reason, RebuildReason::NameNotFound);
    }

    #[test]
    fn rename_of_a_file_with_a_dos_alias_requires_rebuild() {
        let root = reference(5, 1);
        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        let mut long = name(root, "Long name.txt");
        long.namespace = NameNamespace::Win32;
        let mut short = name(root, "LONGNA~1.TXT");
        short.namespace = NameNamespace::Dos;
        builder
            .push(record(20, 1, false, false, vec![long, short], 1))
            .unwrap();
        let index = builder.finish().unwrap();
        let reason = rebuild(index.apply_changes(&[IndexChange::Rename {
            file: reference(20, 1),
            old_parent: root,
            old_name: raw("Long name.txt"),
            new_parent: root,
            new_name: raw("Other.txt"),
        }]));
        assert_eq!(reason, RebuildReason::DosAliasRename);
    }

    #[test]
    fn attribute_change_updates_hidden_and_rejects_kind_changes() {
        let index = applied(base_index().apply_changes(&[IndexChange::Attributes {
            file: reference(21, 1),
            attributes: 0x2,
        }]));
        assert_eq!(all_paths(&index).len(), 3);
        let mut rows = Vec::new();
        index
            .search_subtree(
                reference(5, 1),
                &SearchQuery::default(),
                || false,
                |b| rows.extend(b),
            )
            .unwrap();
        assert!(!rows.iter().any(|row| row.display_name() == "b.txt"));

        let reason = rebuild(base_index().apply_changes(&[IndexChange::Attributes {
            file: reference(21, 1),
            attributes: FILE_ATTRIBUTE_REPARSE_POINT,
        }]));
        assert_eq!(reason, RebuildReason::KindChange);
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Attributes {
            file: reference(21, 1),
            attributes: FILE_ATTRIBUTE_DIRECTORY,
        }]));
        assert_eq!(reason, RebuildReason::KindChange);
    }

    #[test]
    fn reread_replaces_sizes_names_and_hard_links() {
        let root = reference(5, 1);
        let dir = reference(10, 1);
        // a.txt grew, gained a second hard link and a new modification time.
        let mut fresh = record(
            20,
            1,
            false,
            false,
            vec![name(dir, "a.txt"), name(root, "a-link.txt")],
            5000,
        );
        fresh.modified_ticks = Some(777);
        let index = applied(base_index().apply_changes(&[IndexChange::Reread {
            base: fresh,
            extensions: Vec::new(),
        }]));
        assert_eq!(
            all_paths(&index),
            vec!["a-link.txt", "b.txt", "dir", "dir\\a.txt"]
        );
        let (_, meta) = index.file(reference(20, 1)).unwrap();
        assert_eq!(meta.logical_bytes, 5000);
        assert_eq!(meta.modified_ticks, 777);
        // Disk Usage still charges the hard-linked file once.
        let summary = index
            .walk_subtree(root, TraversalOptions::default(), || false, |_| {})
            .unwrap();
        assert_eq!(summary.logical_bytes, 5200);
    }

    #[test]
    fn reread_not_in_use_deletes_and_reused_record_replaces() {
        let mut gone = record(21, 1, false, false, vec![], 0);
        gone.in_use = false;
        let index = applied(base_index().apply_changes(&[IndexChange::Reread {
            base: gone,
            extensions: Vec::new(),
        }]));
        assert_eq!(all_paths(&index), vec!["dir", "dir\\a.txt"]);

        // Record 21 reused with a new sequence for a new file.
        let reused = record(21, 2, false, false, vec![name(reference(5, 1), "z.txt")], 1);
        let index = applied(base_index().apply_changes(&[IndexChange::Reread {
            base: reused,
            extensions: Vec::new(),
        }]));
        assert_eq!(all_paths(&index), vec!["dir", "dir\\a.txt", "z.txt"]);
        assert!(index.file(reference(21, 1)).is_none());
        assert!(index.file(reference(21, 2)).is_some());
    }

    #[test]
    fn reread_of_a_reused_directory_record_with_live_children_requires_rebuild() {
        let reused = record(10, 2, true, false, vec![name(reference(5, 1), "dir2")], 0);
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Reread {
            base: reused,
            extensions: Vec::new(),
        }]));
        assert_eq!(reason, RebuildReason::OrphanedChildren);
    }

    #[test]
    fn reread_merges_extensions_and_rejects_missing_or_foreign_ones() {
        let root = reference(5, 1);
        let mut base = record(40, 3, false, false, vec![name(root, "big.bin")], 0);
        base.data.clear();
        base.attribute_lists = vec![AttributeList::Resident(vec![AttributeListEntry {
            attribute_type: 0x80,
            lowest_vcn: 0,
            record: reference(41, 1),
            attribute_id: 0,
            name: Vec::new(),
        }])];
        let mut extension = record(41, 1, false, false, vec![name(root, "big-link.bin")], 9000);
        extension.base_reference = Some(reference(40, 3));

        let index = applied(base_index().apply_changes(&[IndexChange::Reread {
            base: base.clone(),
            extensions: vec![extension.clone()],
        }]));
        let (_, meta) = index.file(reference(40, 3)).unwrap();
        assert_eq!(meta.logical_bytes, 9000);
        assert!(all_paths(&index).contains(&"big-link.bin".to_string()));

        let reason = rebuild(base_index().apply_changes(&[IndexChange::Reread {
            base: base.clone(),
            extensions: Vec::new(),
        }]));
        assert_eq!(reason, RebuildReason::MissingExtension);

        let mut foreign = extension.clone();
        foreign.base_reference = Some(reference(40, 2));
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Reread {
            base: base.clone(),
            extensions: vec![foreign],
        }]));
        assert_eq!(reason, RebuildReason::ForeignExtension);

        let mut not_base = base.clone();
        not_base.base_reference = Some(reference(1, 1));
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Reread {
            base: not_base,
            extensions: Vec::new(),
        }]));
        assert_eq!(reason, RebuildReason::NotABaseRecord);

        let mut non_resident = base;
        non_resident.attribute_lists = vec![AttributeList::NonResident {
            lowest_vcn: 0,
            highest_vcn: 0,
            logical_bytes: 0,
            runs: Vec::new(),
        }];
        let reason = rebuild(base_index().apply_changes(&[IndexChange::Reread {
            base: non_resident,
            extensions: vec![extension],
        }]));
        assert_eq!(reason, RebuildReason::NonResidentAttributeList);
    }

    #[test]
    fn applied_index_equals_a_fresh_build_of_the_same_records() {
        let root = reference(5, 1);
        let dir = reference(10, 1);
        let changed = record(20, 1, false, false, vec![name(root, "a.txt")], 42);
        let index = applied(base_index().apply_changes(&[IndexChange::Reread {
            base: changed.clone(),
            extensions: Vec::new(),
        }]));

        let mut builder = IndexBuilder::default();
        builder.push(record(5, 1, true, false, vec![], 0)).unwrap();
        builder
            .push(record(10, 1, true, false, vec![name(root, "dir")], 0))
            .unwrap();
        builder.push(changed).unwrap();
        builder
            .push(record(21, 1, false, false, vec![name(root, "b.txt")], 200))
            .unwrap();
        let fresh = builder.finish().unwrap();
        let _ = dir;
        assert_eq!(all_paths(&index), all_paths(&fresh));
        let walk = |index: &CompactNtfsIndex| {
            let mut rows = Vec::new();
            index
                .walk_subtree(
                    root,
                    TraversalOptions::default(),
                    || false,
                    |b| rows.extend(b),
                )
                .unwrap();
            rows.into_iter()
                .map(|row| (row.display_name, row.logical_bytes, row.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(walk(&index), walk(&fresh));
    }

    #[test]
    fn original_index_is_untouched_when_rebuild_is_required() {
        let index = base_index();
        let before = all_paths(&index);
        let _ = rebuild(index.apply_changes(&[
            IndexChange::Delete {
                file: reference(21, 1),
            },
            IndexChange::Delete {
                file: reference(99, 1),
            },
        ]));
        assert_eq!(all_paths(&index), before);
    }
}
