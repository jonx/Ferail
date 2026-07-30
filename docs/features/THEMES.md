# Ferail — Themes & Color Customization

Design note for user-facing theming. Written to be implemented against
directly. Status: **planned** — Phase 0 (the selection-accent override seam)
has shipped; Phases 1–4 are the work this note scopes.

← Back to [Feature notes](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md)

---

## 1. Goal and scope

Let users change how Ferail looks without touching code:

1. **Pick a theme** from a small bundled set (beyond today's single light + dark
   palette).
2. **Drop in their own** theme as a JSON file and have it appear in the picker,
   hot-reloading on save.
3. **Override individual accents** (selection color today; a few more later) on
   top of whichever theme is active.

Non-goals for v1: a full in-app visual theme editor, importing other apps'
theme formats, and per-window themes. They're noted as future work in §8.

## 2. Current state (what exists today)

- The app uses **gpui-component's theme** (`cx.theme()` → `ThemeColor`): one
  light palette and one dark palette, both the library defaults. We never set a
  *named* theme.
- `app_state.theme_pref` holds the **mode** (`light` / `dark` / `system`).
  `settings.rs` (`ThemePref`, `persist_theme_pref`, the theme-tile strip) and
  `main.rs` (`Theme::change(mode, …)` at init) drive it. A background→UI bridge
  in `shell.rs` (`apply_pending_theme`) exists because the worker thread has no
  `&mut App`.
- **Phase 0 shipped (2026-06-20):** [`crate::selection_colors`] adds a
  `SelectionAccent(Option<Hsla>)` global (override-or-`theme.blue`), seeded from
  `app_state.selection_color`, edited by a `ColorPicker` on the Appearance page.
  The file list and icon grid both read its helpers, so selection is one hue
  across views and the user can recolor it live. This is the **override seam**
  the rest of this plan generalizes.

**The gap:** users can flip light/dark and recolor selection, but cannot pick a
different palette or supply their own.

## 3. What gpui-component already provides (verified)

We do **not** need to build a theme engine — the library ships one. Verified in
the pinned checkout (`crates/ui/src/theme/`):

- **`Theme`** global (`Theme::global(cx)` / `global_mut`): holds
  `light_theme: Rc<ThemeConfig>`, `dark_theme: Rc<ThemeConfig>`, and `mode`.
  `Theme::change(mode, window, cx)` re-applies the registry's default light/dark
  for that mode. To use a *named* theme, set `light_theme` / `dark_theme` to a
  registry `ThemeConfig` and re-apply.
- **`ThemeRegistry`** global, installed by `gpui_component::init(cx)` (already
  called at `main.rs:360`). It also installs a global observer that re-applies
  the active theme whenever the registry changes — so loading/adding themes
  repaints live. Key API:
  - `themes() -> HashMap<name, Rc<ThemeConfig>>`, `sorted_themes()`
  - `default_light_theme()` / `default_dark_theme()`
  - `load_themes_from_str(json)` — register themes from a JSON string
  - `watch_dir(dir, cx, on_load)` — background-load every theme file in a
    directory and **hot-reload** on change (off the UI thread)
- **`ThemeConfig`** = `{ name, mode: Light|Dark, colors: ThemeConfigColors,
  highlight? }`. **`ThemeSet`** = `{ name, themes: [ThemeConfig, …] }` — one file
  bundles a light + a dark variant. Both are serde JSON; the token vocabulary is
  shadcn/Tailwind-style (`background`, `foreground`, `accent`, `primary`,
  `table_active`, base scales like `blue` 50–950, …). `highlight` restyles the
  syntax-highlighted preview, so a theme can also recolor code.
- **`Colorize`** trait on `Hsla`: `opacity`, `lighten`, `darken`, `mix`,
  `mix_oklab`, `hue`, `invert`, `to_hex`, `parse_hex`. All the color math the
  override layer needs (Phase 0 already uses `parse_hex` / `to_hex` / `opacity`).
- **`ColorPicker` / `ColorPickerState`** — HSL sliders + palettes + hex input
  (already wired for the selection accent).
- **`Settings`** pages primitive — already hosts the Appearance page.

## 4. Design principles / constraints

- **Prime Directive (UI must never stop).** Theme *files* are I/O, so all
  reads happen off the paint path: `watch_dir` background-loads; persisted
  choice is read once at startup (an existing I/O boundary); **switching** a
  theme at runtime only reads the in-memory registry — no disk touch in the
  gesture.
- **Light and dark are separate `ThemeConfig`s.** A user-facing "theme" is a
  *pair*. Model a picked theme as a **set name** that supplies both a light and
  a dark variant; `mode` (Light/Dark/System) selects which is active. (Allowing
  independent light/dark choices is a later refinement — see §8 / §9.)
- **Overrides are a layer on top of the theme, not theme edits.** The selection
  accent (and future overrides) re-apply *after* a theme loads, so they survive
  theme switches. Each override is independently resettable to "follow theme."
- **Keep it small (Slow AI default).** Start with 2–3 curated bundled themes to
  prove the path; don't ship a sprawl. No new state-management or plugin layer —
  reuse the existing `app_state` key=value file and the `Global` pattern.

## 5. Data model & persistence

Add to `app_state` (flat `key=value`, unknown keys ignored — back/forward safe):

| Key | Type | Meaning |
| --- | --- | --- |
| `theme_pref` *(exists)* | `light\|dark\|system` | Active **mode**. |
| `theme_name` *(new)* | string | Chosen theme-set name. `None`/absent = library default. |
| `selection_color` *(exists)* | `#RRGGBB(AA)` | Selection-accent override. |

Phase 3 may add more override keys (e.g. `heat_tint`, `favorite_star`), each
`Option<hex>`, all behind the same parse/validate guard `selection_color`
already uses. Independent light/dark theme names (`theme_light_name` /
`theme_dark_name`) are reserved for §9 if we adopt that model.

## 6. Phased plan

### Phase 0 — Override seam *(shipped 2026-06-20)*

`selection_colors` module + `SelectionAccent` global + `ColorPicker`. Proves the
override → persist → live-repaint loop. See §2.

### Phase 1 — Bundled themes + theme picker

- **Bundle** 2–3 extra theme sets as JSON assets (alongside the existing asset
  pipeline in `assets.rs`). Each is a `ThemeSet` with a light + dark
  `ThemeConfig`. Suggested starters: one warm/sepia, one high-contrast, one
  cool/solarized-like — enough to show range without bloat.
- **Register** them right after `gpui_component::init(cx)` in `main.rs` via
  `ThemeRegistry::global_mut(cx).load_themes_from_str(json)` for each.
- **Apply persisted choice** at startup: if `theme_name` is set and present in
  the registry, set `Theme::global_mut(cx).{light,dark}_theme` to that set's
  variants, then `Theme::change(mode, …)`.
- **Picker UI:** an Appearance-page **Theme** dropdown listing
  `registry.sorted_themes()` set names (the existing Light/Dark/System tiles stay
  — they choose *mode*; the dropdown chooses *palette*). On select: update the
  `Theme` globals, persist `theme_name`, `cx.refresh_windows()`.
- **Live + persistent;** no relaunch. Mirrors the `persist_theme_pref` +
  `Theme::change` flow already in `settings.rs`.

### Phase 2 — User themes folder (drop-in custom JSON)

- On startup call
  `ThemeRegistry::watch_dir(<config>/Ferail/themes, cx, on_load)` (the same
  `config_dir()` `app_state` uses). Background-loads every theme file; user JSONs
  appear in the picker; saving a file hot-reloads it.
- **Affordances** on the Appearance page: "Open themes folder" (reveal in
  Finder) and, since the watcher already reloads, an optional "Reload themes"
  fallback.
- **Document the schema** for authors: write a `README` + an annotated example
  `ThemeSet` JSON into the folder on first run, and a short authoring section in
  this doc. Note the token vocabulary mirrors gpui-component's
  `ThemeConfigColors`.
- **Guard rails:** a malformed file must not crash or block — `watch_dir`
  already logs and skips on parse error; surface a quiet, non-blocking
  notification listing files that failed to load.

### Phase 3 — Generalize the override layer

- Promote `selection_colors` into a small **appearance-overrides** module that
  holds an `Option<Hsla>` per overridable role (selection accent now; candidates:
  Ant-Trail heat tint, favorite-star tint — both currently hard-coded). Each
  resolves to its theme-derived default when unset.
- **Re-apply order:** after any theme switch, re-seed the override globals so
  overrides ride on top of the new palette (today only one global, trivially
  re-seeded; formalize it as theme switching lands in Phase 1).
- **Settings:** one `ColorPicker` row per role under an "Accents" group, each
  with a "follow theme" clear. Keep the set short — overrides are for the few
  colors the theme can't express well (see the selection rationale: the theme's
  `table_active` is alpha-capped ≤ 0.2 and desaturated, which is *why* selection
  needs an override rather than a theme field).

### Phase 4 — Future / optional

- In-app theme **editor**: live-edit roles with `ColorPicker` rows over a working
  `ThemeConfig`, preview, export to JSON in the themes folder.
- **Import** converters from popular external theme formats → `ThemeConfig`.
- **Independent light/dark** theme selection (see §9).
- Per-syntax **highlight theme** picker (reuse `ThemeConfig.highlight`).

## 7. Settings UX (Appearance page, end state)

```
Theme
  ( ) Light   ( ) Dark   ( ) System         ← mode (exists)
  Theme:  [ Ferail Default ▾ ]            ← Phase 1 dropdown
  Custom themes folder:  [ Open ] [ Reload ] ← Phase 2

Accents
  Selection color   [■]  (clear)            ← Phase 0 (shipped); Phase 3 adds more
```

## 8. Integration points (files)

- `crates/ferail-gpui/src/main.rs` (~360–410) — register bundled themes after
  `gpui_component::init`; apply persisted `theme_name`; start `watch_dir`.
- `crates/ferail-gpui/src/settings.rs` — Theme dropdown + persistence (mirror
  `persist_theme_pref`); the Appearance page already owns the selection picker.
- `crates/ferail-gpui/src/shell.rs` — extend the `apply_pending_theme` bridge
  if a non-UI-thread path ever needs to switch themes (likely not for v1).
- `crates/ferail-gpui/src/app_state.rs` — `theme_name` (+ future override keys)
  field, parse arm, save line.
- `crates/ferail-gpui/src/assets.rs` — bundle the theme JSON assets.
- `crates/ferail-gpui/src/selection_colors.rs` — Phase 3 generalization.

## 9. Open questions (resolve before Phase 1 build)

1. **Pairing model:** pick a *set* (one name → light+dark), or choose light and
   dark **independently**? Recommend set-based for v1 (simpler picker, matches
   how people think); independent is a Phase 4 refinement. The registry tracks
   light/dark names separately, so either is supportable.
2. **How many bundled themes**, and which aesthetics? Recommend 2–3 curated.
3. **Highlight themes:** do bundled themes also restyle the code preview
   (`ThemeConfig.highlight`), or keep syntax colors constant across themes?
4. **Override breadth (Phase 3):** which roles beyond selection are worth a
   picker (heat tint, favorite star, …) vs. left to full themes?

## 10. Risks & trade-offs

- **Schema coupling:** custom themes are pinned to gpui-component's
  `ThemeConfig` schema; a dependency bump could rename/move tokens and break
  user files. Mitigation: document the pinned schema, fail soft (skip + notify),
  and treat a dep bump as a migration checkpoint.
- **Unreadable user themes:** we don't validate contrast; a bad custom theme can
  make the UI illegible. Mitigation: the bundled/default theme is always
  present and one click away; never let a custom theme become unremovable.
- **Watcher cost:** `watch_dir` adds a filesystem watcher, but it's off the UI
  thread and only over a small user folder — negligible, and consistent with the
  Prime Directive.

## 11. Testing & verification

- Extend the screenshot harness `--theme` flag to accept a **theme-set name**
  (today it takes `light`/`dark`); snapshot list + grid + Appearance under each
  bundled theme, light and dark.
- Headless caveat (same as elsewhere): the `ColorPicker` *popup* and any dialog
  animate over multiple paints and render faint in single-frame captures —
  verify the popup interactively; the closed picker button + applied colors are
  captured fine.
- Unit-test `app_state` round-trips for `theme_name` and override keys, and the
  hex validation guard.
