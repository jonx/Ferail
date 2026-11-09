//! Natural ("logical") name ordering: `2.pdf` before `10.pdf`.
//!
//! Explorer (`StrCmpLogicalW`) and Finder both compare runs of digits by
//! numeric value and the rest case-insensitively. A plain lowercase string
//! comparison puts `10_Formulaire` before `1_Deroulement`, because `0`
//! sorts before `_`.
//!
//! [`natural_key`] builds a byte key once per name, so callers keep using
//! `sort_by_cached_key` and pay a `memcmp` per comparison instead of a
//! re-parse.

use std::cmp::Ordering;

/// Byte that opens an encoded digit run. It sorts below every byte a
/// lowercased printable name produces, so a number orders before text at
/// the same position (`1abc` before `abc`, as in Explorer).
const DIGIT_RUN: u8 = 0x01;

/// Sort key for `name`: case-folded text, with each ASCII digit run encoded
/// as `DIGIT_RUN`, its significant-digit count (big-endian `u32`), then the
/// significant digits. Equal keys mean names that differ only by case or
/// leading zeros; callers break those ties with a stable id.
pub fn natural_key(name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(name.len() + 8);
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &bytes[start..i];
            let significant = match run.iter().position(|&b| b != b'0') {
                Some(first) => &run[first..],
                None => &run[run.len() - 1..],
            };
            key.push(DIGIT_RUN);
            key.extend_from_slice(&(significant.len() as u32).to_be_bytes());
            key.extend_from_slice(significant);
        } else {
            key.push(bytes[i]);
            i += 1;
        }
    }
    key
}

/// Compare two names in natural order. Prefer [`natural_key`] with
/// `sort_by_cached_key` when sorting many names.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_key(a).cmp(&natural_key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        v.sort_by_cached_key(|n| natural_key(n));
        v
    }

    #[test]
    fn numbers_compare_by_value() {
        assert_eq!(
            sorted(&["10_Formulaire.pdf", "2_Presentation.pdf", "1_Deroulement.pdf", "21_Pour.pdf"]),
            ["1_Deroulement.pdf", "2_Presentation.pdf", "10_Formulaire.pdf", "21_Pour.pdf"]
        );
    }

    #[test]
    fn case_insensitive_and_prefix_first() {
        assert_eq!(sorted(&["file2", "File", "file10", "FILE1"]), ["File", "FILE1", "file2", "file10"]);
    }

    #[test]
    fn leading_zeros_and_long_runs() {
        assert_eq!(natural_cmp("img007", "img7"), Ordering::Equal);
        assert_eq!(natural_cmp("img008", "img10"), Ordering::Less);
        assert_eq!(
            natural_cmp("a99999999999999999999999", "a100000000000000000000000"),
            Ordering::Less
        );
        assert_eq!(natural_cmp("a0", "a00"), Ordering::Equal);
    }

    #[test]
    fn digits_before_letters_and_issue_numbers() {
        assert_eq!(natural_cmp("1abc", "abc"), Ordering::Less);
        assert_eq!(natural_cmp("Spirou.N4592-4593", "Spirou.N4594"), Ordering::Less);
        assert_eq!(natural_cmp("Society.N289", "Society.N1000"), Ordering::Less);
    }
}
