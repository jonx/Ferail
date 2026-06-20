# Ant Trail

Ant Trail is Ferail's folder-usage heat system. It tracks where the user
actually goes and uses that signal to make navigation feel smarter.

## Status

Shipped with follow-ups.

Feraille currently has:

- `AntTrail` logic in `feraille-core`.
- Process-wide path-based visit counts in `ProcessState`.
- SQLite persistence in `MetadataDb.folder_usage`.
- Startup hydration of visits and recents from the metadata DB.
- Navigation-commit recording with async DB write-through.
- Log-scaled heat normalization.
- Heat tinting in the file list/grid for visited directories.
- Recents sidebar section backed by the same visit log.
- Remove-from-Recents and Clear-Recents actions; these intentionally clear the
  matching Ant Trail heat because the signal is shared.

The remaining work is prediction: using the heat signal to prewarm likely next
folders and adding time decay/recency weighting once the data proves useful.

## Rules

- Ant Trail records committed navigation, not mere hover.
- Expanding/collapsing a tree node is not a visit.
- Reading heat for paint must be cheap and in-memory.
- Persistence, decay, and prediction run outside paint.

## Data Flow

On startup:

1. Open `MetadataDb`.
2. Load `folder_usage` rows ordered by hits.
3. Build `ProcessState::ant_visits` and `ant_max`.
4. Build `ProcessState::recents` from the same rows ordered by last access.
5. Refresh active-tab heat tints once hydration completes.

On navigation commit:

1. Resolve the committed `NodeId` to a path at an action/job boundary.
2. Increment the in-memory hit count.
3. Promote the path to the front of Recents.
4. Update the `NodeStore` heat cache.
5. Spawn a background `record_folder_visit` write to SQLite.

On render:

- File rows read precomputed heat from the table delegate.
- Tree/sidebar surfaces read in-memory recents/heat state.
- No SQLite or filesystem work happens from paint.

## Heat Model

Heat is normalized to `0.0..=1.0` against the most-visited folder and log-scaled:

```text
log2(visits + 1) / log2(max_visits + 1)
```

That keeps one heavily used folder from washing out every other folder.

## Future Prediction

Ant Trail should eventually feed idle prefetch:

- likely child folders,
- parent/sibling folders,
- magic sniffing,
- previews,
- icon cache,
- disk usage summaries.

Keep decay disabled until there is evidence it improves daily navigation. A
simple cumulative count is predictable and easy to reason about.

## Mac Notes

- Heat keys should survive common Mac path churn where possible. Prefer stable
  file IDs/bookmarks later; path is acceptable for the first persistence slice.
- Network volumes and external drives need volume identity in the key.

## Remaining Work

Tracked in [TODO.md](../../TODO.md):

- Stable identity across rename/move/mount changes instead of path-only heat
  keys.
- Hot-folder query API.
- Prediction scheduler.
- Optional time decay / recency weighting.
- Debug overlay for heat and prefetch decisions.
- Tests for "navigation commit only" semantics.
