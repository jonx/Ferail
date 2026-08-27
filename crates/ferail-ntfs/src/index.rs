use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use crate::{
    AttributeList, DataAttribute, ErrorKind, FileName, FileRecord, FileReference, NameNamespace,
    NtfsError, Result,
};

const FILE_DIRECTORY: u16 = 0x0001;
const FILE_REPARSE: u16 = 0x0002;
const FILE_CLOUD_REPARSE: u16 = 0x0004;
const MAX_BATCH_ROWS: usize = 256;

/// Packed base-record metadata. The single UTF-16 arena and adjacency arrays
/// live on [`CompactNtfsIndex`], never as a `PathBuf` per entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileMeta {
    pub record: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub modified_ticks: u64,
    pub sequence: u16,
    flags: u16,
    _reserved: u32,
}

impl FileMeta {
    pub fn reference(self) -> FileReference {
        FileReference {
            record: self.record,
            sequence: self.sequence,
        }
    }

    pub fn is_directory(self) -> bool {
        self.flags & FILE_DIRECTORY != 0
    }

    pub fn is_reparse_point(self) -> bool {
        self.flags & FILE_REPARSE != 0
    }

    pub fn is_cloud_reparse(self) -> bool {
        self.flags & FILE_CLOUD_REPARSE != 0
    }
}

/// One real filename link. This stays at the 24-byte structural gate on 64-
/// bit targets; raw UTF-16 code units live only once in `name_arena`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NameLink {
    pub parent_record: u64,
    pub name_offset: u32,
    pub file_index: u32,
    pub parent_sequence: u16,
    pub name_length: u16,
    pub namespace: u8,
    flags: u8,
    _reserved: u16,
}

impl NameLink {
    pub fn parent(self) -> FileReference {
        FileReference {
            record: self.parent_record,
            sequence: self.parent_sequence,
        }
    }

    fn is_dos_only(self) -> bool {
        self.namespace == 2
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ChildRange {
    start: u32,
    length: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexStats {
    pub records_seen: u64,
    pub base_records: u64,
    pub extension_records: u64,
    pub skipped_deleted: u64,
    pub skipped_unlisted_extensions: u64,
    pub stale_or_missing_parent_links: u64,
    pub suppressed_dos_aliases: u64,
}

#[derive(Debug)]
pub struct CompactNtfsIndex {
    files: Vec<FileMeta>,
    links: Vec<NameLink>,
    name_arena: Vec<u16>,
    record_to_file: Vec<u32>,
    children: Vec<u32>,
    child_ranges: Vec<ChildRange>,
    stats: IndexStats,
}

impl CompactNtfsIndex {
    pub fn files(&self) -> &[FileMeta] {
        &self.files
    }

    pub fn links(&self) -> &[NameLink] {
        &self.links
    }

    pub fn name_arena(&self) -> &[u16] {
        &self.name_arena
    }

    pub const fn stats(&self) -> IndexStats {
        self.stats
    }

    pub fn file(&self, reference: FileReference) -> Option<(u32, &FileMeta)> {
        let record = usize::try_from(reference.record).ok()?;
        let encoded = *self.record_to_file.get(record)?;
        let index = encoded.checked_sub(1)?;
        let meta = self.files.get(index as usize)?;
        (meta.sequence == reference.sequence).then_some((index, meta))
    }

    pub fn file_by_record_number(&self, record: u64) -> Option<(u32, &FileMeta)> {
        let slot = usize::try_from(record).ok()?;
        let index = self.record_to_file.get(slot)?.checked_sub(1)?;
        Some((index, self.files.get(index as usize)?))
    }

    pub fn raw_name(&self, link: NameLink) -> &[u16] {
        let start = link.name_offset as usize;
        let end = start.saturating_add(link.name_length as usize);
        self.name_arena.get(start..end).unwrap_or_default()
    }

    pub fn walk_subtree(
        &self,
        root: FileReference,
        options: TraversalOptions,
        mut is_cancelled: impl FnMut() -> bool,
        mut on_batch: impl FnMut(Vec<NeutralRow>),
    ) -> Result<TraversalSummary> {
        let (root_index, root_meta) = self.file(root).ok_or_else(|| {
            NtfsError::new(
                ErrorKind::InvalidAttribute,
                root.record,
                "root record not found",
            )
        })?;
        if !root_meta.is_directory() {
            return Err(NtfsError::new(
                ErrorKind::InvalidAttribute,
                root.record,
                "subtree root is not a directory",
            ));
        }

        let mut summary = TraversalSummary::default();
        let mut charged = vec![false; self.files.len()];
        let mut ancestors = vec![false; self.files.len()];
        ancestors[root_index as usize] = true;
        let mut next_id = options.first_child_id;
        let mut stack = Vec::new();
        for child in self.children_of(root_index).iter().rev() {
            stack.push(Frame::Visit {
                link: *child,
                parent_id: options.root_id,
            });
        }
        let batch_rows = options.batch_rows.clamp(1, MAX_BATCH_ROWS);
        let mut batch = Vec::with_capacity(batch_rows);

        while let Some(frame) = stack.pop() {
            if is_cancelled() {
                return Err(NtfsError::new(ErrorKind::Cancelled, 0, "subtree traversal"));
            }
            match frame {
                Frame::ExitDirectory(file_index) => ancestors[file_index as usize] = false,
                Frame::Visit { link, parent_id } => {
                    let link = self.links[link as usize];
                    let file = self.files[link.file_index as usize];
                    let id = next_id;
                    next_id = next_id.checked_add(1).ok_or_else(|| {
                        NtfsError::new(ErrorKind::Overflow, id, "scan-local node id overflow")
                    })?;
                    let raw_name = self.raw_name(link);
                    let opaque_package = file.is_directory()
                        && !options.descend_packages
                        && is_package_name(raw_name);
                    // Raw traversal never resolves the target of a reparse
                    // point: it follows only FILE_NAME parent references
                    // already present in this MFT. Cloud directories have
                    // real indexed children; junction targets do not.
                    let has_indexed_children = !self.children_of(link.file_index).is_empty();
                    let kind = if opaque_package {
                        NeutralNodeKind::OpaquePackage
                    } else if file.is_directory()
                        && file.is_reparse_point()
                        && !has_indexed_children
                    {
                        NeutralNodeKind::ReparseDirectory
                    } else if file.is_directory() {
                        NeutralNodeKind::Directory
                    } else {
                        NeutralNodeKind::File
                    };

                    let (logical_bytes, allocated_bytes) = match kind {
                        NeutralNodeKind::File => charge_file(link.file_index, file, &mut charged),
                        NeutralNodeKind::OpaquePackage => self.charge_hidden_subtree(
                            link.file_index,
                            &mut charged,
                            &mut summary.cycles_skipped,
                        ),
                        NeutralNodeKind::Directory | NeutralNodeKind::ReparseDirectory => (0, 0),
                    };
                    batch.push(NeutralRow {
                        id,
                        parent_id,
                        file_record: file.reference(),
                        kind,
                        raw_name: raw_name.to_vec(),
                        display_name: String::from_utf16_lossy(raw_name),
                        logical_bytes,
                        allocated_bytes,
                        modified_ticks: file.modified_ticks,
                    });
                    summary.rows = summary.rows.saturating_add(1);
                    summary.logical_bytes = summary.logical_bytes.saturating_add(logical_bytes);
                    summary.allocated_bytes =
                        summary.allocated_bytes.saturating_add(allocated_bytes);

                    if kind == NeutralNodeKind::Directory {
                        if ancestors[link.file_index as usize] {
                            summary.cycles_skipped = summary.cycles_skipped.saturating_add(1);
                        } else {
                            ancestors[link.file_index as usize] = true;
                            stack.push(Frame::ExitDirectory(link.file_index));
                            for child in self.children_of(link.file_index).iter().rev() {
                                stack.push(Frame::Visit {
                                    link: *child,
                                    parent_id: id,
                                });
                            }
                        }
                    }
                    if batch.len() == batch_rows {
                        on_batch(std::mem::take(&mut batch));
                        batch = Vec::with_capacity(batch_rows);
                    }
                }
            }
        }
        if !batch.is_empty() {
            on_batch(batch);
        }
        Ok(summary)
    }

    fn children_of(&self, file_index: u32) -> &[u32] {
        let Some(range) = self.child_ranges.get(file_index as usize) else {
            return &[];
        };
        let start = range.start as usize;
        let end = start.saturating_add(range.length as usize);
        self.children.get(start..end).unwrap_or_default()
    }

    fn charge_hidden_subtree(
        &self,
        root: u32,
        charged: &mut [bool],
        cycles: &mut u64,
    ) -> (u64, u64) {
        let mut logical = 0u64;
        let mut allocated = 0u64;
        let mut stack = vec![(root, false)];
        let mut ancestors = vec![false; self.files.len()];
        while let Some((file_index, exiting)) = stack.pop() {
            if exiting {
                ancestors[file_index as usize] = false;
                continue;
            }
            let file = self.files[file_index as usize];
            if !file.is_directory() {
                let charged_size = charge_file(file_index, file, charged);
                logical = logical.saturating_add(charged_size.0);
                allocated = allocated.saturating_add(charged_size.1);
                continue;
            }
            if ancestors[file_index as usize] {
                *cycles = cycles.saturating_add(1);
                continue;
            }
            ancestors[file_index as usize] = true;
            stack.push((file_index, true));
            for link in self.children_of(file_index).iter().rev() {
                stack.push((self.links[*link as usize].file_index, false));
            }
        }
        (logical, allocated)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Frame {
    Visit { link: u32, parent_id: u64 },
    ExitDirectory(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeutralNodeKind {
    File,
    Directory,
    ReparseDirectory,
    OpaquePackage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NeutralRow {
    pub id: u64,
    pub parent_id: u64,
    pub file_record: FileReference,
    pub kind: NeutralNodeKind,
    pub raw_name: Vec<u16>,
    pub display_name: String,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub modified_ticks: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraversalOptions {
    pub root_id: u64,
    pub first_child_id: u64,
    pub batch_rows: usize,
    pub descend_packages: bool,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self {
            root_id: 1,
            first_child_id: 2,
            batch_rows: MAX_BATCH_ROWS,
            descend_packages: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TraversalSummary {
    pub rows: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub cycles_skipped: u64,
}

#[derive(Default)]
pub struct IndexBuilder {
    files: Vec<FileMeta>,
    links: Vec<NameLink>,
    name_arena: Vec<u16>,
    record_to_file: Vec<u32>,
    expected_extensions: HashMap<FileReference, u32>,
    pending_extensions: HashMap<FileReference, PendingExtension>,
    pending_by_base: HashMap<FileReference, Vec<FileReference>>,
    stats: IndexStats,
}

impl IndexBuilder {
    pub fn push(&mut self, record: FileRecord) -> Result<()> {
        self.stats.records_seen = self.stats.records_seen.saturating_add(1);
        if !record.in_use {
            self.stats.skipped_deleted = self.stats.skipped_deleted.saturating_add(1);
            return Ok(());
        }
        if let Some(base) = record.base_reference {
            self.stats.extension_records = self.stats.extension_records.saturating_add(1);
            return self.push_extension(base, record);
        }
        self.push_base(record)
    }

    pub fn finish(mut self) -> Result<CompactNtfsIndex> {
        self.stats.skipped_unlisted_extensions = self
            .stats
            .skipped_unlisted_extensions
            .saturating_add(self.pending_extensions.len() as u64);

        // DOS aliases can be suppressed in linear time. The old code sorted
        // every link on the volume just to group `(file, parent)` pairs; a
        // real-name key set expresses the same rule without an O(n log n)
        // whole-volume sort.
        let real_link_keys: HashSet<(u32, FileReference)> = self
            .links
            .iter()
            .filter(|link| !link.is_dos_only())
            .map(|link| (link.file_index, link.parent()))
            .collect();
        let mut suppressed_dos = vec![false; self.links.len()];
        for (index, link) in self.links.iter().copied().enumerate() {
            if link.is_dos_only() && real_link_keys.contains(&(link.file_index, link.parent())) {
                suppressed_dos[index] = true;
                self.stats.suppressed_dos_aliases =
                    self.stats.suppressed_dos_aliases.saturating_add(1);
            }
        }
        drop(real_link_keys);

        // Counting-sort links by parent in O(files + links), then sort only
        // each directory's own child slice by name. This preserves the exact
        // deterministic traversal order without globally sorting millions of
        // unrelated paths together.
        let mut link_parents = vec![u32::MAX; self.links.len()];
        let mut child_counts = vec![0u32; self.files.len()];
        for (link_index, link) in self.links.iter().copied().enumerate() {
            if suppressed_dos[link_index] {
                continue;
            }
            let Some((parent_index, _)) = self.file(link.parent()) else {
                self.stats.stale_or_missing_parent_links =
                    self.stats.stale_or_missing_parent_links.saturating_add(1);
                continue;
            };
            link_parents[link_index] = parent_index;
            child_counts[parent_index as usize] = child_counts[parent_index as usize]
                .checked_add(1)
                .ok_or_else(|| {
                    NtfsError::new(
                        ErrorKind::LimitExceeded,
                        parent_index as u64,
                        "directory has more than u32::MAX links",
                    )
                })?;
        }

        let mut child_ranges = Vec::with_capacity(self.files.len());
        let mut valid_count = 0u32;
        for count in child_counts {
            child_ranges.push(ChildRange {
                start: valid_count,
                length: count,
            });
            valid_count = valid_count.checked_add(count).ok_or_else(|| {
                NtfsError::new(
                    ErrorKind::LimitExceeded,
                    u64::from(valid_count),
                    "more than u32::MAX valid filename links",
                )
            })?;
        }
        let mut children = vec![0u32; valid_count as usize];
        let mut cursors: Vec<u32> = child_ranges.iter().map(|range| range.start).collect();
        for (link_index, parent) in link_parents.into_iter().enumerate() {
            if parent == u32::MAX {
                continue;
            }
            let cursor = &mut cursors[parent as usize];
            children[*cursor as usize] = u32::try_from(link_index).map_err(|_| {
                NtfsError::new(
                    ErrorKind::LimitExceeded,
                    link_index as u64,
                    "more than u32::MAX filename links",
                )
            })?;
            *cursor += 1;
        }
        for range in &child_ranges {
            let start = range.start as usize;
            let end = start + range.length as usize;
            children[start..end].sort_unstable_by(|left_link, right_link| {
                let left = self.links[*left_link as usize];
                let right = self.links[*right_link as usize];
                self.raw_name_for_sort(left)
                    .cmp(self.raw_name_for_sort(right))
                    .then_with(|| left_link.cmp(right_link))
            });
        }
        Ok(CompactNtfsIndex {
            files: self.files,
            links: self.links,
            name_arena: self.name_arena,
            record_to_file: self.record_to_file,
            children,
            child_ranges,
            stats: self.stats,
        })
    }

    fn push_base(&mut self, record: FileRecord) -> Result<()> {
        let record_slot = usize::try_from(record.record_number).map_err(|_| {
            NtfsError::new(
                ErrorKind::LimitExceeded,
                record.record_number,
                "record number does not fit address space",
            )
        })?;
        if record_slot >= self.record_to_file.len() {
            self.record_to_file.resize(
                record_slot.checked_add(1).ok_or_else(|| {
                    NtfsError::new(
                        ErrorKind::Overflow,
                        record.record_number,
                        "record map size overflow",
                    )
                })?,
                0,
            );
        }
        if self.record_to_file[record_slot] != 0 {
            return Err(NtfsError::new(
                ErrorKind::InvalidAttribute,
                record.record_number,
                "duplicate live base record",
            ));
        }
        let file_index = u32::try_from(self.files.len()).map_err(|_| {
            NtfsError::new(
                ErrorKind::LimitExceeded,
                record.record_number,
                "more than u32::MAX base records",
            )
        })?;
        let (logical_bytes, allocated_bytes) = primary_sizes(&record.data, &record.names);
        self.files.push(FileMeta {
            record: record.record_number,
            logical_bytes,
            allocated_bytes,
            modified_ticks: record.modified_ticks.unwrap_or(0),
            sequence: record.sequence,
            flags: (u16::from(record.directory) * FILE_DIRECTORY)
                | (u16::from(record.reparse_point) * FILE_REPARSE)
                | (u16::from(record.is_cloud_reparse()) * FILE_CLOUD_REPARSE),
            _reserved: 0,
        });
        self.record_to_file[record_slot] = file_index.saturating_add(1);
        self.append_names(file_index, &record.names)?;
        self.stats.base_records = self.stats.base_records.saturating_add(1);

        let mut expected_for_file = Vec::new();
        for list in &record.attribute_lists {
            if let AttributeList::Resident(entries) = list {
                for entry in entries {
                    if entry.record.record != record.record_number
                        || entry.record.sequence != record.sequence
                    {
                        self.expected_extensions.insert(entry.record, file_index);
                        expected_for_file.push(entry.record);
                    }
                }
            }
        }
        for reference in expected_for_file {
            if let Some(pending) = self.pending_extensions.remove(&reference) {
                self.merge_extension(file_index, pending)?;
            }
        }
        let base_reference = FileReference {
            record: record.record_number,
            sequence: record.sequence,
        };
        if let Some(references) = self.pending_by_base.remove(&base_reference) {
            for reference in references {
                if let Some(pending) = self.pending_extensions.remove(&reference) {
                    self.merge_extension(file_index, pending)?;
                }
            }
        }
        Ok(())
    }

    fn push_extension(&mut self, base: FileReference, record: FileRecord) -> Result<()> {
        let own_reference = FileReference {
            record: record.record_number,
            sequence: record.sequence,
        };
        let cloud_reparse = record.is_cloud_reparse();
        let pending = PendingExtension {
            base,
            data: record.data,
            names: record.names,
            reparse_point: record.reparse_point,
            cloud_reparse,
        };
        if let Some((owner, _)) = self.file(base) {
            return self.merge_extension(owner, pending);
        }
        if let Some(owner) = self.expected_extensions.get(&own_reference).copied() {
            let meta = self.files[owner as usize];
            if meta.reference() == base {
                return self.merge_extension(owner, pending);
            }
        }
        self.pending_by_base
            .entry(base)
            .or_default()
            .push(own_reference);
        self.pending_extensions.insert(own_reference, pending);
        Ok(())
    }

    fn merge_extension(&mut self, file_index: u32, extension: PendingExtension) -> Result<()> {
        if self.files[file_index as usize].reference() != extension.base {
            self.stats.skipped_unlisted_extensions =
                self.stats.skipped_unlisted_extensions.saturating_add(1);
            return Ok(());
        }
        if let Some(primary) = extension.data.iter().find(|data| data.lowest_vcn == 0) {
            let meta = &mut self.files[file_index as usize];
            meta.logical_bytes = primary.logical_bytes;
            meta.allocated_bytes = primary.allocated_bytes;
        }
        let meta = &mut self.files[file_index as usize];
        if extension.reparse_point {
            meta.flags |= FILE_REPARSE;
        }
        if extension.cloud_reparse {
            meta.flags |= FILE_CLOUD_REPARSE;
        }
        self.append_names(file_index, &extension.names)
    }

    fn append_names(&mut self, file_index: u32, names: &[FileName]) -> Result<()> {
        for name in names {
            if self.links.len() >= u32::MAX as usize {
                return Err(NtfsError::new(
                    ErrorKind::LimitExceeded,
                    self.links.len() as u64,
                    "more than u32::MAX filename links",
                ));
            }
            let name_offset = u32::try_from(self.name_arena.len()).map_err(|_| {
                NtfsError::new(
                    ErrorKind::LimitExceeded,
                    self.name_arena.len() as u64,
                    "UTF-16 name arena exceeds u32",
                )
            })?;
            let name_length = u16::try_from(name.name.len()).map_err(|_| {
                NtfsError::new(
                    ErrorKind::LimitExceeded,
                    name.name.len() as u64,
                    "NTFS name exceeds u16",
                )
            })?;
            self.name_arena.extend_from_slice(&name.name);
            self.links.push(NameLink {
                parent_record: name.parent.record,
                name_offset,
                file_index,
                parent_sequence: name.parent.sequence,
                name_length,
                namespace: namespace_raw(name.namespace),
                flags: 0,
                _reserved: 0,
            });
        }
        Ok(())
    }

    fn file(&self, reference: FileReference) -> Option<(u32, &FileMeta)> {
        let slot = usize::try_from(reference.record).ok()?;
        let index = self.record_to_file.get(slot)?.checked_sub(1)?;
        let meta = self.files.get(index as usize)?;
        (meta.sequence == reference.sequence).then_some((index, meta))
    }

    fn raw_name_for_sort(&self, link: NameLink) -> &[u16] {
        let start = link.name_offset as usize;
        let end = start.saturating_add(link.name_length as usize);
        self.name_arena.get(start..end).unwrap_or_default()
    }
}

struct PendingExtension {
    base: FileReference,
    data: Vec<DataAttribute>,
    names: Vec<FileName>,
    reparse_point: bool,
    cloud_reparse: bool,
}

fn primary_sizes(data: &[DataAttribute], names: &[FileName]) -> (u64, u64) {
    data.iter()
        .find(|attribute| attribute.lowest_vcn == 0)
        .map(|attribute| (attribute.logical_bytes, attribute.allocated_bytes))
        .or_else(|| {
            names
                .iter()
                .max_by_key(|name| name.logical_bytes)
                .map(|name| (name.logical_bytes, name.allocated_bytes))
        })
        .unwrap_or_default()
}

fn namespace_raw(namespace: NameNamespace) -> u8 {
    match namespace {
        NameNamespace::Posix => 0,
        NameNamespace::Win32 => 1,
        NameNamespace::Dos => 2,
        NameNamespace::Win32AndDos => 3,
        NameNamespace::Other(value) => value,
    }
}

fn charge_file(file_index: u32, file: FileMeta, charged: &mut [bool]) -> (u64, u64) {
    let slot = &mut charged[file_index as usize];
    if *slot {
        (0, 0)
    } else {
        *slot = true;
        (file.logical_bytes, file.allocated_bytes)
    }
}

fn is_package_name(name: &[u16]) -> bool {
    const EXTENSIONS: [&str; 6] = ["app", "bundle", "framework", "plugin", "kext", "xcodeproj"];
    let Some(dot) = name.iter().rposition(|unit| *unit == b'.' as u16) else {
        return false;
    };
    let extension = &name[dot + 1..];
    EXTENSIONS.iter().any(|candidate| {
        extension.len() == candidate.len()
            && extension
                .iter()
                .zip(candidate.bytes())
                .all(|(left, right)| {
                    u8::try_from(*left).is_ok_and(|left| left.eq_ignore_ascii_case(&right))
                })
    })
}

const _: () = {
    assert!(size_of::<FileMeta>() <= 64);
    assert!(size_of::<NameLink>() <= 24);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(record: u64, sequence: u16) -> FileReference {
        FileReference { record, sequence }
    }

    fn name(parent: FileReference, value: &str) -> FileName {
        FileName {
            parent,
            namespace: NameNamespace::Win32,
            name: value.encode_utf16().collect(),
            logical_bytes: 0,
            allocated_bytes: 0,
            file_attributes: 0,
        }
    }

    fn record(
        number: u64,
        sequence: u16,
        directory: bool,
        reparse: bool,
        names: Vec<FileName>,
        size: u64,
    ) -> FileRecord {
        FileRecord {
            record_number: number,
            sequence,
            hard_link_count: names.len() as u16,
            in_use: true,
            directory,
            reparse_point: reparse,
            reparse_tag: reparse.then_some(0xa000_0003),
            base_reference: None,
            modified_ticks: Some(number * 10),
            names,
            data: (!directory)
                .then_some(DataAttribute {
                    lowest_vcn: 0,
                    highest_vcn: 0,
                    logical_bytes: size,
                    allocated_bytes: size.saturating_add(4095) / 4096 * 4096,
                    initialized_bytes: size,
                    resident: false,
                    sparse: false,
                    compressed: false,
                    runs: Vec::new(),
                })
                .into_iter()
                .collect(),
            attribute_lists: Vec::new(),
            named_data_attributes: 0,
        }
    }

    fn fixture_index() -> CompactNtfsIndex {
        let root = reference(5, 1);
        let folder = reference(10, 2);
        let package = reference(60, 1);
        let mut builder = IndexBuilder::default();
        builder
            .push(record(5, 1, true, false, Vec::new(), 0))
            .unwrap();
        builder
            .push(record(10, 2, true, false, vec![name(root, "folder")], 0))
            .unwrap();
        builder
            .push(record(
                20,
                1,
                false,
                false,
                vec![name(root, "hard-a.bin"), name(folder, "hard-b.bin")],
                100,
            ))
            .unwrap();
        builder
            .push(record(30, 1, true, true, vec![name(root, "junction")], 0))
            .unwrap();
        builder
            .push(record(
                50,
                1,
                false,
                false,
                vec![name(reference(5, 99), "stale")],
                777,
            ))
            .unwrap();
        builder
            .push(record(60, 1, true, false, vec![name(root, "Demo.app")], 0))
            .unwrap();
        builder
            .push(record(
                61,
                1,
                false,
                false,
                vec![name(package, "inside")],
                250,
            ))
            .unwrap();
        builder.finish().unwrap()
    }

    #[test]
    fn compact_structural_gates_hold() {
        assert!(size_of::<FileMeta>() <= 64);
        assert!(size_of::<NameLink>() <= 24);
    }

    #[test]
    fn traversal_is_parent_first_bounded_and_charges_hard_links_once() {
        let index = fixture_index();
        let mut batches = Vec::new();
        let summary = index
            .walk_subtree(
                reference(5, 1),
                TraversalOptions {
                    batch_rows: 2,
                    ..TraversalOptions::default()
                },
                || false,
                |batch| batches.push(batch),
            )
            .unwrap();
        assert!(batches.iter().all(|batch| batch.len() <= 2));
        let rows: Vec<_> = batches.into_iter().flatten().collect();
        let folder = rows
            .iter()
            .find(|row| row.display_name == "folder")
            .unwrap();
        let hard_b = rows
            .iter()
            .find(|row| row.display_name == "hard-b.bin")
            .unwrap();
        assert_eq!(hard_b.parent_id, folder.id);
        assert_eq!(
            rows.iter()
                .filter(|row| row.file_record.record == 20)
                .map(|row| row.logical_bytes)
                .sum::<u64>(),
            100
        );
        assert!(!rows.iter().any(|row| row.display_name == "stale"));
        let package = rows
            .iter()
            .find(|row| row.display_name == "Demo.app")
            .unwrap();
        assert_eq!(package.kind, NeutralNodeKind::OpaquePackage);
        assert_eq!(package.logical_bytes, 250);
        assert_eq!(summary.logical_bytes, 350);
        assert_eq!(index.stats().stale_or_missing_parent_links, 1);
    }

    #[test]
    fn cancellation_stops_before_emitting_more_rows() {
        let index = fixture_index();
        let mut checks = 0;
        let error = index
            .walk_subtree(
                reference(5, 1),
                TraversalOptions::default(),
                || {
                    checks += 1;
                    checks > 1
                },
                |_| {},
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }

    #[test]
    fn traversal_descends_reparse_directories_with_real_mft_children() {
        let root = reference(5, 1);
        let cloud = reference(10, 1);
        let mut builder = IndexBuilder::default();
        builder
            .push(record(5, 1, true, false, Vec::new(), 0))
            .unwrap();
        let mut cloud_record = record(10, 1, true, true, vec![name(root, "cloud")], 0);
        cloud_record.reparse_tag = Some(0x9000_601a);
        builder.push(cloud_record).unwrap();
        builder
            .push(record(
                11,
                1,
                false,
                false,
                vec![name(cloud, "online.bin")],
                123,
            ))
            .unwrap();
        builder
            .push(record(20, 1, true, true, vec![name(root, "junction")], 0))
            .unwrap();
        let index = builder.finish().unwrap();
        let mut rows = Vec::new();
        let summary = index
            .walk_subtree(
                root,
                TraversalOptions::default(),
                || false,
                |batch| rows.extend(batch),
            )
            .unwrap();

        assert!(rows.iter().any(|row| row.display_name == "online.bin"));
        assert!(rows.iter().any(|row| {
            row.display_name == "junction" && row.kind == NeutralNodeKind::ReparseDirectory
        }));
        assert_eq!(summary.logical_bytes, 123);
    }

    #[test]
    fn cloud_tag_from_extension_record_is_merged_into_base() {
        let root = reference(5, 1);
        let cloud = reference(10, 1);
        let mut builder = IndexBuilder::default();
        builder
            .push(record(5, 1, true, false, Vec::new(), 0))
            .unwrap();
        builder
            .push(record(10, 1, true, false, vec![name(root, "cloud")], 0))
            .unwrap();
        let mut extension = record(100, 2, false, true, Vec::new(), 0);
        extension.base_reference = Some(cloud);
        extension.reparse_tag = Some(0x9000_601a);
        builder.push(extension).unwrap();
        builder
            .push(record(
                11,
                1,
                false,
                false,
                vec![name(cloud, "online.bin")],
                123,
            ))
            .unwrap();
        let index = builder.finish().unwrap();
        let mut rows = Vec::new();
        index
            .walk_subtree(
                root,
                TraversalOptions::default(),
                || false,
                |batch| rows.extend(batch),
            )
            .unwrap();

        assert!(rows.iter().any(|row| row.display_name == "online.bin"));
    }
}
