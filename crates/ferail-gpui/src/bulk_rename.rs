//! Bulk rename with pattern rules (docs/features/BULK_RENAME.md).
//!
//! Two layers in one file:
//!
//! 1. **A pure transform engine** — [`RenameRule`] → [`build_plan`] →
//!    [`RenamePlan`]. Plain string work over *display names*: no gpui
//!    types, no filesystem, unit-tested below. The caller maps display
//!    names back to on-disk leaves at apply time.
//! 2. **The modal UI + apply pipeline** — [`open`] shows a dialog over a
//!    selection snapshot with a live before→after preview;
//!    [`apply`] runs the renames chain/cycle-aware on the background
//!    executor and records a [`UndoOp::RenameBatch`].
//!
//! Re-render model: gpui-component rebuilds every active dialog's
//! builder closure each `Root` render, so the closure stays a cheap
//! frame around one [`BulkRenameView`] entity child — the whole body
//! renders from the entity's *cached* plan and re-renders when the
//! entity notifies. Plan recomputation happens on semantic events only
//! (input Change, toggle clicks): synchronously for typical selections,
//! and on the background executor past [`BACKGROUND_PLAN_THRESHOLD`]
//! items (generation-tagged so a stale result is dropped) — no debounce
//! needed since the plan is pure string work and the `regex` crate is
//! linear-time.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, Hsla, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::shell::{Shell, UndoOp};
use crate::text::TextScale as _;

// ---------------------------------------------------------------------
// Layer 1: pure transform engine (no gpui, no I/O).
// ---------------------------------------------------------------------

/// Case transform applied to the *stem* only; the extension is never
/// case-folded.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaseTransform {
    #[default]
    None,
    Lower,
    Upper,
    /// First letter of each whitespace-separated word uppercased, the
    /// rest lowered. Unicode-aware via `char::to_uppercase` /
    /// `char::to_lowercase` (no extra dependency).
    Title,
}

/// One rule set, applied per file in three stages:
/// find/replace → case transform (stem only) → template.
#[derive(Clone, Debug)]
pub struct RenameRule {
    /// Empty ⇒ the find/replace stage is skipped.
    pub find: String,
    /// Regex mode supports `$1`..`$9` capture references.
    pub replace: String,
    pub use_regex: bool,
    pub case: CaseTransform,
    /// Empty/whitespace ⇒ no template stage. Tokens: `{name}` (post-
    /// transform stem), `{ext}` (extension without the dot, `""` when
    /// none), `{n}` (`counter_start + item index`, zero-padded to
    /// `counter_pad`), `{date}` (file mtime as `YYYY-MM-DD`, UTC).
    /// A template without `{ext}` gets `.{ext}` appended automatically
    /// when the file had an extension, so a template can't silently
    /// strip extensions.
    pub template: String,
    pub counter_start: u32,
    /// Zero-padding width for `{n}`, e.g. `3` → `007`.
    pub counter_pad: u8,
}

impl Default for RenameRule {
    fn default() -> Self {
        Self {
            find: String::new(),
            replace: String::new(),
            use_regex: false,
            case: CaseTransform::None,
            template: String::new(),
            counter_start: 1,
            counter_pad: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowStatus {
    /// `after == before` — skipped at apply, not a conflict.
    Unchanged,
    Renamed,
    /// Static reason, rendered next to the row in the preview.
    Conflict(&'static str),
}

#[derive(Clone, Debug)]
pub struct RenameRow {
    pub before: String,
    pub after: String,
    pub status: RowStatus,
}

#[derive(Clone, Debug, Default)]
pub struct RenamePlan {
    pub rows: Vec<RenameRow>,
    pub renamed: usize,
    pub conflicts: usize,
    /// Plan-level error (an invalid regex pattern) — one line for the
    /// whole plan, never repeated per row. All rows read Unchanged.
    pub error: Option<String>,
}

/// Build the before→after plan for `items` (`(display name, mtime unix)`
/// pairs, in batch order — `{n}` numbers by that order).
///
/// Conflicts detected here, all case-insensitive (APFS/NTFS default to
/// case-insensitive):
/// (a) duplicate targets *within* the batch;
/// (b) an empty result;
/// (c) a target that equals another batch member's *unchanged* name —
///     i.e. it collides with an item that isn't being renamed away.
/// A changed row's target may equal another *changed* row's old name
/// (chains/renumbering); the apply step orders those correctly.
/// On-disk collisions with non-batch siblings can't be seen here — the
/// apply step's guarded rename reports those per item.
pub fn build_plan(rule: &RenameRule, items: &[(String, i64)]) -> RenamePlan {
    // Stage-1 pattern compiles once; a bad pattern is one plan-level
    // error, not a per-row failure.
    let regex = if rule.use_regex && !rule.find.is_empty() {
        match regex::Regex::new(&rule.find) {
            Ok(re) => Some(re),
            Err(e) => {
                let msg = e.to_string();
                let condensed: String = msg.split_whitespace().collect::<Vec<_>>().join(" ");
                return RenamePlan {
                    rows: items
                        .iter()
                        .map(|(name, _)| RenameRow {
                            before: name.clone(),
                            after: name.clone(),
                            status: RowStatus::Unchanged,
                        })
                        .collect(),
                    renamed: 0,
                    conflicts: 0,
                    error: Some(tr!("Invalid regex: {detail}", detail = condensed).to_string()),
                };
            }
        }
    } else {
        None
    };

    let template = rule.template.trim();
    let mut afters: Vec<String> = Vec::with_capacity(items.len());
    for (index, (name, mtime)) in items.iter().enumerate() {
        // 1. find/replace over the whole display name. An empty find is
        // a no-op (both `str::replace` and `Regex::new("")` would
        // otherwise inject the replacement between every character).
        let replaced: String = if rule.find.is_empty() {
            name.clone()
        } else if let Some(re) = &regex {
            re.replace_all(name, rule.replace.as_str()).into_owned()
        } else {
            name.replace(&rule.find, &rule.replace)
        };
        // 2. case transform on the stem only.
        let (stem, ext) = split_stem_ext(&replaced);
        let stem = apply_case(stem, rule.case);
        let ext = ext.map(str::to_owned);
        // 3. template stage.
        let after = if template.is_empty() {
            match &ext {
                Some(e) => format!("{stem}.{e}"),
                None => stem,
            }
        } else {
            let n = rule.counter_start as u64 + index as u64;
            let counter = format!("{:0width$}", n, width = rule.counter_pad as usize);
            let mut out = template
                .replace("{name}", &stem)
                .replace("{ext}", ext.as_deref().unwrap_or(""))
                .replace("{n}", &counter)
                .replace("{date}", &format_date(*mtime));
            // Extension guard: a template that never mentions {ext}
            // keeps the file's extension instead of silently
            // stripping it.
            if !template.contains("{ext}") {
                if let Some(e) = ext.as_deref().filter(|e| !e.is_empty()) {
                    out.push('.');
                    out.push_str(e);
                }
            }
            out
        };
        afters.push(after);
    }

    // Names still occupied after the batch applies: members that are
    // NOT being renamed away. A changed row targeting one of these is a
    // real collision; targeting a *changed* row's old name is a chain
    // the apply step resolves in dependency order.
    let unchanged_lower: std::collections::HashSet<String> = items
        .iter()
        .zip(afters.iter())
        .filter(|((before, _), after)| *after == before)
        .map(|((before, _), _)| before.to_lowercase())
        .collect();
    let mut target_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for ((before, _), after) in items.iter().zip(afters.iter()) {
        if after != before {
            *target_counts.entry(after.to_lowercase()).or_default() += 1;
        }
    }

    let mut rows: Vec<RenameRow> = Vec::with_capacity(items.len());
    let mut renamed = 0;
    let mut conflicts = 0;
    for ((before, _), after) in items.iter().zip(afters.iter()) {
        let status = if after == before {
            RowStatus::Unchanged
        } else if after.trim().is_empty() {
            RowStatus::Conflict(ferail_core::msgid!("empty name"))
        } else if target_counts
            .get(&after.to_lowercase())
            .copied()
            .unwrap_or(0)
            > 1
        {
            RowStatus::Conflict(ferail_core::msgid!("duplicate target"))
        } else if unchanged_lower.contains(&after.to_lowercase()) {
            RowStatus::Conflict(ferail_core::msgid!("name already taken"))
        } else {
            RowStatus::Renamed
        };
        match status {
            RowStatus::Renamed => renamed += 1,
            RowStatus::Conflict(_) => conflicts += 1,
            RowStatus::Unchanged => {}
        }
        rows.push(RenameRow {
            before: before.clone(),
            after: after.clone(),
            status,
        });
    }
    RenamePlan {
        rows,
        renamed,
        conflicts,
        error: None,
    }
}

/// Last-dot stem/extension split on a leaf name. Names without a dot
/// are all stem; a *leading* dot (`.gitignore`) is part of the stem,
/// not an extension separator.
fn split_stem_ext(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], Some(&name[i + 1..])),
        _ => (name, None),
    }
}

fn apply_case(stem: &str, case: CaseTransform) -> String {
    match case {
        CaseTransform::None => stem.to_string(),
        CaseTransform::Lower => stem.to_lowercase(),
        CaseTransform::Upper => stem.to_uppercase(),
        CaseTransform::Title => {
            let mut out = String::with_capacity(stem.len());
            let mut at_word_start = true;
            for ch in stem.chars() {
                if ch.is_whitespace() {
                    at_word_start = true;
                    out.push(ch);
                } else if at_word_start {
                    out.extend(ch.to_uppercase());
                    at_word_start = false;
                } else {
                    out.extend(ch.to_lowercase());
                }
            }
            out
        }
    }
}

/// `{date}` token: mtime as `YYYY-MM-DD` (UTC), from plain proleptic-
/// Gregorian math — same approach as `crate::prefetch`'s ISO formatter
/// (deliberately *copied*, not imported: this layer stays free of the
/// UI crate's other modules so it tests standalone).
fn format_date(mtime_unix: i64) -> String {
    let days = mtime_unix.div_euclid(86_400);
    let (y, m, d) = epoch_days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert "days since 1970-01-01" into (year, month, day).
fn epoch_days_to_ymd(days: i64) -> (i32, u32, u32) {
    let mut y: i32 = 1970;
    let mut d: i64 = days;
    loop {
        let y_days = if is_leap(y) { 366 } else { 365 };
        if d < y_days {
            break;
        }
        d -= y_days;
        y += 1;
    }
    while d < 0 {
        y -= 1;
        let y_days = if is_leap(y) { 366 } else { 365 };
        d += y_days;
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo: u32 = 1;
    for &md in &months {
        if d < md {
            break;
        }
        d -= md;
        mo += 1;
    }
    (y, mo, (d + 1) as u32)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// ---------------------------------------------------------------------
// Filesystem worker: chain/cycle-aware batch rename.
// ---------------------------------------------------------------------

/// Apply `(old, new)` renames, continuing past failures. Returns the
/// successfully renamed `(original, new)` pairs (in the order they
/// applied — the undo record) plus per-item error strings.
///
/// Renumbering batches routinely rename *through* each other's names
/// (`3.jpg → 4.jpg` next to `2.jpg → 3.jpg`), so pairs apply in
/// dependency order: a pair waits until the pair occupying its
/// destination has vacated. Pure cycles (a two-file swap) are broken by
/// parking one file under a temporary sibling name. Each individual
/// rename is guarded against clobbering an existing entry (the one
/// exception: a case-only rename of the same file on a case-insensitive
/// filesystem).
///
/// Runs real filesystem I/O — background executor only.
pub(crate) fn run_renames(
    pairs: Vec<(PathBuf, PathBuf)>,
) -> (Vec<(PathBuf, PathBuf)>, Vec<String>) {
    use std::collections::HashMap;
    let n = pairs.len();
    let lower = |p: &Path| p.to_string_lossy().to_lowercase();
    let mut src_of: HashMap<String, usize> = HashMap::with_capacity(n);
    for (i, (old, _)) in pairs.iter().enumerate() {
        src_of.insert(lower(old), i);
    }
    // waiters[j] = pairs whose destination is j's (still occupied)
    // source; they become ready when j vacates it.
    let mut waiters: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut ready: Vec<usize> = Vec::new();
    for (i, (_, new)) in pairs.iter().enumerate() {
        match src_of.get(&lower(new)) {
            Some(&j) if j != i => waiters.entry(j).or_default().push(i),
            // Self-match is a case-only rename; the guard handles it.
            _ => ready.push(i),
        }
    }
    let mut cur_src: Vec<PathBuf> = pairs.iter().map(|(old, _)| old.clone()).collect();
    let mut done = vec![false; n];
    let mut parked = vec![false; n];
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut remaining = n;
    let mut tmp_seq = 0usize;
    while remaining > 0 {
        if let Some(i) = ready.pop() {
            let dest = pairs[i].1.clone();
            match rename_guarded(&cur_src[i], &dest) {
                Ok(()) => renamed.push((pairs[i].0.clone(), dest)),
                Err(e) => errors.push(e),
            }
            done[i] = true;
            remaining -= 1;
            // Success or not, i's claim is settled; dependents try next
            // (a failed vacate surfaces as their own guard error).
            if let Some(ws) = waiters.remove(&i) {
                ready.extend(ws.into_iter().filter(|&w| !done[w]));
            }
            continue;
        }
        // Nothing is free to move: every remaining pair's destination is
        // another remaining pair's source — a rename cycle (e.g. a swap
        // from counter renumbering). Park one file under a temp name to
        // vacate its source; it finishes when its own blocker completes.
        let Some(i) = (0..n).find(|&i| !done[i] && !parked[i]) else {
            for i in 0..n {
                if !done[i] {
                    errors.push(
                        tr!(
                            "{name}: rename cycle could not be resolved",
                            name = leaf_str(&pairs[i].0)
                        )
                        .to_string(),
                    );
                }
            }
            break;
        };
        tmp_seq += 1;
        let tmp = cur_src[i].with_file_name(format!(
            ".ferail-rename-{}-{tmp_seq}.tmp",
            std::process::id()
        ));
        match std::fs::rename(&cur_src[i], &tmp) {
            Ok(()) => {
                parked[i] = true;
                cur_src[i] = tmp;
                if let Some(ws) = waiters.remove(&i) {
                    ready.extend(ws.into_iter().filter(|&w| !done[w]));
                }
            }
            Err(e) => {
                errors.push(format!("{}: {e}", leaf_str(&cur_src[i])));
                done[i] = true;
                remaining -= 1;
                if let Some(ws) = waiters.remove(&i) {
                    ready.extend(ws.into_iter().filter(|&w| !done[w]));
                }
            }
        }
    }
    (renamed, errors)
}

/// `std::fs::rename` clobbers an existing destination on Unix — guard
/// against it, except when the "existing" destination *is* the source
/// (case-only rename on a case-insensitive filesystem). Same
/// don't-overwrite contract as `UndoOp::MoveBack`.
fn rename_guarded(old: &Path, new: &Path) -> Result<(), String> {
    if let Ok(new_meta) = new.symlink_metadata() {
        let same = old
            .symlink_metadata()
            .ok()
            .map(|old_meta| same_entry(&old_meta, &new_meta, old, new))
            .unwrap_or(false);
        if !same {
            return Err(tr!(
                "{old} \u{2192} {new}: an item with that name already exists",
                old = leaf_str(old),
                new = leaf_str(new)
            )
            .to_string());
        }
    }
    std::fs::rename(old, new).map_err(|e| format!("{}: {e}", leaf_str(old)))
}

/// Same directory entry? (dev, ino) on Unix; leaf-case comparison on
/// Windows (NTFS is case-insensitive by default and has no cheap inode
/// equivalent here).
fn same_entry(
    old_meta: &std::fs::Metadata,
    new_meta: &std::fs::Metadata,
    old: &Path,
    new: &Path,
) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = (old, new);
        old_meta.dev() == new_meta.dev() && old_meta.ino() == new_meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (old_meta, new_meta);
        old.parent() == new.parent()
            && old
                .file_name()
                .zip(new.file_name())
                .map(|(a, b)| {
                    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
                })
                .unwrap_or(false)
    }
}

fn leaf_str(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

// ---------------------------------------------------------------------
// Layer 2: modal UI + apply pipeline.
// ---------------------------------------------------------------------

/// Above this many items the plan recomputes on the background executor
/// (generation-tagged; stale results drop) instead of synchronously in
/// the input-change handler.
const BACKGROUND_PLAN_THRESHOLD: usize = 5_000;

/// Preview rows shown in the dialog before the "…and X more" line.
const PREVIEW_ROWS: usize = 12;

/// Dialog body entity. Holds the selection snapshot (captured once at
/// open — the dialog never re-reads the list), the rule inputs, and the
/// cached [`RenamePlan`] the render reads.
pub struct BulkRenameView {
    /// `(path, display name, mtime unix)` — resolved selection at open.
    items: Vec<(PathBuf, String, i64)>,
    /// The engine-facing projection of `items`, shared with background
    /// plan builds.
    names: Arc<Vec<(String, i64)>>,
    find_input: Entity<InputState>,
    replace_input: Entity<InputState>,
    template_input: Entity<InputState>,
    start_input: Entity<InputState>,
    pad_input: Entity<InputState>,
    use_regex: bool,
    case: CaseTransform,
    plan: RenamePlan,
    plan_generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl BulkRenameView {
    fn new(
        items: Vec<(PathBuf, String, i64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let names: Arc<Vec<(String, i64)>> = Arc::new(
            items
                .iter()
                .map(|(_, name, mtime)| (name.clone(), *mtime))
                .collect(),
        );
        let find_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Text or pattern")));
        let replace_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Replacement ($1\u{2026}$9)")));
        // `{name}` / `{n}` / `{ext}` here are the user's template tokens, not
        // `tr!` placeholders — there are no arguments, so nothing is filled
        // and the braces render as typed.
        let template_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("{name} {n}.{ext} \u{2014} leave empty to skip"))
        });
        let start_input = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        let pad_input = cx.new(|cx| InputState::new(window, cx).default_value("3"));
        let mut subscriptions = Vec::new();
        for input in [
            &find_input,
            &replace_input,
            &template_input,
            &start_input,
            &pad_input,
        ] {
            subscriptions.push(cx.subscribe(
                input,
                |this: &mut Self, _input, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.recompute(cx);
                    }
                },
            ));
        }
        let mut this = Self {
            items,
            names,
            find_input,
            replace_input,
            template_input,
            start_input,
            pad_input,
            use_regex: false,
            case: CaseTransform::None,
            plan: RenamePlan::default(),
            plan_generation: 0,
            _subscriptions: subscriptions,
        };
        this.recompute_now(cx);
        this
    }

    fn current_rule(&self, cx: &App) -> RenameRule {
        RenameRule {
            find: self.find_input.read(cx).value().to_string(),
            replace: self.replace_input.read(cx).value().to_string(),
            use_regex: self.use_regex,
            case: self.case,
            template: self.template_input.read(cx).value().to_string(),
            counter_start: self
                .start_input
                .read(cx)
                .value()
                .trim()
                .parse()
                .unwrap_or(1),
            counter_pad: self.pad_input.read(cx).value().trim().parse().unwrap_or(0),
        }
    }

    /// Recompute the cached plan from the current inputs — synchronously
    /// for typical selections, on the background executor past
    /// [`BACKGROUND_PLAN_THRESHOLD`] items.
    fn recompute(&mut self, cx: &mut Context<Self>) {
        if self.names.len() <= BACKGROUND_PLAN_THRESHOLD {
            self.recompute_now(cx);
            return;
        }
        let rule = self.current_rule(cx);
        self.plan_generation += 1;
        let generation = self.plan_generation;
        let names = self.names.clone();
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_executor()
                .spawn(async move { build_plan(&rule, &names) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer edit superseded this build — drop it.
                if this.plan_generation == generation {
                    this.plan = plan;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Synchronous recompute. Also used by `on_ok` so the commit always
    /// reflects the inputs as-typed, even if a background preview build
    /// is still in flight.
    fn recompute_now(&mut self, cx: &mut Context<Self>) {
        self.plan_generation += 1;
        let rule = self.current_rule(cx);
        self.plan = build_plan(&rule, &self.names);
        cx.notify();
    }

    /// `(old path, new path)` for every Renamed row, mapping the display
    /// name back to an on-disk leaf (macOS `/`→`:`), staying a single
    /// leaf under the item's existing parent.
    fn rename_pairs(&self) -> Vec<(PathBuf, PathBuf)> {
        self.items
            .iter()
            .zip(self.plan.rows.iter())
            .filter(|(_, row)| matches!(row.status, RowStatus::Renamed))
            .map(|((path, _, _), row)| {
                let disk = ferail_fs_native::paths::on_disk_leaf(&row.after).into_owned();
                (path.clone(), path.with_file_name(disk))
            })
            .collect()
    }
}

impl Render for BulkRenameView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let fg = cx.theme().foreground;
        let label = |text: SharedString| div().text_scale_xs().text_color(muted).child(text);

        let total = self.items.len();
        let renamed = self.plan.renamed;
        let conflicts = self.plan.conflicts;
        let overflow = self.plan.rows.len().saturating_sub(PREVIEW_ROWS);

        let mut preview = v_flex().gap_1();
        for row in self.plan.rows.iter().take(PREVIEW_ROWS) {
            let (after_color, note): (Hsla, Option<SharedString>) = match row.status {
                RowStatus::Unchanged => (muted, None),
                RowStatus::Renamed => (fg, None),
                RowStatus::Conflict(reason) => (danger, Some(crate::i18n::tr_static(reason))),
            };
            let before_color = if matches!(row.status, RowStatus::Unchanged) {
                muted
            } else {
                fg
            };
            let after_text: SharedString = if row.after.is_empty() {
                tr!("(empty)")
            } else {
                row.after.clone().into()
            };
            preview = preview.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_scale_xs()
                            .text_color(before_color)
                            .child(SharedString::from(row.before.clone())),
                    )
                    .child(div().text_scale_xs().text_color(muted).child("\u{2192}"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_scale_xs()
                            .text_color(after_color)
                            .child(after_text),
                    )
                    .when_some(note, |el, reason| {
                        el.child(div().text_scale_xxs().text_color(danger).child(reason))
                    }),
            );
        }
        if overflow > 0 {
            preview = preview.child(
                div()
                    .text_scale_xxs()
                    .text_color(muted)
                    .child(tr!("\u{2026}and {overflow} more", overflow = overflow)),
            );
        }

        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(label(tr!("Find")))
                    .child(div().flex_1().child(Input::new(&self.find_input).small()))
                    .child(label(tr!("Replace")))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.replace_input).small()),
                    )
                    .child(
                        Checkbox::new("bulk-rename-regex")
                            .small()
                            .label(tr!("Regex"))
                            .checked(self.use_regex)
                            .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                                this.use_regex = *checked;
                                this.recompute(cx);
                            })),
                    ),
            )
            .when_some(self.plan.error.clone(), |el, err| {
                el.child(div().text_scale_xs().text_color(danger).child(err))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(label(tr!("Case")))
                    .child(
                        ButtonGroup::new("bulk-rename-case")
                            .small()
                            .outline()
                            .compact()
                            .child(
                                Button::new("bulk-rename-case-none")
                                    .label(tr!("None"))
                                    .selected(self.case == CaseTransform::None),
                            )
                            .child(
                                Button::new("bulk-rename-case-lower")
                                    .label(tr!("lower"))
                                    .selected(self.case == CaseTransform::Lower),
                            )
                            .child(
                                Button::new("bulk-rename-case-upper")
                                    .label(tr!("UPPER"))
                                    .selected(self.case == CaseTransform::Upper),
                            )
                            .child(
                                Button::new("bulk-rename-case-title")
                                    .label(tr!("Title"))
                                    .selected(self.case == CaseTransform::Title),
                            )
                            .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                                let case = match clicks.first().copied() {
                                    Some(1) => CaseTransform::Lower,
                                    Some(2) => CaseTransform::Upper,
                                    Some(3) => CaseTransform::Title,
                                    _ => CaseTransform::None,
                                };
                                if this.case != case {
                                    this.case = case;
                                    this.recompute(cx);
                                }
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(label(tr!("Template")))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.template_input).small()),
                    )
                    .child(label(tr!("Start")))
                    .child(
                        div()
                            .w(px(64.))
                            .child(Input::new(&self.start_input).small()),
                    )
                    .child(label(tr!("Pad")))
                    .child(div().w(px(52.)).child(Input::new(&self.pad_input).small())),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .mt_1()
                    .child(div().text_scale_xs().text_color(muted).child(tr!(
                        "{renamed} of {total} will be renamed",
                        renamed = renamed,
                        total = total
                    )))
                    .when(conflicts > 0, |el| {
                        el.child(div().text_scale_xs().text_color(danger).child(trn!(
                            "\u{b7} {n} conflict",
                            "\u{b7} {n} conflicts",
                            conflicts
                        )))
                    }),
            )
            .child(preview)
    }
}

/// Open the bulk-rename dialog over `items` — the caller's resolved
/// selection snapshot (`(path, display name, mtime unix)`), captured
/// once. OK applies; conflicts or a bad pattern keep the dialog open.
///
/// The `&mut Shell` receiver keeps the call shape of the other file-op
/// helpers (`shell.open_text_prompt(…)` ↔ `bulk_rename::open(self, …)`);
/// the dialog itself reaches the shell through `cx.entity()`.
pub fn open(
    _shell: &mut Shell,
    items: Vec<(PathBuf, String, i64)>,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    if items.len() < 2 {
        return;
    }
    let count = items.len();
    let state = cx.new(|cx| BulkRenameView::new(items, window, cx));
    let shell_entity = cx.entity();
    let state_for_dialog = state.clone();
    let title: SharedString = trn!("Rename {n} Item", "Rename {n} Items", count);
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let state = state_for_dialog.clone();
        let shell = shell_entity.clone();
        dialog
            .title(title.clone())
            .w(px(680.))
            .child(state.clone())
            // The stock Dialog draws no buttons unless given a footer;
            // DialogClose/DialogAction dispatch CancelDialog /
            // ConfirmDialog, which route through on_cancel / on_ok below
            // (same path as the Escape / Enter keys).
            .footer(
                DialogFooter::new()
                    // DialogClose/DialogAction render size_full — bound
                    // them so the footer is a right-aligned button pair,
                    // not a half/half split.
                    .child(
                        div().w(px(96.)).child(
                            DialogClose::new().child(
                                Button::new("bulk-rename-cancel")
                                    .label(tr!("Cancel"))
                                    .small(),
                            ),
                        ),
                    )
                    .child(
                        div().w(px(96.)).child(
                            DialogAction::new().child(
                                Button::new("bulk-rename-ok")
                                    .label(tr!("Rename"))
                                    .primary()
                                    .small(),
                            ),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx: &mut App| {
                let pairs = state.update(cx, |this, cx| {
                    // Commit from the inputs as-typed (a large batch's
                    // background preview may still be in flight).
                    this.recompute_now(cx);
                    (this.plan.error.is_none() && this.plan.conflicts == 0)
                        .then(|| this.rename_pairs())
                });
                let Some(pairs) = pairs else {
                    // Conflicts / bad pattern: keep the dialog open so
                    // the user can fix the rule.
                    return false;
                };
                if !pairs.is_empty() {
                    shell.update(cx, |this, cx| apply(this, pairs, window, cx));
                }
                true
            })
    });
    // Focus the Find field once the dialog has mounted — same next-frame
    // trick as `Shell::open_text_prompt`.
    window.on_next_frame(move |window, cx| {
        let input = state.read(cx).find_input.clone();
        input.read(cx).focus_handle(cx).focus(window, cx);
    });
}

/// Run the batch on the background executor: rename each pair, continue
/// past failures, record only the successful pairs as one
/// [`UndoOp::RenameBatch`], reload affected directories, toast the
/// outcome. Mirrors `Shell::spawn_file_op`'s shape, but per-item
/// resilient instead of all-or-nothing.
fn apply(
    shell: &mut Shell,
    pairs: Vec<(PathBuf, PathBuf)>,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    let count = pairs.len();
    let process = shell.process.clone();
    let win = window.window_handle();
    let task_id = process.tasks.borrow_mut().begin(
        crate::tasks::TaskKind::FileOp,
        trn!(
            "Renaming {n} item\u{2026}",
            "Renaming {n} items\u{2026}",
            count
        )
        .to_string(),
        false,
    );
    cx.spawn(async move |weak, cx| {
        let (renamed, errors) = cx
            .background_executor()
            .spawn(async move { run_renames(pairs) })
            .await;
        let first_error = errors.first().cloned();
        let failed = errors.len();
        cx.update(|_| {
            let mut tasks = process.tasks.borrow_mut();
            match &first_error {
                None => tasks.end(task_id),
                Some(e) => tasks.end_failed(task_id, e.clone()),
            }
        });
        if !renamed.is_empty() {
            let mut reload: Vec<PathBuf> = Vec::new();
            for (old, new) in &renamed {
                for parent in [old.parent(), new.parent()].into_iter().flatten() {
                    if !reload.iter().any(|p| p == parent) {
                        reload.push(parent.to_path_buf());
                    }
                }
            }
            let pairs_for_undo = renamed.clone();
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    this.push_undo(UndoOp::RenameBatch(pairs_for_undo));
                    cx.notify();
                });
            }
            Shell::broadcast_reload_for_process(&process, reload, cx);
        }
        let renamed_count = renamed.len();
        let _ = win.update(cx, |_, window, cx| {
            use gpui_component::notification::Notification;
            if failed == 0 {
                window.push_notification(
                    Notification::success(trn!(
                        "Renamed {n} item",
                        "Renamed {n} items",
                        renamed_count
                    )),
                    cx,
                );
            } else {
                window.push_notification(
                    crate::shell::error_notification(
                        tr!(
                            "Renamed {renamed} items, {failed} failed \u{2014} {detail}",
                            renamed = renamed_count,
                            failed = failed,
                            detail = first_error.unwrap_or_default()
                        )
                        .to_string(),
                    ),
                    cx,
                );
            }
        });
    })
    .detach();
}

// ---------------------------------------------------------------------
// Tests (engine layer + rename worker).
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<(String, i64)> {
        names.iter().map(|n| (n.to_string(), 0i64)).collect()
    }

    fn afters(plan: &RenamePlan) -> Vec<&str> {
        plan.rows.iter().map(|r| r.after.as_str()).collect()
    }

    #[test]
    fn literal_replace() {
        let rule = RenameRule {
            find: "IMG".into(),
            replace: "Photo".into(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["IMG_001.jpg", "notes.txt"]));
        assert_eq!(afters(&plan), ["Photo_001.jpg", "notes.txt"]);
        assert_eq!(plan.rows[0].status, RowStatus::Renamed);
        assert_eq!(plan.rows[1].status, RowStatus::Unchanged);
        assert_eq!(plan.renamed, 1);
        assert_eq!(plan.conflicts, 0);
        assert!(plan.error.is_none());
    }

    #[test]
    fn regex_with_captures() {
        let rule = RenameRule {
            find: r"^(\d+)-(\w+)".into(),
            replace: "$2-$1".into(),
            use_regex: true,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["01-intro.txt"]));
        assert_eq!(afters(&plan), ["intro-01.txt"]);
        assert_eq!(plan.renamed, 1);
    }

    #[test]
    fn invalid_regex_is_one_plan_error() {
        let rule = RenameRule {
            find: "(".into(),
            use_regex: true,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["a.txt", "b.txt"]));
        assert!(plan.error.as_deref().unwrap().starts_with("Invalid regex"));
        assert_eq!(plan.renamed, 0);
        assert_eq!(plan.conflicts, 0);
        assert!(
            plan.rows
                .iter()
                .all(|r| r.status == RowStatus::Unchanged && r.after == r.before)
        );
    }

    #[test]
    fn bad_literal_pattern_is_fine_as_regex_off() {
        // The same "(" that breaks regex mode is a plain character in
        // literal mode.
        let rule = RenameRule {
            find: "(".into(),
            replace: "[".into(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["take (1).txt"]));
        assert_eq!(afters(&plan), ["take [1).txt"]);
        assert!(plan.error.is_none());
    }

    #[test]
    fn case_transforms_touch_stem_only() {
        let upper = RenameRule {
            case: CaseTransform::Upper,
            ..Default::default()
        };
        let plan = build_plan(&upper, &items(&["photo.jpg"]));
        assert_eq!(afters(&plan), ["PHOTO.jpg"]);

        let lower = RenameRule {
            case: CaseTransform::Lower,
            ..Default::default()
        };
        let plan = build_plan(&lower, &items(&["READ ME.TXT"]));
        assert_eq!(afters(&plan), ["read me.TXT"]);
    }

    #[test]
    fn leading_dot_names_are_all_stem() {
        let rule = RenameRule {
            case: CaseTransform::Upper,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&[".gitignore"]));
        // No "extension" to protect — the whole name transforms.
        assert_eq!(afters(&plan), [".GITIGNORE"]);
    }

    #[test]
    fn title_case_is_unicode_aware() {
        let rule = RenameRule {
            case: CaseTransform::Title,
            ..Default::default()
        };
        let plan = build_plan(
            &rule,
            &items(&["annual report DRAFT.txt", "\u{e9}clair au caf\u{e9}.txt"]),
        );
        assert_eq!(
            afters(&plan),
            ["Annual Report Draft.txt", "\u{c9}clair Au Caf\u{e9}.txt"]
        );
    }

    #[test]
    fn upper_case_handles_multi_char_expansions() {
        // ß uppercases to SS — char::to_uppercase yields multiple chars.
        let rule = RenameRule {
            case: CaseTransform::Upper,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["stra\u{df}e.txt"]));
        assert_eq!(afters(&plan), ["STRASSE.txt"]);
    }

    #[test]
    fn template_counter_pads_and_increments() {
        let rule = RenameRule {
            template: "{name} {n}.{ext}".into(),
            counter_start: 7,
            counter_pad: 3,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["a.txt", "b.txt"]));
        assert_eq!(afters(&plan), ["a 007.txt", "b 008.txt"]);
        assert_eq!(plan.renamed, 2);
    }

    #[test]
    fn template_date_token() {
        // 1615680000 = 18700 days = 2021-03-14 00:00 UTC.
        let rule = RenameRule {
            template: "{date} {name}".into(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &[("a.txt".to_string(), 1_615_680_000)]);
        assert_eq!(afters(&plan), ["2021-03-14 a.txt"]);
    }

    #[test]
    fn template_auto_appends_extension() {
        let rule = RenameRule {
            template: "{name}-final".into(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["doc.md", "Makefile"]));
        // With an extension it's re-appended; without one nothing is.
        assert_eq!(afters(&plan), ["doc-final.md", "Makefile-final"]);
    }

    #[test]
    fn template_with_ext_token_is_used_as_is() {
        let rule = RenameRule {
            template: "{name}.{ext}".into(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["doc.md"]));
        assert_eq!(afters(&plan), ["doc.md"]);
        // Identity template ⇒ unchanged, not renamed.
        assert_eq!(plan.rows[0].status, RowStatus::Unchanged);
        assert_eq!(plan.renamed, 0);
    }

    #[test]
    fn duplicate_targets_conflict_case_insensitive() {
        // Both rows collapse onto "SAME.txt"/"same.txt" — a duplicate
        // pair on the case-insensitive filesystems we target.
        let rule = RenameRule {
            find: "1".into(),
            replace: String::new(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["SAME1.txt", "same1.txt"]));
        assert_eq!(plan.rows[0].status, RowStatus::Conflict("duplicate target"));
        assert_eq!(plan.rows[1].status, RowStatus::Conflict("duplicate target"));
        assert_eq!(plan.conflicts, 2);
        assert_eq!(plan.renamed, 0);
    }

    #[test]
    fn collision_with_unchanged_member_conflicts() {
        // "a.txt" → "B.txt" while "b.txt" sits in the batch unchanged —
        // its name is still taken (case-insensitively).
        let rule = RenameRule {
            find: "a".into(),
            replace: "B".into(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["a.txt", "b.txt"]));
        assert_eq!(
            plan.rows[0].status,
            RowStatus::Conflict("name already taken")
        );
        assert_eq!(plan.rows[1].status, RowStatus::Unchanged);
        assert_eq!(plan.conflicts, 1);
    }

    #[test]
    fn renumber_chain_is_not_a_conflict() {
        // 1.jpg → 2.jpg while 2.jpg → 3.jpg: the target is another
        // *changed* row's old name — a chain the apply step orders, not
        // a conflict.
        let rule = RenameRule {
            template: "{n}".into(),
            counter_start: 2,
            counter_pad: 0,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["1.jpg", "2.jpg"]));
        assert_eq!(afters(&plan), ["2.jpg", "3.jpg"]);
        assert_eq!(plan.renamed, 2);
        assert_eq!(plan.conflicts, 0);
    }

    #[test]
    fn empty_result_conflicts() {
        let rule = RenameRule {
            find: "a.txt".into(),
            replace: String::new(),
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["a.txt"]));
        assert_eq!(plan.rows[0].status, RowStatus::Conflict("empty name"));
        assert_eq!(plan.conflicts, 1);
    }

    #[test]
    fn empty_rule_leaves_everything_unchanged() {
        let plan = build_plan(&RenameRule::default(), &items(&["a.txt", "b.txt"]));
        assert_eq!(plan.renamed, 0);
        assert_eq!(plan.conflicts, 0);
        assert!(plan.rows.iter().all(|r| r.status == RowStatus::Unchanged));
    }

    #[test]
    fn case_only_self_rename_counts_as_renamed() {
        let rule = RenameRule {
            case: CaseTransform::Upper,
            ..Default::default()
        };
        let plan = build_plan(&rule, &items(&["readme.md"]));
        assert_eq!(afters(&plan), ["README.md"]);
        assert_eq!(plan.rows[0].status, RowStatus::Renamed);
    }

    // --- run_renames: dependency ordering on a real (temp) directory --

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferail-bulk-rename-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn run_renames_orders_chains() {
        let dir = temp_dir("chain");
        touch(&dir, "1.txt", "one");
        touch(&dir, "2.txt", "two");
        // Batch order deliberately wrong: 1→2 listed before 2→3.
        let pairs = vec![
            (dir.join("1.txt"), dir.join("2.txt")),
            (dir.join("2.txt"), dir.join("3.txt")),
        ];
        let (renamed, errors) = run_renames(pairs);
        assert_eq!(errors, Vec::<String>::new());
        assert_eq!(renamed.len(), 2);
        assert_eq!(std::fs::read_to_string(dir.join("2.txt")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(dir.join("3.txt")).unwrap(), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_renames_breaks_swap_cycles() {
        let dir = temp_dir("swap");
        touch(&dir, "a.txt", "was-a");
        touch(&dir, "b.txt", "was-b");
        let pairs = vec![
            (dir.join("a.txt"), dir.join("b.txt")),
            (dir.join("b.txt"), dir.join("a.txt")),
        ];
        let (renamed, errors) = run_renames(pairs);
        assert_eq!(errors, Vec::<String>::new());
        assert_eq!(renamed.len(), 2);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "was-b");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "was-a");
        // No parked temp file left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_renames_guards_existing_targets_and_continues() {
        let dir = temp_dir("guard");
        touch(&dir, "a.txt", "a");
        touch(&dir, "taken.txt", "occupied"); // NOT part of the batch
        touch(&dir, "c.txt", "c");
        let pairs = vec![
            (dir.join("a.txt"), dir.join("taken.txt")),
            (dir.join("c.txt"), dir.join("d.txt")),
        ];
        let (renamed, errors) = run_renames(pairs);
        // a→taken fails (guarded), c→d still applies.
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        assert_eq!(renamed, vec![(dir.join("c.txt"), dir.join("d.txt"))]);
        assert_eq!(
            std::fs::read_to_string(dir.join("taken.txt")).unwrap(),
            "occupied"
        );
        assert!(dir.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
