#!/usr/bin/env python3
"""Pinned Rust toolchains for CI, and their weekly refresh.

The stable pin is `rust-toolchain.toml`'s `channel`; it is also the MSRV in
the manifests. The beta and nightly pins live in `tools/ci/rust-toolchains.toml`.
Workflows never name a version: `.github/actions/rust-toolchain` asks
`resolve` for a channel's pin, and `.github/workflows/rust-toolchain-refresh.yml`
runs `refresh`, which rewrites these files from the published channel
manifests so a toolchain bump never touches `.github/workflows`.

    rust_toolchains.py resolve stable|beta|nightly
    rust_toolchains.py refresh [--manifest-dir DIR] [--summary FILE]

`refresh` validates every manifest and computes every edit before it writes
anything, so a malformed manifest or an unexpected file layout leaves the
tree untouched.
"""

from __future__ import annotations

import argparse
import dataclasses
import os
import re
import sys
import tempfile
import tomllib
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
TOOLCHAIN_FILE = "rust-toolchain.toml"
PINS_FILE = "tools/ci/rust-toolchains.toml"
CHANNELS = ("stable", "beta", "nightly")
MANIFEST_URL = "https://static.rust-lang.org/dist/channel-rust-{channel}.toml"
FETCH_TIMEOUT_SECS = 60

VERSION_PATTERNS = {
    "stable": re.compile(r"\d+\.\d+\.\d+"),
    "beta": re.compile(r"\d+\.\d+\.\d+-beta\.\d+"),
    "nightly": re.compile(r"\d+\.\d+\.\d+-nightly"),
}
DATE_PATTERN = re.compile(r"\d{4}-\d{2}-\d{2}")
CHANNEL_LINE = re.compile(r'^channel = "([^"]+)"$', re.M)

# Files that carry the MSRV, with the exact text that names it. A stable
# bump rewrites each pattern and fails if a file no longer has the expected
# number of matches, so a reworded line is caught instead of left stale.
MSRV_SITES = (
    ("Cargo.toml", 'rust-version = "{msrv}"', 1),
    ("tools/dhat-profile/Cargo.toml", 'rust-version = "{msrv}"', 1),
    ("fuzz/Cargo.toml", 'rust-version = "{msrv}"', 1),
    ("README.md", "Requires Rust **{msrv}** or newer. The repository pins {version} in", 1),
    ("README.md", "Install Rust {version} (pinned in", 1),
    ("docs/contributing/ONBOARDING.md", "| Rust | {version} (pinned in", 1),
)

PINS_TEMPLATE = """\
# Pinned beta and nightly toolchains for the CI beta and nightly cells.
# The stable pin is `channel` in rust-toolchain.toml. Workflows read these
# through .github/actions/rust-toolchain; the weekly
# rust-toolchain-refresh workflow rewrites them with
# `python3 tools/ci/rust_toolchains.py refresh`.

[beta]
toolchain = "{beta_toolchain}"
version = "{beta_version}"

[nightly]
toolchain = "{nightly_toolchain}"
version = "{nightly_version}"
"""


class PinError(Exception):
    """A manifest, pin file or edit site is not in the expected shape."""


@dataclasses.dataclass(frozen=True)
class Pin:
    """One channel's pinned toolchain name and the `rustc -vV` release it reports."""

    channel: str
    toolchain: str
    version: str


def parse_manifest(channel: str, text: str) -> Pin:
    """Returns the pin a channel manifest (`channel-rust-<channel>.toml`) describes."""
    try:
        manifest = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise PinError(f"{channel} manifest is not valid TOML: {error}") from error
    if manifest.get("manifest-version") != "2":
        raise PinError(f"{channel} manifest: unsupported manifest-version {manifest.get('manifest-version')!r}")
    date = manifest.get("date")
    if not isinstance(date, str) or not DATE_PATTERN.fullmatch(date):
        raise PinError(f"{channel} manifest: missing or malformed date {date!r}")
    raw = manifest.get("pkg", {}).get("rust", {}).get("version")
    if not isinstance(raw, str) or not raw.strip():
        raise PinError(f"{channel} manifest: missing [pkg.rust] version")
    version = raw.split()[0]
    if not VERSION_PATTERNS[channel].fullmatch(version):
        raise PinError(f"{channel} manifest: [pkg.rust] version {raw!r} is not a {channel} release")
    toolchain = version if channel == "stable" else f"{channel}-{date}"
    return Pin(channel, toolchain, version)


def read_pins(root: Path) -> dict[str, Pin]:
    """Reads the current stable, beta and nightly pins from the tree."""
    stable = CHANNEL_LINE.findall((root / TOOLCHAIN_FILE).read_text())
    if len(stable) != 1 or not VERSION_PATTERNS["stable"].fullmatch(stable[0]):
        raise PinError(f"{TOOLCHAIN_FILE}: expected one `channel = \"X.Y.Z\"` line, found {stable!r}")
    pins = {"stable": Pin("stable", stable[0], stable[0])}
    try:
        table = tomllib.loads((root / PINS_FILE).read_text())
    except tomllib.TOMLDecodeError as error:
        raise PinError(f"{PINS_FILE} is not valid TOML: {error}") from error
    for channel in ("beta", "nightly"):
        entry = table.get(channel, {})
        toolchain, version = entry.get("toolchain"), entry.get("version")
        if not isinstance(toolchain, str) or not isinstance(version, str):
            raise PinError(f"{PINS_FILE}: [{channel}] needs toolchain and version strings")
        if not VERSION_PATTERNS[channel].fullmatch(version):
            raise PinError(f"{PINS_FILE}: [{channel}] version {version!r} is not a {channel} release")
        if not re.fullmatch(rf"{channel}-\d{{4}}-\d{{2}}-\d{{2}}", toolchain):
            raise PinError(f"{PINS_FILE}: [{channel}] toolchain {toolchain!r} is not {channel}-YYYY-MM-DD")
        pins[channel] = Pin(channel, toolchain, version)
    return pins


def msrv_of(version: str) -> str:
    """The `rust-version` spelling (major.minor) of a stable release."""
    major, minor, _patch = version.split(".")
    return f"{major}.{minor}"


def plan_edits(root: Path, current: dict[str, Pin], wanted: dict[str, Pin]) -> dict[Path, str]:
    """Returns the new contents of every file the move from `current` to `wanted` changes."""
    edits: dict[Path, str] = {}
    if any(current[c] != wanted[c] for c in ("beta", "nightly")):
        edits[root / PINS_FILE] = PINS_TEMPLATE.format(
            beta_toolchain=wanted["beta"].toolchain,
            beta_version=wanted["beta"].version,
            nightly_toolchain=wanted["nightly"].toolchain,
            nightly_version=wanted["nightly"].version,
        )
    old, new = current["stable"].version, wanted["stable"].version
    if old != new:
        path = root / TOOLCHAIN_FILE
        edits[path] = CHANNEL_LINE.sub(f'channel = "{new}"', path.read_text(), count=1)
        for relative, pattern, count in MSRV_SITES:
            path = root / relative
            text = edits.get(path, path.read_text())
            before = pattern.format(msrv=msrv_of(old), version=old)
            after = pattern.format(msrv=msrv_of(new), version=new)
            found = text.count(before)
            if found != count:
                raise PinError(f"{relative}: expected {count} of {before!r}, found {found}")
            edits[path] = text.replace(before, after)
    return edits


def write_all(edits: dict[Path, str]) -> None:
    """Writes every file through a sibling temp file, renaming only once all are staged."""
    staged: list[tuple[Path, Path]] = []
    try:
        for path, text in edits.items():
            fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
            with os.fdopen(fd, "w", newline="") as handle:
                handle.write(text)
            os.chmod(tmp, path.stat().st_mode & 0o7777)
            staged.append((Path(tmp), path))
    except BaseException:
        for tmp, _path in staged:
            tmp.unlink(missing_ok=True)
        raise
    for tmp, path in staged:
        os.replace(tmp, path)


def fetch_manifest(channel: str) -> str:
    url = MANIFEST_URL.format(channel=channel)
    request = urllib.request.Request(url, headers={"User-Agent": "rust-toolchain-refresh"})
    with urllib.request.urlopen(request, timeout=FETCH_TIMEOUT_SECS) as response:
        return response.read().decode("utf-8")


def load_manifests(manifest_dir: Path | None) -> dict[str, Pin]:
    pins = {}
    for channel in CHANNELS:
        if manifest_dir is None:
            text = fetch_manifest(channel)
        else:
            text = (manifest_dir / f"channel-rust-{channel}.toml").read_text()
        pins[channel] = parse_manifest(channel, text)
    return pins


def summarize(current: dict[str, Pin], wanted: dict[str, Pin]) -> str:
    lines = ["Refreshes the pinned Rust toolchains from the published channel manifests.", ""]
    lines += ["| Channel | Was | Now |", "|---|---|---|"]
    for channel in CHANNELS:
        was, now = current[channel], wanted[channel]
        lines.append(f"| {channel} | {was.toolchain} ({was.version}) | {now.toolchain} ({now.version}) |")
    if current["stable"] != wanted["stable"]:
        lines += ["", f"Stable moved, so the MSRV moves with it to {msrv_of(wanted['stable'].version)}."]
    return "\n".join(lines) + "\n"


def write_github_output(**values: str) -> None:
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        # newline="" keeps the lines LF-only on Windows, where a "\r" would
        # become part of the value the workflow reads back.
        with open(target, "a", encoding="utf-8", newline="") as handle:
            for key, value in values.items():
                handle.write(f"{key}={value}\n")


def cmd_resolve(args: argparse.Namespace) -> int:
    pin = read_pins(args.root)[args.channel]
    write_github_output(toolchain=pin.toolchain, version=pin.version)
    print(f"toolchain={pin.toolchain}")
    print(f"version={pin.version}")
    return 0


def cmd_refresh(args: argparse.Namespace) -> int:
    current = read_pins(args.root)
    wanted = load_manifests(args.manifest_dir)
    edits = plan_edits(args.root, current, wanted)
    write_all(edits)
    if args.summary is not None:
        args.summary.write_text(summarize(current, wanted))
    changed = "true" if edits else "false"
    write_github_output(changed=changed)
    for channel in CHANNELS:
        print(f"{channel}: {current[channel].toolchain} -> {wanted[channel].toolchain}")
    print(f"changed={changed}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=REPO_ROOT, help=argparse.SUPPRESS)
    sub = parser.add_subparsers(dest="command", required=True)
    resolve = sub.add_parser("resolve", help="print a channel's pinned toolchain and version")
    resolve.add_argument("channel", choices=CHANNELS)
    resolve.set_defaults(run=cmd_resolve)
    refresh = sub.add_parser("refresh", help="move the pins to the published channel releases")
    refresh.add_argument("--manifest-dir", type=Path, help="read channel-rust-<channel>.toml from DIR instead of fetching")
    refresh.add_argument("--summary", type=Path, help="write a pull-request body describing the change")
    refresh.set_defaults(run=cmd_refresh)
    args = parser.parse_args(argv)
    try:
        return args.run(args)
    except (PinError, OSError, UnicodeDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
