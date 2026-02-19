#!/usr/bin/env python3
"""Generate a markdown report from benchmark_results.json.

Reads benchmark_results.json from the current directory and writes
a markdown table grouped by transfer mode to stdout.
"""

import json
import sys

MODE_LABELS = {
    "local": "Local Copy",
    "ssh_pull": "SSH Pull",
    "ssh_push": "SSH Push",
    "daemon_pull": "Daemon Pull",
    "daemon_push": "Daemon Push",
}


def ratio_indicator(ratio):
    if ratio < 0.95:
        return "faster"
    elif ratio <= 1.05:
        return "~same"
    else:
        return "slower"


def main():
    with open("benchmark_results.json") as f:
        data = json.load(f)

    print("## Benchmark Results\n")
    print(
        f"Test data: {data['test_data']['size_mb']}MB "
        f"({data['test_data']['files']} files)\n"
    )

    # Group tests by mode
    by_mode = {}
    for t in data["tests"]:
        by_mode.setdefault(t["mode"], []).append(t)

    for mode, label in MODE_LABELS.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        print(f"### {label}\n")
        print("| Test | Upstream | oc-rsync | Ratio |")
        print("|------|----------|----------|-------|")

        for t in tests:
            up = t["upstream"]["mean"]
            oc = t["oc_rsync"]["mean"]
            ratio = t["ratio"]
            ind = ratio_indicator(ratio)
            print(f"| {t['name']} | {up:.3f}s | {oc:.3f}s | {ind} {ratio:.2f}x |")

        print()

    # Summary
    summary = data["summary"]
    print(f"### Summary\n")
    print(f"**Overall:** {summary['avg_ratio']}x average ratio")
    print(f"(best {summary['best_ratio']}x, worst {summary['worst_ratio']}x)\n")

    print("| Mode | Avg Ratio |")
    print("|------|-----------|")
    for mode, label in MODE_LABELS.items():
        if mode in summary.get("by_mode", {}):
            r = summary["by_mode"][mode]
            print(f"| {label} | {r:.2f}x |")

    print(f"\n_Ratio < 1.0 = oc-rsync faster, > 1.0 = upstream faster_")


if __name__ == "__main__":
    main()
