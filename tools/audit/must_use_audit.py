#!/usr/bin/env python3
"""Audit #[must_use] annotation coverage on pub fn returning Result/Option."""

import os
import re
import sys
from collections import defaultdict

BASE = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "crates"
)
BASE = os.path.normpath(BASE)

PUB_FN_RE = re.compile(
    r'^\s*pub(?:\s*\([^)]*\))?\s+(?:async\s+|const\s+|unsafe\s+)*fn\s+(\w+)'
)
RETURN_RE = re.compile(r'->\s*([^{;]+)')


def classify_return(rt):
    rt = rt.strip()
    if re.search(r'\b(Result|io::Result|IoResult)\s*<', rt):
        return 'Result'
    if re.match(r'^[\w:]*Result(<|\s|$)', rt):
        return 'Result'
    if re.search(r'\bOption\s*<', rt):
        return 'Option'
    return None


def audit_file(path):
    try:
        with open(path, 'r', encoding='utf-8', errors='ignore') as f:
            lines = f.readlines()
    except Exception:
        return []
    out = []
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        m = PUB_FN_RE.match(line)
        if not m:
            i += 1
            continue
        fn_name = m.group(1)
        j = i
        while j < n and '{' not in lines[j] and ';' not in lines[j]:
            j += 1
        sig_buf = ''.join(lines[i:j + 1]) if j < n else ''.join(lines[i:])
        rm_ = RETURN_RE.search(sig_buf)
        kind = classify_return(rm_.group(1)) if rm_ else None
        if kind is not None:
            has_mu = False
            k = i - 1
            while k >= 0:
                s = lines[k].strip()
                if s == '':
                    k -= 1
                    continue
                if s.startswith('///') or s.startswith('//!') or s.startswith('//'):
                    k -= 1
                    continue
                if s.startswith('#['):
                    if 'must_use' in s:
                        has_mu = True
                        break
                    k -= 1
                    continue
                break
            out.append((i + 1, fn_name, kind, has_mu))
        i = j + 1
    return out


def crate_files(p):
    src = os.path.join(p, 'src')
    if not os.path.isdir(src):
        return []
    out = []
    for root, _, files in os.walk(src):
        for f in files:
            if f.endswith('.rs'):
                out.append(os.path.join(root, f))
    return out


def main():
    crates = sorted(
        d for d in os.listdir(BASE) if os.path.isdir(os.path.join(BASE, d))
    )
    summary = {}
    all_missing = []

    for crate in crates:
        rt = rm = ot = om = 0
        for fp in crate_files(os.path.join(BASE, crate)):
            rel = os.path.relpath(fp, os.path.dirname(BASE))
            for ln, name, kind, has_mu in audit_file(fp):
                if kind == 'Result':
                    rt += 1
                    if not has_mu:
                        rm += 1
                        all_missing.append((crate, rel, ln, name, kind))
                else:
                    ot += 1
                    if not has_mu:
                        om += 1
                        all_missing.append((crate, rel, ln, name, kind))
        summary[crate] = (rt, rm, ot, om)

    print("RESULT_TABLE")
    for crate, (rt, rm, ot, om) in summary.items():
        cov = f"{(rt - rm) * 100.0 / rt:.1f}%" if rt else "n/a"
        print(f"{crate}|{rt}|{rm}|{cov}")
    print("OPTION_TABLE")
    for crate, (rt, rm, ot, om) in summary.items():
        cov = f"{(ot - om) * 100.0 / ot:.1f}%" if ot else "n/a"
        print(f"{crate}|{ot}|{om}|{cov}")
    print("BY_CRATE")
    by_crate = defaultdict(int)
    for c, _, _, _, _ in all_missing:
        by_crate[c] += 1
    for c in sorted(by_crate, key=lambda k: -by_crate[k]):
        print(f"{c}|{by_crate[c]}")
    print("TOP30")
    priority = {
        'core': 1, 'protocol': 2, 'engine': 3, 'transfer': 4, 'checksums': 5,
        'filters': 6, 'metadata': 7, 'daemon': 8, 'cli': 9, 'signature': 10,
        'compress': 11, 'bandwidth': 12, 'fast_io': 13, 'flist': 14,
        'rsync_io': 15,
    }

    def pk(it):
        c, _, _, _, k = it
        return (0 if k == 'Result' else 1, priority.get(c, 99), c)

    for crate, rel, ln, name, kind in sorted(all_missing, key=pk)[:30]:
        print(f"{kind}|{rel}:{ln}|{name}")
    print("CLEAN")
    for crate, (rt, rm, ot, om) in summary.items():
        if rm == 0 and om == 0:
            print(f"{crate}|{rt}|{ot}")

    print("PER_CRATE_SAMPLES")
    by_crate_missing = defaultdict(list)
    for c, rel, ln, name, kind in all_missing:
        by_crate_missing[c].append((rel, ln, name, kind))
    for crate in sorted(by_crate_missing.keys()):
        for (rel, ln, name, kind) in by_crate_missing[crate][:3]:
            print(f"{crate}|{kind}|{rel}:{ln}|{name}")


if __name__ == '__main__':
    main()
