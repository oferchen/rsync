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

TWO POPULATIONS, ONE OF THEM GATING. The line filter used to be "does this line
contain the word `upstream`", which sees `// upstream: flist.c:123 "..."` and
misses every citation written as a bullet under a `/// # Upstream Reference`
heading - the word is on the heading, never on the bullets. That is 5,240 of the
tree's 12,596 `file.c:NNN` citations, 42%, across 748 files: an entire
documentation style the tool had never once opened. The filter is now `.c:`,
the substring both `CITE` and `RANGE` already require, and the old predicate
survives as the SPLIT between two tallies rather than as a skip:

  * BLOCKING - line carries `upstream`. Ratcheted against
    tools/ci/citation_drift_baseline.json; a backwards range fails outright.
    Byte-for-byte the behaviour this tool had before the widening.
  * NON-BLOCKING - everything the widened filter newly reaches. Counted and
    reported (step summary table plus a `::warning` carrying the number), never
    gating, and deliberately NOT baselined: a suppression file holding several
    hundred unreviewed findings reads as "accepted" without anyone having
    accepted them, and the one new drift that matters would vanish inside it.

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

class Tally:
    """Counters for ONE population of citations.

    The tool scans two populations and must never let a reader confuse them:

      * BLOCKING - the citation's line carries the word "upstream" somewhere on
        it (`// upstream: flist.c:123 "..."`). This is the population the tool
        has always seen, and the only one `--ratchet` and the backwards-range
        hard failure act on.
      * EXTENDED - the line carries a `file.c:NNN` citation but NOT the word
        "upstream". Overwhelmingly these are bullets under a
        `/// # Upstream Reference` heading, where the word sits on the heading
        line and never on the citation lines. 42% of the tree's citations are
        written that way and were invisible to this tool until now. They are
        REPORTED and never gate.
    """

    __slots__ = ("cites", "checked", "miss", "unresolved", "ex", "unres", "backwards")

    def __init__(self):
        self.cites = self.checked = self.miss = self.unresolved = 0
        self.ex = []
        self.unres = []
        self.backwards = []


def _summarise(tally):
    return (f"string-anchored={tally.checked} suspected-drift={tally.miss} "
            f"({tally.miss / max(1, tally.checked):.0%}) unresolved={tally.unresolved}")


def audit(crate):
    """Scan one crate and return (blocking, extended, files_read).

    The line filter used to be `if "upstream" not in ln.lower(): continue`,
    which silently dropped every citation written as a bullet under a
    `/// # Upstream Reference` heading - the word is on the heading, never on
    the bullet. The filter is now `.c:`, which is exactly the substring both
    `CITE` and `RANGE` require, so it cannot skip a line either regex could
    have matched. The old predicate survives verbatim, but as the SPLIT between
    the two tallies rather than as a skip: every line that used to be scanned
    still lands in `blocking`, and only lines that used to be dropped land in
    `extended`.
    """
    blocking, extended = Tally(), Tally()
    read = 0
    for rs in glob.glob(f"crates/{crate}/src/**/*.rs", recursive=True):
        read += 1
        for lineno, ln in enumerate(open(rs, errors="replace"), 1):
            if ".c:" not in ln:
                continue
            t = blocking if "upstream" in ln.lower() else extended
            t.cites += len(CITE.findall(ln))
            # Structural invariant, checked first and independent of the anchor
            # machinery: END >= START. `CITE` matches only `file.c:START`, so nothing
            # else in this tool ever looks at END - a retarget that moves START leaves
            # END behind and yields `exclude.c:1381-1237`, which every other check
            # here audits perfectly clean. A gate that accepts a backwards range has
            # demonstrably not checked the range, so this one is hard-failing for the
            # BLOCKING population and is deliberately NOT ratcheted: there is no such
            # thing as an accepted inverted range. An inverted range found only by the
            # widened scan is reported, not failed, because widening the filter must
            # not redden a tree that was green a commit ago.
            for rm in RANGE.finditer(ln):
                if int(rm.group(3)) < int(rm.group(2)):
                    t.backwards.append(f"{rs}:{lineno}: {rm.group(0)} runs backwards")
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
                    t.checked += 1
                    if min(abs(p - a1) for p in locs) > 4:
                        t.miss += 1
                        # Print every hit, not the first 12. A sweep driven by a
                        # truncated list is the sweep that leaves the rest behind.
                        t.ex.append(f"{rs}: {f}.c:{a1} '{a[:24]}' -> {VER}@{locs[:3]}")
                    break
                else:
                    # No anchor on this line resolves anywhere in the pinned source,
                    # so the citation was previously dropped in silence and counted
                    # as neither checked nor drifted. That silence is exactly where a
                    # version bump hides "the code this cites no longer exists", so
                    # report it. It is not a failure on its own - a paraphrased quote
                    # lands here too - and it does not feed the ratchet.
                    t.unresolved += 1
                    t.unres.append(f"{rs}: {f}.c:{a1} '{anc[0][:32]}' resolves nowhere")
    print(f"{crate}: {_summarise(blocking)}"
          + (f" BACKWARDS-RANGES={len(blocking.backwards)}" if blocking.backwards else ""))
    for e in blocking.ex:
        print("  ", e)
    for u in blocking.unres:
        print("  ?", u)
    for b in blocking.backwards:
        print("  !", b)
    if extended.cites:
        print(f"{crate}: [non-blocking] extended-scan citations={extended.cites} "
              f"{_summarise(extended)}"
              + (f" backwards-ranges={len(extended.backwards)}" if extended.backwards else ""))
        # Drift leads are printed in full; the extended `unresolved` list runs to
        # thousands of paraphrase-anchored bullets and would bury them.
        for e in extended.ex:
            print("  +", e)
        for b in extended.backwards:
            print("  +!", b)
    return blocking, extended, read


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

def _load_baseline_quiet():
    import json
    try:
        with open(BASELINE) as fh:
            return {k: v for k, v in json.load(fh).items() if not k.startswith("_")}
    except OSError:
        return {}


def extended_report(blocking, extended, files_read, crates):
    """Emit the widened scan's findings as a counted, NON-BLOCKING report.

    Deliberately not a baseline. A suppression file holding several hundred
    accepted findings would freeze them in as "reviewed" - nobody reviewed them -
    and the one new drift that matters would land inside a number that large and
    never be seen again. A counted warning keeps the population visible and keeps
    the reader's eye on the delta instead of on a ratchet nobody can audit.

    The report always states the size of BOTH populations, so it can never read
    as clean by having scanned nothing: a run that opened no files, or one a
    filter regression emptied, prints zeros next to zeros and is refused by the
    caller rather than passing quietly.
    """
    def tot(d, field):
        return sum(getattr(t, field) for t in d.values())

    b_cites = tot(blocking, "cites")
    b_checked = tot(blocking, "checked")
    b_miss = tot(blocking, "miss")
    e_cites = tot(extended, "cites")
    e_checked = tot(extended, "checked")
    e_miss = tot(extended, "miss")
    e_unres = tot(extended, "unresolved")
    e_back = sum(len(t.backwards) for t in extended.values())
    base = _load_baseline_quiet()

    md = []
    md.append("## Citation drift audit")
    md.append("")
    md.append(f"Scanned {files_read} Rust file(s) across {len(crates)} crate(s) against the "
              f"pinned upstream rsync {VER} source. Two populations, reported separately: "
              "they are not interchangeable and only one of them gates.")
    md.append("")
    md.append("### 1. BLOCKING - citations whose line carries the word `upstream`")
    md.append("")
    md.append(f"{b_cites} citation(s); {b_checked} string-anchored; {b_miss} suspected drift. "
              "Ratcheted per crate against `tools/ci/citation_drift_baseline.json`; a rise "
              "fails this job, and a backwards range fails it outright.")
    md.append("")
    md.append("| crate | citations | string-anchored | suspected-drift | baseline |")
    md.append("| --- | ---: | ---: | ---: | ---: |")
    for c in sorted(blocking):
        t = blocking[c]
        if not t.cites:
            continue
        md.append(f"| `{c}` | {t.cites} | {t.checked} | {t.miss} | {base.get(c, '-')} |")
    md.append("")
    md.append("### 2. NON-BLOCKING - citations the blocking gate cannot see")
    md.append("")
    md.append("These lines carry a `file.c:NNN` citation but not the word `upstream`. Almost "
              "all are bullets under a `/// # Upstream Reference` heading, where the word "
              "sits on the heading line only, so the whole documentation style was invisible "
              "to this tool until the line filter was widened.")
    md.append("")
    md.append(f"{e_cites} citation(s); {e_checked} string-anchored; **{e_miss} suspected drift**; "
              f"{e_unres} unresolved; {e_back} backwards range(s).")
    md.append("")
    md.append("No baseline is written for these and none should be. A frozen list that size "
              "reads as accepted without anyone having accepted it, and hides the next real "
              "drift inside itself. This section is a count to work down, not a ratchet to "
              "satisfy.")
    md.append("")
    md.append("| crate | citations | string-anchored | suspected-drift | unresolved |")
    md.append("| --- | ---: | ---: | ---: | ---: |")
    for c in sorted(extended):
        t = extended[c]
        if not t.cites:
            continue
        md.append(f"| `{c}` | {t.cites} | {t.checked} | {t.miss} | {t.unresolved} |")
    md.append("")
    text = "\n".join(md) + "\n"

    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a") as fh:
            fh.write(text)
    else:
        print()
        print(text, end="")

    # The annotation carries the NUMBER, so a reader who never opens the step
    # summary still sees the size of the population, and names which population
    # it belongs to so it can never be read as the gating one. No `%` anywhere in
    # it: GitHub reads `%` as the start of an escape in a workflow command.
    print(f"::warning title=Citation drift (non-blocking)::{e_miss} suspected-drift finding(s) "
          f"among {e_checked} string-anchored citations that the blocking gate cannot see, "
          f"out of {e_cites} such citations in {files_read} file(s). "
          f"The blocking population is separate and unaffected: {b_miss} of {b_checked} "
          "anchored, ratcheted. Not baselined and not gating - work the number down, do not "
          "freeze it.")
    return text


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
    blocking = {}
    extended = {}
    counts = {}
    files_read = 0
    backwards = []
    for c in crates:
        b, e, read = audit(c)
        blocking[c] = b
        extended[c] = e
        files_read += read
        # Only the BLOCKING tally reaches `counts`, and only `counts` reaches the
        # ratchet and the baseline. Widening the line filter must not move a single
        # citation into the gating population, and this is the one place that could.
        backwards += b.backwards
        if b.checked:
            counts[c] = b.miss
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
    if not args and not sum(t.cites for t in extended.values()):
        # A whole-tree run that finds no citation outside the `upstream`-bearing
        # lines means the widened filter stopped widening - the exact regression
        # this change exists to prevent. Thousands of such citations are in the
        # tree today, so this cannot fire on a healthy run; if it ever does, the
        # non-blocking section would otherwise print an empty table and read as
        # clean when in fact it examined nothing.
        sys.exit(
            "refusing to report: the widened scan saw ZERO citations outside the "
            "lines carrying the word 'upstream'. The line filter has regressed to "
            "the narrow one, and an empty non-blocking report is indistinguishable "
            "from a clean one."
        )
    # Emitted before any exit path decides the status, so a run that hard-fails on
    # the blocking population still publishes the non-blocking report rather than
    # dying with it unwritten.
    extended_report(blocking, extended, files_read, crates)
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
