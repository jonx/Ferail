//! Structured filter expressions for the filter field.
//!
//! The filter box historically matched one case-insensitive substring
//! against the name and Format label. This module upgrades it to a
//! small query language while keeping bare words working exactly as
//! before: a query is whitespace-split into terms, `key:value` tokens
//! become typed predicates over `FileEntry` metadata, everything else
//! stays a substring term. All terms AND together.
//!
//! Supported tokens (also the source of truth for the filter box's
//! autocomplete — see [`TOKEN_HELP`]):
//!
//! - `kind:folder|file|link` — entry kind.
//! - `ext:rs` — file extension, case-insensitive, no dot.
//! - `size:>10mb` / `size:<1gb` / `size:1mb..100mb` — sizes use the
//!   same 1024-based units the Size column displays.
//! - `mod:` / `created:` — date predicates: `>2026-01-01`,
//!   `<=2026-06-30`, `2026-01-01..2026-03-31`, or the relative
//!   keywords `today`, `yesterday`, `week`, `month`, `year`.
//! - `locked:yes|no` — the immutable/read-only flag (Finder's
//!   "Locked" checkbox).
//! - `"quoted phrase"` — exact substring including spaces.
//!
//! A token whose value fails to parse (`size:banana`) degrades to a
//! plain substring term, so typing never makes the filter *lie* — at
//! worst it matches literally.
//!
//! Parsing is pure: relative dates are resolved against a caller-
//! supplied [`DateCtx`] (now + local timezone offset), so the module
//! stays platform-neutral and deterministic in tests. Evaluation reads
//! only in-memory `FileEntry` fields — no I/O, per the prime
//! directive; a metadata value the filesystem didn't provide
//! (`created_unix == None`) quietly fails the predicate.

use crate::{msgid, EntryKind, FileEntry};

/// Clock + zone context for resolving dates. `now_unix` is the current
/// unix time; `tz_offset_secs` is the local zone's offset from UTC in
/// seconds (positive east). Day boundaries ("today", `2026-01-01`) are
/// local-midnight boundaries computed from the pair.
#[derive(Clone, Copy, Debug)]
pub struct DateCtx {
    pub now_unix: i64,
    pub tz_offset_secs: i64,
}

/// One parsed term. All terms in an expression must match (AND).
#[derive(Clone, Debug, PartialEq)]
enum Term {
    /// Lowercased substring over the name and Format label.
    Text(String),
    Kind(EntryKind),
    /// Lowercased extension without the dot.
    Ext(String),
    Locked(bool),
    /// Inclusive byte bounds; `None` = unbounded on that side.
    Size {
        min: Option<u64>,
        max: Option<u64>,
    },
    /// Inclusive unix-seconds bounds on mtime.
    Modified {
        min: Option<i64>,
        max: Option<i64>,
    },
    /// Inclusive unix-seconds bounds on birth time.
    Created {
        min: Option<i64>,
        max: Option<i64>,
    },
}

/// A parsed filter query. Cheap to evaluate per row; parse once per
/// load, not per entry.
#[derive(Clone, Debug, Default)]
pub struct FilterExpr {
    terms: Vec<Term>,
}

impl FilterExpr {
    /// Parse `input` into an expression. Never fails: unparseable
    /// tokens degrade to substring terms. An empty/whitespace input
    /// yields an empty expression that matches everything.
    pub fn parse(input: &str, ctx: DateCtx) -> Self {
        let mut terms = Vec::new();
        for raw in split_terms(input) {
            terms.push(parse_term(&raw, ctx));
        }
        FilterExpr { terms }
    }

    /// No terms at all — matches every entry.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Does the expression carry any non-text (metadata) predicate?
    pub fn has_metadata_terms(&self) -> bool {
        self.terms.iter().any(|t| !matches!(t, Term::Text(_)))
    }

    /// The free-text part of the query, space-joined — what a
    /// name-index engine (Spotlight) should be queried with.
    pub fn text_needle(&self) -> String {
        let mut out = String::new();
        for t in &self.terms {
            if let Term::Text(s) = t {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(s);
            }
        }
        out
    }

    /// Do all text terms match this haystack? The caller supplies the
    /// haystack already lowercased (name, or name+format, or relative
    /// path — per surface).
    pub fn text_matches(&self, haystack_lower: &str) -> bool {
        self.terms.iter().all(|t| match t {
            Term::Text(needle) => haystack_lower.contains(needle),
            _ => true,
        })
    }

    /// Do all metadata (non-text) terms accept this entry?
    pub fn metadata_matches(&self, entry: &FileEntry) -> bool {
        self.terms.iter().all(|t| match t {
            Term::Text(_) => true,
            Term::Kind(kind) => entry.kind == *kind,
            Term::Ext(ext) => {
                !matches!(entry.kind, EntryKind::Directory)
                    && entry
                        .name
                        .rsplit_once('.')
                        .is_some_and(|(stem, e)| !stem.is_empty() && e.eq_ignore_ascii_case(ext))
            }
            Term::Locked(want) => entry.locked == *want,
            Term::Size { min, max } => {
                min.is_none_or(|lo| entry.size >= lo) && max.is_none_or(|hi| entry.size <= hi)
            }
            Term::Modified { min, max } => {
                entry.mtime_unix != 0
                    && min.is_none_or(|lo| entry.mtime_unix >= lo)
                    && max.is_none_or(|hi| entry.mtime_unix <= hi)
            }
            Term::Created { min, max } => entry
                .created_unix
                .is_some_and(|c| min.is_none_or(|lo| c >= lo) && max.is_none_or(|hi| c <= hi)),
        })
    }

    /// Full Tier-0 evaluation: text terms search the name plus the
    /// visible Format label (so "zip archive" still hits rows where
    /// the magic-detected text is the only match), metadata terms read
    /// the entry's cached fields.
    pub fn matches_entry(&self, entry: &FileEntry) -> bool {
        if self.terms.is_empty() {
            return true;
        }
        let text_ok = if self.terms.iter().any(|t| matches!(t, Term::Text(_))) {
            let (format, _) = entry.format_label();
            let haystack = format!("{} {}", entry.name.to_lowercase(), format.to_lowercase());
            self.text_matches(&haystack)
        } else {
            true
        };
        text_ok && self.metadata_matches(entry)
    }
}

/// Split the raw input into term strings: whitespace-separated, with
/// double-quoted phrases kept whole (quotes stripped).
fn split_terms(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_term(raw: &str, ctx: DateCtx) -> Term {
    let fallback = || Term::Text(raw.to_lowercase());
    let Some((key, value)) = raw.split_once(':') else {
        return fallback();
    };
    if value.is_empty() {
        return fallback();
    }
    match key.to_lowercase().as_str() {
        "kind" => match value.to_lowercase().as_str() {
            "folder" | "dir" | "directory" => Term::Kind(EntryKind::Directory),
            "file" => Term::Kind(EntryKind::File),
            "link" | "symlink" => Term::Kind(EntryKind::Symlink),
            _ => fallback(),
        },
        "ext" => Term::Ext(value.trim_start_matches('.').to_lowercase()),
        "locked" => match parse_bool(value) {
            Some(b) => Term::Locked(b),
            None => fallback(),
        },
        "size" => match parse_size_range(value) {
            Some((min, max)) => Term::Size { min, max },
            None => fallback(),
        },
        "mod" | "modified" => match parse_date_range(value, ctx) {
            Some((min, max)) => Term::Modified { min, max },
            None => fallback(),
        },
        "created" => match parse_date_range(value, ctx) {
            Some((min, max)) => Term::Created { min, max },
            None => fallback(),
        },
        _ => fallback(),
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Some(true),
        "no" | "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

/// `>10mb`, `<=1gb`, `10mb..1gb`, or a bare `10mb` (exact-ish: that
/// value or more, matching user intent of "about this big" poorly —
/// so bare means >=). Returns inclusive byte bounds.
fn parse_size_range(value: &str) -> Option<(Option<u64>, Option<u64>)> {
    if let Some((lo, hi)) = value.split_once("..") {
        let lo = parse_size(lo)?;
        let hi = parse_size(hi)?;
        return Some((Some(lo), Some(hi)));
    }
    if let Some(rest) = value.strip_prefix(">=") {
        return Some((Some(parse_size(rest)?), None));
    }
    if let Some(rest) = value.strip_prefix("<=") {
        return Some((None, Some(parse_size(rest)?)));
    }
    if let Some(rest) = value.strip_prefix('>') {
        return Some((Some(parse_size(rest)?.saturating_add(1)), None));
    }
    if let Some(rest) = value.strip_prefix('<') {
        return Some((None, Some(parse_size(rest)?.saturating_sub(1))));
    }
    Some((Some(parse_size(value)?), None))
}

/// `10mb` → bytes. Units are 1024-based to match the Size column
/// (`humanize_bytes`); accepts `b`, `k`/`kb`, `m`/`mb`, `g`/`gb`,
/// `t`/`tb`, and a bare number (bytes). Decimal values allowed
/// (`1.5gb`).
fn parse_size(value: &str) -> Option<u64> {
    let v = value.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    let split = v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let num: f64 = num.trim().parse().ok()?;
    if num < 0.0 {
        return None;
    }
    let mult: u64 = match unit.trim() {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1u64 << 40,
        _ => return None,
    };
    Some((num * mult as f64) as u64)
}

/// Date value forms:
/// - relative keywords: `today`, `yesterday`, `week` (last 7 days),
///   `month` (last 30 days), `year` (last 365 days);
/// - comparisons on a civil date: `>2026-01-01`, `>=…`, `<…`, `<=…`;
///   `>` means after that day *ends*, `>=` from the day's start,
///   symmetric for `<`/`<=`;
/// - inclusive ranges: `2026-01-01..2026-03-31`;
/// - a bare date: that whole local day.
///
/// Returns inclusive unix-seconds bounds.
fn parse_date_range(value: &str, ctx: DateCtx) -> Option<(Option<i64>, Option<i64>)> {
    const DAY: i64 = 86_400;
    let v = value.trim().to_lowercase();
    let today_start = local_day_start(ctx.now_unix, ctx.tz_offset_secs);
    match v.as_str() {
        "today" => return Some((Some(today_start), None)),
        "yesterday" => return Some((Some(today_start - DAY), Some(today_start - 1))),
        "week" => return Some((Some(ctx.now_unix - 7 * DAY), None)),
        "month" => return Some((Some(ctx.now_unix - 30 * DAY), None)),
        "year" => return Some((Some(ctx.now_unix - 365 * DAY), None)),
        _ => {}
    }
    if let Some((lo, hi)) = v.split_once("..") {
        let lo = civil_to_unix(lo, ctx.tz_offset_secs)?;
        let hi = civil_to_unix(hi, ctx.tz_offset_secs)?;
        return Some((Some(lo), Some(hi + DAY - 1)));
    }
    if let Some(rest) = v.strip_prefix(">=") {
        return Some((Some(civil_to_unix(rest, ctx.tz_offset_secs)?), None));
    }
    if let Some(rest) = v.strip_prefix("<=") {
        return Some((
            None,
            Some(civil_to_unix(rest, ctx.tz_offset_secs)? + DAY - 1),
        ));
    }
    if let Some(rest) = v.strip_prefix('>') {
        return Some((Some(civil_to_unix(rest, ctx.tz_offset_secs)? + DAY), None));
    }
    if let Some(rest) = v.strip_prefix('<') {
        return Some((None, Some(civil_to_unix(rest, ctx.tz_offset_secs)? - 1)));
    }
    let start = civil_to_unix(&v, ctx.tz_offset_secs)?;
    Some((Some(start), Some(start + DAY - 1)))
}

/// Unix time of the most recent local midnight before `now`.
fn local_day_start(now_unix: i64, tz_offset_secs: i64) -> i64 {
    let local = now_unix + tz_offset_secs;
    local.div_euclid(86_400) * 86_400 - tz_offset_secs
}

/// `YYYY-MM-DD` → unix seconds at that day's *local* midnight.
/// Howard Hinnant's days-from-civil, branchless and proleptic.
fn civil_to_unix(s: &str, tz_offset_secs: i64) -> Option<i64> {
    let mut it = s.trim().split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 - tz_offset_secs)
}

/// Help metadata for one filter token — drives the filter box's
/// autocomplete menu so the menu and the parser can't drift apart.
pub struct TokenHelp {
    /// The token key including the trailing colon, e.g. `"size:"`.
    pub key: &'static str,
    /// Ready-to-insert example values for the value menu. Empty when
    /// the value is freeform (`ext:`).
    pub values: &'static [&'static str],
    /// One-line description shown next to the key suggestion — a msgid;
    /// translate at the display site with `ferail_core::i18n::tr_raw`.
    pub detail: &'static str,
}

/// Every supported token, in menu order.
pub const TOKEN_HELP: &[TokenHelp] = &[
    TokenHelp {
        key: "kind:",
        values: &["folder", "file", "link"],
        detail: msgid!("entry kind — kind:folder, kind:file, kind:link"),
    },
    TokenHelp {
        key: "ext:",
        values: &[],
        detail: msgid!("file extension — ext:rs, ext:pdf"),
    },
    TokenHelp {
        key: "size:",
        values: &[">1mb", ">100mb", "<1mb", "1mb..100mb"],
        detail: msgid!("file size — size:>10mb, size:1mb..1gb"),
    },
    TokenHelp {
        key: "mod:",
        values: &["today", "yesterday", "week", "month", "year", ">2026-01-01"],
        detail: msgid!("modified date — mod:today, mod:>2026-01-01, mod:2026-01-01..2026-03-31"),
    },
    TokenHelp {
        key: "created:",
        values: &["today", "yesterday", "week", "month", "year", ">2026-01-01"],
        detail: msgid!("creation date — created:week, created:2026-01-01..2026-06-30"),
    },
    TokenHelp {
        key: "locked:",
        values: &["yes", "no"],
        detail: msgid!("locked (immutable / read-only) flag — locked:yes"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;

    // Fixed clock: 2026-08-12 12:00:00 UTC, zone UTC+2 (local 14:00).
    const NOW: i64 = 1_786_536_000;
    const TZ: i64 = 7_200;
    const CTX: DateCtx = DateCtx {
        now_unix: NOW,
        tz_offset_secs: TZ,
    };

    fn entry(name: &str, kind: EntryKind, size: u64, mtime: i64) -> FileEntry {
        FileEntry {
            id: NodeId::from_raw(1).unwrap(),
            name: name.into(),
            display_name: name.into(),
            name_has_hazards: false,
            kind,
            size,
            mtime_unix: mtime,
            display_size: crate::empty_entry_text(),
            display_kind: crate::empty_entry_text(),
            display_magic: crate::empty_entry_text(),
            display_description: crate::empty_entry_text(),
            details_loaded: false,
            is_quarantined: false,
            quarantine: None,
            hidden: false,
            created_unix: None,
            locked: false,
        }
    }

    #[test]
    fn bare_words_and_quotes() {
        let e = FilterExpr::parse("my report", CTX);
        assert!(e.matches_entry(&entry("My Report.txt", EntryKind::File, 1, 1)));
        // AND semantics: both words must appear.
        assert!(!e.matches_entry(&entry("report.txt", EntryKind::File, 1, 1)));
        // Quoted phrase is a single exact substring.
        let e = FilterExpr::parse("\"y r\"", CTX);
        assert!(e.matches_entry(&entry("My Report.txt", EntryKind::File, 1, 1)));
        assert!(!e.matches_entry(&entry("Report by me.txt", EntryKind::File, 1, 1)));
    }

    #[test]
    fn kind_and_ext() {
        let e = FilterExpr::parse("kind:folder", CTX);
        assert!(e.matches_entry(&entry("src", EntryKind::Directory, 0, 1)));
        assert!(!e.matches_entry(&entry("src.txt", EntryKind::File, 0, 1)));

        let e = FilterExpr::parse("ext:RS", CTX);
        assert!(e.matches_entry(&entry("main.rs", EntryKind::File, 1, 1)));
        assert!(!e.matches_entry(&entry("main.rst", EntryKind::File, 1, 1)));
        // A folder named like an extension never matches ext:.
        assert!(!e.matches_entry(&entry("dir.rs", EntryKind::Directory, 0, 1)));
        // Dotfile with no real extension.
        assert!(!e.matches_entry(&entry(".rs", EntryKind::File, 1, 1)));
    }

    #[test]
    fn size_ranges() {
        let e = FilterExpr::parse("size:>1kb", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1025, 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1024, 1)));

        let e = FilterExpr::parse("size:<=1kb", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1024, 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1025, 1)));

        let e = FilterExpr::parse("size:1kb..1mb", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 4096, 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 100, 1)));
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1 << 20, 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, (1 << 20) + 1, 1)));

        // Decimal + bare number.
        let e = FilterExpr::parse("size:>=1.5kb", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1536, 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1535, 1)));
        let e = FilterExpr::parse("size:100", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 100, 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 99, 1)));
    }

    #[test]
    fn locked_flag() {
        let mut locked = entry("a", EntryKind::File, 1, 1);
        locked.locked = true;
        let e = FilterExpr::parse("locked:yes", CTX);
        assert!(e.matches_entry(&locked));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, 1)));
        let e = FilterExpr::parse("locked:no", CTX);
        assert!(!e.matches_entry(&locked));
    }

    #[test]
    fn date_absolute_bounds() {
        // 2026-08-01 local midnight at UTC+2 = 2026-07-31T22:00:00Z.
        let day_start = civil_to_unix("2026-08-01", TZ).unwrap();
        assert_eq!(day_start, 1_785_535_200);

        // Bare date = that whole local day.
        let e = FilterExpr::parse("mod:2026-08-01", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, day_start)));
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, day_start + 86_399)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, day_start + 86_400)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, day_start - 1)));

        // > excludes the named day; >= includes it.
        let e = FilterExpr::parse("mod:>2026-08-01", CTX);
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, day_start + 86_399)));
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, day_start + 86_400)));
        let e = FilterExpr::parse("mod:>=2026-08-01", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, day_start)));

        // Range is inclusive of both endpoint days.
        let e = FilterExpr::parse("mod:2026-08-01..2026-08-02", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, day_start + 2 * 86_400 - 1)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, day_start + 2 * 86_400)));
    }

    #[test]
    fn date_relative() {
        let today_start = local_day_start(NOW, TZ);
        let e = FilterExpr::parse("mod:today", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, today_start + 60)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, today_start - 60)));

        let e = FilterExpr::parse("mod:yesterday", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, today_start - 60)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, today_start + 60)));

        let e = FilterExpr::parse("mod:week", CTX);
        assert!(e.matches_entry(&entry("a", EntryKind::File, 1, NOW - 6 * 86_400)));
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, NOW - 8 * 86_400)));
    }

    #[test]
    fn created_requires_value() {
        let e = FilterExpr::parse("created:week", CTX);
        // Missing birth time fails quietly rather than matching.
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, NOW)));
        let mut with_created = entry("a", EntryKind::File, 1, NOW);
        with_created.created_unix = Some(NOW - 3600);
        assert!(e.matches_entry(&with_created));
    }

    #[test]
    fn unknown_or_bad_tokens_degrade_to_text() {
        // Unknown key: literal substring.
        let e = FilterExpr::parse("smart:yes", CTX);
        assert!(e.matches_entry(&entry("smart:yes plan.txt", EntryKind::File, 1, 1)));
        // Bad value on a known key: also literal.
        let e = FilterExpr::parse("size:banana", CTX);
        assert!(!e.matches_entry(&entry("a", EntryKind::File, 1, 1)));
        assert!(e.matches_entry(&entry("size:banana", EntryKind::File, 1, 1)));
        // Trailing colon with no value: literal.
        let e = FilterExpr::parse("size:", CTX);
        assert!(e.matches_entry(&entry("size: notes", EntryKind::File, 1, 1)));
    }

    #[test]
    fn mixed_query() {
        let e = FilterExpr::parse("report ext:pdf size:>1kb mod:week", CTX);
        let mut hit = entry("Q3 Report.pdf", EntryKind::File, 2048, NOW - 86_400);
        assert!(e.matches_entry(&hit));
        hit.size = 100;
        assert!(!e.matches_entry(&hit));
        assert!(e.has_metadata_terms());
        assert_eq!(e.text_needle(), "report");
    }

    #[test]
    fn empty_matches_everything() {
        let e = FilterExpr::parse("   ", CTX);
        assert!(e.is_empty());
        assert!(e.matches_entry(&entry("anything", EntryKind::File, 1, 1)));
    }

    #[test]
    fn token_help_keys_are_parsed_keys() {
        // Every advertised key must round-trip through the parser as a
        // structured term (not degrade to text).
        for help in TOKEN_HELP {
            let probe = match *help {
                TokenHelp { key: "ext:", .. } => "ext:rs".to_string(),
                TokenHelp { key, values, .. } => format!("{key}{}", values[0]),
            };
            let e = FilterExpr::parse(&probe, CTX);
            assert!(
                e.has_metadata_terms(),
                "token {probe:?} degraded to plain text"
            );
        }
    }
}
