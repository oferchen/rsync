#!/usr/bin/env python3
"""Retarget upstream rsync line citations from one pinned release to the next.

Every `file.c:NNN` citation in the tree names a line of the pinned upstream
release. When the pin moves, each cited construct is re-located in the new
release by CONTENT, never by arithmetic:

  1. Resolve the construct at the old `file:line`: its enclosing function and
     the cited line's text with whitespace normalised (the anchor).
  2. Look for the same anchor inside the same function of the new release, by
     occurrence ordinal. The ordinal is used only when the anchor occurs equally
     often in both versions of the function; otherwise it is not evidence.
  3. Fall back to the same ordinal rule over the whole file.
  4. Cross-check against a line-level diff alignment of the two files, which
     is what also rescues a line whose text changed in place.

Each citation number is classified as

  unchanged  - resolves to the same line number (includes unchanged files);
  moved      - same anchor found at a new line, and the diff agrees;
  flagged    - auto-remapped, but on weaker evidence: the text changed in place
               (1:1 diff replacement), or only one of the two methods resolved;
  ambiguous  - the methods disagree or several candidates remain: NOT rewritten;
  vanished   - the construct is gone in the new release: NOT rewritten;
  resolved   - an ambiguous or vanished citation a human re-located by reading
               the new release, supplied through --resolutions.

Only `unchanged`, `moved` and `flagged` numbers are written back. Ambiguous and
vanished citations are listed for a human, because a deleted or rewritten
construct can mean the behaviour claim beside the citation is now wrong.

Citation forms covered (the path must resolve to a file the release ships, by
exact path or unique basename, as `cargo xtask citations` resolves it):

  flist.c:123            flist.c:123-130         flist.c:123,130-140
  flist.c:123/130        flist.c:123 and 130     flist.c:send_file_list:123
  flist.c:send_file_list():123                   flist.c line 123 / lines 1-9
  rsync-<OLD>/flist.c:123 and target/interop/upstream-src/rsync-<OLD>/...

Release-prefixed citations of the old release are rewritten to the new prefix
when every number in them resolves; otherwise they are left untouched and
reported. Prose naming the old release outside a citation is never touched.

Usage:
  python3 tools/ci/citation_retarget.py --from 3.5.0 --to 3.5.1 \\
      --from-dir <old tree> --to-dir <new tree> [--apply] [--report out.json]

Without --apply nothing is written; the classification summary is printed
either way. The run is deterministic: re-running it on a rebased tree is the
supported way to resolve a merge conflict in the rewritten citations.
"""
from __future__ import annotations

import argparse
import difflib
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field

CITED_EXTENSIONS = ("c", "h", "py", "sh")
UNPACK_PREFIX = "target/interop/upstream-src/"

# Files whose citations are hand-maintained fixtures or deliberate history.
SKIP_SOURCES = {
    "xtask/src/commands/citations.rs",
    "tools/ci/citation_retarget.py",
    "tools/ci/citation_drift_baseline.json",
    "tools/tests/test_citation_retarget.py",
}
SKIP_PREFIXES = ("tools/ci/upstream-",)

# Record-keeping trees. An audit or design note written against an older release
# is accurate BECAUSE it names that release's lines, and most of these were never
# retargeted at the current pin either, so following their numbers through the
# old pin's content would move a 3.4.x coordinate to an unrelated construct. A
# file here is retargeted only when its own quoted anchors show it tracks the old
# pin (see `tracks_old_pin`); every other one is left as written.
RECORD_PREFIXES = ("docs/", "CHANGELOG.md", ".github/RELEASE_NOTES")
RELEASE_IN_NAME = re.compile(r"\d+\.\d+\.\d+")

PATH_CHARS = r"A-Za-z0-9_\-/."
EXT = "|".join(CITED_EXTENSIONS)
NUM = r"\d+(?:-\d+)?"
SEP = r"(?:,\s?(?:and\s)?|/|\s(?:and|or|&)\s)"
CITATION = re.compile(
    rf"(?P<path>[{PATH_CHARS}]*[A-Za-z0-9_]\.(?:{EXT}))"
    rf"(?:(?P<colon>:)(?:(?P<func>[A-Za-z_][A-Za-z0-9_]*)(?:\(\))?:)?|(?P<word>\slines?\s))"
    rf"(?P<nums>{NUM}(?:{SEP}{NUM})*)"
    r"(?![0-9A-Za-z_])"
)
NUMBER = re.compile(r"\d+")


def norm(line: str) -> str:
    return " ".join(line.split())


def read_lines(path: str) -> list[str]:
    with open(path, "rb") as fh:
        return fh.read().decode("utf-8", "replace").splitlines()


def derive_manifest(root: str) -> dict[str, str]:
    """Relative path -> absolute path for every citable file under root."""
    out = {}
    for d, _, files in os.walk(root):
        for f in files:
            if "." in f and f.rsplit(".", 1)[1] in CITED_EXTENSIONS:
                p = os.path.join(d, f)
                out[os.path.relpath(p, root).replace(os.sep, "/")] = p
    return out


def resolve(path: str, manifest: dict[str, str]) -> str | None:
    """Mirror of xtask `resolve_within_release`: exact path, else unique basename."""
    if path in manifest:
        return path
    if "/" in path:
        return None
    hits = [k for k in manifest if k.rsplit("/", 1)[-1] == path]
    return hits[0] if len(hits) == 1 else None


def split_release(path: str) -> tuple[str | None, str, str]:
    """Return (version, prefix, tail) for a `[target/.../]rsync-<VER>/` path."""
    m = re.match(r"^((?:" + re.escape(UNPACK_PREFIX) + r")?rsync-([0-9][0-9.\-]*)/)(.*)$", path)
    if not m:
        return None, "", path
    return m.group(2), m.group(1), m.group(3)


# ---------------------------------------------------------------- structure


def c_functions(lines: list[str]) -> list[tuple[str, int, int]]:
    """(name, signature line, closing-brace line), 1-based, for rsync's style.

    rsync opens every function body with `{` alone in column 0 and closes it
    with `}` alone in column 0; the name is the identifier before the first `(`
    of the signature lines above the brace.
    """
    out = []
    i = 0
    n = len(lines)
    while i < n:
        if lines[i].rstrip() == "{":
            j = i - 1
            sig = []
            while j >= 0 and lines[j].strip() and not lines[j].rstrip().endswith((";", "}")):
                sig.insert(0, lines[j])
                if lines[j][:1] not in (" ", "\t") and "(" in lines[j]:
                    break
                j -= 1
            text = " ".join(sig)
            m = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*\(", text)
            k = i + 1
            while k < n and lines[k].rstrip() != "}":
                k += 1
            if m and k < n:
                out.append((m.group(1), j + 1, k + 1))
                i = k + 1
                continue
        i += 1
    return out


def py_functions(lines: list[str]) -> list[tuple[str, int, int]]:
    out = []
    starts = [(i, re.match(r"^(\s*)def\s+([A-Za-z_][A-Za-z0-9_]*)", l)) for i, l in enumerate(lines)]
    starts = [(i, m) for i, m in starts if m]
    for i, m in starts:
        indent = len(m.group(1))
        k = i + 1
        while k < len(lines):
            s = lines[k]
            if s.strip() and (len(s) - len(s.lstrip())) <= indent:
                break
            k += 1
        out.append((m.group(2), i + 1, k))
    return out


def functions(path: str, lines: list[str]) -> list[tuple[str, int, int]]:
    if path.endswith(".c"):
        return c_functions(lines)
    if path.endswith(".py"):
        return py_functions(lines)
    return []


def enclosing(funcs, line: int):
    """Innermost function span containing `line`, with its occurrence index."""
    best = None
    for name, a, b in funcs:
        if a <= line <= b and (best is None or a >= best[1]):
            best = (name, a, b)
    if best is None:
        return None
    occ = sum(1 for name, a, _ in funcs if name == best[0] and a < best[1])
    return best + (occ,)


@dataclass
class FilePair:
    old: list[str]
    new: list[str]
    old_funcs: list = field(default_factory=list)
    new_funcs: list = field(default_factory=list)
    diff_map: dict = field(default_factory=dict)  # old line -> (new line, exact)

    @classmethod
    def load(cls, rel: str, old_path: str, new_path: str) -> "FilePair":
        old, new = read_lines(old_path), read_lines(new_path)
        fp = cls(old, new, functions(rel, old), functions(rel, new))
        if old != new:
            fp.diff_map = diff_alignment(old, new)
        return fp


def diff_alignment(old: list[str], new: list[str]) -> dict[int, tuple[int, bool]]:
    """Map old line numbers to new ones: equal blocks exactly, equal-size
    replacements one-to-one (text changed in place)."""
    a = [norm(x) for x in old]
    b = [norm(x) for x in new]
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    out = {}
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            for k in range(i2 - i1):
                out[i1 + k + 1] = (j1 + k + 1, True)
        elif tag == "replace" and i2 - i1 == j2 - j1:
            for k in range(i2 - i1):
                out[i1 + k + 1] = (j1 + k + 1, False)
    return out


def ordinal_match(old_lines, new_lines, old_span, new_span, line):
    """Same anchor, same occurrence index, inside spans with equal counts."""
    anchor = norm(old_lines[line - 1])
    oa, ob = old_span
    na, nb = new_span
    olds = [i for i in range(oa, ob + 1) if norm(old_lines[i - 1]) == anchor]
    news = [i for i in range(na, nb + 1) if norm(new_lines[i - 1]) == anchor]
    if not news:
        return None, "absent"
    if len(olds) != len(news):
        return None, "count-mismatch"
    return news[olds.index(line)], "ok"


@dataclass
class Resolution:
    cls: str
    new: int | None
    func: str | None
    anchor: str
    candidates: list = field(default_factory=list)
    why: str = ""


def remap_line(fp: FilePair, line: int, func_hint: str | None = None) -> Resolution:
    """Classify one cited line number; see the module docstring for the classes.

    `moved` needs the diff alignment to place the line with its text intact and
    the anchor ordinal (function first, then file) to agree or be undecidable;
    a disagreement is `ambiguous`, never a coin toss.
    """
    if line < 1 or line > len(fp.old):
        return Resolution("vanished", None, None, "", why="line past end of old file")
    anchor = norm(fp.old[line - 1])
    if fp.old == fp.new:
        return Resolution("unchanged", line, None, anchor)
    enc = enclosing(fp.old_funcs, line)
    func = enc[0] if enc else None
    by_func = None
    func_note = ""
    if enc:
        name, a, b, occ = enc
        same = [f for f in fp.new_funcs if f[0] == name]
        if len(same) > occ:
            _, na, nb = same[occ]
            cand, note = ordinal_match(fp.old, fp.new, (a, b), (na, nb), line)
            func_note = note
            if note == "ok":
                by_func = cand
        else:
            func_note = "function gone"
    by_file = None
    cand, note = ordinal_match(fp.old, fp.new, (1, len(fp.old)), (1, len(fp.new)), line)
    if note == "ok":
        by_file = cand
    if func_note == "ok":
        ordinal = by_func
    elif func_note == "count-mismatch":
        # The anchor is still in the function but occurs a different number of
        # times, so its ordinal no longer identifies one line.
        ordinal = None
    else:
        ordinal = by_file
    diff = fp.diff_map.get(line)
    hint_note = ""
    if func_hint and func != func_hint:
        hint_note = f"; citation names {func_hint}() but the line sits in {func}()"

    def done(cls, new, why=""):
        if cls == "moved" and new == line:
            cls = "unchanged"
        return Resolution(cls, new, func, anchor, why=why + hint_note)

    if diff and diff[1]:
        if ordinal is None or ordinal == diff[0]:
            return done("moved", diff[0])
        return Resolution("ambiguous", None, func, anchor, [ordinal, diff[0]],
                          "anchor ordinal and diff alignment disagree")
    if ordinal is not None:
        # Exact anchor by ordinal, but the diff did not align this line.
        in_func_new = None
        if enc:
            same = [f for f in fp.new_funcs if f[0] == func]
            in_func_new = same[enc[3]] if len(same) > enc[3] else None
        if in_func_new is None or in_func_new[1] <= ordinal <= in_func_new[2]:
            return done("flagged", ordinal, "anchor ordinal only; diff did not align")
        return Resolution("ambiguous", None, func, anchor, [ordinal],
                          "file-wide ordinal lands outside the function")
    if diff and not diff[1]:
        return done("flagged", diff[0], "text changed in place (1:1 diff replacement)")
    # Nothing aligned: collect candidates for the human.
    cands = [i + 1 for i, l in enumerate(fp.new) if norm(l) == anchor and anchor]
    if cands:
        return Resolution("ambiguous", None, func, anchor, cands[:8],
                          f"anchor occurs {len(cands)}x in new file, counts differ ({func_note or 'file'})")
    return Resolution("vanished", None, func, anchor,
                      why=("function removed" if func_note == "function gone" else "anchor text gone"))


# ------------------------------------------------------------------ scanning

QUOTED = re.compile(r'"([^"]{8,60})"|`([^`]{8,60})`')


def anchor_fit(text: str, manifest: dict[str, str], cache: dict) -> tuple[int, int]:
    """(citations whose quoted anchor sits within 4 lines of the cited line in
    this tree, anchor-bearing citations that resolve in it at all).

    Uses the drift audit's anchor rule - a backticked or quoted span of 8-60
    characters with a space in it.
    """
    hit = total = 0
    for ln in text.split("\n"):
        spans = [a or b for a, b in QUOTED.findall(ln)]
        spans = [q.strip() for q in spans if " " in q and ".c:" not in q]
        if not spans:
            continue
        for m in CITATION.finditer(ln):
            rel = resolve(split_release(m.group("path"))[2], manifest)
            if rel is None:
                continue
            key = manifest[rel]
            if key not in cache:
                cache[key] = read_lines(key)
            n = int(NUMBER.match(m.group("nums")).group(0))
            locs = [i + 1 for i, l in enumerate(cache[key]) for q in spans if q in l]
            if not locs:
                continue
            total += 1
            hit += min(abs(p - n) for p in locs) <= 4
    return hit, total


def tracks_old_pin(text: str, old_m: dict[str, str], prior_m: dict[str, str] | None,
                   cache: dict) -> bool:
    """Whether a record file was written against (or retargeted at) the old pin.

    Its quoted anchors must sit at the cited lines in the old pin for at least
    two citations and at least half of them - one agreeing citation is not
    evidence. When the release BEFORE the old pin is supplied, the old pin must
    also fit strictly better than it: a construct that did not move between the
    two matches both, and a note written against the earlier release must keep
    that release's numbers.
    """
    hit, total = anchor_fit(text, old_m, cache)
    if total < 2 or 2 * hit < total:
        return False
    if prior_m is None:
        return True
    return hit > anchor_fit(text, prior_m, cache)[0]


def git_files(root: str) -> list[str]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=root, capture_output=True, check=True).stdout
    return [p for p in out.decode().split("\0") if p]


@dataclass
class Hit:
    source: str
    line: int
    path: str
    upstream: str
    numbers: list[tuple[int, Resolution]]
    prefixed: bool


def process(args) -> tuple[list[Hit], dict[str, str], dict[str, list[str]]]:
    old_m = derive_manifest(args.from_dir)
    new_m = derive_manifest(args.to_dir)
    prior_m = derive_manifest(args.prior_dir) if args.prior_dir else None
    pairs: dict[str, FilePair] = {}
    old_text_cache: dict[str, list[str]] = {}
    hits: list[Hit] = []
    rewrites: dict[str, str] = {}
    records: dict[str, list[str]] = {"kept": [], "retargeted": []}
    for src in git_files(args.root):
        if src in SKIP_SOURCES or src.startswith(SKIP_PREFIXES):
            continue
        full = os.path.join(args.root, src)
        try:
            with open(full, encoding="utf-8", newline="") as fh:
                text = fh.read()
        except (UnicodeDecodeError, OSError):
            continue
        if not any(f".{e}:" in text or f".{e} line" in text for e in CITED_EXTENSIONS):
            continue
        if src.startswith(RECORD_PREFIXES):
            # A record named for a release (`upstream-3.5.0-rrsync-model.md`)
            # documents that release and keeps its lines even when they match.
            if RELEASE_IN_NAME.search(src) or not tracks_old_pin(text, old_m, prior_m, old_text_cache):
                records["kept"].append(src)
                continue
            records["retargeted"].append(src)
        out_lines = []
        changed = False
        for lineno, ln in enumerate(text.split("\n"), 1):
            new_ln, line_hits = rewrite_line(ln, src, lineno, args, old_m, new_m, pairs)
            hits.extend(line_hits)
            if new_ln != ln:
                changed = True
            out_lines.append(new_ln)
        if changed:
            rewrites[src] = "\n".join(out_lines)
    return hits, rewrites, records


def rewrite_line(ln, src, lineno, args, old_m, new_m, pairs):
    hits = []
    pieces = []
    pos = 0
    for m in CITATION.finditer(ln):
        path = m.group("path")
        version, prefix, tail = split_release(path)
        if version is not None and version != args.from_ver:
            continue
        rel = resolve(tail, old_m)
        if rel is None:
            continue
        if m.group("word") and version is None and not re.search(r"\.(c|h)\slines?\s", m.group(0)):
            continue
        if rel not in new_m:
            fp = None
        else:
            if rel not in pairs:
                pairs[rel] = FilePair.load(rel, old_m[rel], new_m[rel])
            fp = pairs[rel]
        nums_text = m.group("nums")
        manual = args.resolutions.get(f"{rel}:{nums_text}")
        results = []
        for nm in NUMBER.finditer(nums_text):
            n = int(nm.group(0))
            if manual is not None:
                cls = "resolved" if manual["to"] else "vanished"
                res = Resolution(cls, None, None, "", why=manual["why"])
            elif fp is None:
                res = Resolution("vanished", None, None, "", why="file removed in new release")
            else:
                res = remap_line(fp, n, m.group("func"))
            results.append((nm, n, res))
        ok = all(r.cls in ("unchanged", "moved", "flagged", "resolved") for _, _, r in results)
        new_nums = nums_text
        if manual is not None and ok:
            new_nums = manual["to"]
            for _, _, r in results:
                r.new = manual["to"]
        elif ok:
            parts, last = [], 0
            for nm, n, r in results:
                parts.append(nums_text[last:nm.start()])
                parts.append(str(r.new))
                last = nm.end()
            parts.append(nums_text[last:])
            new_nums = "".join(parts)
            # A range whose ends moved apart must still run forwards.
            for a, b in re.findall(r"(\d+)-(\d+)", new_nums):
                if int(b) < int(a):
                    ok = False
                    for i, (nm, n, r) in enumerate(results):
                        results[i] = (nm, n, Resolution("ambiguous", None, r.func, r.anchor,
                                                        [r.new], "remapped range runs backwards"))
                    new_nums = nums_text
                    break
        new_path = path
        if version is not None and ok:
            new_path = prefix.replace(f"rsync-{args.from_ver}/", f"rsync-{args.to_ver}/") + tail
        replacement = new_path + ln[m.end("path"):m.start("nums")] + new_nums
        pieces.append(ln[pos:m.start()])
        pieces.append(replacement)
        pos = m.end()
        hits.append(Hit(src, lineno, m.group(0), rel, [(n, r) for _, n, r in results], version is not None))
    pieces.append(ln[pos:])
    return "".join(pieces), hits


def summarise(hits: list[Hit]) -> dict:
    per_class = Counter()
    per_group = defaultdict(Counter)
    items = []
    for h in hits:
        group = h.source.split("/")[1] if h.source.startswith("crates/") else h.source.split("/")[0]
        for n, r in h.numbers:
            per_class[r.cls] += 1
            per_group[group][r.cls] += 1
            if r.cls != "unchanged":
                items.append({
                    "source": f"{h.source}:{h.line}", "citation": h.path, "upstream": h.upstream,
                    "old": n, "new": r.new, "class": r.cls, "function": r.func,
                    "anchor": r.anchor[:100], "candidates": r.candidates, "why": r.why,
                    "prefixed": h.prefixed,
                })
        per_group[group]["citations"] += 1
    return {"per_class": dict(per_class), "per_group": {k: dict(v) for k, v in sorted(per_group.items())},
            "items": items}


def load_resolutions(path: str | None, key: str | None) -> dict:
    """Hand resolutions for citations the tool must not guess.

    Keyed by the WHOLE old citation (`sender.c:295-308`), not by line, because
    one old line can sit inside two citations that mean different constructs.
    `to: null` records a construct with no counterpart: the citation is left as
    written and reported, never silently repointed.
    """
    if not path:
        return {}
    with open(path) as fh:
        data = json.load(fh)
    if key:
        data = data[key]["resolutions"]
    return data


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--from", dest="from_ver", required=True)
    ap.add_argument("--to", dest="to_ver", required=True)
    ap.add_argument("--from-dir", required=True)
    ap.add_argument("--to-dir", required=True)
    ap.add_argument("--prior-dir", help="the release before --from; decides which "
                    "record files (docs/, changelogs) were retargeted at --from")
    ap.add_argument("--root", default=".")
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--report")
    ap.add_argument("--resolutions", help="JSON file of hand resolutions, "
                    "{'<file>:<old numbers>': {'to': '<new numbers>' | null, 'why': ...}}")
    ap.add_argument("--resolutions-key", help="read the resolutions from this key of the file")
    args = ap.parse_args()
    args.resolutions = load_resolutions(args.resolutions, args.resolutions_key)
    for d in (args.from_dir, args.to_dir, args.prior_dir or args.from_dir):
        if not os.path.isdir(d):
            sys.exit(f"upstream tree missing: {d}")
    hits, rewrites, records = process(args)
    if not hits:
        sys.exit("refusing to report: found ZERO citations; the extractor or the trees are wrong")
    report = summarise(hits)
    report["files_rewritten"] = len(rewrites)
    report["record_files"] = records
    print(json.dumps({"per_class": report["per_class"], "files_rewritten": len(rewrites),
                      "citations": len(hits),
                      "record_files_retargeted": len(records["retargeted"]),
                      "record_files_kept_as_written": len(records["kept"])}, indent=2))
    for it in report["items"]:
        if it["class"] in ("ambiguous", "vanished"):
            print(f"{it['class'].upper():9} {it['source']}: {it['citation']} "
                  f"[{it['function']}] '{it['anchor'][:60]}' {it['candidates']} ({it['why']})")
    if args.report:
        with open(args.report, "w") as fh:
            json.dump(report, fh, indent=1, sort_keys=True)
            fh.write("\n")
    if args.apply:
        for src, text in rewrites.items():
            with open(os.path.join(args.root, src), "w", encoding="utf-8", newline="") as fh:
                fh.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
