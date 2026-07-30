# Third-Party Notices

Ferail's own source is dual-licensed under **MIT OR Apache-2.0**
([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). A built
Ferail binary also incorporates third-party components whose licenses
require their copyright and permission notices to travel with redistributed
copies. Those notices are collected here.

This file covers the components that are **compiled or embedded into the
shipped binary**. The complete transitive Rust dependency graph (hundreds of
crates, overwhelmingly MIT and/or Apache-2.0) is pinned in
[`Cargo.lock`](Cargo.lock); to regenerate a full per-crate license listing,
run [`cargo about`](https://github.com/EmbarkStudios/cargo-about) or
[`cargo bundle-licenses`](https://github.com/sstadick/cargo-bundle-licenses)
over the workspace.

---

## GPUI and GPUI Component (Apache-2.0)

The UI framework and component library. Each is licensed Apache-2.0; the
Apache-2.0 license text is reproduced in [LICENSE-APACHE](LICENSE-APACHE).
Per Apache-2.0 §4(d), the upstream attribution notices are preserved below.

- **gpui**, **gpui_platform** — from the Zed editor project.
  <https://github.com/zed-industries/zed>
  Copyright © 2022–2025 Zed Industries, Inc. Licensed under Apache-2.0.
  (The `gpui` crate is deliberately licensed Apache-2.0, separate from the
  GPL-licensed Zed editor crates in the same repository.)

- **gpui-component**, **gpui-component-assets** — the UI primitives and the
  bundled icon assets. <https://github.com/longbridge/gpui-component>
  Copyright © 2024–2025 Longbridge. Licensed under Apache-2.0.

### Transitive GPL-3.0 components (via gpui → sum_tree)

Although `gpui` itself is Apache-2.0, a default build currently links three
small **GPL-3.0-or-later** crates from the Zed repository through a single
non-optional dependency edge:

```
gpui → sum_tree → ztracing → { zlog, ztracing_macro }
```

These supply `#[instrument]` tracing macros that are compiled in but **no-op at
runtime** in non-Zed builds (the `ztracing` cfg is never enabled). They are
recorded here for completeness, because GPL-3.0 object code linked into a
*redistributed binary* carries copyleft obligations on that binary.

- **ztracing**, **zlog**, **ztracing_macro** — from the Zed editor project.
  <https://github.com/zed-industries/zed> Licensed under GPL-3.0-or-later.

Ferail does **not** currently distribute prebuilt binaries: the published
source references these crates via pinned git dependencies but does not itself
redistribute their code, so the MIT/Apache-2.0 grant on Ferail's own source is
unaffected. The upstream license inconsistency (Apache-2.0 `sum_tree` depending
on GPL-3.0 `ztracing`) is tracked at
<https://github.com/zed-industries/zed/issues/55470>. If a redistributable
binary is shipped before that is resolved upstream, this single dependency edge
is severed by a local patch so the binary remains MIT/Apache.

---

## Icon artwork

Ferail embeds ~41 SVG glyphs in `crates/ferail-gpui/resources/icons/` and
references the `gpui-component-assets` icon bundle at runtime. Their provenance
and per-glyph mapping are catalogued in
[docs/features/ICONS.md](docs/features/ICONS.md).

### Lucide (ISC License)

Most embedded glyphs, and the upstream `gpui-component-assets` bundle, derive
from [Lucide](https://lucide.dev). Local copies are re-saved at
`stroke-width="1.75"`; the artwork is unchanged.

```
ISC License

Copyright (c) 2020, Lucide Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

### Bootstrap Icons (MIT License)

One glyph (`resources/icons/nav/cloud.svg`) is from
[Bootstrap Icons](https://icons.getbootstrap.com).

```
The MIT License (MIT)

Copyright (c) 2019-2024 The Bootstrap Authors

Permission is hereby granted, free of charge, to any person obtaining a
copy of this software and associated documentation files (the "Software"),
to deal in the Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, sublicense,
and/or sell copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

### Apple system icons (not redistributed)

On macOS, folder and file-type artwork is fetched at runtime from the system
via `NSWorkspace`/`IconForFile`. This Apple artwork is **never bundled or
redistributed** with Ferail — it is read from the user's own OS at display
time — so no Apple artwork ships in the binary.

---

## libmpv (optional video player — not bundled)

The optional `mpv` build feature (off by default) plays video through
**libmpv** (LGPL-2.1-or-later / GPL depending on build). Ferail does **not**
link, bundle, or redistribute libmpv: when the feature is enabled it loads a
**user-installed** libmpv at runtime via `dlopen`/`LoadLibraryW` from a path the
user supplies (or a system install such as Homebrew's). A default Ferail build
contains no mpv code at all. If you distribute a binary built `--features mpv`,
note that video playback relies on a separately-installed libmpv that Ferail
does not ship.
