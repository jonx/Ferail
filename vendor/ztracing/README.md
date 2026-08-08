# vendor/ztracing — clean-room no-op stub

## Why this exists

Zed's `ztracing` crate is **GPL-3.0-or-later**, and as of zed `00cba838a`
(2026-08-05, PR #62115) `gpui` depends on it **directly** — previously the
only edge was `gpui → sum_tree → ztracing`, which we severed with a vendored
`sum_tree` (see git history for `vendor/sum-tree`). `ztracing` also pulls
`ztracing_macro` and `zlog`, both GPL-3.0-or-later. Linking GPL object code
into Ferail's MIT/Apache-2.0 binaries is a licence conflict, so a
redistributable build has to keep all three out of the graph.

This crate is patched over the zed source in the root `Cargo.toml`:

```toml
[patch."https://github.com/zed-industries/zed"]
ztracing = { path = "vendor/ztracing" }
```

With the patch in place, `ztracing_macro` and `zlog` (whose only path into
the graph is upstream `ztracing`) drop out entirely, and the previously
vendored `sum_tree` fork became unnecessary — upstream `sum_tree`'s
`ztracing` dependency now resolves to this stub too.

Verify after any gpui bump:

```sh
cargo tree -p ferail-gpui -i ztracing   # must print ONLY vendor/ztracing (path)
cargo tree -p ferail-gpui -i zlog       # must print nothing
cargo tree -p ferail-gpui -i ztracing_macro   # must print nothing
```

## Why a stub is safe and equivalent

Upstream `ztracing` is a compile-time switch on Zed's private `--cfg
ztracing` profiling flag, which Ferail builds never set. Without that cfg,
the upstream crate's entire public surface is **already no-ops**: an
`#[instrument]` attribute that emits the item unchanged, span/event macros
that swallow their tokens and yield an inert `Span`, and an `init()` that
does nothing. This stub reproduces that no-op contract — behaviourally
identical to what we were linking before, minus the GPL provenance.

## Clean-room status

Written against the public API contract (the names and signatures consumers
compile against), not copied from the GPL source. The API surface is:

- `#[instrument]` (attribute proc-macro in `macros/`, accepts any args)
- `trace_span!` `debug_span!` `info_span!` `warn_span!` `error_span!`
  `span!` `event!` — token-swallowing macros yielding `Span`
- `struct Span` with `current()`, `enter()`, `record(key, value)`
- `pub use tracing::{Level, field}` (`tracing` is MIT — a real, permissive
  dependency, re-exported so consumer code naming those items compiles)
- `fn init()`

## Re-sync procedure on a gpui bump

There is nothing to re-sync from upstream — this is not a fork. If a bump
fails to compile inside `gpui` or `sum_tree` with an unresolved `ztracing::…`
item, upstream grew its API: add the missing name here **as a no-op**, in the
spirit above. If upstream ever starts requiring real tracing behaviour
unconditionally, stop and reassess — do not port GPL code into this crate.
