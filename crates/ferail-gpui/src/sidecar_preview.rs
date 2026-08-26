//! Bounded, process-memory-only discovery of immediate folder sidecars.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ferail_fs_native::MagicType;
use gpui::AsyncApp;

use crate::preview_queue::{Enqueue, LatestRequestQueue};
use crate::shell::Shell;

const CACHE_CAP: usize = 16;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_SIDECARS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidecarKind {
    Nfo,
    Manifest,
}

#[derive(Clone, Debug)]
pub struct SidecarHint {
    pub path: PathBuf,
    pub name: String,
    pub kind: SidecarKind,
    pub format: &'static str,
}

#[derive(Clone, Debug)]
pub enum FolderSidecarsState {
    Pending,
    Ready {
        hints: Vec<SidecarHint>,
        truncated: bool,
    },
    Failed,
}

pub struct FolderSidecarCache {
    entries: HashMap<PathBuf, FolderSidecarsState>,
    order: Vec<PathBuf>,
    requests: LatestRequestQueue<PathBuf>,
}

impl Default for FolderSidecarCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FolderSidecarCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            requests: LatestRequestQueue::default(),
        }
    }

    pub fn get(&self, path: &Path) -> Option<FolderSidecarsState> {
        self.entries.get(path).cloned()
    }

    fn insert(&mut self, path: PathBuf, state: FolderSidecarsState) {
        if !self.entries.contains_key(&path) {
            self.order.push(path.clone());
        }
        self.entries.insert(path, state);
        while self.order.len() > CACHE_CAP {
            let oldest = self.order.remove(0);
            self.entries.remove(&oldest);
        }
    }

    fn enqueue_request(&mut self, path: PathBuf) -> Enqueue {
        self.requests.enqueue(path)
    }

    fn complete_request(&mut self, path: &PathBuf) -> Option<PathBuf> {
        self.requests.complete(path)
    }

    /// Forget completed folder scans under the explicitly refreshed root.
    /// At most 16 entries are inspected; directory size is irrelevant.
    pub fn invalidate_finished_under(&mut self, root: &Path) {
        self.entries.retain(|path, state| {
            !path.starts_with(root) || matches!(state, FolderSidecarsState::Pending)
        });
        let entries = &self.entries;
        self.order.retain(|path| entries.contains_key(path));
    }
}

pub fn request(shell: &mut Shell, folder: PathBuf, cx: &mut gpui::Context<Shell>) {
    if shell
        .process
        .folder_sidecar_cache
        .borrow()
        .get(&folder)
        .is_some()
    {
        return;
    }
    let enqueue = shell
        .process
        .folder_sidecar_cache
        .borrow_mut()
        .enqueue_request(folder.clone());
    if !matches!(enqueue, Enqueue::Start) {
        return;
    }
    start_request(shell, folder, cx);
}

fn start_request(shell: &mut Shell, folder: PathBuf, cx: &mut gpui::Context<Shell>) {
    shell
        .process
        .folder_sidecar_cache
        .borrow_mut()
        .insert(folder.clone(), FolderSidecarsState::Pending);
    let weak = cx.weak_entity();
    let process = shell.process.clone();
    cx.spawn(async move |_this, cx| {
        let scan_folder = folder.clone();
        let result = cx
            .background_executor()
            .spawn(async move { scan(&scan_folder) })
            .await;
        apply(weak, process, folder, result, cx).await;
    })
    .detach();
}

fn scan(folder: &Path) -> std::io::Result<(Vec<SidecarHint>, bool)> {
    let mut hints = Vec::new();
    let mut inspected = 0usize;
    let mut truncated = false;
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        inspected += 1;
        if inspected > MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !candidate_name(&name) {
            continue;
        }
        let Some(info) = ferail_fs_native::detect_magic_info(&path) else {
            continue;
        };
        let kind = match info.magic_type {
            MagicType::NfoScene | MagicType::NfoKodi | MagicType::NfoMsInfo => SidecarKind::Nfo,
            MagicType::ChecksumSfv | MagicType::ChecksumList => SidecarKind::Manifest,
            _ => continue,
        };
        hints.push(SidecarHint {
            path,
            name,
            kind,
            format: info.magic_type.display_name(),
        });
        if hints.len() == MAX_SIDECARS {
            truncated = true;
            break;
        }
    }
    hints.sort_by_cached_key(|hint| hint.name.to_lowercase());
    Ok((hints, truncated))
}

fn candidate_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".nfo")
        || lower.ends_with(".diz")
        || lower.ends_with(".sfv")
        || lower.ends_with(".md5")
        || lower.ends_with(".sha1")
        || lower.ends_with(".sha224")
        || lower.ends_with(".sha256")
        || lower.ends_with(".sha384")
        || lower.ends_with(".sha512")
        || matches!(
            lower.as_str(),
            "md5sums" | "sha1sums" | "sha224sums" | "sha256sums" | "sha384sums" | "sha512sums"
        )
}

async fn apply(
    weak: gpui::WeakEntity<Shell>,
    process: std::rc::Rc<crate::process_state::ProcessState>,
    folder: PathBuf,
    result: std::io::Result<(Vec<SidecarHint>, bool)>,
    cx: &mut AsyncApp,
) {
    let next = {
        let mut cache = process.folder_sidecar_cache.borrow_mut();
        cache.insert(
            folder.clone(),
            match result {
                Ok((hints, truncated)) => FolderSidecarsState::Ready { hints, truncated },
                Err(_) => FolderSidecarsState::Failed,
            },
        );
        cache.complete_request(&folder)
    };
    if let Some(shell) = weak.upgrade() {
        shell.update(cx, |shell, cx| {
            cx.notify();
            if let Some(next) = next {
                request(shell, next, cx);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_refresh_invalidates_only_finished_scans_under_root() {
        let root = Path::new("/root/folder");
        let inside = root.join("release");
        let pending = root.join("pending");
        let outside = PathBuf::from("/else/release");
        let mut cache = FolderSidecarCache::new();
        cache.insert(inside.clone(), FolderSidecarsState::Failed);
        cache.insert(pending.clone(), FolderSidecarsState::Pending);
        cache.insert(outside.clone(), FolderSidecarsState::Failed);

        cache.invalidate_finished_under(root);

        assert!(cache.get(&inside).is_none());
        assert!(matches!(
            cache.get(&pending),
            Some(FolderSidecarsState::Pending)
        ));
        assert!(matches!(
            cache.get(&outside),
            Some(FolderSidecarsState::Failed)
        ));
    }
}
