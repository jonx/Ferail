//! Dedicated grouped duplicate panel (docs/features/DUPLICATES.md).
//!
//! [`crate::feature_settings::DupePresentation::Panel`] renders confirmed
//! duplicate groups as collapsible cards instead of adjacent table rows,
//! and adds the group-level cleanup actions the grouped-rows view can't
//! express: keep-newest, keep-this (select all but one), and trash the
//! marked set. The backing model is `Tab::dupe_groups` ([`DupeGroupView`])
//! and selection rides the tab's existing `selection` set, so the trash
//! flow and node store are shared rather than duplicated.
//!
//! Prime directive: the render path reads the retained model only — no
//! I/O, no settings reads (presentation is cached on the tab's tool result
//! surface at scan launch). The destructive action runs the same off-thread
//! trash worker as `on_move_to_trash`, then prunes the model and rebuilds the
//! card list from what survived.

// Windows clippy reports `TextScale` (and `ButtonVariants` below) unused:
// the dupe body's styling calls resolve through traits already in scope
// there. Keep the imports for the other platforms, silence that one leg.
#[cfg_attr(target_os = "windows", allow(unused_imports))]
use crate::text::{IconScale as _, TextScale as _};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
};

use ferail_core::NodeId;
use ferail_fs_native::DupeMode;
#[cfg_attr(target_os = "windows", allow(unused_imports))]
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;

use super::dupes::{SimilarCriterion, location_with_note, similar_criterion_range};
use super::tab::DupeGroupView;
use super::*;

impl Shell {
    /// Card-based duplicate panel for the active tab. Caller guarantees the
    /// active tool result is Duplicates with `presentation == Panel`.
    pub(super) fn dupe_panel_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let tab = self.active_tab();
        let Some(dm) = tab
            .tool_result
            .as_ref()
            .and_then(|surface| surface.dupe_mode())
        else {
            return div().into_any_element();
        };
        let selection = &tab.selection;
        let selected = selection.len();
        let scanning = tab.load_cancel.is_some();
        let progress_label = if scanning && dm.mode == DupeMode::Similar {
            Some(match dm.progress.phase {
                ferail_fs_native::DupePhase::Enumerating => tr!(
                    "Enumerating folders\u{2026} {folders} folders scanned \u{00B7} {images} candidate images found",
                    folders = dm.progress.folders_scanned,
                    images = dm.progress.images_discovered
                ),
                ferail_fs_native::DupePhase::Analyzing => tr!(
                    "Analyzing images\u{2026} {done} of {total}",
                    done = dm.progress.images_analyzed,
                    total = dm.progress.images_total
                ),
                ferail_fs_native::DupePhase::Grouping => {
                    tr!("Grouping similar images\u{2026}")
                }
            })
        } else if scanning {
            Some(tr!("scanning\u{2026}"))
        } else {
            None
        };

        // Toolbar: a running summary plus the global actions. "Reclaimable"
        // is the whole-scan figure; the selected count tells the user how
        // much the Trash button will act on.
        let summary = trn!(
            "{n} group \u{00B7} {reclaimable} reclaimable",
            "{n} groups \u{00B7} {reclaimable} reclaimable",
            dm.groups,
            reclaimable = ferail_fs_native::humanize_bytes(dm.wasted_bytes)
        );
        let toolbar = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_scale_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(summary),
                    )
                    .children(progress_label.map(|label| {
                        div()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(label)
                    })),
            )
            .child(div().flex_1())
            .when(dm.mode == DupeMode::Exact, |this| {
                this.child(
                    Button::new("dupe-keep-newest-all")
                        .small()
                        .ghost()
                        .label(tr!("Keep newest everywhere"))
                        .tooltip(tr!("Mark every copy except the most recent in each group"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.dupe_stage_keep_newest_all(cx)),
                        ),
                )
            })
            .child(
                Button::new("dupe-clear")
                    .small()
                    .ghost()
                    .label(tr!("Clear"))
                    .disabled(selected == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.dupe_clear_marks(cx))),
            )
            .child(
                Button::new("dupe-trash-selected")
                    .small()
                    .danger()
                    .label(if selected == 0 {
                        tr!("Trash marked")
                    } else {
                        tr!("Trash {n} marked", n = selected)
                    })
                    .disabled(selected == 0)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.dupe_trash_marked(window, cx)),
                    ),
            );

        let criteria = (dm.mode == DupeMode::Similar).then(|| {
            let values = dm.similarity_criteria;
            let recommended = ferail_fs_native::perceptual::SimilarityCriteria::RECOMMENDED;
            let disabled = scanning || tab.similar_image_index.is_empty();

            v_flex()
                .w_full()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_scale_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(tr!("Similarity criteria")),
                        )
                        .child(
                            div()
                                .text_scale_xs()
                                .text_color(theme.muted_foreground)
                                .child(tr!("Lower values require a closer visual match.")),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("similar-criteria-reset")
                                .xsmall()
                                .ghost()
                                .label(tr!("Reset to recommended"))
                                .disabled(scanning || values == recommended)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reset_similar_criteria(cx)
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_4()
                        .child(
                            h_flex()
                                .flex_1()
                                .gap_2()
                                .child(
                                    self.similar_criteria_track(
                                        SimilarCriterion::Structure,
                                        values.structure,
                                        disabled,
                                        cx,
                                    ),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .w(px(112.0))
                                        .text_scale_xs()
                                        .text_color(theme.foreground)
                                        .child(tr!(
                                            "Structure ≤ {value}",
                                            value = values.structure
                                        )),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .gap_2()
                                .child(self.similar_criteria_track(
                                    SimilarCriterion::Detail,
                                    values.detail,
                                    disabled,
                                    cx,
                                ))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .w(px(92.0))
                                        .text_scale_xs()
                                        .text_color(theme.foreground)
                                        .child(tr!("Detail ≤ {value}", value = values.detail)),
                                ),
                        ),
                )
        });

        let list = if tab.dupe_groups.is_empty() {
            div()
                .id("dupe-panel-empty")
                .flex_1()
                .child(
                    v_flex()
                        .gap_1()
                        .p_8()
                        .child(
                            div()
                                .text_scale_sm()
                                .text_color(theme.muted_foreground)
                                .child(if scanning && dm.mode == DupeMode::Similar {
                                    tr!("Scanning for similar images\u{2026}")
                                } else if scanning {
                                    tr!("Scanning for duplicates\u{2026}")
                                } else if dm.mode == DupeMode::Similar {
                                    tr!("No similar images found.")
                                } else {
                                    tr!("No duplicates found.")
                                }),
                        )
                        .when(dm.mode == DupeMode::Similar, |this| {
                            this.child(
                                div()
                                    .text_scale_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(tr!(
                                        "Supported image formats: JPEG, PNG, GIF, WebP, BMP, and TIFF."
                                    )),
                            )
                        }),
                )
                .into_any_element()
        } else {
            let view = cx.entity().clone();
            let scroll = tab.dupe_panel_scroll.clone();
            let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
                tab.dupe_groups
                    .iter()
                    .map(dupe_group_card_estimated_size)
                    .collect(),
            );
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    crate::multi_table::v_virtual_list(
                        view,
                        "dupe-panel-scroll",
                        item_sizes,
                        move |this, visible_range: Range<usize>, _window, cx| {
                            let tab = this.active_tab();
                            let root = tab
                                .tool_result
                                .as_ref()
                                .and_then(|surface| surface.dupe_mode())
                                .map(|dm| dm.root.clone())
                                .unwrap_or_default();
                            let selection = tab.selection.clone();
                            let groups: Vec<DupeGroupView> = visible_range
                                .filter_map(|ix| tab.dupe_groups.get(ix).cloned())
                                .collect();

                            groups
                                .into_iter()
                                .map(|group| this.dupe_group_card(&group, &root, &selection, cx))
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(&scroll)
                    .flex_1()
                    .size_full()
                    .p_2()
                    .gap_2()
                    .with_sizing_behavior(ListSizingBehavior::Auto),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.0))
                        .child(Scrollbar::vertical(&scroll)),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(toolbar)
            .children(criteria)
            .child(list)
            .into_any_element()
    }

    fn similar_criteria_track(
        &self,
        criterion: SimilarCriterion,
        value: u32,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let range = similar_criterion_range(criterion);
        let entity = cx.entity();
        let (id, tooltip) = match criterion {
            SimilarCriterion::Structure => (
                "similar-structure-slider",
                tr!("Controls image layout and edges (dHash)."),
            ),
            SimilarCriterion::Detail => (
                "similar-detail-slider",
                tr!("Controls overall visual detail (pHash)."),
            ),
        };

        crate::scrub_slider::track(id, range.fraction(value as f32), disabled, cx)
            .flex_1()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| match criterion {
                            SimilarCriterion::Structure => this.similar_structure_track = bounds,
                            SimilarCriterion::Detail => this.similar_detail_track = bounds,
                        })
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .when(!disabled, |this| {
                this.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.begin_similar_criteria_drag(criterion, event.position.x, cx);
                    }),
                )
            })
    }

    /// One collapsible group card.
    fn dupe_group_card(
        &self,
        group: &DupeGroupView,
        root: &Path,
        selection: &std::collections::HashSet<NodeId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let group_no = group.group_no;
        let copies = group.members.len();
        let reclaimable = group.reclaimable_bytes();

        // macOS/APFS zero-copy remediation: replace the redundant copies
        // with clones of the keeper — frees the bytes without deleting any
        // file. Hidden off macOS; surfaces a toast if the volume isn't
        // APFS (clonefile errors there).
        let dedup_btn = if group.mode == DupeMode::Exact
            && cfg!(target_os = "macos")
            && group.distinct_occupants() > 1
        {
            Some(
                Button::new(ElementId::Name(format!("dupe-clone-{group_no}").into()))
                    .xsmall()
                    .ghost()
                    .label(tr!("Dedup \u{2192} clones"))
                    .tooltip(tr!(
                        "Replace extra copies with APFS clones (keeps every file)"
                    ))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dupe_dedup_group(group_no, window, cx)
                    })),
            )
        } else {
            None
        };

        let group_summary = if group.mode == DupeMode::Similar {
            trn!(
                "{n} image · {reclaimable} reclaimable",
                "{n} images · {reclaimable} reclaimable",
                copies,
                reclaimable = ferail_fs_native::humanize_bytes(reclaimable)
            )
        } else {
            trn!(
                "{n} copy · {each} each · {reclaimable} reclaimable",
                "{n} copies · {each} each · {reclaimable} reclaimable",
                copies,
                each = ferail_fs_native::humanize_bytes(group.bytes_each),
                reclaimable = ferail_fs_native::humanize_bytes(reclaimable)
            )
        };

        let header = h_flex()
            .id(ElementId::Name(format!("dupe-card-{group_no}").into()))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .cursor_pointer()
            .hover(|this| this.bg(theme.secondary))
            .on_click(cx.listener(move |this, _, _, cx| this.dupe_toggle_group(group_no, cx)))
            // SVG disclosure (same asset as the sidebar tree), not the
            // ▾/▸ text glyphs — the AROS-bundled font lacks them and
            // drew tofu boxes.
            .child(
                div().w(px(14.0)).flex().items_center().child(
                    gpui::svg()
                        .path(if group.expanded {
                            "icons/nav/disclosure-down.svg"
                        } else {
                            "icons/nav/disclosure-right.svg"
                        })
                        .icon_px(9.0)
                        .text_color(theme.muted_foreground),
                ),
            )
            .child(
                div()
                    .text_scale_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(format!("#{group_no}")),
            )
            .child(
                div()
                    .text_scale_xs()
                    .text_color(theme.muted_foreground)
                    .child(group_summary),
            )
            .child(div().flex_1())
            .when(group.mode == DupeMode::Exact, |this| {
                this.child(
                    Button::new(ElementId::Name(format!("dupe-newest-{group_no}").into()))
                        .xsmall()
                        .ghost()
                        .label(tr!("Keep newest"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dupe_stage_keep_newest(group_no, cx)
                        })),
                )
            })
            .child(
                Button::new(ElementId::Name(format!("dupe-allbutone-{group_no}").into()))
                    .xsmall()
                    .ghost()
                    .label(if group.mode == DupeMode::Similar {
                        tr!("Mark others")
                    } else {
                        tr!("All but one")
                    })
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.dupe_stage_all_but_one(group_no, cx)
                        }),
                    ),
            )
            .children(dedup_btn);

        let mut card = v_flex()
            .w_full()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(header);

        if group.expanded {
            let mut body = v_flex().w_full().px_2().pb_1();
            for member in &group.members {
                let node = member.node;
                let can_view_group = group.mode == DupeMode::Similar;
                let focused = self.active_tab().dupe_panel_focus == Some((group_no, node));
                let is_similar_keeper =
                    group.mode == DupeMode::Similar && group.keeper == Some(node);
                let marked = selection.contains(&node) && !is_similar_keeper;
                let is_keeper = group.keeper == Some(node);
                let name = member
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let location = member_location(&member.path, root);
                let location = location_with_note(&location, member.is_hardlink, member.is_clone);
                let thumbnail = member.image.as_ref().and_then(|image| {
                    image.thumbnail.as_ref().map(|thumbnail| {
                        div()
                            .flex_shrink_0()
                            .w(px(64.0))
                            .h(px(64.0))
                            .rounded(theme.radius)
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .child(
                                img(thumbnail.clone())
                                    .size_full()
                                    .object_fit(gpui::ObjectFit::Contain),
                            )
                    })
                });
                let image_detail = member.image.as_ref().map(|image| {
                    let similarity = similarity_summary(image.dhash_distance, image.phash_distance);
                    let tooltip = tr!(
                        "Compared with the group reference: dHash {structure}/64 (structure), pHash {detail}/64 (detail). Lower numbers mean greater similarity.",
                        structure = image.dhash_distance,
                        detail = image.phash_distance
                    );
                    div()
                        .id(ElementId::Name(
                            format!("dupe-similarity-{}", node.as_raw()).into(),
                        ))
                        .text_scale_xs()
                        .text_color(theme.muted_foreground)
                        .child(tr!(
                            "{width} × {height} · {similarity}",
                            width = image.width,
                            height = image.height,
                            similarity = similarity
                        ))
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tooltip.clone())
                                .build(window, cx)
                        })
                });
                let best_badge = member
                    .image
                    .as_ref()
                    .is_some_and(|image| image.is_best)
                    .then(|| {
                        div()
                            .flex_shrink_0()
                            .rounded(px(3.0))
                            .px_1()
                            .py_0p5()
                            .bg(theme.secondary)
                            .text_scale_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.primary)
                            .child(tr!("Best copy"))
                    });

                // Marked-for-trash checkbox.
                let check = div()
                    .id(ElementId::Name(
                        format!("dupe-mark-{}", node.as_raw()).into(),
                    ))
                    .flex_shrink_0()
                    .w(px(15.0))
                    .h(px(15.0))
                    .rounded(px(2.0))
                    .border_1()
                    .border_color(if marked { theme.danger } else { theme.border })
                    .when(marked, |this| this.bg(theme.danger))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(marked, |this| {
                        this.child(
                            div()
                                .text_scale_xs()
                                .text_color(gpui::white())
                                .child("\u{2713}"),
                        )
                    })
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| this.dupe_toggle_mark(node, cx)));

                // Keep-this radio.
                let radio =
                    div()
                        .id(ElementId::Name(
                            format!("dupe-keep-{}", node.as_raw()).into(),
                        ))
                        .flex_shrink_0()
                        .w(px(15.0))
                        .h(px(15.0))
                        .rounded_full()
                        .border_1()
                        .border_color(if is_keeper {
                            theme.primary
                        } else {
                            theme.border
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .when(is_keeper, |this| {
                            this.child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(theme.primary))
                        })
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dupe_pick_keeper(group_no, node, cx)
                        }));

                let shares = member.shares_storage();
                let row = h_flex()
                    .id(ElementId::Name(
                        format!("dupe-member-{group_no}-{}", node.as_raw()).into(),
                    ))
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .px_1()
                    .rounded(theme.radius)
                    .when(focused, |this| this.bg(theme.secondary))
                    .when(can_view_group, |this| {
                        this.cursor_pointer().on_click(cx.listener(
                            move |this, event: &gpui::ClickEvent, window, cx| {
                                window.focus(&this.focus_handle, cx);
                                this.active_tab_mut().dupe_panel_focus = Some((group_no, node));
                                if event.click_count() >= 2 {
                                    this.dupe_open_similar_group_viewer(group_no, node, window, cx);
                                }
                                cx.notify();
                            },
                        ))
                    })
                    .child(check)
                    .child(radio)
                    .children(thumbnail)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_scale_sm()
                                            .text_color(if shares {
                                                theme.muted_foreground
                                            } else {
                                                theme.foreground
                                            })
                                            .child(name),
                                    )
                                    .children(best_badge),
                            )
                            .child(
                                div()
                                    .max_w(px(520.0))
                                    .truncate()
                                    .text_scale_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(super::loading::middle_truncate_path(&location, 58)),
                            )
                            .children(image_detail),
                    );
                body = body.child(row);
            }
            card = card.child(body);
        }

        card.into_any_element()
    }

    /// Open one Similar Images group as an explicit, in-memory viewer
    /// playlist. The surrounding folder is deliberately excluded: arrow-key
    /// navigation stays inside the candidate series and the paths live only
    /// as long as the result surface / viewer window.
    fn dupe_open_similar_group_viewer(
        &self,
        group_no: usize,
        start_node: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(group) = self
            .active_tab()
            .dupe_groups
            .iter()
            .find(|group| group.group_no == group_no && group.mode == DupeMode::Similar)
        else {
            return false;
        };

        let mut start = 0;
        let playlist = group
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| {
                if member.node == start_node {
                    start = index;
                }
                crate::viewer::PlaylistEntry {
                    name: member
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: member.path.clone(),
                }
            })
            .collect::<Vec<_>>();
        if playlist.is_empty() {
            return false;
        }
        crate::viewer::open_viewer(playlist, start, window, cx);
        true
    }

    /// Spacebar entry point. Returns true only while a Similar Images panel
    /// can open a group, letting the ordinary Quick Look handler run on every
    /// other surface. Before the first click, use the first group's keeper
    /// (normally the ranked best copy) as a useful default.
    pub(super) fn dupe_open_focused_similar_group_viewer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let tab = self.active_tab();
        let is_similar_panel = tab
            .tool_result
            .as_ref()
            .and_then(|surface| surface.dupe_mode())
            .is_some_and(|mode| {
                mode.mode == DupeMode::Similar
                    && mode.presentation == crate::feature_settings::DupePresentation::Panel
            });
        if !is_similar_panel {
            return false;
        }

        let target = tab
            .dupe_panel_focus
            .filter(|(group_no, node)| {
                tab.dupe_groups.iter().any(|group| {
                    group.group_no == *group_no
                        && group.members.iter().any(|member| member.node == *node)
                })
            })
            .or_else(|| {
                tab.dupe_groups.first().and_then(|group| {
                    group
                        .keeper
                        .or_else(|| group.best())
                        .or_else(|| group.members.first().map(|member| member.node))
                        .map(|node| (group.group_no, node))
                })
            });
        let Some((group_no, node)) = target else {
            return false;
        };
        self.active_tab_mut().dupe_panel_focus = Some((group_no, node));
        let opened = self.dupe_open_similar_group_viewer(group_no, node, window, cx);
        if opened {
            cx.notify();
        }
        opened
    }

    /// Move the keyboard focus by one member in the Similar Images panel.
    /// This deliberately changes only presentation focus: the panel's
    /// `selection` remains the marked-for-trash set and arrow navigation must
    /// never stage a destructive action. Crossing a group boundary expands
    /// the destination card and asks the virtual list to reveal it.
    pub(super) fn dupe_move_similar_focus(
        &mut self,
        forward: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let tab = self.active_tab();
        let is_similar_panel = tab
            .tool_result
            .as_ref()
            .and_then(|surface| surface.dupe_mode())
            .is_some_and(|mode| {
                mode.mode == DupeMode::Similar
                    && mode.presentation == crate::feature_settings::DupePresentation::Panel
            });
        if !is_similar_panel {
            return false;
        }

        let members = tab
            .dupe_groups
            .iter()
            .enumerate()
            .flat_map(|(group_index, group)| {
                group
                    .members
                    .iter()
                    .map(move |member| (group_index, group.group_no, member.node))
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            return true;
        }

        let current = tab.dupe_panel_focus.and_then(|focus| {
            members
                .iter()
                .position(|(_, group_no, node)| (*group_no, *node) == focus)
        });
        let Some(next) = stepped_focus_index(members.len(), current, forward) else {
            return true;
        };
        let (group_index, group_no, node) = members[next];

        let tab = self.active_tab_mut();
        if let Some(group) = tab
            .dupe_groups
            .iter_mut()
            .find(|group| group.group_no == group_no)
        {
            group.expanded = true;
        }
        tab.dupe_panel_focus = Some((group_no, node));
        tab.dupe_panel_scroll
            .scroll_to_item(group_index, gpui::ScrollStrategy::Center);
        cx.notify();
        true
    }

    // ===== Group actions =====

    fn dupe_group_mut(groups: &mut [DupeGroupView], group_no: usize) -> Option<&mut DupeGroupView> {
        groups.iter_mut().find(|g| g.group_no == group_no)
    }

    /// Expand / collapse a single card.
    fn dupe_toggle_group(&mut self, group_no: usize, cx: &mut Context<Self>) {
        if let Some(g) = Self::dupe_group_mut(&mut self.active_tab_mut().dupe_groups, group_no) {
            g.expanded = !g.expanded;
            cx.notify();
        }
    }

    /// Pick a keeper (the "keep this" radio) and mark every other member
    /// of that group for trashing.
    fn dupe_pick_keeper(&mut self, group_no: usize, keeper: NodeId, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        let (members, victims) = {
            let Some(group) = Self::dupe_group_mut(&mut tab.dupe_groups, group_no) else {
                return;
            };
            if !group.members.iter().any(|member| member.node == keeper) {
                return;
            }
            group.keeper = Some(keeper);
            (
                group
                    .members
                    .iter()
                    .map(|member| member.node)
                    .collect::<Vec<_>>(),
                group.victims_for_keeper(keeper),
            )
        };
        for member in members {
            tab.selection.remove(&member);
        }
        tab.selection.extend(victims);
        if let Some(dm) = tab
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        {
            dm.wasted_bytes = tab
                .dupe_groups
                .iter()
                .map(DupeGroupView::reclaimable_bytes)
                .sum();
        }
        cx.notify();
    }

    /// Toggle one member in/out of the marked-for-trash set.
    fn dupe_toggle_mark(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let is_protected_keeper = self
            .active_tab()
            .dupe_groups
            .iter()
            .any(|group| group.mode == DupeMode::Similar && group.keeper == Some(node));
        if is_protected_keeper {
            return;
        }
        let sel = &mut self.active_tab_mut().selection;
        if !sel.remove(&node) {
            sel.insert(node);
        }
        cx.notify();
    }

    /// Mark this group's all-but-newest for trashing.
    fn dupe_stage_keep_newest(&mut self, group_no: usize, cx: &mut Context<Self>) {
        let staged =
            Self::dupe_group_mut(&mut self.active_tab_mut().dupe_groups, group_no).map(|group| {
                let keeper = group.newest();
                group.keeper = keeper;
                (
                    group
                        .members
                        .iter()
                        .map(|member| member.node)
                        .collect::<Vec<_>>(),
                    group.victims_keep_newest(),
                )
            });
        if let Some((members, victims)) = staged {
            self.dupe_replace_group_marks(&members, &victims, cx);
        }
    }

    /// Mark this group's all-but-keeper (defaults to the first member).
    fn dupe_stage_all_but_one(&mut self, group_no: usize, cx: &mut Context<Self>) {
        let staged =
            Self::dupe_group_mut(&mut self.active_tab_mut().dupe_groups, group_no).map(|group| {
                let members = group
                    .members
                    .iter()
                    .map(|member| member.node)
                    .collect::<Vec<_>>();
                let victims = group.victims_all_but_one();
                (members, victims)
            });
        if let Some((members, victims)) = staged {
            self.dupe_replace_group_marks(&members, &victims, cx);
        }
    }

    /// Global "keep newest everywhere": union of every group's
    /// all-but-newest.
    fn dupe_stage_keep_newest_all(&mut self, cx: &mut Context<Self>) {
        if self
            .active_tab()
            .tool_result
            .as_ref()
            .and_then(|surface| surface.dupe_mode())
            .is_some_and(|mode| mode.mode != DupeMode::Exact)
        {
            return;
        }
        let victims: Vec<NodeId> = self
            .active_tab()
            .dupe_groups
            .iter()
            .flat_map(|g| g.victims_keep_newest())
            .collect();
        self.dupe_mark(&victims, cx);
    }

    fn dupe_replace_group_marks(
        &mut self,
        members: &[NodeId],
        victims: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        let selection = &mut self.active_tab_mut().selection;
        for member in members {
            selection.remove(member);
        }
        selection.extend(victims.iter().copied());
        cx.notify();
    }

    /// Add nodes to the marked-for-trash set.
    fn dupe_mark(&mut self, nodes: &[NodeId], cx: &mut Context<Self>) {
        let sel = &mut self.active_tab_mut().selection;
        for n in nodes {
            sel.insert(*n);
        }
        cx.notify();
    }

    /// Clear all marks.
    fn dupe_clear_marks(&mut self, cx: &mut Context<Self>) {
        self.active_tab_mut().selection.clear();
        cx.notify();
    }

    /// Trash every marked member, then prune the retained model and
    /// rebuild the card list / table from what survived. Mirrors
    /// `on_move_to_trash`'s off-thread worker + undo, but owns the prune
    /// because a dupe tab's watcher reload is suppressed.
    fn dupe_trash_marked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;

        let tab_id = self.active_tab().id;
        let marked = self.active_tab().selection.clone();
        if marked.is_empty() {
            return;
        }
        // Resolve marked nodes to paths via the retained model (no I/O).
        let paths: Vec<PathBuf> = self
            .active_tab()
            .dupe_groups
            .iter()
            .flat_map(|group| group.members.iter().map(move |member| (group, member)))
            .filter(|(group, member)| {
                marked.contains(&member.node)
                    && !(group.mode == DupeMode::Similar && group.keeper == Some(member.node))
            })
            .map(|(_, member)| member.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let process = self.process.clone();
        let task_id = self.process.tasks.borrow_mut().begin(
            crate::tasks::TaskKind::FileOp,
            trn!("Trashing {n} duplicate", "Trashing {n} duplicates", count),
            false,
        );
        let weak = cx.weak_entity();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let (pairs, error) = cx
                .background_executor()
                .spawn(async move {
                    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
                    for path in &paths {
                        match ferail_fs_native::move_to_trash(path) {
                            Ok(Some(trashed)) => pairs.push((path.clone(), trashed)),
                            Ok(None) => {}
                            Err(e) => return (pairs, Some(e.to_string())),
                        }
                    }
                    (pairs, None)
                })
                .await;
            match &error {
                Some(e) => process.tasks.borrow_mut().end_failed(task_id, e.clone()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    if !pairs.is_empty() {
                        let trashed_nodes: Vec<PathBuf> =
                            pairs.iter().map(|(orig, _)| orig.clone()).collect();
                        this.prune_dupe_model_by_path(tab_id, &trashed_nodes, cx);
                        this.push_undo(UndoOp::TrashRestore(pairs.clone()));
                    }
                    cx.notify();
                });
            }
            let _ = win.update(cx, |_, window, cx| match &error {
                None => window.push_notification(
                    Notification::info(trn!(
                        "Moved {n} duplicate to Trash",
                        "Moved {n} duplicates to Trash",
                        count
                    )),
                    cx,
                ),
                Some(e) => window.push_notification(
                    crate::shell::error_notification(
                        tr!("Trash failed: {detail}", detail = e).to_string(),
                    ),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// macOS/APFS zero-copy dedup: replace a group's redundant,
    /// storage-owning copies with `clonefile` clones of the keeper, after
    /// an explicit confirm. Keeps every file; frees the duplicated bytes.
    #[cfg(target_os = "macos")]
    fn dupe_dedup_group(&mut self, group_no: usize, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;

        let tab_id = self.active_tab().id;
        let (keeper_path, victims): (PathBuf, Vec<(NodeId, PathBuf)>) = {
            let Some(g) = self
                .active_tab()
                .dupe_groups
                .iter()
                .find(|g| g.group_no == group_no)
            else {
                return;
            };
            // Similar files are intentionally different bytes. Replacing one
            // with another via clonefile would be silent data loss.
            if g.mode != DupeMode::Exact {
                return;
            }
            let Some(keeper) = g.keeper.or_else(|| g.newest()) else {
                return;
            };
            let Some(keeper_path) = g
                .members
                .iter()
                .find(|m| m.node == keeper)
                .map(|m| m.path.clone())
            else {
                return;
            };
            let victims = g
                .members
                .iter()
                .filter(|m| m.node != keeper && !m.shares_storage())
                .map(|m| (m.node, m.path.clone()))
                .collect();
            (keeper_path, victims)
        };
        if victims.is_empty() {
            return;
        }
        let count = victims.len();
        let keeper_name = keeper_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let process = self.process.clone();
        let weak = cx.weak_entity();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let (go_tx, go_rx) = async_channel::bounded::<bool>(1);
            let opened = win.update(cx, |_, window, cx| {
                let tx = go_tx.clone();
                window.open_dialog(cx, move |dialog, _window, _cx| {
                    let tx_go = tx.clone();
                    let tx_cancel = tx.clone();
                    let body = trn!(
                        "Replace {n} extra copy with APFS clones of \u{201C}{keeper}\u{201D}? \
                         Every file stays; the duplicated bytes are freed.",
                        "Replace {n} extra copies with APFS clones of \u{201C}{keeper}\u{201D}? \
                         Every file stays; the duplicated bytes are freed.",
                        count,
                        keeper = keeper_name
                    );
                    dialog
                        .title(tr!("Dedup with clones?"))
                        .child(div().text_scale_sm().child(body))
                        .child(
                            h_flex().pt_2().child(
                                Button::new("dupe-clone-go")
                                    .label(tr!("Dedup"))
                                    .primary()
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let _ = tx_go.try_send(true);
                                        window.close_dialog(cx);
                                    }),
                            ),
                        )
                        .on_cancel(move |_, _, _| {
                            let _ = tx_cancel.try_send(false);
                            true
                        })
                });
            });
            if opened.is_err() || !matches!(go_rx.recv().await, Ok(true)) {
                return;
            }
            let task_id = process.tasks.borrow_mut().begin(
                crate::tasks::TaskKind::FileOp,
                trn!("Cloning {n} duplicate", "Cloning {n} duplicates", count),
                false,
            );
            let victim_paths: Vec<PathBuf> = victims.iter().map(|(_, p)| p.clone()).collect();
            let keeper_for_bg = keeper_path.clone();
            let (done_ix, error) = cx
                .background_executor()
                .spawn(async move {
                    let mut done: Vec<usize> = Vec::new();
                    let mut err: Option<String> = None;
                    for (i, vp) in victim_paths.iter().enumerate() {
                        match ferail_fs_native::clone_dedup(&keeper_for_bg, vp) {
                            Ok(()) => done.push(i),
                            Err(e) => {
                                err = Some(e);
                                break;
                            }
                        }
                    }
                    (done, err)
                })
                .await;
            match &error {
                Some(e) => process.tasks.borrow_mut().end_failed(task_id, e.clone()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            let cloned: Vec<NodeId> = done_ix
                .into_iter()
                .filter_map(|i| victims.get(i).map(|(n, _)| *n))
                .collect();
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    this.mark_dupe_members_cloned(tab_id, group_no, &cloned, cx);
                });
            }
            let _ = win.update(cx, |_, window, cx| match &error {
                None => window.push_notification(
                    Notification::info(trn!(
                        "Replaced {n} copy with clones",
                        "Replaced {n} copies with clones",
                        count
                    )),
                    cx,
                ),
                Some(e) => window.push_notification(
                    crate::shell::error_notification(
                        tr!("Dedup failed: {detail}", detail = e).to_string(),
                    ),
                    cx,
                ),
            });
        })
        .detach();
    }

    #[cfg(not(target_os = "macos"))]
    fn dupe_dedup_group(
        &mut self,
        _group_no: usize,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// Flag freshly-created clones in the retained model and recompute the
    /// reclaim summary. The panel re-renders straight from the model — no
    /// table rebuild, no I/O. macOS-only (the dedup path that calls it is
    /// gated).
    #[cfg(target_os = "macos")]
    fn mark_dupe_members_cloned(
        &mut self,
        tab_id: TabId,
        group_no: usize,
        nodes: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let tab = &mut self.tabs[idx];
        if let Some(g) = tab.dupe_groups.iter_mut().find(|g| g.group_no == group_no) {
            for m in g.members.iter_mut() {
                if nodes.contains(&m.node) {
                    m.is_clone = true;
                }
            }
        }
        if let Some(dm) = tab
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        {
            dm.wasted_bytes = tab.dupe_groups.iter().map(|g| g.reclaimable_bytes()).sum();
        }
        cx.notify();
    }

    /// Drop trashed members (by path) from the retained groups, drop any
    /// group left with fewer than two members, renumber, recompute the
    /// reclaim summary, and clear the marks. The panel renders from this
    /// model directly, so there is nothing to rebuild and no I/O.
    fn prune_dupe_model_by_path(
        &mut self,
        tab_id: TabId,
        trashed: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let trashed: std::collections::HashSet<&PathBuf> = trashed.iter().collect();
        let tab = &mut self.tabs[idx];
        for g in tab.dupe_groups.iter_mut() {
            g.members.retain(|m| !trashed.contains(&m.path));
        }
        tab.dupe_groups.retain(|g| g.members.len() >= 2);
        // Renumber 1..N so the cards stay gap-free after a cleanup.
        for (i, g) in tab.dupe_groups.iter_mut().enumerate() {
            g.group_no = i + 1;
        }
        tab.selection.clear();
        if let Some(dm) = tab
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        {
            dm.groups = tab.dupe_groups.len();
            dm.wasted_bytes = tab.dupe_groups.iter().map(|g| g.reclaimable_bytes()).sum();
        }
        cx.notify();
    }
}

fn dupe_group_card_estimated_size(group: &DupeGroupView) -> Size<Pixels> {
    const HEADER_H: f32 = 36.0;
    const BODY_PAD_H: f32 = 8.0;
    let member_row_h = if group.mode == DupeMode::Similar {
        72.0
    } else {
        25.0
    };

    let body_h = if group.expanded {
        BODY_PAD_H + member_row_h * group.members.len() as f32
    } else {
        0.0
    };
    Size {
        width: px(0.0),
        height: px(HEADER_H + body_h),
    }
}

/// Member's parent directory relative to the scan root, matching the
/// grouped-rows location string.
fn member_location(path: &Path, root: &Path) -> String {
    path.parent()
        .map(|parent| match parent.strip_prefix(root) {
            Ok(rel) if rel.as_os_str().is_empty() => "\u{00B7}".to_string(),
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => parent.to_string_lossy().into_owned(),
        })
        .unwrap_or_default()
}

/// Human wording for the two perceptual distances stored against the group's
/// medoid/reference. The closest half of each accepted range gets the stronger
/// label; the exact 0/0 member is the reference itself, not necessarily a
/// byte-identical file.
fn similarity_summary(structure: u32, detail: u32) -> SharedString {
    if structure == 0 && detail == 0 {
        return tr!("Group reference");
    }
    if structure <= 3 && detail <= 6 {
        tr!(
            "Very similar · structure {structure}/64 · detail {detail}/64",
            structure = structure,
            detail = detail
        )
    } else {
        tr!(
            "Similar · structure {structure}/64 · detail {detail}/64",
            structure = structure,
            detail = detail
        )
    }
}

/// List-style one-step navigation: clamp at the ends, and choose the first
/// (Down) or last (Up) item when the panel has no focus yet.
fn stepped_focus_index(item_count: usize, current: Option<usize>, forward: bool) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    Some(
        match (current.filter(|index| *index < item_count), forward) {
            (Some(index), true) => (index + 1).min(item_count - 1),
            (Some(index), false) => index.saturating_sub(1),
            (None, true) => 0,
            (None, false) => item_count - 1,
        },
    )
}

#[cfg(test)]
mod focus_navigation_tests {
    use super::stepped_focus_index;

    #[test]
    fn arrows_choose_an_edge_without_an_existing_focus() {
        assert_eq!(stepped_focus_index(4, None, true), Some(0));
        assert_eq!(stepped_focus_index(4, None, false), Some(3));
    }

    #[test]
    fn arrows_step_and_clamp_at_the_ends() {
        assert_eq!(stepped_focus_index(4, Some(1), true), Some(2));
        assert_eq!(stepped_focus_index(4, Some(2), false), Some(1));
        assert_eq!(stepped_focus_index(4, Some(3), true), Some(3));
        assert_eq!(stepped_focus_index(4, Some(0), false), Some(0));
    }

    #[test]
    fn an_empty_panel_has_nowhere_to_focus() {
        assert_eq!(stepped_focus_index(0, None, true), None);
    }
}
