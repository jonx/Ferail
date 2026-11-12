//! Compare Folders: the two panes of a split window, compared by name and
//! content, with a one-click sync in either direction
//! (docs/features/COMPARE_FOLDERS.md).
//!
//! The comparison is a worker in `ferail_fs_native::folder_compare`, run on
//! the background executor under a generation counter and a cancel flag,
//! the shape of `Shell::load_path_for_tab`. Verdicts land in a name-keyed
//! map shared with both panes' table delegates, which tint their rows from
//! it at render time. Sync is a plan over those verdicts handed to the
//! ordinary transfer engine.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferail_core::EntryKind;
use ferail_core::counts::format_count;
use ferail_fs_native::file_ops::CollisionPolicy;
use ferail_fs_native::folder_compare::{self, ComparePair, CompareState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Axis, Context, Hsla, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled, Window, div, px,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};

use super::tab::TabId;
use super::{CompareFolders, Shell, TransferMode};
use crate::dupe_cache::DbHashCache;
use crate::overlay::OverlayWindowExt as _;
use crate::tasks::{TaskId, TaskKind};
use crate::text::TextScale as _;

/// Verdicts by entry name, shared between the running comparison and the
/// two compared panes' table delegates.
pub type CompareMarks = Rc<RefCell<HashMap<Arc<str>, CompareState>>>;

/// Messages queued between the worker and the UI.
const COMPARE_CHANNEL_MESSAGES: usize = 16;
/// Upper bound on verdicts in one message, and on the time a partial
/// batch waits before it is sent.
const VERDICT_BATCH: usize = 256;
const VERDICT_FLUSH: Duration = Duration::from_millis(120);
/// The UI applies queued messages at most this often.
const COMPARE_UI_TICK: Duration = Duration::from_millis(60);

/// What the sync needs to know about one side of a compared name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SideFacts {
    pub kind: EntryKind,
    pub mtime_unix: i64,
}

/// One name of the union of both folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompareRow {
    pub name: Arc<str>,
    pub state: CompareState,
    /// `[first pane, second pane]`.
    pub sides: [Option<SideFacts>; 2],
}

impl CompareRow {
    fn from_pair(pair: &ComparePair) -> Self {
        let facts = |item: &Option<folder_compare::CompareItem>| {
            item.as_ref().map(|i| SideFacts {
                kind: i.kind,
                mtime_unix: i.mtime_unix,
            })
        };
        Self {
            name: Arc::from(pair.name.as_str()),
            state: pair.state,
            sides: [facts(&pair.left), facts(&pair.right)],
        }
    }
}

/// Tallies for the summary strip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompareCounts {
    /// Names only in the first, and only in the second, pane.
    pub only: [usize; 2],
    pub different: usize,
    pub identical: usize,
    pub pending: usize,
    pub unknown: usize,
}

pub(crate) fn count_rows(rows: &[CompareRow]) -> CompareCounts {
    let mut c = CompareCounts::default();
    for row in rows {
        match row.state {
            CompareState::OnlyLeft => c.only[0] += 1,
            CompareState::OnlyRight => c.only[1] += 1,
            CompareState::Different => c.different += 1,
            CompareState::Identical => c.identical += 1,
            CompareState::Pending => c.pending += 1,
            CompareState::Unknown => c.unknown += 1,
        }
    }
    c
}

/// Names one sync direction copies from slot `from` into the other slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncPlan {
    /// Present on the source side only: copied as they are, folders whole.
    pub copy: Vec<Arc<str>>,
    /// Files that differ and are newer on the source side: they replace
    /// the older file on the other side.
    pub replace: Vec<Arc<str>>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.copy.is_empty() && self.replace.is_empty()
    }
}

/// What "copy missing and newer" sends from slot `from` to the other slot.
///
/// Never deletes: nothing that exists only on the destination side is
/// touched. Two different folders are not merged, because the engine
/// replaces a folder as a whole and that would delete what only the older
/// folder holds; the user opens them in both panes to sync their insides.
/// A file against a folder of the same name is left alone for the same
/// reason. Equal modification times name no newer side, and a name whose
/// verdict has not landed yet is not touched.
pub(crate) fn sync_plan(rows: &[CompareRow], from: usize) -> SyncPlan {
    let to = 1 - from;
    let only_here = if from == 0 {
        CompareState::OnlyLeft
    } else {
        CompareState::OnlyRight
    };
    let mut plan = SyncPlan::default();
    for row in rows {
        if row.state == only_here {
            plan.copy.push(row.name.clone());
            continue;
        }
        if row.state != CompareState::Different {
            continue;
        }
        let (Some(src), Some(dst)) = (row.sides[from], row.sides[to]) else {
            continue;
        };
        let leaf = |k: EntryKind| k != EntryKind::Directory;
        if leaf(src.kind) && leaf(dst.kind) && src.mtime_unix > dst.mtime_unix {
            plan.replace.push(row.name.clone());
        }
    }
    plan
}

/// The comparison a window is showing.
pub(crate) struct FolderCompare {
    /// Tabs of the first and second pane.
    pub panes: [TabId; 2],
    pub dirs: [PathBuf; 2],
    pub axis: Axis,
    generation: u64,
    cancel: Arc<AtomicBool>,
    task: Option<TaskId>,
    rows: Vec<CompareRow>,
    index: HashMap<Arc<str>, usize>,
    marks: CompareMarks,
    pub counts: CompareCounts,
    /// Whether each direction has anything to copy.
    pub can_sync: [bool; 2],
    pub listed: bool,
    pub done: bool,
    pub error: Option<String>,
}

impl FolderCompare {
    fn recount(&mut self) {
        self.counts = count_rows(&self.rows);
        self.can_sync = [
            !sync_plan(&self.rows, 0).is_empty(),
            !sync_plan(&self.rows, 1).is_empty(),
        ];
    }
}

enum CompareMsg {
    Listed(Vec<ComparePair>),
    Verdicts(Vec<(usize, CompareState)>),
    Done(Option<String>),
}

/// Worker body: list both folders, send the pairing, then settle every
/// pending pair and stream the verdicts. Files are checked before folders,
/// whose recursive walk can take much longer.
fn run_compare(
    dirs: [PathBuf; 2],
    include_hidden: bool,
    cache: Option<DbHashCache>,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<CompareMsg>,
) {
    use ferail_fs_native::DupeHashCache;
    let list = |dir: &PathBuf| {
        folder_compare::list_compare_items(dir, include_hidden, &cancel)
            .map_err(|e| format!("{}: {e}", dir.display()))
    };
    let listed = list(&dirs[0]).and_then(|a| list(&dirs[1]).map(|b| (a, b)));
    let (left, right) = match listed {
        Ok(pair) => pair,
        Err(e) => {
            let _ = tx.send_blocking(CompareMsg::Done(Some(e)));
            return;
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let pairs = folder_compare::pair_listings(left, right);
    let mut order: Vec<usize> = (0..pairs.len())
        .filter(|&i| pairs[i].state == CompareState::Pending)
        .collect();
    order.sort_by_key(|&i| {
        pairs[i]
            .left
            .as_ref()
            .is_some_and(|item| item.kind == EntryKind::Directory)
    });
    if tx.send_blocking(CompareMsg::Listed(pairs.clone())).is_err() {
        return;
    }
    let cache_ref = cache.as_ref().map(|c| c as &dyn DupeHashCache);
    let mut batch = Vec::new();
    let mut last_flush = Instant::now();
    for i in order {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let state = folder_compare::verify_pair(&dirs[0], &dirs[1], &pairs[i], cache_ref, &cancel);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        batch.push((i, state));
        if batch.len() >= VERDICT_BATCH || last_flush.elapsed() >= VERDICT_FLUSH {
            if tx
                .send_blocking(CompareMsg::Verdicts(std::mem::take(&mut batch)))
                .is_err()
            {
                return;
            }
            last_flush = Instant::now();
        }
    }
    if !batch.is_empty() && tx.send_blocking(CompareMsg::Verdicts(batch)).is_err() {
        return;
    }
    let _ = tx.send_blocking(CompareMsg::Done(None));
}

/// Tint a file-list row with its comparison verdict. Theme tokens at low
/// opacity, like the Ant Trail heat and the cut dimming, so it reads in
/// light and dark themes and stays under the selection fill.
pub(crate) fn compare_row_tint<E: Styled>(row: E, state: CompareState, cx: &App) -> E {
    match compare_color(state, cx) {
        Some(color) => row.bg(color.opacity(ROW_TINT_OPACITY)),
        None => row,
    }
}

const ROW_TINT_OPACITY: f32 = 0.16;

/// The colour of a verdict; `None` for the neutral states, which stay
/// untinted until a verdict lands.
fn compare_color(state: CompareState, cx: &App) -> Option<Hsla> {
    let theme = cx.theme();
    match state {
        CompareState::OnlyLeft | CompareState::OnlyRight => Some(theme.info),
        CompareState::Different => Some(theme.warning),
        CompareState::Identical => Some(theme.success),
        CompareState::Pending | CompareState::Unknown => None,
    }
}

impl Shell {
    /// The two panes' tabs and folders, when the window shows a split with
    /// a plain folder listing in each pane.
    fn comparable_pair(&self) -> Option<([TabId; 2], [PathBuf; 2], Axis)> {
        let split = self.split.as_ref()?;
        let tab = |id: TabId| self.tabs.iter().find(|t| t.id == id);
        let (a, b) = (tab(split.panes[0])?, tab(split.panes[1])?);
        let plain = |t: &super::tab::Tab| t.tool_result.is_none() && t.platform_namespace.is_none();
        (plain(a) && plain(b)).then(|| {
            (
                split.panes,
                [a.current_dir.clone(), b.current_dir.clone()],
                split.axis,
            )
        })
    }

    pub(super) fn on_compare_folders(
        &mut self,
        _: &CompareFolders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_split();
        if self.compare.is_some() {
            self.stop_compare(cx);
            return;
        }
        match self.comparable_pair() {
            Some((panes, dirs, axis)) => self.start_compare(panes, dirs, axis, cx),
            None => window.push_notice(
                gpui_component::notification::Notification::info(tr!(
                    "Compare Folders needs a split view with a folder in each pane."
                )),
                cx,
            ),
        }
    }

    /// Keep a running comparison in line with the split: follow a pane that
    /// shows another folder or another tab, and stop when the split closes
    /// or a pane stops showing a folder. Called while rendering the split
    /// area; it only schedules work.
    pub(super) fn sync_compare(&mut self, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_ref() else {
            return;
        };
        match self.comparable_pair() {
            None => self.stop_compare(cx),
            Some((panes, dirs, axis)) => {
                if panes != compare.panes || dirs != compare.dirs {
                    self.start_compare(panes, dirs, axis, cx);
                } else if axis != compare.axis {
                    if let Some(compare) = self.compare.as_mut() {
                        compare.axis = axis;
                    }
                }
            }
        }
    }

    /// A compared pane (re)loaded its folder: Refresh, a watcher reload
    /// after a sync, or a navigation. Compare again from the new listing.
    pub(super) fn compare_after_tab_load(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_ref() else {
            return;
        };
        if !compare.panes.contains(&tab) {
            return;
        }
        match self.comparable_pair() {
            Some((panes, dirs, axis)) => self.start_compare(panes, dirs, axis, cx),
            None => self.stop_compare(cx),
        }
    }

    fn set_pane_marks(&mut self, panes: [TabId; 2], marks: Option<CompareMarks>, cx: &mut App) {
        for id in panes {
            let Some(tab) = self.tabs.iter().find(|t| t.id == id) else {
                continue;
            };
            let marks = marks.clone();
            tab.table.update(cx, |state, cx| {
                state.delegate_mut().compare_marks = marks;
                cx.notify();
            });
        }
    }

    fn cancel_compare_worker(&mut self) {
        if let Some(compare) = self.compare.as_mut() {
            compare.cancel.store(true, Ordering::Relaxed);
            if let Some(task) = compare.task.take() {
                self.process.tasks.borrow_mut().end(task);
            }
        }
    }

    pub(super) fn stop_compare(&mut self, cx: &mut Context<Self>) {
        self.cancel_compare_worker();
        if let Some(compare) = self.compare.take() {
            self.set_pane_marks(compare.panes, None, cx);
        }
        cx.notify();
    }

    fn start_compare(
        &mut self,
        panes: [TabId; 2],
        dirs: [PathBuf; 2],
        axis: Axis,
        cx: &mut Context<Self>,
    ) {
        self.cancel_compare_worker();
        // Panes that stop being compared lose their tint.
        if let Some(old) = self.compare.take() {
            let gone: Vec<TabId> = old
                .panes
                .iter()
                .copied()
                .filter(|id| !panes.contains(id))
                .collect();
            for id in gone {
                if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
                    tab.table.update(cx, |state, cx| {
                        state.delegate_mut().compare_marks = None;
                        cx.notify();
                    });
                }
            }
        }
        self.compare_generation = self.compare_generation.wrapping_add(1);
        let generation = self.compare_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let name = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        let task = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::FolderCompare,
            tr!(
                "Comparing {left} and {right}",
                left = name(&dirs[0]),
                right = name(&dirs[1])
            )
            .to_string(),
            cancel.clone(),
        );
        let marks: CompareMarks = Rc::new(RefCell::new(HashMap::new()));
        self.set_pane_marks(panes, Some(marks.clone()), cx);
        self.compare = Some(FolderCompare {
            panes,
            dirs: dirs.clone(),
            axis,
            generation,
            cancel: cancel.clone(),
            task: Some(task),
            rows: Vec::new(),
            index: HashMap::new(),
            marks,
            counts: CompareCounts::default(),
            can_sync: [false; 2],
            listed: false,
            done: false,
            error: None,
        });

        let cache = self.process.db_snapshot().map(|db| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            DbHashCache::new(db, now)
        });
        let include_hidden = self.show_hidden;
        let (tx, rx) = async_channel::bounded(COMPARE_CHANNEL_MESSAGES);
        cx.background_executor()
            .spawn(async move {
                run_compare(dirs, include_hidden, cache, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                let mut msgs = vec![first];
                while let Ok(msg) = rx.try_recv() {
                    msgs.push(msg);
                }
                let done = msgs.iter().any(|m| matches!(m, CompareMsg::Done(_)));
                let stale = this
                    .update(cx, |this, cx| {
                        if this
                            .compare
                            .as_ref()
                            .is_none_or(|c| c.generation != generation)
                        {
                            return true;
                        }
                        for msg in msgs {
                            this.apply_compare_msg(msg, cx);
                        }
                        false
                    })
                    .unwrap_or(true);
                if stale || done {
                    break;
                }
                cx.background_executor().timer(COMPARE_UI_TICK).await;
            }
        })
        .detach();
        cx.notify();
    }

    fn apply_compare_msg(&mut self, msg: CompareMsg, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_mut() else {
            return;
        };
        match msg {
            CompareMsg::Listed(pairs) => {
                compare.rows = pairs.iter().map(CompareRow::from_pair).collect();
                compare.index = compare
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, row)| (row.name.clone(), i))
                    .collect();
                let mut marks = compare.marks.borrow_mut();
                marks.clear();
                marks.extend(compare.rows.iter().map(|r| (r.name.clone(), r.state)));
                drop(marks);
                compare.listed = true;
            }
            CompareMsg::Verdicts(verdicts) => {
                let mut marks = compare.marks.borrow_mut();
                for (i, state) in verdicts {
                    if let Some(row) = compare.rows.get_mut(i) {
                        row.state = state;
                        marks.insert(row.name.clone(), state);
                    }
                }
            }
            CompareMsg::Done(error) => {
                compare.done = true;
                compare.error = error;
                if let Some(task) = compare.task.take() {
                    self.process.tasks.borrow_mut().end(task);
                }
            }
        }
        let Some(compare) = self.compare.as_mut() else {
            return;
        };
        compare.recount();
        let panes = compare.panes;
        for id in panes {
            if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
                tab.table.update(cx, |_, cx| cx.notify());
            }
        }
        cx.notify();
    }

    /// Copy what is missing on the other side, and the newer version of each
    /// file that differs, from pane `from` into the other pane's folder.
    /// Runs as an ordinary transfer: progress, cancel, collision prompt for
    /// anything that appeared since the comparison, undo when nothing was
    /// replaced. The destination's reload then refreshes the comparison.
    fn run_compare_sync(&mut self, from: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_ref() else {
            return;
        };
        let plan = sync_plan(&compare.rows, from);
        if plan.is_empty() {
            return;
        }
        let src_dir = compare.dirs[from].clone();
        let dest = compare.dirs[1 - from].clone();
        let mut presets = HashMap::new();
        let mut sources = Vec::with_capacity(plan.copy.len() + plan.replace.len());
        for name in &plan.copy {
            sources.push(src_dir.join(name.as_ref()));
        }
        for name in &plan.replace {
            let src = src_dir.join(name.as_ref());
            presets.insert(src.clone(), CollisionPolicy::Replace);
            sources.push(src);
        }
        let replaced = plan.replace.len();
        // A replaced file cannot be undone, so replacing asks first. A sync
        // that only adds missing items is undoable and starts at once.
        // `confirm_fanout` asks from its threshold up: force it with MAX.
        let force = if replaced > 0 { usize::MAX } else { 0 };
        self.confirm_fanout(
            force,
            tr!("Replace older files?"),
            trn!(
                "{n} older file in \u{201c}{folder}\u{201d} will be replaced by the newer version from the other side. This cannot be undone.",
                "{n} older files in \u{201c}{folder}\u{201d} will be replaced by the newer version from the other side. This cannot be undone.",
                replaced,
                folder = crate::private_mode::present_leaf_str(
                    &dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    false
                )
            )
            .to_string(),
            tr!("Replace"),
            window,
            cx,
            move |this, window, cx| {
                this.spawn_transfer_op_with_presets(sources, dest, TransferMode::Copy, presets, window, cx);
            },
        );
    }

    /// The legend and sync strip shown above the panes while comparing.
    pub(super) fn compare_strip(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let compare = self.compare.as_ref()?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let swatch = |color: Option<Hsla>| {
            div()
                .flex_none()
                .size(px(10.0))
                .rounded(px(2.0))
                .map(|d| match color {
                    Some(c) => d.bg(c.opacity(0.75)),
                    None => d.border_1().border_color(muted),
                })
        };
        let chip = |color: Option<Hsla>, label: gpui::SharedString| {
            h_flex()
                .flex_none()
                .gap_1()
                .items_center()
                .child(swatch(color))
                .child(label)
        };
        let c = compare.counts;
        let n = |v: usize| format_count(v as u64);
        let horizontal = compare.axis == Axis::Horizontal;
        let (only_first, only_second) = if horizontal {
            (
                tr!("Only left: {count}", count = n(c.only[0])),
                tr!("Only right: {count}", count = n(c.only[1])),
            )
        } else {
            (
                tr!("Only top: {count}", count = n(c.only[0])),
                tr!("Only bottom: {count}", count = n(c.only[1])),
            )
        };
        let info = theme.info;
        let warning = theme.warning;
        let success = theme.success;
        let mut legend = h_flex().flex_1().min_w_0().gap_3().items_center().flex_wrap();
        if let Some(error) = &compare.error {
            legend = legend.child(
                div()
                    .text_color(theme.danger)
                    .child(tr!("Could not compare: {error}", error = error.as_str())),
            );
        } else {
            legend = legend
                .child(chip(Some(info), only_first))
                .child(chip(Some(info), only_second))
                .child(chip(
                    Some(warning),
                    tr!("Different: {count}", count = n(c.different)),
                ))
                .child(chip(
                    Some(success),
                    tr!("Identical: {count}", count = n(c.identical)),
                ))
                .when(c.unknown > 0, |l| {
                    l.child(chip(
                        None,
                        tr!("Not compared: {count}", count = n(c.unknown)),
                    ))
                })
                .when(!compare.done, |l| {
                    l.child(
                        div()
                            .text_color(muted)
                            .child(tr!("Comparing\u{2026}")),
                    )
                });
        }
        let (to_second, to_first, icon_second, icon_first) = if horizontal {
            (
                tr!("Copy missing and newer to right"),
                tr!("Copy missing and newer to left"),
                "icons/arrow-right.svg",
                "icons/arrow-left.svg",
            )
        } else {
            (
                tr!("Copy missing and newer to bottom"),
                tr!("Copy missing and newer to top"),
                "icons/arrow-down.svg",
                "icons/arrow-up.svg",
            )
        };
        let tooltip = tr!(
            "Copies items missing on the other side and replaces older versions of files that differ. Nothing is deleted. Folders that differ are not merged: open them in both panes."
        );
        let sync_button = |id: &'static str,
                           from: usize,
                           label: gpui::SharedString,
                           icon: &'static str,
                           enabled: bool| {
            Button::new(id)
                .xsmall()
                .outline()
                .icon(gpui_component::Icon::empty().path(icon))
                .label(label)
                .tooltip(tooltip.clone())
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_compare_sync(from, window, cx);
                }))
        };
        let strip = h_flex()
            .id("compare-strip")
            .flex_none()
            .w_full()
            .gap_3()
            .px_2()
            .py_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .text_scale_xs()
            .child(legend)
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .child(sync_button(
                        "compare-sync-to-first",
                        1,
                        to_first,
                        icon_first,
                        compare.can_sync[1],
                    ))
                    .child(sync_button(
                        "compare-sync-to-second",
                        0,
                        to_second,
                        icon_second,
                        compare.can_sync[0],
                    )),
            );
        Some(strip.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: CompareState, l: Option<(EntryKind, i64)>, r: Option<(EntryKind, i64)>) -> CompareRow {
        let facts = |s: Option<(EntryKind, i64)>| {
            s.map(|(kind, mtime_unix)| SideFacts { kind, mtime_unix })
        };
        CompareRow {
            name: Arc::from(name),
            state,
            sides: [facts(l), facts(r)],
        }
    }

    fn names(v: &[Arc<str>]) -> Vec<&str> {
        v.iter().map(|n| n.as_ref()).collect()
    }

    fn sample() -> Vec<CompareRow> {
        use CompareState::*;
        use EntryKind::{Directory as D, File as F};
        vec![
            row("only-left.txt", OnlyLeft, Some((F, 1)), None),
            row("only-left-dir", OnlyLeft, Some((D, 1)), None),
            row("only-right.txt", OnlyRight, None, Some((F, 1))),
            row("newer-left.txt", Different, Some((F, 20)), Some((F, 10))),
            row("newer-right.txt", Different, Some((F, 10)), Some((F, 20))),
            row("same-time.txt", Different, Some((F, 10)), Some((F, 10))),
            row("dirs-differ", Different, Some((D, 30)), Some((D, 10))),
            row("file-vs-dir", Different, Some((F, 30)), Some((D, 10))),
            row("identical.txt", Identical, Some((F, 30)), Some((F, 10))),
            row("pending.txt", Pending, Some((F, 30)), Some((F, 10))),
            row("unknown.txt", Unknown, Some((F, 30)), Some((F, 10))),
        ]
    }

    #[test]
    fn compare_sync_plan_to_the_right() {
        let plan = sync_plan(&sample(), 0);
        assert_eq!(names(&plan.copy), vec!["only-left.txt", "only-left-dir"]);
        assert_eq!(names(&plan.replace), vec!["newer-left.txt"]);
    }

    #[test]
    fn compare_sync_plan_to_the_left() {
        let plan = sync_plan(&sample(), 1);
        assert_eq!(names(&plan.copy), vec!["only-right.txt"]);
        assert_eq!(names(&plan.replace), vec!["newer-right.txt"]);
    }

    #[test]
    fn compare_sync_plan_never_touches_unsettled_or_folder_pairs() {
        let rows = sample();
        for from in [0, 1] {
            let plan = sync_plan(&rows, from);
            let all: Vec<&str> = names(&plan.copy)
                .into_iter()
                .chain(names(&plan.replace))
                .collect();
            for skipped in [
                "same-time.txt",
                "dirs-differ",
                "file-vs-dir",
                "identical.txt",
                "pending.txt",
                "unknown.txt",
            ] {
                assert!(!all.contains(&skipped), "{skipped} must not sync");
            }
        }
        assert!(sync_plan(&[], 0).is_empty());
    }

    #[test]
    fn compare_counts_tally_every_state() {
        let c = count_rows(&sample());
        assert_eq!(c.only, [2, 1]);
        assert_eq!(c.different, 5);
        assert_eq!(c.identical, 1);
        assert_eq!(c.pending, 1);
        assert_eq!(c.unknown, 1);
    }

    #[test]
    fn compare_rows_carry_listing_and_verdict() {
        use ferail_fs_native::folder_compare::{CompareItem, pair_listings};
        let item = |name: &str, size: u64, mtime_unix: i64| CompareItem {
            name: name.into(),
            kind: EntryKind::File,
            size,
            mtime_unix,
            dataless: false,
        };
        let pairs = pair_listings(
            vec![item("a", 1, 5), item("b", 2, 50)],
            vec![item("b", 3, 40), item("c", 1, 5)],
        );
        let rows: Vec<CompareRow> = pairs.iter().map(CompareRow::from_pair).collect();
        assert_eq!(rows[0].state, CompareState::OnlyLeft);
        assert_eq!(rows[1].state, CompareState::Different);
        assert_eq!(rows[2].state, CompareState::OnlyRight);
        // Different sizes: the newer left file replaces the right one.
        let plan = sync_plan(&rows, 0);
        assert_eq!(names(&plan.copy), vec!["a"]);
        assert_eq!(names(&plan.replace), vec!["b"]);
    }
}
