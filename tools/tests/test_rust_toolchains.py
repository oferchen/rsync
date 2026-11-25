"""Tests for tools/ci/rust_toolchains.py, the CI toolchain pins and their refresh.

The refresh job turns a published channel manifest into edits of the pin
files and, when stable moves, the MSRV. These tests pin what makes that safe
to run unattended: recorded manifests resolve to the right toolchains, a
malformed manifest is refused, and a refused refresh writes nothing.
"""

from __future__ import annotations

import io
import os
import re
import shutil
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

import yaml

from tools.ci import rust_toolchains as rt

REPO = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "data" / "rust_channels"

# The real manifests as published on the day the pins were last set by hand.
RECORDED = {
    "stable": rt.Pin("stable", "1.100.0", "1.100.0"),
    "beta": rt.Pin("beta", "beta-2026-11-23", "1.101.0-beta.3"),
    "nightly": rt.Pin("nightly", "nightly-2026-11-24", "1.102.0-nightly"),
}


def manifest(date: str, version: str, manifest_version: str = "2") -> str:
    return (
        f'manifest-version = "{manifest_version}"\n'
        f'date = "{date}"\n\n'
        "[pkg.rust]\n"
        f'version = "{version} (0123456789 {date})"\n'
    )


def recorded(channel: str) -> str:
    return (FIXTURES / f"channel-rust-{channel}.toml").read_text()


class ParseManifestTests(unittest.TestCase):
    def test_recorded_manifests_resolve_to_the_pinned_toolchains(self) -> None:
        for channel, pin in RECORDED.items():
            with self.subTest(channel=channel):
                self.assertEqual(rt.parse_manifest(channel, recorded(channel)), pin)

    def test_a_stable_toolchain_is_its_version_and_others_are_dated(self) -> None:
        self.assertEqual(rt.parse_manifest("stable", manifest("2027-01-08", "1.103.0")).toolchain, "1.103.0")
        self.assertEqual(
            rt.parse_manifest("beta", manifest("2027-01-08", "1.104.0-beta.1")).toolchain, "beta-2027-01-08"
        )

    def test_malformed_manifests_are_refused(self) -> None:
        cases = {
            "not toml": ("beta", "manifest-version = \n"),
            "wrong manifest version": ("beta", manifest("2026-11-23", "1.101.0-beta.3", manifest_version="1")),
            "malformed date": ("beta", manifest("23-11-2026", "1.101.0-beta.3")),
            "missing pkg.rust": ("beta", 'manifest-version = "2"\ndate = "2026-11-23"\n'),
            "stable manifest read as beta": ("beta", recorded("stable")),
            "beta manifest read as stable": ("stable", recorded("beta")),
            "nightly manifest read as beta": ("beta", recorded("nightly")),
        }
        for name, (channel, text) in cases.items():
            with self.subTest(name):
                with self.assertRaises(rt.PinError):
                    rt.parse_manifest(channel, text)


class TreeTestCase(unittest.TestCase):
    """Runs against a scratch tree holding copies of every file refresh edits."""

    EDITED = sorted(
        {rt.TOOLCHAIN_FILE, rt.PINS_FILE} | {site for site, _pattern, _count in rt.MSRV_SITES}
    )

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        for relative in self.EDITED:
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPO / relative, target)
        self.manifests = self.root / "manifests"
        self.manifests.mkdir()
        for channel in rt.CHANNELS:
            (self.manifests / f"channel-rust-{channel}.toml").write_text(recorded(channel))

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def snapshot(self) -> dict[str, str]:
        return {relative: (self.root / relative).read_text() for relative in self.EDITED}

    def publish(self, channel: str, text: str) -> None:
        (self.manifests / f"channel-rust-{channel}.toml").write_text(text)

    def refresh(self) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = rt.main(["--root", str(self.root), "refresh", "--manifest-dir", str(self.manifests)])
        return code, out.getvalue(), err.getvalue()


class RepoPinTests(TreeTestCase):
    def test_the_tree_is_pinned_to_the_recorded_releases(self) -> None:
        self.assertEqual(rt.read_pins(REPO), RECORDED)

    def test_refresh_against_the_recorded_manifests_changes_nothing(self) -> None:
        before = self.snapshot()
        code, out, _err = self.refresh()
        self.assertEqual(code, 0)
        self.assertIn("changed=false", out)
        self.assertEqual(self.snapshot(), before)

    def test_resolve_prints_the_pin_for_the_action(self) -> None:
        for channel, pin in RECORDED.items():
            out = io.StringIO()
            with redirect_stdout(out):
                self.assertEqual(rt.main(["--root", str(self.root), "resolve", channel]), 0)
            self.assertEqual(out.getvalue(), f"toolchain={pin.toolchain}\nversion={pin.version}\n")

    def test_resolve_writes_lf_only_github_output(self) -> None:
        # A CRLF here made the version compare fail on the Windows stable cell.
        output = self.root / "github_output"
        os.environ["GITHUB_OUTPUT"] = str(output)
        try:
            with redirect_stdout(io.StringIO()):
                self.assertEqual(rt.main(["--root", str(self.root), "resolve", "beta"]), 0)
        finally:
            del os.environ["GITHUB_OUTPUT"]
        pin = RECORDED["beta"]
        self.assertEqual(output.read_bytes(), f"toolchain={pin.toolchain}\nversion={pin.version}\n".encode())


class RefreshTests(TreeTestCase):
    def test_a_beta_and_nightly_move_leaves_stable_and_the_msrv_alone(self) -> None:
        before = self.snapshot()
        self.publish("beta", manifest("2026-11-30", "1.101.0-beta.4"))
        self.publish("nightly", manifest("2026-12-01", "1.102.0-nightly"))
        code, out, _err = self.refresh()
        self.assertEqual(code, 0)
        self.assertIn("changed=true", out)
        after = self.snapshot()
        changed = {path for path in self.EDITED if after[path] != before[path]}
        self.assertEqual(changed, {rt.PINS_FILE})
        pins = rt.read_pins(self.root)
        self.assertEqual(pins["beta"], rt.Pin("beta", "beta-2026-11-30", "1.101.0-beta.4"))
        self.assertEqual(pins["nightly"], rt.Pin("nightly", "nightly-2026-12-01", "1.102.0-nightly"))
        self.assertEqual(pins["stable"], RECORDED["stable"])

    def test_a_stable_move_bumps_the_pin_and_the_msrv_everywhere_together(self) -> None:
        self.publish("stable", manifest("2026-12-24", "1.101.0"))
        code, _out, err = self.refresh()
        self.assertEqual(code, 0, err)
        self.assertEqual(rt.read_pins(self.root)["stable"], rt.Pin("stable", "1.101.0", "1.101.0"))
        for relative, pattern, count in rt.MSRV_SITES:
            text = (self.root / relative).read_text()
            with self.subTest(relative=relative, pattern=pattern):
                self.assertEqual(text.count(pattern.format(msrv="1.101", version="1.101.0")), count)
                self.assertEqual(text.count(pattern.format(msrv="1.100", version="1.100.0")), 0)
        # No stale spelling of the old stable survives in the edited files.
        for relative in self.EDITED:
            with self.subTest(stale=relative):
                self.assertNotRegex((self.root / relative).read_text(), r"\b1\.100(\.0)?\b")

    def test_a_malformed_manifest_writes_nothing(self) -> None:
        before = self.snapshot()
        self.publish("beta", manifest("2026-11-30", "1.101.0-beta.4"))
        self.publish("nightly", 'manifest-version = "2"\ndate = "2026-12-01"\n')
        code, _out, err = self.refresh()
        self.assertEqual(code, 1)
        self.assertIn("nightly manifest: missing [pkg.rust] version", err)
        self.assertEqual(self.snapshot(), before)

    def test_a_missing_msrv_site_writes_nothing_not_even_the_beta_pin(self) -> None:
        readme = self.root / "README.md"
        readme.write_text(readme.read_text().replace("Install Rust 1.100.0 (pinned in", "Install Rust (pinned in"))
        before = self.snapshot()
        self.publish("beta", manifest("2026-11-30", "1.101.0-beta.4"))
        self.publish("stable", manifest("2026-12-24", "1.101.0"))
        code, _out, err = self.refresh()
        self.assertEqual(code, 1)
        self.assertIn("README.md: expected 1 of 'Install Rust 1.100.0 (pinned in', found 0", err)
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(sorted(p.name for p in self.root.rglob(".*") if p.is_file()), [])

    def test_refresh_reports_changed_to_the_workflow(self) -> None:
        output = self.root / "github_output"
        self.publish("nightly", manifest("2026-12-01", "1.102.0-nightly"))
        os.environ["GITHUB_OUTPUT"] = str(output)
        try:
            code, _out, _err = self.refresh()
        finally:
            del os.environ["GITHUB_OUTPUT"]
        self.assertEqual(code, 0)
        self.assertEqual(output.read_bytes(), b"changed=true\n")


class WorkflowPinTests(unittest.TestCase):
    """Every workflow takes its toolchain from the pins, never from its own text."""

    ACTION = ".github/actions/rust-toolchain/action.yml"

    def files(self) -> list[Path]:
        return sorted((REPO / ".github").rglob("*.yml"))

    def test_only_the_pin_action_installs_rust(self) -> None:
        installers = re.compile(r"dtolnay/rust-toolchain|actions-rs/toolchain|rustup toolchain install")
        offenders = [
            str(path.relative_to(REPO))
            for path in self.files()
            if installers.search(path.read_text()) and str(path.relative_to(REPO)) != self.ACTION
        ]
        self.assertEqual(offenders, [])

    def test_the_resolver_runs_on_a_pinned_python(self) -> None:
        # tomllib needs Python 3.11+; ubuntu-22.04's python3 is 3.10, so the
        # action must not pick an interpreter from PATH.
        steps = yaml.safe_load((REPO / self.ACTION).read_text())["runs"]["steps"]
        setup = [s for s in steps if str(s.get("uses", "")).startswith("actions/setup-python@")]
        self.assertEqual(len(setup), 1)
        self.assertRegex(setup[0]["uses"], r"@[0-9a-f]{40}$")
        self.assertEqual(setup[0]["with"]["python-version"], "3.12")
        resolve = next(s for s in steps if s.get("id") == "pin")
        self.assertEqual(resolve["env"]["PIN_PYTHON"], "${{ steps.%s.outputs.python-path }}" % setup[0]["id"])
        self.assertIn('"$PIN_PYTHON" tools/ci/rust_toolchains.py resolve', resolve["run"])
        self.assertNotRegex((REPO / self.ACTION).read_text(), r"command -v python|\bpython3? tools/ci")

    def test_the_action_avoids_windows_shell_traps(self) -> None:
        # The Windows stable cell failed on `rustc -vV | tee /dev/stderr`;
        # the pin reaches GITHUB_OUTPUT from the script, never through a pipe.
        text = (REPO / self.ACTION).read_text()
        self.assertNotIn("/dev/stderr", text)
        self.assertNotRegex(text, r"\|\s*tee\b")

    def test_the_install_is_one_bounded_attempt(self) -> None:
        # A per-attempt kill-and-retry restarted slow downloads from a partial
        # install every time; the action makes one attempt under one bound.
        text = (REPO / self.ACTION).read_text()
        self.assertEqual(text.count("rustup \"${args[@]}\""), 1)
        self.assertIn("INSTALL_TIMEOUT_SECS", text)
        self.assertNotRegex(text, r"\bfor attempt\b|\buntil\b|\bwhile\b")
        self.assertNotIn("dtolnay/rust-toolchain", text)

    def test_no_workflow_names_a_toolchain_version_or_a_floating_channel_override(self) -> None:
        version = re.compile(r"""toolchain:\s*["']?\d+\.\d+""")
        plus_channel = re.compile(r"\b(?:cargo|rustc|rustup run)\s+\+?(?:stable|beta|nightly)\b")
        for path in self.files():
            text = path.read_text()
            with self.subTest(path=str(path.relative_to(REPO))):
                self.assertIsNone(version.search(text))
                self.assertIsNone(plus_channel.search(text))

    def test_every_pin_action_call_names_a_channel(self) -> None:
        call = re.compile(r"uses: \./\.github/actions/rust-toolchain\n(?P<body>(?:\s+.*\n)+?)(?=\s*-\s|\Z)")
        channel = re.compile(r"^\s+channel:\s*(?P<value>.+)$", re.M)
        literal = {"stable", "beta", "nightly"}
        seen = 0
        for path in self.files():
            for match in call.finditer(path.read_text()):
                seen += 1
                found = channel.search(match.group("body"))
                with self.subTest(path=str(path.relative_to(REPO)), at=match.start()):
                    self.assertIsNotNone(found)
                    value = found.group("value").strip()
                    self.assertTrue(value in literal or value.startswith("${{"), value)
        self.assertGreater(seen, 40)


if __name__ == "__main__":
    unittest.main()
