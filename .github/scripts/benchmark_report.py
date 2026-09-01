#!/usr/bin/env python3
"""Generate a markdown report from benchmark_results.json.

Reads benchmark_results.json from the current directory and writes
a markdown table grouped by transfer mode to stdout.

Every upstream-comparison table carries one column per baseline the run
measured, because the two upstream releases in circulation are two different
answers and publishing only one of them hides the other.
"""

import json
import sys

# Both scripts live in .github/scripts and are invoked as
# `python3 .github/scripts/<name>.py`, which puts that directory on sys.path[0].
# benchmark_chart is pure stdlib, so importing its size formatter here keeps one
# definition of the IEC convention rather than a second copy that can drift.
from benchmark_chart import fmt_bytes
from benchmark_env import send_zc_verdict

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


# ---------------------------------------------------------------------------
# Per-baseline accessors
#
# A results file written before the baseline dimension existed has a single
# `upstream` series and a single `ratio`. Each accessor falls back to that
# shape so an archived run still renders, and so the renderer self-tests can
# state one fact at a time without restating the whole schema.
# ---------------------------------------------------------------------------


def baseline_labels(data):
    """Upstream releases this run measured, in the order it measured them."""
    declared = data.get("baselines")
    if declared:
        return [b["label"] for b in declared]
    return [data.get("upstream_version") or "3.5.0"]


def upstream_series(test, label):
    return (test.get("upstreams") or {}).get(label) or test["upstream"]


def oc_series(test, label):
    return (test.get("oc_rsync_per_baseline") or {}).get(label) or test["oc_rsync"]


def per_baseline_oc(test):
    """True when oc-rsync was re-timed against each baseline as a peer."""
    return bool(test.get("oc_rsync_per_baseline"))


def test_ratio(test, label):
    return (test.get("ratios") or {}).get(label, test.get("ratio", 0.0))


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


def failed_series(test):
    """Series in this row whose binary did not complete, if any."""
    return test.get("failed_series") or []


def fmt_secs(series):
    """Elapsed time with the run-to-run spread that produced it.

    The published figure is a median. Printing it alone lets a cell whose
    fastest and slowest runs differed by half their own magnitude read exactly
    like a cell that repeated to within a percent, so the spread travels with
    the number rather than sitting unread in the JSON.
    """
    mean = series.get("mean", 0.0)
    spread = series.get("spread_pct")
    if spread is None:
        return f"{mean:.3f}s"
    return f"{mean:.3f}s ±{spread:.0f}%"


def fmt_rate(series):
    """Corpus rate in MiB/s, or a dash when the corpus size was not recorded.

    This is corpus bytes over elapsed seconds, not bytes on the wire: it is
    the rate at which the tool got through the tree, which is comparable
    across an initial transfer and a no-change scan of the same corpus.
    """
    rate = series.get("corpus_mibps")
    return f"{rate:.0f}" if rate else "-"


def highlight_lines(data, labels):
    """Lead with oc-rsync's widest wins over upstream, stated per baseline."""
    summary = data.get("summary", {})
    per_baseline = summary.get("by_baseline") or {}
    lines = ["### Highlights\n"]

    for label in labels:
        stats = per_baseline.get(label) or summary
        by_mode = stats.get("by_mode", {})
        avg = stats.get("avg_ratio")
        if avg is not None:
            phrase = speedup_phrase(avg)
            # "at parity" reads "at parity with upstream"; a speedup reads
            # "Nx faster/slower than upstream".
            connector = "with" if phrase == "at parity" else "than"
            lines.append(
                f"- **vs rsync {label}:** oc-rsync is {phrase} {connector} "
                f"upstream on average across transfer modes."
            )
        ranked = sorted(
            (
                (mode, ratio)
                for mode, ratio in by_mode.items()
                if mode in UPSTREAM_COMPARISON_MODES
            ),
            key=lambda kv: kv[1],
        )
        wins = [(m, r) for m, r in ranked if r < 0.95][:3]
        for mode, ratio in wins:
            lines.append(
                f"  - {UPSTREAM_COMPARISON_MODES[mode]}: {speedup_phrase(ratio)}."
            )
        if not wins:
            lines.append(
                f"  - oc-rsync holds parity with rsync {label} across measured "
                f"modes."
            )
    lines.append("")
    return lines


def environment_lines(data):
    """State the machine and the io_uring capability the numbers rest on.

    A benchmark whose environment is unstated cannot be compared across runs,
    and the SEND_ZC line is deliberately loud in the negative: numbers taken
    on a kernel without the opcode are not evidence about the zero-copy send
    path, whatever else they show.
    """
    env = data.get("environment")
    if not env:
        return []
    lines = ["### Environment\n"]
    lines.append(
        f"- Kernel `{env.get('kernel_release', 'unknown')}` on "
        f"`{env.get('machine', 'unknown')}`, "
        f"{env.get('cpu_count', '?')} CPUs."
    )
    lines.append(f"- {send_zc_verdict(env)}")
    lines.append("")
    return lines


def memory_indicator(ratio):
    """Ratio wording for a BYTES metric.

    ratio_indicator() says "faster"/"slower", which is meaningless for peak
    RSS -- memory is not fast. Using it here would repeat the unit conflation
    benchmark_chart.py already had to fix, where a bytes metric was described
    in duration language. Same 5% noise band as the timing indicator so the
    two columns agree on what counts as a real difference.
    """
    if ratio < 0.95:
        return "lower"
    if ratio <= 1.05:
        return "~same"
    return "higher"


def rss_cells(t, primary):
    """Return the three peak-RSS cells for a comparison row.

    benchmark_rss() records `peak_rss_kb` for every mode, not just the memory
    mode, so a throughput comparison can show what the speed cost in memory
    without a second benchmark run. The measurement can still be absent -- a
    run that timed out, or a platform whose /usr/bin/time output this parser
    does not recognise -- so a missing value degrades to a dash rather than
    inventing a number or dropping the row.

    Peak RSS is shown against the primary baseline only. Memory use is a
    property of the binary and its workload, not of the peer it talked to, so
    a column per baseline would repeat one measurement rather than add one.
    """
    up_kb = upstream_series(t, primary).get("peak_rss_kb")
    oc_kb = oc_series(t, primary).get("peak_rss_kb")
    if not up_kb or not oc_kb:
        return "-", "-", "-"
    ratio = oc_kb / up_kb
    return (
        fmt_bytes(up_kb * 1024),
        fmt_bytes(oc_kb * 1024),
        f"{memory_indicator(ratio)} {ratio:.2f}x",
    )


def comparison_table(tests, labels, *, with_rss):
    """Render one upstream-comparison table across every baseline."""
    primary = labels[0]
    split_oc = any(per_baseline_oc(t) for t in tests)

    headers = ["Test"] + [f"rsync {label}" for label in labels]
    if split_oc:
        headers += [f"oc-rsync vs {label}" for label in labels]
    else:
        headers.append("oc-rsync")
    headers += [f"oc / {label}" for label in labels]
    headers.append("oc-rsync MiB/s")
    if with_rss:
        headers += [f"rsync {primary} RSS", "oc-rsync RSS", "Peak RSS"]

    print("| " + " | ".join(headers) + " |")
    print("|" + "|".join("------" for _ in headers) + "|")

    for t in tests:
        broken = failed_series(t)
        name = t["name"]
        if broken:
            # A ratio against a binary that errored out is not a comparison.
            # Say so on the row rather than letting the number stand.
            name = f"{name} **(failed: {', '.join(broken)})**"
        cells = [name]
        cells += [fmt_secs(upstream_series(t, label)) for label in labels]
        if split_oc:
            cells += [fmt_secs(oc_series(t, label)) for label in labels]
        else:
            cells.append(fmt_secs(t["oc_rsync"]))
        for label in labels:
            ratio = test_ratio(t, label)
            cells.append(f"{ratio_indicator(ratio)} {ratio:.2f}x")
        cells.append(fmt_rate(oc_series(t, primary)))
        if with_rss:
            cells += list(rss_cells(t, primary))
        print("| " + " | ".join(cells) + " |")

    print()


def main():
    with open("benchmark_results.json") as f:
        data = json.load(f)

    labels = baseline_labels(data)
    primary = labels[0]
    oc_version = data.get("oc_rsync_version") or "unknown"
    wire_compat = data.get("oc_rsync_wire_compat_version")

    print("## Benchmark Results\n")
    baseline_phrase = " and ".join(f"rsync {label}" for label in labels)
    compat_note = (
        f" (wire-compatible with rsync {wire_compat})" if wire_compat else ""
    )
    print(
        f"oc-rsync {oc_version}{compat_note} vs upstream {baseline_phrase} on "
        f"{data['test_data']['size_mb']}MB "
        f"({data['test_data']['files']} files).\n"
    )

    for line in environment_lines(data):
        print(line)

    for line in highlight_lines(data, labels):
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
        comparison_table(tests, labels, with_rss=True)

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
    # subprocess-vs-russh 2-bar render. This mode compares oc-rsync transports
    # against one another, so it is measured against the primary baseline only.
    for mode, label in SSH_TRANSPORT_MODES.items():
        tests = by_mode.get(mode, [])
        if not tests:
            continue

        three_way = all("upstream_ssh" in t for t in tests)

        print(f"### {label}\n")
        if three_way:
            print(
                "| Test "
                f"| Upstream {primary} (ssh) "
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
        comparison_table(tests, labels, with_rss=False)

    # Memory usage (peak RSS)
    mem_tests = by_mode.get(MEMORY_MODE, [])
    if mem_tests:
        print("### Memory Usage (Peak RSS)\n")
        headers = ["Test"]
        headers += [f"rsync {label}" for label in labels]
        headers += ["oc-rsync", "Time Ratio"]
        headers += [f"RSS rsync {label}" for label in labels]
        headers.append("RSS oc-rsync")
        print("| " + " | ".join(headers) + " |")
        print("|" + "|".join("------" for _ in headers) + "|")

        for t in mem_tests:
            ratio = test_ratio(t, primary)
            cells = [t["name"]]
            cells += [fmt_secs(upstream_series(t, label)) for label in labels]
            cells.append(fmt_secs(t["oc_rsync"]))
            cells.append(f"{ratio_indicator(ratio)} {ratio:.2f}x")
            for label in labels:
                kb = upstream_series(t, label).get("peak_rss_kb")
                cells.append(f"{kb / 1024:.1f}MB" if kb else "N/A")
            oc_kb = t["oc_rsync"].get("peak_rss_kb")
            cells.append(f"{oc_kb / 1024:.1f}MB" if oc_kb else "N/A")
            print("| " + " | ".join(cells) + " |")

        print()

    # Summary
    summary = data["summary"]
    per_baseline = summary.get("by_baseline") or {}
    print("### Summary\n")
    for label in labels:
        stats = per_baseline.get(label) or summary
        print(
            f"**vs rsync {label}:** {stats['avg_ratio']}x average ratio "
            f"(best {stats['best_ratio']}x, worst {stats['worst_ratio']}x)\n"
        )

    header = ["Mode"] + [f"Avg ratio vs {label}" for label in labels]
    print("| " + " | ".join(header) + " |")
    print("|" + "|".join("------" for _ in header) + "|")
    variant_modes = []
    for mode, label in ALL_LABELS.items():
        row_values = []
        for baseline in labels:
            stats = per_baseline.get(baseline) or summary
            row_values.append(stats.get("by_mode", {}).get(mode))
        if all(v is None for v in row_values):
            # A mode with no baseline dimension: it compares oc-rsync build
            # variants against each other. Repeating one number under two
            # baseline headings would claim a comparison that was not made,
            # so those modes get their own table below.
            fallback = summary.get("by_mode", {}).get(mode)
            if fallback is not None:
                variant_modes.append((label, fallback))
            continue
        cells = [label] + [
            f"{v:.2f}x" if v is not None else "-" for v in row_values
        ]
        print("| " + " | ".join(cells) + " |")

    if variant_modes:
        print("\n**oc-rsync build variants** (no upstream baseline):\n")
        print("| Mode | Avg Ratio |")
        print("|------|-----------|")
        for label, value in variant_modes:
            print(f"| {label} | {value:.2f}x |")

    excluded = summary.get("excluded_tests") or []
    if excluded:
        print(
            f"\n> **{len(excluded)} test(s) excluded from these averages** "
            f"because a binary did not complete: "
            f"{', '.join(f'`{e}`' for e in excluded)}. Their rows above are "
            f"marked; a ratio against a command that errored out states a "
            f"verdict the run never measured."
        )

    print("\n_Ratio < 1.0 = oc-rsync faster, > 1.0 = upstream faster._")
    print(
        "_Elapsed figures are the warm-up-discarded median of the timed runs; "
        "`±N%` is the min-to-max spread of that cell. `oc-rsync MiB/s` is "
        "corpus bytes over elapsed seconds, not bytes on the wire._"
    )


if __name__ == "__main__":
    main()
