//! Source-string extraction: scans the workspace's Rust sources for
//! `tr!(…)`, `trc!(…, …)`, `trn!(…, …, …)` and `msgid!(…)` invocations and
//! renders `locales/en.json`, the canonical msgid list.
//!
//! Deliberately a small hand-rolled scanner rather than a `syn` walk: it
//! only has to recognise string literals directly inside those macro
//! calls, it must see `#[cfg]`-gated platform code without compiling it,
//! and it runs as an ordinary unit test (`en_json_is_up_to_date`) with no
//! extra tooling. Regenerate with:
//!
//! ```sh
//! FERAIL_I18N_UPDATE=1 cargo test -p ferail-core i18n::extract
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::pack::{LanguagePack, PackValue};

/// Everything the scanner found.
#[derive(Default, Debug)]
pub struct Extracted {
    pub strings: BTreeMap<String, PackValue>,
    /// msgid → `crate/src/file.rs:line` list (debug aid; not written to
    /// en.json).
    pub locations: BTreeMap<String, Vec<String>>,
}

/// The repository root (two levels above this crate's manifest).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("../.."))
}

/// Scan every `crates/**/*.rs` under `root` (skipping `target/`).
pub fn scan_workspace(root: &Path) -> Extracted {
    let mut out = Extracted::default();
    let mut files = Vec::new();
    collect_rs(&root.join("crates"), &mut files);
    files.sort();
    for file in files {
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        let label = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        scan_source(&src, &label, &mut out);
    }
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name == "target" || name == ".git" {
                continue;
            }
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

const MACROS: &[&str] = &["tr", "trc", "trn", "msgid"];

/// Scan one source text. `label` names it in `locations`.
pub fn scan_source(src: &str, label: &str, out: &mut Extracted) {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip `//` comments so documentation examples don't count.
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Skip string literals that are not part of a macro call, so a
        // literal containing `tr!(` is not mistaken for an invocation.
        if bytes[i] == b'"' || (bytes[i] == b'r' && is_raw_string_start(&src[i..])) {
            match parse_string_literal(&src[i..]) {
                Some((_, len)) => {
                    i += len;
                    continue;
                }
                None => {
                    i += 1; // `"` and `r` are ASCII: still a char boundary
                    continue;
                }
            }
        }
        if let Some((name, rest_at)) = macro_at(src, i) {
            let line = src[..i].matches('\n').count() + 1;
            let rest = &src[rest_at..];
            if let Some(consumed) = parse_invocation(name, rest, &format!("{label}:{line}"), out) {
                i = rest_at + consumed;
                continue;
            }
        }
        // Advance one *character*, not one byte — sources are UTF-8 and
        // `&src[i..]` above must land on a char boundary.
        i += src[i..].chars().next().map_or(1, char::len_utf8);
    }
}

fn is_raw_string_start(s: &str) -> bool {
    let b = s.as_bytes();
    if b.first() != Some(&b'r') {
        return false;
    }
    let mut j = 1;
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    b.get(j) == Some(&b'"')
}

/// If a supported macro invocation starts at `i` (`name!(`), return its
/// name and the index just past the `(`.
fn macro_at(src: &str, i: usize) -> Option<(&'static str, usize)> {
    let b = src.as_bytes();
    // Must start an identifier: previous char is not ident-ish (and not a
    // `'` — lifetimes — nor part of a path like `::tr!` which is fine).
    if i > 0 {
        let p = b[i - 1];
        if p.is_ascii_alphanumeric() || p == b'_' {
            return None;
        }
    }
    for name in MACROS {
        if src[i..].starts_with(name) {
            let mut j = i + name.len();
            if j >= b.len() || b[j] != b'!' {
                continue;
            }
            j += 1;
            while j < b.len() && (b[j] as char).is_whitespace() {
                j += 1;
            }
            if j < b.len() && b[j] == b'(' {
                return Some((name, j + 1));
            }
        }
    }
    None
}

/// Parse the literal arguments of one invocation starting right after its
/// `(`. Returns how many bytes were consumed (to the last literal), or
/// `None` if the arguments aren't literals (a dynamic `tr!` — not allowed,
/// but don't choke).
fn parse_invocation(name: &str, rest: &str, where_: &str, out: &mut Extracted) -> Option<usize> {
    let mut pos = skip_ws(rest, 0);
    let (first, len) = parse_string_literal(&rest[pos..])?;
    pos += len;
    match name {
        "tr" | "msgid" => {
            record(out, first, PackValue::Text(String::new()), where_);
            Some(pos)
        }
        "trc" => {
            pos = expect_comma(rest, pos)?;
            let (msgid, len) = parse_string_literal(&rest[pos..])?;
            pos += len;
            record(
                out,
                format!("{first}{}{msgid}", super::CONTEXT_SEPARATOR),
                PackValue::Text(String::new()),
                where_,
            );
            Some(pos)
        }
        "trn" => {
            pos = expect_comma(rest, pos)?;
            let (other, len) = parse_string_literal(&rest[pos..])?;
            pos += len;
            let mut forms = BTreeMap::new();
            forms.insert("one".to_owned(), first.clone());
            forms.insert("other".to_owned(), other);
            record(out, first, PackValue::Plural(forms), where_);
            Some(pos)
        }
        _ => None,
    }
}

fn record(out: &mut Extracted, key: String, value: PackValue, where_: &str) {
    let value = match value {
        // For plain entries the English value is the key itself.
        PackValue::Text(_) => PackValue::Text(key.clone()),
        v => v,
    };
    out.strings.entry(key.clone()).or_insert(value);
    out.locations
        .entry(key)
        .or_default()
        .push(where_.to_owned());
}

fn skip_ws(s: &str, mut pos: usize) -> usize {
    let b = s.as_bytes();
    while pos < b.len() && (b[pos] as char).is_whitespace() {
        pos += 1;
    }
    pos
}

fn expect_comma(s: &str, pos: usize) -> Option<usize> {
    let pos = skip_ws(s, pos);
    if s.as_bytes().get(pos) != Some(&b',') {
        return None;
    }
    Some(skip_ws(s, pos + 1))
}

/// Parse a Rust string literal (`"…"` with escapes and `\`-newline
/// continuations, or `r#"…"#`) at the start of `s`. Returns the decoded
/// text and the byte length consumed.
pub fn parse_string_literal(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b.first() == Some(&b'r') {
        let mut hashes = 0;
        while b.get(1 + hashes) == Some(&b'#') {
            hashes += 1;
        }
        if b.get(1 + hashes) != Some(&b'"') {
            return None;
        }
        let body_start = 2 + hashes;
        let closer = format!("\"{}", "#".repeat(hashes));
        let end = s[body_start..].find(&closer)? + body_start;
        return Some((s[body_start..end].to_owned(), end + closer.len()));
    }
    if b.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = s[1..].char_indices();
    while let Some((idx, c)) = chars.next() {
        match c {
            '"' => return Some((out, 1 + idx + 1)),
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '\'' => out.push('\''),
                    'u' => {
                        // \u{XXXX}
                        let (_, brace) = chars.next()?;
                        if brace != '{' {
                            return None;
                        }
                        let mut hex = String::new();
                        for (_, h) in chars.by_ref() {
                            if h == '}' {
                                break;
                            }
                            hex.push(h);
                        }
                        let cp = u32::from_str_radix(hex.trim_matches('_'), 16).ok()?;
                        out.push(char::from_u32(cp)?);
                    }
                    'x' => {
                        let (_, h1) = chars.next()?;
                        let (_, h2) = chars.next()?;
                        let cp = u8::from_str_radix(&format!("{h1}{h2}"), 16).ok()?;
                        out.push(cp as char);
                    }
                    '\n' | '\r' => {
                        // Line continuation: skip following whitespace.
                        let peek = chars.clone();
                        for (_, w) in peek {
                            if w.is_whitespace() {
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
    }
    None
}

/// Render the `locales/en.json` document for an extraction.
pub fn render_en_json(extracted: &Extracted) -> String {
    let mut pack = LanguagePack::empty("en", "English", "English");
    pack.strings = extracted.strings.clone();
    pack.to_json()
}

/// Path of `locales/en.json` under `root`.
pub fn en_json_path(root: &Path) -> PathBuf {
    root.join("locales").join("en.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_literals() {
        assert_eq!(
            parse_string_literal(r#""plain" rest"#),
            Some(("plain".into(), 7))
        );
        assert_eq!(
            parse_string_literal(r#""a \"q\" \u{2026}""#).unwrap().0,
            "a \"q\" …"
        );
        assert_eq!(
            parse_string_literal("\"line \\\n     continued\"")
                .unwrap()
                .0,
            "line continued"
        );
        assert_eq!(
            parse_string_literal(r##"r#"raw "x""# tail"##),
            Some(("raw \"x\"".into(), 12))
        );
        assert_eq!(parse_string_literal("not"), None);
    }

    #[test]
    fn scans_each_macro_form() {
        let src = r#"
            // tr!("in a comment")
            let a = tr!("Hello");
            let b = tr!("Copied {n} files", n = 3);
            let c = trc!("menu", "Open");
            let d = trn!("{n} file", "{n} files", count);
            const T: &str = msgid!("New Window");
            let e = crate::tr!("Path form");
            let f = tr! (
                "Spaced"
            );
            let g = "tr!(\"inside a string\")";
            let h = tr!(dynamic_not_literal);
        "#;
        let mut out = Extracted::default();
        scan_source(src, "x.rs", &mut out);
        let keys: Vec<&String> = out.strings.keys().collect();
        assert_eq!(
            keys,
            vec![
                "Copied {n} files",
                "Hello",
                "New Window",
                "Path form",
                "Spaced",
                "menu::Open",
                "{n} file"
            ]
        );
        assert_eq!(out.strings["Hello"], PackValue::Text("Hello".into()));
        match &out.strings["{n} file"] {
            PackValue::Plural(forms) => {
                assert_eq!(forms["one"], "{n} file");
                assert_eq!(forms["other"], "{n} files");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(out.locations["Hello"], vec!["x.rs:3"]);
    }

    /// `locales/en.json` must match the sources. Regenerate with
    /// `FERAIL_I18N_UPDATE=1 cargo test -p ferail-core i18n::extract`.
    #[test]
    fn en_json_is_up_to_date() {
        let root = workspace_root();
        let extracted = scan_workspace(&root);
        let rendered = render_en_json(&extracted);
        let path = en_json_path(&root);
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        if rendered == on_disk {
            return;
        }
        if std::env::var_os("FERAIL_I18N_UPDATE").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rendered).unwrap();
            eprintln!("updated {}", path.display());
            return;
        }
        let old = LanguagePack::parse(&on_disk)
            .map(|p| p.strings)
            .unwrap_or_default();
        let added: Vec<_> = extracted
            .strings
            .keys()
            .filter(|k| !old.contains_key(*k))
            .collect();
        let removed: Vec<_> = old
            .keys()
            .filter(|k| !extracted.strings.contains_key(*k))
            .collect();
        panic!(
            "locales/en.json is out of date ({} added, {} removed).\n  added: {:?}\n  removed: {:?}\nRun: FERAIL_I18N_UPDATE=1 cargo test -p ferail-core i18n::extract",
            added.len(),
            removed.len(),
            added.iter().take(20).collect::<Vec<_>>(),
            removed.iter().take(20).collect::<Vec<_>>(),
        );
    }
}
