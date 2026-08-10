#!/usr/bin/env python3
"""Benchmark: upstream rsync vs oc-rsync (current build).

Tests 2 binaries x 5 transfer modes x 4 copy modes x 3 scenarios = 120 data points.
Runs inside the benchmark container with SSH loopback and rsync daemon configured.

Usage: python3 run_full_benchmark.py [--runs N] [--json]
"""

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

BINARIES = {
    "upstream": "/usr/local/bin/upstream-rsync",
    "oc-rsync": "/usr/local/bin/oc-rsync-release",
}

TRANSFER_MODES = {
    "local": {
        "template": "{bin} {flags} {src}/ {dst}/",
        "label": "Local Copy",
    },
    "ssh_pull": {
        "template": "{bin} {flags} --timeout=30 -e 'ssh -o StrictHostKeyChecking=no' localhost:{src}/ {dst}/",
        "label": "SSH Pull",
    },
    "ssh_push": {
        "template": "{bin} {flags} --timeout=30 -e 'ssh -o StrictHostKeyChecking=no' {src}/ localhost:{dst}/",
        "label": "SSH Push",
    },
    "daemon_pull": {
        "template": "{bin} {flags} --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
        "label": "Daemon Pull",
    },
    "daemon_push": {
        "template": "{bin} {flags} --timeout=30 {src}/ rsync://localhost:{port}/dest/",
        "label": "Daemon Push",
    },
}

COPY_MODES = {
    "whole_file": {"flags": "-avW", "label": "Whole-file (-W)"},
    "delta": {"flags": "-av", "label": "Delta (default)"},
    "checksum": {"flags": "-avc", "label": "Checksum (-c)"},
    "compressed": {"flags": "-avz", "label": "Compressed (-z)"},
}

SCENARIOS = ["initial", "no_change", "incremental"]


def find_free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def wait_for_port(port, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("localhost", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.25)
    return False


def version_string(binary):
    try:
        out = subprocess.run(
            [binary, "--version"], capture_output=True, text=True, timeout=5
        )
        return out.stdout.splitlines()[0] if out.stdout else "unknown"
    except Exception:
        return "unavailable"


CMD_TIMEOUT = 120


def benchmark(cmd, runs=5):
    """Run command multiple times and return timing stats."""
    times = []
    failures = 0
    for _ in range(runs):
        start = time.perf_counter()
        try:
            result = subprocess.run(
                cmd, shell=True, capture_output=True, timeout=CMD_TIMEOUT
            )
        except subprocess.TimeoutExpired:
            failures += 1
            print(
                f"  WARNING: timeout ({CMD_TIMEOUT}s): {cmd[:120]}",
                file=sys.stderr,
            )
            if failures >= 2:
                return None
            continue
        elapsed = time.perf_counter() - start
        if result.returncode not in (0, 23, 24):
            failures += 1
            stderr = result.stderr.decode(errors="replace").strip()
            print(
                f"  WARNING: exit {result.returncode}: {cmd[:120]}",
                file=sys.stderr,
            )
            if stderr:
                print(f"    {stderr[:200]}", file=sys.stderr)
            if failures >= 2:
                return None
        else:
            times.append(elapsed)
    if not times:
        return None
    return {
        "mean": round(sum(times) / len(times), 4),
        "min": round(min(times), 4),
        "max": round(max(times), 4),
    }


def create_test_data(base):
    """Generate 10,000 files (~150 MB)."""
    os.makedirs(f"{base}/small", exist_ok=True)
    os.makedirs(f"{base}/medium", exist_ok=True)
    os.makedirs(f"{base}/large", exist_ok=True)

    for i in range(9000):
        with open(f"{base}/small/file_{i:05d}.txt", "wb") as f:
            f.write(os.urandom(1024))

    for i in range(800):
        with open(f"{base}/medium/file_{i:04d}.bin", "wb") as f:
            f.write(os.urandom(100 * 1024))

    for i in range(200):
        with open(f"{base}/large/file_{i:04d}.dat", "wb") as f:
            f.write(os.urandom(1024 * 1024))


def total_stats(path):
    total_bytes = 0
    total_files = 0
    for dp, _, fnames in os.walk(path):
        for fn in fnames:
            total_bytes += os.path.getsize(os.path.join(dp, fn))
            total_files += 1
    return total_bytes, total_files


def modify_files(src, fraction=0.1):
    """Modify a fraction of files for incremental tests."""
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


def setup_ssh():
    """Start SSH server and configure loopback."""
    subprocess.run(["ssh-keygen", "-A"], capture_output=True)
    os.makedirs("/run/sshd", exist_ok=True)
    subprocess.Popen(
        ["/usr/sbin/sshd"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(1)

    home = os.path.expanduser("~")
    ssh_dir = f"{home}/.ssh"
    os.makedirs(ssh_dir, mode=0o700, exist_ok=True)

    key_path = f"{ssh_dir}/id_ed25519"
    if not os.path.exists(key_path):
        subprocess.run(
            ["ssh-keygen", "-t", "ed25519", "-N", "", "-f", key_path],
            capture_output=True,
        )

    pub_key = open(f"{key_path}.pub").read().strip()
    auth_keys = f"{ssh_dir}/authorized_keys"
    existing = open(auth_keys).read() if os.path.exists(auth_keys) else ""
    if pub_key not in existing:
        with open(auth_keys, "a") as f:
            f.write(pub_key + "\n")
    os.chmod(auth_keys, 0o600)

    result = subprocess.run(
        ["ssh", "-o", "StrictHostKeyChecking=no", "localhost", "echo", "ok"],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if result.returncode != 0:
        print(f"WARNING: SSH loopback failed: {result.stderr}", file=sys.stderr)
        return False
    return True


def main():
    parser = argparse.ArgumentParser(description="oc-rsync benchmark vs upstream")
    parser.add_argument("--runs", type=int, default=5, help="Runs per test")
    parser.add_argument("--json", action="store_true", help="Output JSON only")
    args = parser.parse_args()

    runs = args.runs
    tmpdir = tempfile.mkdtemp(prefix="rsync_bench_")
    daemon_proc = None
    results = {"tests": [], "summary": {}}

    try:
        versions = {}
        available = {}
        for name, binary in BINARIES.items():
            ver = version_string(binary)
            versions[name] = ver
            available[name] = os.path.isfile(binary)

        results["versions"] = versions

        if not args.json:
            print("=" * 80)
            print("  oc-rsync Benchmark — upstream rsync vs oc-rsync")
            print("=" * 80)
            for name, ver in versions.items():
                status = "OK" if available[name] else "MISSING"
                print(f"  {name:>10} : {ver} [{status}]")
            print(f"  {'runs':>10} : {runs}")
            print()

        active_binaries = {k: v for k, v in BINARIES.items() if available[k]}
        if "upstream" not in active_binaries:
            print("ERROR: upstream rsync not found", file=sys.stderr)
            sys.exit(1)
        if "oc-rsync" not in active_binaries:
            print("ERROR: oc-rsync not found", file=sys.stderr)
            sys.exit(1)

        src = f"{tmpdir}/src"
        if not args.json:
            print("Creating test data (10,000 files, ~150 MB)...", file=sys.stderr)
        create_test_data(src)
        total_bytes, total_files = total_stats(src)
        total_mb = total_bytes / 1024 / 1024
        results["test_data"] = {
            "size_mb": round(total_mb, 1),
            "files": total_files,
        }
        if not args.json:
            print(f"Test data: {total_mb:.1f} MB, {total_files} files\n")

        if not args.json:
            print("Setting up SSH loopback...", file=sys.stderr)
        ssh_ok = setup_ssh()

        port = find_free_port()
        daemon_dst = f"{tmpdir}/daemon_dest"
        os.makedirs(daemon_dst, exist_ok=True)
        conf_path = f"{tmpdir}/rsyncd.conf"
        with open(conf_path, "w") as f:
            f.write(
                f"port = {port}\n"
                f"use chroot = false\n"
                f"\n"
                f"[bench]\n"
                f"    path = {src}\n"
                f"    read only = true\n"
                f"\n"
                f"[dest]\n"
                f"    path = {daemon_dst}\n"
                f"    read only = false\n"
            )

        if not args.json:
            print(f"Starting rsync daemon on port {port}...", file=sys.stderr)
        daemon_proc = subprocess.Popen(
            [BINARIES["upstream"], "--daemon", "--config", conf_path, "--no-detach"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        daemon_ok = wait_for_port(port)
        if not daemon_ok:
            print("WARNING: rsync daemon failed to start", file=sys.stderr)

        dsts = {}
        for name in active_binaries:
            dsts[name] = f"{tmpdir}/dst_{name}"

        def reset_dsts():
            for dst in dsts.values():
                shutil.rmtree(dst, ignore_errors=True)
                os.makedirs(dst, exist_ok=True)

        def reset_daemon_dst():
            shutil.rmtree(daemon_dst, ignore_errors=True)
            os.makedirs(daemon_dst, exist_ok=True)

        if not args.json:
            hdr = f"{'Mode':<14} {'Copy':<12} {'Scenario':<12}"
            for name in active_binaries:
                hdr += f" {name:>10}"
            hdr += "  ratio"
            print(hdr)
            print("-" * len(hdr))

        modified_for_incremental = False

        for mode_id, mode_cfg in TRANSFER_MODES.items():
            if mode_id.startswith("ssh_") and not ssh_ok:
                if not args.json:
                    print(f"SKIP {mode_id}: SSH not available", file=sys.stderr)
                continue
            if mode_id.startswith("daemon_") and not daemon_ok:
                if not args.json:
                    print(f"SKIP {mode_id}: daemon not available", file=sys.stderr)
                continue

            for copy_id, copy_cfg in COPY_MODES.items():
                for scenario in SCENARIOS:
                    if not args.json:
                        print(
                            f"Running: [{mode_id}] [{copy_id}] [{scenario}]...",
                            file=sys.stderr,
                        )

                    if scenario == "initial":
                        reset_dsts()
                        if mode_id == "daemon_push":
                            reset_daemon_dst()

                    if scenario == "incremental" and not modified_for_incremental:
                        modify_files(src, 0.1)
                        modified_for_incremental = True

                    timings = {}
                    skip = False
                    for name, binary in active_binaries.items():
                        tpl = mode_cfg["template"]
                        cmd = tpl.format(
                            bin=binary,
                            flags=copy_cfg["flags"],
                            src=src,
                            dst=dsts[name],
                            port=port,
                        )

                        if mode_id == "daemon_push" and scenario == "initial":
                            reset_daemon_dst()

                        result = benchmark(cmd, runs=runs)
                        if result is None:
                            if not args.json:
                                print(
                                    f"  SKIP {mode_id}/{copy_id}/{scenario}: {name} failed",
                                    file=sys.stderr,
                                )
                            skip = True
                            break
                        timings[name] = result

                    if skip:
                        continue

                    up_mean = timings["upstream"]["mean"]
                    oc_mean = timings["oc-rsync"]["mean"]
                    ratio = round(oc_mean / up_mean, 3) if up_mean > 0 else 0

                    entry = {
                        "mode": mode_id,
                        "copy_mode": copy_id,
                        "scenario": scenario,
                        "timings": timings,
                        "ratio": ratio,
                    }
                    results["tests"].append(entry)

                    if not args.json:
                        row = f"{mode_id:<14} {copy_id:<12} {scenario:<12}"
                        for name in active_binaries:
                            row += f" {timings[name]['mean']:>9.3f}s"
                        tag = "faster" if ratio < 0.85 else ("~same" if ratio <= 1.15 else "slower")
                        row += f"  {ratio:.2f}x ({tag})"
                        print(row)

        # Summary
        all_ratios = [t["ratio"] for t in results["tests"]]
        by_mode = {}
        by_copy = {}
        by_scenario = {}
        for t in results["tests"]:
            by_mode.setdefault(t["mode"], []).append(t["ratio"])
            by_copy.setdefault(t["copy_mode"], []).append(t["ratio"])
            by_scenario.setdefault(t["scenario"], []).append(t["ratio"])

        def avg(lst):
            return round(sum(lst) / len(lst), 3) if lst else 0

        if not all_ratios:
            summary = {"overall": {"avg": 0, "best": 0, "worst": 0}}
            results["summary"] = summary
            if not args.json:
                print("\nNo successful benchmarks to summarize.")
            if args.json:
                print(json.dumps(results, indent=2))
            return

        summary = {
            "overall": {"avg": avg(all_ratios), "best": round(min(all_ratios), 3), "worst": round(max(all_ratios), 3)},
            "by_mode": {m: avg(v) for m, v in by_mode.items()},
            "by_copy_mode": {m: avg(v) for m, v in by_copy.items()},
            "by_scenario": {m: avg(v) for m, v in by_scenario.items()},
        }
        results["summary"] = summary

        if not args.json:
            print()
            print("=" * 80)
            print("  Summary (ratio < 1.0 = oc-rsync faster than upstream)")
            print("=" * 80)
            o = summary["overall"]
            print(f"\n  Overall: avg {o['avg']:.2f}x  (best {o['best']:.2f}x, worst {o['worst']:.2f}x)")
            print(f"  By mode:     {', '.join(f'{m}={v:.2f}x' for m, v in summary['by_mode'].items())}")
            print(f"  By copy:     {', '.join(f'{m}={v:.2f}x' for m, v in summary['by_copy_mode'].items())}")
            print(f"  By scenario: {', '.join(f'{m}={v:.2f}x' for m, v in summary['by_scenario'].items())}")

        if args.json:
            print(json.dumps(results, indent=2))

    finally:
        if daemon_proc is not None:
            daemon_proc.terminate()
            daemon_proc.wait(timeout=5)
        shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
