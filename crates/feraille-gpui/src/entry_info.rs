//! Get Info popup: the cross-crate composition that builds a
//! [`feraille_core::entry_info::EntryInfo`] for one path, plus the modal
//! view that renders it.
//!
//! Composition lives here (not in a domain crate) because it is the one
//! place that legitimately touches every layer at once: POSIX stat
//! (`feraille-fs-native`), AppKit resource values (`feraille-shell-mac`),
//! volume info, magic, and tags. The gather runs on the background executor
//! — never the paint path — and the result is a fully-formatted, neutral
//! record the view paints without any further I/O.
//!
//! The popup is a gpui-component `Dialog` hosting this view, so ESC /
//! overlay-click / focus-trap come for free (same primitive as the About
//! box). Editing is layered on top in a later pass; today the panel reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use feraille_core::commands::TagColor;
use feraille_core::entry_info::{
    Attr, EntryInfo, InfoSection, InfoTarget, InfoValue, PermBits, PermMatrix, SizeValue,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::Button, checkbox::Checkbox, dialog::Dialog, h_flex, v_flex, ActiveTheme, Sizable,
    WindowExt as _,
};

use crate::file_list::tag_color_rgba;

/// Open the Get Info popup for `path`. `name`/`target` are the caller's
/// best guess (from the selected row) used for the loading header; the
/// background gather recomputes them authoritatively.
pub fn open(path: PathBuf, name: String, target: InfoTarget, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| EntryInfoView::new(path, name, target, cx));
    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title("Get Info")
            .w(px(380.0))
            .overlay_closable(true)
            .keyboard(true)
            .close_button(true)
            .child(view.clone())
    });
}

/// Classify a path for the size/volume behavior. Touches the filesystem
/// (`is_dir`) — only ever called on the gather worker.
fn classify(path: &Path) -> InfoTarget {
    if path == Path::new("/") || path.parent() == Some(Path::new("/Volumes")) {
        InfoTarget::Volume
    } else if path.is_dir() {
        InfoTarget::Folder
    } else {
        InfoTarget::File
    }
}

/// Display name: volume's localized name when it's a volume, else the file
/// name, else the path itself.
fn display_name(path: &Path, target: InfoTarget, vol_name: Option<&str>) -> String {
    if target == InfoTarget::Volume {
        if let Some(n) = vol_name {
            return n.to_string();
        }
    }
    path.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

/// Build the full Get Info record. Runs on the background executor: every
/// call here is a native read, none of it is allowed on the paint path.
pub fn gather(path: &Path) -> EntryInfo {
    use feraille_fs_native as fsn;
    use feraille_shell_mac as shell;

    let target = classify(path);
    let stat = fsn::stat_info::read_stat_info(path);
    let sh = shell::read_shell_info(path);
    let vol = fsn::volume_info_for_path(path);
    let colors = shell::read_canonical_tags(path);
    let default_app = shell::open_with_candidates(path)
        .into_iter()
        .find(|c| c.is_default)
        .map(|c| c.name);

    let name = display_name(path, target, vol.as_ref().map(|v| v.name.as_str()));
    let kind = sh
        .kind
        .clone()
        .or_else(|| fsn::detect_magic(path).map(str::to_string))
        .unwrap_or_else(|| match target {
            InfoTarget::Volume => "Volume".into(),
            InfoTarget::Folder => "Folder".into(),
            InfoTarget::File => "Document".into(),
        });

    let fmt_date = fsn::stat_info::format_local_datetime;

    // ---- General ----
    let mut general = InfoSection::new("General").text_if("Kind", kind.clone());
    if let Some(uti) = sh.uti.clone() {
        general = general.text_if("Type", uti);
    }
    match target {
        InfoTarget::File => {
            let bytes = stat.as_ref().map(|s| s.size).unwrap_or(0);
            general = general.row(
                "Size",
                InfoValue::Size(SizeValue::Known {
                    bytes,
                    display: fsn::humanize_bytes(bytes),
                }),
            );
        }
        InfoTarget::Folder => {
            general = general.row("Size", InfoValue::Size(SizeValue::Calculable));
        }
        InfoTarget::Volume => {}
    }
    if let Some(s) = &stat {
        if let Some(c) = s.created_unix {
            general = general.text_if("Created", fmt_date(c));
        }
        general = general.text_if("Modified", fmt_date(s.modified_unix));
        if let Some(a) = s.accessed_unix {
            general = general.text_if("Last opened", fmt_date(a));
        }
    }
    if let Some(added) = sh.added_unix {
        general = general.text_if("Added", fmt_date(added));
    }
    if let Some(app) = default_app {
        general = general.text_if("Application", app);
    }
    general = general.text_if("Where", path.display().to_string());

    // ---- Attributes ----
    let mut attributes = InfoSection::new("Attributes");
    if let Some(s) = &stat {
        attributes = attributes
            .row(
                Attr::Locked.label(),
                InfoValue::Toggle {
                    on: s.is_locked,
                    attr: Attr::Locked,
                },
            )
            .row(
                Attr::Invisible.label(),
                InfoValue::Toggle {
                    on: s.is_invisible,
                    attr: Attr::Invisible,
                },
            );
    }
    if let Some(he) = sh.hidden_extension {
        attributes = attributes.row(
            Attr::HiddenExtension.label(),
            InfoValue::Toggle {
                on: he,
                attr: Attr::HiddenExtension,
            },
        );
    }
    attributes = attributes.row(
        "Tags",
        InfoValue::Tags {
            colors,
            custom: Vec::new(),
        },
    );

    // ---- Ownership & Permissions ----
    let mut permissions = InfoSection::new("Ownership & Permissions");
    if let Some(s) = &stat {
        let mode = s.mode & 0o7777;
        permissions = permissions
            .text_if("Owner", s.owner_name.clone())
            .text_if("Group", s.group_name.clone())
            .row(
                "Permissions",
                InfoValue::Permissions(PermMatrix {
                    owner_name: s.owner_name.clone(),
                    group_name: s.group_name.clone(),
                    owner: PermBits::from_triple((mode >> 6) & 0b111),
                    group: PermBits::from_triple((mode >> 3) & 0b111),
                    other: PermBits::from_triple(mode & 0b111),
                    kind_char: s.kind_char(),
                    raw_mode: mode,
                }),
            );
    }

    // ---- Volume ----
    let mut volume = InfoSection::new("Volume");
    if let Some(v) = &vol {
        volume = volume.text_if("Volume", v.name.clone());
        if let Some(t) = v.total_bytes {
            volume = volume.text_if("Capacity", fsn::humanize_bytes(t));
        }
        if let Some(a) = v.available_bytes {
            volume = volume.text_if("Available", fsn::humanize_bytes(a));
        }
        if let Some(f) = v.format.clone() {
            volume = volume.text_if("Format", f);
        }
        volume = volume.text_if("Mount point", v.path.display().to_string());
        if let Some(d) = v.bsd_device.clone() {
            volume = volume.text_if("Device", d);
        }
    }

    let sections = [general, attributes, permissions, volume]
        .into_iter()
        .filter(|s| !s.rows.is_empty())
        .collect();

    EntryInfo {
        name,
        kind,
        target,
        sections,
    }
}

enum GatherState {
    Loading,
    Ready(EntryInfo),
}

/// The modal's content view. Owns the target path and the gathered record;
/// gathers on construction and re-renders when it lands.
pub struct EntryInfoView {
    path: PathBuf,
    name: String,
    kind: String,
    state: GatherState,
    /// Cancel flag for an in-flight recursive "Calculate".
    size_cancel: Option<Arc<AtomicBool>>,
}

impl EntryInfoView {
    fn new(path: PathBuf, name: String, _target: InfoTarget, cx: &mut Context<Self>) -> Self {
        let gather_path = path.clone();
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async move { gather(&gather_path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.name = info.name.clone();
                this.kind = info.kind.clone();
                this.state = GatherState::Ready(info);
                cx.notify();
            });
        })
        .detach();

        Self {
            path,
            name,
            kind: String::new(),
            state: GatherState::Loading,
            size_cancel: None,
        }
    }

    /// Kick a recursive size scan for a folder/volume's "Calculate" button.
    /// Read-only work (no mutation) — streams the total back into the open
    /// record when it finishes.
    fn calculate_size(&mut self, cx: &mut Context<Self>) {
        if let GatherState::Ready(info) = &mut self.state {
            info.set_size_value(SizeValue::Calculating);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.size_cancel = Some(cancel.clone());
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let bytes = cx
                .background_executor()
                .spawn(async move { feraille_fs_native::recursive_size(&path, &cancel) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let GatherState::Ready(info) = &mut this.state {
                    info.set_size_value(SizeValue::Known {
                        bytes,
                        display: feraille_fs_native::humanize_bytes(bytes),
                    });
                }
                this.size_cancel = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Drop for EntryInfoView {
    fn drop(&mut self) {
        // If a recursive size scan is still running when the popup closes,
        // tell it to stop — its result has nowhere to land.
        if let Some(c) = &self.size_cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Render for EntryInfoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let header = v_flex()
            .gap_0p5()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(self.name.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(self.kind.clone()),
            );

        let body = match &self.state {
            GatherState::Loading => v_flex().child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("Gathering details\u{2026}"),
            ),
            GatherState::Ready(info) => {
                let mut col = v_flex().gap_3();
                let mut row_ix = 0usize;
                for section in &info.sections {
                    col = col.child(self.render_section(section, &mut row_ix, muted, cx));
                }
                col
            }
        };

        v_flex().gap_3().child(header).child(body)
    }
}

impl EntryInfoView {
    fn render_section(
        &self,
        section: &InfoSection,
        row_ix: &mut usize,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut block = v_flex().gap_1().child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(section.title.clone()),
        );
        for row in &section.rows {
            let ix = *row_ix;
            *row_ix += 1;
            block = block.child(self.render_row(&row.label, &row.value, ix, muted, cx));
        }
        block
    }

    fn render_row(
        &self,
        label: &str,
        value: &InfoValue,
        ix: usize,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> Div {
        h_flex()
            .gap_2()
            .items_start()
            .child(
                div()
                    .w(px(96.0))
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .text_right()
                    .child(label.to_string()),
            )
            .child(div().flex_1().text_xs().child(self.render_value(value, ix, cx)))
    }

    fn render_value(&self, value: &InfoValue, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        match value {
            InfoValue::Text(s) | InfoValue::Name(s) => {
                div().child(s.clone()).into_any_element()
            }
            InfoValue::Toggle { on, .. } => {
                // Read-only in this layer: the checkbox shows state; the
                // edit wiring (write-back + undo) lands in the next pass.
                Checkbox::new(ElementId::Name(format!("entry-info-tog-{ix}").into()))
                    .checked(*on)
                    .into_any_element()
            }
            InfoValue::Tags { colors, custom } => {
                let mut row = h_flex().gap_1p5().items_center();
                if colors.is_empty() && custom.is_empty() {
                    return div()
                        .text_color(cx.theme().muted_foreground)
                        .child("None")
                        .into_any_element();
                }
                for c in colors {
                    let rgba = tag_color_rgba(*c);
                    row = row.child(
                        div()
                            .w(px(12.0))
                            .h(px(12.0))
                            .rounded_full()
                            .bg(rgba)
                            .border_1()
                            .border_color(cx.theme().border),
                    );
                }
                for name in custom {
                    row = row.child(div().child(name.clone()));
                }
                row.into_any_element()
            }
            InfoValue::Permissions(m) => div().child(m.symbolic()).into_any_element(),
            InfoValue::Size(size) => match size {
                SizeValue::Known { display, .. } => {
                    div().child(display.clone()).into_any_element()
                }
                SizeValue::Calculating => div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Calculating\u{2026}")
                    .into_any_element(),
                SizeValue::Calculable => Button::new("entry-info-calc-size")
                    .label("Calculate")
                    .xsmall()
                    .on_click(cx.listener(|this, _, _window, cx| this.calculate_size(cx)))
                    .into_any_element(),
            },
        }
    }
}
