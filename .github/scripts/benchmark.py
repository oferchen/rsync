#!/usr/bin/env python3
"""Run benchmark suite comparing oc-rsync against upstream rsync.

Tests local copy, SSH (push + pull), and daemon (push + pull) modes.
Outputs JSON results to stdout.
"""

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time

UPSTREAM = "target/interop/upstream-src/rsync-3.4.1/rsync"
OC_RSYNC = "target/release/oc-rsync"
OC_RSYNC_OPENSSL = os.environ.get("OC_RSYNC_OPENSSL", "")
OC_RSYNC_RUSSH = os.environ.get("OC_RSYNC_RUSSH", "")
IS_LINUX = sys.platform.startswith("linux")

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

# OpenSSL vs pure-Rust checksum comparison (only run if OC_RSYNC_OPENSSL is set)
OPENSSL_TESTS = [
    {
        "id": "openssl_checksum_initial",
        "name": "Initial checksum sync",
        "mode": "checksum_openssl",
        "args": "-avc {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "openssl_checksum_nochange",
        "name": "No-change checksum sync",
        "mode": "checksum_openssl",
        "args": "-avc {src}/ {dst}/",
        "reset": False,
    },
]

# SSH transport comparison: subprocess `ssh` (host:path operand) vs embedded
# russh (`ssh://` URI operand). Only run if OC_RSYNC_RUSSH is set.
# The default oc-rsync binary handles the subprocess form; the russh-built
# binary handles the URI form via the embedded transport.
RUSSH_TESTS = [
    {
        "id": "ssh_transport_pull_initial",
        "name": "Initial pull",
        "mode": "ssh_transport",
        "subprocess_args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "russh_args": "-av --timeout=30 ssh://localhost{src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "ssh_transport_pull_nochange",
        "name": "No-change pull",
        "mode": "ssh_transport",
        "subprocess_args": "-av --timeout=30 localhost:{src}/ {dst}/",
        "russh_args": "-av --timeout=30 ssh://localhost{src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "ssh_transport_push_initial",
        "name": "Initial push",
        "mode": "ssh_transport",
        "subprocess_args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "russh_args": "-av --timeout=30 {src}/ ssh://localhost{dst}/",
        "reset": True,
    },
    {
        "id": "ssh_transport_push_nochange",
        "name": "No-change push",
        "mode": "ssh_transport",
        "subprocess_args": "-av --timeout=30 {src}/ localhost:{dst}/",
        "russh_args": "-av --timeout=30 {src}/ ssh://localhost{dst}/",
        "reset": False,
    },
]


COMPRESSION_TESTS = [
    {
        "id": "compress_zlib_initial",
        "name": "zlib initial sync",
        "mode": "compression",
        "args": "-avz {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "compress_zlib_nochange",
        "name": "zlib no-change sync",
        "mode": "compression",
        "args": "-avz {src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "compress_zstd_initial",
        "name": "zstd initial sync",
        "mode": "compression",
        "args": "-av --compress-choice=zstd {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "compress_zstd_nochange",
        "name": "zstd no-change sync",
        "mode": "compression",
        "args": "-av --compress-choice=zstd {src}/ {dst}/",
        "reset": False,
    },
]

DELTA_TESTS = [
    {
        "id": "delta_local",
        "name": "Local delta sync",
        "mode": "delta",
        "args": "-av {src}/ {dst}/",
    },
    {
        "id": "delta_checksum",
        "name": "Local delta checksum sync",
        "mode": "delta",
        "args": "-avc {src}/ {dst}/",
    },
]

LARGE_FILE_TESTS = [
    {
        "id": "large_file_initial",
        "name": "1GB file initial sync",
        "mode": "large_file",
        "args": "-av {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "large_file_nochange",
        "name": "1GB file no-change sync",
        "mode": "large_file",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
    {
        "id": "large_file_delta",
        "name": "1GB file delta sync",
        "mode": "large_file",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
]

MANY_SMALL_FILES_TESTS = [
    {
        "id": "many_small_initial",
        "name": "100K files initial sync",
        "mode": "many_small",
        "args": "-av {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "many_small_nochange",
        "name": "100K files no-change sync",
        "mode": "many_small",
        "args": "-av {src}/ {dst}/",
        "reset": False,
    },
]

SPARSE_TESTS = [
    {
        "id": "sparse_initial",
        "name": "Sparse initial sync",
        "mode": "sparse",
        "args": "-avS {src}/ {dst}/",
        "reset": True,
    },
    {
        "id": "sparse_nochange",
        "name": "Sparse no-change sync",
        "mode": "sparse",
        "args": "-avS {src}/ {dst}/",
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


PER_RUN_TIMEOUT = 600  # seconds per individual rsync invocation


def parse_peak_rss_kb(stderr_text):
    """Extract peak RSS in KB from /usr/bin/time output.

    Linux (-v): 'Maximum resident set size (kbytes): 12345'
    macOS (-l): '12345  maximum resident set size' (bytes, convert to KB)
    """
    m = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr_text)
    if m:
        return int(m.group(1))
    m = re.search(r"(\d+)\s+maximum resident set size", stderr_text)
    if m:
        return int(m.group(1)) // 1024
    return None


def benchmark_rss(cmd, runs=3):
    """Run a command with /usr/bin/time and return timing + peak RSS stats."""
    time_flag = "-v" if IS_LINUX else "-l"
    wrapped = f"/usr/bin/time {time_flag} {cmd}"
    times = []
    rss_values = []
    for i in range(runs):
        start = time.perf_counter()
        try:
            result = subprocess.run(
                wrapped, shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
            )
            elapsed = time.perf_counter() - start
            stderr = result.stderr.decode(errors="replace")
            if result.returncode != 0:
                print(f"WARNING: exit {result.returncode}: {cmd}", file=sys.stderr)
                if stderr.strip():
                    print(f"  stderr: {stderr[:200]}", file=sys.stderr)
            rss = parse_peak_rss_kb(stderr)
            if rss is not None:
                rss_values.append(rss)
        except subprocess.TimeoutExpired:
            elapsed = time.perf_counter() - start
            print(
                f"ERROR: timeout after {PER_RUN_TIMEOUT}s (run {i+1}/{runs}): {cmd}",
                file=sys.stderr,
            )
        times.append(elapsed)
    result = {
        "mean": sum(times) / len(times),
        "min": min(times),
        "max": max(times),
    }
    if rss_values:
        result["peak_rss_kb"] = max(rss_values)
        result["avg_rss_kb"] = sum(rss_values) // len(rss_values)
    return result


def benchmark(cmd, runs=5):
    """Run a command multiple times and return timing statistics."""
    times = []
    for i in range(runs):
        start = time.perf_counter()
        try:
            result = subprocess.run(
                cmd, shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
            )
            elapsed = time.perf_counter() - start
            if result.returncode != 0:
                print(
                    f"WARNING: exit {result.returncode}: {cmd}",
                    file=sys.stderr,
                )
                stderr = result.stderr.decode(errors="replace").strip()
                if stderr:
                    print(f"  stderr: {stderr[:200]}", file=sys.stderr)
        except subprocess.TimeoutExpired:
            elapsed = time.perf_counter() - start
            print(
                f"ERROR: timeout after {PER_RUN_TIMEOUT}s (run {i+1}/{runs}): {cmd}",
                file=sys.stderr,
            )
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

        # OpenSSL vs pure-Rust comparison
        if OC_RSYNC_OPENSSL and os.path.isfile(OC_RSYNC_OPENSSL):
            print("Running OpenSSL vs pure-Rust comparison...", file=sys.stderr)
            dst_pure = f"{tmpdir}/dst_pure"
            dst_ssl = f"{tmpdir}/dst_ssl"

            def reset_openssl_dst():
                shutil.rmtree(dst_pure, ignore_errors=True)
                shutil.rmtree(dst_ssl, ignore_errors=True)
                os.makedirs(dst_pure, exist_ok=True)
                os.makedirs(dst_ssl, exist_ok=True)

            for test in OPENSSL_TESTS:
                test_id = test["id"]
                name = test["name"]
                mode = test["mode"]
                args_tpl = test["args"]

                print(f"Running: [{mode}] {name}...", file=sys.stderr)

                if test["reset"]:
                    reset_openssl_dst()

                pure_args = args_tpl.format(src=src, dst=dst_pure, port=port)
                ssl_args = args_tpl.format(src=src, dst=dst_ssl, port=port)

                pure_result = benchmark(f"{OC_RSYNC} {pure_args}")
                ssl_result = benchmark(f"{OC_RSYNC_OPENSSL} {ssl_args}")

                ratio = (
                    ssl_result["mean"] / pure_result["mean"]
                    if pure_result["mean"] > 0
                    else 0
                )

                results["tests"].append(
                    {
                        "id": test_id,
                        "name": name,
                        "mode": mode,
                        "upstream": pure_result,
                        "oc_rsync": ssl_result,
                        "ratio": round(ratio, 2),
                    }
                )
        elif OC_RSYNC_OPENSSL:
            print(
                f"WARNING: OC_RSYNC_OPENSSL={OC_RSYNC_OPENSSL} not found, skipping",
                file=sys.stderr,
            )

        # SSH transport: subprocess vs embedded russh
        if OC_RSYNC_RUSSH and os.path.isfile(OC_RSYNC_RUSSH):
            print("Running SSH transport (subprocess vs russh)...", file=sys.stderr)
            dst_sub_pull = f"{tmpdir}/dst_sub_pull"
            dst_russh_pull = f"{tmpdir}/dst_russh_pull"
            dst_sub_push = f"{tmpdir}/dst_sub_push"
            dst_russh_push = f"{tmpdir}/dst_russh_push"

            def reset_transport_pull():
                shutil.rmtree(dst_sub_pull, ignore_errors=True)
                shutil.rmtree(dst_russh_pull, ignore_errors=True)
                os.makedirs(dst_sub_pull, exist_ok=True)
                os.makedirs(dst_russh_pull, exist_ok=True)

            def reset_transport_push():
                shutil.rmtree(dst_sub_push, ignore_errors=True)
                shutil.rmtree(dst_russh_push, ignore_errors=True)
                os.makedirs(dst_sub_push, exist_ok=True)
                os.makedirs(dst_russh_push, exist_ok=True)

            for test in RUSSH_TESTS:
                test_id = test["id"]
                name = test["name"]
                mode = test["mode"]
                is_push = "push" in test_id

                print(f"Running: [{mode}] {name}...", file=sys.stderr)

                if test["reset"]:
                    if is_push:
                        reset_transport_push()
                    else:
                        reset_transport_pull()

                if is_push:
                    sub_dst, russh_dst = dst_sub_push, dst_russh_push
                else:
                    sub_dst, russh_dst = dst_sub_pull, dst_russh_pull

                sub_args = test["subprocess_args"].format(src=src, dst=sub_dst)
                russh_args = test["russh_args"].format(src=src, dst=russh_dst)

                sub_result = benchmark(f"{OC_RSYNC} {sub_args}")
                russh_result = benchmark(f"{OC_RSYNC_RUSSH} {russh_args}")

                ratio = (
                    russh_result["mean"] / sub_result["mean"]
                    if sub_result["mean"] > 0
                    else 0
                )

                # "upstream" field repurposed as the subprocess baseline so the
                # report/chart can render this as a two-bar comparison without
                # special-casing the data shape.
                results["tests"].append(
                    {
                        "id": test_id,
                        "name": name,
                        "mode": mode,
                        "upstream": sub_result,
                        "oc_rsync": russh_result,
                        "ratio": round(ratio, 2),
                    }
                )
        elif OC_RSYNC_RUSSH:
            print(
                f"WARNING: OC_RSYNC_RUSSH={OC_RSYNC_RUSSH} not found, skipping",
                file=sys.stderr,
            )

        # io_uring vs standard I/O comparison (Linux only)
        if IS_LINUX:
            print("Running io_uring vs standard I/O comparison...", file=sys.stderr)
            dst_uring = f"{tmpdir}/dst_uring"
            dst_no_uring = f"{tmpdir}/dst_no_uring"

            io_uring_tests = [
                {
                    "id": "io_uring_local",
                    "name": "Local initial sync",
                    "mode": "io_uring",
                    "args": "-av {src}/ {dst}/",
                },
                {
                    "id": "io_uring_daemon_pull",
                    "name": "Daemon pull initial",
                    "mode": "io_uring",
                    "args": "-av --timeout=30 rsync://localhost:{port}/bench/ {dst}/",
                },
                {
                    "id": "io_uring_ssh_pull",
                    "name": "SSH pull initial",
                    "mode": "io_uring",
                    "args": "-av --timeout=30 localhost:{src}/ {dst}/",
                },
            ]

            for test in io_uring_tests:
                print(f"Running: [io_uring] {test['name']}...", file=sys.stderr)
                args_tpl = test["args"]

                # Run with --io-uring (enabled)
                shutil.rmtree(dst_uring, ignore_errors=True)
                os.makedirs(dst_uring, exist_ok=True)
                uring_args = args_tpl.format(src=src, dst=dst_uring, port=port)
                uring_result = benchmark(f"{OC_RSYNC} --io-uring {uring_args}")

                # Run with --no-io-uring (disabled)
                shutil.rmtree(dst_no_uring, ignore_errors=True)
                os.makedirs(dst_no_uring, exist_ok=True)
                no_uring_args = args_tpl.format(src=src, dst=dst_no_uring, port=port)
                no_uring_result = benchmark(f"{OC_RSYNC} --no-io-uring {no_uring_args}")

                ratio = (
                    uring_result["mean"] / no_uring_result["mean"]
                    if no_uring_result["mean"] > 0
                    else 0
                )

                results["tests"].append(
                    {
                        "id": test["id"],
                        "name": test["name"],
                        "mode": "io_uring",
                        "upstream": no_uring_result,
                        "oc_rsync": uring_result,
                        "ratio": round(ratio, 2),
                    }
                )
        else:
            print("Skipping io_uring tests (not Linux).", file=sys.stderr)

        # Compression benchmarks (zlib and zstd)
        print("Running compression benchmarks...", file=sys.stderr)
        dst_comp_up = f"{tmpdir}/dst_comp_up"
        dst_comp_oc = f"{tmpdir}/dst_comp_oc"

        def reset_comp_dst():
            shutil.rmtree(dst_comp_up, ignore_errors=True)
            shutil.rmtree(dst_comp_oc, ignore_errors=True)
            os.makedirs(dst_comp_up, exist_ok=True)
            os.makedirs(dst_comp_oc, exist_ok=True)

        for test in COMPRESSION_TESTS:
            print(f"Running: [compression] {test['name']}...", file=sys.stderr)
            if test["reset"]:
                reset_comp_dst()
            up_args = test["args"].format(src=src, dst=dst_comp_up, port=port)
            oc_args = test["args"].format(src=src, dst=dst_comp_oc, port=port)
            up_result = benchmark(f"{UPSTREAM} {up_args}")
            oc_result = benchmark(f"{OC_RSYNC} {oc_args}")
            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )
            results["tests"].append({
                "id": test["id"],
                "name": test["name"],
                "mode": "compression",
                "upstream": up_result,
                "oc_rsync": oc_result,
                "ratio": round(ratio, 2),
            })

        # Delta transfer benchmarks (modify files then re-sync)
        print("Running delta transfer benchmarks...", file=sys.stderr)
        dst_delta_up = f"{tmpdir}/dst_delta_up"
        dst_delta_oc = f"{tmpdir}/dst_delta_oc"

        # Initial sync to populate destinations
        shutil.rmtree(dst_delta_up, ignore_errors=True)
        shutil.rmtree(dst_delta_oc, ignore_errors=True)
        os.makedirs(dst_delta_up, exist_ok=True)
        os.makedirs(dst_delta_oc, exist_ok=True)
        subprocess.run(
            f"{UPSTREAM} -av {src}/ {dst_delta_up}/",
            shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
        )
        subprocess.run(
            f"{OC_RSYNC} -av {src}/ {dst_delta_oc}/",
            shell=True, capture_output=True, timeout=PER_RUN_TIMEOUT,
        )

        # Modify ~10% of medium files (append 4KB to trigger delta)
        for i in range(0, 400, 10):
            path = f"{src}/medium/file_{i}.bin"
            with open(path, "ab") as f:
                f.write(os.urandom(4096))

        for test in DELTA_TESTS:
            print(f"Running: [delta] {test['name']}...", file=sys.stderr)
            up_args = test["args"].format(src=src, dst=dst_delta_up, port=port)
            oc_args = test["args"].format(src=src, dst=dst_delta_oc, port=port)
            up_result = benchmark(f"{UPSTREAM} {up_args}")
            oc_result = benchmark(f"{OC_RSYNC} {oc_args}")
            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )
            results["tests"].append({
                "id": test["id"],
                "name": test["name"],
                "mode": "delta",
                "upstream": up_result,
                "oc_rsync": oc_result,
                "ratio": round(ratio, 2),
            })

        # Restore modified files to original size for subsequent benchmarks
        for i in range(0, 400, 10):
            path = f"{src}/medium/file_{i}.bin"
            with open(path, "r+b") as f:
                f.truncate(100 * 1024)

        # Large single file benchmark (1GB)
        print("Running large file benchmarks...", file=sys.stderr)
        large_src = f"{tmpdir}/large_src"
        dst_large_up = f"{tmpdir}/dst_large_up"
        dst_large_oc = f"{tmpdir}/dst_large_oc"
        os.makedirs(large_src, exist_ok=True)

        large_file_path = f"{large_src}/bigfile.dat"
        with open(large_file_path, "wb") as f:
            # Write 1GB in 1MB chunks
            for _ in range(1024):
                f.write(os.urandom(1024 * 1024))

        for test in LARGE_FILE_TESTS:
            print(f"Running: [large_file] {test['name']}...", file=sys.stderr)
            if test["reset"]:
                shutil.rmtree(dst_large_up, ignore_errors=True)
                shutil.rmtree(dst_large_oc, ignore_errors=True)
                os.makedirs(dst_large_up, exist_ok=True)
                os.makedirs(dst_large_oc, exist_ok=True)

            # For delta test, modify a 64KB region in the middle of the file
            if test["id"] == "large_file_delta":
                with open(large_file_path, "r+b") as f:
                    f.seek(512 * 1024 * 1024)
                    f.write(os.urandom(64 * 1024))

            up_args = test["args"].format(
                src=large_src, dst=dst_large_up, port=port,
            )
            oc_args = test["args"].format(
                src=large_src, dst=dst_large_oc, port=port,
            )
            up_result = benchmark(f"{UPSTREAM} {up_args}", runs=3)
            oc_result = benchmark(f"{OC_RSYNC} {oc_args}", runs=3)
            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )
            results["tests"].append({
                "id": test["id"],
                "name": test["name"],
                "mode": "large_file",
                "upstream": up_result,
                "oc_rsync": oc_result,
                "ratio": round(ratio, 2),
            })

        # Many small files benchmark (100K files)
        print("Running many small files benchmarks...", file=sys.stderr)
        many_src = f"{tmpdir}/many_src"
        dst_many_up = f"{tmpdir}/dst_many_up"
        dst_many_oc = f"{tmpdir}/dst_many_oc"

        # Create 100K files x 100B across 100 directories
        for d in range(100):
            dir_path = f"{many_src}/d{d:03d}"
            os.makedirs(dir_path, exist_ok=True)
            for i in range(1000):
                with open(f"{dir_path}/f{i:04d}.txt", "wb") as f:
                    f.write(os.urandom(100))

        for test in MANY_SMALL_FILES_TESTS:
            print(f"Running: [many_small] {test['name']}...", file=sys.stderr)
            if test["reset"]:
                shutil.rmtree(dst_many_up, ignore_errors=True)
                shutil.rmtree(dst_many_oc, ignore_errors=True)
                os.makedirs(dst_many_up, exist_ok=True)
                os.makedirs(dst_many_oc, exist_ok=True)
            up_args = test["args"].format(
                src=many_src, dst=dst_many_up, port=port,
            )
            oc_args = test["args"].format(
                src=many_src, dst=dst_many_oc, port=port,
            )
            up_result = benchmark(f"{UPSTREAM} {up_args}", runs=3)
            oc_result = benchmark(f"{OC_RSYNC} {oc_args}", runs=3)
            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )
            results["tests"].append({
                "id": test["id"],
                "name": test["name"],
                "mode": "many_small",
                "upstream": up_result,
                "oc_rsync": oc_result,
                "ratio": round(ratio, 2),
            })

        # Memory usage (peak RSS) benchmark
        print("Running memory usage benchmarks...", file=sys.stderr)
        dst_mem_up = f"{tmpdir}/dst_mem_up"
        dst_mem_oc = f"{tmpdir}/dst_mem_oc"

        memory_tests = [
            {
                "id": "memory_initial",
                "name": "Initial sync (10K files)",
                "src": src,
                "args": "-av {src}/ {dst}/",
                "reset": True,
            },
            {
                "id": "memory_large_file",
                "name": "1GB file sync",
                "src": large_src,
                "args": "-av {src}/ {dst}/",
                "reset": True,
            },
            {
                "id": "memory_many_files",
                "name": "100K files sync",
                "src": many_src,
                "args": "-av {src}/ {dst}/",
                "reset": True,
            },
        ]

        for test in memory_tests:
            print(f"Running: [memory] {test['name']}...", file=sys.stderr)
            if test["reset"]:
                shutil.rmtree(dst_mem_up, ignore_errors=True)
                shutil.rmtree(dst_mem_oc, ignore_errors=True)
                os.makedirs(dst_mem_up, exist_ok=True)
                os.makedirs(dst_mem_oc, exist_ok=True)
            up_args = test["args"].format(
                src=test["src"], dst=dst_mem_up, port=port,
            )
            oc_args = test["args"].format(
                src=test["src"], dst=dst_mem_oc, port=port,
            )
            up_result = benchmark_rss(f"{UPSTREAM} {up_args}")
            oc_result = benchmark_rss(f"{OC_RSYNC} {oc_args}")
            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )
            results["tests"].append({
                "id": test["id"],
                "name": test["name"],
                "mode": "memory",
                "upstream": up_result,
                "oc_rsync": oc_result,
                "ratio": round(ratio, 2),
            })

        # Sparse file benchmark
        print("Running sparse file benchmarks...", file=sys.stderr)
        sparse_src = f"{tmpdir}/sparse_src"
        dst_sparse_up = f"{tmpdir}/dst_sparse_up"
        dst_sparse_oc = f"{tmpdir}/dst_sparse_oc"
        os.makedirs(sparse_src, exist_ok=True)

        # Create files with large zero runs (simulating sparse data)
        for i in range(50):
            path = f"{sparse_src}/sparse_{i}.dat"
            with open(path, "wb") as f:
                # 10MB file: 1MB data, 8MB zeros, 1MB data
                f.write(os.urandom(1024 * 1024))
                f.write(b"\0" * (8 * 1024 * 1024))
                f.write(os.urandom(1024 * 1024))

        for test in SPARSE_TESTS:
            print(f"Running: [sparse] {test['name']}...", file=sys.stderr)
            if test["reset"]:
                shutil.rmtree(dst_sparse_up, ignore_errors=True)
                shutil.rmtree(dst_sparse_oc, ignore_errors=True)
                os.makedirs(dst_sparse_up, exist_ok=True)
                os.makedirs(dst_sparse_oc, exist_ok=True)
            up_args = test["args"].format(
                src=sparse_src, dst=dst_sparse_up, port=port,
            )
            oc_args = test["args"].format(
                src=sparse_src, dst=dst_sparse_oc, port=port,
            )
            up_result = benchmark(f"{UPSTREAM} {up_args}", runs=3)
            oc_result = benchmark(f"{OC_RSYNC} {oc_args}", runs=3)
            ratio = (
                oc_result["mean"] / up_result["mean"]
                if up_result["mean"] > 0
                else 0
            )
            results["tests"].append({
                "id": test["id"],
                "name": test["name"],
                "mode": "sparse",
                "upstream": up_result,
                "oc_rsync": oc_result,
                "ratio": round(ratio, 2),
            })

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
