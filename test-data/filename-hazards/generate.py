#!/usr/bin/env python3
"""Generate Ferail's filename test fixtures.

Two independent fixture sets, both deterministic and stdlib-only:

1. `samples/` (next to this script) — deceptive filenames that exercise
   Ferail's filename hazard detection (see
   crates/ferail-core/src/name_hazards.rs). Each entry pairs a filename —
   often containing invisible or deceptive characters written here as
   explicit \\u escapes — with a short note on the trick it demonstrates.
   Open the folder in Ferail and select each file to see what Get Info
   flags.

2. `../open-reveal/` — the `WCORPUS-OPEN` corpus from
   docs/testing/WINDOWS_RELIABILITY_TEST_PLAN.md: small, genuinely
   openable files of the associated types (JPEG, PNG, PDF, TXT, WAV,
   archive, script, no-association), plus copies under difficult names
   (Unicode, spaces, `#`, `%`, `!`, `&`, …), difficult directory names,
   Windows-only forced names (trailing space/dot, reserved device names),
   and a nested chain whose absolute paths exceed 260 characters. Used by
   the open / Reveal acceptance matrix (WTEST-060…065) and by hand tests
   of the same paths over UNC (`\\\\localhost\\C$\\…`).

    python3 test-data/filename-hazards/generate.py           # (re)create both
    python3 test-data/filename-hazards/generate.py --clean   # remove generated trees

Regeneration always deletes and recreates the generated trees, so counts
and the printed manifest checksum are reproducible. A `manifest.txt`
(sorted relative paths) is written into the corpus root; the acceptance
plan wants its checksum recorded with the evidence.
"""

import base64
import hashlib
import os
import shutil
import struct
import sys
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "samples")
OPEN_ROOT = os.path.join(os.path.dirname(HERE), "open-reveal")

# ---------------------------------------------------------------------------
# Set 1 — filename hazard samples
# ---------------------------------------------------------------------------

# (filename, what it demonstrates)
SAMPLES = [
    ("clean_invoice.pdf", "Clean ASCII name — should NOT be flagged."),
    ("quarterly report v2.txt", "Interior spaces are normal — should NOT be flagged."),
    (" leading-space.txt", "Leading whitespace (amber)."),
    ("trailing-space.txt ", "Trailing whitespace (amber)."),
    ("tab\tinside.txt", "A literal TAB masquerading as a space (amber)."),
    ("no break space.txt", "Non-breaking spaces instead of real ones (amber)."),
    ("zero​width​split.exe", "Zero-width spaces hiding the real token (red)."),
    ("word⁠joiner.dll", "A word-joiner — invisible, splits a token (red)."),
    ("statement‮gpj.exe", "RLO bidi override: displays as 'statementexe.jpg' (red)."),
    ("раypal-login.exe", "Cyrillic 'р' and 'а' impersonating 'paypal' (red)."),
    ("gοοgle-update.exe", "Greek omicrons impersonating 'google' (red)."),
    ("ｆｕｌｌwidth.exe", "Fullwidth Latin letters mimicking 'full' (red)."),
    ("café_menu.txt", "A combining acute accent rather than precomposed 'é' (red)."),
    ("alertbell.log", "An embedded BEL control character (red)."),
    (
        "invoice​_рeal‭_final.exe",
        "Mixed: zero-width + Cyrillic homoglyph + bidi override (red).",
    ),
]


def write_hazard_samples() -> None:
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


# ---------------------------------------------------------------------------
# Set 2 — WCORPUS-OPEN: open / Reveal difficult-path corpus
# ---------------------------------------------------------------------------

# 1×1 blue-grey baseline JPEG (633 bytes, GDI+-produced and verified to
# decode). Kept as data because JPEG entropy coding is not worth
# reimplementing for a fixture.
JPEG_1PX = base64.b64decode(
    "/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoH"
    "BwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQME"
    "BAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU"
    "FBQUFBQUFBQUFBQUFBT/wAARCAABAAEDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEA"
    "AAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIh"
    "MUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6"
    "Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZ"
    "mqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx"
    "8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREA"
    "AgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAV"
    "YnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hp"
    "anN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPE"
    "xcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDn"
    "6KKK/cz8cP/Z"
)


def png_1px() -> bytes:
    """A valid 1×1 opaque white PNG, built from spec primitives."""

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)  # 1×1, 8-bit RGB
    idat = zlib.compress(b"\x00\xff\xff\xff", 9)  # filter 0 + one white pixel
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", idat)
        + chunk(b"IEND", b"")
    )


def pdf_minimal() -> bytes:
    """One A6 page with visible text and a correct xref table — preview
    handlers render something recognizably non-blank."""
    content = (
        b"BT /F1 24 Tf 30 370 Td (WCORPUS-OPEN) Tj "
        b"0 -36 Td (fixture PDF) Tj ET\n"
    )
    objects = [
        b"<</Type/Catalog/Pages 2 0 R>>",
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>",
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 298 420]"
        b"/Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R>>",
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>",
        b"<</Length %d>>\nstream\n%s\nendstream" % (len(content), content),
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += b"%d 0 obj\n%s\nendobj\n" % (i, body)
    xref_at = len(out)
    out += b"xref\n0 %d\n" % (len(objects) + 1)
    out += b"0000000000 65535 f \n"
    for off in offsets:
        out += b"%010d 00000 n \n" % off
    out += (
        b"trailer\n<</Size %d/Root 1 0 R>>\nstartxref\n%d\n%%%%EOF\n"
        % (len(objects) + 1, xref_at)
    )
    return bytes(out)


def wav_beep() -> bytes:
    """0.25 s of a 440 Hz square wave — small, valid, audibly openable."""
    rate, secs, freq, amp = 8000, 0.25, 440, 12000
    n = int(rate * secs)
    frames = bytearray()
    for i in range(n):
        v = amp if (i * freq * 2 // rate) % 2 == 0 else -amp
        frames += struct.pack("<h", v)
    hdr = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF", 36 + len(frames), b"WAVE", b"fmt ", 16, 1, 1,
        rate, rate * 2, 2, 16, b"data", len(frames),
    )
    return hdr + bytes(frames)


# `ftyp isom` box only. Enough for association/open-verb tests; players
# will report an unplayable file, which is fine — the *path handling* is
# what WCORPUS-OPEN exercises, not the codec.
MP4_STUB = bytes.fromhex(
    "0000002066747970" "69736f6d" "00000200" "69736f6d69736f3261766331" "6d703431"
)

NOTE_TXT = (
    "WCORPUS-OPEN fixture (see docs/testing/WINDOWS_RELIABILITY_TEST_PLAN.md).\n"
    "Double-click must open the Windows default app; Reveal must select\n"
    "exactly this file in Explorer, whatever the path contains.\n"
)

CMD_TEXT = (
    "@echo off\r\n"
    "echo WCORPUS-OPEN: the open verb reached this script.\r\n"
    "pause\r\n"
)


def zip_fixture() -> bytes:
    """A deterministic one-entry STORED zip (fixed timestamp)."""
    import io

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as zf:
        info = zipfile.ZipInfo("readme.txt", date_time=(2020, 1, 1, 0, 0, 0))
        zf.writestr(info, NOTE_TXT)
    return buf.getvalue()


def base_files() -> "dict[str, bytes]":
    return {
        "photo.jpg": JPEG_1PX,
        "image.png": png_1px(),
        "document.pdf": pdf_minimal(),
        "notes.txt": NOTE_TXT.encode("utf-8"),
        "clip.mp4": MP4_STUB,
        "sound.wav": wav_beep(),
        "archive.zip": zip_fixture(),
        "hello.cmd": CMD_TEXT.encode("utf-8"),
        "no-association.zzferail": NOTE_TXT.encode("utf-8"),
    }


# Difficult *file* names → which base file's bytes they carry. Covers the
# handover matrix (Unicode, spaces, #, %, !) plus the shell-quoting and
# URL-encoding classics that broke Explorer `/select,` strings.
HAZARD_FILE_NAMES = [
    ("espaces  doubles  intérieurs.jpg", "photo.jpg"),
    ("dièse#numéro#1.pdf", "document.pdf"),
    ("pourcent%20piège.txt", "notes.txt"),  # literal %20 — URL-decode trap
    ("pourcent 100%.png", "image.png"),
    ("exclamation!fort!.mp4", "clip.mp4"),
    ("esperluette & co.txt", "notes.txt"),
    ("plus+plus.pdf", "document.pdf"),
    ("point-virgule;fin.txt", "notes.txt"),
    ("apostrophe'simple.jpg", "photo.jpg"),
    ("virgule,liste.txt", "notes.txt"),
    ("égal=signe.png", "image.png"),
    ("crochets [v2].pdf", "document.pdf"),
    ("accolades {v3}.txt", "notes.txt"),
    ("tilde~1.txt", "notes.txt"),
    ("chapeau^circonflexe.txt", "notes.txt"),
    ("arobase@courriel.txt", "notes.txt"),
    ("café été à côté.jpg", "photo.jpg"),
    ("grec αβγδ.txt", "notes.txt"),
    ("cyrillique файл.pdf", "document.pdf"),
    ("cjk 漢字テスト.png", "image.png"),
    ("emoji 🎉🚀.txt", "notes.txt"),
    ("combining e\u0301e\u0301.pdf", "document.pdf"),
    # Maximum-length single component (255 chars incl. extension).
    ("n" * 251 + ".txt", "notes.txt"),
]

# Windows refuses these through normal Win32 paths; created via the \\?\
# namespace so Reveal/open must cope with what other tools do create.
FORCED_FILE_NAMES = [
    ("espace-final.txt ", "notes.txt"),  # trailing space
    ("point-final.txt.", "notes.txt"),  # trailing dot
    ("CON.txt", "notes.txt"),  # reserved device name
    ("NUL.png", "image.png"),  # reserved device name
]

# Difficult *directory* names — open (navigate) and Reveal targets.
HAZARD_DIR_NAMES = [
    "dossier avec espaces",
    "dossier#dièse",
    "dossier 100%",
    "dossier!exclamation",
    "dossier & fils",
    "dossier café 漢字 🎉",
]

# Nested chain pushing absolute paths well past MAX_PATH (260).
LONG_COMPONENT = "chemin très long niveau %02d avec accents éàü"
LONG_DEPTH = 8


def _w(path: str) -> str:
    """Long/forced-path-safe form of an absolute path on Windows."""
    if os.name == "nt" and not path.startswith("\\\\?\\"):
        return "\\\\?\\" + os.path.abspath(path)
    return path


def _write(path: str, data: bytes) -> bool:
    try:
        with open(_w(path), "wb") as fh:
            fh.write(data)
        return True
    except OSError as exc:
        print(f"  skipped {os.path.basename(path)!r}: {exc}")
        return False


GENERATED = ["files", "names", "dirs", "long", "manifest.txt"]


def clean_open_corpus() -> None:
    for child in GENERATED:
        path = os.path.join(OPEN_ROOT, child)
        if os.path.isdir(_w(path)):
            shutil.rmtree(_w(path))
        elif os.path.exists(_w(path)):
            os.remove(_w(path))


def write_open_corpus() -> None:
    clean_open_corpus()
    base = base_files()
    created = 0

    # files/ — the plain-name association set, plus one plain folder.
    files_dir = os.path.join(OPEN_ROOT, "files")
    os.makedirs(_w(os.path.join(files_dir, "plain-folder")), exist_ok=True)
    for name, data in base.items():
        created += _write(os.path.join(files_dir, name), data)
    created += _write(
        os.path.join(files_dir, "plain-folder", "inside.txt"),
        NOTE_TXT.encode("utf-8"),
    )

    # names/ — the same bytes under difficult names.
    names_dir = os.path.join(OPEN_ROOT, "names")
    os.makedirs(_w(names_dir), exist_ok=True)
    for name, src in HAZARD_FILE_NAMES:
        created += _write(os.path.join(names_dir, name), base[src])
    for name, src in FORCED_FILE_NAMES:
        created += _write(os.path.join(names_dir, name), base[src])

    # dirs/ — difficult directory names, each with two openable files.
    dirs_dir = os.path.join(OPEN_ROOT, "dirs")
    for dname in HAZARD_DIR_NAMES:
        d = os.path.join(dirs_dir, dname)
        os.makedirs(_w(d), exist_ok=True)
        created += _write(os.path.join(d, "notes.txt"), base["notes.txt"])
        created += _write(os.path.join(d, "photo.jpg"), base["photo.jpg"])

    # long/ — depth chain; absolute paths at the bottom exceed 260 chars.
    long_dir = os.path.join(OPEN_ROOT, "long")
    for i in range(LONG_DEPTH):
        long_dir = os.path.join(long_dir, LONG_COMPONENT % i)
    os.makedirs(_w(long_dir), exist_ok=True)
    created += _write(os.path.join(long_dir, "notes.txt"), base["notes.txt"])
    created += _write(os.path.join(long_dir, "photo.jpg"), base["photo.jpg"])
    created += _write(
        os.path.join(long_dir, "au bout du chemin #final 100%.pdf"),
        base["document.pdf"],
    )
    depth_note = len(os.path.abspath(os.path.join(long_dir, "notes.txt")))

    # manifest.txt — sorted relative paths; checksum goes into evidence.
    rels = []
    for walk_root, _dirs, files in os.walk(_w(OPEN_ROOT)):
        for f in files:
            if f == "manifest.txt" or f == "README.md":
                continue
            abs_path = os.path.join(walk_root, f)
            rel = os.path.relpath(abs_path, _w(OPEN_ROOT))
            rels.append(rel.replace(os.sep, "/"))
    rels.sort()
    manifest = "".join(r + "\n" for r in rels).encode("utf-8")
    _write(os.path.join(OPEN_ROOT, "manifest.txt"), manifest)
    digest = hashlib.sha256(manifest).hexdigest()

    print(f"Wrote {created} WCORPUS-OPEN files into {OPEN_ROOT}")
    print(f"  deepest fixture path: {depth_note} chars (> 260 exercises MAX_PATH)")
    print(f"  manifest: {len(rels)} files, sha256 {digest}")
    print("  UNC pass: reopen the same folder via \\\\localhost\\C$\\...")


def main() -> None:
    if "--clean" in sys.argv[1:]:
        clean_open_corpus()
        if os.path.isdir(OUT):
            shutil.rmtree(_w(OUT))
        print("Removed generated fixture trees.")
        return
    write_hazard_samples()
    write_open_corpus()


if __name__ == "__main__":
    main()
