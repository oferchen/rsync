"""Tests for the daily fuzz sweep matrix.

The old daily sweep listed its targets by hand and missed 14 of 36; it only
replayed corpora, and a blanket continue-on-error hid every failure. These
tests pin what the replacement depends on: every target file is a declared
bin, every declared bin is fuzzed by exactly one schedule, committed seed
corpora are named so cargo-fuzz finds them, and the workflow fuzzes (not just
replays) and fails on a finding.
"""

from __future__ import annotations

import json
import tempfile
import tomllib
import unittest
from pathlib import Path

from tools.ci.fuzz_target_matrix import (
    OVERNIGHT_WORKFLOWS,
    REPO,
    SHARD_SIZE,
    WORKSPACES,
    build_matrix,
    fuzz_bins,
    main,
    overnight_targets,
)

WORKFLOW = REPO / ".github" / "workflows" / "fuzz-coverage-report.yml"


def _write_workspace(root: Path, workspace: str, bins: list[str]) -> None:
    ws = root / workspace
    ws.mkdir(parents=True)
    body = '[package]\nname = "x"\n\n[package.metadata]\ncargo-fuzz = true\n'
    for name in bins:
        body += f'\n[[bin]]\nname = "{name}"\npath = "fuzz_targets/{name}.rs"\n'
    (ws / "Cargo.toml").write_text(body)


def _write_overnight(root: Path, targets: list[str]) -> None:
    for i, workflow in enumerate(OVERNIGHT_WORKFLOWS):
        path = root / workflow
        path.parent.mkdir(parents=True, exist_ok=True)
        rows = "".join(f"          - target: {t}\n" for t in targets[i::2])
        path.write_text(f"jobs:\n  fuzz:\n    strategy:\n      matrix:\n        include:\n{rows}")


def _cells_targets(cells: list[dict[str, str]]) -> list[str]:
    return [t for cell in cells for t in cell["targets"].split()]


class FuzzTargetMatrixTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _fixture(self, per_workspace: dict[str, list[str]], overnight: list[str]) -> None:
        for workspace in WORKSPACES:
            _write_workspace(self.root, workspace, per_workspace.get(workspace, ["only"]))
        _write_overnight(self.root, overnight)

    def test_an_overnight_target_no_workspace_declares_is_an_error(self) -> None:
        # A renamed or deleted overnight target would otherwise drop out of
        # both schedules without anyone noticing.
        self._fixture({"fuzz": ["a", "slow_diff", "b"]}, ["slow_diff", "other_diff"])
        with self.assertRaisesRegex(ValueError, "other_diff"):
            build_matrix(self.root)

    def test_overnight_targets_are_left_out_of_the_daily_sweep(self) -> None:
        self._fixture(
            {"fuzz": ["a", "slow_diff", "b"], "crates/protocol/fuzz": ["p"]},
            ["slow_diff", "slow_diff"],
        )
        cells = build_matrix(self.root)
        self.assertEqual(_cells_targets(cells), ["a", "b", "p", "only"])
        self.assertEqual(
            [c["workspace"] for c in cells],
            ["fuzz", "crates/protocol/fuzz", "crates/filters/fuzz"],
        )

    def test_shards_split_at_shard_size_within_a_workspace(self) -> None:
        names = [f"t{i}" for i in range(SHARD_SIZE + 1)]
        self._fixture({"fuzz": names + ["d"]}, ["d", "d"])
        cells = [c for c in build_matrix(self.root) if c["workspace"] == "fuzz"]
        self.assertEqual([len(c["targets"].split()) for c in cells], [SHARD_SIZE, 1])
        self.assertEqual([c["shard"] for c in cells], ["fuzz-1", "fuzz-2"])

    def test_a_workspace_without_bins_is_an_error(self) -> None:
        # A typo'd or emptied manifest must fail the plan, not sweep nothing.
        self._fixture({"crates/filters/fuzz": []}, ["d", "d"])
        with self.assertRaisesRegex(ValueError, "declares no"):
            build_matrix(self.root)

    def test_a_non_cargo_fuzz_manifest_is_rejected(self) -> None:
        ws = self.root / "w"
        ws.mkdir()
        (ws / "Cargo.toml").write_text('[package]\nname = "w"\n[[bin]]\nname = "x"\n')
        with self.assertRaisesRegex(ValueError, "not a cargo-fuzz"):
            fuzz_bins("w", self.root)

    def test_main_rejects_arguments(self) -> None:
        self.assertEqual(main(["extra"]), 2)

    # The real tree.

    def test_every_target_file_is_a_declared_bin(self) -> None:
        # A fuzz_targets/*.rs without a [[bin]] entry is never built, so the
        # matrix (which reads [[bin]]) would silently skip it.
        for workspace in WORKSPACES:
            manifest = tomllib.loads((REPO / workspace / "Cargo.toml").read_text())
            declared = {Path(b["path"]).name for b in manifest["bin"]}
            on_disk = {p.name for p in (REPO / workspace / "fuzz_targets").glob("*.rs")}
            self.assertEqual(declared, on_disk, workspace)

    def test_every_bin_has_exactly_one_schedule(self) -> None:
        cells = build_matrix()
        daily = _cells_targets(cells)
        overnight = overnight_targets()
        declared = [n for ws in WORKSPACES for n in fuzz_bins(ws)]
        self.assertEqual(len(daily), len(set(daily)), "target in two shards")
        self.assertFalse(set(daily) & overnight, "target fuzzed both daily and overnight")
        self.assertEqual(set(daily) | overnight, set(declared))
        self.assertTrue(all(len(c["targets"].split()) <= SHARD_SIZE for c in cells))

    def test_committed_corpora_are_named_after_their_target(self) -> None:
        # cargo fuzz reads corpus/<target>/ by default; a seed directory under
        # any other name is never loaded (the filters corpus once was).
        for workspace in WORKSPACES:
            corpus = REPO / workspace / "corpus"
            if not corpus.is_dir():
                continue
            bins = set(fuzz_bins(workspace))
            for entry in corpus.iterdir():
                if entry.is_dir():
                    self.assertIn(entry.name, bins, f"{workspace}/corpus/{entry.name}")

    def test_main_prints_the_matrix_as_json(self) -> None:
        from contextlib import redirect_stdout
        from io import StringIO

        buf = StringIO()
        with redirect_stdout(buf):
            self.assertEqual(main([]), 0)
        self.assertEqual(json.loads(buf.getvalue()), build_matrix())

    def test_workflow_fuzzes_and_fails_on_findings(self) -> None:
        text = WORKFLOW.read_text()
        self.assertIn("python3 tools/ci/fuzz_target_matrix.py", text)
        self.assertIn("fromJSON(needs.plan.outputs.matrix)", text)
        self.assertIn("cargo +nightly fuzz run", text)
        self.assertIn("-max_total_time=", text)
        self.assertNotIn("-runs=0", text)
        self.assertNotIn("continue-on-error", text)
        self.assertIn("artifacts/", text)

    def test_weekly_smoke_duplicate_is_gone(self) -> None:
        self.assertFalse((REPO / ".github" / "workflows" / "fuzz-coverage.yml").exists())


if __name__ == "__main__":
    unittest.main()
