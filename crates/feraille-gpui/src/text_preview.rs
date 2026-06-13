//! Inline text/code preview for the preview pane.
//!
//! A Quick Look thumbnail of a source file is a tiny unreadable
//! image, so text files get their actual content rendered monospaced
//! instead (docs/features/PREVIEW.md). Detection happens in the
//! worker — read a bounded prefix, reject on NUL bytes or invalid
//! UTF-8 — so there's no dependency on magic having been sniffed yet,
//! and the UI thread never reads the file.
//!
//! Parallel in shape to [`crate::preview`] (the thumbnail provider):
//! per-path LRU cache, `Pending` markers dedup in-flight reads,
//! results re-enter through `shell.update`. `preview::request` kicks
//! both providers, so the render picks text when it's text and the
//! thumbnail otherwise.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use gpui::{AsyncApp, SharedString};

use crate::shell::Shell;

/// How much of the file to read for detection + preview. A code file
/// rarely needs more on screen, and it bounds the cost for a huge
/// file that happens to be text (logs, CSVs).
const MAX_BYTES: usize = 128 * 1024;
/// Lines kept after decoding — the pane scrolls, but an enormous
/// single-line minified file shouldn't balloon the render tree.
const MAX_LINES: usize = 500;
/// Per-path cache cap — the pane shows one at a time, so small is fine.
const CACHE_CAP: usize = 16;

#[derive(Clone)]
pub enum TextPreviewState {
    /// Read in flight on the background executor.
    Pending,
    /// Decoded text (already line-capped), ready to render.
    Loaded(SharedString),
    /// Read succeeded but the content isn't text (binary / image /
    /// etc.) — the thumbnail provider covers those.
    NotText,
    /// The read itself failed (permissions, gone).
    Failed,
}

pub struct TextPreviewCache {
    by_path: HashMap<PathBuf, TextPreviewState>,
    order: Vec<PathBuf>,
}

impl Default for TextPreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TextPreviewCache {
    pub fn new() -> Self {
        Self {
            by_path: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn get(&self, path: &Path) -> Option<TextPreviewState> {
        self.by_path.get(path).cloned()
    }

    pub fn insert(&mut self, path: PathBuf, state: TextPreviewState) {
        if !self.by_path.contains_key(&path) {
            self.order.push(path.clone());
        }
        self.by_path.insert(path, state);
        while self.order.len() > CACHE_CAP {
            let oldest = self.order.remove(0);
            self.by_path.remove(&oldest);
        }
    }
}

/// Kick the background text read for `path` unless the cache already
/// has it (in any state — `Pending` dedups, `NotText`/`Failed` stop
/// retry storms). Mirrors `preview::request`; call sites already hold
/// `&mut Shell`.
pub fn request(shell: &mut Shell, path: PathBuf, cx: &mut gpui::Context<Shell>) {
    if shell.process.text_preview_cache.borrow().get(&path).is_some() {
        return;
    }
    shell
        .process
        .text_preview_cache
        .borrow_mut()
        .insert(path.clone(), TextPreviewState::Pending);

    let weak = cx.weak_entity();
    cx.spawn(async move |_this, cx| {
        let p = path.clone();
        let result = cx
            .background_executor()
            .spawn(async move { read_text_preview(&p) })
            .await;
        apply_result(weak, path, result, cx).await;
    })
    .detach();
}

/// Read up to [`MAX_BYTES`], decide text-vs-binary, and return the
/// line-capped text. `Ok(None)` = read fine but not text; `Err` =
/// read failed. Worker-thread only.
fn read_text_preview(path: &Path) -> Result<Option<String>, ()> {
    let mut f = std::fs::File::open(path).map_err(|_| ())?;
    let mut buf = vec![0u8; MAX_BYTES];
    let n = f.read(&mut buf).map_err(|_| ())?;
    buf.truncate(n);
    if buf.is_empty() {
        // An empty file is "text" (empty) — nicer than a blank
        // thumbnail box.
        return Ok(Some(String::new()));
    }
    // A NUL byte in the prefix is the classic binary tell.
    if buf.contains(&0) {
        return Ok(None);
    }
    // Decode as UTF-8, tolerating only a multibyte char split at the
    // read boundary (error_len == None). A real invalid sequence
    // mid-buffer means binary.
    let valid_end = match std::str::from_utf8(&buf) {
        Ok(_) => buf.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => return Ok(None),
    };
    let text = std::str::from_utf8(&buf[..valid_end]).map_err(|_| ())?;
    let mut out: String = text
        .lines()
        .take(MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if text.lines().count() > MAX_LINES {
        out.push_str("\n\u{2026}");
    }
    Ok(Some(out))
}

async fn apply_result(
    weak: gpui::WeakEntity<Shell>,
    path: PathBuf,
    result: Result<Option<String>, ()>,
    cx: &mut AsyncApp,
) {
    let state = match result {
        Ok(Some(text)) => TextPreviewState::Loaded(SharedString::from(text)),
        Ok(None) => TextPreviewState::NotText,
        Err(()) => TextPreviewState::Failed,
    };
    let Some(shell) = weak.upgrade() else { return };
    shell.update(cx, |shell, cx| {
        shell
            .process
            .text_preview_cache
            .borrow_mut()
            .insert(path, state);
        cx.notify();
    });
}

/// Lookup helper for `Shell::preview`: the renderable text when ready,
/// else `None`.
pub fn loaded_text(state: Option<TextPreviewState>) -> Option<SharedString> {
    match state {
        Some(TextPreviewState::Loaded(t)) => Some(t),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("feraille-textprev-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn reads_utf8_text() {
        let p = scratch("a.rs");
        std::fs::write(&p, "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        let out = read_text_preview(&p).unwrap().unwrap();
        assert!(out.contains("fn main()"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_nul_binary() {
        let p = scratch("b.bin");
        std::fs::write(&p, [0x00, 0x01, 0x02, b'h', b'i']).unwrap();
        assert!(read_text_preview(&p).unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_invalid_utf8() {
        let p = scratch("c.bin");
        // 0xFF 0xFE mid-buffer is not a valid UTF-8 lead.
        std::fs::write(&p, [b'h', b'i', 0xFF, 0xFE, b'y', b'o']).unwrap();
        assert!(read_text_preview(&p).unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_file_is_empty_text() {
        let p = scratch("d.txt");
        std::fs::write(&p, "").unwrap();
        assert_eq!(read_text_preview(&p).unwrap(), Some(String::new()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn caps_long_files() {
        let p = scratch("e.txt");
        let body: String = (0..MAX_LINES + 50).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        let out = read_text_preview(&p).unwrap().unwrap();
        assert!(out.ends_with('\u{2026}'));
        assert!(out.lines().count() <= MAX_LINES + 1);
        let _ = std::fs::remove_file(&p);
    }
}
