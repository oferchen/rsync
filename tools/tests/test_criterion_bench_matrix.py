"""Tests for the per-bench-target Criterion release matrix.

The release run timed out because one cell ran a whole crate's benches in
series. These tests pin the two properties the fix depends on: every declared
bench target gets its own cell, and the workflow builds its matrix from this
script over the same crates it benchmarks.
"""

from __future__ import annotations

import re
import tempfile
import unittest
from pathlib import Path

from tools.ci.criterion_bench_matrix import MAX_CELLS, REPO, bench_targets, build_matrix, main

WORKFLOW = REPO / ".github" / "workflows" / "benchmark-release.yml"


def _write_crate(root: Path, name: str, benches: list[str], package: str | None = None) -> None:
    crate = root / "crates" / name
    crate.mkdir(parents=True)
    body = f'[package]\nname = "{package or name}"\n'
    for bench in benches:
        body += f'\n[[bench]]\nname = "{bench}"\nharness = false\n'
    (crate / "Cargo.toml").write_text(body)


def _workflow_crates() -> list[str]:
    match = re.search(r"criterion_bench_matrix\.py ([a-z_ ]+)", WORKFLOW.read_text())
    assert match, "benchmark-release.yml does not call criterion_bench_matrix.py"
    return match.group(1).split()


class CriterionBenchMatrixTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_one_cell_per_declared_bench_target(self) -> None:
        _write_crate(self.root, "alpha", ["a_one", "a_two"])
        _write_crate(self.root, "beta", ["b_one"])
        self.assertEqual(
            build_matrix(["alpha", "beta"], self.root),
            [
                {"crate": "alpha", "bench": "a_one"},
                {"crate": "alpha", "bench": "a_two"},
                {"crate": "beta", "bench": "b_one"},
            ],
        )

    def test_a_crate_without_benches_is_an_error(self) -> None:
        # A typo in the workflow's crate list must fail the plan, not silently
        # benchmark nothing for that crate.
        _write_crate(self.root, "empty", [])
        with self.assertRaisesRegex(ValueError, "declares no"):
            build_matrix(["empty"], self.root)

    def test_directory_and_package_name_must_agree(self) -> None:
        # `cargo bench -p` takes the package name; a mismatch would build nothing.
        _write_crate(self.root, "dir", ["x"], package="other")
        with self.assertRaisesRegex(ValueError, "package 'other'"):
            bench_targets("dir", self.root)

    def test_matrix_over_github_limit_is_rejected(self) -> None:
        _write_crate(self.root, "huge", [f"b{i}" for i in range(MAX_CELLS + 1)])
        with self.assertRaisesRegex(ValueError, "matrix limit"):
            build_matrix(["huge"], self.root)

    def test_main_rejects_missing_arguments(self) -> None:
        self.assertEqual(main([]), 2)

    def test_real_workflow_matrix_covers_every_bench_file(self) -> None:
        # Every benches/*.rs of the benchmarked crates is a declared target,
        # so a bench added without a [[bench]] entry is caught here too.
        crates = _workflow_crates()
        cells = build_matrix(crates)
        declared = {(c["crate"], c["bench"]) for c in cells}
        on_disk = {
            (crate, path.stem)
            for crate in crates
            for path in (REPO / "crates" / crate / "benches").glob("*.rs")
        }
        self.assertEqual(declared, on_disk)
        self.assertEqual(len(declared), len(cells), "duplicate bench target")

    def test_workflow_runs_one_bench_target_per_cell(self) -> None:
        text = WORKFLOW.read_text()
        self.assertIn("fromJSON(needs.plan.outputs.matrix)", text)
        self.assertIn("--bench ${{ matrix.bench }}", text)
        self.assertNotRegex(text, r"cargo bench -p \$\{\{ matrix\.crate \}\} --all-features")


if __name__ == "__main__":
    unittest.main()
