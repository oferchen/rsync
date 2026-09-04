#!/usr/bin/env python3
"""Classify every Rust comment line so "unhelpful" is a measured set, not an opinion.

The repository's comment corpus is large enough (>240k lines) that a manual
sweep cannot be reviewed, and a previous content-keyed comment audit here had
three of its four classes come back 100% false positive. So a deletion
candidate set has to be produced mechanically, be conservative by construction,
and be inspectable before anything is edited.

A comment is classified as part of its BLOCK, not as a line. That is the single
correction that made the output usable: provenance stated in a block's first
line governs the upstream C quoted beneath it, and a sentence wrapped across
three lines is one claim, not three. Line-scoped classification split both, and
every candidate it produced was a false positive.

Classes, in the order they are decided:

  UPSTREAM   the block names an upstream rsync source location, a CVE, or an
             RERR_ code. NEVER a deletion candidate: these carry the provenance
             that makes a port auditable, and the project rule is to keep them.

  FILLER     a divider or a blank continuation. Carries no claim either way.

  EXAMPLE    a rustdoc line inside a ``` fence. `cargo test --doc` compiles it,
             so it is executable documentation, not commented-out code.

  CONTINUATION  a line that neither ends a sentence nor starts one, i.e. the
             middle or an end of a wrapped claim. It cannot be judged on its
             own words because they are half a sentence.

  COMMENTED  a plain `//` payload that parses as a statement AND carries code
             syntax AND is not prose by stopword count.

  RESTATES   every content word also appears as an identifier token on the code
             line the comment introduces.

  KEEPS      everything else.

MEASURED RESULT, whole tree, 2026-09-04 (241,996 comment lines, 3,103 files):

  UPSTREAM 103,462 (42.8%) | KEEPS 39,632 | FILLER 29,570
  EXAMPLE 5,029 | CONTINUATION 63,085 | RESTATES 217 (0.1%) | COMMENTED 1

  COMMENTED precision: 0 of 42 sites across four successive corrections. The
  single surviving site (crates/filters/src/wildmatch.rs) is upstream C quoted
  on purpose to show what the Rust beneath it transliterates - a keep. There is
  no commented-out code in this tree.

  RESTATES precision: roughly 5 of 20 on a random sample. Its false positives
  are rustdoc summaries on public items (removing them trips `missing_docs`),
  itemize-legend rows, and arithmetic annotations. So the genuinely-redundant
  population is on the order of 50 lines in 242,000 - 0.02%.

The conclusion this instrument produces is therefore that a comment-deletion
sweep is not warranted. Run it to keep that answer current, not to generate a
work list. `--json` emits the per-site list so any claim above can be audited.
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import subprocess
import sys

# A comment naming an rsync source file - with or without a line number - a
# header, or simply saying "upstream", is the provenance trail for a port.
# Matching loosely is deliberate: a false UPSTREAM is a comment we decline to
# touch, which is the safe direction.
#
# The shape is described rather than spelled out, because writing an example
# citation here makes this file cite a source location that does not exist, and
# the drift gate reads it as a real one.
UPSTREAM = re.compile(
    r"""\b[a-z_0-9]+\.[ch]\b       # rsync source file, with or without a line
      | \bupstream\b
      | \brsync-3\.[0-9]
      | \bRERR_[A-Z]+
      | \bCVE-\d{4}-\d+
    """,
    re.IGNORECASE | re.VERBOSE,
)

# Payload shapes that are code rather than prose.
#
# ⚠ Both conditions below were added after the first version of this script
# measured ~100% false positives on a 22-site sample. Prose that happens to end
# in a semicolon ("...the binary prologue;") matched a bare `.*;$`, and every
# line of a rustdoc example matched too. The extra evidence requirements and
# the fence tracking in `classify` are what make this class usable.
CODE_SHAPE = re.compile(
    r"""^ (?: [}{]\s*$                     # a lone brace
            | .*[;,]\s*$                   # a statement or a trailing element
            | (?:pub\s+)?(?:fn|let|use|impl|struct|enum|match|if|for|while)\b.*[({]
          )$""",
    re.VERBOSE,
)

# A statement needs syntax, not just a terminator: an assignment, a call, a
# path, or an arrow. Prose almost never carries these; code almost always does.
CODE_SYNTAX = re.compile(r"(?:=|\(|\)|::|->|=>|\[)")

# Words that carry no discriminating content when comparing prose to code.
STOPWORDS = frozenset(
    """a an and are as at be but by for from has have if in into is it its of on
    or so than that the then there these this to via was were when which while
    with we our us you your not no all any each per both same other only just
    also more most much such because since after before above below up down out
    over under again once here how why what who whom whose""".split()
)

WORD = re.compile(r"[A-Za-z][A-Za-z0-9]*")


def identifier_tokens(code: str) -> set[str]:
    """Split a code line into lowercase identifier fragments.

    `snake_case` splits on `_`; `camelCase` and `PascalCase` split on the case
    boundary. Both forms are reduced to the same vocabulary so a comment saying
    "file entry" matches `file_entry`, `FileEntry` and `fileEntry` alike.
    """
    out: set[str] = set()
    for raw in WORD.findall(code):
        for part in re.findall(r"[A-Z]+(?![a-z])|[A-Z][a-z0-9]*|[a-z0-9]+", raw):
            out.add(part.lower())
    return out


def content_words(text: str) -> list[str]:
    """The comment's discriminating words: alphabetic, >=3 chars, not a stopword."""
    return [
        w.lower()
        for w in WORD.findall(text)
        if len(w) >= 3 and w.lower() not in STOPWORDS
    ]


def strip_marker(line: str) -> tuple[str, str] | None:
    """Split a comment line into (marker, payload), or None if it is not one."""
    t = line.strip()
    for marker in ("///", "//!", "//"):
        if t.startswith(marker):
            return marker, t[len(marker) :].strip()
    return None


def next_code_line(lines: list[str], start: int) -> str | None:
    """The first following line that is neither blank, comment, nor attribute."""
    for probe in lines[start + 1 : start + 8]:
        t = probe.strip()
        if not t or t.startswith("//") or t.startswith("#["):
            continue
        return t
    return None


def in_doc_example(lines: list[str], index: int) -> bool:
    """True when this doc-comment line sits inside a ``` fence.

    Rustdoc examples are compiled and run by `cargo test --doc`, so every line
    of one is executable documentation, not commented-out code. Counting fences
    from the top of the file is what separates the two; without it the entire
    example body reads as `COMMENTED`.
    """
    fences = 0
    for probe in lines[: index + 1]:
        split = strip_marker(probe)
        if split is None:
            continue
        marker, payload = split
        if marker in ("///", "//!") and payload.lstrip().startswith("```"):
            fences += 1
    # The fence line itself is odd-numbered and belongs to the example too.
    return fences % 2 == 1 or (
        fences > 0
        and (strip_marker(lines[index]) or ("", ""))[1].lstrip().startswith("```")
    )


def block_bounds(lines: list[str], index: int) -> tuple[int, int]:
    """The contiguous run of comment lines containing `index`.

    A comment is a paragraph, not a line. Provenance stated in the first line
    ("upstream: exclude.c:88-123 ...") governs the quoted C that follows it,
    and a sentence wrapped across three lines is one claim. Classifying lines
    independently splits both, which is what made the quoted-upstream-C lines
    read as commented-out code.
    """
    lo = index
    while lo > 0 and strip_marker(lines[lo - 1]) is not None:
        lo -= 1
    hi = index
    while hi + 1 < len(lines) and strip_marker(lines[hi + 1]) is not None:
        hi += 1
    return lo, hi


def classify(lines: list[str], index: int) -> str:
    split = strip_marker(lines[index])
    if split is None:
        return "NOT_A_COMMENT"
    marker, payload = split

    if not payload or set(payload) <= set("-=*/ !#"):
        # A divider or an empty continuation line. Carries no claim either way,
        # so it is neither a keep nor a candidate.
        return "FILLER"

    # Provenance is a property of the whole paragraph. A block that cites an
    # upstream site anywhere protects every line it quotes or explains.
    lo, hi = block_bounds(lines, index)
    for probe in lines[lo : hi + 1]:
        pair = strip_marker(probe)
        if pair and UPSTREAM.search(pair[1]):
            return "UPSTREAM"

    is_doc = marker in ("///", "//!")
    if is_doc and in_doc_example(lines, index):
        return "EXAMPLE"

    # A wrapped sentence is one claim spread over several lines, and neither
    # half can be judged on its own words. Both directions matter and the
    # forward one is the more common: `// ...adjustment (601 / 599 bytes),`
    # opens a paragraph and reads as a statement only because the sentence was
    # cut mid-clause. The backward case is the tail, `/// [`SOME_LINK`].`.
    ends_a_sentence = payload[-1] in ".:!?"
    if index > lo:
        prev = strip_marker(lines[index - 1])
        if prev and prev[1] and prev[1][-1] not in ".:!?":
            return "CONTINUATION"
    if index < hi and not ends_a_sentence:
        return "CONTINUATION"

    # Only a plain `//` can be commented-out code. A `///` line is documentation
    # by definition, and its code lives in the fenced examples handled above.
    if (
        not is_doc
        and CODE_SHAPE.match(payload)
        and CODE_SYNTAX.search(payload)
        and sum(1 for w in WORD.findall(payload) if w.lower() in STOPWORDS) <= 1
    ):
        return "COMMENTED"

    words = content_words(payload)
    if len(words) >= 2:
        code = next_code_line(lines, index)
        if code and set(words) <= identifier_tokens(code):
            return "RESTATES"

    return "KEEPS"


def tracked_rust_files() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "--", "*.rs"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [p for p in out.split("\n") if p]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--json", metavar="PATH", help="write the per-site candidate list here"
    )
    ap.add_argument(
        "--crate",
        action="append",
        default=[],
        help="restrict to crates/<name>/ (repeatable)",
    )
    args = ap.parse_args()

    counts: collections.Counter[str] = collections.Counter()
    per_crate: dict[str, collections.Counter[str]] = collections.defaultdict(
        collections.Counter
    )
    sites: list[dict[str, object]] = []

    for path in tracked_rust_files():
        if args.crate:
            if not any(path.startswith(f"crates/{c}/") for c in args.crate):
                continue
        crate = path.split("/")[1] if path.startswith("crates/") else "<root>"
        try:
            lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines):
            verdict = classify(lines, i)
            if verdict == "NOT_A_COMMENT":
                continue
            counts[verdict] += 1
            per_crate[crate][verdict] += 1
            if verdict in ("COMMENTED", "RESTATES"):
                sites.append(
                    {
                        "path": path,
                        "line": i + 1,
                        "class": verdict,
                        "text": line.strip(),
                    }
                )

    order = [
        "UPSTREAM",
        "KEEPS",
        "EXAMPLE",
        "CONTINUATION",
        "FILLER",
        "RESTATES",
        "COMMENTED",
    ]
    total = sum(counts.values())
    print(f"comment lines classified: {total}")
    for k in order:
        pct = (100.0 * counts[k] / total) if total else 0.0
        print(f"  {k:<10} {counts[k]:>7}  {pct:5.1f}%")
    print()
    print("candidates (COMMENTED + RESTATES) by crate:")
    ranked = sorted(
        per_crate.items(),
        key=lambda kv: kv[1]["COMMENTED"] + kv[1]["RESTATES"],
        reverse=True,
    )
    for crate, c in ranked:
        n = c["COMMENTED"] + c["RESTATES"]
        if n:
            print(f"  {crate:<14} {n:>5}  (commented {c['COMMENTED']}, restates {c['RESTATES']})")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(sites, fh, indent=2)
        print(f"\nwrote {len(sites)} candidate sites to {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
