#!/usr/bin/env python3
"""Generate a markdown report from benchmark_results.json.

Reads benchmark_results.json from the current directory and writes
a markdown table to stdout.
"""

import json
import sys


def main():
    with open("benchmark_results.json") as f:
        data = json.load(f)

    print("## Benchmark Results\n")
    print(
        f"Test data: {data['test_data']['size_mb']}MB "
        f"({data['test_data']['files']} files)\n"
    )
    print("| Test | Upstream | oc-rsync | Ratio |")
    print("|------|----------|----------|-------|")

    for t in data["tests"]:
        up = t["upstream"]["mean"]
        oc = t["oc_rsync"]["mean"]
        ratio = t["ratio"]
        if ratio < 1.0:
            indicator = "faster"
        elif ratio < 2.0:
            indicator = "similar"
        else:
            indicator = "slower"
        print(
            f"| {t['name']} | {up:.3f}s | {oc:.3f}s | {indicator} {ratio:.2f}x |"
        )

    print(f"\n**Summary:** Average ratio: {data['summary']['avg_ratio']}x")
    print(f"- Best: {data['summary']['best_ratio']}x")
    print(f"- Worst: {data['summary']['worst_ratio']}x")
    print(f"\n_Ratio < 1.0 = oc-rsync faster, > 1.0 = upstream faster_")


if __name__ == "__main__":
    main()
