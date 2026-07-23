#!/usr/bin/env python3
"""Audit `// upstream: <file>.c:<line>` citations against the pinned upstream source.

Rust comments cite specific line numbers in the upstream rsync C source
(target/interop/upstream-src/rsync-<VER>/). Those line numbers drift when the
upstream source is bumped (files gain/lose lines between releases), leaving
citations pointing at the wrong line.

This tool locates a distinctive quoted C-string from each citation's comment in
the upstream source and reports citations whose cited line is far from where the
string actually lives.

CAVEAT (false positives): a comment may cite a variable *definition* while
quoting a *usage* found elsewhere, or quote a very common token. Treat the output
as a ranked lead list for manual review, NOT a hard gate. In practice a healthy
crate audits at ~10-20% (false-positive-dominated); a crate whose citations were
bulk-shifted onto wrong lines audits much higher (e.g. >50%).

Usage:
    python3 tools/ci/citation_drift_audit.py [crate ...]   # default: all crates
"""
import re, os, sys, glob

VER = "3.4.4"
S = f"target/interop/upstream-src/rsync-{VER}"
HIGH = {"flist","generator","receiver","io","token","sender","clientserver","options","main",
        "exclude","delete","backup","acls","rsync","batch","compat","log","socket","util1","util2","xattrs","checksum","match"}
_cache = {}
def src(f):
    if f not in _cache:
        p = f"{S}/{f}.c"
        _cache[f] = open(p, errors="replace").read().splitlines() if os.path.exists(p) else None
    return _cache[f]

CITE = re.compile(r'\b([a-z_0-9]+)\.c:(\d+)')
def anchors(comment):
    out = []
    for q in re.findall(r'"([^"]{8,60})"', comment) + re.findall(r'`([^`]{8,60})`', comment):
        q = q.strip()
        if (' ' in q or '%' in q) and '.c:' not in q and '://' not in q and not q.startswith('/'):
            out.append(q.replace('\\n', '').split('%')[0].strip())
    return [x for x in out if len(x) >= 8]

def audit(crate):
    checked = miss = 0; ex = []
    for rs in glob.glob(f"crates/{crate}/src/**/*.rs", recursive=True):
        for ln in open(rs, errors="replace"):
            if "upstream" not in ln.lower():
                continue
            anc = anchors(ln)
            if not anc:
                continue
            for m in CITE.finditer(ln):
                f, a1 = m.group(1), int(m.group(2))
                if f not in HIGH:
                    continue
                s = src(f)
                if not s:
                    continue
                for a in anc:
                    locs = [i + 1 for i, l in enumerate(s) if a in l]
                    if not locs:
                        continue
                    checked += 1
                    if min(abs(p - a1) for p in locs) > 4:
                        miss += 1
                        if len(ex) < 12:
                            ex.append(f"{rs}: {f}.c:{a1} '{a[:24]}' -> {VER}@{locs[:3]}")
                    break
    print(f"{crate}: string-anchored={checked} suspected-drift={miss} ({miss/max(1,checked):.0%})")
    for e in ex:
        print("  ", e)
    return checked, miss

if __name__ == "__main__":
    if not os.path.isdir(S):
        sys.exit(f"upstream source missing: {S} (run tools/ci/run_interop.sh to fetch)")
    crates = sys.argv[1:] or sorted(os.path.basename(os.path.dirname(os.path.dirname(p)))
                                    for p in glob.glob("crates/*/src"))
    for c in crates:
        audit(c)
