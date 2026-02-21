#!/usr/bin/env python3
"""Benchmark: upstream rsync 3.4.1 vs oc-rsync v0.5.8 vs oc-rsync HEAD.

Tests 3 binaries x 5 transfer modes x 4 copy modes x 3 scenarios = 180 data points.
Runs inside the Arch Linux benchmark container with SSH loopback and rsync
daemon configured.

Usage: python3 run_arch_benchmark.py [--runs N] [--json]
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
    "v0.5.8": "/usr/local/bin/oc-rsync-v058",
    "dev": "/usr/local/bin/oc-rsync-dev",
}

TRANSFER_MODES = {
    "local": {
        "template": "{bin} {flags} {src}/ {dst}/",
        "label": "Local Copy",
    },
    "ssh_pull": {
        "template": (
            "{bin} {flags} --timeout=60"
            " -e 'ssh -o StrictHostKeyChecking=no'"
            " localhost:{src}/ {dst}/"
        ),
        "label": "SSH Pull",
    },
    "ssh_push": {
        "template": (
            "{bin} {flags} --timeout=60"
            " -e 'ssh -o StrictHostKeyChecking=no'"
            " {src}/ localhost:{dst}/"
        ),
        "label": "SSH Push",
    },
    "daemon_pull": {
        "template": (
            "{bin} {flags} --timeout=60"
            " rsync://localhost:{port}/bench/ {dst}/"
        ),
        "label": "Daemon Pull",
    },
    "daemon_push": {
        "template": (
            "{bin} {flags} --timeout=60"
            " {src}/ rsync://localhost:{port}/dest/"
        ),
        "label": "Daemon Push",
    },
}

COPY_MODES = {
    "delta": {"flags": "-av", "label": "Delta (default)"},
    "whole_file": {"flags": "-avW", "label": "Whole-file (-W)"},
    "checksum": {"flags": "-avc", "label": "Checksum (-c)"},
    "compressed": {"flags": "-avz", "label": "Compressed (-z)"},
}

SCENARIOS = ["initial", "no_change", "incremental"]

CMD_TIMEOUT = 90


def find_free_port():
    """Find an available TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def wait_for_port(port, timeout=10):
    """Wait for a TCP port to accept connections."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("localhost", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.25)
    return False


def version_string(binary):
    """Get version string from a binary."""
    try:
        out = subprocess.run(
            [binary, "--version"], capture_output=True, text=True, timeout=5
        )
        return out.stdout.splitlines()[0] if out.stdout else "unknown"
    except Exception:
        return "unavailable"


def run_timed(cmd, runs=5, warmup=1):
    """Run a command multiple times and return timing stats.

    Warmup runs are excluded from timing. Returns None on first failure
    (timeout or non-zero exit) to avoid wasting time on known-broken combos.
    """
    for _ in range(warmup):
        try:
            result = subprocess.run(
                cmd, shell=True, capture_output=True, timeout=CMD_TIMEOUT
            )
            if result.returncode not in (0, 23, 24):
                return None
        except subprocess.TimeoutExpired:
            return None

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
            if failures >= 1:
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
                print(f"    {stderr[:300]}", file=sys.stderr)
            if failures >= 1:
                return None
        else:
            times.append(elapsed)
    if not times:
        return None
    times.sort()
    median = times[len(times) // 2]
    return {
        "mean": round(sum(times) / len(times), 4),
        "median": round(median, 4),
        "min": round(min(times), 4),
        "max": round(max(times), 4),
        "stddev": round(
            (sum((t - sum(times) / len(times)) ** 2 for t in times) / len(times))
            ** 0.5,
            4,
        ),
        "runs": len(times),
    }


def run_with_stats(binary, flags, src, dst):
    """Run a transfer with --stats and parse throughput."""
    cmd = f"{binary} {flags} --stats {src}/ {dst}/"
    try:
        result = subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=CMD_TIMEOUT
        )
    except subprocess.TimeoutExpired:
        return None
    if result.returncode not in (0, 23, 24):
        return None
    stats = {}
    for line in result.stdout.splitlines() + result.stderr.splitlines():
        line = line.strip()
        if "Total bytes sent:" in line or "total bytes sent:" in line:
            parts = line.split(":")
            if len(parts) >= 2:
                try:
                    stats["bytes_sent"] = int(
                        parts[1].strip().replace(",", "").split()[0]
                    )
                except (ValueError, IndexError):
                    pass
        elif "Total bytes received:" in line or "total bytes received:" in line:
            parts = line.split(":")
            if len(parts) >= 2:
                try:
                    stats["bytes_received"] = int(
                        parts[1].strip().replace(",", "").split()[0]
                    )
                except (ValueError, IndexError):
                    pass
        elif "Total file size:" in line or "total size is" in line:
            parts = line.split(":")
            if len(parts) >= 2:
                try:
                    stats["total_size"] = int(
                        parts[1].strip().replace(",", "").split()[0]
                    )
                except (ValueError, IndexError):
                    pass
    return stats


def create_test_data(base):
    """Generate 10,000 files (~290 MB) with three tiers."""
    os.makedirs(f"{base}/small", exist_ok=True)
    os.makedirs(f"{base}/medium", exist_ok=True)
    os.makedirs(f"{base}/large", exist_ok=True)

    print("  Generating 9,000 small files (1KB each)...", file=sys.stderr)
    for i in range(9000):
        with open(f"{base}/small/file_{i:05d}.txt", "wb") as f:
            f.write(os.urandom(1024))

    print("  Generating 800 medium files (100KB each)...", file=sys.stderr)
    for i in range(800):
        with open(f"{base}/medium/file_{i:04d}.bin", "wb") as f:
            f.write(os.urandom(100 * 1024))

    print("  Generating 200 large files (1MB each)...", file=sys.stderr)
    for i in range(200):
        with open(f"{base}/large/file_{i:04d}.dat", "wb") as f:
            f.write(os.urandom(1024 * 1024))


def total_stats(path):
    """Count total bytes and files under a path."""
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


def start_daemon(binary, conf_path, port):
    """Start an rsync daemon and wait for it to listen."""
    proc = subprocess.Popen(
        [binary, "--daemon", "--config", conf_path, "--no-detach"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if not wait_for_port(port):
        print(
            f"WARNING: daemon failed to start on port {port} ({binary})",
            file=sys.stderr,
        )
        return None
    return proc


def main():
    parser = argparse.ArgumentParser(
        description="oc-rsync benchmark: upstream vs v0.5.8 vs HEAD"
    )
    parser.add_argument("--runs", type=int, default=5, help="Runs per test")
    parser.add_argument("--json", action="store_true", help="Output JSON only")
    args = parser.parse_args()

    runs = args.runs
    tmpdir = tempfile.mkdtemp(prefix="rsync_bench_")
    daemon_procs = []
    results = {"tests": [], "summary": {}}

    try:
        # Check binary availability
        versions = {}
        available = {}
        for name, binary in BINARIES.items():
            ver = version_string(binary)
            versions[name] = ver
            available[name] = os.path.isfile(binary)
        results["versions"] = versions

        if not args.json:
            print("=" * 90)
            print("  oc-rsync Benchmark — upstream rsync 3.4.1 vs v0.5.8 vs HEAD")
            print("=" * 90)
            for name, ver in versions.items():
                status = "OK" if available[name] else "MISSING"
                print(f"  {name:>10} : {ver} [{status}]")
            print(f"  {'runs':>10} : {runs}")
            print()

        active_binaries = {k: v for k, v in BINARIES.items() if available[k]}
        if "upstream" not in active_binaries:
            print("ERROR: upstream rsync not found", file=sys.stderr)
            sys.exit(1)
        if len(active_binaries) < 2:
            print("ERROR: need at least 2 binaries", file=sys.stderr)
            sys.exit(1)

        # Generate test data
        src = f"{tmpdir}/src"
        if not args.json:
            print("Creating test data (10,000 files, ~290 MB)...", file=sys.stderr)
        create_test_data(src)
        total_bytes, total_files = total_stats(src)
        total_mb = total_bytes / 1024 / 1024
        results["test_data"] = {
            "size_mb": round(total_mb, 1),
            "files": total_files,
        }
        if not args.json:
            print(f"Test data: {total_mb:.1f} MB, {total_files} files\n")

        # Setup SSH
        if not args.json:
            print("Setting up SSH loopback...", file=sys.stderr)
        ssh_ok = setup_ssh()

        # Setup rsync daemon (use upstream rsync as daemon for all clients)
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
        daemon_proc = start_daemon(BINARIES["upstream"], conf_path, port)
        daemon_ok = daemon_proc is not None
        if daemon_proc:
            daemon_procs.append(daemon_proc)

        # Destination directories per binary
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

        # Print header
        if not args.json:
            hdr = f"{'Mode':<14} {'Copy':<12} {'Scenario':<12}"
            for name in active_binaries:
                hdr += f" {name:>10}"
            for name in active_binaries:
                if name != "upstream":
                    hdr += f" {name}/up"
            print(hdr)
            print("-" * len(hdr))

        modified_for_incremental = False

        for mode_id, mode_cfg in TRANSFER_MODES.items():
            if mode_id.startswith("ssh_") and not ssh_ok:
                if not args.json:
                    print(
                        f"SKIP {mode_id}: SSH not available", file=sys.stderr
                    )
                continue
            if mode_id.startswith("daemon_") and not daemon_ok:
                if not args.json:
                    print(
                        f"SKIP {mode_id}: daemon not available",
                        file=sys.stderr,
                    )
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
                    elif scenario == "no_change":
                        pass
                    elif scenario == "incremental":
                        if not modified_for_incremental:
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

                        result = run_timed(cmd, runs=runs, warmup=1)
                        if result is None:
                            if not args.json:
                                print(
                                    f"  SKIP {mode_id}/{copy_id}/{scenario}:"
                                    f" {name} failed",
                                    file=sys.stderr,
                                )
                            skip = True
                            break
                        timings[name] = result

                    if skip:
                        continue

                    # Compute ratios vs upstream
                    up_mean = timings["upstream"]["mean"]
                    ratios = {}
                    for name in active_binaries:
                        if name != "upstream" and up_mean > 0:
                            ratios[f"{name}_vs_upstream"] = round(
                                timings[name]["mean"] / up_mean, 3
                            )

                    entry = {
                        "mode": mode_id,
                        "copy_mode": copy_id,
                        "scenario": scenario,
                        "timings": timings,
                        "ratios": ratios,
                    }
                    results["tests"].append(entry)

                    if not args.json:
                        row = f"{mode_id:<14} {copy_id:<12} {scenario:<12}"
                        for name in active_binaries:
                            row += f" {timings[name]['mean']:>9.3f}s"
                        for name in active_binaries:
                            if name != "upstream":
                                r = ratios.get(f"{name}_vs_upstream", 0)
                                row += f" {r:>6.2f}x"
                        print(row)

        # Summary statistics
        all_ratios_by_binary = {}
        by_mode = {}
        by_copy = {}
        by_scenario = {}
        for t in results["tests"]:
            for rname, rval in t["ratios"].items():
                all_ratios_by_binary.setdefault(rname, []).append(rval)
                by_mode.setdefault((rname, t["mode"]), []).append(rval)
                by_copy.setdefault((rname, t["copy_mode"]), []).append(rval)
                by_scenario.setdefault((rname, t["scenario"]), []).append(rval)

        def avg(lst):
            return round(sum(lst) / len(lst), 3) if lst else 0

        summary = {}
        for rname, ratios in all_ratios_by_binary.items():
            summary[rname] = {
                "avg": avg(ratios),
                "best": round(min(ratios), 3) if ratios else 0,
                "worst": round(max(ratios), 3) if ratios else 0,
            }
            summary[f"{rname}_by_mode"] = {}
            for (rn, mode), vals in by_mode.items():
                if rn == rname:
                    summary[f"{rname}_by_mode"][mode] = avg(vals)
            summary[f"{rname}_by_copy_mode"] = {}
            for (rn, copy_mode), vals in by_copy.items():
                if rn == rname:
                    summary[f"{rname}_by_copy_mode"][copy_mode] = avg(vals)
            summary[f"{rname}_by_scenario"] = {}
            for (rn, scenario), vals in by_scenario.items():
                if rn == rname:
                    summary[f"{rname}_by_scenario"][scenario] = avg(vals)

        results["summary"] = summary

        if not args.json:
            print()
            print("=" * 90)
            print(
                "  Summary (ratio < 1.0 = faster than upstream,"
                " > 1.0 = slower)"
            )
            print("=" * 90)
            for rname, s in summary.items():
                if isinstance(s, dict) and "avg" in s:
                    print(
                        f"\n  {rname}: avg {s['avg']:.2f}x"
                        f"  (best {s['best']:.2f}x,"
                        f" worst {s['worst']:.2f}x)"
                    )
            for rname in all_ratios_by_binary:
                mode_key = f"{rname}_by_mode"
                if mode_key in summary:
                    items = ", ".join(
                        f"{m}={v:.2f}x"
                        for m, v in summary[mode_key].items()
                    )
                    print(f"  {rname} by mode: {items}")
                copy_key = f"{rname}_by_copy_mode"
                if copy_key in summary:
                    items = ", ".join(
                        f"{m}={v:.2f}x"
                        for m, v in summary[copy_key].items()
                    )
                    print(f"  {rname} by copy: {items}")
                scenario_key = f"{rname}_by_scenario"
                if scenario_key in summary:
                    items = ", ".join(
                        f"{m}={v:.2f}x"
                        for m, v in summary[scenario_key].items()
                    )
                    print(f"  {rname} by scenario: {items}")

        if args.json:
            print(json.dumps(results, indent=2))

        # Save results to /results if mounted
        if os.path.isdir("/results"):
            ts = time.strftime("%Y%m%d_%H%M%S")
            json_path = f"/results/benchmark_{ts}.json"
            with open(json_path, "w") as f:
                json.dump(results, f, indent=2)
            with open("/results/benchmark.json", "w") as f:
                json.dump(results, f, indent=2)
            if not args.json:
                print(f"\nResults saved to {json_path}")

    finally:
        for proc in daemon_procs:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
