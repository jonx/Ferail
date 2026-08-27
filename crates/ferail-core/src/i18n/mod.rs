//! Localization — an English-as-key runtime string catalog.
//!
//! Design (see `docs/features/LOCALIZATION.md`):
//!
//! - **The English source text is the key.** Code writes
//!   `tr!("Empty folder")`, never an abstract id. Rewording the English
//!   automatically invalidates stale translations (the old key simply stops
//!   being looked up), there is no second place to keep in sync, and a
//!   translator — human or LLM — sees real sentences.
//! - **A language is one JSON file** ([`pack::LanguagePack`]) — the same
//!   format whether it ships inside the binary (`locales/fr.json`, embedded
//!   via `include_str!`) or sits in the user's config dir. That file is also
//!   the import/export/translation artifact.
//! - **`locales/en.json` is generated** by [`extract`] from every
//!   `tr!`/`trc!`/`trn!`/`msgid!` call site in the workspace, and a test
//!   fails when it is stale. It is the complete msgid set, so coverage and
//!   "untranslated" lists are exact.
//! - **Lookups are render-safe.** The active [`Catalog`] lives in an
//!   `ArcSwap`; `tr!` is one lock-free load plus one `HashMap` probe, and
//!   when English is active it returns the `&'static str` untouched — no
//!   allocation, no lock, no I/O (Prime Directive). Loading and parsing a
//!   pack is the caller's job and belongs on a background executor.
//!
//! Macros (exported at the crate root):
//!
//! ```ignore
//! tr!("Empty folder")                              // -> Text
//! tr!("Copied {n} files to {dest}", n = 3, dest = d) // {name} placeholders
//! trc!("menu", "Open")                             // disambiguating context
//! trn!("{n} file", "{n} files", count)             // plural; {n} is implicit
//! msgid!("New Window")                             // marks a literal for
//!                                                  //   extraction, returns it
//!                                                  //   unchanged (const tables)
//! ```
//!
//! `msgid!` exists for static tables such as the command catalogue: the
//! literal is extracted like any other, and the display site translates the
//! `&'static str` later with [`tr_raw`].

use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;

pub mod extract;
pub mod pack;
pub mod plural;

pub use pack::{LanguagePack, PackValue, ValidationReport};

/// Separator between a context and its msgid inside pack keys:
/// `trc!("menu", "Open")` is stored as `"menu::Open"`. Lookups never split
/// keys — a plain `tr!("a::b")` is matched literally — so the separator only
/// has to be readable, not unambiguous.
pub const CONTEXT_SEPARATOR: &str = "::";

// =============================================================================
// Text
// =============================================================================

/// A localized string: the English source (`&'static str`, zero-copy) or a
/// translation shared with the active catalog (`Arc<str>`, one refcount bump).
/// Derefs to `str`; UI layers convert it to their own string type without
/// copying.
#[derive(Clone, Debug)]
pub enum Text {
    Static(&'static str),
    Shared(Arc<str>),
}

impl Text {
    pub fn as_str(&self) -> &str {
        match self {
            Text::Static(s) => s,
            Text::Shared(s) => s,
        }
    }

    pub fn into_arc(self) -> Arc<str> {
        match self {
            Text::Static(s) => Arc::from(s),
            Text::Shared(s) => s,
        }
    }

    pub fn into_string(self) -> String {
        self.as_str().to_owned()
    }
}

impl Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Text {}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Text::Shared(Arc::from(s))
    }
}

impl From<Arc<str>> for Text {
    fn from(s: Arc<str>) -> Self {
        Text::Shared(s)
    }
}

impl From<&'static str> for Text {
    fn from(s: &'static str) -> Self {
        Text::Static(s)
    }
}

impl From<Text> for String {
    fn from(t: Text) -> Self {
        t.into_string()
    }
}

// =============================================================================
// Catalog
// =============================================================================

/// One translated entry: a single string, or CLDR plural forms.
#[derive(Clone, Debug)]
pub enum Entry {
    One(Arc<str>),
    Plural(Vec<(plural::Category, Arc<str>)>),
}

/// The strings of one language, indexed for lock-free render-time lookup.
/// Build one with [`Catalog::from_pack`] (off the UI thread) and install it
/// with [`set_active`].
#[derive(Debug)]
pub struct Catalog {
    code: String,
    lang: String,
    plain: HashMap<String, Entry>,
    by_context: HashMap<String, HashMap<String, Entry>>,
}

impl Catalog {
    /// The built-in source language: every lookup falls through to the
    /// English literal in the code.
    pub fn english() -> Self {
        Self {
            code: "en".to_owned(),
            lang: "en".to_owned(),
            plain: HashMap::new(),
            by_context: HashMap::new(),
        }
    }

    /// Index a parsed pack. Keys containing [`CONTEXT_SEPARATOR`] are also
    /// registered under their context so `trc!` finds them without building
    /// a composite key at lookup time.
    pub fn from_pack(pack: &LanguagePack) -> Self {
        let mut plain = HashMap::with_capacity(pack.strings.len());
        let mut by_context: HashMap<String, HashMap<String, Entry>> = HashMap::new();
        for (key, value) in &pack.strings {
            let entry = match value {
                PackValue::Text(s) => Entry::One(Arc::from(s.as_str())),
                PackValue::Plural(forms) => Entry::Plural(
                    forms
                        .iter()
                        .filter_map(|(cat, s)| {
                            plural::Category::from_name(cat).map(|c| (c, Arc::from(s.as_str())))
                        })
                        .collect(),
                ),
            };
            if let Some((ctx, msgid)) = key.split_once(CONTEXT_SEPARATOR) {
                by_context
                    .entry(ctx.to_owned())
                    .or_default()
                    .insert(msgid.to_owned(), entry.clone());
            }
            plain.insert(key.clone(), entry);
        }
        Self {
            code: pack.code.clone(),
            lang: language_subtag(&pack.code).to_owned(),
            plain,
            by_context,
        }
    }

    /// BCP-47-ish code of this catalog (`"fr"`, `"pt-BR"`).
    pub fn code(&self) -> &str {
        &self.code
    }

    /// True for the built-in English catalog (every lookup is a no-op).
    pub fn is_english(&self) -> bool {
        self.plain.is_empty() && self.lang == "en"
    }

    pub fn lookup(&self, msgid: &str) -> Option<&Entry> {
        self.plain.get(msgid)
    }

    pub fn lookup_ctx(&self, ctx: &str, msgid: &str) -> Option<&Entry> {
        self.by_context.get(ctx)?.get(msgid)
    }

    fn plural_form<'a>(
        &self,
        forms: &'a [(plural::Category, Arc<str>)],
        n: u64,
    ) -> Option<&'a Arc<str>> {
        let want = plural::category(&self.lang, n);
        forms
            .iter()
            .find(|(c, _)| *c == want)
            .or_else(|| forms.iter().find(|(c, _)| *c == plural::Category::Other))
            .map(|(_, s)| s)
    }
}

/// `"pt-BR"` → `"pt"`; lower-cased.
pub fn language_subtag(code: &str) -> &str {
    code.split(['-', '_']).next().unwrap_or(code)
}

// =============================================================================
// Active catalog
// =============================================================================

static ACTIVE: OnceLock<ArcSwap<Catalog>> = OnceLock::new();

fn active_cell() -> &'static ArcSwap<Catalog> {
    ACTIVE.get_or_init(|| ArcSwap::from_pointee(Catalog::english()))
}

/// Install `catalog` as the language every `tr!` resolves against from now
/// on. Cheap and lock-free for readers; callers must still repaint their UI
/// (and rebuild anything that cached translated text, e.g. native menus).
pub fn set_active(catalog: Catalog) {
    active_cell().store(Arc::new(catalog));
}

/// Snapshot of the active catalog.
pub fn active() -> Arc<Catalog> {
    active_cell().load_full()
}

/// Code of the active catalog (`"en"` until a pack is installed).
pub fn active_code() -> String {
    active_cell().load().code.clone()
}

/// The OS's preferred UI language as a BCP-47 tag (`"fr-FR"`), if it can be
/// determined. Environment/registry read — cheap, but call it once at
/// startup, not per frame.
pub fn system_locale() -> Option<String> {
    sys_locale::get_locale()
}

// =============================================================================
// Lookup entry points (used by the macros; callable directly for dynamic
// `&'static str` msgids such as the command catalogue's titles)
// =============================================================================

/// Translate a plain msgid. English active → the literal itself, zero-copy.
pub fn tr_raw(msgid: &'static str) -> Text {
    let cat = active_cell().load();
    if cat.is_english() {
        return Text::Static(msgid);
    }
    match cat.lookup(msgid) {
        Some(Entry::One(s)) => Text::Shared(s.clone()),
        _ => Text::Static(msgid),
    }
}

/// Translate a msgid that is only known at runtime (a kind word such as
/// `"Folder"` cached inside a `FileEntry`, a label persisted as data).
/// `None` when the active language has no entry — including always under
/// English — so the caller displays its own string without copying. The
/// literal must still appear in a `msgid!` somewhere for extraction.
pub fn tr_dynamic(msgid: &str) -> Option<Arc<str>> {
    let cat = active_cell().load();
    if cat.is_english() {
        return None;
    }
    match cat.lookup(msgid) {
        Some(Entry::One(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Translate a msgid under a disambiguating context (`trc!`).
pub fn trc_raw(ctx: &'static str, msgid: &'static str) -> Text {
    let cat = active_cell().load();
    if cat.is_english() {
        return Text::Static(msgid);
    }
    match cat.lookup_ctx(ctx, msgid) {
        Some(Entry::One(s)) => Text::Shared(s.clone()),
        _ => Text::Static(msgid),
    }
}

/// Translate a plural pair (`trn!`). The singular is the key; the English
/// fallback picks `one` for exactly 1, `other` otherwise.
pub fn trn_raw(one: &'static str, other: &'static str, n: u64) -> Text {
    let english = || Text::Static(if n == 1 { one } else { other });
    let cat = active_cell().load();
    if cat.is_english() {
        return english();
    }
    match cat.lookup(one) {
        Some(Entry::Plural(forms)) => match cat.plural_form(forms, n) {
            Some(s) => Text::Shared(s.clone()),
            None => english(),
        },
        // A translator may have supplied a single string for a plural
        // msgid (fine for languages without number agreement, e.g. ja).
        Some(Entry::One(s)) => Text::Shared(s.clone()),
        None => english(),
    }
}

/// Substitute `{name}` placeholders. Unknown names are left as-is so a
/// mistranslated placeholder is visible rather than silently dropped;
/// `{{` / `}}` produce literal braces.
pub fn fill(template: &str, args: &[(&str, &dyn fmt::Display)]) -> String {
    fn push_segment(out: &mut String, seg: &str) {
        if seg.contains("}}") {
            out.push_str(&seg.replace("}}", "}"));
        } else {
            out.push_str(seg);
        }
    }
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        push_segment(&mut out, &rest[..open]);
        let after = &rest[open + 1..];
        if let Some(stripped) = after.strip_prefix('{') {
            out.push('{');
            rest = stripped;
            continue;
        }
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => {
                        use fmt::Write as _;
                        let _ = write!(out, "{v}");
                    }
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    push_segment(&mut out, rest);
    out
}

// =============================================================================
// Macros
// =============================================================================

/// Translate a string literal; see the module docs. Returns [`Text`].
#[macro_export]
macro_rules! tr {
    ($msg:literal $(,)?) => {
        $crate::i18n::tr_raw($msg)
    };
    ($msg:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::Text::from($crate::i18n::fill(
            &$crate::i18n::tr_raw($msg),
            &[$((stringify!($name), &$val as &dyn ::std::fmt::Display)),+],
        ))
    };
}

/// Translate a literal under a context: `trc!("menu", "Open")`.
#[macro_export]
macro_rules! trc {
    ($ctx:literal, $msg:literal $(,)?) => {
        $crate::i18n::trc_raw($ctx, $msg)
    };
    ($ctx:literal, $msg:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::Text::from($crate::i18n::fill(
            &$crate::i18n::trc_raw($ctx, $msg),
            &[$((stringify!($name), &$val as &dyn ::std::fmt::Display)),+],
        ))
    };
}

/// Translate a plural pair: `trn!("{n} file", "{n} files", count)`. `{n}`
/// is filled from the count; extra `name = value` placeholders are allowed.
///
/// The plural category is chosen from the raw count, but `{n}` is
/// *displayed* through [`crate::counts::format_count`] — a plural `{n}`
/// is always a count of something, and Ferail counts run to the millions,
/// so it reads "1.104.619 files" without every call site asking. Counts
/// that ride in a named placeholder are not covered: format those with
/// `format_count` yourself.
#[macro_export]
macro_rules! trn {
    ($one:literal, $other:literal, $n:expr $(,)?) => {{
        let __n: u64 = ($n) as u64;
        let __shown = $crate::counts::format_count(__n);
        $crate::i18n::Text::from($crate::i18n::fill(
            &$crate::i18n::trn_raw($one, $other, __n),
            &[("n", &__shown as &dyn ::std::fmt::Display)],
        ))
    }};
    ($one:literal, $other:literal, $n:expr, $($name:ident = $val:expr),+ $(,)?) => {{
        let __n: u64 = ($n) as u64;
        let __shown = $crate::counts::format_count(__n);
        $crate::i18n::Text::from($crate::i18n::fill(
            &$crate::i18n::trn_raw($one, $other, __n),
            &[("n", &__shown as &dyn ::std::fmt::Display), $((stringify!($name), &$val as &dyn ::std::fmt::Display)),+],
        ))
    }};
}

/// Mark a literal as a translatable msgid without translating it here —
/// for `const` tables whose display site calls [`tr_raw`] later.
#[macro_export]
macro_rules! msgid {
    ($msg:literal $(,)?) => {
        $msg
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counts::format_count;
    use std::collections::BTreeMap;

    fn pack(code: &str, pairs: &[(&str, PackValue)]) -> LanguagePack {
        let mut strings = BTreeMap::new();
        for (k, v) in pairs {
            strings.insert((*k).to_owned(), v.clone());
        }
        LanguagePack {
            format: 1,
            code: code.to_owned(),
            name: code.to_owned(),
            english_name: code.to_owned(),
            instructions: String::new(),
            strings,
            untranslated: BTreeMap::new(),
        }
    }

    fn plural(forms: &[(&str, &str)]) -> PackValue {
        PackValue::Plural(
            forms
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn fill_substitutes_and_keeps_unknowns() {
        assert_eq!(
            fill("Copied {n} to {dest}", &[("n", &3), ("dest", &"x")]),
            "Copied 3 to x"
        );
        assert_eq!(fill("{missing} stays", &[]), "{missing} stays");
        assert_eq!(fill("{{literal}} {n}", &[("n", &1)]), "{literal} 1");
        assert_eq!(fill("dangling { brace", &[]), "dangling { brace");
    }

    /// A plural count is displayed grouped, but the plural *category*
    /// is still chosen from the raw number — "1.000 files", not
    /// "1.000 file" and not "1000 files".
    #[test]
    fn plural_counts_are_displayed_grouped() {
        assert_eq!(
            trn!("{n} file", "{n} files", 1_204u64).to_string(),
            "1.204 files"
        );
        assert_eq!(trn!("{n} file", "{n} files", 1u64).to_string(), "1 file");
        // Named placeholders are the call site's job to format; `{n}`
        // is not. (Real msgid — the extractor reads this file too.)
        assert_eq!(
            trn!(
                "Scanning: {n} file in {dirs} folders",
                "Scanning: {n} files in {dirs} folders",
                12_345u64,
                dirs = format_count(88_000)
            )
            .to_string(),
            "Scanning: 12.345 files in 88.000 folders"
        );
    }

    #[test]
    fn english_catalog_is_passthrough() {
        let cat = Catalog::english();
        assert!(cat.is_english());
        assert!(cat.lookup("Open").is_none());
    }

    #[test]
    fn catalog_resolves_plain_context_and_plural() {
        let p = pack(
            "fr",
            &[
                ("Open", PackValue::Text("Ouvrir".into())),
                ("menu::Open", PackValue::Text("Ouvrir (menu)".into())),
                (
                    "{n} file",
                    plural(&[("one", "{n} fichier"), ("other", "{n} fichiers")]),
                ),
            ],
        );
        let cat = Catalog::from_pack(&p);
        assert!(!cat.is_english());
        assert!(matches!(cat.lookup("Open"), Some(Entry::One(s)) if &**s == "Ouvrir"));
        assert!(
            matches!(cat.lookup_ctx("menu", "Open"), Some(Entry::One(s)) if &**s == "Ouvrir (menu)")
        );
        match cat.lookup("{n} file") {
            Some(Entry::Plural(forms)) => {
                assert_eq!(&**cat.plural_form(forms, 0).unwrap(), "{n} fichier"); // fr: 0 is `one`
                assert_eq!(&**cat.plural_form(forms, 1).unwrap(), "{n} fichier");
                assert_eq!(&**cat.plural_form(forms, 2).unwrap(), "{n} fichiers");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn plural_falls_back_to_other_form() {
        let p = pack("ru", &[("{n} file", plural(&[("other", "{n} файлов")]))]);
        let cat = Catalog::from_pack(&p);
        match cat.lookup("{n} file") {
            Some(Entry::Plural(forms)) => {
                assert_eq!(&**cat.plural_form(forms, 1).unwrap(), "{n} файлов");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn language_subtag_strips_region() {
        assert_eq!(language_subtag("pt-BR"), "pt");
        assert_eq!(language_subtag("zh_Hant"), "zh");
        assert_eq!(language_subtag("de"), "de");
    }

    #[test]
    fn macros_expand_against_english() {
        // The process-global active catalog may have been swapped by
        // another test; these only exercise the macro plumbing, using the
        // raw functions' English path semantics through `fill`.
        // (No `tr!`/`msgid!` literals in tests: the extractor would list
        // them in en.json.)
        let t = fill(&tr_raw("Copied {n} files"), &[("n", &2)]);
        assert_eq!(t, "Copied 2 files");
    }
}
