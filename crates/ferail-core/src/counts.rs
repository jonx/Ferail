//! Display formatting for counts of things — files, folders, items,
//! entries, matches, duplicate groups.
//!
//! Ferail shows counts that routinely run into the millions (a disk-usage
//! scan reports every file it walked), and `1104619` is unreadable at a
//! glance. Every user-visible count is therefore grouped with `.` every
//! three digits — `1.104.619` — the separator the UI settled on for the
//! status bar, applied everywhere so the same number reads the same way
//! in the footer, a scan header, a toast, and a task row.
//!
//! Two entry points, because count strings are built two ways:
//!
//! - [`format_count`] formats one number for a **named placeholder**:
//!   `tr!("{files} files", files = counts::format_count(n))`. Prefer it —
//!   it groups exactly the number you meant and nothing else.
//! - [`group_digits`] is a **post-translation pass** over a finished
//!   label, for `trn!` whose plural `{n}` is substituted implicitly and
//!   so cannot take a pre-formatted string. It groups every run of four
//!   or more digits in the text, so only pass it a short count phrase
//!   (optionally carrying a humanized size) — never a string holding a
//!   path, file name, hash, year, or version, whose digits it would
//!   happily mangle.

/// Format a count for display: `1104619` → `"1.104.619"`.
///
/// Numbers below 1000 come back unchanged.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (index, ch) in digits.char_indices() {
        if index > 0 && (len - index) % 3 == 0 {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

/// Group every run of four or more digits in `text` the way
/// [`format_count`] groups one number.
///
/// Applied *after* translation so plural selection and word order stay
/// entirely locale-driven. Idempotent: text that is already grouped has
/// no digit run longer than three and passes through untouched.
///
/// Only for short count phrases — see the module docs for what not to
/// feed it.
pub fn group_digits(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 3);
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some(ch) = text[cursor..].chars().next() else {
            break;
        };
        if !ch.is_ascii_digit() {
            out.push(ch);
            cursor += ch.len_utf8();
            continue;
        }
        let start = cursor;
        while cursor < text.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        let digits = &text[start..cursor];
        for (index, digit) in digits.bytes().enumerate() {
            if index > 0 && (digits.len() - index) % 3 == 0 {
                out.push('.');
            }
            out.push(char::from(digit));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_numbers_are_left_alone() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(7), "7");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn grouping_starts_at_four_digits() {
        assert_eq!(format_count(1_000), "1.000");
        assert_eq!(format_count(12_345), "12.345");
        assert_eq!(format_count(1_104_619), "1.104.619");
    }

    #[test]
    fn a_label_keeps_its_words_and_its_size() {
        assert_eq!(
            group_digits("1104619 files, 743.4 GB"),
            "1.104.619 files, 743.4 GB"
        );
    }

    #[test]
    fn grouping_a_grouped_label_changes_nothing() {
        let once = group_digits("4138016 items");
        assert_eq!(group_digits(&once), once);
    }
}
