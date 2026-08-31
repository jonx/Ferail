//! Which menu entries the user turned off.
//!
//! Storage is `(surface, command)`: the same command can be wanted in one menu
//! and not in another. The persisted form is one `AppState` string,
//! `surface:id,id;surface:id`, and it follows the rules the column-visibility
//! spec already established, because they are what makes a saved preference
//! survive an upgrade:
//!
//! - unknown surfaces and unknown ids are **ignored**, so a spec written by a
//!   newer build does not wedge an older one;
//! - an entry the spec never mentions is **visible**, so a command added in a
//!   later version is never invisible to a user who upgraded;
//! - a few entries can never be hidden at all, so nobody can configure
//!   themselves out of the menu's primary verbs.
//!
//! Menus must not get slower to open. The spec is parsed once at startup and
//! whenever the user changes it, never at menu-open time, and the common case
//! (nothing hidden) short-circuits to exactly the old behaviour.

use std::sync::{Arc, RwLock};

use ferail_core::commands::CommandId;

use super::{MenuSurface, ids};

/// Entries no preference may hide.
///
/// Open and Get Info are the menu's reason to exist, and the same rule already
/// protects the file table's `name` column. Enforced when the spec is parsed,
/// not only in the settings UI, so a hand-edited file cannot do what the UI
/// refuses to.
pub(crate) const ALWAYS_VISIBLE: [CommandId; 2] = [ids::OPEN, ids::GET_INFO];

/// Parsed spec: hidden ids per surface key. A `Vec` rather than a set because
/// it is nearly always empty or tiny, and a linear scan over three strings
/// beats hashing.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Hidden {
    surfaces: Vec<(String, Vec<String>)>,
}

impl Hidden {
    fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    fn ids(&self, surface: MenuSurface) -> &[String] {
        self.surfaces
            .iter()
            .find(|(key, _)| key == surface.key())
            .map_or(&[], |(_, ids)| ids.as_slice())
    }

    fn contains(&self, surface: MenuSurface, id: CommandId) -> bool {
        self.ids(surface).iter().any(|hidden| hidden == id.0)
    }

    fn set(&mut self, surface: MenuSurface, id: CommandId, hidden: bool) {
        if hidden && ALWAYS_VISIBLE.contains(&id) {
            return;
        }
        let position = self
            .surfaces
            .iter()
            .position(|(key, _)| key == surface.key());
        match (position, hidden) {
            (Some(index), true) => {
                let ids = &mut self.surfaces[index].1;
                if !ids.iter().any(|held| held == id.0) {
                    ids.push(id.0.to_string());
                }
            }
            (Some(index), false) => {
                self.surfaces[index].1.retain(|held| held != id.0);
                if self.surfaces[index].1.is_empty() {
                    self.surfaces.remove(index);
                }
            }
            (None, true) => self
                .surfaces
                .push((surface.key().to_string(), vec![id.0.to_string()])),
            (None, false) => {}
        }
    }

    fn clear(&mut self, surface: MenuSurface) {
        self.surfaces.retain(|(key, _)| key != surface.key());
    }
}

/// Parse the persisted spec. Anything unparseable is dropped rather than
/// failing the load: a broken preference must cost the user their
/// customization, never their menus.
pub(crate) fn parse(spec: &str) -> Hidden {
    let mut hidden = Hidden::default();
    for group in spec.split(';').filter(|group| !group.trim().is_empty()) {
        let Some((surface, ids)) = group.split_once(':') else {
            continue;
        };
        let surface = surface.trim();
        if !MenuSurface::ALL.iter().any(|known| known.key() == surface) {
            continue;
        }
        let ids: Vec<String> = ids
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            // The floor is enforced here, not only in the UI: a spec that
            // names Open was either hand-edited or written by a build with a
            // different floor, and neither is a reason to hide it.
            .filter(|id| !ALWAYS_VISIBLE.iter().any(|always| always.0 == *id))
            .map(str::to_string)
            .collect();
        if ids.is_empty() {
            continue;
        }
        match hidden.surfaces.iter_mut().find(|(key, _)| key == surface) {
            Some((_, existing)) => existing.extend(ids),
            None => hidden.surfaces.push((surface.to_string(), ids)),
        }
    }
    hidden
}

/// Render back to the persisted form. `None` when nothing is hidden, so the
/// key disappears from the state file rather than persisting an empty string.
pub(crate) fn serialize(hidden: &Hidden) -> Option<String> {
    if hidden.is_empty() {
        return None;
    }
    Some(
        hidden
            .surfaces
            .iter()
            .map(|(surface, ids)| format!("{surface}:{}", ids.join(",")))
            .collect::<Vec<_>>()
            .join(";"),
    )
}

/// The live spec. `Arc` so a menu build takes a clone of the pointer and
/// releases the lock immediately, rather than holding a read guard across the
/// whole render.
fn current() -> &'static RwLock<Arc<Hidden>> {
    static CURRENT: std::sync::OnceLock<RwLock<Arc<Hidden>>> = std::sync::OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(Arc::new(Hidden::default())))
}

/// Adopt the persisted spec. Called once at startup and after every change:
/// never from menu building.
pub fn init(spec: Option<&str>) {
    let parsed = spec.map(parse).unwrap_or_default();
    if let Ok(mut slot) = current().write() {
        *slot = Arc::new(parsed);
    }
}

/// The parsed spec, for one menu build. One `Arc` clone, no parsing, no I/O.
fn snapshot() -> Arc<Hidden> {
    // A poisoned lock means a panic while someone held it. The preference is
    // not worth propagating that: fall back to "nothing hidden", which is the
    // behaviour with no customization at all.
    current()
        .read()
        .map(|slot| Arc::clone(&slot))
        .unwrap_or_else(|_| Arc::new(Hidden::default()))
}

/// Whether anything at all is hidden. The fast path: with no customization
/// (the overwhelmingly common case) menu rendering skips the check entirely.
pub(crate) fn any_hidden() -> bool {
    !snapshot().is_empty()
}

pub(crate) fn is_hidden(surface: MenuSurface, id: CommandId) -> bool {
    snapshot().contains(surface, id)
}

/// Change one entry and persist. Returns the new spec so the caller can write
/// it into `AppState` without re-reading the memo.
pub(crate) fn set_hidden(surface: MenuSurface, id: CommandId, hidden: bool) -> Option<String> {
    let mut next = Hidden {
        surfaces: snapshot().surfaces.clone(),
    };
    next.set(surface, id, hidden);
    let spec = serialize(&next);
    if let Ok(mut slot) = current().write() {
        *slot = Arc::new(next);
    }
    spec
}

/// Show every entry of one surface again.
pub(crate) fn reset(surface: MenuSurface) -> Option<String> {
    let mut next = Hidden {
        surfaces: snapshot().surfaces.clone(),
    };
    next.clear(surface);
    let spec = serialize(&next);
    if let Ok(mut slot) = current().write() {
        *slot = Arc::new(next);
    }
    spec
}

/// Whether this surface has any hidden entry, for the settings UI's Reset.
pub(crate) fn surface_is_customized(surface: MenuSurface) -> bool {
    !snapshot().ids(surface).is_empty()
}

#[cfg(test)]
mod tests {
    use super::{ALWAYS_VISIBLE, Hidden, parse, serialize};
    use crate::menu_plan::{MenuSurface, ids};

    #[test]
    fn round_trips_through_the_persisted_form() {
        let mut hidden = Hidden::default();
        hidden.set(MenuSurface::FileRow, ids::MAKE_ALIAS, true);
        hidden.set(MenuSurface::FileRow, ids::DUPLICATE, true);
        hidden.set(MenuSurface::FileBackground, ids::REFRESH, true);
        let spec = serialize(&hidden).expect("something is hidden");
        assert_eq!(parse(&spec), hidden);
    }

    #[test]
    fn nothing_hidden_persists_nothing() {
        let mut hidden = Hidden::default();
        assert_eq!(serialize(&hidden), None);
        // Un-hiding the last entry of a surface drops the surface too, rather
        // than leaving `file.row:` behind to be parsed back as noise.
        hidden.set(MenuSurface::FileRow, ids::MAKE_ALIAS, true);
        hidden.set(MenuSurface::FileRow, ids::MAKE_ALIAS, false);
        assert_eq!(serialize(&hidden), None);
    }

    #[test]
    fn the_floor_cannot_be_hidden_by_the_api_or_by_hand() {
        let mut hidden = Hidden::default();
        for id in ALWAYS_VISIBLE {
            hidden.set(MenuSurface::FileRow, id, true);
            assert!(!hidden.contains(MenuSurface::FileRow, id));
        }
        // A hand-written file, or one from a build with a smaller floor.
        let parsed = parse("file.row:file.open,file.get_info,file.make_alias");
        assert!(!parsed.contains(MenuSurface::FileRow, ids::OPEN));
        assert!(!parsed.contains(MenuSurface::FileRow, ids::GET_INFO));
        assert!(parsed.contains(MenuSurface::FileRow, ids::MAKE_ALIAS));
    }

    #[test]
    fn a_preference_applies_to_one_surface_only() {
        let mut hidden = Hidden::default();
        hidden.set(MenuSurface::FileRow, ids::COPY_PATH, true);
        assert!(hidden.contains(MenuSurface::FileRow, ids::COPY_PATH));
        assert!(!hidden.contains(MenuSurface::FileBackground, ids::COPY_PATH));
    }

    #[test]
    fn junk_costs_the_customization_never_the_menu() {
        // Unknown surface, unknown id, missing colon, stray separators: each
        // is dropped on its own without taking the rest of the spec with it.
        let parsed = parse(
            ";;sidebar.favorites:file.rename;nonsense;file.row:file.make_alias,,file.not_a_command;",
        );
        assert!(parsed.contains(MenuSurface::FileRow, ids::MAKE_ALIAS));
        // An unknown id is kept as text: it costs nothing, and it is how a
        // preference set in a newer build survives a downgrade and back.
        assert_eq!(parsed.ids(MenuSurface::FileRow).len(), 2);
        assert!(parsed.ids(MenuSurface::FileBackground).is_empty());
    }

    #[test]
    fn resetting_a_surface_leaves_the_others_alone() {
        let mut hidden = Hidden::default();
        hidden.set(MenuSurface::FileRow, ids::MAKE_ALIAS, true);
        hidden.set(MenuSurface::FileBackground, ids::REFRESH, true);
        hidden.clear(MenuSurface::FileRow);
        assert!(hidden.ids(MenuSurface::FileRow).is_empty());
        assert!(hidden.contains(MenuSurface::FileBackground, ids::REFRESH));
    }
}
