//! Inline text/code preview for the preview pane.
//!
//! A Quick Look thumbnail of a source file is a tiny unreadable
//! image, so text files get their actual content rendered monospaced
//! instead (docs/features/PREVIEW.md). Detection happens in the
//! worker — read a bounded prefix, reject on NUL bytes, then decode
//! as UTF-8 with a Latin-1 fallback for legacy single-byte text — so
//! there's no dependency on magic having been sniffed yet, and the UI
//! thread never reads the file.
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
    if shell
        .process
        .text_preview_cache
        .borrow()
        .get(&path)
        .is_some()
    {
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
    Ok(decode_text_preview(buf))
}

/// The decode half of [`read_text_preview`], over bytes we already hold —
/// archive entries are read into memory rather than written out, so they need
/// the same text-vs-binary decision without a file to point at.
pub(crate) fn decode_text_preview(mut buf: Vec<u8>) -> Option<String> {
    buf.truncate(MAX_BYTES);
    let r: Result<Option<String>, ()> = (|| {
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
        // mid-buffer means the file isn't UTF-8 — but it may still be
        // single-byte text (ISO-8859-1: Amiga/DOS-era readmes, scene
        // .nfo files, old exports), which deserves a preview just as much
        // as UTF-8 does. Fall back to a Latin-1 decode when the bytes
        // look overwhelmingly printable; the NUL check above has already
        // rejected the classic binary shape.
        let text: std::borrow::Cow<'_, str> = match std::str::from_utf8(&buf) {
            Ok(t) => std::borrow::Cow::Borrowed(t),
            Err(e) if e.error_len().is_none() => std::borrow::Cow::Borrowed(
                std::str::from_utf8(&buf[..e.valid_up_to()]).map_err(|_| ())?,
            ),
            Err(_) if looks_like_single_byte_text(&buf) => {
                // In Latin-1 every byte IS its code point, so this cast
                // is the whole decode.
                std::borrow::Cow::Owned(buf.iter().map(|&b| b as char).collect())
            }
            Err(_) => return Ok(None),
        };
        let text = text.as_ref();
        let mut out: String = text.lines().take(MAX_LINES).collect::<Vec<_>>().join("\n");
        if text.lines().count() > MAX_LINES {
            out.push_str("\n\u{2026}");
        }
        Ok(Some(out))
    })();
    r.ok().flatten()
}

/// Gate for the Latin-1 fallback: ≥ 85% of the first 512 bytes must be
/// printable — ASCII graphic/whitespace or Latin-1 high bytes (0xA0+,
/// accents, ©, box-drawing in legacy pages). Mirrors the magic
/// sniffer's plain-text ratio (`ferail-fs-native/src/magic/text.rs`)
/// widened for the single-byte range; the 15% budget absorbs the odd
/// escape code in ANSI-art .nfo files without letting real binaries
/// through (their headers are control-byte-dense).
fn looks_like_single_byte_text(buf: &[u8]) -> bool {
    let sample = &buf[..buf.len().min(512)];
    let printable = sample
        .iter()
        .filter(|&&b| {
            b.is_ascii_graphic() || matches!(b, b' ' | b'\n' | b'\r' | b'\t') || b >= 0xA0
        })
        .count();
    printable * 100 / sample.len().max(1) >= 85
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

/// Turn a file's name + text into a markdown source string for
/// `gpui_component::text::markdown` (docs/features/PREVIEW.md):
///
/// - Markdown files (`.md` / `.markdown` / `.mdx`) pass through so the
///   TextView renders them formatted.
/// - Everything else is wrapped in a fenced code block tagged with the
///   file extension (the highlighter accepts extensions as language
///   aliases — `rs`, `py`, `ts`, …), so source files render
///   syntax-highlighted and unknown kinds fall back to plain mono.
///
/// The fence is made longer than any backtick run in the content so a
/// file that itself contains ``` can't break out of the block.
pub fn to_markdown_source(name: &str, text: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "md" | "markdown" | "mdx") {
        return text.to_string();
    }
    // A handful of well-known extensionless names map to a language;
    // everything else fences with the bare extension (empty for none).
    let lang = if ext.is_empty() {
        match name.to_ascii_lowercase().as_str() {
            "makefile" => "make",
            "cmakelists.txt" => "cmake",
            _ => "",
        }
        .to_string()
    } else {
        ext
    };
    let longest = longest_backtick_run(text);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}{lang}\n{text}\n{fence}")
}

/// Longest consecutive run of backtick characters anywhere in `text`.
fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferail-textprev-{}", std::process::id()));
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
    fn rejects_control_dense_binary() {
        let p = scratch("c.bin");
        // Invalid UTF-8 AND control-byte-dense (no NULs): a typical
        // binary header shape. The Latin-1 fallback must not claim it.
        let mut bytes = vec![b'M', b'Z'];
        bytes.extend((1u8..=120).flat_map(|b| [b % 0x1F + 1, 0x90]));
        std::fs::write(&p, &bytes).unwrap();
        assert!(read_text_preview(&p).unwrap().is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reads_latin1_text() {
        let p = scratch("readme-latin1.txt");
        // ISO-8859-1 "Café © Digita" — 0xE9 (é) and 0xA9 (©) are
        // invalid UTF-8 lead bytes, but this is text and previews as
        // such via the Latin-1 fallback (Amiga/DOS-era readmes).
        std::fs::write(
            &p,
            [
                b'C', b'a', b'f', 0xE9, b' ', 0xA9, b' ', b'D', b'i', b'g', b'i', b't', b'a',
            ],
        )
        .unwrap();
        let out = read_text_preview(&p).unwrap().unwrap();
        assert_eq!(out, "Café © Digita");
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
    fn markdown_passes_through() {
        assert_eq!(to_markdown_source("readme.md", "# Hi\ntext"), "# Hi\ntext");
        assert_eq!(to_markdown_source("a.MARKDOWN", "x"), "x");
    }

    #[test]
    fn source_is_fenced_with_extension() {
        let out = to_markdown_source("main.rs", "fn main() {}");
        assert_eq!(out, "```rs\nfn main() {}\n```");
        // Extensionless well-known name.
        assert!(to_markdown_source("Makefile", "all:").starts_with("```make\n"));
        // Unknown extension still fences (plain, no highlight).
        assert!(to_markdown_source("notes.xyz", "hi").starts_with("```xyz\n"));
    }

    #[test]
    fn fence_outgrows_inner_backticks() {
        // Content containing a ``` run must get a 4-backtick fence.
        let out = to_markdown_source("x.md_not", "a\n```\nb");
        assert!(out.starts_with("````"));
        assert!(out.ends_with("````"));
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
