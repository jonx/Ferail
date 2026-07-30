#!/usr/bin/env python3
"""Generate a folder of sample filenames that exercise Ferail's filename
hazard detection (see crates/ferail-core/src/name_hazards.rs).

Each entry pairs a filename — often containing invisible or deceptive
characters written here as explicit \\u escapes — with a short note on the
trick it demonstrates. Run it to (re)create the `samples/` folder, then open
that folder in Ferail and select each file to see what Get Info flags.

    python3 test-data/filename-hazards/generate.py
"""

import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "samples")

# (filename, what it demonstrates)
SAMPLES = [
    ("clean_invoice.pdf", "Clean ASCII name — should NOT be flagged."),
    ("quarterly report v2.txt", "Interior spaces are normal — should NOT be flagged."),
    (" leading-space.txt", "Leading whitespace (amber)."),
    ("trailing-space.txt ", "Trailing whitespace (amber)."),
    ("tab\tinside.txt", "A literal TAB masquerading as a space (amber)."),
    ("no break space.txt", "Non-breaking spaces instead of real ones (amber)."),
    ("zero​width​split.exe", "Zero-width spaces hiding the real token (red)."),
    ("word⁠joiner.dll", "A word-joiner — invisible, splits a token (red)."),
    ("statement‮gpj.exe", "RLO bidi override: displays as 'statementexe.jpg' (red)."),
    ("раypal-login.exe", "Cyrillic 'р' and 'а' impersonating 'paypal' (red)."),
    ("gοοgle-update.exe", "Greek omicrons impersonating 'google' (red)."),
    ("ｆｕｌｌwidth.exe", "Fullwidth Latin letters mimicking 'full' (red)."),
    ("café_menu.txt", "A combining acute accent rather than precomposed 'é' (red)."),
    ("alertbell.log", "An embedded BEL control character (red)."),
    (
        "invoice​_рeal‭_final.exe",
        "Mixed: zero-width + Cyrillic homoglyph + bidi override (red).",
    ),
]


def main() -> None:
    os.makedirs(OUT, exist_ok=True)
    created = 0
    for name, note in SAMPLES:
        path = os.path.join(OUT, name)
        try:
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(f"Filename hazard sample.\nDemonstrates: {note}\n")
            created += 1
        except OSError as exc:  # some filesystems reject some names
            print(f"  skipped {name!r}: {exc}")
    print(f"Wrote {created}/{len(SAMPLES)} sample files into {OUT}")


if __name__ == "__main__":
    main()
