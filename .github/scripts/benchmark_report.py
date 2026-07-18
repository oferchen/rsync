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
    "ssh_transport": (
        "SSH Transport: upstream vs oc-rsync subprocess vs oc-rsync russh"
    ),
}

EXTRA_MODES = {
    "compression": "Compression",
    "delta": "Delta Transfer",
    "large_file": "Large File (1GB)",
    "many_small": "Many Small Files (100K)",
    "sparse": "Sparse Files",
}

MEMORY_MODE = "memory"

# Modes that pit oc-rsync directly against upstream rsync. The OpenSSL,
# io_uring, and SSH-transport modes compare oc-rsync build variants against
# each other, so they are excluded from the upstream-comparison highlights.
UPSTREAM_COMPARISON_MODES = {**MODE_LABELS, **EXTRA_MODES}

ALL_LABELS = {
    **MODE_LABELS,
    **OPENSSL_MODES,
    **IO_URING_MODES,
    **SSH_TRANSPORT_MODES,
    **EXTRA_MODES,
    MEMORY_MODE: "Memory Usage",
}


def ratio_indicator(ratio):
    if ratio < 0.95:
        return "faster"
    elif ratio <= 1.05:
        return "~same"
    else:
        return "slower"


def speedup_phrase(ratio):
    """Phrase a timing ratio as an oc-rsync-relative speedup.

    Ratio < 1.0 means oc-rsync finished sooner, so report it as an
    `Nx faster` gain; ratios within noise read as parity.
    """
    if ratio < 0.95:
        return f"{1.0 / ratio:.2f}x faster"
    elif ratio <= 1.05:
        return "at parity"
    else:
        return f"{ratio:.2f}x slower"


def highlight_lines(summary):
    """Lead with oc-rsync's widest wins over upstream across transfer modes."""
    by_mode = summary.get("by_mode", {})
    ranked = sorted(
        (
            (mode, ratio)
            for mode, ratio in by_mode.items()
            if mode in UPSTREAM_COMPARISON_MODES
        ),
        key=lambda kv: kv[1],
    )
    lines = ["### Highlights\n"]
    avg = summary.get("avg_ratio")
    if avg is not None:
        lines.append(
            f"- **Overall:** oc-rsync is {speedup_phrase(avg)} than upstream "
            f"on average across transfer modes."
        )
    wins = [(m, r) for m, r in ranked if r < 0.95][:3]
    for mode, ratio in wins:
        lines.append(
            f"- **{UPSTREAM_COMPARISON_MODES[mode]}:** {speedup_phrase(ratio)}."
        )
    if not wins:
        lines.append("- oc-rsync holds parity with upstream across measured modes.")
    lines.append("")
    return lines


def main():
    with open("benchmark_results.json") as f:
        data = json.load(f)

    upstream_version = data.get("upstream_version") or "3.4.4"

    print("## Benchmark Results\n")
    print(
        f"oc-rsync vs upstream rsync {upstream_version} on "
        f"{data['test_data']['size_mb']}MB "
        f"({data['test_data']['files']} files).\n"
    )

    for line in highlight_lines(data["summary"]):
        print(line)

    # Group tests by mode
    by_mode = {}
    for t in data["tests"]:
        by_mode.setdefault(t["mode"], []).append(t)

    for mode, label in MODE_LABELS.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        print(f"### {label}\n")
        print(f"| Test | rsync {upstream_version} | oc-rsync | Ratio |")
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

    # SSH transport: 3-way (upstream vs oc-rsync subprocess vs oc-rsync russh)
    # when the new fields are present; otherwise fall back to the legacy
    # subprocess-vs-russh 2-bar render.
    for mode, label in SSH_TRANSPORT_MODES.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        three_way = all("upstream_ssh" in t for t in tests)

        print(f"### {label}\n")
        if three_way:
            print(
                "| Test "
                "| Upstream (ssh) "
                "| oc-rsync (ssh) "
                "| oc-rsync (russh) "
                "| oc-sub / upstream "
                "| russh / oc-sub |"
            )
            print(
                "|------"
                "|----------------"
                "|----------------"
                "|------------------"
                "|-------------------"
                "|----------------|"
            )
            for t in tests:
                up = t["upstream_ssh"]["mean"]
                sub = t["oc_subprocess"]["mean"]
                russh = t["oc_russh"]["mean"]
                r_sub = t.get("ratio_sub_vs_upstream", 0.0)
                r_russh = t.get("ratio_russh_vs_sub", 0.0)
                ind_sub = ratio_indicator(r_sub)
                ind_russh = ratio_indicator(r_russh)
                print(
                    f"| {t['name']} "
                    f"| {up:.3f}s "
                    f"| {sub:.3f}s "
                    f"| {russh:.3f}s "
                    f"| {ind_sub} {r_sub:.2f}x "
                    f"| {ind_russh} {r_russh:.2f}x |"
                )
        else:
            print("| Test | Subprocess (ssh) | Embedded (russh) | Ratio |")
            print("|------|------------------|------------------|-------|")
            for t in tests:
                sub = t["upstream"]["mean"]
                russh = t["oc_rsync"]["mean"]
                ratio = t["ratio"]
                ind = ratio_indicator(ratio)
                print(
                    f"| {t['name']} | {sub:.3f}s | {russh:.3f}s "
                    f"| {ind} {ratio:.2f}x |"
                )

        print()

    # Extra benchmark modes (compression, delta, large file, many small, sparse)
    for mode, label in EXTRA_MODES.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        print(f"### {label}\n")
        print(f"| Test | rsync {upstream_version} | oc-rsync | Ratio |")
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
        print(
            f"| Test | rsync {upstream_version} | oc-rsync | Time Ratio "
            f"| RSS rsync {upstream_version} | RSS oc-rsync |"
        )
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
    for mode, label in ALL_LABELS.items():
        if mode in summary.get("by_mode", {}):
            r = summary["by_mode"][mode]
            print(f"| {label} | {r:.2f}x |")

    print(f"\n_Ratio < 1.0 = oc-rsync faster, > 1.0 = upstream faster_")


if __name__ == "__main__":
    main()
