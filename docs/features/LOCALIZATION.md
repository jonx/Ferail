# Localization

Ferail's UI can be shown in any language for which a **language pack** exists.
Packs are plain JSON files; two (French, German) ship inside the app, and
anyone can create another one without touching the code — typically by
handing a generated template to a translator or to an AI assistant (Claude,
ChatGPT, a local model…) and importing the result. Ferail itself never calls
an LLM.

← [Feature notes](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md)

## For users: Settings › Appearance › Language

| Control | What it does |
| --- | --- |
| **Language** dropdown | *System* (follow the OS language, falling back to English when no pack matches), *English*, or any installed pack — shown with its coverage, e.g. *Français (87 %)*. Takes effect immediately: every window repaints and the menu bar is rebuilt. Strings a pack does not cover stay in English. |
| **New language…** | Pick a language; Ferail writes `<code>.json` — a template with every string still in English, plus the translation instructions — into your languages folder and reveals it. |
| **Import…** | Pick a translated `.json`. It is validated (unknown keys, lost `{placeholders}`, bad plural categories are reported, none are fatal), copied into the languages folder as `<code>.json`, and selected. |
| **Export…** | Save the selected language as a template: its translations plus the strings still untranslated (with instructions) — to finish a partial pack, fix a bundled one, or contribute it back. With *English* selected it saves the source catalog. |
| **Show folder** | Reveal the languages folder. |
| **Reload** | Re-scan the folder (after editing a file by hand). |
| **Copy instructions** | Put the translator brief on the clipboard, for pasting into a chat along with the file. |

The languages folder is `~/Library/Application Support/Ferail/languages`
(macOS), `%APPDATA%\Ferail\languages` (Windows), `$XDG_CONFIG_HOME/ferail/languages`
(Linux). A user pack with the same code as a bundled one replaces it.

**The translation workflow** (no API key, any model):

1. *New language…* → pick the language → the template opens in Finder.
2. Drop the file into a chat and say "translate this"; the instructions are
   in the file's `"instructions"` field (or use *Copy instructions*). The
   assistant returns the same JSON with `"untranslated"` moved into
   `"strings"`.
3. Save the reply as a `.json` file → *Import…*. Done; coverage shows in the
   dropdown.

To contribute a language, export it and open a pull request adding
`locales/<code>.json` (and a line in `BUNDLED` in
`crates/ferail-core/src/i18n/pack.rs`).

## The pack format

```json
{
  "format": 1,
  "code": "fr",
  "name": "Français",
  "english_name": "French",
  "instructions": "…only in templates…",
  "strings": {
    "Open": "Ouvrir",
    "menu::Open": "Ouvrir",
    "{n} file": { "one": "{n} fichier", "other": "{n} fichiers" }
  },
  "untranslated": { "Move to Trash": "Move to Trash" }
}
```

- **Keys are the English source text.** A key of the form `context::text`
  carries a disambiguating context (`trc!`); the text after `::` is what is
  translated and the key stays verbatim.
- `{name}` placeholders are substituted at runtime and must survive
  translation (they may be reordered). `{{`/`}}` are literal braces.
- A plural entry is an object keyed by CLDR category (`zero one two few many
  other`; `other` is required). The count is always `{n}`. Which category a
  number falls into is decided per language by
  `ferail_core::i18n::plural` (extend it for a new language family).
- `untranslated` exists only in templates: strings not yet done, with their
  English text. Import reads `strings` only.
- `locales/en.json` has the same shape with English values; it is **generated**
  and is the complete list of msgids.

## For developers: writing strings

The core lives in `ferail_core::i18n` (catalog, macros, pack format, plural
rules, extractor); `ferail_gpui::i18n` adds the gpui glue (SharedString,
the `Languages` global, switching, the Settings operations).

```rust
// ferail-gpui (returns SharedString):
tr!("Empty folder")
tr!("Copied {n} files to {dest}", n = count, dest = dir.display())
trn!("{n} item", "{n} items", count)          // {n} is filled automatically
trc!("menu", "Open")                          // same English, different meaning
crate::i18n::tr_static(spec.title)            // a msgid from a static table

// ferail-core / shell crates (returns ferail_core::i18n::Text — Deref<str>,
// Display, Into<String>):
use ferail_core::{tr, trc, trn, msgid};
let label = tr!("Move to Trash");
NSString::from_str(&tr!("Open With"));

// Static tables (const contexts can't call tr!):
const CATALOGUE: &[CommandSpec] = &[CommandSpec { title: msgid!("New Window"), … }];
```

Rules:

- **Arguments must be string literals.** The extractor (see below) only sees
  literals; a dynamic `tr!(x)` does not compile. For a `&'static str` that
  came from a `msgid!` table, call `tr_raw` / `tr_static`.
- **Whole sentences, with placeholders** — never concatenate fragments
  (`tr!("Copied ") + n + tr!(" files")` is untranslatable); word order differs
  between languages.
- **Keep the English exactly as it is displayed today** when converting an
  existing literal; rewording is a separate change (and invalidates the
  translation, by design).
- Use `trn!` for anything with a count; use `trc!` only when one English word
  genuinely needs two translations.
- **What is translated:** everything the user reads in the UI — labels, menu
  items, titles, descriptions, tooltips, placeholders, column headers, status
  text, empty states, dialog text, toasts and their advice.
- **What stays English:** log lines, the activity trail, the diagnostics
  report / `--doctor`, report bundles, CLI `--help`, element ids, icon paths,
  persisted values (`"list"`, `"grid"`), env vars, test code, and — for now —
  error text coming from the backend crates (`ferail-fs-native`,
  `ferail-archive`): a bug report must stay readable by the maintainers.
- `tr!` is render-safe: one lock-free load and one hash probe, zero-copy
  when English is active. Don't cache its result across frames (it would
  miss a language change); do keep `tr!`-with-placeholders out of per-row
  hot paths where a `format!` wasn't already there.

### `locales/en.json` is generated

`ferail_core::i18n::extract` scans every `crates/**/*.rs` for
`tr!`/`trc!`/`trn!`/`msgid!` literals (including `#[cfg]`-gated platform code)
and renders `locales/en.json`. The test `i18n::extract::tests::en_json_is_up_to_date`
fails when the file is stale:

```sh
FERAIL_I18N_UPDATE=1 cargo test -p ferail-core i18n::extract
```

Coverage numbers, the `untranslated` list of a template, and import
validation all derive from that file. `bundled_packs_parse` checks the
shipped packs still parse; `validate()` warns about stale keys and
placeholder drift so a rewording in the code shows up as "N of M
translated" rather than silently wrong text.

### Runtime

- The active `Catalog` sits in an `ArcSwap`; `set_active` swaps it. English
  is a catalog with no entries, so `tr!` returns the `&'static str` itself.
- `ferail_gpui::i18n::init` runs at boot before any window: reads the
  persisted choice (`app_state.language`: `system` / `en` / code), probes the
  OS locale (`sys-locale`), scans bundled + user packs, and installs the
  catalog synchronously — the same class of startup read as `app_state::load`,
  and it avoids a flash of English.
- Every later change (`set_selection`, `reload`, import) scans/parses on the
  background executor and lands through `cx.update`, guarded by a generation
  counter; then `install` swaps the catalog, calls `gpui_component::set_locale`
  (so the widgets' own OK/Cancel follow where gpui-component has them),
  rebuilds the menu bar (`boot::install_app_menus`) and `refresh_windows()`.
- Because menus, the command palette and the shortcuts page all translate
  `CommandSpec.title` at display time, the catalogue itself stays static.

## Known gaps / follow-ups

- Backend error messages (`ferail-fs-native`, `ferail-archive`) and the
  failure-report bodies are English by design for now.
- `trn!` plural support is CLDR-correct for the languages listed in
  `plural.rs`; unlisted ones get the English one/other rule.
- RTL layout (Arabic, Hebrew) is not mirrored — gpui has no RTL layout
  support; text renders correctly but alignment stays LTR.
- Dates, sizes and numbers are not locale-formatted.
- gpui-component's built-in widget strings only exist for the locales the
  library ships (en, zh-CN, zh-HK, it); other languages see English there.
- No in-app LLM call: deliberately, to avoid API-key handling. The
  export/import round trip is the supported path; an in-app provider could
  layer on top later without changing the format.
