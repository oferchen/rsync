#!/usr/bin/env python3
"""Two-owner FFI dependency gate, and the blocking check over it.

The unsafe-code policy names exactly two crates that may carry FFI: `platform`
(process / identity / environment / signals) and `fast_io` (I/O syscalls).
Every other crate is meant to call their safe public APIs instead of declaring
its own FFI dependency.  The checkable form of that rule is the dependency
list: a crate OUTSIDE the two owners must not declare `libc`, `windows-sys`,
`windows`, `nix`, or `rustix` as a direct dependency.

The scan reads each workspace crate's Cargo.toml and looks at its production
dependency tables - `[dependencies]` and `[target.*.dependencies]` - only.
`[dev-dependencies]` and `[build-dependencies]` are test / build tooling that
cannot lend production code an unsafe FFI surface, so they are out of scope.
A dependency is matched by its EFFECTIVE crate name: a `package = "libc"`
rename is still a `libc` dependency however the key is spelled.

The tree does not satisfy the rule today, so the gate is not "zero direct FFI
deps" - it is "zero UNALLOWLISTED direct FFI deps".  Every current violator is
carried in tools/ci/unsafe_owner_deps_allowlist.tsv with a reason and an
expiry date, exactly as the dead-code census gate carries its tier.  The gate
fails when:

  * a crate declares an owner dep and has no allowlist row (a NEW violator);
  * an allowlist row has EXPIRED;
  * an allowlist row is STALE - the crate no longer declares that dep, so the
    row must be removed in the change that removed the dependency;
  * a row is MALFORMED (not four tab-separated fields, or a bad expiry date);
  * a row has an empty reason.

`platform` and `fast_io` are the exempt owners; a row naming either of them is
itself an error, because the rule does not apply to them.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import sys
import tomllib
from pathlib import Path

ALLOWLIST = Path(__file__).resolve().parent / "unsafe_owner_deps_allowlist.tsv"

# The FFI crates the two-owner rule is about.  Widening this set widens what
# the gate polices, so change it deliberately.
OWNER_DEPS = frozenset({"libc", "windows-sys", "windows", "nix", "rustix"})

# The two crates permitted to hold FFI.  A crate name here is never a violator
# and must never appear in the allowlist.
OWNER_CRATES = frozenset({"platform", "fast_io"})

# Production dependency tables.  dev/build dependencies are excluded: they are
# test and build-script tooling and cannot grant production code an FFI surface.
PROD_TABLES = ("dependencies",)


def crate_manifests(root: Path) -> list[Path]:
    """Every workspace crate manifest: crates/<name>/Cargo.toml plus xtask.

    Nested manifests such as crates/*/fuzz/Cargo.toml are deliberately skipped
    - fuzz targets are not shipped crates and are not workspace members.
    """
    manifests = []
    crates_dir = root / "crates"
    if crates_dir.is_dir():
        for child in sorted(crates_dir.iterdir()):
            manifest = child / "Cargo.toml"
            if manifest.is_file():
                manifests.append(manifest)
    xtask = root / "xtask" / "Cargo.toml"
    if xtask.is_file():
        manifests.append(xtask)
    return manifests


def effective_dep_names(table: dict) -> set[str]:
    """Effective crate names declared in one dependency table.

    A dependency's effective crate name is its `package` rename when present,
    otherwise the table key.  So `foo = { package = "libc" }` counts as `libc`.
    """
    names: set[str] = set()
    for key, spec in table.items():
        if isinstance(spec, dict) and isinstance(spec.get("package"), str):
            names.add(spec["package"])
        else:
            names.add(key)
    return names


def scan_crate(manifest: Path) -> tuple[str, set[str]]:
    """Return (crate name, set of owner deps it declares in production tables)."""
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    name = data.get("package", {}).get("name", manifest.parent.name)
    declared: set[str] = set()
    for tbl in PROD_TABLES:
        if isinstance(data.get(tbl), dict):
            declared |= effective_dep_names(data[tbl])
    target = data.get("target")
    if isinstance(target, dict):
        for cfg_spec in target.values():
            if not isinstance(cfg_spec, dict):
                continue
            for tbl in PROD_TABLES:
                if isinstance(cfg_spec.get(tbl), dict):
                    declared |= effective_dep_names(cfg_spec[tbl])
    return name, declared & OWNER_DEPS


def scan(root: Path) -> list[tuple[str, str, str]]:
    """All (crate, dep, manifest-relative-path) violations outside the owners."""
    violations: list[tuple[str, str, str]] = []
    for manifest in crate_manifests(root):
        name, owner_deps = scan_crate(manifest)
        if name in OWNER_CRATES:
            continue
        rel = manifest.relative_to(root).as_posix()
        for dep in sorted(owner_deps):
            violations.append((name, dep, rel))
    return violations


def parse_allowlist(path: Path) -> tuple[dict[tuple[str, str], dict], list[str]]:
    """Parse `crate<TAB>dep<TAB>expiry<TAB>reason` rows keyed on (crate, dep)."""
    entries: dict[tuple[str, str], dict] = {}
    errors: list[str] = []
    if not path.exists():
        return entries, [f"allowlist not found: {path}"]
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 4:
            errors.append(f"{path.name}:{lineno}: expected 4 tab-separated fields")
            continue
        crate, dep, expiry, reason = (p.strip() for p in parts)
        if not reason:
            errors.append(f"{path.name}:{lineno}: {crate}/{dep}: empty reason")
            continue
        if crate in OWNER_CRATES:
            errors.append(
                f"{path.name}:{lineno}: {crate} is an owner crate - the rule does "
                f"not apply to it, so it must not be allowlisted"
            )
            continue
        if dep not in OWNER_DEPS:
            errors.append(
                f"{path.name}:{lineno}: {dep!r} is not an owner dep "
                f"({', '.join(sorted(OWNER_DEPS))})"
            )
            continue
        try:
            expiry_date = _dt.date.fromisoformat(expiry)
        except ValueError:
            errors.append(
                f"{path.name}:{lineno}: {crate}/{dep}: expiry {expiry!r} "
                f"is not YYYY-MM-DD"
            )
            continue
        key = (crate, dep)
        if key in entries:
            errors.append(f"{path.name}:{lineno}: duplicate entry for {crate}/{dep}")
            continue
        entries[key] = {"expiry": expiry_date, "reason": reason, "lineno": lineno}
    return entries, errors


def run_gate(root: Path, allowlist_path: Path, today: _dt.date) -> int:
    violations = scan(root)
    entries, problems = parse_allowlist(allowlist_path)
    live_keys = {(c, d) for c, d, _ in violations}

    for crate, dep, rel in violations:
        entry = entries.get((crate, dep))
        if entry is None:
            problems.append(
                f"{crate} declares a direct `{dep}` dependency ({rel}). Only "
                f"{' and '.join(sorted(OWNER_CRATES))} may carry FFI deps - route "
                f"through their safe APIs, or add an allowlist row with a reason "
                f"and an expiry date."
            )
        elif entry["expiry"] < today:
            problems.append(
                f"{crate}/{dep} allowlist entry EXPIRED on {entry['expiry']} "
                f"(reason was: {entry['reason']}). Remove the dependency, or renew "
                f"the expiry deliberately."
            )
    for (crate, dep), entry in sorted(entries.items()):
        if (crate, dep) not in live_keys:
            problems.append(
                f"stale allowlist row: {crate}/{dep} ({allowlist_path.name}:"
                f"{entry['lineno']}) no longer declares that dependency - "
                f"remove the row."
            )

    if problems:
        print(
            f"unsafe-owner-deps gate: {len(problems)} problem(s)", file=sys.stderr
        )
        for p in problems:
            print(f"error: {p}", file=sys.stderr)
        return 1
    print(
        f"unsafe-owner-deps gate: OK ({len(violations)} direct FFI deps outside "
        f"{'/'.join(sorted(OWNER_CRATES))}, all allowlisted and unexpired)"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "mode",
        choices=["gate", "report"],
        nargs="?",
        default="gate",
        help="gate: enforce the allowlist (CI); report: print the violations",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root to scan",
    )
    parser.add_argument("--allowlist", type=Path, default=ALLOWLIST)
    parser.add_argument(
        "--today",
        type=_dt.date.fromisoformat,
        default=_dt.date.today(),
        help="override the expiry reference date (tests)",
    )
    args = parser.parse_args(argv)

    if args.mode == "report":
        for crate, dep, rel in scan(args.root):
            print(f"{crate}\t{dep}\t{rel}")
        return 0
    return run_gate(args.root, args.allowlist, args.today)


if __name__ == "__main__":
    sys.exit(main())
