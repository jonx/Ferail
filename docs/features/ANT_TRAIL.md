# Ant Trail

Ant Trail is Ferail's folder-usage heat system. It tracks where the user
actually goes and uses that signal to make navigation feel smarter.

## Status

Partial.

Feraille currently has:

- In-memory `AntTrail` in `feraille-core`.
- Navigation records a visit when a folder commit happens.
- File tree rows can draw a subtle heat indicator.

## Rules

- Ant Trail records committed navigation, not mere hover.
- Expanding/collapsing a tree node is not a visit.
- Reading heat for paint must be cheap and in-memory.
- Persistence, decay, and prediction run outside paint.

## Target

- Persist heat in SQLite.
- Use logarithmic normalization so one heavily-used folder does not flatten
  every other signal.
- Keep optional decay disabled by default until there is real evidence it helps.
- Feed idle prefetch:
  - likely child folders,
  - parent/sibling folders,
  - magic sniffing,
  - previews,
  - icon cache,
  - disk usage summaries.

## Mac Notes

- Heat keys should survive common Mac path churn where possible. Prefer stable
  file IDs/bookmarks later; path is acceptable for the first persistence slice.
- Network volumes and external drives need volume identity in the key.

## Todo

- Persistent store.
- Hot-folder query API.
- Prediction scheduler.
- Debug overlay for heat and prefetch decisions.
- Tests for "navigation commit only" semantics.
