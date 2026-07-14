//! Domain types shared between FS, controls, and app layers.
//! This crate has zero platform deps and zero UI deps. That is enforced by
//! convention, not the compiler — if you find yourself reaching for `windows`
//! or `winit` here, stop.

pub mod commands;
pub mod entry_info;
pub mod favorites;
pub mod media;
pub mod name_hazards;
pub mod navigation;
pub mod node_store;
pub mod path_guard;
pub mod power;
pub mod video;

use std::num::NonZeroU64;

/// Stable identifier for a tree/list node. Opaque to the UI; the FS layer
/// owns the mapping `NodeId <-> path/PIDL`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(NonZeroU64);

impl NodeId {
    pub fn from_raw(raw: u64) -> Option<Self> {
        NonZeroU64::new(raw).map(Self)
    }
    pub fn as_raw(self) -> u64 {
        self.0.get()
    }
}

impl From<u64> for NodeId {
    fn from(v: u64) -> Self {
        Self(NonZeroU64::new(v.max(1)).expect("post-max nonzero"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

/// One row in the file pane. Display strings are pre-formatted; paint never
/// formats numbers. The modification time is the deliberate exception: it is
/// rendered *live* from [`mtime_unix`](Self::mtime_unix) via
/// [`humanize_mtime`] so a relative label ("4 seconds ago") keeps counting up
/// instead of freezing at enumerate time — see that function for why this is
/// cheap and paint-safe.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub id: NodeId,
    /// The raw on-disk leaf name — the bytes `readdir` returned. This is the
    /// *truth* used to reconstruct the file's path (joins, renames, opens);
    /// never the user-facing string. On macOS a colon here is the HFS/Unix
    /// separator that Finder shows as a slash — see [`display_name`](Self::display_name).
    pub name: String,
    /// The user-facing leaf name, pre-computed at enumerate time. On macOS a
    /// `:` in [`name`](Self::name) is shown as `/` to match Finder (see
    /// `feraille_fs_native::paths::display_leaf`); elsewhere this equals
    /// `name`. Every visible surface (list row, preview, Get Info, tooltips)
    /// renders this, while path operations keep using `name`.
    pub display_name: String,
    /// Pre-computed `name_hazards::has_hazards(&display_name)`. Lets the dense
    /// list row decide — with a cheap bool, no per-paint `analyze()` alloc —
    /// whether to draw the deceptive-character highlight treatment.
    pub name_has_hazards: bool,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_unix: i64,
    pub display_size: String,
    /// Friendly type label — "Folder", "Symlink", uppercased extension
    /// (e.g. "RS", "MD"), or "File" when there's no extension. macOS shell
    /// crate (iter-4) replaces this with `NSWorkspace.localizedDescription`.
    pub display_kind: String,
    /// Magic-byte detected type, e.g. "PNG image", "Mach-O 64-bit", "Plain text".
    /// Empty string when not yet detected or no match. Populated lazily by
    /// the host (App) — `feraille-core` never blocks on file I/O.
    pub display_magic: String,
    /// Rich ` · `-joined fact string for the Description column,
    /// e.g. `"Windows PE · 64-bit · x86-64 · GUI · .NET"`,
    /// `"PNG image · 1920×1080 · alpha"`,
    /// `"MP3 · stereo · 44 kHz · 192 kbps · 03:24"`.
    /// Empty when not yet detected or no extra facts to report.
    /// Populated lazily by the host (prefetch worker), same contract as
    /// `display_magic`.
    pub display_description: String,
    /// Hot-path flag for the icon-overlay dot. True when the file carries
    /// `com.apple.quarantine` (macOS Mark-of-the-Web equivalent). Populated
    /// lazily by the host alongside `quarantine`; defaults to false.
    pub is_quarantined: bool,
    /// Detail-panel rows for downloaded files. `None` until the prefetch
    /// worker reports back; `Some` with empty fields means "we looked,
    /// nothing to show beyond the flag."
    pub quarantine: Option<QuarantineDetails>,
    /// Platform "hidden" semantics, resolved at enumerate time by the
    /// filesystem backend — NOT a name heuristic. macOS: dot-prefix OR
    /// the `UF_HIDDEN` BSD flag (what Finder hides). Windows: dot-prefix
    /// OR `FILE_ATTRIBUTE_HIDDEN` (covers `$RECYCLE.BIN`, `desktop.ini`,
    /// etc.). Filter sites must use this flag, never re-derive from the
    /// name, so the show-hidden toggle behaves like the native file
    /// manager on every platform.
    pub hidden: bool,
}

/// How strongly the Format column should flag the relationship between a
/// file's extension and its content-detected type.
///
/// The model is *directional risk escalation*, not symmetric disagreement:
/// a file is only alarming when its real content is more dangerous than its
/// extension lets on. Benign disagreements (a PNG kept as `.txt`, a config
/// that is really XML) are surfaced quietly so they don't drown out the
/// genuine disguises.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatFlag {
    /// Extension and content agree, it's an honest executable, or there is
    /// no extension claim to contradict. Draw nothing.
    None,
    /// They describe different but *benign* formats — a renamed or resaved
    /// file. Worth a quiet, non-alarming cue; not a threat.
    Notice,
    /// The content is active/dangerous and the extension hides that — an
    /// executable or script wearing an image/document/text extension,
    /// hidden macros in an Office file, or an archive/binary smuggled
    /// inside a media/document/text file. Draw the danger indicator.
    Alert,
}

impl FormatFlag {
    pub fn is_alert(self) -> bool {
        matches!(self, FormatFlag::Alert)
    }
    pub fn is_notice(self) -> bool {
        matches!(self, FormatFlag::Notice)
    }
}

impl FileEntry {
    /// Unified Format label for the file list: prefer the magic-detected
    /// description, fall back to the extension-derived kind. Returns
    /// `(primary, flag)` where `flag` grades how the extension and the
    /// detected content relate — see [`FormatFlag`]. The danger tier fires
    /// only for genuine disguises (dangerous content under an innocent
    /// extension); mere terminology or benign-format differences earn the
    /// quiet [`FormatFlag::Notice`] tier instead.
    pub fn format_label(&self) -> (String, FormatFlag) {
        let mag = self.display_magic.trim();
        let kind = self.display_kind.trim();
        if mag.is_empty() {
            return (kind.to_string(), FormatFlag::None);
        }
        if kind.is_empty() {
            return (mag.to_string(), FormatFlag::None);
        }
        (
            mag.to_string(),
            classify_format(kind, mag, &self.display_description),
        )
    }
}

/// Heuristic: do the extension-derived `kind` and the magic-detected
/// `magic` strings describe compatible format families? Used to drive
/// the file-list mismatch indicator without raising false alarms for
/// the common "extension says JSON, magic says plain-text" case.
fn formats_compatible(kind: &str, magic: &str) -> bool {
    let k = normalize_format(kind);
    let m = normalize_format(magic);
    if k.is_empty() || m.is_empty() {
        return true;
    }
    // Placeholder kinds ("File" / "Folder" / "Symlink") fire when a
    // file has no extension or we couldn't derive one — they're
    // *missing* information, not an assertion about format. A Mach-O
    // binary with no extension still shows kind="File", which
    // doesn't contradict the magic-detected type. Same for folders
    // and symlinks (which won't reach magic detection but we belt-
    // and-suspender it).
    if matches!(k.as_str(), "file" | "folder" | "symlink") {
        return true;
    }
    if k == m || m.contains(&k) || k.contains(&m) {
        return true;
    }
    // Textual extensions all live happily under "plain text" / "ascii text" / "utf-8".
    let textual = [
        "txt", "md", "markdown", "rst", "log", "json", "yaml",
        "toml", "ini", "csv", "tsv", "xml", "html", "css", "scss",
        "rs", "py", "js", "ts", "go", "rb", "c", "cpp", "h", "hpp",
        "java", "kt", "swift", "sh", "bash", "zsh", "vim", "lua",
        "sql", "graphql", "proto", "tex", "el", "svg",
    ];
    if (m.contains("text") || m.contains("script") || m.contains("source"))
        && textual.iter().any(|t| k.contains(t))
    {
        return true;
    }
    // Office / EPUB / JAR / APK formats are ZIP archives at the byte level.
    let zip_kindly = [
        "docx", "xlsx", "pptx", "epub", "jar", "apk", "ipa", "odt",
        "ods", "odp", "zip", "war",
    ];
    if m.contains("zip") && zip_kindly.iter().any(|t| k.contains(t)) {
        return true;
    }
    // Office-app pairings: when the magic detector identifies the
    // ZIP-wrapped Office subtype directly ("PowerPoint presentation",
    // "Word document", "Excel spreadsheet") the extension still
    // says "DOCX" / "XLSX" / "PPTX". These agree semantically; the
    // string forms just differ. Match them explicitly so the file
    // list doesn't flag every Office file as a content mismatch.
    let office_pairs: &[(&str, &[&str])] = &[
        ("docx", &["word", "docx"]),
        ("xlsx", &["excel", "xlsx", "spreadsheet"]),
        ("pptx", &["powerpoint", "pptx", "presentation"]),
        ("doc", &["word", "doc"]),
        ("xls", &["excel", "xls", "spreadsheet"]),
        ("ppt", &["powerpoint", "ppt", "presentation"]),
        ("odt", &["opendocument", "writer", "odt"]),
        ("ods", &["opendocument", "calc", "ods", "spreadsheet"]),
        ("odp", &["opendocument", "impress", "odp", "presentation"]),
        ("epub", &["epub", "publication"]),
        ("rtf", &["rich text"]),
    ];
    for (ext, magic_keywords) in office_pairs {
        if k.contains(ext) && magic_keywords.iter().any(|kw| m.contains(kw)) {
            return true;
        }
    }
    // Windows Media: `.wma`/`.wmv`/`.asf` all live in the one ASF container,
    // which the detector labels "Windows Media [Audio|Video]". Treat them as
    // agreeing so an ordinary WMA/WMV isn't flagged as a content mismatch.
    if matches!(k.as_str(), "wma" | "wmv" | "asf") && m.contains("windows media") {
        return true;
    }
    false
}

/// Coarse danger class of *content*, derived from the magic label and the
/// description string (the macro flag rides in the description as
/// `… · macro-enabled`). Drives [`classify_format`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentRisk {
    /// Native code or an executable bundle (PE / ELF / Mach-O / .NET /
    /// JAR / APK / Java class) or an OS shortcut (`.lnk` / `.url`).
    Executable,
    /// An interpreted script (shell / python / perl / ruby / node / …).
    Script,
    /// A macro-enabled Office document.
    Macro,
    /// An opaque archive or unrecognized binary blob.
    ArchiveOrBinary,
    /// Everything inert: images, audio, video, PDFs, plain text, markup,
    /// data, fonts, unknown text.
    Passive,
}

fn content_risk(magic: &str, description: &str) -> ContentRisk {
    // Macro-enabled Office is only distinguishable from a plain document
    // via the description (`display_magic` collapses both to e.g. "Word
    // document"). Check it first so a macro doc never reads as Passive.
    if description.to_ascii_lowercase().contains("macro-enabled") {
        return ContentRisk::Macro;
    }
    let m = magic.to_ascii_lowercase();
    if m.contains("executable")
        || m.contains("dylib")
        || m.contains("pe /")
        || m.contains("java jar")
        || m.contains("java class")
        || m.contains("android apk")
        || m.contains("shortcut")
    {
        return ContentRisk::Executable;
    }
    if m.contains("script") {
        return ContentRisk::Script;
    }
    if m.contains("archive") || m == "binary" {
        return ContentRisk::ArchiveOrBinary;
    }
    ContentRisk::Passive
}

/// Coarse class of the *extension* claim, derived from the uppercased
/// extension that `describe_kind` stores in `display_kind`. Tells us what
/// the file is presenting itself as, so we can spot when the content is
/// more dangerous than the presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExtClass {
    /// The extension itself denotes runnable code or a shortcut, so
    /// executable/script content is honest, not a disguise.
    Code,
    /// A macro-enabled Office extension (`.docm` / `.xlsm` / …), where
    /// macros are expected rather than hidden.
    OfficeMacro,
    /// Image / audio / video.
    Media,
    /// A document the user opens to read, not to run.
    Document,
    /// Plain text, source, markup, or config.
    Text,
    /// A declared archive / disk image — archive content is honest here.
    Archive,
    /// A recognized extension that fits none of the above (opaque data).
    Opaque,
    /// No usable extension claim: `File` / `Folder` / `Symlink` / empty.
    Placeholder,
}

fn ext_class(kind: &str) -> ExtClass {
    let k = kind.trim().to_ascii_lowercase();
    if k.is_empty() || matches!(k.as_str(), "file" | "folder" | "symlink") {
        return ExtClass::Placeholder;
    }
    const CODE: &[&str] = &[
        "exe", "dll", "so", "dylib", "scr", "com", "bat", "cmd", "ps1", "psm1",
        "vbs", "vbe", "wsf", "wsh", "hta", "msi", "msix", "msp", "appx", "app",
        "pkg", "run", "jar", "apk", "class", "jnlp", "gadget", "sh", "bash",
        "zsh", "fish", "ksh", "csh", "command", "py", "pyw", "rb", "pl", "pm",
        "lua", "tcl", "ahk", "js", "mjs", "cjs", "ts", "lnk", "url", "desktop",
    ];
    const OFFICE_MACRO: &[&str] = &[
        "docm", "dotm", "xlsm", "xltm", "xlam", "pptm", "potm", "ppam", "ppsm",
    ];
    const MEDIA: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "bmp", "webp", "ico", "tif", "tiff",
        "heic", "heif", "svg", "mp4", "mov", "avi", "mkv", "webm", "m4v",
        "wmv", "flv", "mpg", "mpeg", "3gp", "mp3", "wav", "flac", "ogg",
        "oga", "m4a", "aac", "wma", "aiff", "opus",
    ];
    const DOCUMENT: &[&str] = &[
        "pdf", "doc", "docx", "dot", "dotx", "xls", "xlsx", "xlt", "xltx",
        "ppt", "pptx", "pps", "ppsx", "odt", "ods", "odp", "rtf", "epub",
        "pages", "key", "numbers",
    ];
    const TEXT: &[&str] = &[
        "txt", "md", "markdown", "rst", "log", "json", "xml", "yaml", "yml",
        "toml", "ini", "config", "cfg", "conf", "csv", "tsv", "html", "htm",
        "css", "scss", "tex", "srt", "vtt", "plist", "properties", "env",
    ];
    const ARCHIVE: &[&str] = &[
        "zip", "rar", "7z", "gz", "tgz", "bz2", "tbz2", "xz", "zst", "tar",
        "war", "ear", "cab", "iso", "dmg", "lz", "lzma",
    ];
    let has = |set: &[&str]| set.contains(&k.as_str());
    if has(OFFICE_MACRO) {
        ExtClass::OfficeMacro
    } else if has(CODE) {
        ExtClass::Code
    } else if has(MEDIA) {
        ExtClass::Media
    } else if has(DOCUMENT) {
        ExtClass::Document
    } else if has(TEXT) {
        ExtClass::Text
    } else if has(ARCHIVE) {
        ExtClass::Archive
    } else {
        ExtClass::Opaque
    }
}

/// Grade the relationship between an extension and the content actually
/// found inside the file. The danger tier ([`FormatFlag::Alert`]) is
/// reserved for genuine disguises — dangerous content wearing an innocent
/// extension. Benign disagreements fall to [`FormatFlag::Notice`], and
/// anything consistent (or lacking an extension claim) is
/// [`FormatFlag::None`].
fn classify_format(kind: &str, magic: &str, description: &str) -> FormatFlag {
    let ext = ext_class(kind);
    // No extension claim → nothing to contradict.
    if ext == ExtClass::Placeholder {
        return FormatFlag::None;
    }
    match content_risk(magic, description) {
        // Runnable code / scripts / shortcuts are honest only when the
        // extension itself advertises code. A `.exe` holding a PE is fine;
        // a `.jpg` holding one is a disguise.
        ContentRisk::Executable | ContentRisk::Script => {
            if ext == ExtClass::Code {
                FormatFlag::None
            } else {
                FormatFlag::Alert
            }
        }
        // Macros are expected only under a macro extension. Hidden in a
        // plain `.docx` / `.xlsx` / `.pptx`, they're the disguise.
        ContentRisk::Macro => {
            if ext == ExtClass::OfficeMacro {
                FormatFlag::None
            } else {
                FormatFlag::Alert
            }
        }
        // Archives are honest under an archive extension or the ZIP-wrapped
        // document family (docx / jar / apk / epub …, handled by
        // `formats_compatible`). Hidden inside something the user opens to
        // *view* — a picture, a document, a text file — it's smuggled.
        ContentRisk::ArchiveOrBinary => {
            if ext == ExtClass::Archive || formats_compatible(kind, magic) {
                FormatFlag::None
            } else if matches!(ext, ExtClass::Media | ExtClass::Document | ExtClass::Text) {
                FormatFlag::Alert
            } else {
                FormatFlag::Notice
            }
        }
        // Inert content: agreement is None, anything else is a benign
        // renamed/resaved file → the quiet cue.
        ContentRisk::Passive => {
            if formats_compatible(kind, magic) {
                FormatFlag::None
            } else {
                FormatFlag::Notice
            }
        }
    }
}

/// Normalize a format label for comparison. Strips common qualifier
/// words (`image`, `archive`, `document`, `file`, `data`), then maps
/// known aliases to a single canonical spelling so e.g. `JPG` and
/// `JPEG image` both reduce to `jpeg`. Pure ASCII so the lowercasing
/// is locale-independent.
fn normalize_format(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let stripped = lower
        .replace(" image", "")
        .replace(" archive", "")
        .replace(" document", "")
        .replace(" file", "")
        .replace(" data", "")
        .trim()
        .to_string();
    match stripped.as_str() {
        "jpg" | "jpeg" => "jpeg".to_string(),
        "tif" | "tiff" => "tiff".to_string(),
        "htm" | "html" => "html".to_string(),
        "mpg" | "mpeg" => "mpeg".to_string(),
        "yml" | "yaml" => "yaml".to_string(),
        "md" | "markdown" => "markdown".to_string(),
        "rs" | "rust" => "rust".to_string(),
        "py" | "python" => "python".to_string(),
        "js" | "javascript" => "javascript".to_string(),
        "ts" | "typescript" => "typescript".to_string(),
        _ => stripped,
    }
}

/// Current wall-clock time as whole seconds since the Unix epoch. Cheap
/// (a vDSO-backed clock read on the platforms we target), so it is safe to
/// call from the paint path — once per frame, or once per visible row.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Human-readable *relative* modification time: "just now", "4 seconds ago",
/// "3 min 30 sec ago", "2 hr 5 min ago", "3 days ago". Once a file is a week
/// or more old — where second-level precision stops being useful — it falls
/// back to an absolute date ("Mar 4", then "2026-05-01" past a year).
///
/// Computed against a caller-supplied `now_unix` rather than reading the clock
/// itself, so the UI can recompute it every frame and let the label tick
/// forward instead of freezing at enumerate time.
///
/// Why this is allowed on the paint path when sizes/dates are otherwise
/// pre-formatted: a *relative* duration is timezone-independent (`now - mtime`
/// is identical in every zone), so unlike absolute hour-of-day formatting it
/// needs no local-timezone machinery — it is pure integer arithmetic plus one
/// small allocation, bounded to the handful of on-screen rows.
pub fn humanize_mtime(mtime_unix: i64, now_unix: i64) -> String {
    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;
    const DAY: i64 = 86_400;
    const WEEK: i64 = 7 * DAY;
    const YEAR: i64 = 365 * DAY;

    let diff = now_unix - mtime_unix;

    // Future-stamped files (clock skew, or mtimes copied from an archive):
    // a tiny lead reads as "just now"; anything materially ahead shows its
    // date rather than a nonsensical negative "ago".
    if diff < 0 {
        return if diff > -MIN {
            "just now".to_string()
        } else {
            format_date(mtime_unix)
        };
    }
    if diff == 0 {
        return "just now".to_string();
    }
    if diff < MIN {
        return format!("{diff} second{} ago", plural(diff));
    }
    if diff < HOUR {
        let (m, s) = (diff / MIN, diff % MIN);
        return if s == 0 {
            format!("{m} min ago")
        } else {
            format!("{m} min {s} sec ago")
        };
    }
    if diff < DAY {
        let (h, m) = (diff / HOUR, (diff % HOUR) / MIN);
        return if m == 0 {
            format!("{h} hr ago")
        } else {
            format!("{h} hr {m} min ago")
        };
    }
    if diff < WEEK {
        let d = diff / DAY;
        return format!("{d} day{} ago", plural(d));
    }
    if diff < YEAR {
        return format_month_day(mtime_unix);
    }
    format_date(mtime_unix)
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Days-from-unix-epoch → (Y, M, D) via Howard Hinnant's `civil_from_days`.
fn ymd(unix: i64) -> (i32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn format_month_day(unix: i64) -> String {
    let (_, m, d) = ymd(unix);
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {}", NAMES[(m as usize - 1).min(11)], d)
}

fn format_date(unix: i64) -> String {
    let (y, m, d) = ymd(unix);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[cfg(test)]
mod time_tests {
    use super::*;

    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;
    const DAY: i64 = 86_400;

    // Anchor "now" at a fixed instant so the relative labels are
    // deterministic (the function never reads the clock itself).
    const NOW: i64 = 1_777_593_600; // 2026-05-01 00:00:00 UTC

    #[test]
    fn relative_labels_near_now() {
        assert_eq!(humanize_mtime(NOW, NOW), "just now");
        assert_eq!(humanize_mtime(NOW - 1, NOW), "1 second ago");
        assert_eq!(humanize_mtime(NOW - 4, NOW), "4 seconds ago");
        assert_eq!(humanize_mtime(NOW - 59, NOW), "59 seconds ago");
        // 3 min 30 sec — the user's example.
        assert_eq!(humanize_mtime(NOW - (3 * MIN + 30), NOW), "3 min 30 sec ago");
        // Whole minute drops the seconds component.
        assert_eq!(humanize_mtime(NOW - 5 * MIN, NOW), "5 min ago");
        assert_eq!(humanize_mtime(NOW - (2 * HOUR + 5 * MIN), NOW), "2 hr 5 min ago");
        assert_eq!(humanize_mtime(NOW - 3 * HOUR, NOW), "3 hr ago");
    }

    #[test]
    fn relative_labels_days_then_date_fallback() {
        assert_eq!(humanize_mtime(NOW - DAY, NOW), "1 day ago");
        assert_eq!(humanize_mtime(NOW - 3 * DAY, NOW), "3 days ago");
        // A week or more old falls back to the month/day label.
        assert_eq!(humanize_mtime(NOW - 8 * DAY, NOW), format_month_day(NOW - 8 * DAY));
        // Over a year old uses the full ISO date.
        assert_eq!(humanize_mtime(NOW - 400 * DAY, NOW), format_date(NOW - 400 * DAY));
    }

    #[test]
    fn future_stamps_are_handled() {
        // Small clock skew reads as "just now"; a real future date shows it.
        assert_eq!(humanize_mtime(NOW + 10, NOW), "just now");
        assert_eq!(humanize_mtime(NOW + 5 * DAY, NOW), format_date(NOW + 5 * DAY));
    }

    #[test]
    fn ymd_known_dates() {
        assert_eq!(ymd(1_777_593_600), (2026, 5, 1));
        assert_eq!(ymd(0), (1970, 1, 1));
    }
}

#[cfg(test)]
mod format_label_tests {
    use super::*;

    fn entry(kind: &str, magic: &str) -> FileEntry {
        FileEntry {
            id: NodeId(std::num::NonZeroU64::new(1).unwrap()),
            name: String::new(),
            display_name: String::new(),
            name_has_hazards: false,
            kind: EntryKind::File,
            size: 0,
            mtime_unix: 0,
            display_size: String::new(),
            display_kind: kind.into(),
            display_magic: magic.into(),
            display_description: String::new(),
            is_quarantined: false,
            quarantine: None,
            hidden: false,
        }
    }

    fn entry_desc(kind: &str, magic: &str, description: &str) -> FileEntry {
        let mut e = entry(kind, magic);
        e.display_description = description.into();
        e
    }

    fn flag(kind: &str, magic: &str) -> FormatFlag {
        entry(kind, magic).format_label().1
    }

    #[test]
    fn magic_is_primary() {
        let (label, flag) = entry("PNG", "PNG image").format_label();
        assert_eq!(label, "PNG image");
        assert_eq!(flag, FormatFlag::None);
    }

    #[test]
    fn empty_magic_falls_back_to_kind() {
        let (label, flag) = entry("PDF", "").format_label();
        assert_eq!(label, "PDF");
        assert_eq!(flag, FormatFlag::None);
    }

    // --- Agreement / terminology differences: never flagged. ---

    #[test]
    fn json_vs_plain_text_is_none() {
        assert_eq!(flag("JSON", "Plain text"), FormatFlag::None);
    }

    #[test]
    fn docx_vs_zip_archive_is_none() {
        assert_eq!(flag("DOCX", "ZIP archive"), FormatFlag::None);
    }

    #[test]
    fn jpg_vs_jpeg_image_is_none() {
        assert_eq!(flag("JPG", "JPEG image"), FormatFlag::None, "jpg ≡ jpeg");
    }

    #[test]
    fn tif_vs_tiff_image_is_none() {
        assert_eq!(flag("TIF", "TIFF image"), FormatFlag::None, "tif ≡ tiff");
    }

    #[test]
    fn htm_vs_html_is_none() {
        assert_eq!(flag("HTM", "HTML document"), FormatFlag::None, "htm ≡ html");
    }

    #[test]
    fn yml_vs_yaml_is_none() {
        assert_eq!(flag("YML", "YAML data"), FormatFlag::None, "yml ≡ yaml");
    }

    #[test]
    fn office_subtypes_are_none() {
        assert_eq!(flag("PPTX", "PowerPoint presentation"), FormatFlag::None);
        assert_eq!(flag("DOCX", "Word document"), FormatFlag::None);
        assert_eq!(flag("XLSX", "Excel spreadsheet"), FormatFlag::None);
    }

    #[test]
    fn pdf_vs_pdf_document_is_none() {
        assert_eq!(flag("PDF", "PDF document"), FormatFlag::None);
    }

    #[test]
    fn honest_executables_are_none() {
        // The extension already advertises code — not a disguise.
        assert_eq!(flag("EXE", "PE / DOS executable"), FormatFlag::None);
        assert_eq!(flag("DLL", "PE / DOS executable"), FormatFlag::None);
        assert_eq!(flag("SO", "ELF executable"), FormatFlag::None);
        assert_eq!(flag("PY", "Python script"), FormatFlag::None);
        assert_eq!(flag("SH", "Shell script"), FormatFlag::None);
        assert_eq!(flag("LNK", "Windows shortcut"), FormatFlag::None);
    }

    #[test]
    fn no_extension_executable_is_none() {
        // Bare unix binaries (kind="File") are normal, not disguises.
        assert_eq!(flag("File", "Mach-O executable"), FormatFlag::None);
        assert_eq!(flag("File", "ELF executable"), FormatFlag::None);
        assert_eq!(flag("Folder", "directory"), FormatFlag::None);
        assert_eq!(flag("Symlink", "symbolic link"), FormatFlag::None);
    }

    #[test]
    fn macro_doc_under_macro_extension_is_none() {
        // .docm/.xlsm expect macros — not hidden.
        assert_eq!(
            entry_desc("DOCM", "Word document", "Word document · macro-enabled")
                .format_label()
                .1,
            FormatFlag::None
        );
        assert_eq!(
            entry_desc("XLSM", "Excel spreadsheet", "Excel spreadsheet · macro-enabled")
                .format_label()
                .1,
            FormatFlag::None
        );
    }

    // --- Dangerous disguises: red alert. ---

    #[test]
    fn executable_under_image_extension_is_alert() {
        assert_eq!(flag("JPG", "PE / DOS executable"), FormatFlag::Alert);
        assert_eq!(flag("PNG", "Mach-O executable"), FormatFlag::Alert);
        assert_eq!(flag("GIF", "ELF executable"), FormatFlag::Alert);
    }

    #[test]
    fn script_under_innocent_extension_is_alert() {
        assert_eq!(flag("PNG", "Shell script"), FormatFlag::Alert);
        assert_eq!(flag("CSV", "Python script"), FormatFlag::Alert);
        assert_eq!(flag("TXT", "Shell script"), FormatFlag::Alert);
    }

    #[test]
    fn shortcut_under_image_extension_is_alert() {
        // A .lnk renamed to .jpg is a classic lure.
        assert_eq!(flag("JPG", "Windows shortcut"), FormatFlag::Alert);
    }

    #[test]
    fn hidden_macros_in_plain_office_doc_is_alert() {
        // Macros where the extension claims a plain document.
        assert_eq!(
            entry_desc("DOCX", "Word document", "Word document · macro-enabled")
                .format_label()
                .1,
            FormatFlag::Alert
        );
    }

    #[test]
    fn smuggled_archive_in_media_or_doc_is_alert() {
        // A picture/PDF/text file that is really an opaque archive/blob.
        assert_eq!(flag("JPG", "ZIP archive"), FormatFlag::Alert);
        assert_eq!(flag("PDF", "ZIP archive"), FormatFlag::Alert);
        assert_eq!(flag("PNG", "RAR archive"), FormatFlag::Alert);
        assert_eq!(flag("TXT", "Binary"), FormatFlag::Alert);
    }

    // --- Benign disagreements: quiet notice, never the danger tier. ---

    #[test]
    fn config_that_is_xml_is_notice_not_alert() {
        // The motivating case: a .config that's really XML is harmless.
        assert_eq!(flag("CONFIG", "XML"), FormatFlag::Notice);
    }

    #[test]
    fn png_saved_as_txt_is_notice() {
        // Used to be a red mismatch; an image under .txt isn't dangerous.
        assert_eq!(flag("TXT", "PNG image"), FormatFlag::Notice);
        assert_eq!(flag("DAT", "PNG image"), FormatFlag::Notice);
    }

    #[test]
    fn archive_under_opaque_extension_is_notice() {
        // .dat that's a ZIP isn't a disguise — .dat promises nothing.
        assert_eq!(flag("DAT", "ZIP archive"), FormatFlag::Notice);
    }
}

/// Display-ready provenance fields for a quarantined file. Strings are
/// pre-formatted in the worker so paint never allocates or parses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QuarantineDetails {
    /// Quarantining agent name from the `com.apple.quarantine` string —
    /// e.g. "Safari", "com.google.Chrome". `None` when the field was empty.
    pub agent: Option<String>,
    /// ISO-8601 download timestamp from the quarantine record. `None` when
    /// missing or unparseable.
    pub downloaded_iso: Option<String>,
    /// Source URLs from `kMDItemWhereFroms`. May be empty.
    pub where_from: Vec<String>,
}

/// Filesystem trait — implemented by `feraille-fs-native` (cross-platform std::fs)
/// and `feraille-shell-win32` (Windows shell namespace, PIDLs, virtual roots).
/// The UI talks to *this*, never to platform APIs directly.
pub trait FsBackend: Send + Sync {
    /// Begin an enumeration of `node`. The returned handle can be polled for
    /// streamed batches; the UI never blocks.
    fn enumerate(&self, node: NodeId) -> EnumerationHandle;
}

/// Why an enumeration failed to produce a complete listing. UI surfaces
/// this as an empty-state when `initial` is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumerationError {
    /// macOS TCC / Unix EACCES — the user can grant access via System
    /// Settings → Privacy & Security → Files and Folders (macOS) or by
    /// running with appropriate permissions (Linux).
    PermissionDenied,
    /// Path doesn't exist or has been moved/deleted.
    NotFound,
    /// Other I/O error. The string is a human-readable hint, not a
    /// programmable code.
    Other(String),
}

/// Opaque handle to a streamed enumeration. Real impl pushes batches over a
/// channel; the slice's stub returns one synchronous batch. `error` is
/// `Some` only on hard failure — partial listings are not currently
/// represented (would land alongside async enumeration).
pub struct EnumerationHandle {
    pub initial: Vec<FileEntry>,
    pub error: Option<EnumerationError>,
}

// (The old in-memory `AntTrail` struct lived here; superseded by the
// SQLite-backed heat in `feraille-meta` + `ProcessState::ant_visits`
// and deleted as dead code.)
