//! The viewer window entity: playlist navigation, sticky zoom, and
//! (iter 5) slideshow playback. docs/features/VIEWER.md.
//!
//! One reusable window per process — `super::open_viewer` retargets a
//! live window instead of stacking new ones. Keyboard goes through
//! gpui actions gated on [`VIEWER_CONTEXT`] so Shell shortcuts can't
//! fire here and vice versa.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable, button::Button, h_flex, v_flex};

use std::time::Duration;

use super::loader::{self, FrameState, ViewerFrame};
use super::playback::Playback;
use super::stage::{self, StageState, ZoomMode};
use crate::process_state::ProcessState;

/// Key-binding context for the viewer window (`KeyBinding::new(_, _,
/// Some(VIEWER_CONTEXT))` in `keymap::install_extras`).
pub const VIEWER_CONTEXT: &str = "Viewer";

actions!(
    viewer,
    [
        ViewerPrev,
        ViewerNext,
        ViewerZoomIn,
        ViewerZoomOut,
        ViewerZoomReset,
        ViewerActualSize,
        ViewerToggleFullscreen,
        ViewerTogglePlay,
        ViewerDismiss
    ]
);

/// One playlist item, snapshotted from the file list at open time.
/// The viewer never re-reads the directory (prime directive — the
/// snapshot is the contract; live sync is deferred to watcher work).
#[derive(Clone)]
pub struct PlaylistEntry {
    pub path: PathBuf,
    pub name: String,
}

const TOOLBAR_H: f32 = 44.0;
const STATUS_H: f32 = 28.0;
/// Per-step zoom factor for the toolbar buttons / Cmd+= / Cmd+-.
const ZOOM_STEP: f32 = 1.25;
/// Fullscreen: hovering within this many px of the window top reveals
/// the hidden toolbar.
const CHROME_REVEAL_STRIP: f32 = 56.0;

/// Extensions routed to the native video overlay — the formats
/// AVFoundation reliably plays. Everything else stays a Quick Look
/// poster. [mac]; win-parity revisits the set with Media Foundation.
const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov"];

fn is_video(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub struct ViewerWindow {
    playlist: Vec<PlaylistEntry>,
    index: usize,
    cache: loader::ViewerCache,
    /// Sticky zoom/pan — survives navigation by design.
    stage: StageState,
    focus_handle: FocusHandle,
    /// Stage-area size captured at render time so keyboard zoom (which
    /// has no cursor position) can anchor at the viewport center.
    last_stage_size: (f32, f32),
    /// Last cursor position of an in-flight left-button drag, in
    /// stage-local coordinates. `None` when not dragging.
    drag_last: Option<(f32, f32)>,
    /// Stage area's top edge in window coordinates, captured at render
    /// time (0 in fullscreen, TOOLBAR_H otherwise).
    stage_origin_y: f32,
    /// Slideshow state (docs/features/VIEWER.md §playback).
    playback: Playback,
    /// In fullscreen the chrome hides; hovering the top strip brings
    /// the toolbar back. Pure mouse-position state — no timers.
    chrome_hover: bool,
    /// Title last pushed to the platform window, so render-time title
    /// sync only crosses into AppKit when the text actually changed.
    last_title: String,
    /// Live native video overlay: (platform handle, the entry path it
    /// plays). `None` when the current entry isn't a video. [mac]
    video_overlay: Option<(u64, PathBuf)>,
    /// Frame last pushed to the overlay, to skip no-op AppKit calls.
    video_frame: (f32, f32, f32, f32),
    /// Video-ended events, keyed by entry path so a stale end (user
    /// already navigated away) is dropped instead of advancing.
    video_ended_tx: async_channel::Sender<PathBuf>,
    /// Process singleton — the shared 512 px preview cache doubles as
    /// an instant placeholder while the full-res decode is in flight.
    process: Rc<ProcessState>,
}

impl ViewerWindow {
    pub fn new(
        playlist: Vec<PlaylistEntry>,
        start: usize,
        process: Rc<ProcessState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let interval = crate::app_state::load()
            .viewer_slideshow_interval
            .unwrap_or(super::playback::DEFAULT_INTERVAL_SECS);
        // Video-ended events arrive from the AppKit main loop with no
        // App context; the drain task re-enters through entity update.
        let (video_ended_tx, video_ended_rx) = async_channel::unbounded::<PathBuf>();
        cx.spawn(async move |this, cx| {
            while let Ok(path) = video_ended_rx.recv().await {
                let Some(this) = this.upgrade() else { break };
                this.update(cx, |v, cx| v.on_video_ended(&path, cx));
            }
        })
        .detach();
        let mut this = Self {
            index: start.min(playlist.len().saturating_sub(1)),
            playlist,
            cache: loader::ViewerCache::default(),
            stage: StageState::default(),
            focus_handle,
            last_stage_size: (1100.0, 760.0 - TOOLBAR_H - STATUS_H),
            drag_last: None,
            stage_origin_y: TOOLBAR_H,
            playback: Playback::new(interval),
            chrome_hover: false,
            last_title: String::new(),
            video_overlay: None,
            video_frame: (0.0, 0.0, 0.0, 0.0),
            video_ended_tx,
            process,
        };
        this.request_current(cx);
        this.prefetch_neighbors(cx);
        this
    }

    /// Point the live window at a new playlist (user invoked Open
    /// Viewer again). Fresh intent → fresh zoom; the frame cache stays,
    /// revisiting the same folder is instant.
    pub fn retarget(
        &mut self,
        playlist: Vec<PlaylistEntry>,
        start: usize,
        cx: &mut Context<Self>,
    ) {
        self.index = start.min(playlist.len().saturating_sub(1));
        self.playlist = playlist;
        self.stage = StageState::default();
        self.playback.playing = false;
        self.playback.bump();
        self.request_current(cx);
        self.prefetch_neighbors(cx);
        cx.notify();
    }

    fn current(&self) -> Option<&PlaylistEntry> {
        self.playlist.get(self.index)
    }

    /// Render-time title sync. The platform call only happens when the
    /// text changed, so per-frame cost is one string compare.
    fn sync_title(&mut self, window: &mut Window) {
        let title = match self.current() {
            Some(e) => format!("{} \u{2014} {} of {}", e.name, self.index + 1, self.playlist.len()),
            None => "Viewer".to_string(),
        };
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }
    }

    /// Kick the background decode for the current entry unless the
    /// cache already has it (in any state — Pending dedups in-flight
    /// work, Failed prevents retry storms). Same shape as
    /// `preview::request`.
    fn request_current(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.current().map(|e| e.path.clone()) else {
            return;
        };
        self.request_path(path, cx);
    }

    fn request_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.cache.get(&path).is_some() {
            return;
        }
        self.cache.insert(path.clone(), FrameState::Pending);
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let p = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { loader::decode_full_res(&p) })
                .await;
            let Some(this) = weak.upgrade() else { return };
            this.update(cx, |this, cx| {
                let state = match result {
                    Some((rgba, w, h)) => {
                        FrameState::Loaded(Arc::new(loader::build_frame(rgba, w, h)))
                    }
                    None => FrameState::Failed,
                };
                this.cache.insert(path, state);
                cx.notify();
            });
        })
        .detach();
    }

    /// Move through the playlist with wrap-around. The stage state is
    /// deliberately NOT touched — sticky zoom is the feature. Any
    /// navigation (manual or timer-driven) re-arms the slideshow
    /// timer when playing, so manual skips don't stop the show.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.playlist.len();
        if len == 0 {
            return;
        }
        self.index = (self.index as isize + delta).rem_euclid(len as isize) as usize;
        self.request_current(cx);
        self.prefetch_neighbors(cx);
        let epoch = self.playback.bump();
        // Video entries advance on their own end-of-playback event,
        // not the interval timer — a 4-minute clip plays through.
        if self.playback.playing && !self.current_is_video() {
            self.arm_timer(epoch, cx);
        }
        cx.notify();
    }

    // -- video overlay [mac] ---------------------------------------

    /// True when the *current* entry plays through the native overlay
    /// (slideshow advance is then driven by the video's end, not the
    /// interval timer).
    fn current_is_video(&self) -> bool {
        self.current().map(|e| is_video(&e.path)).unwrap_or(false)
    }

    /// Render-time overlay sync, change-detected like `sync_title`:
    /// attach when the current entry became a video, retarget on nav,
    /// reposition on resize, tear down otherwise. Overlay creation
    /// does no blocking I/O (AVFoundation loads media asynchronously);
    /// steady-state frames compare two tuples and do nothing.
    fn sync_video(&mut self, window: &mut Window, rect: (f32, f32, f32, f32)) {
        let want = self
            .current()
            .map(|e| e.path.clone())
            .filter(|p| is_video(p));
        match (&want, &self.video_overlay) {
            (None, None) => {}
            (None, Some(_)) => self.teardown_video(),
            (Some(p), existing) => {
                if let Some((id, current)) = existing {
                    if current == p {
                        if rect != self.video_frame {
                            crate::platform_shell::video_overlay_set_frame(*id, rect_f64(rect));
                            self.video_frame = rect;
                        }
                        return;
                    }
                }
                self.teardown_video();
                let Some(ns_view) = content_ns_view(window) else {
                    return;
                };
                let tx = self.video_ended_tx.clone();
                let ended_path = p.clone();
                let id = crate::platform_shell::video_overlay_show(
                    ns_view,
                    p,
                    rect_f64(rect),
                    Box::new(move || {
                        let _ = tx.try_send(ended_path.clone());
                    }),
                );
                if id != 0 {
                    self.video_overlay = Some((id, p.clone()));
                    self.video_frame = rect;
                }
            }
        }
    }

    fn teardown_video(&mut self) {
        if let Some((id, _)) = self.video_overlay.take() {
            crate::platform_shell::video_overlay_remove(id);
        }
    }

    /// A video played to its end. Only advances when the show is
    /// playing AND the event belongs to the entry still on screen —
    /// ends queued behind a manual navigation are dropped.
    fn on_video_ended(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        let current = self.current().map(|e| e.path.clone());
        if self.playback.playing && current.as_ref() == Some(path) {
            self.step(1, cx);
        }
    }

    // -- slideshow --------------------------------------------------

    fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        if self.playback.playing == playing {
            return;
        }
        self.playback.playing = playing;
        let epoch = self.playback.bump();
        if playing && !self.current_is_video() {
            self.arm_timer(epoch, cx);
        }
        cx.notify();
    }

    /// One-shot timer tick. The epoch check makes stale ticks inert —
    /// the same staleness idiom enumeration cancel flags use.
    fn arm_timer(&mut self, epoch: u64, cx: &mut Context<Self>) {
        let interval = Duration::from_secs(self.playback.interval_secs);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |this, cx| {
                if this.playback.playing && this.playback.epoch == epoch {
                    this.step(1, cx);
                }
            });
        })
        .detach();
    }

    fn cycle_interval(&mut self, cx: &mut Context<Self>) {
        self.playback.interval_secs = Playback::next_interval(self.playback.interval_secs);
        if self.playback.playing && !self.current_is_video() {
            let epoch = self.playback.bump();
            self.arm_timer(epoch, cx);
        }
        let mut state = crate::app_state::load();
        state.viewer_slideshow_interval = Some(self.playback.interval_secs);
        crate::app_state::save(&state);
        cx.notify();
    }

    /// Warm the cache for the entries on either side of the current
    /// one so the next arrow press is usually instant. Silent — no
    /// task registry entries for speculative work.
    fn prefetch_neighbors(&mut self, cx: &mut Context<Self>) {
        let len = self.playlist.len();
        if len <= 1 {
            return;
        }
        for d in [1isize, -1] {
            let ix = (self.index as isize + d).rem_euclid(len as isize) as usize;
            if let Some(path) = self.playlist.get(ix).map(|e| e.path.clone()) {
                self.request_path(path, cx);
            }
        }
    }

    fn current_frame(&mut self) -> Option<Arc<ViewerFrame>> {
        let path = self.current()?.path.clone();
        match self.cache.get(&path) {
            Some(FrameState::Loaded(f)) => Some(f),
            _ => None,
        }
    }

    /// Keyboard/toolbar zoom anchors at the viewport center (wheel
    /// zoom, which has a real cursor, lands in iter 4).
    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        let Some(f) = self.current_frame() else { return };
        let img = (f.w as f32, f.h as f32);
        let view = self.last_stage_size;
        let center = (view.0 / 2.0, view.1 / 2.0);
        self.stage = stage::zoom_at(self.stage, center, img, view, factor);
        self.set_playing(false, cx);
        cx.notify();
    }

    /// Stage-local coordinates for a window-space event position (the
    /// stage sits directly under the toolbar, or at the window top in
    /// fullscreen; render keeps `stage_origin_y` current).
    fn stage_local(&self, position: Point<Pixels>) -> (f32, f32) {
        (position.x.as_f32(), position.y.as_f32() - self.stage_origin_y)
    }

    /// Wheel = zoom toward the cursor (image-viewer convention; the
    /// stage has nothing to scroll). Exponential mapping keeps trackpad
    /// and clicky-wheel speeds comparable.
    fn on_stage_scroll(
        &mut self,
        e: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(f) = self.current_frame() else { return };
        let dy = e.delta.pixel_delta(window.line_height()).y.as_f32();
        if dy == 0.0 {
            return;
        }
        let factor = 2.0_f32.powf(dy / 240.0);
        let cursor = self.stage_local(e.position);
        let img = (f.w as f32, f.h as f32);
        self.stage = stage::zoom_at(self.stage, cursor, img, self.last_stage_size, factor);
        // Zooming means the user is inspecting — auto-advancing under
        // them is exactly the "intrusive" behavior we're avoiding.
        self.set_playing(false, cx);
        cx.notify();
    }

    fn on_stage_mouse_down(
        &mut self,
        e: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if e.click_count >= 2 {
            self.toggle_actual_at(self.stage_local(e.position), cx);
            self.drag_last = None;
            return;
        }
        self.drag_last = Some(self.stage_local(e.position));
    }

    fn on_stage_mouse_move(
        &mut self,
        e: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Fullscreen chrome reveal: hovering the top strip shows the
        // toolbar. Pure position state — only notify on transitions.
        if window.is_fullscreen() {
            let want = e.position.y.as_f32() < CHROME_REVEAL_STRIP;
            if want != self.chrome_hover {
                self.chrome_hover = want;
                cx.notify();
            }
        }
        if e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(last) = self.drag_last else { return };
        let now = self.stage_local(e.position);
        let delta = (now.0 - last.0, now.1 - last.1);
        self.drag_last = Some(now);
        if let Some(f) = self.current_frame() {
            let img = (f.w as f32, f.h as f32);
            self.stage = stage::pan_by(self.stage, delta, img, self.last_stage_size);
            self.set_playing(false, cx);
            cx.notify();
        }
    }

    fn end_drag(&mut self) {
        self.drag_last = None;
    }

    /// Double-click: fit ↔ 1:1. Going to 1:1 centers the viewport on
    /// the clicked image point so "double-click the detail" inspects
    /// that detail.
    fn toggle_actual_at(&mut self, cursor: (f32, f32), cx: &mut Context<Self>) {
        let Some(f) = self.current_frame() else { return };
        if self.stage.mode == ZoomMode::Actual {
            self.stage = StageState::default();
        } else {
            let img = (f.w as f32, f.h as f32);
            let r = stage::layout(img, self.last_stage_size, self.stage);
            let frac = (
                ((cursor.0 - r.x) / r.w).clamp(0.0, 1.0),
                ((cursor.1 - r.y) / r.h).clamp(0.0, 1.0),
            );
            self.stage = StageState {
                mode: ZoomMode::Actual,
                center: frac,
            };
        }
        self.set_playing(false, cx);
        cx.notify();
    }

    // -- action handlers ------------------------------------------

    fn on_prev(&mut self, _: &ViewerPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn on_next(&mut self, _: &ViewerNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn on_toggle_play(&mut self, _: &ViewerTogglePlay, _window: &mut Window, cx: &mut Context<Self>) {
        let playing = self.playback.playing;
        self.set_playing(!playing, cx);
    }

    fn on_zoom_in(&mut self, _: &ViewerZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &ViewerZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0 / ZOOM_STEP, cx);
    }

    fn on_zoom_reset(&mut self, _: &ViewerZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        self.stage = StageState::default();
        cx.notify();
    }

    fn on_actual_size(
        &mut self,
        _: &ViewerActualSize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Toggle: 1:1 ↔ fit, keeping the pan center so the user stays
        // on the region they were inspecting.
        self.stage.mode = match self.stage.mode {
            ZoomMode::Actual => ZoomMode::FitDown,
            _ => ZoomMode::Actual,
        };
        cx.notify();
    }

    fn on_toggle_fullscreen(
        &mut self,
        _: &ViewerToggleFullscreen,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        window.toggle_fullscreen();
    }

    /// Esc — leave fullscreen first; a second Esc closes the window.
    fn on_dismiss(&mut self, _: &ViewerDismiss, window: &mut Window, _cx: &mut Context<Self>) {
        if window.is_fullscreen() {
            window.toggle_fullscreen();
        } else {
            window.remove_window();
        }
    }

    // -- render pieces --------------------------------------------

    fn toolbar(&mut self, cx: &mut Context<Self>) -> Div {
        let count = self.playlist.len();
        let counter = format!("{} / {}", (self.index + 1).min(count), count);
        let frame = self.current_frame();
        let zoom_label = frame
            .as_ref()
            .map(|f| {
                let s = stage::effective_scale(
                    self.stage.mode,
                    (f.w as f32, f.h as f32),
                    self.last_stage_size,
                );
                format!("{:.0}%", s * 100.0)
            })
            .unwrap_or_else(|| "\u{2014}".to_string());
        let actual = self.stage.mode == ZoomMode::Actual;
        let name = self.current().map(|e| e.name.clone()).unwrap_or_default();

        h_flex()
            .h(px(TOOLBAR_H))
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("viewer-prev")
                    .icon(gpui_component::Icon::empty().path("icons/chevron-left.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(56.0))
                    .text_center()
                    .child(counter),
            )
            .child(
                Button::new("viewer-next")
                    .icon(gpui_component::Icon::empty().path("icons/chevron-right.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
            )
            .child(
                Button::new("viewer-play")
                    .icon(gpui_component::Icon::empty().path(if self.playback.playing {
                        "icons/pause.svg"
                    } else {
                        "icons/play.svg"
                    }))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        let playing = this.playback.playing;
                        this.set_playing(!playing, cx);
                    })),
            )
            .child(
                Button::new("viewer-interval")
                    .label(Playback::interval_label(self.playback.interval_secs))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_interval(cx))),
            )
            .child(div().w(px(1.0)).h(px(20.0)).bg(cx.theme().border))
            .child(
                Button::new("viewer-zoom-out")
                    .icon(gpui_component::Icon::empty().path("icons/minus.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.0 / ZOOM_STEP, cx))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(44.0))
                    .text_center()
                    .child(zoom_label),
            )
            .child(
                Button::new("viewer-zoom-in")
                    .icon(gpui_component::Icon::empty().path("icons/plus.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(ZOOM_STEP, cx))),
            )
            .child(
                Button::new("viewer-actual")
                    .label(if actual { "Fit" } else { "1:1" })
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_actual_size(&ViewerActualSize, window, cx)
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_center()
                    .truncate()
                    .text_color(cx.theme().foreground)
                    .child(name),
            )
            .child(
                Button::new("viewer-fullscreen")
                    .icon(gpui_component::Icon::empty().path("icons/maximize.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_toggle_fullscreen(&ViewerToggleFullscreen, window, cx)
                    })),
            )
    }

    fn stage_area(&mut self, stage_w: f32, stage_h: f32, cx: &mut Context<Self>) -> Div {
        let path = self.current().map(|e| e.path.clone());
        let state = path.as_ref().and_then(|p| self.cache.get(p));
        let area = div()
            .relative()
            .overflow_hidden()
            .w_full()
            .h(px(stage_h))
            .bg(cx.theme().secondary.opacity(0.35))
            .on_scroll_wheel(cx.listener(Self::on_stage_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_stage_mouse_down))
            .on_mouse_move(cx.listener(Self::on_stage_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_drag()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_drag()),
            );
        match state {
            Some(FrameState::Loaded(f)) => {
                let r = stage::layout(
                    (f.w as f32, f.h as f32),
                    (stage_w, stage_h),
                    self.stage,
                );
                area.child(
                    gpui::img(f.image.clone())
                        .absolute()
                        .left(px(r.x))
                        .top(px(r.y))
                        .w(px(r.w))
                        .h(px(r.h)),
                )
            }
            Some(FrameState::Failed) => area.flex().items_center().justify_center().child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No preview available"),
            ),
            // Pending (or first paint before the request lands): show
            // the shared 512 px info-pane thumbnail as an instant
            // stand-in when one is cached, laid out with the same
            // stage state so the swap to full-res doesn't jump.
            _ => {
                let thumb = path
                    .as_ref()
                    .and_then(|p| crate::preview::loaded_image(
                        self.process.preview_cache.borrow().get(p),
                    ));
                match thumb {
                    Some(img) => {
                        let sz = img.size(0);
                        let dims = (sz.width.0 as f32, sz.height.0 as f32);
                        let r = stage::layout(dims, (stage_w, stage_h), self.stage);
                        area.child(
                            gpui::img(img)
                                .absolute()
                                .left(px(r.x))
                                .top(px(r.y))
                                .w(px(r.w))
                                .h(px(r.h)),
                        )
                    }
                    None => area.flex().items_center().justify_center().child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading\u{2026}"),
                    ),
                }
            }
        }
    }

    fn status_strip(&mut self, cx: &mut Context<Self>) -> Div {
        let dims = self
            .current_frame()
            .map(|f| format!("{}\u{00d7}{}", f.w, f.h))
            .unwrap_or_default();
        let pos = format!(
            "{} of {}",
            (self.index + 1).min(self.playlist.len()),
            self.playlist.len()
        );
        h_flex()
            .h(px(STATUS_H))
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .px_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(pos)
            .when(!dims.is_empty(), |this| this.child(dims))
            .when(self.playback.playing, |this| {
                this.child(format!(
                    "Slideshow \u{00b7} {}",
                    Playback::interval_label(self.playback.interval_secs)
                ))
            })
    }
}

/// gpui's window content NSView, for mounting the native video
/// overlay. `None` on non-AppKit platforms (the win32 overlay stub
/// ignores it anyway).
fn content_ns_view(window: &Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // UFCS: gpui::Window has an inherent `window_handle()` (the gpui
    // entity handle) that shadows the raw-window-handle trait method.
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
        _ => None,
    }
}

fn rect_f64(r: (f32, f32, f32, f32)) -> (f64, f64, f64, f64) {
    (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64)
}

impl Focusable for ViewerWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for ViewerWindow {
    /// Window close must stop playback — the overlay is an AppKit
    /// child of the (dying) window, but the AVPlayer would keep the
    /// audio session alive without an explicit pause.
    fn drop(&mut self) {
        self.teardown_video();
    }
}

impl Render for ViewerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_title(window);
        let fullscreen = window.is_fullscreen();
        if !fullscreen {
            self.chrome_hover = false;
        }
        let viewport = window.viewport_size();
        let chrome_h = if fullscreen { 0.0 } else { TOOLBAR_H + STATUS_H };
        let stage_w = viewport.width.as_f32();
        let stage_h = (viewport.height.as_f32() - chrome_h).max(100.0);
        self.last_stage_size = (stage_w, stage_h);
        self.stage_origin_y = if fullscreen { 0.0 } else { TOOLBAR_H };
        self.sync_video(window, (0.0, self.stage_origin_y, stage_w, stage_h));
        let stage_area = self.stage_area(stage_w, stage_h, cx);

        let root = v_flex()
            .key_context(VIEWER_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_actual_size))
            .on_action(cx.listener(Self::on_toggle_fullscreen))
            .on_action(cx.listener(Self::on_toggle_play))
            .on_action(cx.listener(Self::on_dismiss))
            .relative()
            .size_full()
            .bg(cx.theme().background);

        if fullscreen {
            // Image edge to edge; toolbar only as a hover overlay at
            // the top (no timers — pure mouse-position state).
            let chrome = self.chrome_hover.then(|| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bg(cx.theme().background.opacity(0.92))
                    .child(self.toolbar(cx))
            });
            root.child(stage_area).when_some(chrome, Div::child)
        } else {
            let toolbar = self.toolbar(cx);
            let status = self.status_strip(cx);
            root.child(toolbar).child(stage_area).child(status)
        }
    }
}
