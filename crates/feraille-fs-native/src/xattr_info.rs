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

use std::path::Path;

use feraille_core::QuarantineDetails;

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
        Self { quarantined: false, agent: None, downloaded_at: None, where_from: Vec::new() }
    }
}

/// Read the macOS quarantine + where-from xattrs for `path`.
///
/// Never panics: missing attrs and parse failures yield default values.
/// The returned `quarantined` flag reflects only whether the
/// `com.apple.quarantine` attribute is present — agent / timestamp
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
/// 4=Restricted. We treat 3+ as "quarantined" — the cases Windows
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

    let mut zone_id: Option<i32> = None;
    let mut host_url: Option<String> = None;
    let mut referrer: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("ZoneId=") {
            zone_id = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("HostUrl=") {
            let s = rest.trim();
            if !s.is_empty() {
                host_url = Some(s.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("ReferrerUrl=") {
            let s = rest.trim();
            if !s.is_empty() {
                referrer = Some(s.to_string());
            }
        }
    }

    // Treat Internet (3) and Restricted (4) as quarantined — that's
    // what Explorer's "this came from another computer" notice keys
    // on. Local / Intranet / Trusted aren't flagged.
    let internet_or_restricted = matches!(zone_id, Some(3) | Some(4));
    if !internet_or_restricted && host_url.is_none() && referrer.is_none() {
        // ADS present but contains nothing actionable — leave as
        // not-quarantined.
        return info;
    }

    info.quarantined = internet_or_restricted;

    // "Agent" doesn't map cleanly on Windows; the closest thing is
    // the host URL's domain. Browsers that write Zone.Identifier
    // (Edge, Chrome, Firefox) don't all populate HostUrl, but when
    // present it's the most useful "downloaded from" label.
    info.agent = host_url
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

    if let Some(h) = host_url {
        info.where_from.push(h);
    }
    if let Some(r) = referrer {
        if !info.where_from.contains(&r) {
            info.where_from.push(r);
        }
    }

    info
}

#[cfg(windows)]
fn parse_url_host(url: &str) -> Option<&str> {
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

#[cfg(not(any(target_os = "macos", windows)))]
pub fn fetch_quarantine_info(_path: &Path) -> QuarantineInfo {
    QuarantineInfo::empty()
}

/// Format a unix timestamp into a minute-resolution ISO-8601 string in UTC.
/// Kept here (rather than in `feraille-core`) because it's only used by the
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
}
