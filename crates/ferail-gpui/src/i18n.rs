//! App-side localization: the `tr!` family for GPUI code, the language
//! registry (bundled packs + the user's `languages/` folder), switching the
//! active language at runtime, and the import / export / new-language
//! operations behind Settings › Appearance › Language.
//!
//! The string machinery itself lives in [`ferail_core::i18n`]; this module
//! only adapts it to gpui (`SharedString`, globals, background loading,
//! menus). See `docs/features/LOCALIZATION.md`.
//!
//! Prime Directive notes: reading the languages folder, parsing a pack,
//! and writing files all run on the background executor; the render path
//! only reads the [`Languages`] global and the lock-free active catalog.
//! The one deliberate exception is [`init`], which loads the persisted
//! language synchronously during boot, before any window exists, same
//! class of startup read as `app_state::load`.

use std::path::PathBuf;

use ferail_core::i18n::{self as core, Catalog, LanguagePack, Text, ValidationReport};
use gpui::{App, AsyncApp, Global, SharedString, Window};

use crate::app_state;

// =============================================================================
// Text -> SharedString, and the macros
// =============================================================================

/// Convert a core [`Text`] to gpui's string type without copying: the
/// English literal stays `&'static str`, a translation shares its `Arc`.
pub fn sh(t: Text) -> SharedString {
    match t {
        Text::Static(s) => SharedString::new_static(s),
        Text::Shared(a) => SharedString::from(a),
    }
}

/// `tr!("Empty folder")` / `tr!("Copied {n} files", n = count)` →
/// `SharedString`. Same forms as [`ferail_core::tr!`].
#[macro_export]
macro_rules! tr {
    ($($t:tt)*) => {
        $crate::i18n::sh(::ferail_core::tr!($($t)*))
    };
}

/// `trc!("menu", "Open")` → `SharedString`.
#[macro_export]
macro_rules! trc {
    ($($t:tt)*) => {
        $crate::i18n::sh(::ferail_core::trc!($($t)*))
    };
}

/// `trn!("{n} file", "{n} files", count)` → `SharedString`.
#[macro_export]
macro_rules! trn {
    ($($t:tt)*) => {
        $crate::i18n::sh(::ferail_core::trn!($($t)*))
    };
}

/// Translate a `&'static str` msgid that came from a `msgid!` table (the
/// command catalogue, option-label tables).
pub fn tr_static(msgid: &'static str) -> SharedString {
    sh(core::tr_raw(msgid))
}

/// Translate a runtime string (e.g. an entry's cached `display_kind`) when
/// the active pack knows it, else hand back the string itself. Render-safe:
/// `None` fast-path under English, one hash probe otherwise.
pub fn tr_dyn(text: &str) -> SharedString {
    match core::tr_dynamic(text) {
        Some(t) => SharedString::from(t),
        None => SharedString::from(text.to_owned()),
    }
}

/// Like [`tr_dyn`] but keeps an already-owned `SharedString` when there is
/// no translation (no copy).
pub fn tr_dyn_shared(text: SharedString) -> SharedString {
    match core::tr_dynamic(&text) {
        Some(t) => SharedString::from(t),
        None => text,
    }
}

// =============================================================================
// Registry
// =============================================================================

/// Selection value meaning "follow the OS language".
pub const SYSTEM: &str = "system";
/// Selection value / code of the built-in source language.
pub const ENGLISH: &str = "en";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Compiled into the binary (`locales/*.json`).
    Bundled,
    /// A file in the user's languages folder.
    User,
}

/// One installed language, as listed in Settings.
#[derive(Clone, Debug)]
pub struct LanguageInfo {
    pub code: String,
    /// The language's own name (`Français`); falls back to the code.
    pub name: String,
    pub english_name: String,
    pub translated: usize,
    pub total: usize,
    pub origin: Origin,
    /// The file, for user packs.
    pub path: Option<PathBuf>,
}

impl LanguageInfo {
    fn from_pack(pack: &LanguagePack, origin: Origin, path: Option<PathBuf>) -> Self {
        let (translated, total) = pack.coverage();
        Self {
            code: pack.code.clone(),
            name: if pack.name.trim().is_empty() {
                pack.code.clone()
            } else {
                pack.name.clone()
            },
            english_name: pack.english_name.clone(),
            translated,
            total,
            origin,
            path,
        }
    }

    /// `Français (87 %)`.
    pub fn label(&self) -> String {
        if self.total == 0 {
            return self.name.clone();
        }
        let pct = (self.translated * 100) / self.total;
        format!("{} ({pct} %)", self.name)
    }
}

/// Process-wide language state, read by the settings page. Mutated only on
/// the UI thread; the expensive parts (scans, parsing) happen off-thread and
/// land here through `cx.update`.
#[derive(Default)]
pub struct Languages {
    /// Installed packs, one per code (a user pack hides a bundled one with
    /// the same code), sorted by English name.
    pub packs: Vec<LanguageInfo>,
    /// What the user chose: [`SYSTEM`], [`ENGLISH`], or a pack code.
    pub selection: String,
    /// Code of the catalog currently installed (`"en"` when nothing is).
    pub active: String,
    /// The OS UI language, probed once at boot.
    pub system_locale: Option<String>,
    /// Bumped on every (re)load so a slow load can't overwrite a newer
    /// choice.
    generation: u64,
}

impl Global for Languages {}

/// Read access for render code. Before [`init`] ran (headless harnesses,
/// tests) this is an empty registry rather than a panic.
pub fn languages(cx: &App) -> &Languages {
    static EMPTY: Languages = Languages {
        packs: Vec::new(),
        selection: String::new(),
        active: String::new(),
        system_locale: None,
        generation: 0,
    };
    cx.try_global::<Languages>().unwrap_or(&EMPTY)
}

/// Mutable access; installs an empty registry if [`init`] never ran.
fn languages_mut(cx: &mut App) -> &mut Languages {
    if !cx.has_global::<Languages>() {
        cx.set_global(Languages {
            selection: SYSTEM.to_owned(),
            active: ENGLISH.to_owned(),
            ..Default::default()
        });
    }
    cx.global_mut::<Languages>()
}

/// The folder user language packs live in (`<config_dir>/languages`).
/// Not created here: see [`ensure_user_dir`].
pub fn user_dir() -> Option<PathBuf> {
    app_state::config_dir().map(|d| d.join("languages"))
}

fn ensure_user_dir() -> Result<PathBuf, String> {
    let dir = user_dir().ok_or_else(|| "no configuration directory available".to_owned())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Languages offered by "New language…" (code, native name, English name).
/// Anything else: copy an existing file and edit its `code`/`name`.
pub const PRESET_LANGUAGES: &[(&str, &str, &str)] = &[
    ("ar", "العربية", "Arabic"),
    ("eu", "Euskara", "Basque"),
    ("ca", "Català", "Catalan"),
    ("zh-Hans", "简体中文", "Chinese (Simplified)"),
    ("zh-Hant", "繁體中文", "Chinese (Traditional)"),
    ("cs", "Čeština", "Czech"),
    ("da", "Dansk", "Danish"),
    ("nl", "Nederlands", "Dutch"),
    ("fi", "Suomi", "Finnish"),
    ("fr", "Français", "French"),
    ("gl", "Galego", "Galician"),
    ("de", "Deutsch", "German"),
    ("el", "Ελληνικά", "Greek"),
    ("he", "עברית", "Hebrew"),
    ("hi", "हिन्दी", "Hindi"),
    ("hu", "Magyar", "Hungarian"),
    ("id", "Bahasa Indonesia", "Indonesian"),
    ("it", "Italiano", "Italian"),
    ("ja", "日本語", "Japanese"),
    ("ko", "한국어", "Korean"),
    ("lb", "Lëtzebuergesch", "Luxembourgish"),
    ("nb", "Norsk bokmål", "Norwegian Bokmål"),
    ("pl", "Polski", "Polish"),
    ("pt-BR", "Português (Brasil)", "Portuguese (Brazil)"),
    ("pt-PT", "Português (Portugal)", "Portuguese (Portugal)"),
    ("ro", "Română", "Romanian"),
    ("ru", "Русский", "Russian"),
    ("sk", "Slovenčina", "Slovak"),
    ("es", "Español", "Spanish"),
    ("sv", "Svenska", "Swedish"),
    ("th", "ไทย", "Thai"),
    ("tr", "Türkçe", "Turkish"),
    ("uk", "Українська", "Ukrainian"),
    ("vi", "Tiếng Việt", "Vietnamese"),
];

// =============================================================================
// Boot + switching
// =============================================================================

/// Install the persisted language before the first window renders. Reads
/// at most the bundled packs (embedded) and the user's languages folder:
/// a startup read of the same kind as `app_state::load`.
pub fn init(cx: &mut App) {
    // `FERAIL_LANGUAGE=fr` overrides the persisted choice for this process
    // only (screenshots, testing a pack without touching settings).
    let selection = std::env::var("FERAIL_LANGUAGE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| app_state::load().language)
        .unwrap_or_else(|| SYSTEM.to_owned());
    let system_locale = core::system_locale();
    let packs = scan_packs();
    let mut langs = Languages {
        packs,
        selection,
        active: ENGLISH.to_owned(),
        system_locale,
        generation: 0,
    };
    if let Some(info) = resolve(
        &langs.selection,
        &langs.packs,
        langs.system_locale.as_deref(),
    ) {
        match load_catalog(&info) {
            Ok(catalog) => {
                langs.active = info.code.clone();
                core::set_active(catalog);
                gpui_component::set_locale(&info.code);
            }
            Err(e) => crate::log_warn!(90, "language {}: {e}", info.code),
        }
    }
    crate::log_info!(
        90,
        "i18n: selection={} active={} system={:?} packs={}",
        langs.selection,
        langs.active,
        langs.system_locale,
        langs.packs.len()
    );
    cx.set_global(langs);
}

/// Persist and apply a new selection ([`SYSTEM`], [`ENGLISH`], or a code).
pub fn set_selection(selection: &str, cx: &mut App) {
    let mut state = app_state::load();
    state.language = Some(selection.to_owned());
    app_state::save(&state);
    languages_mut(cx).selection = selection.to_owned();
    apply(cx);
}

/// Re-scan the languages folder (background) and re-apply the selection,
/// after an import, a new template, or the user editing files by hand.
pub fn reload(cx: &mut App) {
    let generation = {
        let langs = languages_mut(cx);
        langs.generation += 1;
        langs.generation
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let packs = cx
            .background_executor()
            .spawn(async move { scan_packs() })
            .await;
        cx.update(|cx| {
            let langs = languages_mut(cx);
            if langs.generation != generation {
                return;
            }
            langs.packs = packs;
            apply(cx);
        });
    })
    .detach();
}

/// Resolve the current selection against the known packs and install the
/// catalog (loading it off-thread). English is immediate.
fn apply(cx: &mut App) {
    let (selection, packs, system_locale, generation) = {
        let langs = languages_mut(cx);
        langs.generation += 1;
        (
            langs.selection.clone(),
            langs.packs.clone(),
            langs.system_locale.clone(),
            langs.generation,
        )
    };
    let Some(info) = resolve(&selection, &packs, system_locale.as_deref()) else {
        install(Catalog::english(), cx);
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        let loaded = cx
            .background_executor()
            .spawn(async move { load_catalog(&info) })
            .await;
        cx.update(|cx| {
            if languages(cx).generation != generation {
                return; // superseded by a newer choice
            }
            match loaded {
                Ok(catalog) => install(catalog, cx),
                Err(e) => crate::log_warn!(90, "language load failed: {e}"),
            }
        });
    })
    .detach();
}

/// Swap the active catalog and refresh everything that shows text: every
/// window repaints, and the menu bar is rebuilt because its titles are
/// captured when it is installed.
fn install(catalog: Catalog, cx: &mut App) {
    let code = catalog.code().to_owned();
    core::set_active(catalog);
    gpui_component::set_locale(&code);
    languages_mut(cx).active = code;
    crate::boot::install_app_menus(cx);
    cx.refresh_windows();
}

/// Which pack a selection means. `None` = English.
fn resolve(
    selection: &str,
    packs: &[LanguageInfo],
    system_locale: Option<&str>,
) -> Option<LanguageInfo> {
    match selection {
        ENGLISH => None,
        SYSTEM => {
            let locale = system_locale?;
            // "zh-Hans-CN" → try "zh-Hans-CN", "zh-Hans", "zh".
            let tag = locale.replace('_', "-");
            let parts: Vec<&str> = tag.split('-').collect();
            for len in (1..=parts.len()).rev() {
                let candidate = parts[..len].join("-");
                if let Some(p) = packs
                    .iter()
                    .find(|p| p.code.eq_ignore_ascii_case(&candidate))
                {
                    return Some(p.clone());
                }
            }
            // Last resort: any pack whose language subtag matches.
            let lang = core::language_subtag(&tag).to_ascii_lowercase();
            packs
                .iter()
                .find(|p| core::language_subtag(&p.code).eq_ignore_ascii_case(&lang))
                .cloned()
        }
        code => packs.iter().find(|p| p.code == code).cloned(),
    }
}

/// Read + parse + index one pack. Background work.
fn load_catalog(info: &LanguageInfo) -> Result<Catalog, String> {
    let pack = match (&info.origin, &info.path) {
        (Origin::User, Some(path)) => {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            LanguagePack::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?
        }
        _ => core::pack::bundled(&info.code)
            .ok_or_else(|| format!("no bundled pack {}", info.code))?,
    };
    Ok(Catalog::from_pack(&pack))
}

/// Bundled packs plus everything parseable in the user folder; a user pack
/// replaces a bundled one with the same code. Background work (directory
/// listing + file reads).
fn scan_packs() -> Vec<LanguageInfo> {
    let mut out: Vec<LanguageInfo> = Vec::new();
    for code in core::pack::bundled_codes() {
        if let Some(pack) = core::pack::bundled(code) {
            out.push(LanguageInfo::from_pack(&pack, Origin::Bundled, None));
        }
    }
    if let Some(dir) = user_dir() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                })
                .collect();
            files.sort();
            for path in files {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                match LanguagePack::parse(&text) {
                    Ok(pack) => {
                        let info = LanguageInfo::from_pack(&pack, Origin::User, Some(path));
                        out.retain(|p| p.code != info.code);
                        out.push(info);
                    }
                    Err(e) => crate::log_warn!(90, "ignoring {}: {e}", path.display()),
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.english_name
            .to_lowercase()
            .cmp(&b.english_name.to_lowercase())
            .then(a.code.cmp(&b.code))
    });
    out
}

// =============================================================================
// Operations (Settings buttons)
// =============================================================================

/// Outcome of an import / export / template operation, for the toast.
pub enum Outcome {
    Ok {
        headline: SharedString,
        details: Option<String>,
    },
    Err {
        headline: SharedString,
        details: Option<String>,
    },
}

/// Show an [`Outcome`] as a toast in `window`. Details, when present, are
/// revealed on demand and can be copied, same shape as the shell's error
/// toasts.
pub fn notify(outcome: Outcome, window: &mut Window, cx: &mut App) {
    use gpui_component::WindowExt as _;
    use gpui_component::notification::Notification;
    let note = match outcome {
        Outcome::Ok {
            headline,
            details: None,
        } => Notification::success(headline),
        Outcome::Ok {
            headline,
            details: Some(d),
        } => with_details(Notification::warning(headline), d),
        Outcome::Err {
            headline,
            details: None,
        } => Notification::error(headline),
        Outcome::Err {
            headline,
            details: Some(d),
        } => with_details(Notification::error(headline), d),
    };
    window.push_notification(note, cx);
}

fn with_details(
    note: gpui_component::notification::Notification,
    details: String,
) -> gpui_component::notification::Notification {
    use crate::text::TextScale as _;
    use gpui::prelude::*;
    use gpui::{div, px};
    use gpui_component::{ActiveTheme as _, Sizable as _, button::Button, h_flex, v_flex};
    let details = std::rc::Rc::new(details);
    note.autohide(false).content(move |_note, _window, cx| {
        let copy = details.clone();
        v_flex()
            .gap_1()
            .pt_1()
            .child(
                div()
                    .id("i18n-details")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .text_scale_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(details.as_str().to_owned())),
            )
            .child(
                h_flex().gap_2().child(
                    Button::new("i18n-copy-details")
                        .label(tr!("Copy"))
                        .small()
                        .on_click(move |_, _, _| crate::platform_shell::copy_to_clipboard(&copy)),
                ),
            )
            .into_any_element()
    })
}

/// "Import…": pick a `.json` pack, validate it, copy it into the languages
/// folder, switch to it.
pub fn import_file(window: &mut Window, cx: &mut App) {
    let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(tr!("Import")),
    });
    window
        .spawn(cx, async move |cx| {
            let Some(path) = rx
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|v| v.into_iter().next())
            else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { import_path(&path) })
                .await;
            let _ = cx.update(|window, cx| match result {
                Ok((info, report)) => {
                    let headline = tr!(
                        "Imported {language}: {translated} of {total} strings translated",
                        language = info.name,
                        translated = ferail_core::counts::format_count(report.translated as u64),
                        total = ferail_core::counts::format_count(report.total as u64)
                    );
                    let details = report.has_warnings().then(|| report.details());
                    notify(Outcome::Ok { headline, details }, window, cx);
                    set_selection(&info.code, cx);
                    reload(cx);
                }
                Err(e) => notify(
                    Outcome::Err {
                        headline: tr!("Couldn't import that language pack"),
                        details: Some(e),
                    },
                    window,
                    cx,
                ),
            });
        })
        .detach();
}

/// Background half of [`import_file`].
fn import_path(path: &std::path::Path) -> Result<(LanguageInfo, ValidationReport), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut pack = LanguagePack::parse(&text)?;
    if pack.code == ENGLISH {
        return Err("this is the English source catalog, not a translation".to_owned());
    }
    let report = pack.validate();
    // Strip the translator scaffolding: an imported file is a pack now.
    pack.instructions.clear();
    pack.untranslated.clear();
    let dir = ensure_user_dir()?;
    let dest = dir.join(format!("{}.json", pack.code));
    write_atomic(&dest, &pack.to_json())?;
    Ok((
        LanguageInfo::from_pack(&pack, Origin::User, Some(dest)),
        report,
    ))
}

/// "Export…": save the selected language as a translation template (its
/// translations plus everything still untranslated, with instructions),
/// for handing to a translator/LLM or contributing back.
pub fn export_current(window: &mut Window, cx: &mut App) {
    let langs = languages(cx);
    let info = resolve(
        &langs.selection,
        &langs.packs,
        langs.system_locale.as_deref(),
    );
    let (code, suggested) = match &info {
        Some(i) => (i.code.clone(), format!("ferail-{}.json", i.code)),
        None => (ENGLISH.to_owned(), "ferail-en.json".to_owned()),
    };
    let start_dir = dirs_download_or_home();
    let rx = cx.prompt_for_new_path(&start_dir, Some(&suggested));
    window
        .spawn(cx, async move |cx| {
            let Some(dest) = rx.await.ok().and_then(Result::ok).flatten() else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { export_to(&info, &code, &dest) })
                .await;
            let _ = cx.update(|window, cx| match result {
                Ok(dest) => notify(
                    Outcome::Ok {
                        headline: tr!("Saved {file}", file = dest.display()),
                        details: None,
                    },
                    window,
                    cx,
                ),
                Err(e) => notify(
                    Outcome::Err {
                        headline: tr!("Couldn't export the language pack"),
                        details: Some(e),
                    },
                    window,
                    cx,
                ),
            });
        })
        .detach();
}

fn export_to(
    info: &Option<LanguageInfo>,
    code: &str,
    dest: &std::path::Path,
) -> Result<PathBuf, String> {
    let pack = match info {
        Some(i) => load_pack(i)?,
        None => core::pack::source().clone(),
    };
    let text = if code == ENGLISH {
        pack.to_json()
    } else {
        pack.template().to_json()
    };
    write_atomic(dest, &text)?;
    Ok(dest.to_path_buf())
}

fn load_pack(info: &LanguageInfo) -> Result<LanguagePack, String> {
    match (&info.origin, &info.path) {
        (Origin::User, Some(path)) => {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            LanguagePack::parse(&text)
        }
        _ => {
            core::pack::bundled(&info.code).ok_or_else(|| format!("no bundled pack {}", info.code))
        }
    }
}

/// "New language…": write an empty template for `code` into the languages
/// folder and reveal it, ready to be handed to a translator or an AI chat.
pub fn create_template(
    code: &str,
    name: &str,
    english_name: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let (code, name, english_name) = (code.to_owned(), name.to_owned(), english_name.to_owned());
    window
        .spawn(cx, async move |cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let dir = ensure_user_dir()?;
                    let dest = dir.join(format!("{code}.json"));
                    if dest.exists() {
                        return Err(format!("{} already exists", dest.display()));
                    }
                    let pack = LanguagePack::empty(&code, &name, &english_name).template();
                    write_atomic(&dest, &pack.to_json())?;
                    Ok::<PathBuf, String>(dest)
                })
                .await;
            let _ = cx.update(|window, cx| match result {
                Ok(dest) => {
                    notify(
                        Outcome::Ok {
                            headline: tr!(
                                "Created {file}. Give it to a translator or an AI assistant, then use Import…",
                                file = dest.display()
                            ),
                            details: None,
                        },
                        window,
                        cx,
                    );
                    crate::platform_shell::reveal_in_finder(&dest);
                    reload(cx);
                }
                Err(e) => notify(
                    Outcome::Err { headline: tr!("Couldn't create the language file"), details: Some(e) },
                    window,
                    cx,
                ),
            });
        })
        .detach();
}

/// "Show folder": open the languages folder (creating it) in the file
/// manager.
pub fn reveal_folder(cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let dir = cx
            .background_executor()
            .spawn(async move { ensure_user_dir() })
            .await;
        cx.update(|_cx| match dir {
            Ok(dir) => crate::platform_shell::reveal_in_finder(&dir),
            Err(e) => crate::log_warn!(90, "languages folder: {e}"),
        });
    })
    .detach();
}

/// "Copy instructions": the translator brief for the selected language.
pub fn copy_instructions(cx: &App) {
    let langs = languages(cx);
    let info = resolve(
        &langs.selection,
        &langs.packs,
        langs.system_locale.as_deref(),
    );
    let language = info
        .as_ref()
        .map(|i| {
            if i.english_name.is_empty() {
                i.code.clone()
            } else {
                i.english_name.clone()
            }
        })
        .unwrap_or_else(|| "the target language".to_owned());
    crate::platform_shell::copy_to_clipboard(&core::pack::instructions_for(&language));
}

fn write_atomic(dest: &std::path::Path, text: &str) -> Result<(), String> {
    let tmp = dest.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

fn dirs_download_or_home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let dl = PathBuf::from(&home).join("Downloads");
        return if dl.is_dir() { dl } else { PathBuf::from(home) };
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(code: &str, origin: Origin) -> LanguageInfo {
        LanguageInfo {
            code: code.into(),
            name: code.into(),
            english_name: code.into(),
            translated: 1,
            total: 1,
            origin,
            path: None,
        }
    }

    #[test]
    fn resolve_selection() {
        let packs = vec![
            info("fr", Origin::Bundled),
            info("pt-BR", Origin::User),
            info("zh-Hans", Origin::User),
        ];
        assert!(resolve(ENGLISH, &packs, Some("fr-FR")).is_none());
        assert_eq!(resolve("fr", &packs, None).unwrap().code, "fr");
        assert!(resolve("xx", &packs, None).is_none());
        // System: exact, then shorter prefixes, then language subtag.
        assert_eq!(resolve(SYSTEM, &packs, Some("fr-CH")).unwrap().code, "fr");
        assert_eq!(
            resolve(SYSTEM, &packs, Some("zh-Hans-CN")).unwrap().code,
            "zh-Hans"
        );
        assert_eq!(
            resolve(SYSTEM, &packs, Some("pt_PT")).unwrap().code,
            "pt-BR"
        );
        assert!(resolve(SYSTEM, &packs, Some("en-US")).is_none());
        assert!(resolve(SYSTEM, &packs, None).is_none());
    }

    #[test]
    fn label_shows_coverage() {
        let mut i = info("fr", Origin::Bundled);
        i.name = "Français".into();
        i.translated = 87;
        i.total = 100;
        assert_eq!(i.label(), "Français (87 %)");
    }
}
