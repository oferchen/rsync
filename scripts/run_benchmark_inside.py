#!/usr/bin/env python3
"""Three-way benchmark: upstream rsync 3.4.1 vs oc-rsync v0.5.4 vs dev snapshot.

Runs inside the benchmark container.  Generates test data of varying profiles
and measures wall-clock time for several transfer scenarios.

Usage: python3 run_benchmark_inside.py [--runs N] [--json]
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

BINARIES = {
    "upstream": "/usr/local/bin/upstream-rsync",
    "v0.5.4": "/usr/local/bin/oc-rsync-v054",
    "dev": "/usr/local/bin/oc-rsync-dev",
}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def version_string(binary):
    try:
        out = subprocess.run(
            [binary, "--version"], capture_output=True, text=True, timeout=5
        )
        return out.stdout.splitlines()[0] if out.stdout else "unknown"
    except Exception:
        return "unavailable"


def benchmark(cmd, runs=5, warmup=1):
    """Time a shell command, returning per-run samples."""
    for _ in range(warmup):
        subprocess.run(cmd, shell=True, capture_output=True)

    times = []
    for _ in range(runs):
        start = time.perf_counter()
        result = subprocess.run(cmd, shell=True, capture_output=True)
        elapsed = time.perf_counter() - start
        times.append({"elapsed": elapsed, "returncode": result.returncode})
    return times


def stats(samples):
    """Compute mean/min/max from samples."""
    times = [s["elapsed"] for s in samples]
    return {
        "mean": round(sum(times) / len(times), 4),
        "min": round(min(times), 4),
        "max": round(max(times), 4),
    }


def create_test_data(base):
    """Generate test data with three tiers."""
    os.makedirs(f"{base}/small", exist_ok=True)
    os.makedirs(f"{base}/medium", exist_ok=True)
    os.makedirs(f"{base}/large", exist_ok=True)
    os.makedirs(f"{base}/deep/a/b/c/d/e", exist_ok=True)

    # 1000 x 1KB
    for i in range(1000):
        with open(f"{base}/small/file_{i:04d}.txt", "wb") as f:
            f.write(os.urandom(1024))

    # 100 x 100KB
    for i in range(100):
        with open(f"{base}/medium/file_{i:03d}.bin", "wb") as f:
            f.write(os.urandom(100 * 1024))

    # 10 x 10MB
    for i in range(10):
        with open(f"{base}/large/file_{i:02d}.dat", "wb") as f:
            f.write(os.urandom(10 * 1024 * 1024))

    # Deep directory tree
    for i in range(20):
        depth = "deep/" + "/".join("abcde"[: (i % 5) + 1])
        os.makedirs(f"{base}/{depth}", exist_ok=True)
        with open(f"{base}/{depth}/nested_{i}.txt", "wb") as f:
            f.write(os.urandom(4096))


def total_stats(path):
    total_bytes = 0
    total_files = 0
    for dp, _, fnames in os.walk(path):
        for fn in fnames:
            total_bytes += os.path.getsize(os.path.join(dp, fn))
            total_files += 1
    return total_bytes, total_files


def modify_files(src, fraction=0.1):
    all_files = []
    for dp, _, fnames in os.walk(src):
        for fn in fnames:
            all_files.append(os.path.join(dp, fn))
    count = max(1, int(len(all_files) * fraction))
    for path in all_files[:count]:
        with open(path, "r+b") as f:
            data = f.read()
            mid = len(data) // 2
            patch = os.urandom(min(64, len(data)))
            f.seek(mid)
            f.write(patch)


def ratio_str(ratio):
    if ratio < 0.85:
        return "FASTER"
    elif ratio <= 1.15:
        return "~same"
    else:
        return "slower"


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="oc-rsync three-way benchmark")
    parser.add_argument("--runs", type=int, default=5, help="Runs per test")
    parser.add_argument("--json", action="store_true", help="Output JSON")
    args = parser.parse_args()

    runs = args.runs

    tmpdir = tempfile.mkdtemp(prefix="rsync_bench_")
    src = f"{tmpdir}/src"

    # Each binary gets its own destination tree
    dsts = {}
    for name in BINARIES:
        dsts[name] = f"{tmpdir}/dst_{name}"

    results = {"tests": [], "summary": {}}

    try:
        # Versions
        versions = {}
        for name, binary in BINARIES.items():
            versions[name] = version_string(binary)

        results["versions"] = versions

        if not args.json:
            print("=" * 78)
            print("  oc-rsync Performance Benchmark — Three-Way Comparison")
            print("=" * 78)
            for name, ver in versions.items():
                print(f"  {name:>10} : {ver}")
            print(f"  {'runs':>10} : {runs}")
            print()

        # Create test data
        if not args.json:
            print("Creating test data...", file=sys.stderr)
        create_test_data(src)
        total_bytes, total_files = total_stats(src)
        total_mb = total_bytes / 1024 / 1024
        results["test_data"] = {
            "size_mb": round(total_mb, 1),
            "files": total_files,
        }
        if not args.json:
            print(f"Test data: {total_mb:.1f} MB, {total_files} files\n")

        # Test definitions
        tests = [
            {"id": "initial_sync", "name": "Initial sync (-a)",
             "args": "-a {src}/ {dst}/", "reset": True},
            {"id": "no_change", "name": "No-change sync (-a)",
             "args": "-a {src}/ {dst}/", "reset": False},
            {"id": "checksum", "name": "Checksum sync (-ac)",
             "args": "-ac {src}/ {dst}/", "reset": False},
            {"id": "dry_run", "name": "Dry run (-an)",
             "args": "-an {src}/ {dst}/", "reset": False},
            {"id": "incremental", "name": "Incremental (10% changed)",
             "args": "-a {src}/ {dst}/", "reset": False,
             "pre": lambda: modify_files(src, 0.1)},
            {"id": "delete", "name": "With --delete",
             "args": "-a --delete {src}/ {dst}/", "reset": False},
            {"id": "large_only", "name": "Large files (100MB)",
             "args": "-a {src}/large/ {dst}/", "reset": True},
            {"id": "small_only", "name": "Small files (1000x1KB)",
             "args": "-a {src}/small/ {dst}/", "reset": True},
            {"id": "compressed", "name": "Compressed (-az)",
             "args": "-az {src}/ {dst}/", "reset": True},
        ]

        def reset_all():
            for dst in dsts.values():
                shutil.rmtree(dst, ignore_errors=True)
                os.makedirs(dst, exist_ok=True)

        # Table header
        if not args.json:
            hdr = f"{'Test':<28}"
            for name in BINARIES:
                hdr += f" {name:>10}"
            hdr += f" {'v054/up':>8} {'dev/up':>8} {'dev/054':>8}"
            print(hdr)
            print("-" * len(hdr))

        for test in tests:
            if test.get("reset"):
                reset_all()

            if "pre" in test:
                test["pre"]()

            test_results = {}
            for name, binary in BINARIES.items():
                cmd_args = test["args"].format(src=src, dst=dsts[name])
                samples = benchmark(f"{binary} {cmd_args}", runs=runs)
                test_results[name] = stats(samples)

            # Ratios
            up_mean = test_results["upstream"]["mean"]
            v054_mean = test_results["v0.5.4"]["mean"]
            dev_mean = test_results["dev"]["mean"]

            r_v054_up = v054_mean / up_mean if up_mean > 0 else 0
            r_dev_up = dev_mean / up_mean if up_mean > 0 else 0
            r_dev_v054 = dev_mean / v054_mean if v054_mean > 0 else 0

            entry = {
                "id": test["id"],
                "name": test["name"],
                "upstream": test_results["upstream"],
                "v0.5.4": test_results["v0.5.4"],
                "dev": test_results["dev"],
                "ratio_v054_vs_upstream": round(r_v054_up, 3),
                "ratio_dev_vs_upstream": round(r_dev_up, 3),
                "ratio_dev_vs_v054": round(r_dev_v054, 3),
            }
            results["tests"].append(entry)

            if not args.json:
                row = f"{test['name']:<28}"
                for name in BINARIES:
                    row += f" {test_results[name]['mean']:>9.3f}s"
                row += f" {r_v054_up:>7.2f}x {r_dev_up:>7.2f}x {r_dev_v054:>7.2f}x"
                print(row)

        # Summary
        r1 = [t["ratio_v054_vs_upstream"] for t in results["tests"]]
        r2 = [t["ratio_dev_vs_upstream"] for t in results["tests"]]
        r3 = [t["ratio_dev_vs_v054"] for t in results["tests"]]

        results["summary"] = {
            "v054_vs_upstream": {
                "avg": round(sum(r1) / len(r1), 3),
                "best": round(min(r1), 3),
                "worst": round(max(r1), 3),
            },
            "dev_vs_upstream": {
                "avg": round(sum(r2) / len(r2), 3),
                "best": round(min(r2), 3),
                "worst": round(max(r2), 3),
            },
            "dev_vs_v054": {
                "avg": round(sum(r3) / len(r3), 3),
                "best": round(min(r3), 3),
                "worst": round(max(r3), 3),
            },
        }

        if not args.json:
            print("-" * 78)
            print()
            print("Ratios (< 1.0 = faster, > 1.0 = slower):")
            print()
            s = results["summary"]
            print(f"  v0.5.4 vs upstream : avg {s['v054_vs_upstream']['avg']:.2f}x"
                  f"  (best {s['v054_vs_upstream']['best']:.2f}x,"
                  f" worst {s['v054_vs_upstream']['worst']:.2f}x)")
            print(f"  dev vs upstream    : avg {s['dev_vs_upstream']['avg']:.2f}x"
                  f"  (best {s['dev_vs_upstream']['best']:.2f}x,"
                  f" worst {s['dev_vs_upstream']['worst']:.2f}x)")
            print(f"  dev vs v0.5.4      : avg {s['dev_vs_v054']['avg']:.2f}x"
                  f"  (best {s['dev_vs_v054']['best']:.2f}x,"
                  f" worst {s['dev_vs_v054']['worst']:.2f}x)")

        if args.json:
            print(json.dumps(results, indent=2))

    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
