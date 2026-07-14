# Making a Rule Non-Negotiable for a Coding Agent

← Back to [feature notes](README.md) · [Architecture](../ARCHITECTURE.md)

A method for turning a project principle into something an AI coding agent
(or a tired human) cannot accidentally violate. Written after hardening
Feraille's Prime Directive ("the UI must never stop"); the layers below are
generic — swap in your own rule.

## The core idea

**Prose is negotiable. Consequences are not.** An agent skims documentation,
pattern-matches on nearby code, and rationalizes exceptions ("it's fast on my
machine", "it's just one stat call"). A rule becomes non-negotiable only when
violating it produces a mechanical failure — a lint error, a debug panic, a
failing check — at the cheapest possible stage, and when that failure message
itself teaches the rule. Documentation explains; enforcement enforces. You
need both, plus a closed loop between them: every tripwire points back at the
doctrine, and the doctrine names every tripwire.

## The seven layers

1. **One canonical statement, everywhere else pointers.** State the rule once,
   in the file the agent is guaranteed to read first (`CLAUDE.md`, the system
   prompt, the top of `ARCHITECTURE.md`). Say explicitly that it is
   non-negotiable and what it outranks ("feature completeness, code brevity,
   every convenience"). All other mentions link to it — copies drift, and a
   stale copy is ammunition for rationalization.

2. **Name the failure mode and the innocent-looking violations.** Agents bend
   rules they think don't apply. Preempt the rationalization in the text:
   *why* the rule exists (a stat call that's microseconds on an SSD is seconds
   on a spun-down drive) and a list of calls that *look* harmless but violate
   it (`Path::exists`, `metadata`, a watcher registration that canonicalizes
   internally). A rule stated without its failure mode gets "reasonably"
   violated.

3. **Give the sanctioned path a name.** A ban without an alternative gets
   worked around. Point at one canonical in-repo example to copy
   (`Shell::load_path_for_tab` here) and provide sanctioned wrappers for the
   banned calls (`canonicalize_for_identity`, the `FsWatcher` command
   channel). Agents learn by pattern-matching — give them the right pattern.

4. **Static wall: ban the calls mechanically.** Use the linter's
   disallowed-call machinery (clippy `disallowed-methods`, ESLint
   `no-restricted-imports/syntax`, semgrep) scoped to exactly where the rule
   applies (per-crate/per-package config, not repo-wide — worker code may do
   legitimately what UI code must not). Keep the list short and high-signal;
   banning everything buries the point under a hundred `allow`s. Make the
   lint's `reason` string teach: name the hazard, the sanctioned alternative,
   and the doctrine section. Legitimate exceptions get a **per-site** allow
   with a justification comment — the annotation *is* the review marker.
   Never allow crate-wide.

5. **Runtime tripwire: assert the invariant where the lint can't see it.**
   Static analysis can't tell which *thread* or *phase* code runs in. Mark
   the protected context at startup (`mark_ui_thread()`, `enter_render()`),
   then make the dangerous entry points assert
   (`assert_off_ui_thread("enumerate_streaming")`) — panicking in debug
   builds with a message that states the fix ("schedule it on the background
   executor…") and cites the doctrine. Crucially, forbid the easy out **in
   the message and the docs**: "never fix a guard panic by removing the
   guard." Agents fix symptoms; name the forbidden symptom-fix in advance.

6. **Wire enforcement into the finishing ritual.** Whatever checklist the
   agent runs before declaring done (the "Verification" section of
   `CLAUDE.md`, CI, a pre-commit hook) must include the enforcement commands
   (`cargo clippy -p <ui-crate>`, `cargo test` with debug assertions on). A
   tripwire outside the default loop doesn't exist. Debug runs and screenshot
   harnesses double as guard exercises for free.

7. **Keep an honest ledger of known violations.** After an audit, surviving
   exceptions go in TODO with the reason they're deferred. Without the
   ledger, the first existing violation an agent finds reads as precedent
   ("the rule is aspirational"); with it, the same discovery reads as known
   debt with a plan.

## Order of operations when applying this to a new rule

1. Write the canonical section (layers 1–3) — rule, why, hazards, sanctioned
   pattern.
2. **Audit before arming.** Sweep the codebase for existing violations (fan
   out review agents if large); fix what's practical, ledger the rest.
   Arming tripwires before auditing turns your debug builds into a minefield
   of pre-existing panics.
3. Add the static wall, run it, annotate the legitimate sites.
4. Add the runtime guards to the highest-value entry points; boot the app /
   run the suite to prove the normal path is clean.
5. Add the enforcement commands to the verification ritual.

## What *not* to do

- **Don't build a wrapper interface and hope.** A wrapper alone forbids
  nothing — the raw calls remain reachable. Wrappers are layer 3 (the
  sanctioned path); layers 4–5 are what make bypassing them fail.
- **Don't ban broadly.** 137 call sites needing `#[allow]` is noise that
  trains everyone to add allows reflexively. Six high-risk calls with sharp
  reasons is a wall.
- **Don't rely on the agent reading deeply.** The rule must be in the
  first-read file, and every enforcement message must carry enough context
  to act on without further reading.

## The Feraille instantiation (worked example)

| Layer | Artifact |
| --- | --- |
| Canonical statement | `CLAUDE.md` § Prime Directive → `docs/ARCHITECTURE.md#prime-directive` |
| Failure modes named | Slow-media list in both docs (exists/metadata/canonicalize/watch) |
| Sanctioned path | `Shell::load_path_for_tab` pattern; `canonicalize_for_identity`; `FsWatcher` worker |
| Static wall | `crates/feraille-gpui/clippy.toml` + `disallowed_methods = "deny"` |
| Runtime tripwire | `feraille_core::path_guard` — render guard + `assert_off_ui_thread` |
| Finishing ritual | `CLAUDE.md` § Verification (clippy line) |
| Ledger | `TODO.md` § Responsiveness — "known remaining UI-thread I/O" |
