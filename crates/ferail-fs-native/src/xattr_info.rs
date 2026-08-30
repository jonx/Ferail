//! macOS quarantine / "downloaded from" extended attributes.
//!
//! Reads `com.apple.quarantine` and `com.apple.metadata:kMDItemWhereFroms`
//! off-thread for one path at a time. Designed to be called by a worker;
//! never invoke from paint or hit-test.
//!
//! The quarantine string is the macOS analogue of Windows' Mark-of-the-Web.
//! Format: four semicolon-separated ASCII fields:
//!   `flags;hex_seconds_since_2001;agent_name;event_uuid`
//! e.g. `0083;6649e000;Safari;ABCDEF12-3456-7890-ABCD-EF1234567890`.
//!
//! `kMDItemWhereFroms` is a binary plist holding an array of NSString URLs.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use ferail_core::QuarantineDetails;

/// Raw xattr read result. Display formatting happens at the call site so
/// the host can decide ISO format / locale rules.
pub struct QuarantineInfo {
    pub quarantined: bool,
    pub agent: Option<String>,
    pub downloaded_at: Option<i64>,
    pub where_from: Vec<String>,
}

impl QuarantineInfo {
    pub fn empty() -> Self {
        Self {
            quarantined: false,
            agent: None,
            downloaded_at: None,
            where_from: Vec::new(),
        }
    }
}

/// Read the macOS quarantine + where-from xattrs for `path`.
///
/// Never panics: missing attrs and parse failures yield default values.
/// The returned `quarantined` flag reflects only whether the
/// `com.apple.quarantine` attribute is present: agent / timestamp
/// fields may still be `None` even when `quarantined` is true.
#[cfg(target_os = "macos")]
pub fn fetch_quarantine_info(path: &Path) -> QuarantineInfo {
    let q_bytes = xattr::get(path, "com.apple.quarantine").ok().flatten();
    let where_bytes = xattr::get(path, "com.apple.metadata:kMDItemWhereFroms")
        .ok()
        .flatten();

    let mut info = QuarantineInfo::empty();
    if let Some(bytes) = q_bytes {
        info.quarantined = true;
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let parts: Vec<&str> = s.splitn(4, ';').collect();
            if parts.len() >= 2 {
                if let Ok(secs_since_2001) = i64::from_str_radix(parts[1].trim(), 16) {
                    // macOS quarantine uses seconds since 2001-01-01 UTC
                    // (CFAbsoluteTime epoch). Offset to 1970-01-01 unix epoch.
                    const EPOCH_DELTA_2001_TO_1970: i64 = 978_307_200;
                    info.downloaded_at = Some(secs_since_2001 + EPOCH_DELTA_2001_TO_1970);
                }
            }
            if parts.len() >= 3 {
                let agent = parts[2].trim();
                if !agent.is_empty() {
                    info.agent = Some(agent.to_string());
                }
            }
        }
    }

    if let Some(bytes) = where_bytes {
        if let Ok(urls) = plist::from_bytes::<Vec<String>>(&bytes) {
            info.where_from = urls.into_iter().filter(|u| !u.is_empty()).collect();
        }
    }

    info
}

/// Windows arm: read the NTFS Alternate Data Stream
/// `<file>:Zone.Identifier`, which Windows writes whenever a file
/// arrives from the Internet zone (downloaded via a browser, copied
/// from an email attachment, extracted from an archive flagged as
/// Internet, etc.). Format is a tiny INI:
///
/// ```text
/// [ZoneTransfer]
/// ZoneId=3
/// ReferrerUrl=https://example.com/
/// HostUrl=https://example.com/file.zip
/// ```
///
/// ZoneId values: 0=Local, 1=Intranet, 2=Trusted, 3=Internet,
/// 4=Restricted. We treat 3+ as "quarantined": the cases Windows
/// itself flags in Explorer's Security tab.
///
/// File timestamps don't live in the ADS; the file's own creation
/// time is the best proxy and the UI displays it as the
/// "downloaded at" value.
#[cfg(windows)]
pub fn fetch_quarantine_info(path: &Path) -> QuarantineInfo {
    let mut info = QuarantineInfo::empty();

    // The stream is opened by appending `:Zone.Identifier` to the
    // path. `std::fs::read` routes through CreateFileW on Windows
    // which recognizes ADS syntax.
    let mut ads_path = path.as_os_str().to_os_string();
    ads_path.push(":Zone.Identifier");
    let Ok(bytes) = std::fs::read(&ads_path) else {
        return info;
    };
    let text = String::from_utf8_lossy(&bytes);
    let zt = parse_zone_identifier(&text);

    if !zt.internet_or_restricted() && zt.host_url.is_none() && zt.referrer.is_none() {
        // ADS present but contains nothing actionable: leave as
        // not-quarantined.
        return info;
    }

    info.quarantined = zt.internet_or_restricted();

    // "Agent" doesn't map cleanly on Windows; the closest thing is
    // the host URL's domain. Browsers that write Zone.Identifier
    // (Edge, Chrome, Firefox) don't all populate HostUrl, but when
    // present it's the most useful "downloaded from" label.
    info.agent = zt
        .host_url
        .as_deref()
        .and_then(parse_url_host)
        .map(str::to_string);

    // Downloaded-at proxy: the file's creation time. NTFS preserves
    // this even when the file is moved within the same volume.
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(ctime) = meta.created() {
            if let Ok(d) = ctime.duration_since(std::time::UNIX_EPOCH) {
                info.downloaded_at = Some(d.as_secs() as i64);
            }
        }
    }

    if let Some(h) = zt.host_url {
        info.where_from.push(h);
    }
    if let Some(r) = zt.referrer {
        if !info.where_from.contains(&r) {
            info.where_from.push(r);
        }
    }

    info
}

/// Remove the Mark-of-the-Web AND its provenance record from `path`.
///
/// macOS: deletes `com.apple.quarantine` (the Gatekeeper mark) and
/// `com.apple.metadata:kMDItemWhereFroms` (the downloaded-from URLs).
/// Windows: deletes the `Zone.Identifier` alternate data stream,
/// which holds both the zone mark and the Host/Referrer URLs.
///
/// Idempotent: clearing a file that carries no mark succeeds. Callers
/// run this on a worker (it's metadata I/O) and must also scrub any
/// cached quarantine state (e.g. ferail-meta rows) so a later
/// prefetch doesn't resurrect the badge from cache.
#[cfg(target_os = "macos")]
pub fn clear_quarantine(path: &Path) -> std::io::Result<()> {
    clear_quarantine_status(path).map(|_| ())
}

#[cfg(target_os = "macos")]
fn clear_quarantine_status(path: &Path) -> std::io::Result<bool> {
    let mut result = Ok(());
    let mut removed = false;
    for attr in [
        "com.apple.quarantine",
        "com.apple.metadata:kMDItemWhereFroms",
    ] {
        // Only attempt removal when present: xattr::remove on a
        // missing attr returns ENOATTR, which isn't a failure for us.
        if let Ok(Some(_)) = xattr::get(path, attr) {
            if let Err(e) = xattr::remove(path, attr) {
                result = Err(e);
            } else {
                removed = true;
            }
        }
    }
    result.map(|()| removed)
}

#[cfg(windows)]
pub fn clear_quarantine(path: &Path) -> std::io::Result<()> {
    clear_quarantine_status(path).map(|_| ())
}

#[cfg(windows)]
fn clear_quarantine_status(path: &Path) -> std::io::Result<bool> {
    let mut ads_path = path.as_os_str().to_os_string();
    ads_path.push(":Zone.Identifier");
    match std::fs::remove_file(&ads_path) {
        Ok(()) => Ok(true),
        // No stream == nothing to clear; idempotent success.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Linux: drop the freedesktop provenance xattrs (the "Unblock" equivalent).
/// Best-effort and idempotent: a missing attribute is success.
#[cfg(target_os = "linux")]
pub fn clear_quarantine(path: &Path) -> std::io::Result<()> {
    clear_quarantine_status(path).map(|_| ())
}

#[cfg(target_os = "linux")]
fn clear_quarantine_status(path: &Path) -> std::io::Result<bool> {
    let mut removed = false;
    for attr in ["user.xdg.origin.url", "user.xdg.referrer.url"] {
        match xattr::remove(path, attr) {
            Ok(()) => removed = true,
            // Missing attr (ENODATA/ENOATTR) or unsupported FS, nothing to do.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
    Ok(removed)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn clear_quarantine(_path: &Path) -> std::io::Result<()> {
    // No quarantine concept on this platform.
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn clear_quarantine_status(_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// Result of a recursive Mark-of-the-Web removal. Failures are retained so
/// the caller can present the same per-item report as other file operations;
/// no successful path is accumulated, keeping memory bounded for deep trees.
#[derive(Debug, Default)]
pub struct ClearQuarantineTreeSummary {
    pub visited: u64,
    pub cleared: u64,
    pub failure_count: u64,
    pub failures: Vec<(PathBuf, std::io::Error)>,
    pub cancelled: bool,
}

impl ClearQuarantineTreeSummary {
    fn record_failure(&mut self, path: PathBuf, error: std::io::Error) {
        self.failure_count = self.failure_count.saturating_add(1);
        // A damaged or permission-hostile tree must not turn an error report
        // into another unbounded result stream.
        if self.failures.len() < 256 {
            self.failures.push((path, error));
        }
    }
}

/// Clear quarantine metadata from `root` and, when it is a real directory,
/// every descendant. Symlinks and Windows reparse points are skipped rather
/// than followed; macOS packages are ordinary directories here by design.
///
/// `on_cleared` is invoked only when a mark was actually removed, allowing
/// metadata caches to be scrubbed in bounded batches without retaining
/// millions of clean paths.
pub fn clear_quarantine_tree(
    root: &Path,
    cancel: &AtomicBool,
    mut on_cleared: impl FnMut(&Path),
) -> ClearQuarantineTreeSummary {
    let mut summary = ClearQuarantineTreeSummary::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            summary.cancelled = true;
            break;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                summary.record_failure(path, error);
                continue;
            }
        };
        if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) {
            continue;
        }
        summary.visited = summary.visited.saturating_add(1);
        match clear_quarantine_status(&path) {
            Ok(true) => {
                summary.cleared = summary.cleared.saturating_add(1);
                on_cleared(&path);
            }
            Ok(false) => {}
            Err(error) => summary.record_failure(path.clone(), error),
        }
        if !metadata.is_dir() {
            continue;
        }
        match std::fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => pending.push(entry.path()),
                        Err(error) => summary.record_failure(path.clone(), error),
                    }
                }
            }
            Err(error) => summary.record_failure(path, error),
        }
    }
    summary
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Parsed view of a `Zone.Identifier` ADS body. Compiled on every
/// platform (pure string parsing) so the logic is unit-testable from
/// the macOS dev/CI hosts even though only the Windows arm reads it
/// from disk.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ZoneTransfer {
    pub zone_id: Option<i32>,
    pub host_url: Option<String>,
    pub referrer: Option<String>,
}

impl ZoneTransfer {
    /// Internet (3) and Restricted (4) are what Explorer's "this file
    /// came from another computer" notice keys on; Local / Intranet /
    /// Trusted aren't flagged.
    pub fn internet_or_restricted(&self) -> bool {
        matches!(self.zone_id, Some(3) | Some(4))
    }
}

/// Parse the tiny INI-ish `[ZoneTransfer]` body. Tolerant by design:
/// unknown lines ignored, values trimmed (CRLF endings handled by
/// `str::lines`), empty values treated as absent, malformed ZoneId
/// treated as absent.
pub fn parse_zone_identifier(text: &str) -> ZoneTransfer {
    let mut zt = ZoneTransfer::default();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ZoneId=") {
            zt.zone_id = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("HostUrl=") {
            let s = rest.trim();
            if !s.is_empty() {
                zt.host_url = Some(s.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("ReferrerUrl=") {
            let s = rest.trim();
            if !s.is_empty() {
                zt.referrer = Some(s.to_string());
            }
        }
    }
    zt
}

/// Extract the host from a URL without pulling a URL crate: strip a
/// known scheme prefix, cut at the first delimiter. Pure; compiled
/// everywhere for testability (only the Windows arm calls it today).
pub fn parse_url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ftp://"))
        .unwrap_or(url);
    let end = rest.find(['/', '?', '#', ':']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Linux: freedesktop download provenance. Browsers (Firefox, Chromium, …)
/// set the `user.xdg.origin.url` xattr (and optionally `user.xdg.referrer.url`)
/// on downloaded files: the Linux analogue of macOS quarantine / Windows
/// Mark-of-the-Web. Presence of an origin URL marks the file as "downloaded".
/// There's no agent name or timestamp in this scheme, so those stay `None`.
#[cfg(target_os = "linux")]
pub fn fetch_quarantine_info(path: &Path) -> QuarantineInfo {
    let read = |attr: &str| -> Option<String> {
        xattr::get(path, attr)
            .ok()
            .flatten()
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim_end_matches('\0').trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let origin = read("user.xdg.origin.url");
    let referrer = read("user.xdg.referrer.url");

    let mut where_from = Vec::new();
    if let Some(o) = &origin {
        where_from.push(o.clone());
    }
    if let Some(r) = &referrer {
        if origin.as_ref() != Some(r) {
            where_from.push(r.clone());
        }
    }
    QuarantineInfo {
        quarantined: origin.is_some(),
        agent: None,
        downloaded_at: None,
        where_from,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn fetch_quarantine_info(_path: &Path) -> QuarantineInfo {
    QuarantineInfo::empty()
}

/// Format a unix timestamp into a minute-resolution ISO-8601 string in UTC.
/// Kept here (rather than in `ferail-core`) because it's only used by the
/// quarantine path; the host has its own date-formatter for mtime rows.
pub fn format_iso_minute_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_in_day = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_in_day / 3600;
    let minute = (secs_in_day % 3600) / 60;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        year, month, day, hour, minute
    )
}

// Howard Hinnant's civil-from-days algorithm (public domain), restricted to
// proleptic Gregorian. Avoids pulling in `chrono` for one timestamp.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Convert a `QuarantineInfo` into the display-ready `QuarantineDetails`
/// the UI actually consumes. None-when-empty rules live here so callers
/// don't have to repeat them.
pub fn details_from(info: &QuarantineInfo) -> QuarantineDetails {
    QuarantineDetails {
        agent: info.agent.clone(),
        downloaded_iso: info.downloaded_at.map(format_iso_minute_utc),
        where_from: info.where_from.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux download-provenance round-trip: a browser-style
    /// `user.xdg.origin.url` xattr is read as "downloaded", surfaced in
    /// where_from, and cleared by clear_quarantine. Skips when the temp
    /// filesystem doesn't support user xattrs (e.g. some tmpfs mounts).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_provenance_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ferail-prov-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("download.bin");
        std::fs::write(&f, b"x").unwrap();

        if xattr::set(&f, "user.xdg.origin.url", b"https://example.com/file.bin").is_err() {
            eprintln!("skip: filesystem does not support user xattrs");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let _ = xattr::set(&f, "user.xdg.referrer.url", b"https://example.com/");

        let info = fetch_quarantine_info(&f);
        assert!(info.quarantined, "origin url marks it downloaded");
        assert_eq!(info.where_from[0], "https://example.com/file.bin");
        assert!(info
            .where_from
            .contains(&"https://example.com/".to_string()));

        clear_quarantine(&f).unwrap();
        assert!(
            !fetch_quarantine_info(&f).quarantined,
            "unblock removed the provenance"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn iso_minute_known_epoch() {
        assert_eq!(format_iso_minute_utc(0), "1970-01-01 00:00 UTC");
    }

    #[test]
    fn iso_minute_round_value() {
        // 2024-05-19 00:00 UTC = 1716076800
        assert_eq!(format_iso_minute_utc(1_716_076_800), "2024-05-19 00:00 UTC");
    }

    #[test]
    fn details_from_empty_info() {
        let d = details_from(&QuarantineInfo::empty());
        assert!(d.agent.is_none());
        assert!(d.downloaded_iso.is_none());
        assert!(d.where_from.is_empty());
    }

    #[test]
    fn details_from_filled_info() {
        let info = QuarantineInfo {
            quarantined: true,
            agent: Some("Safari".into()),
            downloaded_at: Some(1_716_076_800),
            where_from: vec!["https://example.com/x".into()],
        };
        let d = details_from(&info);
        assert_eq!(d.agent.as_deref(), Some("Safari"));
        assert_eq!(d.downloaded_iso.as_deref(), Some("2024-05-19 00:00 UTC"));
        assert_eq!(d.where_from, vec!["https://example.com/x".to_string()]);
    }

    // ---- Zone.Identifier parsing (Windows Mark-of-the-Web) ----
    // The parser is pure and compiled on every platform; these run
    // on macOS CI even though only the Windows arm reads ADS data.

    #[test]
    fn zone_identifier_typical_browser_download() {
        // CRLF endings, exactly as Edge/Chrome write them.
        let text = "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/page\r\nHostUrl=https://cdn.example.com/file.zip\r\n";
        let zt = parse_zone_identifier(text);
        assert_eq!(zt.zone_id, Some(3));
        assert!(zt.internet_or_restricted());
        assert_eq!(
            zt.host_url.as_deref(),
            Some("https://cdn.example.com/file.zip")
        );
        assert_eq!(zt.referrer.as_deref(), Some("https://example.com/page"));
    }

    #[test]
    fn zone_identifier_trusted_zone_not_flagged() {
        let zt = parse_zone_identifier("[ZoneTransfer]\nZoneId=2\n");
        assert_eq!(zt.zone_id, Some(2));
        assert!(!zt.internet_or_restricted());
    }

    #[test]
    fn zone_identifier_tolerates_garbage_and_gaps() {
        // Missing ZoneId, unknown keys, empty values, no section header.
        let zt = parse_zone_identifier("Garbage\nHostUrl=\nReferrerUrl=https://r.example\nFoo=Bar");
        assert_eq!(zt.zone_id, None);
        assert!(!zt.internet_or_restricted());
        assert_eq!(zt.host_url, None); // empty value → absent
        assert_eq!(zt.referrer.as_deref(), Some("https://r.example"));
        // Malformed ZoneId → absent, not a panic.
        let zt = parse_zone_identifier("ZoneId=banana");
        assert_eq!(zt.zone_id, None);
        // Empty input.
        assert_eq!(parse_zone_identifier(""), ZoneTransfer::default());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clear_quarantine_removes_mark_and_provenance() {
        use std::path::PathBuf;
        let dir = std::env::temp_dir();
        let file: PathBuf = dir.join(format!("ferail-quarantine-test-{}", std::process::id()));
        std::fs::write(&file, b"payload").unwrap();
        // Plant a realistic quarantine record + where-froms plist.
        xattr::set(
            &file,
            "com.apple.quarantine",
            b"0083;6649e000;Safari;ABCDEF12-3456-7890-ABCD-EF1234567890",
        )
        .unwrap();
        let urls = vec!["https://example.com/file.zip".to_string()];
        let mut plist_bytes = Vec::new();
        plist::to_writer_binary(&mut plist_bytes, &urls).unwrap();
        xattr::set(&file, "com.apple.metadata:kMDItemWhereFroms", &plist_bytes).unwrap();

        let before = fetch_quarantine_info(&file);
        assert!(before.quarantined);
        assert_eq!(before.where_from, urls);

        clear_quarantine(&file).unwrap();
        let after = fetch_quarantine_info(&file);
        assert!(!after.quarantined);
        assert!(after.where_from.is_empty());

        // Idempotent on an already-clean file.
        clear_quarantine(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
    }

    #[test]
    fn recursive_clear_walks_real_directories_and_is_cancellable() {
        let root = std::env::temp_dir().join(format!(
            "ferail-quarantine-tree-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let nested = root.join("Package.app").join("Contents");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("payload.bin"), b"payload").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();

        let cancel = AtomicBool::new(false);
        let mut visited = Vec::new();
        let summary =
            clear_quarantine_tree(&root, &cancel, |path| visited.push(path.to_path_buf()));
        assert!(!summary.cancelled);
        assert!(summary.failures.is_empty(), "{:?}", summary.failures);
        assert_eq!(summary.visited, 4); // root, Package.app, Contents, payload
        assert_eq!(summary.cleared, 0);
        assert!(visited.is_empty());

        let cancelled = AtomicBool::new(true);
        let summary = clear_quarantine_tree(&root, &cancelled, |_| {});
        assert!(summary.cancelled);
        assert_eq!(summary.cleared, 0);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn url_host_extraction() {
        assert_eq!(
            parse_url_host("https://cdn.example.com/a/b.zip"),
            Some("cdn.example.com")
        );
        assert_eq!(
            parse_url_host("http://example.com:8080/x"),
            Some("example.com")
        );
        assert_eq!(
            parse_url_host("ftp://files.example.org"),
            Some("files.example.org")
        );
        assert_eq!(parse_url_host("bare-host/path"), Some("bare-host"));
        assert_eq!(parse_url_host("https://"), None);
        assert_eq!(parse_url_host(""), None);
    }
}
