//! Table-driven context menus: a menu as data before it is a widget.
//!
//! Menus used to be built straight into `PopupMenu`, one `menu(label, action)`
//! call after another, which made an entry's identity its Rust action type.
//! Nothing stable existed to key a preference on, so "let the user hide the
//! entries they never use" had nowhere to attach, and every label was written
//! twice (once in the command catalogue, once at the menu site).
//!
//! A [`MenuPlan`] is the same menu expressed as a list of [`PlanItem`]s, each
//! carrying a stable [`CommandId`], built first and rendered second. The
//! rendering step is where cross-cutting rules live: today separator hygiene,
//! next the user's visibility preferences. Building the list costs one `Vec`
//! per open and no I/O, so the Prime Directive rule for menu construction is
//! unchanged: read-only, allocation-light, no shell or filesystem queries.
//!
//! See [CONTEXT_MENU.md](../../../docs/features/CONTEXT_MENU.md).

use gpui::{Action, Entity, SharedString};
use gpui_component::menu::{PopupMenu, PopupMenuItem};

use ferail_core::commands::CommandId;

pub(crate) mod ids;
pub(crate) mod inventory;
pub(crate) mod layout;
pub mod prefs;

/// Which menu an entry belongs to. Visibility is stored per
/// `(surface, command)`: the same command can be wanted in one menu and not
/// in another, so the surface is part of the key, never inferred.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MenuSurface {
    /// File list row and icon-grid cell: one definition serves both.
    FileRow,
    /// Empty space in the file pane, targeting the browsed folder.
    FileBackground,
    /// A row in a trash folder. Its own surface rather than a variant of
    /// [`Self::FileRow`], because a deleted item answers to a different set of
    /// verbs: most of the ordinary ones are meaningless on it (renaming,
    /// compressing, tagging, trashing something already in the trash) and the
    /// ones that matter, putting it back and deleting it for good, exist
    /// nowhere else. A separate surface also means a user's preferences for
    /// the two cannot collide.
    TrashRow,
    /// Empty space in a trash folder.
    TrashBackground,
}

impl MenuSurface {
    /// Every surface, in the order the settings UI lists them.
    pub(crate) const ALL: [MenuSurface; 4] = [
        Self::FileRow,
        Self::FileBackground,
        Self::TrashRow,
        Self::TrashBackground,
    ];

    /// Stable key for persistence. Never derived from the variant name, so
    /// renaming the variant cannot silently orphan a user's saved preference.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::FileRow => "file.row",
            Self::FileBackground => "file.background",
            Self::TrashRow => "trash.row",
            Self::TrashBackground => "trash.background",
        }
    }
}

/// One entry in a planned menu.
///
/// Submenus arrive pre-built as entities, because gpui-component builds a
/// nested `PopupMenu` through its own `build`, which needs a `Window`. The
/// plan only decides whether the submenu appears and under which id: its
/// contents stay where they are built.
pub(crate) enum PlanItem {
    Action {
        id: CommandId,
        label: SharedString,
        action: Box<dyn Action>,
    },
    Submenu {
        id: CommandId,
        label: SharedString,
        menu: Entity<PopupMenu>,
    },
    Separator,
}

/// The two questions the list rules ask of an item.
///
/// Extracted as a trait so `tidy` and `duplicate_ids` can be tested against a
/// plain enum: a real [`PlanItem`] carries a boxed `gpui::Action` or a live
/// `Entity`, neither of which a unit test can mint, and neither of which the
/// rules ever look at.
trait PlanShape {
    fn is_separator(&self) -> bool;
    fn entry_id(&self) -> Option<&'static str>;
}

impl PlanShape for PlanItem {
    fn is_separator(&self) -> bool {
        matches!(self, Self::Separator)
    }

    fn entry_id(&self) -> Option<&'static str> {
        match self {
            Self::Action { id, .. } | Self::Submenu { id, .. } => Some(id.0),
            Self::Separator => None,
        }
    }
}

/// A menu, as data.
pub(crate) struct MenuPlan {
    surface: MenuSurface,
    items: Vec<PlanItem>,
}

impl MenuPlan {
    pub(crate) fn new(surface: MenuSurface) -> Self {
        // Row menus run to roughly fifty entries; one allocation up front
        // beats growing during a right-click.
        Self {
            surface,
            items: Vec::with_capacity(56),
        }
    }

    pub(crate) fn action(
        mut self,
        id: CommandId,
        label: impl Into<SharedString>,
        action: Box<dyn Action>,
    ) -> Self {
        self.items.push(PlanItem::Action {
            id,
            label: label.into(),
            action,
        });
        self
    }

    pub(crate) fn submenu(
        mut self,
        id: CommandId,
        label: impl Into<SharedString>,
        menu: Entity<PopupMenu>,
    ) -> Self {
        self.items.push(PlanItem::Submenu {
            id,
            label: label.into(),
            menu,
        });
        self
    }

    pub(crate) fn separator(mut self) -> Self {
        self.items.push(PlanItem::Separator);
        self
    }

    /// Render into the widget: apply the user's preferences, tidy what is
    /// left, then emit it.
    pub(crate) fn render(mut self, mut menu: PopupMenu) -> PopupMenu {
        // A duplicated id is not cosmetic: it makes one preference govern two
        // different entries, so it has to fail while the plan is being
        // written, not after a user reports a menu item that will not stay
        // hidden. Debug-only, and the plans are static enough that a debug
        // run over each surface is a real check.
        #[cfg(debug_assertions)]
        {
            let duplicates = duplicate_ids(&self.items);
            assert!(
                duplicates.is_empty(),
                "duplicate menu entry ids in {}: {duplicates:?}",
                self.surface.key()
            );
            // The settings UI lists what a surface can show from
            // `inventory`, which a live plan could drift away from. Checking
            // it here means adding an entry and forgetting the inventory
            // fails on the first right-click in a dev build, rather than
            // silently shipping an entry nobody can turn off.
            for id in self.items.iter().filter_map(PlanShape::entry_id) {
                assert!(
                    inventory::lists(self.surface, CommandId(id)),
                    "{id} is in the {} menu but not in its inventory",
                    self.surface.key()
                );
            }
        }
        // Fast path: with no customization at all, which is the overwhelmingly
        // common case, this is exactly the old behaviour plus one atomic read.
        if prefs::any_hidden() {
            let surface = self.surface;
            self.items.retain(|item| {
                item.entry_id()
                    .is_none_or(|id| !prefs::is_hidden(surface, CommandId(id)))
            });
        }
        if prefs::surface_is_arranged(self.surface) {
            self.items = arrange(self.items, &prefs::arrangement(self.surface));
        }
        for item in tidy(self.items) {
            menu = match item {
                PlanItem::Action { label, action, .. } => menu.menu(label, action),
                PlanItem::Submenu {
                    label, menu: sub, ..
                } => menu.item(PopupMenuItem::submenu(label, sub)),
                PlanItem::Separator => menu.separator(),
            };
        }
        menu
    }
}

/// Drop leading and trailing separators and collapse runs of them to one.
///
/// Entries come and go with `Availability`, and soon with the user's
/// preferences, so a group can empty out and leave the separators that framed
/// it behind. Doing this as one pass over the finished list, rather than as
/// bookkeeping at each `if`, keeps the group structure readable at the call
/// site and makes the rule testable on its own.
fn tidy<T: PlanShape>(items: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if item.is_separator() && (out.is_empty() || out.last().is_some_and(T::is_separator)) {
            continue;
        }
        out.push(item);
    }
    if out.last().is_some_and(T::is_separator) {
        out.pop();
    }
    out
}

/// Reorder a plan to the user's arrangement.
///
/// The arrangement names every entry the surface can ever show; a plan holds
/// only the ones `Availability` allowed for this particular right-click, so
/// most slots find nothing and are skipped. The separators come from the
/// arrangement rather than from the plan: once a user has moved anything, the
/// group boundaries are theirs, and re-adding the built-in ones would put back
/// exactly what they removed. `tidy` still runs afterwards, so a group that
/// emptied out for this click does not leave its separators behind.
fn arrange(mut items: Vec<PlanItem>, arrangement: &[layout::Slot]) -> Vec<PlanItem> {
    let mut ordered = Vec::with_capacity(items.len());
    for slot in arrangement {
        match slot.id() {
            None => ordered.push(PlanItem::Separator),
            Some(id) => {
                if let Some(at) = items.iter().position(|item| item.entry_id() == Some(id)) {
                    ordered.push(items.remove(at));
                }
            }
        }
    }
    // An entry the arrangement did not name keeps its place at the end instead
    // of disappearing. `merge` should make this unreachable, but a menu entry
    // silently vanishing is not a failure mode worth risking on that.
    ordered.extend(items.into_iter().filter(|item| !item.is_separator()));
    ordered
}

/// Ids appearing more than once in the same plan.
fn duplicate_ids<T: PlanShape>(items: &[T]) -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::with_capacity(items.len());
    let mut duplicates = Vec::new();
    for id in items.iter().filter_map(T::entry_id) {
        if seen.contains(&id) {
            duplicates.push(id);
        } else {
            seen.push(id);
        }
    }
    duplicates
}

#[cfg(test)]
mod tests {
    use super::{MenuSurface, PlanShape, duplicate_ids, tidy};

    enum Test {
        Entry(&'static str),
        Separator,
    }

    impl PlanShape for Test {
        fn is_separator(&self) -> bool {
            matches!(self, Self::Separator)
        }

        fn entry_id(&self) -> Option<&'static str> {
            match self {
                Self::Entry(id) => Some(id),
                Self::Separator => None,
            }
        }
    }

    /// Read a plan out of a sketch: `x` is an entry, `-` a separator. Entry
    /// ids are positional so a shape assertion stays readable.
    fn plan(sketch: &str) -> Vec<Test> {
        const IDS: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let mut next = 0;
        sketch
            .chars()
            .map(|ch| match ch {
                '-' => Test::Separator,
                _ => {
                    next += 1;
                    Test::Entry(IDS[next - 1])
                }
            })
            .collect()
    }

    fn shape(items: &[Test]) -> String {
        items
            .iter()
            .map(|item| if item.is_separator() { '-' } else { 'x' })
            .collect()
    }

    #[test]
    fn separators_collapse_and_never_bookend_the_menu() {
        // What a menu looks like when two groups emptied out: a leading pair,
        // an inner run, and a trailing one.
        assert_eq!(shape(&tidy(plan("--x--x-"))), "x-x");
    }

    #[test]
    fn a_menu_of_nothing_but_separators_renders_empty() {
        assert!(tidy(plan("---")).is_empty());
    }

    #[test]
    fn an_already_tidy_menu_is_left_alone() {
        assert_eq!(shape(&tidy(plan("x-xx"))), "x-xx");
        assert_eq!(shape(&tidy(plan("x"))), "x");
        assert!(tidy(plan("")).is_empty());
    }

    #[test]
    fn duplicate_entry_ids_are_reported_and_separators_are_never_ids() {
        let items = vec![
            Test::Entry("file.open"),
            Test::Separator,
            Test::Separator,
            Test::Entry("file.rename"),
            Test::Entry("file.open"),
        ];
        assert_eq!(duplicate_ids(&items), vec!["file.open"]);

        let items = vec![Test::Entry("file.open"), Test::Separator, Test::Separator];
        assert!(duplicate_ids(&items).is_empty());
    }

    #[test]
    fn surface_keys_are_stable_and_distinct() {
        assert_eq!(MenuSurface::FileRow.key(), "file.row");
        assert_eq!(MenuSurface::FileBackground.key(), "file.background");
    }
}
