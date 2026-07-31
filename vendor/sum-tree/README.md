# Vendored `sum_tree` — GPL severance for redistributable binaries

This is zed's `sum_tree` crate (Apache-2.0), copied from the exact gpui rev this
workspace pins, with **one change**: the GPL-3.0 `ztracing` dependency removed.

## Why this exists

A stock build reaches GPL-3.0 code through a single non-optional edge:

```text
ferail-gpui → gpui → sum_tree → ztracing   (GPL-3.0-or-later)
                              → zlog       (GPL-3.0-or-later, dev-dep)
```

`ztracing` supplies `#[instrument]` macros that **no-op at runtime outside Zed's
own builds**, so nothing is lost by dropping them — but linking them into a
binary distributed under MIT/Apache-2.0 is a licence conflict. Publishing
*source* is unaffected; a redistributable *binary* is not. That made this a hard
blocker on shipping the Windows download.

`sum_tree` is the **only** consumer of `ztracing` in the whole dependency graph,
which is what makes this severable at all.

## The delta, in full

Nine lines, all mechanical:

| File | Change |
|---|---|
| `Cargo.toml` | dropped `ztracing`; dropped dev-deps (incl. GPL-3.0 `zlog`); workspace-inherited deps pinned explicitly |
| `src/cursor.rs` | removed `use ztracing::instrument;` + 4 × `#[instrument(skip_all)]` |
| `src/sum_tree.rs` | removed `use ztracing::instrument;` + 3 × `#[instrument(skip_all)]` |

No logic is touched. Verify with:

```sh
diff -r ~/.cargo/git/checkouts/zed-*/<rev>/crates/sum_tree/src vendor/sum-tree/src
```

## Wiring

The workspace `Cargo.toml` redirects the git-sourced crate here:

```toml
[patch."https://github.com/zed-industries/zed"]
sum_tree = { path = "vendor/sum-tree" }
```

This is an **in-repo relative path**, so it is present in every clone and on CI
— unlike a sibling-checkout path, which is what previously made `main`
unbuildable on any machine that lacked it.

Confirm the edge is gone (should print nothing):

```sh
cargo tree -p ferail-gpui -i ztracing
```

## Re-syncing on a gpui bump

This copy is pinned to one gpui rev. When that rev changes:

1. Diff upstream's `crates/sum_tree` against `src/` here.
2. Copy the new sources in; re-apply the deletions in the table above.
3. Re-pin any dependency whose version moved in zed's workspace.
4. Re-run `cargo tree -p ferail-gpui -i ztracing` — it must stay empty.

Track it in `CHANGELOG-DEPS.md` like any other pin bump.

## Upstream fix

Zed issue <https://github.com/zed-industries/zed/issues/55470> asks for the same
severance upstream. It is acknowledged but stuck in legal — do **not** assume it
lands on a timeline. If it does, delete this directory and the `[patch]` block.
