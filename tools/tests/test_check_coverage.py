"""Unit tests for the coverage gate helper."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.check_coverage import enforce_thresholds, load_metrics


class CheckCoverageTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.summary_path = Path(self._tempdir.name, "summary.json")
        self.summary_path.write_text(
            json.dumps(
                {
                    "data": [
                        {
                            "totals": {
                                "lines": {"count": 200, "covered": 190, "percent": 95.0},
                                "functions": {"count": 40, "covered": 39, "percent": 97.5},
                                "regions": {"count": 10, "covered": 8, "percent": 80.0},
                            }
                        }
                    ]
                }
            )
        )

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def test_load_metrics_extracts_percentages(self) -> None:
        metrics = load_metrics(self.summary_path)
        self.assertIn("lines", metrics)
        self.assertIn("functions", metrics)
        self.assertAlmostEqual(metrics["lines"].percent, 95.0)
        self.assertEqual(metrics["regions"].covered, 8.0)

    def test_enforce_thresholds_handles_success_and_failure(self) -> None:
        metrics = load_metrics(self.summary_path)
        self.assertEqual(0, enforce_thresholds(metrics, {"lines": 90.0, "functions": 95.0}))
        self.assertEqual(1, enforce_thresholds(metrics, {"branches": 10.0}))
        self.assertEqual(1, enforce_thresholds(metrics, {"regions": 85.0}))


if __name__ == "__main__":
    unittest.main()
