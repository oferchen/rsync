"""Unit tests for the test-seam gate.

The manifest tests pin where `test-seams` may be enabled: fast_io's own
`[features]` definition and `[dev-dependencies]` tables only.  The source
tests pin where the seam may be named: the allow-listed test paths and the
three fast_io files that define and read it.  The allow-list itself is pinned
entry by entry, so widening it changes this file as well as the gate.  Each
rejecting case is paired with a clean fixture that the gate passes, so a gate
that rejects everything cannot pass these tests.
"""

from __future__ import annotations

import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

from tools.ci.test_seam_gate import (
    ALLOWED_SOURCE_PATTERNS,
    run_gate,
    source_is_allowed,
)

FAST_IO = (
    '[package]\nname = "fast_io"\n'
    '[features]\ndefault = []\ntest-seams = []\n'
)
CONSUMER_DEV = (
    '[package]\nname = "core"\n'
    '[dependencies]\nfast_io = { path = "../fast_io" }\n'
    '[dev-dependencies]\nfast_io = { path = "../fast_io", features = ["test-seams"] }\n'
)


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self._tempdir.name)
        self.write("crates/fast_io/Cargo.toml", FAST_IO)
        self.write("crates/core/Cargo.toml", CONSUMER_DEV)

    def tearDown(self) -> None:
        self._tempdir.cleanup()

    def write(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def gate(self) -> tuple[int, str]:
        err = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(err):
            code = run_gate(self.root)
        return code, err.getvalue()


class ManifestTests(Fixture):
    def test_the_definition_and_a_dev_dependency_pass(self) -> None:
        self.assertEqual(self.gate()[0], 0)

    def test_a_per_target_dev_dependency_passes(self) -> None:
        self.write(
            "crates/transfer/Cargo.toml",
            '[package]\nname = "transfer"\n'
            "[target.'cfg(unix)'.dev-dependencies]\n"
            'fast_io = { path = "../fast_io", features = ["test-seams"] }\n',
        )
        self.assertEqual(self.gate()[0], 0)

    def test_a_production_dependency_enabling_it_fails(self) -> None:
        self.write(
            "crates/core/Cargo.toml",
            '[package]\nname = "core"\n'
            '[dependencies]\nfast_io = { path = "../fast_io", features = ["test-seams"] }\n',
        )
        code, err = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/core/Cargo.toml [dependencies]", err)

    def test_a_per_target_production_dependency_enabling_it_fails(self) -> None:
        self.write(
            "crates/core/Cargo.toml",
            '[package]\nname = "core"\n'
            "[target.'cfg(unix)'.dependencies]\n"
            'fast_io = { path = "../fast_io", features = ["test-seams"] }\n',
        )
        code, err = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("[target.cfg(unix).dependencies]", err)

    def test_a_build_dependency_enabling_it_fails(self) -> None:
        self.write(
            "crates/core/Cargo.toml",
            '[package]\nname = "core"\n'
            '[build-dependencies]\nfast_io = { path = "../fast_io", features = ["test-seams"] }\n',
        )
        self.assertEqual(self.gate()[0], 1)

    def test_a_workspace_dependency_enabling_it_fails(self) -> None:
        self.write(
            "Cargo.toml",
            '[workspace]\nmembers = []\n'
            '[workspace.dependencies]\n'
            'fast_io = { path = "crates/fast_io", features = ["test-seams"] }\n',
        )
        code, err = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("[workspace.dependencies]", err)

    def test_a_feature_forwarding_to_it_fails(self) -> None:
        self.write(
            "Cargo.toml",
            '[package]\nname = "bin"\n'
            '[features]\ntesting = ["fast_io/test-seams"]\n',
        )
        code, err = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("`testing` forwards to `fast_io/test-seams`", err)

    def test_a_weak_feature_forwarding_to_it_fails(self) -> None:
        self.write(
            "crates/core/Cargo.toml",
            '[package]\nname = "core"\n'
            '[features]\nextra = ["fast_io?/test-seams"]\n',
        )
        self.assertEqual(self.gate()[0], 1)

    def test_another_crate_defining_a_feature_of_that_name_fails(self) -> None:
        self.write(
            "crates/core/Cargo.toml",
            '[package]\nname = "core"\n[features]\ntest-seams = []\n',
        )
        code, err = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("defines `test-seams` outside fast_io", err)


class SourceTests(Fixture):
    CALL = "let _owner = fast_io::test_seams::judge_symlink_owner(\"alt\", 7);\n"

    def test_a_call_in_an_integration_test_passes(self) -> None:
        self.write("crates/core/tests/single_file_destination.rs", self.CALL)
        self.write("tests/local_copy.rs", self.CALL)
        self.assertEqual(self.gate()[0], 0)

    def test_a_call_in_an_in_crate_test_module_passes(self) -> None:
        self.write("crates/core/src/client/run/tests.rs", self.CALL)
        self.write("crates/engine/src/local_copy/tests/mod.rs", self.CALL)
        self.assertEqual(self.gate()[0], 0)

    def test_a_call_in_production_source_fails(self) -> None:
        self.write("crates/core/src/client/run/mod.rs", self.CALL)
        code, err = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("crates/core/src/client/run/mod.rs:1", err)

    def test_a_feature_cfg_in_production_source_fails(self) -> None:
        self.write(
            "crates/transfer/src/receiver.rs",
            '#[cfg(feature = "test-seams")]\nfn hook() {}\n',
        )
        self.assertEqual(self.gate()[0], 1)

    def test_build_output_is_not_scanned(self) -> None:
        self.write("target/debug/build/out/generated.rs", self.CALL)
        self.assertEqual(self.gate()[0], 0)

    def test_a_similar_identifier_is_not_a_reference(self) -> None:
        self.write("crates/core/src/lib.rs", "fn my_test_seams_helper() {}\n")
        self.assertEqual(self.gate()[0], 0)


class RepositoryTests(unittest.TestCase):
    """The gate's enforcement point: tools/ci/run_tools_tests.sh runs this in
    the tools-tests CI job, so the real tree must pass it."""

    def test_the_repository_passes_the_gate(self) -> None:
        root = Path(__file__).resolve().parents[2]
        err = io.StringIO()
        with redirect_stdout(io.StringIO()), redirect_stderr(err):
            code = run_gate(root)
        self.assertEqual(code, 0, err.getvalue())


class AllowListTests(unittest.TestCase):
    def test_the_allow_list_is_exactly_these_entries(self) -> None:
        self.assertEqual(
            ALLOWED_SOURCE_PATTERNS,
            (
                "crates/fast_io/src/lib.rs",
                "crates/fast_io/src/test_seams.rs",
                "crates/fast_io/src/owner_walk.rs",
                "tests/*",
                "crates/*/tests/*",
                "crates/*/src/tests.rs",
                "crates/*/src/*/tests.rs",
                "crates/*/src/tests/*",
                "crates/*/src/*/tests/*",
            ),
        )

    def test_production_files_are_not_allowed(self) -> None:
        for rel in (
            "crates/fast_io/src/owner_walk/mod.rs",
            "crates/fast_io/src/confined_open.rs",
            "crates/core/src/client/run/mod.rs",
            "crates/test-support/src/lib.rs",
            "src/bin/oc-rsync.rs",
            "crates/core/src/attests.rs",
        ):
            self.assertFalse(source_is_allowed(rel), rel)


if __name__ == "__main__":
    unittest.main()
