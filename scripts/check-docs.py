#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Repository-local Markdown checker: links, anchors, tables of contents, identifiers, blocks, indexes.

Copy this file into the repository's tool directory and edit CONFIG. It has no
dependencies beyond the Python 3 standard library and never touches the
network.

Checks:

1. every relative link resolves to a file or directory, and every ``#fragment``
   on a Markdown link resolves to a GitHub-style heading anchor;
2. every ``<!-- toc -->`` ... ``<!-- /toc -->`` block matches the headings of
   its file (``--write-toc`` regenerates them; ``<!-- toc depth=2 -->`` limits
   the depth);
3. every identifier matching an ``identifiers`` contract resolves to a heading
   in one file or to a file in one directory;
4. every file matching a ``blocks`` contract carries the required line within
   its first N lines;
5. every member of an ``indexes`` contract is mentioned in its index file.

Fenced code is ignored everywhere, so examples never trigger a check.
"""

from __future__ import annotations

import argparse
import glob
from pathlib import Path
import re
import sys
from urllib.parse import unquote


CONFIG = {
    # Trees never scanned.
    "excluded_trees": {
        ".git", "node_modules", "vendor", "third_party", "target", "build",
        "dist", "screenshots",
    },
    # Links into these trees are counted but not required to exist.
    "private_trees": {"screenshots"},
    # Identifier contracts: every mention must resolve to its home.
    "identifiers": [
        {
            "name": "windows item",
            "pattern": r"\bWIN-(\d{3})\b",
            "headings_in": "docs/features/WINDOWS_COMPATIBILITY_PLAN.md",
            "heading": r"^###\s+WIN-(\d{3})\b",
        },
    ],
    # Navigation-block contracts.
    "blocks": [
        {
            "glob": "docs/features/*.md",
            "pattern": r"^\u2190 ",
            "within": 10,
            "hint": "see docs/DOCUMENTATION.md",
        },
    ],
    # Index contracts: every document is listed in its index.
    "indexes": [
        {
            "members": "docs/*.md",
            "index": "docs/README.md",
            "mention": "({name})",
        },
        {
            "members": "docs/features/*.md",
            "index": "docs/features/README.md",
            "mention": "({name})",
        },
        {
            "members": "docs/testing/*.md",
            "index": "docs/README.md",
            "mention": "(testing/{name})",
        },
        {
            "members": "docs/memos/*.md",
            "index": "docs/README.md",
            "mention": "(memos/{name})",
        },
    ],
}


LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*#*\s*$")
SCHEME = re.compile(r"^[a-z][a-z0-9+.-]*:", re.IGNORECASE)
FENCE = re.compile(r"^\s*(```|~~~)")
TOC_START = re.compile(r"^<!--\s*toc(?:\s+depth=([1-6]))?\s*-->\s*$")
TOC_END = re.compile(r"^<!--\s*/toc\s*-->\s*$")


def github_slug(text: str) -> str:
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text).strip().lower()
    text = re.sub(r"[`*_~]", "", text)
    text = "".join(c for c in text if c.isalnum() or c in " -_")
    return re.sub(r"\s", "-", text)


def markdown_files(root: Path) -> list[Path]:
    excluded = CONFIG["excluded_trees"]
    return sorted(
        p for p in root.rglob("*.md")
        if not any(part in excluded for part in p.relative_to(root).parts)
    )


def strip_fences(text: str) -> str:
    out, in_fence = [], False
    for line in text.splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if not in_fence:
            out.append(line)
    return "\n".join(out)


def headings(lines: list[str]) -> list[tuple[int, int, str, str]]:
    counts: dict[str, int] = {}
    found, in_fence = [], False
    for index, line in enumerate(lines):
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        m = HEADING.match(line)
        if not m:
            continue
        base = github_slug(m.group(2))
        n = counts.get(base, 0)
        counts[base] = n + 1
        found.append((index, len(m.group(1)), m.group(2),
                      base if n == 0 else f"{base}-{n}"))
    return found


def read_lines(path: Path) -> list[str]:
    return path.read_text(encoding="utf-8").splitlines()


def destination(raw: str) -> str:
    value = raw.strip()
    if value.startswith("<"):
        close = value.find(">")
        if close != -1:
            return value[1:close]
    for sep in (' "', " '"):
        if sep in value:
            value = value.split(sep, 1)[0]
    return value


def under(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def render_toc(lines: list[str], depth: int, toc_line: int) -> list[str]:
    return [
        f"{'  ' * (level - 2)}- [{text}](#{anchor})"
        for index, level, text, anchor in headings(lines)
        if 2 <= level <= depth and index >= toc_line
    ]


def process_toc(path: Path, write: bool) -> tuple[bool, bool]:
    lines = read_lines(path)
    start = end = -1
    depth = 3
    for i, line in enumerate(lines):
        m = TOC_START.match(line)
        if m and start == -1:
            start = i
            if m.group(1):
                depth = int(m.group(1))
        elif TOC_END.match(line) and start != -1:
            end = i
            break
    if start == -1:
        return False, False
    if end == -1:
        raise ValueError(f"{path}: <!-- toc --> without <!-- /toc -->")
    expected = render_toc(lines, depth, start)
    current = [l for l in lines[start + 1:end] if l.strip()]
    stale = current != expected
    if stale and write:
        lines[start + 1:end] = ([""] + expected + [""]) if expected else [""]
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return True, stale


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path,
                        default=Path(__file__).resolve().parents[1])
    parser.add_argument("--write-toc", action="store_true",
                        help="regenerate stale <!-- toc --> blocks instead of failing")
    args = parser.parse_args()
    root = args.root.resolve()
    files = markdown_files(root)
    problems: list[str] = []
    rel = lambda p: str(p.relative_to(root))  # noqa: E731

    # 2. Tables of contents first, so anchors reflect the final text.
    toc_files = toc_stale = 0
    for path in files:
        has, stale = process_toc(path, args.write_toc)
        toc_files += has
        if stale and not args.write_toc:
            toc_stale += 1
            problems.append(f"{rel(path)} (stale toc; run --write-toc)")

    anchors = {p.resolve(): {a for _, _, _, a in headings(read_lines(p))}
               for p in files}
    texts = {p: strip_fences(p.read_text(encoding="utf-8")) for p in files}

    # 1. Links and fragments.
    checked = external = private = 0
    for source in files:
        for m in LINK.finditer(texts[source]):
            raw = destination(m.group(1))
            if SCHEME.match(raw) or raw.startswith("//"):
                external += 1
                continue
            target_text, sep, fragment = raw.partition("#")
            target = source if not target_text else source.parent / unquote(target_text)
            if any(under(target, root / t) for t in CONFIG["private_trees"]):
                private += 1
                continue
            checked += 1
            label = f"{rel(source)} -> {raw}"
            if not target.exists():
                problems.append(f"{label} (missing)")
            elif (sep and fragment and target.is_file()
                  and target.suffix.lower() == ".md"
                  and unquote(fragment).lower() not in anchors.get(target.resolve(), set())):
                problems.append(f"{label} (missing anchor)")

    # 3. Identifier contracts.
    id_refs = 0
    for contract in CONFIG["identifiers"]:
        pattern = re.compile(contract["pattern"])
        known: set[str] = set()
        if "headings_in" in contract:
            heading = re.compile(contract["heading"])
            source = root / contract["headings_in"]
            if source.exists():
                for line in read_lines(source):
                    m = heading.match(line)
                    if m:
                        known.add(m.group(1))
            where = contract["headings_in"]
        else:
            folder = root / contract["files_in"]
            known = {p.name for p in folder.iterdir()} if folder.exists() else set()
            where = contract["files_in"] + "/"
        for path in files:
            for m in pattern.finditer(texts[path]):
                id_refs += 1
                ident = m.group(1) if m.groups() else m.group(0)
                for old, new in contract.get("normalize", {}).items():
                    ident = ident.replace(old, new)
                name = contract.get("file", "{id}").format(id=ident)
                if name not in known:
                    problems.append(
                        f"{rel(path)} -> {m.group(0)} (no {contract['name']} {name} in {where})")

    # 4. Block contracts.
    blocks = 0
    for contract in CONFIG["blocks"]:
        pattern = re.compile(contract["pattern"])
        for name in glob.glob(str(root / contract["glob"])):
            path = Path(name)
            blocks += 1
            head = read_lines(path)[: contract.get("within", 40)]
            if not any(pattern.match(l) for l in head):
                problems.append(
                    f"{rel(path)} (missing required block {contract['pattern']!r}; "
                    f"{contract.get('hint', '')})")

    # 5. Index contracts.
    indexed = 0
    for contract in CONFIG["indexes"]:
        index = root / contract["index"]
        index_text = index.read_text(encoding="utf-8") if index.exists() else ""
        for name in glob.glob(str(root / contract["members"])):
            path = Path(name)
            if path == index:
                continue
            indexed += 1
            mention = contract["mention"].format(
                name=path.name, parent=path.parent.name,
                rel=str(path.relative_to(index.parent)) if under(path, index.parent) else path.name)
            if mention not in index_text:
                problems.append(f"{contract['index']} does not list {rel(path)}")

    result = "PASS" if not problems else "FAIL"
    print(f"check-docs result={result} markdown={len(files)} links={checked} "
          f"external={external} private={private} toc_files={toc_files} "
          f"toc_stale={toc_stale} identifier_refs={id_refs} blocks={blocks} "
          f"indexed={indexed} problems={len(problems)}")
    for item in problems:
        print(item)
    return 1 if problems else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, ValueError) as error:
        print(f"check-docs result=FAIL reason={error}", file=sys.stderr)
        raise SystemExit(1)
