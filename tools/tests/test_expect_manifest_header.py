"""The expect-manifest `# Outcome:` header must agree with its own rows.

`tools/ci/run_upstream_testsuite.sh` derives that line from the rows it just
wrote, so the two can only disagree if someone hand-edits a file whose first
line says "do not hand-edit". Measured 2026-09-05: exactly that had happened -
the macOS manifest's header claimed "236 passed / 4 failed / 105 skipped" over
rows reading 238 pass / 2 fail / 105 skip, and the header's own prose two lines
below said "the 2 `fail` rows", contradicting itself as well as the data.

A reader quoting the header quotes the residual, so a drifted header
misreports how far the port is from upstream parity.
"""

from __future__ import annotations

import re
import unittest
from collections import Counter
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MANIFEST_DIR = REPO_ROOT / "tools" / "ci"
MANIFEST_GLOB = "upstream-*-expect*.txt"

OUTCOME_RE = re.compile(
    r"^#\s*Outcome:\s*(\d+) pass / (\d+) fail / (\d+) skip / (\d+) xfail\s*$"
)
VALID_OUTCOMES = ("pass", "fail", "skip", "xfail")


def _manifests() -> list[Path]:
    return sorted(MANIFEST_DIR.glob(MANIFEST_GLOB))


def _tally(text: str) -> Counter:
    """Count the outcome of every non-comment row, upstream's parse order.

    Mirrors runtests.py's `parse_expect_result` (rsync-3.5.0/runtests.py:332):
    strip from '#', skip blanks, then split into exactly two fields.
    """
    counts: Counter = Counter()
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        assert len(fields) == 2, f"malformed row: {raw!r}"
        counts[fields[1]] += 1
    return counts


class ExpectManifestHeaderTests(unittest.TestCase):
    def test_manifests_are_discovered(self) -> None:
        # Without this the whole suite would pass by matching zero files - the
        # vacuity the other assertions cannot detect on their own.
        self.assertGreaterEqual(
            len(_manifests()), 5, f"no expect manifests matched {MANIFEST_GLOB}"
        )

    def test_outcome_header_agrees_with_its_rows(self) -> None:
        checked = 0
        for path in _manifests():
            text = path.read_text()
            headers = [
                m for m in (OUTCOME_RE.match(ln) for ln in text.splitlines()) if m
            ]
            self.assertLessEqual(
                len(headers), 1, f"{path.name}: more than one Outcome header"
            )
            if not headers:
                # Manifests generated before the emitter wrote the line carry
                # no claim; a missing header cannot drift.
                continue
            checked += 1
            claimed = tuple(int(g) for g in headers[0].groups())
            counts = _tally(text)
            actual = tuple(counts[name] for name in VALID_OUTCOMES)
            self.assertEqual(
                claimed,
                actual,
                f"{path.name}: header claims {claimed} but rows are {actual} "
                f"(order: {VALID_OUTCOMES}). Regenerate with EMIT_EXPECT_RESULT "
                f"rather than editing the header.",
            )
        self.assertGreater(
            checked, 0, "no manifest carried an Outcome header to check"
        )

    def test_every_row_names_a_valid_outcome(self) -> None:
        for path in _manifests():
            counts = _tally(path.read_text())
            unknown = set(counts) - set(VALID_OUTCOMES)
            self.assertFalse(unknown, f"{path.name}: unknown outcomes {unknown}")


if __name__ == "__main__":
    unittest.main()
