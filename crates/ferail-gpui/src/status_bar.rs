//! Bottom-of-window status bar — task count + progress indicator.
//!
//! Reads from `Shell::tasks` (a shared
//! `Rc<RefCell<TaskRegistry>>`), so its text + progress always reflect
//! the live set of background jobs.
//!
//! Layout (left → right):
//! - "<N> item(s)" entry count for the active tab's listing.
//! - "Doing X…" when exactly one task is in flight (uses the task's
//!   label). When >1 task is in flight: "N tasks running".
//! - A thin progress strip on the right: indeterminate stripe when at
//!   least one task is `Indeterminate`, otherwise determinate fill at
//!   the latest task's fraction.
//!
//! Clicking the count region toggles the (future) task panel popover;
//! today it's a no-op placeholder — the popover lands in Stage 5.c
//! alongside the toast surface.

use crate::text::TextScale as _;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, h_flex};

use crate::tasks::{TaskProgress, TaskRegistry};

/// Click-event callback the owning Shell hands to status-bar regions
/// (task area, progress strip).
pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Window-level action callback with no event payload (e.g. the
/// Show-Hidden switch, which carries its own state).
pub type ActionHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// Status-bar-local byte-size formatter. Mirrors the one in
/// disk_usage.rs (1 KB = 1024 B; 1 decimal place above KB). `pub(crate)`
/// so the system-stats segment's MEM figure uses the same convention.
pub(crate) fn humanize_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut s = b as f64;
    let mut u = 0;
    while s >= 1024.0 && u + 1 < UNITS.len() {
        s /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", b, UNITS[u])
    } else {
        format!("{:.1} {}", s, UNITS[u])
    }
}

/// Render the status bar row. `entries` is the active tab's entry
/// count; `tasks` is the shared task registry. `simulated_progress`
/// is `Some(_)` only when the `--simulate-progress` CLI flag is set
/// (used to visualise the strip in screenshots without spinning up
/// real work). `on_toggle_task_panel` fires when the user clicks the
/// task region or progress strip — `None` when the host doesn't want
/// the panel (screenshots, etc.).
/// Density-of-decisions metrics surfaced by the status bar
/// (Phase 8). Each field is precomputed by the Shell so the render
/// path doesn't recompute on every paint.
#[derive(Default, Clone)]
pub struct StatusMetrics {
    pub entries: usize,
    pub selected_count: usize,
    pub selected_size: u64,
    pub total_size: u64,
    pub free_bytes: Option<u64>,
    pub volume_name: Option<SharedString>,
    /// The tab's volume is mounted read-only (CD/DVD, locked card,
    /// read-only image, `ro` mount). Replaces the free-space label —
    /// "0 B free" on a CD is true but buries the actual story.
    pub volume_read_only: bool,
    /// Hidden entries the current listing skipped (show-hidden off) —
    /// count and summed sizes. Zero when the toggle is on or the folder
    /// has none, which also hides the chip. Passive discoverability:
    /// the user learns hidden content exists (and how much) without
    /// unhiding it.
    pub hidden_count: usize,
    pub hidden_bytes: u64,
    /// Entries the filter field excluded from the current listing —
    /// count and summed sizes. Zero when the field is empty. Keeps the
    /// count and total honest about the whole folder: without it, "12
    /// items · 3.2 MB" while a filter is typed reads as the folder's
    /// full contents.
    pub filtered_count: usize,
    pub filtered_bytes: u64,
    /// App-footprint figures (up · CPU · MEM · rps), pre-formatted by
    /// the off-thread sampler's snapshot (or `--simulate-stats`).
    /// `None` until the sampler's first real reading.
    pub stats: Option<crate::system_stats::SegmentParts>,
}

/// How much room the status bar has to say what it has to say.
///
/// The bar carries a lot of small readouts, and translation swells them
/// unevenly — English "up 3m" is French "en service depuis 3m". Rather
/// than let the tail run off a narrow window, every wordy segment has
/// three wordings, and [`plan`] picks the widest one that fits.
///
/// - [`Density::Full`] — the sentence: "126.3 GB free on Macintosh HD".
/// - [`Density::Short`] — the same fact without the parts the context
///   already supplies: "126.3 GB free".
/// - [`Density::Minimal`] — the figure plus, at most, a universal token:
///   "126.3 GB", "UP 3m". Deliberately untranslated where that token is
///   an acronym — the bar already ships `CPU` / `MEM` / `rps`
///   unlocalized, and a two-letter code beats a truncated word.
///
/// Narrower still and [`plan`] starts dropping segments outright, in
/// least-essential-first order. Two things never drop: the entry count,
/// and the Show-Hidden switch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Density {
    Full,
    Short,
    Minimal,
}

/// The four app-footprint figures at one density.
#[derive(Clone)]
pub(crate) struct StatsTexts {
    pub up: SharedString,
    pub cpu: SharedString,
    pub mem: SharedString,
    pub rps: SharedString,
}

/// Every wordy segment's text at one density. Built by [`segments`] and
/// measured by [`plan`], so the fit decision reads the strings that will
/// actually be painted rather than an estimate of a different wording.
#[derive(Clone)]
pub(crate) struct Segments {
    pub count: String,
    pub filtered: Option<String>,
    pub free: Option<String>,
    pub hidden: Option<String>,
    pub stats: Option<StatsTexts>,
    /// `None` at [`Density::Minimal`] — the switch keeps its meaning
    /// through a tooltip there rather than a word.
    pub switch_label: Option<SharedString>,
}

/// What [`plan`] decided: which wording to use, how big to set the text,
/// and which segments had to go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Plan {
    pub density: Density,
    /// Drop one type tier (Xs → Xxs). Cheaper than losing a readout, so
    /// it happens once the minimal wording still doesn't fit and before
    /// anything is hidden.
    pub small_text: bool,
    pub show_stats: bool,
    pub show_hidden_chip: bool,
    pub show_filtered: bool,
    pub show_free: bool,
    pub show_task: bool,
}

impl Plan {
    /// The roomiest plan — what a wide window gets, and the fallback for
    /// callers that have no width to work from.
    pub(crate) fn full() -> Self {
        Self {
            density: Density::Full,
            small_text: false,
            show_stats: true,
            show_hidden_chip: true,
            show_filtered: true,
            show_free: true,
            show_task: true,
        }
    }

    /// Gap between segments, in logical px at `ui_scale == 1`. Tightening
    /// it is free — nobody reads the whitespace — so a degraded bar buys
    /// room back here before it starts cutting words.
    pub(crate) fn gap_px(&self) -> f32 {
        match self.density {
            Density::Full => 16.0,
            Density::Short => 10.0,
            Density::Minimal => 6.0,
        }
    }

    pub(crate) fn text_size(&self) -> crate::text::TextSize {
        if self.small_text {
            crate::text::TextSize::Xxs
        } else {
            crate::text::TextSize::Xs
        }
    }

    /// Progress-strip width. The strip is a shape, not a sentence, so it
    /// can shrink well past the point where text would stop being
    /// readable.
    pub(crate) fn progress_w(&self) -> f32 {
        match self.density {
            Density::Full => 120.0,
            Density::Short => 90.0,
            Density::Minimal => 56.0,
        }
    }

    /// Per-figure minimum widths for the stats cells, in rems. Each
    /// figure sits in a right-aligned box wide enough for its realistic
    /// worst case, so a live value changing width ("9.8%" → "10%") never
    /// shifts its neighbours and the bar doesn't jitter on every tick.
    /// The minimal wording is shorter, so its boxes are too.
    pub(crate) fn stats_min_rems(&self) -> [f32; 4] {
        match self.density {
            Density::Full | Density::Short => [3.9, 3.4, 5.0, 2.6],
            Density::Minimal => [3.4, 3.0, 4.4, 2.4],
        }
    }
}

/// The left-hand count/size text, plus the "N filtered out · X"
/// companion when the filter field is holding entries back.
///
/// The count and total describe *what is on screen*; the companion
/// carries the rest of the folder, so a filtered view never passes
/// itself off as the whole thing. When the needle matches nothing, the
/// count itself becomes the explanation — "Empty folder" would send the
/// user hunting for files that are merely filtered.
///
/// At [`Density::Minimal`] the words go and the figures stay: "12 · 3.0
/// MB", "3/12 · 1.0 KB". Nothing is lost there that the list itself
/// doesn't already say.
///
/// Pure and split out of `render` so the wording is unit-testable.
pub(crate) fn count_labels(metrics: &StatusMetrics, density: Density) -> (String, Option<String>) {
    let entries = metrics.entries;
    let filtered = metrics.filtered_count;
    // Compositions of numbers and separators with no words in them are
    // built with `format!`, not `tr!`: there is nothing to translate, and
    // a msgid whose whole body is "{n} · {size}" only invites
    // translators to disagree about the separator.
    if entries == 0 {
        let label = if filtered == 0 {
            match density {
                Density::Minimal => tr!("Empty").to_string(),
                _ => tr!("Empty folder").to_string(),
            }
        } else {
            let size = humanize_bytes(metrics.filtered_bytes);
            match density {
                Density::Full => trn!(
                    "{n} item filtered out \u{00B7} {size}",
                    "All {n} items filtered out \u{00B7} {size}",
                    filtered,
                    size = size
                )
                .to_string(),
                Density::Short => trn!(
                    "{n} filtered out \u{00B7} {size}",
                    "{n} filtered out \u{00B7} {size}",
                    filtered,
                    size = size
                )
                .to_string(),
                Density::Minimal => format!("0/{filtered} \u{00B7} {size}"),
            }
        };
        return (group_large_numbers(&label), None);
    }
    let count_label = if entries != 1 && metrics.selected_count > 0 {
        let size = humanize_bytes(metrics.selected_size);
        match density {
            Density::Full | Density::Short => trn!(
                "{n} of {total} selected \u{00B7} {size}",
                "{n} of {total} selected \u{00B7} {size}",
                metrics.selected_count,
                total = entries,
                size = size
            )
            .to_string(),
            Density::Minimal => {
                format!("{}/{} \u{00B7} {}", metrics.selected_count, entries, size)
            }
        }
    } else {
        let size = humanize_bytes(metrics.total_size);
        match density {
            Density::Full | Density::Short => trn!(
                "{n} item \u{00B7} {size}",
                "{n} items \u{00B7} {size}",
                entries,
                size = size
            )
            .to_string(),
            Density::Minimal => format!("{entries} \u{00B7} {size}"),
        }
    };
    let filtered_label = (filtered > 0).then(|| {
        let size = humanize_bytes(metrics.filtered_bytes);
        match density {
            Density::Full => trn!(
                "{n} filtered out \u{00B7} {size}",
                "{n} filtered out \u{00B7} {size}",
                filtered,
                size = size
            )
            .to_string(),
            // The size is the first thing to go: the count alone still
            // says "the folder holds more than you can see".
            Density::Short | Density::Minimal => {
                trn!("{n} filtered", "{n} filtered", filtered).to_string()
            }
        }
    });
    (
        group_large_numbers(&count_label),
        filtered_label.map(|label| group_large_numbers(&label)),
    )
}

/// Group long decimal runs using the compact separator requested by the UI
/// (`4.138.016`). Applied after translation so plural selection and word
/// order remain entirely locale-driven.
fn group_large_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 3);
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
        while cursor < text.len() && text.as_bytes()[cursor].is_ascii_digit() {
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

/// Free-space (or read-only) wording for a density.
pub(crate) fn free_label(metrics: &StatusMetrics, density: Density) -> Option<SharedString> {
    if metrics.volume_read_only {
        return Some(match (&metrics.volume_name, density) {
            (Some(name), Density::Full) => tr!("{name} is read-only", name = name.clone()),
            (_, Density::Minimal) => tr!("Read-only"),
            _ => tr!("Read-only volume"),
        });
    }
    let bytes = metrics.free_bytes?;
    let size = humanize_bytes(bytes);
    Some(match (&metrics.volume_name, density) {
        // The volume name is the first thing to go: on a single-disk Mac
        // it repeats what the sidebar already says.
        (Some(name), Density::Full) => {
            tr!("{size} free on {name}", size = size, name = name.clone())
        }
        (_, Density::Minimal) => SharedString::from(size),
        _ => tr!("{size} free", size = size),
    })
}

/// Hidden-content chip wording for a density.
fn hidden_label(metrics: &StatusMetrics, density: Density) -> Option<String> {
    if metrics.hidden_count == 0 {
        return None;
    }
    Some(match density {
        Density::Full => trn!(
            "{n} hidden \u{00B7} {size}",
            "{n} hidden \u{00B7} {size}",
            metrics.hidden_count,
            size = humanize_bytes(metrics.hidden_bytes)
        )
        .to_string(),
        _ => trn!("{n} hidden", "{n} hidden", metrics.hidden_count).to_string(),
    })
}

/// App-footprint figures for a density. `CPU` / `MEM` / `rps` are already
/// the shortest honest form in every language we ship, so only the
/// uptime changes: the translated "up" wording gives way to the
/// universal `UP` token.
fn stats_texts(parts: &crate::system_stats::SegmentParts, density: Density) -> StatsTexts {
    StatsTexts {
        up: match density {
            Density::Minimal => SharedString::from(format!("UP {}", parts.uptime)),
            _ => parts.up.clone(),
        },
        cpu: parts.cpu.clone(),
        mem: parts.mem.clone(),
        rps: parts.rps.clone(),
    }
}

/// Every segment's text at one density.
pub(crate) fn segments(metrics: &StatusMetrics, density: Density) -> Segments {
    let (count, filtered) = count_labels(metrics, density);
    Segments {
        count,
        filtered,
        free: free_label(metrics, density).map(|s| s.to_string()),
        hidden: hidden_label(metrics, density),
        stats: metrics.stats.as_ref().map(|p| stats_texts(p, density)),
        switch_label: match density {
            Density::Full => Some(tr!("Show hidden")),
            Density::Short => Some(tr!("Hidden")),
            Density::Minimal => None,
        },
    }
}

/// Rough advance width of one character, as a fraction of the font size.
/// The bar is proportional text, so this is an average over mixed
/// digits, lowercase and separators — deliberately on the generous side,
/// because under-estimating pushes the tail off the window while
/// over-estimating only degrades a step early.
const AVG_CHAR_W: f32 = 0.55;
/// Show-Hidden switch, at `xsmall`.
const SWITCH_W: f32 = 30.0;
/// `px_3` on both sides, plus a margin so the last element isn't flush
/// against the window edge.
const EDGE_PAD: f32 = 32.0;
/// The task label truncates rather than wraps, so it only needs enough
/// room to stay recognizable.
const TASK_MIN_W: f32 = 90.0;

fn text_w(s: &str, font_px: f32) -> f32 {
    s.chars().count() as f32 * font_px * AVG_CHAR_W
}

/// Estimated width of the bar as `plan` would paint it, in logical px at
/// `ui_scale == 1`.
fn estimated_width(
    seg: &Segments,
    plan: &Plan,
    has_task: bool,
    has_progress: bool,
    rem_px: f32,
) -> f32 {
    let font = ferail_design::TextTokens::BASE.get(plan.text_size());
    let mut boxes: Vec<f32> = vec![text_w(&seg.count, font)];
    if plan.show_filtered
        && let Some(t) = &seg.filtered
    {
        boxes.push(text_w(t, font));
    }
    if plan.show_task && has_task {
        boxes.push(TASK_MIN_W);
    }
    if has_progress {
        boxes.push(plan.progress_w());
    }
    if plan.show_free
        && let Some(t) = &seg.free
    {
        boxes.push(text_w(t, font));
    }
    if plan.show_stats
        && let Some(st) = &seg.stats
    {
        let mins = plan.stats_min_rems();
        let cells = [&st.up, &st.cpu, &st.mem, &st.rps];
        // Each figure occupies the larger of its text and its
        // anti-jitter floor; the three "·" separators and their gaps
        // ride along at roughly a character each.
        let w: f32 = cells
            .iter()
            .zip(mins)
            .map(|(t, min_rems)| text_w(t, font).max(min_rems * rem_px))
            .sum();
        boxes.push(w + 3.0 * (font + plan.gap_px() * 0.5));
    }
    if plan.show_hidden_chip
        && let Some(t) = &seg.hidden
    {
        boxes.push(text_w(t, font));
    }
    if let Some(t) = &seg.switch_label {
        boxes.push(text_w(t, font));
    }
    boxes.push(SWITCH_W);
    let gaps = plan.gap_px() * boxes.len().saturating_sub(1) as f32;
    boxes.iter().sum::<f32>() + gaps + EDGE_PAD
}

/// Pick the least-degraded plan that fits `avail_px` (the window's
/// logical width) and return it with the segment texts it chose.
///
/// The ladder, in order: shorten the wording, then minimalize it, then
/// give up one type tier, then start hiding — stats first (developer
/// telemetry), then the hidden chip, the filtered chip, free space, and
/// last of all the task label. The entry count and the Show-Hidden
/// switch are not on the ladder at all; they survive every width.
///
/// Pure, so the ladder is unit-testable without a window.
pub(crate) fn plan(
    metrics: &StatusMetrics,
    avail_px: f32,
    rem_px: f32,
    has_task: bool,
    has_progress: bool,
) -> (Plan, Segments) {
    let mut chosen = Plan::full();
    let mut seg = segments(metrics, Density::Full);
    // A zero/unknown width (a host that never sized a window) means
    // "don't degrade", not "degrade everything".
    if avail_px <= 0.0 {
        return (chosen, seg);
    }
    let fits = |seg: &Segments, plan: &Plan| {
        estimated_width(seg, plan, has_task, has_progress, rem_px) <= avail_px
    };
    if fits(&seg, &chosen) {
        return (chosen, seg);
    }
    for density in [Density::Short, Density::Minimal] {
        chosen.density = density;
        seg = segments(metrics, density);
        if fits(&seg, &chosen) {
            return (chosen, seg);
        }
    }
    // The minimal wording still overflows: give up one type tier, then
    // start taking segments away, least essential first.
    chosen.small_text = true;
    for drop in 0..5 {
        match drop {
            0 => {}
            1 => chosen.show_stats = false,
            2 => chosen.show_hidden_chip = false,
            3 => chosen.show_filtered = false,
            4 => chosen.show_free = false,
            _ => unreachable!(),
        }
        if fits(&seg, &chosen) {
            return (chosen, seg);
        }
    }
    // Last resort: the task label goes too. The count and the switch stay
    // whatever happens — past here the window is narrower than they are,
    // and truncating them is the honest answer, not another tier.
    chosen.show_task = false;
    (chosen, seg)
}

// One more argument than clippy's default, in line with the other
// render helpers in this crate (`keyboard_help`, `favorites_section`):
// the alternative is a parameter struct that exists only to satisfy the
// count.
#[allow(clippy::too_many_arguments)]
pub fn render(
    metrics: StatusMetrics,
    tasks: &Rc<RefCell<TaskRegistry>>,
    simulated_progress: Option<f32>,
    on_toggle_task_panel: Option<ClickHandler>,
    show_hidden: bool,
    on_toggle_hidden: Option<ActionHandler>,
    window: &Window,
    cx: &mut App,
) -> Div {
    // Snapshot theme colours up-front — the later progress_strip
    // call takes `&mut App`, which would otherwise conflict with the
    // outstanding `theme` borrow inside `when_some(free_label, ...)`.
    let theme_border = cx.theme().border;
    let theme_secondary = cx.theme().secondary;
    let theme_muted_fg = cx.theme().muted_foreground;
    let registry = tasks.borrow();

    // Middle: task summary. Only surfaced tasks count — sub-perceptual
    // work (instant clones) begins and ends inside SURFACE_DELAY and
    // never flickers a label into view.
    let surfaced = registry.iter().filter(|t| t.is_surfaced()).count();
    let task_label = if let Some(_p) = simulated_progress {
        Some(tr!("Simulating progress\u{2026}"))
    } else if surfaced == 0 {
        None
    } else if let Some(t) = registry.primary().filter(|t| t.is_surfaced()) {
        // The primary (foreground-preferring) task owns the line. With
        // exactly one surfaced task, or whenever the primary is a
        // foreground op, show its label + live rate/ETA. Otherwise just
        // count the ambient background work.
        if surfaced == 1 || t.kind.is_foreground() {
            Some(SharedString::from(label_with_rate(t)))
        } else {
            Some(trn!("{n} task running", "{n} tasks running", surfaced))
        }
    } else {
        Some(trn!("{n} task running", "{n} tasks running", surfaced))
    };

    // Right side: progress strip. Determinate fraction = the
    // primary task's fraction (or the simulated value). Anything
    // indeterminate flips the strip into the indeterminate mode.
    let (visible, indeterminate, fraction) = compute_progress(&registry, simulated_progress);

    // How much of all that actually fits. The bar is a single row of
    // flex_shrink_0 children, so nothing here can be left to flexbox:
    // an overflowing row does not compress, it pushes its own tail
    // (the Show-Hidden switch) off the window edge. `plan` picks the
    // wording, the type tier and the segment set from the window's
    // width before any of it is built. Cheap: a handful of `format!`s
    // and character counts, no measurement and no I/O.
    let rem_px = window.rem_size().as_f32();
    let ui_scale = rem_px / crate::text::BASE_REM_PX;
    // The plan's widths are logical px at `ui_scale == 1`, so compare
    // against the viewport in those same units — UI zoom shrinks the
    // room the bar has just as surely as a narrower window does.
    let avail = window.viewport_size().width.as_f32() / ui_scale.max(0.01);
    let (plan, seg) = plan(
        &metrics,
        avail,
        crate::text::BASE_REM_PX,
        task_label.is_some(),
        visible,
    );

    let on_toggle = on_toggle_task_panel;
    // Returns AnyElement so the two branches (id'd Stateful<Div> vs.
    // plain Div) unify.
    let make_clickable = |d: Div, region_id: &'static str| -> AnyElement {
        if let Some(cb) = on_toggle.clone() {
            d.id(region_id)
                .cursor_pointer()
                .on_click(move |evt, window, cx| cb(evt, window, cx))
                .into_any_element()
        } else {
            d.into_any_element()
        }
    };

    let gap = plan.gap_px();
    h_flex()
        .w_full()
        .flex_shrink_0()
        .items_center()
        .gap(rems(gap / crate::text::BASE_REM_PX))
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(theme_border)
        .bg(theme_secondary)
        .text_token(plan.text_size())
        .text_color(theme_muted_fg)
        // Never dropped, never abbreviated past its figures: what is in
        // this folder, and how big it is.
        .child(div().flex_shrink_0().child(seg.count))
        // What the filter field is holding back, sitting next to the
        // count it qualifies — same muted treatment as the hidden chip.
        .when_some(
            seg.filtered.filter(|_| plan.show_filtered),
            |this, label| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme_muted_fg.opacity(0.85))
                        .child(label),
                )
            },
        )
        .when_some(task_label.filter(|_| plan.show_task), |this, label| {
            this.child(make_clickable(
                div().flex_1().min_w_0().truncate().child(label),
                "status-bar-task-label",
            ))
        })
        // The elastic gap that pushes the right-hand cluster to the edge.
        // Without a task label there is no other flex_1 in the row.
        .when(
            task_label_none(&registry, simulated_progress) || !plan.show_task,
            |this| this.child(div().flex_1()),
        )
        .when(visible, |this| {
            this.child(make_clickable(
                progress_strip(plan.progress_w(), indeterminate, fraction, cx),
                "status-bar-progress",
            ))
        })
        // Phase 8: free-disk-space label sits between the task
        // summary and the Show-Hidden toggle. Only rendered when we
        // could query the volume info — non-macOS / sandboxed
        // builds skip it gracefully.
        .when_some(seg.free.filter(|_| plan.show_free), |this, label| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_color(theme_muted_fg.opacity(0.85))
                    .child(label),
            )
        })
        // App-footprint stats (up · CPU · MEM · rps), precomputed by
        // the off-thread sampler (system_stats.rs) — render only
        // formats a cached snapshot. Absent until the sampler's first
        // real reading, and always absent in screenshot mode unless
        // `--simulate-stats` pins deterministic values. First on the
        // chopping block when the window narrows: it is the one segment
        // that says nothing about the folder in front of the user.
        .when_some(seg.stats.filter(|_| plan.show_stats), |this, parts| {
            let mins = plan.stats_min_rems();
            let cell = |text: SharedString, min_w_rems: f32| {
                h_flex()
                    .flex_shrink_0()
                    .justify_end()
                    .min_w(rems(min_w_rems))
                    .child(text)
            };
            this.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .text_color(theme_muted_fg.opacity(0.85))
                    .child(cell(parts.up, mins[0]))
                    .child("\u{00B7}")
                    .child(cell(parts.cpu, mins[1]))
                    .child("\u{00B7}")
                    .child(cell(parts.mem, mins[2]))
                    .child("\u{00B7}")
                    .child(cell(parts.rps, mins[3])),
            )
        })
        // Hidden-content summary: what the Show-Hidden toggle beside it
        // would reveal. Same muted treatment as the free-space label.
        .when_some(
            seg.hidden.filter(|_| plan.show_hidden_chip),
            |this, label| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme_muted_fg.opacity(0.85))
                        .child(label),
                )
            },
        )
        // Phase 7 user ask: Show-Hidden moved out of the toolbar
        // and lives here next to the count + task summary. View-mode
        // toggle belongs alongside the rest of the status-bar state.
        // Its word is the last text to go and the switch itself never
        // does — at the narrowest widths a tooltip carries the meaning.
        .when_some(seg.switch_label, |this, label| {
            this.child(div().flex_shrink_0().child(label))
        })
        .child(
            div()
                .id("status-bar-hidden-toggle-box")
                .flex_shrink_0()
                .when(plan.density == Density::Minimal, |this| {
                    let tip = tr!("Show hidden");
                    this.tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                })
                .child(
                    gpui_component::switch::Switch::new("status-bar-hidden-toggle")
                        .checked(show_hidden)
                        .xsmall()
                        .when_some(on_toggle_hidden, |sw, cb| {
                            sw.on_click(move |_state, window, cx| {
                                // Switch's on_click hands us the new bool
                                // value; we don't need it here — Shell's
                                // toggle_hidden flips its own state from
                                // whatever the current Shell value is.
                                cb(window, cx);
                            })
                        }),
                ),
        )
}

fn task_label_none(registry: &TaskRegistry, simulated_progress: Option<f32>) -> bool {
    !registry.iter().any(|t| t.is_surfaced()) && simulated_progress.is_none()
}

/// Compact label for the spotlight task: its own label, plus a live
/// "· 320 MB/s · ~12s" tail when it's a transfer with a known rate. The
/// full breakdown (counts, current file) lives in the task panel.
fn label_with_rate(task: &crate::tasks::ActiveTask) -> String {
    let mut s = task.label.clone();
    if let Some(t) = &task.transfer {
        if t.bytes_per_sec >= 1.0 {
            s.push_str(&format!(
                " \u{00B7} {}/s",
                humanize_bytes(t.bytes_per_sec as u64)
            ));
        }
        if let Some(eta) = t.eta_secs {
            s.push_str(&format!(" \u{00B7} ~{}", humanize_secs(eta)));
        }
    }
    s
}

fn humanize_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        // Skip a zero remainder — rounded ETAs land on whole minutes
        // and "51m" reads better than "51m 0s".
        match (s / 60, s % 60) {
            (m, 0) => format!("{m}m"),
            (m, s) => format!("{m}m {s}s"),
        }
    } else {
        match (s / 3600, (s % 3600) / 60) {
            (h, 0) => format!("{h}h"),
            (h, m) => format!("{h}h {m}m"),
        }
    }
}

fn compute_progress(registry: &TaskRegistry, simulated_progress: Option<f32>) -> (bool, bool, f32) {
    if let Some(p) = simulated_progress {
        if p < 0.0 {
            return (true, true, 0.0);
        }
        return (true, false, p.clamp(0.0, 1.0));
    }
    // Only surfaced tasks drive the strip — an instant clone that lives
    // <150ms never paints a bar.
    if !registry.iter().any(|t| t.is_surfaced()) {
        return (false, false, 0.0);
    }
    let any_indeterminate = registry
        .iter()
        .filter(|t| t.is_surfaced())
        .any(|t| matches!(t.progress, TaskProgress::Indeterminate));
    if any_indeterminate {
        return (true, true, 0.0);
    }
    // All determinate — show the primary task's fraction.
    let fraction = match registry
        .primary()
        .filter(|t| t.is_surfaced())
        .map(|t| t.progress)
    {
        Some(TaskProgress::Determinate(p)) => p,
        _ => 0.0,
    };
    (true, false, fraction)
}

/// Status-bar progress strip — a thin accent strip on the right, sized
/// by the density plan (120 DIP at full width, narrower as the bar
/// tightens).
/// Uses `gpui_component::Progress` so the indeterminate state gets
/// the library's built-in sliding animation (we used to paint a
/// static 30%-wide fill, which read as a stuck progress bar rather
/// than ongoing work, and at certain themes the track and fill
/// merged into one flat white line).
fn progress_strip(width: f32, indeterminate: bool, fraction: f32, _cx: &mut App) -> Div {
    use gpui_component::{Sizable as _, progress::Progress};
    div()
        .flex_shrink_0()
        .w(rems(width / crate::text::BASE_REM_PX))
        .child(
            Progress::new("status-progress")
                .xsmall()
                .loading(indeterminate)
                .value(fraction.clamp(0.0, 1.0) * 100.0),
        )
}

#[cfg(test)]
mod count_label_tests {
    // Deliberately *not* `use super::*`: that re-imports `gpui::*`,
    // whose glob shadows the built-in `#[test]` with gpui's own test
    // macro, and expanding that here blows the crate's recursion limit.
    use super::{Density, StatusMetrics, count_labels, group_large_numbers};

    fn metrics(entries: usize, total: u64, filtered: usize, filtered_bytes: u64) -> StatusMetrics {
        StatusMetrics {
            entries,
            total_size: total,
            filtered_count: filtered,
            filtered_bytes,
            ..Default::default()
        }
    }

    #[test]
    fn no_filter_keeps_the_plain_count() {
        let (count, chip) = count_labels(&metrics(12, 3 * 1024 * 1024, 0, 0), Density::Full);
        assert_eq!(count, "12 items \u{00B7} 3.0 MB");
        assert!(chip.is_none());
    }

    #[test]
    fn large_counts_use_grouping_dots() {
        assert_eq!(group_large_numbers("4138016 items"), "4.138.016 items");
        assert_eq!(group_large_numbers("999 items"), "999 items");
        let (count, _) = count_labels(&metrics(4_138_016, 0, 0, 0), Density::Full);
        assert_eq!(count, "4.138.016 items · 0 B");
    }

    #[test]
    fn filter_adds_what_it_holds_back() {
        let (count, chip) = count_labels(
            &metrics(12, 3 * 1024 * 1024, 48, 12 * 1024 * 1024),
            Density::Full,
        );
        assert_eq!(count, "12 items \u{00B7} 3.0 MB");
        assert_eq!(chip.unwrap(), "48 filtered out \u{00B7} 12.0 MB");
    }

    #[test]
    fn selection_still_wins_the_count_and_keeps_the_chip() {
        let m = StatusMetrics {
            selected_count: 3,
            selected_size: 1024,
            ..metrics(12, 3 * 1024 * 1024, 48, 1024)
        };
        let (count, chip) = count_labels(&m, Density::Full);
        assert_eq!(count, "3 of 12 selected \u{00B7} 1.0 KB");
        assert!(chip.is_some());
    }

    #[test]
    fn everything_filtered_out_is_not_an_empty_folder() {
        let (count, chip) = count_labels(&metrics(0, 0, 60, 15 * 1024 * 1024), Density::Full);
        assert_eq!(count, "All 60 items filtered out \u{00B7} 15.0 MB");
        assert!(chip.is_none());
    }

    #[test]
    fn one_filtered_out_reads_singular() {
        let (count, _) = count_labels(&metrics(0, 0, 1, 2048), Density::Full);
        assert_eq!(count, "1 item filtered out \u{00B7} 2.0 KB");
    }

    #[test]
    fn a_genuinely_empty_folder_still_says_so() {
        let (count, chip) = count_labels(&metrics(0, 0, 0, 0), Density::Full);
        assert_eq!(count, "Empty folder");
        assert!(chip.is_none());
    }

    /// The minimal wording keeps every figure and drops only the words
    /// around them — a narrow bar must still answer "how many, how big".
    #[test]
    fn minimal_count_keeps_the_figures() {
        let (count, chip) = count_labels(
            &metrics(12, 3 * 1024 * 1024, 48, 12 * 1024 * 1024),
            Density::Minimal,
        );
        assert_eq!(count, "12 \u{00B7} 3.0 MB");
        assert_eq!(chip.unwrap(), "48 filtered");

        let m = StatusMetrics {
            selected_count: 3,
            selected_size: 1024,
            ..metrics(12, 3 * 1024 * 1024, 0, 0)
        };
        assert_eq!(count_labels(&m, Density::Minimal).0, "3/12 \u{00B7} 1.0 KB");
        assert_eq!(
            count_labels(&metrics(0, 0, 60, 15 * 1024 * 1024), Density::Minimal).0,
            "0/60 \u{00B7} 15.0 MB"
        );
    }
}

#[cfg(test)]
mod density_ladder_tests {
    use super::{Density, StatusMetrics, plan};

    /// A busy bar: every optional segment present, so the ladder has
    /// something to give up at each rung.
    fn busy() -> StatusMetrics {
        StatusMetrics {
            entries: 1_966_592,
            total_size: 109 * 1024 * 1024 * 1024,
            free_bytes: Some(126 * 1024 * 1024 * 1024),
            volume_name: Some("Macintosh HD".into()),
            hidden_count: 2,
            hidden_bytes: 52 * 1024,
            filtered_count: 40,
            filtered_bytes: 1024,
            stats: Some(crate::system_stats::SegmentParts::simulated()),
            ..Default::default()
        }
    }

    fn at(width: f32) -> super::Plan {
        plan(&busy(), width, crate::text::BASE_REM_PX, true, true).0
    }

    /// A wide window says everything, in sentences.
    #[test]
    fn wide_window_keeps_every_segment() {
        let p = at(2400.0);
        assert_eq!(p.density, Density::Full);
        assert!(!p.small_text);
        assert!(p.show_stats && p.show_free && p.show_hidden_chip && p.show_filtered);
    }

    /// Narrowing degrades monotonically: no width may bring back a
    /// segment (or a wordier density) that a wider one had already given
    /// up. Without this a resize could flap between two rungs.
    #[test]
    fn the_ladder_only_ever_goes_down() {
        let rung = |p: &super::Plan| {
            (
                match p.density {
                    Density::Full => 0,
                    Density::Short => 1,
                    Density::Minimal => 2,
                },
                p.small_text as u8,
                !p.show_stats as u8
                    + !p.show_hidden_chip as u8
                    + !p.show_filtered as u8
                    + !p.show_free as u8
                    + !p.show_task as u8,
            )
        };
        let mut prev = rung(&at(2400.0));
        let mut w = 2400.0;
        while w > 260.0 {
            w -= 20.0;
            let now = rung(&at(w));
            assert!(
                now >= prev,
                "narrowing to {w} px un-degraded the bar: {prev:?} -> {now:?}"
            );
            prev = now;
        }
    }

    /// Whatever the width, the two things the user needs survive: the
    /// count is always built, and the Show-Hidden switch is not on the
    /// ladder at all — only its *word* is.
    #[test]
    fn the_narrowest_bar_still_carries_count_and_switch() {
        let (p, seg) = plan(&busy(), 200.0, crate::text::BASE_REM_PX, true, true);
        assert_eq!(p.density, Density::Minimal);
        assert!(p.small_text);
        assert!(!seg.count.is_empty());
        assert!(
            seg.switch_label.is_none(),
            "the word goes, the switch stays"
        );
    }

    /// Telemetry goes before anything that describes the folder.
    #[test]
    fn stats_are_the_first_thing_dropped() {
        let mut width = 2400.0;
        let mut first_drop = None;
        while width > 260.0 {
            width -= 10.0;
            let p = at(width);
            if !p.show_stats || !p.show_free || !p.show_hidden_chip || !p.show_filtered {
                first_drop = Some(p);
                break;
            }
        }
        let p = first_drop.expect("the ladder must start hiding somewhere");
        assert!(!p.show_stats);
        assert!(p.show_free && p.show_hidden_chip && p.show_filtered);
    }

    /// A host with no window (headless render paths) must get the full
    /// bar, not the most degraded one.
    #[test]
    fn unknown_width_is_not_a_narrow_window() {
        let p = at(0.0);
        assert_eq!(p.density, Density::Full);
        assert!(p.show_stats);
    }
}
