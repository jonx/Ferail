#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 John Knipper
"""Symbolize a gpui_aros profile (GPUI_AROS_PROFILE) against the unstripped
AROS binary that produced it.

    scripts/aros-profile-symbolize.py PROFILE BINARY [--top N] [--callers] [--stacks]

The file starts with a header naming the runtime address of
gpa_profile_start; the loader relocates .text as one block, so the
difference between that address and the symbol's value in the binary maps
every sampled program counter inside the binary to a symbol. Addresses
outside it (the kernel, the libraries) are named by the "sym" lines the
sampler appended through debug.library. Samples taken while the main task
waited count as idle. --stacks folds the frame-pointer chains into
caller paths (innermost first)."""
import argparse, bisect, collections, re, struct, subprocess, sys

def symbols(binary, nm):
    out = subprocess.run([nm, "--defined-only", "-n", binary], capture_output=True, text=True, check=True).stdout
    syms = []
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3 and parts[1] in "tTwW":
            syms.append((int(parts[0], 16), parts[2]))
    syms.sort()
    return syms

def demangle(names, filt):
    if not filt or not names:
        return names
    out = subprocess.run([filt], input="\n".join(names), capture_output=True, text=True).stdout.splitlines()
    return out if len(out) == len(names) else names

import os

# The AROS project root, as scripts/aros-cargo.py finds it.
PROJECT = os.environ.get("AROS_PROJECT", os.path.expanduser("~/Source/aros-project"))
# The AROS build whose ROM modules the profiled system ran; set it to name
# frames outside the program.
MODULE_ROOTS = [r for r in [os.environ.get("AROS_MODULE_ROOT")] if r]

def find_module_elf(name):
    for root in MODULE_ROOTS:
        for dirpath, _, files in os.walk(root):
            if name in files:
                return os.path.join(dirpath, name)
    return None

class Namer:
    """Addresses inside the binary by its own symbols. Addresses in a
    resident module by the module's ELF symbols: the Resident tag's
    runtime address (from the "mod" lines) and the tag's symbol in the
    ELF (`<Name>_resident`, the one `rt_MatchTag` points at) anchor the
    module, whose code the loader relocated as one block; every address is
    assigned to the module whose anchored image range contains it. The
    rest by debug.library's table or as raw addresses."""
    def __init__(self, syms, delta, table, modules, nm, anchored=()):
        self.syms, self.addrs, self.delta, self.table = syms, [s[0] for s in syms], delta, table
        self.end = self.addrs[-1] + 0x100000 if syms else 0
        self.nm = nm
        self.ranges = []  # (lo, hi, name, base, syms, addrs) with base = runtime address of ELF offset 0
        seen = set()
        for base, hi, name in anchored:
            elf = find_module_elf(name)
            msyms = symbols(elf, nm) if elf else []
            if msyms:
                self.ranges.append((base, hi, name, base, msyms, [v for v, _ in msyms])); seen.add(name)
        for tag_rt, _, name in modules:
            if name in seen:
                continue
            elf = find_module_elf(name)
            if not elf:
                continue
            msyms = symbols(elf, nm)
            if not msyms:
                continue
            tag = next((v for v, n in msyms if n.lower().endswith("_resident")), None)
            if tag is None:
                continue
            base = tag_rt - tag
            hi = base + msyms[-1][0] + 0x4000
            self.ranges.append((base, hi, name, base, msyms, [v for v, _ in msyms]))
        self.ranges.sort()
    def name(self, a):
        off = a - self.delta
        if 0 <= off <= self.end:
            i = bisect.bisect_right(self.addrs, off) - 1
            if i >= 0:
                return self.syms[i][1]
        for lo, hi, name, base, msyms, maddrs in self.ranges:
            if lo <= a < hi:
                j = bisect.bisect_right(maddrs, a - base) - 1
                if j >= 0:
                    return f"[{name}] {msyms[j][1]}"
        t = self.table.get(a & ~0xf)
        if t and t[1] != "?+0":
            module, symbol = t
            return f"[{module}] {symbol}"
        return "<outside: %x>" % a

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("profile"); ap.add_argument("binary")
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument("--callers", action="store_true", help="group by (function, caller) pairs")
    ap.add_argument("--stacks", action="store_true", help="fold the frame chains into paths")
    ap.add_argument("--depth", type=int, default=4)
    ap.add_argument("--nm", default=os.path.join(PROJECT, "aros-crosstools/bin/llvm-nm"))
    ap.add_argument("--cxxfilt", default=os.path.join(PROJECT, "aros-crosstools/bin/llvm-cxxfilt"))
    a = ap.parse_args()
    data = open(a.profile, "rb").read()
    nl = data.index(b"\n")
    header = data[:nl].decode()
    m = re.match(r"gpui-aros-profile-v(\d) start=([0-9a-f]+) interval_us=(\d+) samples=(\d+)(?: frames=(\d+))?", header)
    if not m:
        sys.exit(f"bad header: {header!r}")
    version, start_rt, interval, count = int(m.group(1)), int(m.group(2), 16), int(m.group(3)), int(m.group(4))
    nframes = int(m.group(5) or 0)
    rec = struct.Struct("<QQII") if version == 1 else struct.Struct("<QQ%dQII" % nframes)
    body = data[nl + 1:]
    table = {}; modules = []; lvos = {}; tagmods = []
    tail = body[count * rec.size:]
    for line in tail.decode("latin-1").splitlines():
        parts = line.split(" ", 3)
        if len(parts) == 4 and parts[0] == "sym":
            table[int(parts[1], 16)] = (parts[2], parts[3])
        elif parts[0] == "mod":
            # "mod <name> <tag> <end>": the Resident tag's runtime address
            # (a name may hold spaces)
            _, name, tag, end = line.rsplit(" ", 3)
            try:
                tagmods.append((int(tag, 16), int(end, 16), name))
            except ValueError:
                pass
        elif parts[0] == "lvo":
            # "lvo <module> <n> <addr>": the runtime address of jump vector n
            _, module, n, addr = line.rsplit(" ", 3)
            lvos.setdefault(module, []).append((int(n), int(addr, 16)))
    syms = symbols(a.binary, a.nm)
    start_sym = next((v for v, n in syms if n == "gpa_profile_start"), None)
    if start_sym is None:
        sys.exit("gpa_profile_start not in the binary's symbols (unstripped build needed)")
    # Anchor each ROM module's .text through one jump vector: the ELF names
    # the function of vector n as <Prefix>_<n>_<Name>, so base = addr - value.
    for module, vectors in lvos.items():
        elf = find_module_elf(module)
        if not elf:
            continue
        msyms = symbols(elf, a.nm)
        for n, addr in vectors:
            hit = next((v for v, name in msyms if re.match(r"^[A-Za-z0-9]+_%d_" % n, name)), None)
            if hit is not None:
                base = addr - hit
                modules.append((base, base + msyms[-1][0] + 0x1000, module))
                break
    namer = Namer(syms, start_rt - start_sym, table, tagmods, a.nm, anchored=modules)
    idle = 0; busy = collections.Counter(); pairs = collections.Counter(); stacks = collections.Counter()
    for i in range(min(count, len(body) // rec.size)):
        fields = rec.unpack_from(body, i * rec.size)
        pc, lr = fields[0], fields[1]
        frames = list(fields[2:2 + nframes]) if version >= 2 else []
        waiting, n = fields[-2], fields[-1]
        if waiting:
            idle += 1
            continue
        key = namer.name(pc)
        busy[key] += 1
        if a.callers:
            pairs[(key, namer.name(lr))] += 1
        if a.stacks:
            chain = [key] + [namer.name(f) for f in frames[:max(0, n if version >= 2 else 0)][: a.depth]]
            stacks[tuple(chain)] += 1
    total = idle + sum(busy.values())
    print(f"samples={total} interval={interval}us idle={idle} ({100.0*idle/max(total,1):.1f}%) busy={sum(busy.values())} modules={len(namer.ranges)} named_outside={len(table)} delta=0x{namer.delta:x}")
    names = [k for k, _ in busy.most_common(a.top)]
    for name, pretty in zip(names, demangle(names, a.cxxfilt)):
        n = busy[name]
        print(f"{n:7d} {100.0*n/max(total,1):5.1f}%  {pretty}")
    if a.callers:
        print("\n-- (function <- caller) --")
        keys = pairs.most_common(a.top)
        flat = [k for (k, _), _ in keys] + [c for (_, c), _ in keys]
        pretty = demangle(flat, a.cxxfilt); half = len(keys)
        for i, ((k, c), n) in enumerate(keys):
            print(f"{n:7d} {100.0*n/max(total,1):5.1f}%  {pretty[i]}  <-  {pretty[half + i]}")
    if a.stacks:
        print("\n-- stacks (innermost first) --")
        for chain, n in stacks.most_common(a.top):
            pretty = demangle(list(chain), a.cxxfilt)
            print(f"{n:7d} {100.0*n/max(total,1):5.1f}%  " + "  <-  ".join(p[:90] for p in pretty))

if __name__ == "__main__":
    main()
