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

CAVEAT (false negatives - a clean run is NOT proof): the matcher looks for the
quoted anchor inside a *single* upstream line, so a comment quoting C that
upstream wraps across two lines is skipped silently and never counted. The 3.4.4
sweep found three wrong citations in crates/filters/src/rule.rs this way: only
the one quoting the short `case '/'` was flagged; its two siblings quoting
`case '/': rule->rflags |= FILTRULE_ABS_PATH;` were invisible because upstream
splits that across exclude.c:1238-1239. Nor does the tool check the *claim* - a
citation can land on a real line that says something else entirely. Both classes
need eyes on the comment and the upstream context.

The false-negative class above is now *counted*: a citation carrying anchors that
resolve nowhere in the pinned source is reported as `unresolved` rather than being
dropped in silence. An unresolved count is not a failure - a paraphrased quote lands
there too - but it is the population a version bump must be read against, because
that is where "the code this cites no longer exists" hides.

Usage:
    python3 tools/ci/citation_drift_audit.py [crate ...]   # default: all crates
"""
import re, os, sys, glob

VER = "3.5.0"
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
RANGE = re.compile(r'\b([a-z_0-9]+)\.c:(\d+)-(\d+)\b')
def anchors(comment):
    out = []
    for q in re.findall(r'"([^"]{8,60})"', comment) + re.findall(r'`([^`]{8,60})`', comment):
        q = q.strip()
        if (' ' in q or '%' in q) and '.c:' not in q and '://' not in q and not q.startswith('/'):
            out.append(q.replace('\\n', '').split('%')[0].strip())
    return [x for x in out if len(x) >= 8]

def audit(crate):
    checked = miss = read = unresolved = 0; ex = []; unres = []; backwards = []
    for rs in glob.glob(f"crates/{crate}/src/**/*.rs", recursive=True):
        read += 1
        for lineno, ln in enumerate(open(rs, errors="replace"), 1):
            if "upstream" not in ln.lower():
                continue
            # Structural invariant, checked first and independent of the anchor
            # machinery: END >= START. `CITE` matches only `file.c:START`, so nothing
            # else in this tool ever looks at END - a retarget that moves START leaves
            # END behind and yields `exclude.c:1381-1237`, which every other check
            # here audits perfectly clean. A gate that accepts a backwards range has
            # demonstrably not checked the range, so this one is hard-failing and is
            # deliberately NOT ratcheted: there is no such thing as an accepted
            # inverted range.
            for rm in RANGE.finditer(ln):
                if int(rm.group(3)) < int(rm.group(2)):
                    backwards.append(f"{rs}:{lineno}: {rm.group(0)} runs backwards")
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
                        # Print every hit, not the first 12. A sweep driven by a
                        # truncated list is the sweep that leaves the rest behind.
                        ex.append(f"{rs}: {f}.c:{a1} '{a[:24]}' -> {VER}@{locs[:3]}")
                    break
                else:
                    # No anchor on this line resolves anywhere in the pinned source,
                    # so the citation was previously dropped in silence and counted
                    # as neither checked nor drifted. That silence is exactly where a
                    # version bump hides "the code this cites no longer exists", so
                    # report it. It is not a failure on its own - a paraphrased quote
                    # lands here too - and it does not feed the ratchet.
                    unresolved += 1
                    unres.append(f"{rs}: {f}.c:{a1} '{anc[0][:32]}' resolves nowhere")
    print(f"{crate}: string-anchored={checked} suspected-drift={miss} "
          f"({miss/max(1,checked):.0%}) unresolved={unresolved}"
          + (f" BACKWARDS-RANGES={len(backwards)}" if backwards else ""))
    for e in ex:
        print("  ", e)
    for u in unres:
        print("  ?", u)
    for b in backwards:
        print("  !", b)
    return checked, miss, read, backwards

BASELINE = "tools/ci/citation_drift_baseline.json"

def ratchet(counts, path):
    """Compare per-crate drift counts against the committed baseline.

    Fails only when a crate's count INCREASES, so the 20-ish standing false
    positives cost nothing while newly introduced drift is caught. A hard gate
    at zero would be reverted the first time a legitimate definition-vs-usage
    citation tripped it; a bare report would print into a log nobody reads,
    which is the failure mode this audit already had once.
    """
    import json
    try:
        with open(path) as fh:
            base = json.load(fh)
    except OSError:
        sys.exit(f"baseline missing: {path} (regenerate with --write-baseline)")
    over = [(c, n, base.get(c, 0)) for c, n in sorted(counts.items()) if n > base.get(c, 0)]
    stale = [(c, n, base[c]) for c, n in sorted(counts.items()) if c in base and n < base[c]]
    for c, n, b in stale:
        print(f"note: {c} improved {b} -> {n}; lower the baseline to lock the gain in")
    if not over:
        print(f"ratchet OK against {path}")
        return 0
    for c, n, b in over:
        print(f"FAIL: {c} suspected-drift rose {b} -> {n}")
    print("\nEach new finding is a lead, not a verdict. Resolve it against the\n"
          f"pinned upstream source ({S}): update a stale line number, or - if the\n"
          "citation is a range or a definition the anchor cannot see - raise the\n"
          "baseline in the same commit with a note saying why.")
    return 1

if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    if not os.path.isdir(S):
        sys.exit(f"upstream source missing: {S} (run tools/ci/run_interop.sh to fetch)")
    crates = args or sorted(os.path.basename(os.path.dirname(p))
                            for p in glob.glob("crates/*/src"))
    if not crates:
        sys.exit(
            "refusing to report: resolved ZERO crates to audit. This tool once "
            "collapsed every crate name to the literal 'crates' and printed a "
            "clean 0/0 for months without opening a file; examining nothing is "
            "a failure, not a pass."
        )
    counts = {}
    files_read = 0
    backwards = []
    for c in crates:
        checked, miss, read, bad = audit(c)
        files_read += read
        backwards += bad
        if checked:
            counts[c] = miss
    if files_read == 0:
        sys.exit(
            f"refusing to report: opened ZERO Rust files across {len(crates)} "
            f"crate(s) {sorted(crates)}. Either the crate names do not resolve "
            "to crates/<name>/src or the tool is being run outside the "
            "workspace root."
        )
    if not counts:
        sys.exit(
            f"refusing to report: read {files_read} file(s) but string-anchored "
            "ZERO citations. The anchor extractor or the upstream source lookup "
            "is broken; a clean result here would be meaningless."
        )
    if backwards:
        # Fails in every mode, including --write-baseline: a backwards range is
        # never correct, so there is nothing here to accept as debt. Fix the range,
        # then regenerate.
        sys.exit(
            f"FAIL: {len(backwards)} citation range(s) run backwards.\n  "
            + "\n  ".join(backwards)
            + "\n\nEND must be >= START. This usually means a retarget moved START "
              "and left END on the old pin: `CITE` matches only `file.c:START`, so a "
              "search-and-replace over citations never touches END."
        )
    if "--write-baseline" in flags:
        import json
        # Merge, never replace. Two traps this avoids: `--write-baseline engine`
        # would otherwise drop every other crate's entry, and rewriting the file
        # would erase the `_`-prefixed notes that tell the next reader lowering
        # the baseline is mandatory, not optional.
        try:
            with open(BASELINE) as fh:
                merged = json.load(fh)
        except OSError:
            merged = {}
        merged.update(counts)
        with open(BASELINE, "w") as fh:
            json.dump(merged, fh, indent=2, sort_keys=True)
            fh.write("\n")
        print(f"wrote {BASELINE} ({', '.join(sorted(counts)) or 'no crates'} updated)")
    elif "--ratchet" in flags:
        sys.exit(ratchet(counts, BASELINE))
