#!/usr/bin/env python3
"""Run benchmark suite comparing oc-rsync against upstream rsync.

Outputs JSON results to stdout.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

UPSTREAM = "target/interop/upstream-src/rsync-3.4.1/rsync"
OC_RSYNC = "target/release/oc-rsync"


def benchmark(cmd, runs=5):
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        subprocess.run(cmd, shell=True, capture_output=True)
        elapsed = time.perf_counter() - start
        times.append(elapsed)
    return {
        "mean": sum(times) / len(times),
        "min": min(times),
        "max": max(times),
    }


def main():
    tmpdir = tempfile.mkdtemp(prefix="rsync_bench_")
    results = {"tests": [], "summary": {}}

    try:
        src = f"{tmpdir}/src"
        dst_up = f"{tmpdir}/dst_upstream"
        dst_oc = f"{tmpdir}/dst_oc"

        os.makedirs(f"{src}/small", exist_ok=True)
        os.makedirs(f"{src}/medium", exist_ok=True)
        os.makedirs(f"{src}/large", exist_ok=True)

        # Create test data
        print("Creating test data...", file=sys.stderr)

        # Small files (1000 x 1KB)
        for i in range(1000):
            with open(f"{src}/small/file_{i}.txt", "wb") as f:
                f.write(os.urandom(1024))

        # Medium files (100 x 100KB)
        for i in range(100):
            with open(f"{src}/medium/file_{i}.bin", "wb") as f:
                f.write(os.urandom(100 * 1024))

        # Large files (10 x 10MB)
        for i in range(10):
            with open(f"{src}/large/file_{i}.dat", "wb") as f:
                f.write(os.urandom(10 * 1024 * 1024))

        total_size = sum(
            os.path.getsize(os.path.join(dp, f))
            for dp, dn, fn in os.walk(src)
            for f in fn
        )
        total_files = sum(len(fn) for _, _, fn in os.walk(src))

        results["test_data"] = {
            "size_mb": round(total_size / 1024 / 1024, 1),
            "files": total_files,
        }

        def reset_dst():
            shutil.rmtree(dst_up, ignore_errors=True)
            shutil.rmtree(dst_oc, ignore_errors=True)
            os.makedirs(dst_up, exist_ok=True)
            os.makedirs(dst_oc, exist_ok=True)

        tests = [
            ("initial_sync", "Initial sync (-av)", f"-av {src}/ {{dst}}/", True),
            ("nochange_sync", "No-change sync (-av)", f"-av {src}/ {{dst}}/", False),
            ("checksum_sync", "Checksum sync (-avc)", f"-avc {src}/ {{dst}}/", False),
            ("dryrun", "Dry-run (-avn)", f"-avn {src}/ {{dst}}/", False),
            (
                "delete_sync",
                "Delete sync (--delete)",
                f"-av --delete {src}/ {{dst}}/",
                False,
            ),
            ("large_files", "Large files (100MB)", f"-av {src}/large/ {{dst}}/", True),
            (
                "small_files",
                "Small files (1000x1KB)",
                f"-av {src}/small/ {{dst}}/",
                True,
            ),
        ]

        for test_id, name, args, do_reset in tests:
            print(f"Running: {name}...", file=sys.stderr)

            if do_reset:
                reset_dst()

            up_args = args.format(dst=dst_up)
            oc_args = args.format(dst=dst_oc)

            up_result = benchmark(f"{UPSTREAM} {up_args}")
            oc_result = benchmark(f"{OC_RSYNC} {oc_args}")

            ratio = (
                oc_result["mean"] / up_result["mean"] if up_result["mean"] > 0 else 0
            )

            results["tests"].append(
                {
                    "id": test_id,
                    "name": name,
                    "upstream": up_result,
                    "oc_rsync": oc_result,
                    "ratio": round(ratio, 2),
                }
            )

        # Calculate summary
        ratios = [t["ratio"] for t in results["tests"]]
        results["summary"] = {
            "avg_ratio": round(sum(ratios) / len(ratios), 2),
            "best_ratio": round(min(ratios), 2),
            "worst_ratio": round(max(ratios), 2),
        }

        print(json.dumps(results, indent=2))

    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
