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

OPENSSL_MODES = {
    "checksum_openssl": "Checksum: OpenSSL vs Pure Rust",
}

IO_URING_MODES = {
    "io_uring": "io_uring vs Standard I/O",
}

SSH_TRANSPORT_MODES = {
    "ssh_transport": "SSH Transport: Subprocess vs russh",
}

EXTRA_MODES = {
    "compression": "Compression",
    "delta": "Delta Transfer",
    "large_file": "Large File (1GB)",
    "many_small": "Many Small Files (100K)",
    "sparse": "Sparse Files",
}

MEMORY_MODE = "memory"


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

    # OpenSSL vs Pure Rust comparison
    for mode, label in OPENSSL_MODES.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        print(f"### {label}\n")
        print("| Test | Pure Rust | OpenSSL | Ratio |")
        print("|------|-----------|---------|-------|")

        for t in tests:
            pure = t["upstream"]["mean"]
            ssl = t["oc_rsync"]["mean"]
            ratio = t["ratio"]
            ind = ratio_indicator(ratio)
            print(f"| {t['name']} | {pure:.3f}s | {ssl:.3f}s | {ind} {ratio:.2f}x |")

        print()

    # io_uring vs standard I/O comparison
    for mode, label in IO_URING_MODES.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        print(f"### {label}\n")
        print("| Test | Standard I/O | io_uring | Ratio |")
        print("|------|-------------|----------|-------|")

        for t in tests:
            std = t["upstream"]["mean"]
            uring = t["oc_rsync"]["mean"]
            ratio = t["ratio"]
            ind = ratio_indicator(ratio)
            print(f"| {t['name']} | {std:.3f}s | {uring:.3f}s | {ind} {ratio:.2f}x |")

        print()

    # SSH transport: subprocess vs embedded russh
    for mode, label in SSH_TRANSPORT_MODES.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        print(f"### {label}\n")
        print("| Test | Subprocess (ssh) | Embedded (russh) | Ratio |")
        print("|------|------------------|------------------|-------|")

        for t in tests:
            sub = t["upstream"]["mean"]
            russh = t["oc_rsync"]["mean"]
            ratio = t["ratio"]
            ind = ratio_indicator(ratio)
            print(f"| {t['name']} | {sub:.3f}s | {russh:.3f}s | {ind} {ratio:.2f}x |")

        print()

    # Extra benchmark modes (compression, delta, large file, many small, sparse)
    for mode, label in EXTRA_MODES.items():
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

    # Memory usage (peak RSS)
    mem_tests = by_mode.get(MEMORY_MODE, [])
    if mem_tests:
        print("### Memory Usage (Peak RSS)\n")
        print("| Test | Upstream | oc-rsync | Time Ratio | RSS Upstream | RSS oc-rsync |")
        print("|------|----------|----------|------------|-------------|-------------|")

        for t in mem_tests:
            up = t["upstream"]["mean"]
            oc = t["oc_rsync"]["mean"]
            ratio = t["ratio"]
            ind = ratio_indicator(ratio)
            up_rss = t["upstream"].get("peak_rss_kb")
            oc_rss = t["oc_rsync"].get("peak_rss_kb")
            up_rss_str = f"{up_rss / 1024:.1f}MB" if up_rss else "N/A"
            oc_rss_str = f"{oc_rss / 1024:.1f}MB" if oc_rss else "N/A"
            print(
                f"| {t['name']} | {up:.3f}s | {oc:.3f}s "
                f"| {ind} {ratio:.2f}x | {up_rss_str} | {oc_rss_str} |"
            )

        print()

    # Summary
    summary = data["summary"]
    print(f"### Summary\n")
    print(f"**Overall:** {summary['avg_ratio']}x average ratio")
    print(f"(best {summary['best_ratio']}x, worst {summary['worst_ratio']}x)\n")

    print("| Mode | Avg Ratio |")
    print("|------|-----------|")
    all_labels = {
        **MODE_LABELS,
        **OPENSSL_MODES,
        **IO_URING_MODES,
        **SSH_TRANSPORT_MODES,
        **EXTRA_MODES,
    }
    all_labels[MEMORY_MODE] = "Memory Usage"
    for mode, label in all_labels.items():
        if mode in summary.get("by_mode", {}):
            r = summary["by_mode"][mode]
            print(f"| {label} | {r:.2f}x |")

    print(f"\n_Ratio < 1.0 = oc-rsync faster, > 1.0 = upstream faster_")


if __name__ == "__main__":
    main()
