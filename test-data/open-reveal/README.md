# WCORPUS-OPEN — open / Reveal fixtures

The materialization of the `WCORPUS-OPEN` corpus from
[WINDOWS_RELIABILITY_TEST_PLAN.md](../../docs/testing/WINDOWS_RELIABILITY_TEST_PLAN.md)
(§3, used by WTEST-060…065): every fixture the double-click/default-open and
Reveal-in-Explorer acceptance matrix needs, generated deterministically.

## Use

```sh
python3 test-data/filename-hazards/generate.py           # (re)create
python3 test-data/filename-hazards/generate.py --clean   # remove
```

Everything except this README is generated and git-ignored — some names
(trailing space/dot, `CON.txt`, `NUL.png`) can only exist through the `\\?\`
namespace and upset ordinary tools, so the generator (reviewable plain text)
is what's versioned. The script prints the manifest checksum; record it with
the run's evidence per the acceptance plan.

## Layout

| Path | What it tests |
| --- | --- |
| `files/` | Plain names, one of each association: JPEG, PNG, PDF, TXT, MP4 (`ftyp` stub — the open verb, not the codec), WAV (genuinely playable), ZIP, `.cmd`, a no-association `.zzferail`, and a folder. Every file is small but *valid*, so the default app actually opens it. `photo-exif.jpg` additionally carries EXIF (camera, date taken, orientation, exposure, and a GPS latitude) for Get Info's Image section — the app must report GPS *presence only*, never the coordinates. |
| `names/` | The same bytes under difficult names: spaces, `#`, `%`, literal `%20`, `!`, `&`, `+`, `;`, `'`, `,`, `=`, `[]`, `{}`, `~`, `^`, `@`, accents, Greek, Cyrillic, CJK, emoji, combining accents, a 255-char component — plus `\\?\`-forced names Windows normally refuses (trailing space, trailing dot, `CON.txt`, `NUL.png`). |
| `dirs/` | Difficult *directory* names (open = navigate, and Reveal targets), each holding two openable files. |
| `long/` | An 8-level accented chain whose leaf paths exceed 260 chars (MAX_PATH), with three openable files at the bottom. |
| `manifest.txt` | Sorted relative paths; the count and SHA-256 the plan wants recorded. |

## The matrix

For each fixture: double-click, Enter, context-menu Open, and context-menu
Reveal. Open must launch the Windows default `open` verb (never Print, never
a mangled path); Reveal must select **exactly that item** in Explorer. Then
repeat a subset over UNC by reopening this folder via
`\\localhost\C$\<repo>\test-data\open-reveal\` — same fixtures, UNC identity.

The forced names in `names/` are the honesty check: Explorer itself struggles
with them, so the acceptance criterion is "Ferail does not crash, mis-target,
or silently do nothing — an explicit refusal with the reason is acceptable."
