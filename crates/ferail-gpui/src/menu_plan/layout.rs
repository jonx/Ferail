//! The order the user put a menu in, and the separators they placed.
//!
//! Hiding an entry needs no order: a hidden set is a set, entries the spec
//! never names stay visible, and nothing has to be decided about where they
//! sit. Reordering is not like that, and the hard part is not the dragging:
//!
//! **Where does a command added in a later version land, for a user who
//! rearranged the menu a year ago?** Storing the arrangement as a replacement
//! list makes the only possible answer "at the end", which buries every new
//! command under whatever the user happened to leave last. So the saved list
//! is an *override anchored to the built-in one*: an entry the user never
//! moved is reinserted next to the built-in neighbour it was designed to
//! follow, wherever that neighbour ended up.
//!
//! The persisted form is `surface:tok,tok;surface:tok`, where a token is a
//! command id or `-` for a separator. Same robustness rules as
//! [`super::prefs`]: unknown surfaces are ignored, junk costs the
//! customization and never the menu, and nothing here can make an entry
//! vanish, since [`merge`] reinserts every built-in the saved list forgot.

use ferail_core::commands::CommandId;

use super::inventory::Item;
use super::MenuSurface;

/// One position in a menu's arrangement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Slot {
    Entry(String),
    Separator,
}

impl Slot {
    pub(crate) fn is_separator(&self) -> bool {
        matches!(self, Self::Separator)
    }

    pub(crate) fn id(&self) -> Option<&str> {
        match self {
            Self::Entry(id) => Some(id),
            Self::Separator => None,
        }
    }

    fn token(&self) -> &str {
        match self {
            Self::Entry(id) => id,
            Self::Separator => SEPARATOR_TOKEN,
        }
    }
}

const SEPARATOR_TOKEN: &str = "-";

/// Merge a saved arrangement with the built-in one.
///
/// Every built-in entry the saved list does not mention is reinserted **after
/// the built-in entry that precedes it and is present**, so a command added in
/// a new version lands beside the neighbour it was designed to follow rather
/// than at the bottom of the menu. An entry the saved list names but this
/// build does not have is dropped from the result and kept in storage by the
/// caller, so a downgrade does not silently discard an arrangement.
pub(crate) fn merge(builtin: &[Item], saved: &[Slot]) -> Vec<Slot> {
    // Nothing saved: the built-in arrangement, separators and all. This is
    // also what the editor starts a user from, which is why the separators
    // have to live in the inventory rather than only in the menu builder.
    if saved.is_empty() {
        return builtin
            .iter()
            .map(|item| match item.id() {
                Some(id) => Slot::Entry(id.0.to_string()),
                None => Slot::Separator,
            })
            .collect();
    }
    // Past that point the separators are the user's, not the menu's: a saved
    // arrangement carries the ones they kept, and reinserting the built-in
    // ones would put back exactly what they removed.
    let mut merged: Vec<Slot> = saved
        .iter()
        .filter(|slot| {
            slot.id()
                .is_none_or(|id| builtin.iter().any(|known| known.id().is_some_and(|k| k.0 == id)))
        })
        .cloned()
        .collect();

    let entries: Vec<CommandId> = builtin.iter().filter_map(|item| item.id()).collect();
    for (position, id) in entries.iter().enumerate() {
        if merged.iter().any(|slot| slot.id() == Some(id.0)) {
            continue;
        }
        // Walk back through the built-in order for the nearest neighbour that
        // survived, and land right after it. Nothing before it survived (or it
        // was the first entry to begin with): the front of the menu.
        let anchor = entries[..position]
            .iter()
            .rev()
            .find_map(|earlier| merged.iter().position(|slot| slot.id() == Some(earlier.0)));
        let at = anchor.map_or(0, |index| index + 1);
        merged.insert(at, Slot::Entry(id.0.to_string()));
    }
    merged
}

/// Parse the persisted form into per-surface arrangements.
pub(crate) fn parse(spec: &str) -> Vec<(String, Vec<Slot>)> {
    let mut out: Vec<(String, Vec<Slot>)> = Vec::new();
    for group in spec.split(';').filter(|group| !group.trim().is_empty()) {
        let Some((surface, tokens)) = group.split_once(':') else {
            continue;
        };
        let surface = surface.trim();
        if !MenuSurface::ALL.iter().any(|known| known.key() == surface) {
            continue;
        }
        let slots: Vec<Slot> = tokens
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(|token| {
                if token == SEPARATOR_TOKEN {
                    Slot::Separator
                } else {
                    Slot::Entry(token.to_string())
                }
            })
            .collect();
        if slots.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(key, _)| key == surface) {
            Some((_, existing)) => existing.extend(slots),
            None => out.push((surface.to_string(), slots)),
        }
    }
    out
}

pub(crate) fn serialize(layouts: &[(String, Vec<Slot>)]) -> Option<String> {
    let rendered: Vec<String> = layouts
        .iter()
        .filter(|(_, slots)| !slots.is_empty())
        .map(|(surface, slots)| {
            let tokens: Vec<&str> = slots.iter().map(Slot::token).collect();
            format!("{surface}:{}", tokens.join(","))
        })
        .collect();
    (!rendered.is_empty()).then(|| rendered.join(";"))
}

/// Tidy an arrangement the way the rendered menu will be tidied anyway:
/// separators never bookend it, and never run two deep.
///
/// Applied when the user's edit is saved, not only when the menu is drawn, so
/// the editor cannot show a leading separator that the real menu drops. What
/// the list shows is what the menu will be.
pub(crate) fn tidy(mut slots: Vec<Slot>) -> Vec<Slot> {
    let mut out: Vec<Slot> = Vec::with_capacity(slots.len());
    for slot in slots.drain(..) {
        if slot.is_separator() && (out.is_empty() || out.last().is_some_and(Slot::is_separator)) {
            continue;
        }
        out.push(slot);
    }
    if out.last().is_some_and(Slot::is_separator) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{merge, parse, serialize, tidy, Item, Slot};
    use crate::menu_plan::MenuSurface;
    use ferail_core::commands::CommandId;

    /// A built-in arrangement from a sketch: `-` is a separator, anything
    /// else an entry id.
    fn ids(list: &[&'static str]) -> Vec<Item> {
        list.iter()
            .map(|token| {
                if *token == "-" {
                    Item::Separator
                } else {
                    Item::Entry(CommandId(token))
                }
            })
            .collect()
    }

    fn slots(list: &[&str]) -> Vec<Slot> {
        list.iter()
            .map(|token| {
                if *token == "-" {
                    Slot::Separator
                } else {
                    Slot::Entry(token.to_string())
                }
            })
            .collect()
    }

    fn tokens(slots: &[Slot]) -> Vec<&str> {
        slots
            .iter()
            .map(|slot| slot.id().unwrap_or("-"))
            .collect()
    }

    #[test]
    fn no_saved_arrangement_is_the_built_in_one() {
        let builtin = ids(&["a", "b", "c"]);
        assert_eq!(tokens(&merge(&builtin, &[])), vec!["a", "b", "c"]);
    }

    #[test]
    fn a_new_command_lands_beside_its_neighbour_not_at_the_end() {
        // The user moved things around back when the menu was a, c, d. This
        // build inserts `b` after `a`, so that is where it should reappear:
        // at the end it would be buried under everything the user left last.
        let builtin = ids(&["a", "b", "c", "d"]);
        let saved = slots(&["d", "-", "c", "a"]);
        assert_eq!(tokens(&merge(&builtin, &saved)), vec!["d", "-", "c", "a", "b"]);

        // And when the neighbour is in the middle of the saved list, the new
        // entry goes right after it rather than at either end.
        let saved = slots(&["a", "c", "d"]);
        assert_eq!(tokens(&merge(&builtin, &saved)), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn a_new_first_command_lands_at_the_front() {
        let builtin = ids(&["new", "a", "b"]);
        let saved = slots(&["b", "a"]);
        assert_eq!(tokens(&merge(&builtin, &saved)), vec!["new", "b", "a"]);
    }

    #[test]
    fn an_entry_this_build_does_not_have_is_dropped_from_the_result() {
        // Saved by a newer version. It must not render, and it must not take
        // the rest of the arrangement down with it.
        let builtin = ids(&["a", "b"]);
        let saved = slots(&["b", "from.the.future", "a"]);
        assert_eq!(tokens(&merge(&builtin, &saved)), vec!["b", "a"]);
    }

    #[test]
    fn merging_never_loses_an_entry() {
        let builtin = ids(&["a", "b", "c", "d", "e"]);
        for saved in [
            slots(&[]),
            slots(&["e"]),
            slots(&["-", "-"]),
            slots(&["c", "-", "a"]),
            slots(&["e", "d", "c", "b", "a"]),
        ] {
            let merged = merge(&builtin, &saved);
            for id in builtin.iter().filter_map(|item| item.id()) {
                assert!(
                    merged.iter().any(|slot| slot.id() == Some(id.0)),
                    "{} vanished from {:?}",
                    id.0,
                    tokens(&merged)
                );
            }
        }
    }

    #[test]
    fn separators_are_tidied_when_the_arrangement_is_saved() {
        assert_eq!(tokens(&tidy(slots(&["-", "-", "a", "-", "-", "b", "-"]))), vec!["a", "-", "b"]);
        assert!(tidy(slots(&["-", "-"])).is_empty());
    }

    #[test]
    fn round_trips_through_the_persisted_form() {
        let layouts = vec![
            ("file.row".to_string(), slots(&["file.open", "-", "file.rename"])),
            ("file.background".to_string(), slots(&["file.refresh"])),
        ];
        let spec = serialize(&layouts).expect("something is arranged");
        assert_eq!(parse(&spec), layouts);
    }

    #[test]
    fn junk_costs_the_arrangement_never_the_menu() {
        let parsed = parse(";;nonsense;sidebar.favorites:file.open;file.row:file.open,,-,file.rename;");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "file.row");
        assert_eq!(tokens(&parsed[0].1), vec!["file.open", "-", "file.rename"]);
    }

    #[test]
    fn the_built_in_arrangement_covers_every_listed_entry() {
        for surface in MenuSurface::ALL {
            let arrangement = merge(crate::menu_plan::inventory::items(surface), &[]);
            for id in crate::menu_plan::inventory::entries(surface) {
                assert!(
                    arrangement.iter().any(|slot| slot.id() == Some(id.0)),
                    "{} missing from the built-in arrangement of {}",
                    id.0,
                    surface.key()
                );
            }
        }
    }
}
