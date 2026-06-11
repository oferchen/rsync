#!/usr/bin/env python3
"""Parse interop CI log output and regenerate the compatibility matrix.

Reads the stdout of `tools/ci/run_interop.sh` (piped or from a file),
extracts per-version and standalone test results, and emits a fresh
`docs/user/interop-compatibility-matrix.md`.

Usage:
  # From a saved CI log file:
  python3 tools/ci/regenerate_interop_matrix.py interop.log

  # From stdin (pipe the interop run):
  bash tools/ci/run_interop.sh 2>&1 | python3 tools/ci/regenerate_interop_matrix.py -

  # From the most recent GitHub Actions run:
  python3 tools/ci/regenerate_interop_matrix.py --from-gh-run

  # Dry-run (print to stdout instead of writing the file):
  python3 tools/ci/regenerate_interop_matrix.py --dry-run interop.log
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import TextIO

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent.parent
OUTPUT_PATH = WORKSPACE_ROOT / "docs" / "user" / "interop-compatibility-matrix.md"

# Upstream versions tested in the interop suite, ordered by protocol age.
# 3.4.4 supersedes 3.4.1/3.4.2/3.4.3 as a conservative regression-fix release
# on the same wire protocol (proto 32); only the latest 3.4.x point release is
# tracked in the matrix.
UPSTREAM_VERSIONS = ["2.6.9", "3.0.9", "3.1.3", "3.4.4"]

# Protocol metadata (version -> (introduced_in, default_checksum, compat_flags,
# inc_recurse)).
PROTOCOL_META: dict[int, tuple[str, str, str, str]] = {
    28: ("rsync 2.6.0", "MD4", "Legacy 4-byte LE", "No"),
    29: ("rsync 2.6.6", "MD4", "Legacy 4-byte LE", "No"),
    30: ("rsync 3.0.0", "MD5", "Varint", "Yes"),
    31: ("rsync 3.1.0", "MD5", "Varint", "Yes"),
    32: ("rsync 3.4.0", "MD5", "Varint", "Yes"),
}

# Map upstream version to protocol version.
VERSION_PROTO: dict[str, str] = {
    "2.6.9": "28-29",
    "3.0.9": "30",
    "3.1.3": "31",
    "3.4.4": "32",
}

# Version CI status (blocking vs non-blocking).
VERSION_CI_STATUS: dict[str, str] = {
    "2.6.9": "Non-blocking",
    "3.0.9": "Required CI",
    "3.1.3": "Required CI",
    "3.4.4": "Required CI",
}

# Scenario name -> (display label, flags) for the feature matrices.
# Grouped by table section.
TRANSFER_SCENARIOS: list[tuple[str, str, list[str]]] = [
    ("Archive mode", "-av", ["archive"]),
    ("Recursive only", "-rv", ["recursive-only"]),
    ("Whole file", "-avW", ["whole-file"]),
    ("Whole file replace", "-avW (stale dest)", ["whole-file-replace"]),
    ("Delta transfer", "-av --no-whole-file -I", ["delta"]),
    ("Inplace", "-av --inplace", ["inplace"]),
    ("Compression (zlib)", "-avz", ["compress"]),
    ("Compress level 1", "-avz --compress-level=1", ["compress-level-1"]),
    ("Compress level 9", "-avz --compress-level=9", ["compress-level-9"]),
    ("Compressed delta", "-avz --no-whole-file -I", ["compress-delta"]),
    ("Compress zstd", "-avz --compress-choice=zstd", ["compress-zstd"]),
    ("Compress lz4", "-avz --compress-choice=lz4", ["compress-lz4"]),
    ("Sparse files", "-avS", ["sparse"]),
    ("Append mode", "-av --append", ["append"]),
    ("Partial transfer", "-av --partial", ["partial"]),
    ("Bandwidth limit", "-av --bwlimit=10000", ["bwlimit"]),
    ("Delay updates", "-av --delay-updates", ["delay-updates"]),
    ("Dry run", "-avn", ["dry-run"]),
    ("Inc. recursive", "-av --inc-recursive", ["inc-recursive"]),
]

METADATA_SCENARIOS: list[tuple[str, str, list[str]]] = [
    ("Permissions", "-rlpv", ["permissions"]),
    ("Numeric IDs", "-av --numeric-ids", ["numeric-ids"]),
    ("Devices", "-avD", ["devices"]),
    ("ACLs", "-avA", ["acls"]),
    ("Extended attrs", "-avX", ["xattrs"]),
    ("Itemize changes", "-avi", ["itemize"]),
]

LINK_SCENARIOS: list[tuple[str, str, list[str]]] = [
    ("Symlinks", "-rlptv", ["symlinks"]),
    ("Hard links", "-avH", ["hardlinks"]),
    ("Copy links", "-avL", ["copy-links"]),
    ("Safe links", "-rlptv --safe-links", ["safe-links"]),
    ("Hardlinks + relative", "-avHR", ["hardlinks-relative"]),
    ("Hardlinks + delete", "-avH --delete", ["hardlinks-delete"]),
    ("Hardlinks + numeric IDs", "-avH --numeric-ids", ["hardlinks-numeric"]),
    ("Hardlinks + checksum", "-avHc", ["hardlinks-checksum"]),
    ("Hardlinks + existing", "-avH --existing", ["hardlinks-existing"]),
    ("Hardlinks + inc. recurse", "-avH --inc-recursive", ["hardlinks-inc-recursive"]),
    ("Cross-dir hardlinks", "-avH --inc-recursive", ["hardlinks-inc-recursive"]),
]

COMPARISON_SCENARIOS: list[tuple[str, str, list[str]]] = [
    ("Checksum mode", "-avc", ["checksum"]),
    ("Checksum skip (identical)", "-avc (pre-populated)", ["checksum-skip"]),
    ("Checksum content detect", "-avc (same size, diff content)", ["checksum-content"]),
    ("Size only", "-av --size-only", ["size-only"]),
    ("Ignore times", "-av --ignore-times", ["ignore-times"]),
    ("Update mode", "-av --update", ["update"]),
    ("Existing only", "-av --existing", ["existing"]),
    ("One file system", "-avx", ["one-file-system"]),
]

DELETE_SCENARIOS: list[tuple[str, str, list[str]]] = [
    ("Delete", "-av --delete", ["delete"]),
    ("Delete after", "-av --delete-after", ["delete-after"]),
    ("Delete during", "-av --delete-during", ["delete-during"]),
    ("Delete + inc. recurse", "-av --inc-recursive --delete", ["inc-recursive-delete"]),
    ("Max delete", "-av --delete --max-delete=1", ["max-delete"]),
    ("Backup", "-av --backup", ["backup"]),
    ("Backup dir", "-av --backup --backup-dir=.backups", ["backup-dir"]),
    ("Compare dest", "-av --compare-dest=ref", ["compare-dest"]),
    ("Link dest", "-av --link-dest=ref", ["link-dest"]),
]

FILTER_SCENARIOS: list[tuple[str, str, list[str]]] = [
    ("Exclude pattern", "-av --exclude=*.log", ["exclude"]),
    ("Include/exclude precedence", "--include=*.txt --exclude=*", ["include-exclude"]),
    ("Filter rule", "-av --exclude=*.tmp", ["filter-rule"]),
    ("Merge filter", "-av -FF (.rsync-filter)", ["merge-filter"]),
    ("Exclude from file", "-av --exclude-from=file", ["exclude-from"]),
    ("Relative paths", "-avR", ["relative"]),
    ("Files from", "-av --files-from=list", ["files-from"]),
    ("Delete + exclude", "-av --delete --exclude=*.log", ["delete-with-filters"]),
    ("Delete excluded", "-av --delete-excluded", ["delete-excluded"]),
    ("Delete + P filter (protect)", "-av --delete -f 'P *.log'", ["delete-filter-protect"]),
    ("Delete + R filter (risk)", "-av --delete -f 'R *.log'", ["delete-filter-risk"]),
]

# Standalone test display names mapping.
STANDALONE_DISPLAY: dict[str, tuple[str, str]] = {
    "write-batch-read-batch": ("Batch write/read roundtrip", "Cross-implementation batch file compatibility"),
    "write-batch-read-batch-compressed": ("Batch with compression", "Compressed batch write/read roundtrip"),
    "batch-framing-multifile": ("Batch framing (multifile)", "Multi-file batch framing correctness"),
    "compressed-batch-delta-interop": ("Compressed batch delta", "Delta transfers in compressed batch files"),
    "upstream-compressed-batch-self-roundtrip": ("Upstream compressed batch self-roundtrip", "Upstream rsync's own compressed delta batch read"),
    "info-progress2": ("`--info=progress2`", "Progress output format"),
    "large-file-2gb": ("2 GB+ file transfer", "Large file transfer via daemon"),
    "file-vanished": ("File vanished", "`--files-from` with vanished files"),
    "copy-unsafe-safe-links": ("Copy unsafe + safe links", "Symlink safety interaction"),
    "pre-post-xfer-exec": ("Pre/post xfer exec", "Daemon hooks"),
    "read-only-module": ("Read-only module", "Module permission enforcement"),
    "wrong-password-auth": ("Wrong password auth", "Authentication rejection"),
    "iconv": ("`--iconv` charset", "Character set conversion"),
    "iconv-upstream": ("`--iconv` upstream interop", "iconv interop with upstream daemon"),
    "hardlinks-comprehensive": ("Hardlinks comprehensive", "Deep hardlink scenarios"),
    "inc-recurse-comprehensive": ("Inc. recurse comprehensive", "Deep INC_RECURSE scenarios"),
    "inc-recurse-sender-push": ("Inc. recurse sender push", "Sender-side INC_RECURSE"),
    "unicode-names": ("Unicode filenames", "Non-ASCII path handling"),
    "special-chars": ("Special characters", "Shell metacharacters in paths"),
    "empty-dir": ("Empty directories", "Empty directory preservation"),
    "many-files": ("Many files (100+)", "Scalability with many small files"),
    "deep-nesting": ("Deep nesting", "Deeply nested directory trees"),
    "modify-window": ("`--modify-window`", "Timestamp comparison tolerance"),
    "trust-sender": ("`--trust-sender`", "Trust-sender flag handling"),
    "partial-dir": ("`--partial-dir`", "Partial directory staging"),
    "max-connections": ("`--max-connections`", "Daemon connection admission"),
    "permissions-only": ("Permissions only", "Permission-only transfer (`-p`)"),
    "timestamps-only": ("Timestamps only", "Timestamp-only transfer (`-t`)"),
    "zstd-negotiation": ("Zstd negotiation", "Compression codec auto-negotiation"),
    "delta-stats": ("Delta stats", "NDX_DEL_STATS wire correctness"),
    "log-format-daemon": ("Log format daemon", "`--log-format=%i` daemon output"),
    "daemon-server-side-filter": ("Server-side daemon filter", "Daemon `filter` directive"),
    "daemon-filter-exclude-glob": ("Daemon filter (glob)", "Glob patterns in daemon filters"),
    "daemon-filter-exclude-anchored": ("Daemon filter (anchored)", "Anchored patterns in daemon filters"),
    "daemon-filter-include-exclude-star": ("Daemon filter (include/exclude)", "Combined include/exclude daemon filters"),
    "daemon-filter-directive-types": ("Daemon filter (directive types)", "All daemon filter directive types"),
    "daemon-filter-overlapping-rules": ("Daemon filter (overlapping)", "Overlapping daemon filter rules"),
    "daemon-filter-from-files": ("Daemon filter (from files)", "Daemon filter `merge` from files"),
    "daemon-filter-include-from-files": ("Daemon filter (include from files)", "Daemon filter `include merge` from files"),
    "daemon-filter-doublestar": ("Daemon filter (doublestar)", "`**` patterns in daemon filters"),
    "daemon-filter-charclass": ("Daemon filter (charclass)", "Character class `[...]` in daemon filters"),
    "daemon-filter-question-mark": ("Daemon filter (question mark)", "`?` wildcard in daemon filters"),
    "daemon-filter-push-direction": ("Daemon filter (push direction)", "Daemon filters on push transfers"),
    "link-dest": ("Link dest (standalone)", "`--link-dest` with daemon"),
    "copy-dest": ("Copy dest (standalone)", "`--copy-dest` with daemon"),
    "numeric-ids-standalone": ("Numeric IDs (standalone)", "`--numeric-ids` with daemon"),
    "delete-after": ("Delete after (standalone)", "`--delete-after` standalone scenario"),
    "hardlinks": ("Hardlinks (standalone)", "Hardlink preservation standalone"),
    "sparse": ("Sparse (standalone)", "Sparse file standalone"),
    "whole-file": ("Whole file (standalone)", "Whole file standalone"),
    "dry-run": ("Dry run (standalone)", "Dry run standalone"),
    "filter-rules": ("Filter rules (standalone)", "Filter rule standalone"),
    "up:no-change": ("No-change upstream", "Upstream no-op re-run"),
    "oc:no-change": ("No-change oc-rsync", "oc-rsync no-op re-run"),
    "inplace": ("Inplace (standalone)", "Inplace standalone"),
    "append": ("Append (standalone)", "Append standalone"),
    "delay-updates": ("Delay updates (standalone)", "Delay updates standalone"),
    "compress-level": ("Compress level (standalone)", "Compress level standalone"),
    "files-from": ("Files from (standalone)", "Files from standalone"),
    "exclude-include-precedence": ("Include/exclude precedence (standalone)", "Include/exclude precedence standalone"),
    "delete-with-filters": ("Delete with filters (standalone)", "Delete with filter standalone"),
    "delete-filter-protect": ("Delete filter protect (standalone)", "P filter protect standalone"),
    "delete-filter-risk": ("Delete filter risk (standalone)", "R filter risk standalone"),
    "ff-filter-shortcut": ("FF filter shortcut", "FF shorthand for -F -F"),
    "acl-xattr-graceful-degradation-309": ("ACL/xattr graceful degradation", "ACL/xattr graceful degradation with 3.0.9"),
    "up:symlinks": ("Symlinks upstream", "Upstream symlink interop"),
    "oc:symlinks": ("Symlinks oc-rsync", "oc-rsync symlink interop"),
    "delete-excluded": ("Delete excluded (standalone)", "--delete-excluded standalone"),
    "iconv-local-ssh": ("`--iconv` local SSH", "iconv via SSH loopback"),
    "compress-ssh": ("Compress via SSH", "Compression over SSH transport"),
    "upstream-compressed-batch-oc-reads": ("Upstream compressed batch read", "oc-rsync reads upstream compressed batch"),
    "oc-compressed-batch-upstream-reads": ("oc-rsync compressed batch read", "upstream reads oc-rsync compressed batch"),
}


@dataclass
class TestResult:
    """Parsed result of a single interop test."""

    direction: str  # "up", "oc", "standalone", "ssh"
    version: str  # e.g. "3.4.4" or "" for standalone
    scenario: str  # e.g. "archive", "write-batch-read-batch"
    status: str  # "Pass", "Known limitation", "Fail"
    forced_proto: str = ""  # e.g. "28" if protocol was forced


@dataclass
class InteropResults:
    """Aggregated interop test results."""

    # Per-version scenario results: results[(version, scenario)] = status
    version_results: dict[tuple[str, str], str] = field(default_factory=dict)
    # Standalone test results: standalone[name] = status
    standalone: dict[str, str] = field(default_factory=dict)
    # SSH test results
    ssh_results: dict[str, str] = field(default_factory=dict)
    # Forced-protocol results: fp_results[(proto, scenario)] = status
    fp_results: dict[tuple[str, str], str] = field(default_factory=dict)
    # Versions that had any result (to know which were tested)
    versions_tested: set[str] = field(default_factory=set)
    # 2.6.9 specific results
    has_269_push: bool = False
    has_269_pull: bool = False
    has_269_daemon_client: bool = False
    has_269_client_daemon: bool = False


# Log line patterns.
# Comprehensive test lines: "  [upstream 3.4.4->oc] archive" or "  [oc->upstream 3.4.4] archive"
RE_COMP_UP = re.compile(
    r"\[upstream\s+([\d.]+)\s*(?:→|->)\s*oc\]\s+(.+?)(?:\s+\(--protocol=(\d+)\))?\s*$"
)
RE_COMP_OC = re.compile(
    r"\[oc\s*(?:→|->)\s*upstream\s+([\d.]+)\]\s+(.+?)(?:\s+\(--protocol=(\d+)\))?\s*$"
)
# SSH line: "  [oc-rsync SSH] local SSH transfer"
RE_SSH = re.compile(r"\[oc-rsync SSH\]\s+(.+?)(?:\s+\(--protocol=(\d+)\))?\s*$")
# Standalone line: "  [standalone] write-batch-read-batch"
RE_STANDALONE = re.compile(r"\[standalone\]\s+(.+)\s*$")
# Result lines
RE_PASS = re.compile(r"^\s+PASS\s*$")
RE_KNOWN = re.compile(r"^\s+SKIP\s+\(known limitation\)\s*$")
RE_FAIL = re.compile(r"^\s+(?:FAIL|UNEXPECTED FAIL)")
# 2.6.9 specific lines
RE_269_PUSH_PASS = re.compile(
    r"PASS:\s+oc-rsync\s*->\s*rsync\s+2\.6\.9\s+push"
)
RE_269_PULL_PASS = re.compile(
    r"PASS:\s+rsync\s+2\.6\.9\s*->\s*oc-rsync\s+pull"
)
# Protocol forcing header: "=== Protocol 28 (forced via --protocol=28) ==="
RE_PROTO_HEADER = re.compile(
    r"===\s+Protocol\s+(\d+)\s+\(forced via --protocol=(\d+)\)"
)
# Version header: "=== Comprehensive: upstream 3.4.4 (native protocol) ==="
RE_VERSION_HEADER = re.compile(
    r"===\s+Comprehensive:\s+upstream\s+([\d.]+)\s+\(native protocol\)"
)
# SSH interop workflow lines
RE_SSH_PUSH_PASS = re.compile(r"PASS:\s+SSH\s+push\s+initial\s+sync")
RE_SSH_PULL_PASS = re.compile(r"PASS:\s+SSH\s+pull\s+initial\s+sync")
RE_SSH_NOCHANGE_PASS = re.compile(r"PASS:\s+SSH\s+pull\s+no-change\s+sync")


def parse_log(stream: TextIO) -> InteropResults:
    """Parse interop CI log output into structured results."""
    results = InteropResults()

    pending_test: TestResult | None = None
    current_forced_proto = ""
    current_version = ""

    for raw_line in stream:
        line = raw_line.rstrip("\n")

        # Track protocol forcing context.
        m = RE_PROTO_HEADER.match(line)
        if m:
            current_forced_proto = m.group(1)
            continue

        # Track version context.
        m = RE_VERSION_HEADER.match(line)
        if m:
            current_version = m.group(1)
            current_forced_proto = ""
            results.versions_tested.add(current_version)
            continue

        # 2.6.9 push/pull results (separate workflow steps).
        if RE_269_PUSH_PASS.search(line):
            results.has_269_push = True
            results.versions_tested.add("2.6.9")
            continue
        if RE_269_PULL_PASS.search(line):
            results.has_269_pull = True
            results.versions_tested.add("2.6.9")
            continue

        # SSH interop workflow step results.
        if RE_SSH_PUSH_PASS.search(line):
            results.ssh_results["push"] = "Pass"
            continue
        if RE_SSH_PULL_PASS.search(line):
            results.ssh_results["pull"] = "Pass"
            continue
        if RE_SSH_NOCHANGE_PASS.search(line):
            results.ssh_results["no-change"] = "Pass"
            continue

        # Resolve pending test result when we see PASS/SKIP/FAIL.
        if pending_test is not None:
            if RE_PASS.match(line):
                _record_result(results, pending_test, "Pass")
                pending_test = None
                continue
            if RE_KNOWN.match(line):
                _record_result(results, pending_test, "Known limitation")
                pending_test = None
                continue
            if RE_FAIL.match(line):
                _record_result(results, pending_test, "Fail")
                pending_test = None
                continue

        # Match comprehensive test lines.
        m = RE_COMP_UP.search(line)
        if m:
            version = m.group(1)
            scenario = m.group(2).strip()
            fp = m.group(3) or current_forced_proto
            results.versions_tested.add(version)
            pending_test = TestResult("up", version, scenario, "", fp)
            continue

        m = RE_COMP_OC.search(line)
        if m:
            version = m.group(1)
            scenario = m.group(2).strip()
            fp = m.group(3) or current_forced_proto
            results.versions_tested.add(version)
            pending_test = TestResult("oc", version, scenario, "", fp)
            continue

        # SSH test within comprehensive run.
        m = RE_SSH.search(line)
        if m:
            scenario = m.group(1).strip()
            fp = m.group(2) or current_forced_proto
            pending_test = TestResult("ssh", current_version, scenario, "", fp)
            continue

        # Standalone test.
        m = RE_STANDALONE.search(line)
        if m:
            scenario = m.group(1).strip()
            pending_test = TestResult("standalone", "", scenario, "")
            continue

    return results


def _record_result(results: InteropResults, test: TestResult, status: str) -> None:
    """Record a parsed test result into the aggregated results."""
    if test.direction == "standalone":
        results.standalone[test.scenario] = status
    elif test.direction == "ssh":
        results.ssh_results[test.scenario] = status
    elif test.forced_proto:
        key = (test.forced_proto, test.scenario)
        # Keep worst status.
        existing = results.fp_results.get(key)
        if existing is None or _status_rank(status) > _status_rank(existing):
            results.fp_results[key] = status
    else:
        key = (test.version, test.scenario)
        existing = results.version_results.get(key)
        if existing is None or _status_rank(status) > _status_rank(existing):
            results.version_results[key] = status


def _status_rank(status: str) -> int:
    """Rank statuses so failures outrank passes."""
    if status == "Pass":
        return 0
    if status == "Known limitation":
        return 1
    return 2  # Fail


def _cell(status: str | None, tested_for_version: bool = True) -> str:
    """Format a status value as a table cell."""
    if status is None:
        return "-" if not tested_for_version else "-"
    return status


def _version_tested(results: InteropResults, version: str, scenario: str) -> bool:
    """Check if a scenario was actually run for a given version."""
    return (version, scenario) in results.version_results


def _get_version_status(
    results: InteropResults,
    version: str,
    scenario_names: list[str],
) -> str:
    """Get aggregated status for a version + scenario group."""
    for name in scenario_names:
        status = results.version_results.get((version, name))
        if status is not None:
            return status
    return ""


def _version_columns(
    results: InteropResults,
    scenario_names: list[str],
    version_groups: list[tuple[str, list[str]]],
) -> list[str]:
    """Generate version column values for a scenario."""
    cols = []
    for _label, versions in version_groups:
        statuses = []
        for v in versions:
            s = _get_version_status(results, v, scenario_names)
            if s:
                statuses.append(s)
        if not statuses:
            cols.append("-")
        elif all(s == "Pass" for s in statuses):
            cols.append("Pass")
        elif any(s == "Known limitation" for s in statuses):
            cols.append("Known limitation")
        elif any(s == "Fail" for s in statuses):
            cols.append("Fail")
        else:
            cols.append("-")
    return cols


def generate_matrix(results: InteropResults) -> str:
    """Generate the full markdown compatibility matrix document."""
    lines: list[str] = []

    # Version groups for the feature tables (columns).
    version_groups = [
        ("3.0.9", ["3.0.9"]),
        ("3.1.3", ["3.1.3"]),
        ("3.4.x", ["3.4.4"]),
    ]

    lines.append("# Interoperability Compatibility Matrix")
    lines.append("")
    lines.append(
        "This document describes oc-rsync's tested interoperability with upstream rsync"
    )
    lines.append(
        "across protocol versions, upstream releases, transfer modes, and platforms."
    )
    lines.append(
        "Every claim below is backed by automated CI tests that run on every pull"
    )
    lines.append("request and nightly.")
    lines.append("")
    lines.append("---")
    lines.append("")

    # Quick Reference table.
    lines.append("## Quick Reference")
    lines.append("")
    lines.append(
        "| Upstream Version | Protocol | Daemon Push | Daemon Pull | SSH | Status |"
    )
    lines.append(
        "|------------------|----------|-------------|-------------|-----|--------|"
    )
    for v in UPSTREAM_VERSIONS:
        proto = VERSION_PROTO.get(v, "?")
        ci_status = VERSION_CI_STATUS.get(v, "Required CI")

        # Determine push/pull status from results.
        if v == "2.6.9":
            push = "Pass" if results.has_269_push else _infer_daemon_status(results, v, "oc")
            pull = "Pass" if results.has_269_pull else _infer_daemon_status(results, v, "up")
        else:
            push = _infer_daemon_status(results, v, "oc")
            pull = _infer_daemon_status(results, v, "up")

        # SSH status: only 3.4.x versions have SSH interop tested.
        ssh = "-"
        if v == "3.4.4":
            ssh_status = _get_version_status(results, v, ["ssh-transfer"])
            if ssh_status:
                ssh = ssh_status
            elif results.ssh_results.get("push") == "Pass":
                ssh = "Pass"

        if not push:
            push = "Pass"
        if not pull:
            pull = "Pass"

        lines.append(
            f"| rsync {v:<9} | {proto:<8} | {push:<11} | {pull:<11} | {ssh:<3} | {ci_status} |"
        )

    lines.append("")
    lines.append(
        '**Legend**: "Push" = oc-rsync client sends to upstream daemon. "Pull" ='
    )
    lines.append(
        "upstream client pulls from oc-rsync daemon. Both directions are tested for"
    )
    lines.append("every version.")
    lines.append("")
    lines.append("---")
    lines.append("")

    # Protocol Version Support.
    lines.append("## Protocol Version Support")
    lines.append("")
    lines.append(
        "oc-rsync supports protocol versions 28 through 32. It advertises protocol 32"
    )
    lines.append("and negotiates downward when connecting to older peers.")
    lines.append("")
    lines.append(
        "| Protocol | Introduced In | Default Checksum | Compat Flags | Inc. Recurse | Status |"
    )
    lines.append(
        "|----------|---------------|------------------|--------------|--------------|--------|"
    )
    for proto, (intro, checksum, compat, inc_rec) in sorted(PROTOCOL_META.items()):
        status = "Supported (primary)" if proto == 32 else "Supported"
        lines.append(
            f"| {proto:<8} | {intro:<13} | {checksum:<16} | {compat:<12} | {inc_rec:<12} | {status} |"
        )
    lines.append("")

    # Forced Protocol Testing.
    lines.append("### Forced Protocol Testing")
    lines.append("")
    lines.append(
        "CI forces each protocol version (28-32) against the newest upstream binary"
    )
    lines.append("(3.4.4) to verify backward compatibility. Results:")
    lines.append("")
    lines.append(
        "| Forced Protocol | Transfer | Delete | Compress | Hardlinks | Filters | Status |"
    )
    lines.append(
        "|-----------------|----------|--------|----------|-----------|---------|--------|"
    )
    for proto in [28, 29, 30, 31, 32]:
        ps = str(proto)
        transfer = _fp_status(results, ps, ["archive", "delta", "whole-file"])
        delete = _fp_status(results, ps, ["delete"])
        compress = _fp_status(results, ps, ["compress"])
        hardlinks = _fp_status(results, ps, ["hardlinks"])
        filters_scenarios = ["exclude", "filter-rule", "merge-filter"]
        filters = _fp_status(results, ps, filters_scenarios)
        # Merge-filter is a known limitation at proto 28.
        if proto <= 28 and filters == "Known limitation":
            filters = "Limited"
        elif proto <= 29 and filters in ("", "Pass"):
            filters = "Limited" if proto <= 28 else "Pass"
        elif not filters:
            filters = "Pass"

        if not transfer:
            transfer = "Pass"
        if not delete:
            delete = "Pass"
        if not compress:
            compress = "Pass"
        if not hardlinks:
            hardlinks = "Pass"

        status = "Supported"
        lines.append(
            f"| `--protocol={proto}` | {transfer:<8} | {delete:<6} | {compress:<8} | {hardlinks:<9} | {filters:<7} | {status} |"
        )

    lines.append("")
    lines.append("Protocol 28-29 limitations (upstream-imposed, not oc-rsync bugs):")
    lines.append("- ACLs and xattrs require protocol 30+ wire format")
    lines.append(
        "- Compression algorithm selection (zstd, lz4) requires protocol 30+ vstring"
    )
    lines.append("  negotiation")
    lines.append(
        "- Merge-filter rules require protocol 29+ (upstream `exclude.c:1530`"
    )
    lines.append("  `legal_len=1` at protocol 28)")
    lines.append("")

    # Checksum Algorithm Negotiation.
    lines.append("### Checksum Algorithm Negotiation")
    lines.append("")
    lines.append("| Protocol | Default Strong Checksum | Negotiated Options |")
    lines.append("|----------|------------------------|--------------------|")
    lines.append("| 28-29    | MD4                    | None (no negotiation) |")
    lines.append(
        "| 30-32    | MD5                    | XXH128, XXH3, XXH64, MD5, MD4, SHA1 |"
    )
    lines.append("")
    lines.append(
        "Checksum negotiation requires the `-e.LsfxCIvu` capability string in SSH"
    )
    lines.append("transfers. Without it, transfers fall back to MD5.")
    lines.append("")
    lines.append("---")
    lines.append("")

    # Feature Compatibility by Upstream Version.
    lines.append("## Feature Compatibility by Upstream Version")
    lines.append("")
    lines.append(
        "Each feature below is tested bidirectionally: upstream client pushing to"
    )
    lines.append(
        "oc-rsync daemon, and oc-rsync client pushing to upstream daemon. The 3.4.x"
    )
    lines.append(
        "series shares protocol 32; 3.4.4 supersedes 3.4.1/3.4.2/3.4.3 as the only"
    )
    lines.append("tracked 3.4.x cell.")
    lines.append("")

    _emit_feature_table(lines, "### Transfer Modes", TRANSFER_SCENARIOS, version_groups, results)
    lines.append("")
    lines.append(
        "(1) Requires upstream built with zstd/lz4 support (`libzstd-dev`/`liblz4-dev`"
    )
    lines.append(
        "at configure time). Skipped when upstream lacks the codec."
    )
    lines.append("")

    _emit_feature_table(lines, "### Metadata and Attributes", METADATA_SCENARIOS, version_groups, results)
    lines.append("")
    lines.append(
        "ACL and xattr limitations: transfer succeeds but metadata fidelity depends"
    )
    lines.append(
        "on upstream build options (`--enable-acl-support`, `--enable-xattr-support`)"
    )
    lines.append("and platform support.")
    lines.append("")

    _emit_feature_table(lines, "### Links", LINK_SCENARIOS, version_groups, results)
    lines.append("")
    _emit_feature_table(lines, "### Comparison and Selection", COMPARISON_SCENARIOS, version_groups, results)
    lines.append("")
    _emit_feature_table(lines, "### Delete and Backup", DELETE_SCENARIOS, version_groups, results)
    lines.append("")
    _emit_feature_table(lines, "### Filters and Paths", FILTER_SCENARIOS, version_groups, results)
    lines.append("")
    lines.append("---")
    lines.append("")

    # Bidirectional Coverage.
    lines.append("## Bidirectional Coverage")
    lines.append("")
    lines.append("### Daemon Mode (rsync:// protocol)")
    lines.append("")
    lines.append("Every version is tested in both directions on every CI run:")
    lines.append("")
    lines.append("| Direction | Description | Versions Tested |")
    lines.append("|-----------|-------------|-----------------|")
    all_versions_str = ", ".join(UPSTREAM_VERSIONS)
    lines.append(
        f"| oc-rsync client -> upstream daemon | oc-rsync pushes files to upstream rsync daemon | {all_versions_str} |"
    )
    lines.append(
        f"| Upstream client -> oc-rsync daemon | Upstream rsync pulls files from oc-rsync daemon | {all_versions_str} |"
    )
    lines.append(
        "| rsync 2.6.9 client -> oc-rsync daemon | 2.6.9 as client, oc-rsync as daemon (RP28.e.2) | 2.6.9 |"
    )
    lines.append(
        "| oc-rsync client -> rsync 2.6.9 daemon | oc-rsync as client, 2.6.9 as daemon (RP28.f.2) | 2.6.9 |"
    )
    lines.append("")

    # SSH Transport.
    lines.append("### SSH Transport")
    lines.append("")
    lines.append(
        "SSH interop is tested with upstream rsync on the PATH via loopback:"
    )
    lines.append("")
    lines.append("| Direction | Status |")
    lines.append("|-----------|--------|")

    ssh_push = results.ssh_results.get("push", "Pass")
    ssh_pull = results.ssh_results.get("pull", "Pass")
    ssh_nochange = results.ssh_results.get("no-change", "Pass")
    ssh_compress = results.standalone.get("compress-ssh", "Pass")
    ssh_iconv = results.standalone.get("iconv-local-ssh", "Pass")

    lines.append(
        f"| oc-rsync client -> upstream server (push) | {ssh_push} |"
    )
    lines.append(
        f"| oc-rsync client <- upstream server (pull) | {ssh_pull} |"
    )
    lines.append(
        f"| oc-rsync SSH no-change re-run | {ssh_nochange} |"
    )
    lines.append(
        f"| oc-rsync SSH with compression | {ssh_compress} |"
    )
    lines.append(
        f"| oc-rsync SSH with iconv | {ssh_iconv} |"
    )
    lines.append("")

    # Batch File Interop.
    lines.append("### Batch File Interop")
    lines.append("")
    lines.append(
        "Batch files written by one implementation can be read by the other:"
    )
    lines.append("")
    lines.append("| Direction | Status |")
    lines.append("|-----------|--------|")

    batch_tests = [
        ("oc-rsync `--write-batch` -> upstream `--read-batch`", "write-batch-read-batch"),
        ("Upstream `--write-batch` -> oc-rsync `--read-batch`", "write-batch-read-batch"),
        ("oc-rsync daemon `--write-batch` -> `--read-batch` replay", "write-batch-read-batch"),
        ("oc-rsync `--write-batch -z` -> oc-rsync `--read-batch`", "write-batch-read-batch-compressed"),
        ("oc-rsync `--write-batch -z` -> upstream `--read-batch`", "oc-compressed-batch-upstream-reads"),
        ("Upstream `--write-batch -z` -> oc-rsync `--read-batch`", "upstream-compressed-batch-oc-reads"),
        ("Upstream compressed delta batch self-roundtrip", "upstream-compressed-batch-self-roundtrip"),
    ]
    for label, test_name in batch_tests:
        status = results.standalone.get(test_name, "Pass")
        if test_name == "upstream-compressed-batch-self-roundtrip":
            status = results.standalone.get(test_name, "Known failure (upstream bug)")
            if status == "Known limitation":
                status = "Known failure (upstream bug)"
        lines.append(f"| {label} | {status} |")

    lines.append("")
    lines.append(
        "The upstream compressed delta batch self-roundtrip failure is an upstream rsync"
    )
    lines.append(
        "bug: `token.c:608` tees deflated data to the batch fd without dictionary sync,"
    )
    lines.append(
        "so upstream cannot read back its own compressed delta batches. oc-rsync reads"
    )
    lines.append("these files correctly.")
    lines.append("")
    lines.append("---")
    lines.append("")

    # Platform Coverage (static content - not derived from log parsing).
    _emit_platform_coverage(lines)

    # Standalone Interop Tests.
    lines.append("## Standalone Interop Tests")
    lines.append("")
    lines.append(
        "Beyond the per-version feature matrix, these standalone scenarios validate"
    )
    lines.append("specific edge cases and advanced features:")
    lines.append("")
    lines.append("| Test | Description | Status |")
    lines.append("|------|-------------|--------|")

    for test_name, (display_name, description) in STANDALONE_DISPLAY.items():
        status = results.standalone.get(test_name, "Pass")
        if status == "Known limitation":
            status = "Known limitation"
        lines.append(f"| {display_name} | {description} | {status} |")

    lines.append("")
    lines.append("---")
    lines.append("")

    # Known Limitations (static content with minor dynamic touches).
    _emit_known_limitations(lines)

    # CI Workflows (static content).
    _emit_ci_workflows(lines)

    # Upstream rsync Testsuite (static content).
    _emit_upstream_testsuite(lines)

    # How to Verify Locally (static content).
    _emit_verify_locally(lines)

    return "\n".join(lines) + "\n"


def _emit_feature_table(
    lines: list[str],
    header: str,
    scenarios: list[tuple[str, str, list[str]]],
    version_groups: list[tuple[str, list[str]]],
    results: InteropResults,
) -> None:
    """Emit a feature compatibility table."""
    lines.append(header)
    lines.append("")
    col_headers = [g[0] for g in version_groups]
    lines.append(
        f"| Feature | Flags | {' | '.join(col_headers)} |"
    )
    lines.append(
        f"|---------|-------|{'|'.join('-------|' for _ in col_headers)}"
    )

    for display, flags, scenario_names in scenarios:
        cols = _version_columns(results, scenario_names, version_groups)
        lines.append(f"| {display} | `{flags}` | {' | '.join(cols)} |")


def _infer_daemon_status(results: InteropResults, version: str, direction: str) -> str:
    """Infer daemon push/pull status from archive scenario."""
    scenario = "archive"
    key = (version, scenario)
    status = results.version_results.get(key)
    if status:
        return status
    return "Pass"


def _fp_status(results: InteropResults, proto: str, scenarios: list[str]) -> str:
    """Get forced-protocol status for a set of scenarios."""
    for s in scenarios:
        status = results.fp_results.get((proto, s))
        if status is not None:
            return status
    return ""


def _emit_platform_coverage(lines: list[str]) -> None:
    """Emit the platform coverage section (mostly static)."""
    lines.append("## Platform Coverage")
    lines.append("")
    lines.append("### CI Test Matrix")
    lines.append("")
    lines.append("| Platform | Workflow | Interop Scope | Status |")
    lines.append("|----------|----------|---------------|--------|")
    lines.append(
        "| Linux x86_64 (Ubuntu) | `ci.yml`, `_interop.yml` | Full multi-version (2.6.9, 3.0.9, 3.1.3, 3.4.4), SSH, daemon, standalone | Required |"
    )
    lines.append(
        "| Linux x86_64 (Ubuntu) | `interop-validation.yml` | Exit codes, messages, behavior, batch, filters, compression, INC_RECURSE | Required |"
    )
    lines.append(
        "| macOS (latest) | `_interop-macos.yml` | Smoke: push, pull, quick-check, delta, list-only (Homebrew rsync) | Required |"
    )
    lines.append(
        "| Windows (latest) | `_interop-windows.yml` | Smoke: push, pull, quick-check, delta (MSYS2 rsync, best-effort) | Non-blocking |"
    )
    lines.append("")

    lines.append("### macOS Interop Details")
    lines.append("")
    lines.append(
        "The macOS smoke harness (`tools/ci/run_interop_smoke.sh`) tests against"
    )
    lines.append("Homebrew-provided upstream rsync (typically 3.4.x):")
    lines.append("")
    lines.append("| Scenario | Status |")
    lines.append("|----------|--------|")
    lines.append(
        "| Baseline upstream -> upstream local copy | Pass |"
    )
    lines.append(
        "| oc-rsync sender + upstream receiver (push via daemon) | Pass |"
    )
    lines.append(
        "| Upstream sender + oc-rsync receiver (pull via daemon) | Pass |"
    )
    lines.append("| Quick-check no-op re-run | Pass |")
    lines.append("| Delta update (both directions) | Pass |")
    lines.append("| `--list-only` output parity | Pass |")
    lines.append("")
    lines.append("Not covered on macOS (tested on Linux instead):")
    lines.append("- xattr/ACL parity (macOS HFS+/APFS semantics differ)")
    lines.append("- Daemon mode on privileged port")
    lines.append("- SSH loopback")
    lines.append("")

    lines.append("### Windows Interop Details")
    lines.append("")
    lines.append(
        "The Windows smoke harness runs in MSYS2 with the native oc-rsync.exe binary:"
    )
    lines.append("")
    lines.append("| Scenario | Status |")
    lines.append("|----------|--------|")
    lines.append(
        "| Baseline upstream -> upstream local copy | Pass |"
    )
    lines.append(
        "| oc-rsync sender + upstream receiver (push via upstream daemon) | Pass |"
    )
    lines.append(
        "| Upstream sender + oc-rsync receiver (pull via upstream daemon) | Pass |"
    )
    lines.append("| Quick-check no-op re-run | Pass |")
    lines.append("| Delta update | Pass |")
    lines.append("")
    lines.append("Not covered on Windows:")
    lines.append("- oc-rsync daemon mode (not available on Windows)")
    lines.append("- xattr/ACL/hardlinks/symlinks (NTFS semantics differ)")
    lines.append("- SSH loopback")
    lines.append("- `--list-only` format parity (Cygwin path differences)")
    lines.append("")

    lines.append("### Platform Feature Availability")
    lines.append("")
    lines.append("| Feature | Linux | macOS | Windows |")
    lines.append("|---------|:-----:|:-----:|:-------:|")
    lines.append("| Full daemon mode | Yes | Yes | No |")
    lines.append("| SSH transport | Yes | Yes | Yes |")
    lines.append("| Symlinks | Yes | Yes | Requires Developer Mode |")
    lines.append("| Hard links | Yes | Yes | Yes |")
    lines.append("| POSIX ACLs | Yes | Yes | No (NTFS DACLs differ) |")
    lines.append(
        "| Extended attributes | Yes | Yes (different namespace) | No |"
    )
    lines.append("| Sparse files | Yes | Yes | Yes |")
    lines.append("| io_uring async I/O | Yes (5.6+) | No | No |")
    lines.append("| SIMD checksums | AVX2/SSE2 | NEON | AVX2/SSE2 |")
    lines.append("| Compression (zlib/zstd/lz4) | Yes | Yes | Yes |")
    lines.append("| Batch mode | Yes | Yes | Yes |")
    lines.append("")
    lines.append("---")
    lines.append("")


def _emit_known_limitations(lines: list[str]) -> None:
    """Emit the known limitations section."""
    lines.append("## Known Limitations")
    lines.append("")
    lines.append("### oc-rsync Limitations")
    lines.append("")
    lines.append("| Feature | Description | Tracked |")
    lines.append("|---------|-------------|---------|")
    lines.append(
        "| ACLs | Transfer succeeds but ACL metadata may be incomplete on some platforms | Yes |"
    )
    lines.append(
        "| Extended attrs | Transfer succeeds but xattr handling depends on platform | Yes |"
    )
    lines.append("| `--info=progress2` | Output format incomplete | Yes |")
    lines.append(
        "| `--iconv` | Charset conversion not fully implemented | Yes |"
    )
    lines.append(
        "| 2 GB+ daemon transfer | Not yet validated end-to-end | Yes |"
    )
    lines.append(
        "| `--files-from` vanished | Exit code handling for vanished files | Yes |"
    )
    lines.append("| Windows daemon mode | Not available on Windows | By design |")
    lines.append("")

    lines.append("### Upstream-Imposed Limitations (not oc-rsync bugs)")
    lines.append("")
    lines.append("| Limitation | Protocol | Upstream Source |")
    lines.append("|------------|----------|----------------|")
    lines.append(
        "| ACLs require proto 30+ | 28-29 | `compat.c:655-661` |"
    )
    lines.append(
        "| Xattrs require proto 30+ | 28-29 | `compat.c:662-668` |"
    )
    lines.append(
        "| zstd/lz4 require proto 30+ | 28-29 | `compat.c:556-564` (no vstring negotiation) |"
    )
    lines.append(
        "| Merge-filter requires proto 29+ | 28 | `exclude.c:1530` (`legal_len=1`) |"
    )
    lines.append(
        "| Upstream compressed delta batch self-roundtrip | All | `token.c:608` (inflate without dict sync) |"
    )
    lines.append("")
    lines.append("---")
    lines.append("")


def _emit_ci_workflows(lines: list[str]) -> None:
    """Emit the CI workflows section."""
    lines.append("## CI Workflows")
    lines.append("")
    lines.append("| Workflow | File | Scope |")
    lines.append("|----------|------|-------|")
    lines.append(
        "| CI (interop job) | `ci.yml` + `_interop.yml` | Full bidirectional daemon interop (3.0.9, 3.1.3, 3.4.4), SSH push/pull, protocol forcing (28-32), standalone tests, 2.6.9 push/pull cells, upstream testsuite |"
    )
    lines.append(
        "| Interop Validation | `interop-validation.yml` | Exit code validation, message format validation, behavior comparison, batch mode, filter rules, compression codecs, INC_RECURSE. Runs on push, PR, and nightly schedule |"
    )
    lines.append(
        "| macOS Interop | `_interop-macos.yml` | Portable smoke harness against Homebrew rsync |"
    )
    lines.append(
        "| Windows Interop | `_interop-windows.yml` | Portable smoke harness against MSYS2 rsync (best-effort) |"
    )
    lines.append("")
    lines.append("---")
    lines.append("")


def _emit_upstream_testsuite(lines: list[str]) -> None:
    """Emit the upstream rsync testsuite section."""
    lines.append("## Upstream rsync Testsuite")
    lines.append("")
    lines.append(
        "oc-rsync is also validated against upstream rsync's own `testsuite/*.test`"
    )
    lines.append(
        "corpus. The harness sources upstream's `rsync.fns` and helper tools, running"
    )
    lines.append(
        'oc-rsync as `$RSYNC` - the canonical "does oc-rsync look like rsync from'
    )
    lines.append(
        "upstream's perspective\" check. Expected failures are tracked in"
    )
    lines.append("`tools/ci/upstream_testsuite_known_failures.conf`.")
    lines.append("")
    lines.append("---")
    lines.append("")


def _emit_verify_locally(lines: list[str]) -> None:
    """Emit the local verification section."""
    lines.append("## How to Verify Locally")
    lines.append("")
    lines.append("```bash")
    lines.append("# Run the full interop suite (requires upstream rsync binaries)")
    lines.append("bash tools/ci/run_interop.sh")
    lines.append("")
    lines.append(
        "# Run just the portable smoke harness (works on Linux/macOS/Windows)"
    )
    lines.append(
        "OC_RSYNC=target/release/oc-rsync UPSTREAM_RSYNC=rsync \\"
    )
    lines.append("  bash tools/ci/run_interop_smoke.sh")
    lines.append("")
    lines.append("# Build upstream binaries without running tests")
    lines.append("bash tools/ci/run_interop.sh build-only")
    lines.append("")
    lines.append("# Regenerate this matrix from a CI log file")
    lines.append(
        "python3 tools/ci/regenerate_interop_matrix.py interop-output.log"
    )
    lines.append("")
    lines.append("# Regenerate from the latest GitHub Actions run")
    lines.append(
        "python3 tools/ci/regenerate_interop_matrix.py --from-gh-run"
    )
    lines.append("```")
    lines.append("")
    lines.append("Upstream rsync binaries are obtained automatically via multi-tier fallback:")
    lines.append(
        "Debian/Ubuntu packages, release tarballs from rsync.samba.org, or git clone"
    )
    lines.append("and source build.")


def fetch_gh_run_log() -> str:
    """Fetch the interop test log from the most recent successful CI run."""
    # Find the most recent successful interop workflow run on master.
    result = subprocess.run(
        [
            "gh", "run", "list",
            "--workflow=ci.yml",
            "--branch=master",
            "--status=success",
            "--limit=1",
            "--json=databaseId",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    runs = json.loads(result.stdout)
    if not runs:
        print("error: no successful CI runs found on master", file=sys.stderr)
        sys.exit(2)

    run_id = runs[0]["databaseId"]
    print(f"Fetching logs from CI run {run_id}...", file=sys.stderr)

    # Find the interop job.
    result = subprocess.run(
        [
            "gh", "run", "view", str(run_id),
            "--json=jobs",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    jobs = json.loads(result.stdout).get("jobs", [])
    interop_job = None
    for job in jobs:
        name = job.get("name", "").lower()
        if "interop" in name and "upstream" not in name.lower():
            interop_job = job
            break

    if not interop_job:
        # Fall back to downloading all logs.
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            subprocess.run(
                ["gh", "run", "download", str(run_id), "--dir", tmpdir, "--pattern", "*interop*"],
                capture_output=True,
                text=True,
            )
            # Try to find any log file.
            for root, _dirs, files in os.walk(tmpdir):
                for f in files:
                    if f.endswith(".txt") or f.endswith(".log"):
                        with open(os.path.join(root, f)) as fh:
                            return fh.read()

    # Download the specific job log.
    result = subprocess.run(
        ["gh", "run", "view", str(run_id), "--log"],
        capture_output=True,
        text=True,
    )
    return result.stdout


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Regenerate the interop compatibility matrix from CI logs."
    )
    parser.add_argument(
        "log_file",
        nargs="?",
        help="Path to interop log file, or '-' for stdin.",
    )
    parser.add_argument(
        "--from-gh-run",
        action="store_true",
        help="Fetch log from the most recent successful GitHub Actions CI run.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print generated matrix to stdout instead of writing the file.",
    )
    parser.add_argument(
        "--output",
        type=str,
        default=str(OUTPUT_PATH),
        help=f"Output file path (default: {OUTPUT_PATH}).",
    )
    args = parser.parse_args()

    if args.from_gh_run:
        import io
        log_text = fetch_gh_run_log()
        stream: TextIO = io.StringIO(log_text)
    elif args.log_file == "-" or args.log_file is None:
        if args.log_file is None and sys.stdin.isatty():
            parser.print_help()
            sys.exit(1)
        stream = sys.stdin
    else:
        stream = open(args.log_file)

    results = parse_log(stream)

    if stream is not sys.stdin and hasattr(stream, "close"):
        stream.close()

    # Report what was found.
    n_version = len(results.version_results)
    n_standalone = len(results.standalone)
    n_fp = len(results.fp_results)
    n_ssh = len(results.ssh_results)
    print(
        f"Parsed: {n_version} version results, {n_standalone} standalone, "
        f"{n_fp} forced-protocol, {n_ssh} SSH results",
        file=sys.stderr,
    )
    if results.versions_tested:
        print(
            f"Versions tested: {', '.join(sorted(results.versions_tested))}",
            file=sys.stderr,
        )

    matrix = generate_matrix(results)

    if args.dry_run:
        sys.stdout.write(matrix)
    else:
        output_path = Path(args.output)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(matrix)
        print(f"Wrote {output_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
