# Magic Sniffing

Purpose: identify real file types from content without trusting extensions.

## Status

Partial.

Feraille currently has:

- A small native detector in `feraille-fs-native`.
- `display_magic` on `FileEntry`.
- Magic column and sort support.
- Worker-based prefetch from the app, so navigation no longer blocks on file
  reads.
- A `(path, mtime)` in-memory cache.

## Nonblocking Contract

Magic sniffing reads file bytes. It must never run from paint, navigation
commit, selection, hover, scroll, or row drawing.

Allowed trigger points:

- After navigation commits.
- When a file row enters or nears the viewport.
- During idle prefetch.
- When a preview provider needs type data.

Every request must be cancellable or ignorable by generation id.

## Target

- Read a bounded prefix only.
- Limit concurrency.
- Cache by stable file identity plus mtime/size where possible.
- Persist results in the metadata DB.
- Feed icons, preview selection, duplicate grouping, search/filter, and treemap
  categorization.

## Mac Notes

- Some cloud files may fault in content on read. Treat magic sniffing as
  speculative and low priority.
- Extended attributes and Uniform Type Identifiers can complement magic, but
  asking the OS for them may also block. Cache and worker-boundary them.

## Todo

- Port a larger Ferail magic table.
- Add persistent cache.
- Add per-volume/cloud skip rules.
- Add tests for stale result dropping.
- Add a debug overlay showing queued/running magic jobs.
