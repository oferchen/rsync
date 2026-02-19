#!/usr/bin/env python3
"""Run benchmark suite comparing oc-rsync against upstream rsync.

Tests local copy, SSH (push + pull), and daemon (push + pull) modes.
Outputs JSON results to stdout.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

UPSTREAM = "target/interop/upstream-src/rsync-3.4.1/rsync"
OC_RSYNC = "target/release/oc-rsync"

TESTS = [
    # Local copy
    {
        "id": "local_initial",
        "name": "Initial sync",
        "mode": "local",
        "args": "-av {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "local_nochange",
        "name": "No-change sync",
        "mode": "local",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "local_checksum",
        "name": "Checksum sync",
        "mode": "local",
        "args": "-avc {src}/ {dst}/",
        "reset": False,
    },
    # SSH pull (local=receiver, remote=sender)
    {
        "id": "ssh_pull_initial",
        "name": "Initial sync",
        "mode": "ssh_pull",
        "args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "ssh_pull_nochange",
        "name": "No-change sync",
        "mode": "ssh_pull",
        "args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "reset": False,
    },
    # SSH push (local=sender, remote=receiver)
    {
        "id": "ssh_push_initial",
        "name": "Initial sync",
        "mode": "ssh_push",
        "args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "reset": True,
    },
    {
        "id": "ssh_push_nochange",
        "name": "No-change sync",
        "mode": "ssh_push",
        "args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "reset": False,
    },
    # Daemon pull
    {
        "id": "daemon_pull_initial",
        "name": "Initial sync",
        "mode": "daemon_pull",
        "args": "-av --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
        "reset": True,
    },
    {
        "id": "daemon_pull_nochange",
        "name": "No-change sync",
        "mode": "daemon_pull",
        "args": "-av --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
        "reset": False,
    },
    # Daemon push
    {
        "id": "daemon_push_initial",
        "name": "Initial sync",
        "mode": "daemon_push",
        "args": "-av --timeout=30 {src}/ rsync://localhost:{port}/dest/",
        "reset": True,
    },
    {
        "id": "daemon_push_nochange",
        "name": "No-change sync",
        "mode": "daemon_push",
        "args": "-av --timeout=30 {src}/ rsync://localhost:{port}/dest/",
        "reset": False,
    },
]


def find_free_port():
    """Find an available TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def wait_for_port(port, timeout=10):
    """Block until a TCP port accepts connections."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("localhost", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.25)
    return False


def benchmark(cmd, runs=5):
    """Run a command multiple times and return timing statistics."""
    times = []
    for _ in range(runs):
        start = time.perf_counter()
        result = subprocess.run(cmd, shell=True, capture_output=True)
        elapsed = time.perf_counter() - start
        if result.returncode != 0:
            print(
                f"WARNING: exit {result.returncode}: {cmd}",
                file=sys.stderr,
            )
            stderr = result.stderr.decode(errors="replace").strip()
            if stderr:
                print(f"  stderr: {stderr[:200]}", file=sys.stderr)
        times.append(elapsed)
    return {
        "mean": sum(times) / len(times),
        "min": min(times),
        "max": max(times),
    }


def main():
    tmpdir = tempfile.mkdtemp(prefix="rsync_bench_")
    daemon_proc = None
    results = {"tests": [], "summary": {}}

    try:
        src = f"{tmpdir}/src"
        dst_up = f"{tmpdir}/dst_upstream"
        dst_oc = f"{tmpdir}/dst_oc"
        daemon_dst = f"{tmpdir}/daemon_dest"

        os.makedirs(f"{src}/small", exist_ok=True)
        os.makedirs(f"{src}/medium", exist_ok=True)
        os.makedirs(f"{src}/large", exist_ok=True)

        # Create test data
        print("Creating test data...", file=sys.stderr)

        # Small files (9500 x 1KB = ~9.5 MB)
        for i in range(9500):
            with open(f"{src}/small/file_{i}.txt", "wb") as f:
                f.write(os.urandom(1024))

        # Medium files (400 x 100KB = ~40 MB)
        for i in range(400):
            with open(f"{src}/medium/file_{i}.bin", "wb") as f:
                f.write(os.urandom(100 * 1024))

        # Large files (100 x 1MB = ~100 MB)
        for i in range(100):
            with open(f"{src}/large/file_{i}.dat", "wb") as f:
                f.write(os.urandom(1024 * 1024))

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

        # Start rsync daemon for daemon benchmarks
        port = find_free_port()
        conf_path = f"{tmpdir}/rsyncd.conf"
        os.makedirs(daemon_dst, exist_ok=True)

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

        print(f"Starting rsync daemon on port {port}...", file=sys.stderr)
        daemon_proc = subprocess.Popen(
            [UPSTREAM, "--daemon", "--config", conf_path, "--no-detach"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if not wait_for_port(port):
            print("ERROR: rsync daemon failed to start", file=sys.stderr)
            sys.exit(1)
        print("Daemon ready.", file=sys.stderr)

        def reset_dst():
            shutil.rmtree(dst_up, ignore_errors=True)
            shutil.rmtree(dst_oc, ignore_errors=True)
            os.makedirs(dst_up, exist_ok=True)
            os.makedirs(dst_oc, exist_ok=True)

        def reset_daemon_dst():
            shutil.rmtree(daemon_dst, ignore_errors=True)
            os.makedirs(daemon_dst, exist_ok=True)

        for test in TESTS:
            test_id = test["id"]
            name = test["name"]
            mode = test["mode"]
            args_tpl = test["args"]
            do_reset = test["reset"]

            print(f"Running: [{mode}] {name}...", file=sys.stderr)

            if do_reset:
                reset_dst()
                if mode == "daemon_push":
                    reset_daemon_dst()

            up_args = args_tpl.format(src=src, dst=dst_up, port=port)
            oc_args = args_tpl.format(src=src, dst=dst_oc, port=port)

            # For daemon push, both tools push to the same daemon dest,
            # so reset between them to get fair initial-transfer timing.
            if mode == "daemon_push" and do_reset:
                reset_daemon_dst()
                up_result = benchmark(f"{UPSTREAM} {up_args}")
                reset_daemon_dst()
                oc_result = benchmark(f"{OC_RSYNC} {oc_args}")
            else:
                up_result = benchmark(f"{UPSTREAM} {up_args}")
                oc_result = benchmark(f"{OC_RSYNC} {oc_args}")

            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )

            results["tests"].append(
                {
                    "id": test_id,
                    "name": name,
                    "mode": mode,
                    "upstream": up_result,
                    "oc_rsync": oc_result,
                    "ratio": round(ratio, 2),
                }
            )

        # Calculate summary
        ratios = [t["ratio"] for t in results["tests"]]
        by_mode = {}
        for t in results["tests"]:
            by_mode.setdefault(t["mode"], []).append(t["ratio"])

        results["summary"] = {
            "avg_ratio": round(sum(ratios) / len(ratios), 2),
            "best_ratio": round(min(ratios), 2),
            "worst_ratio": round(max(ratios), 2),
            "by_mode": {
                m: round(sum(r) / len(r), 2) for m, r in by_mode.items()
            },
        }

        print(json.dumps(results, indent=2))

    finally:
        if daemon_proc is not None:
            daemon_proc.terminate()
            daemon_proc.wait(timeout=5)
        shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
