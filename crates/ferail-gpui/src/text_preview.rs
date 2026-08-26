//! Inline text/code preview for the preview pane.
//!
//! A Quick Look thumbnail of a source file is a tiny unreadable
//! image, so text files get their actual content rendered monospaced
//! instead (docs/features/PREVIEW.md). Detection happens in the
//! worker — read a bounded prefix, then use the shared UTF/CP437/Latin-1
//! decoder (with an inert ANSI layout pass for scene art) — so
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
use std::sync::Arc;

use gpui::{AsyncApp, SharedString};

use crate::preview_queue::{Enqueue, LatestRequestQueue};
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

#[derive(Clone, Debug)]
pub struct TextPreviewDocument {
    pub text: SharedString,
    pub ansi_spans: Arc<Vec<ferail_core::text_encoding::AnsiSpan>>,
    pub terminal_art: bool,
}

#[derive(Clone)]
pub enum TextPreviewState {
    /// Read in flight on the background executor.
    Pending,
    /// Decoded text (already line-capped), ready to render.
    Loaded(Arc<TextPreviewDocument>),
    /// Read succeeded but the content isn't text (binary / image /
    /// etc.) — the thumbnail provider covers those.
    NotText,
    /// The read itself failed (permissions, gone).
    Failed,
}

pub struct TextPreviewCache {
    by_path: HashMap<PathBuf, TextPreviewState>,
    order: Vec<PathBuf>,
    requests: LatestRequestQueue<PathBuf>,
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
            requests: LatestRequestQueue::default(),
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

    /// Drop one cached preview before an explicit user-requested reload.
    /// This is intentionally not called from render: cache invalidation must
    /// never add filesystem I/O or mutation to the paint path.
    pub fn invalidate(&mut self, path: &Path) {
        self.by_path.remove(path);
        self.order.retain(|cached| cached != path);
    }

    /// Invalidate completed previews belonging to a refreshed directory.
    /// The cache is capped at 16 paths, so this work is independent of the
    /// directory's row count. Pending work stays scheduled; removing it would
    /// make the latest-wins queue suppress the replacement request.
    pub fn invalidate_finished_under(&mut self, root: &Path) {
        self.by_path.retain(|path, state| {
            !path.starts_with(root) || matches!(state, TextPreviewState::Pending)
        });
        let by_path = &self.by_path;
        self.order.retain(|path| by_path.contains_key(path));
    }

    fn enqueue_request(&mut self, path: PathBuf) -> Enqueue {
        self.requests.enqueue(path)
    }

    fn complete_request(&mut self, path: &PathBuf) -> Option<PathBuf> {
        self.requests.complete(path)
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
    let enqueue = shell
        .process
        .text_preview_cache
        .borrow_mut()
        .enqueue_request(path.clone());
    if !matches!(enqueue, Enqueue::Start) {
        return;
    }
    start_request(shell, path, cx);
}

fn start_request(shell: &mut Shell, path: PathBuf, cx: &mut gpui::Context<Shell>) {
    shell
        .process
        .text_preview_cache
        .borrow_mut()
        .insert(path.clone(), TextPreviewState::Pending);

    let weak = cx.weak_entity();
    let process = shell.process.clone();
    cx.spawn(async move |_this, cx| {
        let p = path.clone();
        let result = cx
            .background_executor()
            .spawn(async move { read_text_preview(&p) })
            .await;
        apply_result(weak, process, path, result, cx).await;
    })
    .detach();
}

/// Read up to [`MAX_BYTES`], decide text-vs-binary, and return the
/// line-capped text. `Ok(None)` = read fine but not text; `Err` =
/// read failed. Worker-thread only.
fn read_text_preview(path: &Path) -> Result<Option<TextPreviewDocument>, ()> {
    let mut f = std::fs::File::open(path).map_err(|_| ())?;
    let mut buf = vec![0u8; MAX_BYTES];
    let n = f.read(&mut buf).map_err(|_| ())?;
    buf.truncate(n);
    Ok(decode_text_preview_document(buf))
}

/// The decode half of [`read_text_preview`], over bytes we already hold —
/// archive entries are read into memory rather than written out, so they need
/// the same text-vs-binary decision without a file to point at.
pub(crate) fn decode_text_preview(buf: Vec<u8>) -> Option<String> {
    decode_text_preview_document(buf).map(|document| document.text.to_string())
}

fn decode_text_preview_document(mut buf: Vec<u8>) -> Option<TextPreviewDocument> {
    buf.truncate(MAX_BYTES);
    // A bounded read can end between UTF-8 bytes. Preserve the prior friendly
    // behaviour by dropping only that incomplete final codepoint.
    if let Err(error) = std::str::from_utf8(&buf) {
        if error.error_len().is_none() && error.valid_up_to() > 0 {
            buf.truncate(error.valid_up_to());
        }
    }
    let decoded = ferail_core::text_encoding::decode_text(&buf)?;
    if let Some(kodi) = format_kodi_preview(&decoded.text) {
        return Some(TextPreviewDocument {
            text: cap_lines(kodi).into(),
            ansi_spans: Arc::new(Vec::new()),
            terminal_art: false,
        });
    }
    let terminal_art = decoded.text.contains('\u{1b}')
        || ferail_core::text_encoding::looks_like_cp437_art(&buf)
        || ferail_core::text_encoding::looks_like_text_art(&decoded.text);
    let rendered = ferail_core::text_encoding::render_ansi(&decoded.text, 240, MAX_LINES);
    Some(TextPreviewDocument {
        text: cap_lines(rendered.text).into(),
        ansi_spans: Arc::new(rendered.spans),
        terminal_art,
    })
}

fn cap_lines(rendered: String) -> String {
    let mut lines = rendered.lines();
    let mut out = lines
        .by_ref()
        .take(MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if lines.next().is_some() {
        out.push_str("\n\u{2026}");
    }
    out
}

/// Present Kodi metadata as a small readable summary, followed by the local
/// source. URLs remain inert text; this function performs no network work.
fn format_kodi_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let roots = [
        "movie",
        "movieset",
        "set",
        "tvshow",
        "episodedetails",
        "musicvideo",
        "artist",
        "album",
    ];
    let xml = roots
        .iter()
        .any(|root| lower.contains(&format!("<{root}>")) || lower.contains(&format!("<{root} ")));
    let scraper_url = text.lines().map(str::trim).find(|line| {
        let lower = line.to_ascii_lowercase();
        (lower.starts_with("http://") || lower.starts_with("https://"))
            && [
                "themoviedb.org/",
                "thetvdb.com/",
                "tvdb.com/",
                "imdb.com/title/",
                "musicbrainz.org/",
            ]
            .iter()
            .any(|host| lower.contains(host))
    });
    if !xml && scraper_url.is_none() {
        return None;
    }

    let mut out = String::new();
    out.push_str(&tr!("Kodi metadata"));
    out.push('\n');
    for (label, tag) in [
        (tr!("Title"), "title"),
        (tr!("Original title"), "originaltitle"),
        (tr!("Year"), "year"),
        (tr!("Season"), "season"),
        (tr!("Episode"), "episode"),
        (tr!("Rating"), "rating"),
        (tr!("Plot"), "plot"),
    ] {
        if let Some(value) = xml_tag_value(text, &lower, tag) {
            out.push_str(&format!("{label}: {}\n", decode_xml_entities(value.trim())));
        }
    }
    if let Some(url) = scraper_url {
        out.push_str(&format!("{}: {url}\n", tr!("Scraping URL")));
    }
    out.push_str(&format!("\n{}\n────────\n", tr!("Source")));
    out.push_str(text);
    Some(out)
}

fn xml_tag_value<'a>(text: &'a str, lower: &str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = lower[start..].find(&close)? + start;
    text.get(start..end)
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

async fn apply_result(
    weak: gpui::WeakEntity<Shell>,
    process: std::rc::Rc<crate::process_state::ProcessState>,
    path: PathBuf,
    result: Result<Option<TextPreviewDocument>, ()>,
    cx: &mut AsyncApp,
) {
    let state = match result {
        Ok(Some(document)) => TextPreviewState::Loaded(Arc::new(document)),
        Ok(None) => TextPreviewState::NotText,
        Err(()) => TextPreviewState::Failed,
    };
    let next = {
        let mut cache = process.text_preview_cache.borrow_mut();
        cache.insert(path.clone(), state);
        cache.complete_request(&path)
    };
    let Some(shell) = weak.upgrade() else { return };
    shell.update(cx, |shell, cx| {
        cx.notify();
        if let Some(next) = next {
            request(shell, next, cx);
        }
    });
}

/// Lookup helper for `Shell::preview`: the renderable text when ready,
/// else `None`.
pub fn loaded_text(state: Option<TextPreviewState>) -> Option<SharedString> {
    match state {
        Some(TextPreviewState::Loaded(document)) => Some(document.text.clone()),
        _ => None,
    }
}

pub fn loaded_document(state: Option<TextPreviewState>) -> Option<Arc<TextPreviewDocument>> {
    match state {
        Some(TextPreviewState::Loaded(document)) => Some(document),
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
        assert!(out.text.contains("fn main()"));
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
        assert_eq!(out.text, "Café © Digita");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reads_cp437_scene_fixture_and_removes_ansi_controls() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/sidecars/generated/nfo");
        let cp437 = read_text_preview(&root.join("scene-cp437.nfo"))
            .unwrap()
            .unwrap();
        assert!(cp437.text.contains("╔"));
        assert!(cp437.text.contains("████"));
        assert!(cp437.terminal_art);

        let ansi = read_text_preview(&root.join("scene-ansi.nfo"))
            .unwrap()
            .unwrap();
        assert!(ansi.text.contains("Placed text"));
        assert!(!ansi.text.contains('\u{1b}'));
        assert!(!ansi.text.contains("52;c"));
        assert!(!ansi.ansi_spans.is_empty());

        let release = read_text_preview(&root.join("ferail-release-color.nfo"))
            .unwrap()
            .unwrap();
        assert!(release.text.contains("GREETINGS TO MUMU"));
        assert!(release.terminal_art);
        assert!(release.ansi_spans.len() > 10);
        assert!(release.ansi_spans.iter().any(|span| matches!(
            span.style.foreground,
            Some(ferail_core::text_encoding::AnsiColor::Indexed(_))
        )));
    }

    #[test]
    fn reads_utf16_msinfo_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/sidecars/generated/nfo/msinfo.nfo");
        let text = read_text_preview(&path).unwrap().unwrap();
        assert!(text.text.contains("<MsInfo>"));
    }

    #[test]
    fn kodi_preview_is_structured_and_keeps_the_source_local() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/sidecars/generated/nfo/kodi-metadata.nfo");
        let text = read_text_preview(&path).unwrap().unwrap();
        assert!(text.text.contains("Title: Fixture Movie"));
        assert!(text.text.contains("Year: 2026"));
        assert!(text.text.contains("<movie>"));
    }

    #[test]
    fn empty_file_is_empty_text() {
        let p = scratch("d.txt");
        std::fs::write(&p, "").unwrap();
        assert_eq!(
            read_text_preview(&p).unwrap().unwrap().text,
            SharedString::from("")
        );
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
        assert!(out.text.ends_with('\u{2026}'));
        assert!(out.text.lines().count() <= MAX_LINES + 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn directory_refresh_invalidates_only_finished_previews_under_root() {
        let root = Path::new("/root/folder");
        let inside = root.join("inside.nfo");
        let pending = root.join("pending.nfo");
        let outside = PathBuf::from("/else/outside.nfo");
        let mut cache = TextPreviewCache::new();
        cache.insert(inside.clone(), TextPreviewState::NotText);
        cache.insert(pending.clone(), TextPreviewState::Pending);
        cache.insert(outside.clone(), TextPreviewState::Failed);

        cache.invalidate_finished_under(root);

        assert!(cache.get(&inside).is_none());
        assert!(matches!(
            cache.get(&pending),
            Some(TextPreviewState::Pending)
        ));
        assert!(matches!(
            cache.get(&outside),
            Some(TextPreviewState::Failed)
        ));
    }
}
