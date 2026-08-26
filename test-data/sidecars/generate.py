#!/usr/bin/env python3
"""Generate deterministic NFO/SFV/checksum fixtures.

Binary/text-encoding fixtures are generated rather than hand-edited so Git,
editors and platform line-ending conversion cannot silently change them.
"""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import shutil
import zlib


ROOT = Path(__file__).resolve().parent
OUT = ROOT / "generated"


def write_bytes(relative: str, data: bytes) -> None:
    path = OUT / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def write_text(relative: str, text: str, *, newline: str = "\n") -> None:
    write_bytes(relative, text.replace("\n", newline).encode("utf-8"))


def digest(name: str, data: bytes) -> str:
    return hashlib.new(name, data).hexdigest()


def ferail_release_nfo() -> bytes:
    """A synthetic ANSI/CP437 release NFO with no identifying metadata."""
    esc = "\x1b["
    reset = f"{esc}0m"
    width = 76

    def color(code: str, text: str) -> str:
        return f"{esc}{code}m{text}{reset}"

    def box(text: str = "", code: str = "37") -> str:
        padding = width - len(text)
        left = padding // 2
        right = padding - left
        return (
            color("38;5;45", "║")
            + (" " * left)
            + color(code, text)
            + (" " * right)
            + color("38;5;45", "║")
            + "\r\n"
        )

    logo = (
        "███████╗███████╗██████╗  █████╗ ██╗██╗",
        "██╔════╝██╔════╝██╔══██╗██╔══██╗██║██║",
        "█████╗  █████╗  ██████╔╝███████║██║██║",
        "██╔══╝  ██╔══╝  ██╔══██╗██╔══██║██║██║",
        "██║     ███████╗██║  ██║██║  ██║██║███████╗",
        "╚═╝     ╚══════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝╚══════╝",
    )
    lines = [
        f"{esc}2J{esc}H",
        color("38;5;45", "╔" + "═" * width + "╗") + "\r\n",
        box("[ FERAIL // RELEASE INTELLIGENCE ]", "1;38;5;213"),
        box(),
    ]
    logo_colors = ("38;5;51", "38;5;45", "38;5;39", "38;5;33", "38;5;99", "38;5;135")
    # Centre the logo as one rectangle. Centring each FIGlet row separately
    # shifts its longer baseline rows left and makes the foot of the F (and
    # the other letters) look detached.
    logo_width = max(len(row) for row in logo)
    lines.extend(
        box(row.ljust(logo_width), logo_colors[index])
        for index, row in enumerate(logo)
    )
    lines.extend(
        [
            box(),
            box("root@ferail:~$ ./ferail --release-info", "1;32"),
            box("FAST FILE OPERATIONS // BUILT FOR THE BIG DIRECTORIES", "1;37"),
            box(),
            box("[+] CONTENT-FIRST NFO PREVIEW", "38;5;82"),
            box("[+] SFV + SHA-256 VERIFICATION", "38;5;82"),
            box("[+] ANSI / CP437, RENDERED LOCALLY", "38;5;82"),
            box("[+] NO UPLOADS // NO TELEMETRY // YOUR FILES STAY YOURS", "38;5;82"),
            box(),
            box("-- GREETINGS TO MUMU --", "1;38;5;213"),
            box("KEEP IT FAST. KEEP IT LOCAL.", "38;5;220"),
            box(),
            box("EOF // FERAIL", "2;37"),
            color("38;5;45", "╚" + "═" * width + "╝") + reset + "\r\n",
        ]
    )
    return "".join(lines).encode("cp437")


def generate() -> None:
    if OUT.exists():
        shutil.rmtree(OUT)

    payloads = {
        "alpha.bin": b"Ferail sidecar fixture: alpha\n",
        "file with spaces.txt": b"spaces are valid in manifest filenames\n",
        "unicodé.txt": "Unicode filenames remain Unicode.\n".encode(),
        "subdir/nested.dat": bytes(range(64)),
        "back\\slash.txt": b"A literal backslash is valid on Unix.\n",
        "line\nbreak.txt": b"This filename cannot be represented by SFV safely.\n",
    }
    # Backslash and newline names exist only as parser records below. They are
    # legal on some Unix filesystems but cannot be checked out or generated as
    # ordinary Win32 names, so the tracked corpus itself stays cross-platform.
    materialized = ("alpha.bin", "file with spaces.txt", "unicodé.txt", "subdir/nested.dat")
    for name in materialized:
        data = payloads[name]
        write_bytes(f"payload/{name}", data)
    for name in materialized:
        write_bytes(f"release/{name}", payloads[name])

    sfv_lines = [
        "; Generated deterministic Ferail fixture",
        "; filename CRC32",
    ]
    for name in materialized:
        sfv_lines.append(f"{name} {zlib.crc32(payloads[name]) & 0xFFFFFFFF:08X}")
    write_bytes("manifests/release.sfv", ("\r\n".join(sfv_lines) + "\r\n").encode("cp437"))
    write_bytes("release/release.sfv", ("\r\n".join(sfv_lines) + "\r\n").encode("cp437"))
    write_text(
        "release/problems.sfv",
        # Distinct, recognizable dummy CRC32 values make the Expected column
        # self-explanatory in manual UI fixtures. None is presented as a real
        # checksum for the corresponding target.
        "alpha.bin DEADBEEF\nmissing.bin CAFEBABE\n../outside.bin BAD0C0DE\n",
    )

    sha_lines = []
    for name in materialized:
        sha_lines.append(f"{digest('sha256', payloads[name])} *{name}")
    # GNU marks escaped records with a leading backslash and escapes the name.
    backslash_payload = payloads["back\\slash.txt"]
    newline_payload = payloads["line\nbreak.txt"]
    sha_lines.append(f"\\{digest('sha256', backslash_payload)} *back\\\\slash.txt")
    sha_lines.append(f"\\{digest('sha256', newline_payload)} *line\\nbreak.txt")
    write_text("manifests/SHA256SUMS", "\n".join(sha_lines) + "\n")
    write_text("release/SHA256SUMS", "\n".join(sha_lines[:4]) + "\n")

    write_text(
        "manifests/MD5SUMS",
        f"{digest('md5', payloads['alpha.bin'])} *alpha.bin\n",
    )
    write_text(
        "manifests/SHA1SUMS",
        f"{digest('sha1', payloads['alpha.bin'])} *alpha.bin\n",
    )
    write_text(
        "manifests/BSD-SHA256",
        f"SHA256 (alpha.bin) = {digest('sha256', payloads['alpha.bin'])}\n",
    )
    write_text(
        "manifests/MIXEDSUMS",
        f"{digest('md5', payloads['alpha.bin'])} *alpha.bin\n"
        f"{digest('sha256', payloads['alpha.bin'])} *alpha.bin\n",
    )
    write_text(
        "manifests/malformed.sfv",
        "; mostly prose, must not pass the manifest confidence threshold\n"
        "This is not an entry\nStill not an entry\nalpha.bin NOTCRC32\n",
    )

    cp437_art = (
        "╔══════════════════════════════════════╗\r\n"
        "║         FERAIL FIXTURE NFO           ║\r\n"
        "╠══════════════════════════════════════╣\r\n"
        "║ ░░░░  ▒▒▒▒  ▓▓▓▓  ████              ║\r\n"
        "║ CP437 art, café, no personal data.   ║\r\n"
        "╚══════════════════════════════════════╝\r\n"
    )
    write_bytes("nfo/scene-cp437.nfo", cp437_art.encode("cp437"))
    write_bytes("release/release.nfo", cp437_art.encode("cp437"))
    release_art = ferail_release_nfo()
    write_bytes("nfo/ferail-release-color.nfo", release_art)
    write_bytes("release/FERAIL.NFO", release_art)
    ansi_art = (
        "\x1b[2J\x1b[H\x1b[36m"
        "╔════════════════════════════╗\r\n"
        "║ ANSI POSITIONING FIXTURE   ║\r\n"
        "╚════════════════════════════╝\x1b[0m\r\n"
        "\x1b[5;3HPlaced text\r\n"
        # These controls must be discarded, never executed.
        "\x1b]8;;https://example.invalid\x07inert link\x1b]8;;\x07\r\n"
        "\x1b]52;c;bmV2ZXItdG91Y2gtdGhlLWNsaXBib2FyZA==\x07"
    )
    write_bytes("nfo/scene-ansi.nfo", ansi_art.encode("cp437"))
    write_text(
        "nfo/scene-utf8.nfo",
        "┌──────────────────────────┐\n│ UTF-8 BOX ART FIXTURE    │\n└──────────────────────────┘\n",
    )

    write_text(
        "nfo/kodi-metadata.nfo",
        "<movie><title>Fixture Movie</title><originaltitle>Original Fixture</originaltitle>"
        "<year>2026</year><rating>8.2</rating><plot>Local fixture only.</plot></movie>\n",
    )
    write_text("nfo/kodi-url.nfo", "https://www.themoviedb.org/movie/12345\n")
    write_text(
        "nfo/kodi-combined.nfo",
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
        "<episodedetails><title>Fixture Episode</title><season>2</season>"
        "<episode>3</episode></episodedetails>\n"
        "https://www.thetvdb.com/series/example/episodes/12345\n",
    )
    write_text("nfo/kodi-artist.nfo", "<artist><name>Fixture Artist</name></artist>\n")

    msinfo = (
        '<?xml version="1.0"?>\r\n'
        '<MsInfo><Metadata><Version>1.0</Version></Metadata>'
        '<Category name="System Summary"><Data><Item>Fixture</Item>'
        '<Value>No machine data</Value></Data></Category></MsInfo>\r\n'
    )
    write_bytes("nfo/msinfo.nfo", b"\xff\xfe" + msinfo.encode("utf-16-le"))

    french = (
        "ÉTÉ À PARIS - ÇA RESTE DE LA PROSE.\r\n"
        "ÀÉÈÇ ÀÉÈÇ ne dessinent pas un cadre, même répétés.\r\n"
        "Une détection CP437 ne doit pas transformer ce texte en art ANSI.\r\n"
    )
    write_bytes("negative/french-latin1.txt", french.encode("latin-1"))
    write_text("negative/generic.xml", "<?xml version=\"1.0\"?><catalog><item>plain</item></catalog>\n")
    write_text("negative/plain.nfo", "Meeting notes\nNothing here describes another file.\n")

    write_text("security-root/outside-secret.txt", "MUST NEVER BE READ THROUGH A MANIFEST\n")
    write_text("security-root/child/safe.bin", "safe\n")
    write_text(
        "security-root/child/unsafe.sfv",
        "safe.bin 4FCF3C0A\n"
        "../outside-secret.txt 00000000\n"
        "..\\outside-secret.txt 00000000\n"
        "/etc/passwd 00000000\n"
        "C:\\Windows\\win.ini 00000000\n"
        "\\\\server\\share\\file 00000000\n"
        "\\\\?\\C:\\device-path 00000000\n",
    )


def generate_large(count: int) -> None:
    large = ROOT / "large"
    large.mkdir(parents=True, exist_ok=True)
    path = large / f"SHA256SUMS-{count}"
    with path.open("w", encoding="ascii", newline="\n") as handle:
        zero = "0" * 64
        for index in range(count):
            handle.write(f"{zero} *files/{index:09}.bin\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--large", type=int, metavar="COUNT")
    args = parser.parse_args()
    generate()
    if args.large is not None:
        if args.large < 1:
            parser.error("--large must be positive")
        generate_large(args.large)


if __name__ == "__main__":
    main()
