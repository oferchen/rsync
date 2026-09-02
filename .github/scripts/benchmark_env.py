#!/usr/bin/env python3
"""Capture the machine a benchmark ran on, and probe io_uring SEND_ZC.

A benchmark whose environment is unstated cannot be compared across runs: a
number from a 2-vCPU cloud runner and a number from a bare-metal host are not
the same measurement, and neither is a number from a kernel that cannot run
the zero-copy send path.

`IORING_OP_SEND_ZC` (Linux 6.0+) is the specific capability the release
benchmark has to state, because oc-rsync's io_uring send path can only be
exercised on a kernel that advertises it. Support is *probed*, not inferred:
distros backport features and container runtimes misreport kernel versions,
so a `uname -r` comparison alone would be a claim rather than a measurement.
The probe mirrors `fast_io::io_uring::send_zc::probe_send_zc` -- the mainline
version floor AND the `IORING_REGISTER_PROBE` opcode bit, both required --
so this module and the Rust dispatch cannot disagree about the same kernel.

Pure stdlib (ctypes), so it runs wherever benchmark.py runs.
"""

from __future__ import annotations

import ctypes
import os
import platform
import sys

# include/uapi/linux/io_uring.h. Cross-checked against the io-uring crate's
# generated sys bindings, which is what fast_io's probe resolves
# `opcode::SendZc::CODE` to.
IORING_OP_SEND_ZC = 47
IORING_REGISTER_PROBE = 8
IO_URING_OP_SUPPORTED = 1 << 0

# io_uring_setup(2) / io_uring_register(2). These landed after the kernel
# unified syscall numbering, so the numbers are the same on every Linux
# architecture that has io_uring at all.
SYS_IO_URING_SETUP = 425
SYS_IO_URING_REGISTER = 427

# sizeof(struct io_uring_params): 7 x __u32 + __u32 resv[3] + two 40-byte
# offset structs. Only its size matters here -- it is passed zeroed.
IO_URING_PARAMS_SIZE = 120

# struct io_uring_probe: 16-byte header (last_op, ops_len, resv, resv2[3])
# followed by a flexible array of 8-byte struct io_uring_probe_op.
PROBE_HEADER_SIZE = 16
PROBE_OP_SIZE = 8
PROBE_NR_OPS = 256

# Mainline release that shipped IORING_OP_SEND_ZC. Some 5.x vendor kernels
# advertise the opcode bit but ship incomplete zero-copy semantics, so the
# floor is required in addition to the opcode bit -- the same pairing
# fast_io applies before it will ever submit a SEND_ZC SQE.
SEND_ZC_KERNEL_MIN = (6, 0)


def parse_kernel_version(release: str) -> tuple[int, int] | None:
    """Extract `(major, minor)` from a `uname -r` release string."""
    parts = release.split("-", 1)[0].split(".")
    try:
        return int(parts[0]), int(parts[1])
    except (IndexError, ValueError):
        return None


def _probe_send_zc_opcode() -> tuple[bool | None, str]:
    """Ask the running kernel whether it advertises `IORING_OP_SEND_ZC`.

    Returns `(advertised, detail)`. `advertised` is None when the question
    could not be asked at all -- no io_uring, or the ring was refused -- which
    is a different answer from "asked, and the kernel said no".
    """
    try:
        libc = ctypes.CDLL(None, use_errno=True)
    except OSError as exc:  # pragma: no cover - no libc is not a real host
        return None, f"libc unavailable: {exc}"

    libc.syscall.restype = ctypes.c_long

    params = (ctypes.c_ubyte * IO_URING_PARAMS_SIZE)()
    libc.syscall.argtypes = [ctypes.c_long, ctypes.c_uint, ctypes.c_void_p]
    ctypes.set_errno(0)
    fd = libc.syscall(SYS_IO_URING_SETUP, 4, ctypes.byref(params))
    if fd < 0:
        err = ctypes.get_errno()
        return None, f"io_uring_setup failed: {os.strerror(err)} (errno {err})"

    try:
        size = PROBE_HEADER_SIZE + PROBE_OP_SIZE * PROBE_NR_OPS
        buf = (ctypes.c_ubyte * size)()
        libc.syscall.argtypes = [
            ctypes.c_long,
            ctypes.c_int,
            ctypes.c_uint,
            ctypes.c_void_p,
            ctypes.c_uint,
        ]
        ctypes.set_errno(0)
        rc = libc.syscall(
            SYS_IO_URING_REGISTER,
            fd,
            IORING_REGISTER_PROBE,
            ctypes.byref(buf),
            PROBE_NR_OPS,
        )
        if rc < 0:
            err = ctypes.get_errno()
            return None, (
                f"IORING_REGISTER_PROBE failed: {os.strerror(err)} (errno {err})"
            )

        last_op = buf[0]
        ops_len = buf[1]
        for i in range(min(ops_len, PROBE_NR_OPS)):
            base = PROBE_HEADER_SIZE + i * PROBE_OP_SIZE
            if buf[base] != IORING_OP_SEND_ZC:
                continue
            flags = buf[base + 2] | (buf[base + 3] << 8)
            supported = bool(flags & IO_URING_OP_SUPPORTED)
            return supported, (
                f"IORING_REGISTER_PROBE: op {IORING_OP_SEND_ZC} "
                f"{'advertised' if supported else 'not advertised'} "
                f"(last_op={last_op}, ops_len={ops_len})"
            )
        return False, (
            f"IORING_REGISTER_PROBE: op {IORING_OP_SEND_ZC} absent from the "
            f"probe table (last_op={last_op}, ops_len={ops_len})"
        )
    finally:
        os.close(fd)


def probe_send_zc() -> dict:
    """Full SEND_ZC availability answer for the running kernel.

    Reports the version floor and the opcode bit as separate facts, then the
    conjunction, so a report can distinguish "kernel too old" from "io_uring
    is blocked here" from "the opcode is simply not offered".
    """
    release = platform.release()
    if not sys.platform.startswith("linux"):
        return {
            "supported": False,
            "opcode": IORING_OP_SEND_ZC,
            "kernel_release": release,
            "kernel_floor": "%d.%d" % SEND_ZC_KERNEL_MIN,
            "meets_kernel_floor": False,
            "opcode_advertised": None,
            "detail": f"not Linux (sys.platform={sys.platform})",
        }

    parsed = parse_kernel_version(release)
    meets_floor = parsed is not None and parsed >= SEND_ZC_KERNEL_MIN
    advertised, detail = _probe_send_zc_opcode()
    return {
        "supported": bool(meets_floor and advertised),
        "opcode": IORING_OP_SEND_ZC,
        "kernel_release": release,
        "kernel_floor": "%d.%d" % SEND_ZC_KERNEL_MIN,
        "meets_kernel_floor": meets_floor,
        "opcode_advertised": advertised,
        "detail": detail,
    }


def send_zc_verdict(env: dict) -> str:
    """One sentence stating whether these numbers can show SEND_ZC at all.

    Deliberately blunt in the negative case: a benchmark published from a
    kernel that cannot run the zero-copy send path must not be read as
    evidence about that path.
    """
    zc = env.get("io_uring_send_zc", {})
    dispatch = env.get("oc_rsync_send_zc_dispatch", "")
    if not zc.get("supported"):
        return (
            "**SEND_ZC UNAVAILABLE** - the kernel these numbers were measured "
            f"on ({zc.get('kernel_release', 'unknown')}) does not offer "
            f"IORING_OP_SEND_ZC ({zc.get('detail', 'no probe detail')}). "
            "No figure below is evidence about the io_uring zero-copy send "
            "path."
        )
    line = (
        f"Kernel {zc.get('kernel_release')} advertises IORING_OP_SEND_ZC "
        f"({zc.get('detail')})."
    )
    if dispatch and dispatch != "enabled":
        line += (
            f" The measured oc-rsync binary does not dispatch it: {dispatch}."
        )
    return line


def capture(oc_rsync_send_zc_dispatch: str = "") -> dict:
    """Environment block published alongside the measurements."""
    return {
        "platform": sys.platform,
        "kernel_release": platform.release(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
        "io_uring_send_zc": probe_send_zc(),
        # Declared by the build step that produced the benchmarked binary:
        # `iouring-send-zc` is not in any default feature set, and the
        # binary's --version output does not enumerate fast_io features, so
        # the workflow that ran cargo is the only place this is knowable.
        "oc_rsync_send_zc_dispatch": oc_rsync_send_zc_dispatch
        or "not declared by the build",
    }


if __name__ == "__main__":
    import json

    env = capture(os.environ.get("OC_RSYNC_SEND_ZC_DISPATCH", ""))
    print(json.dumps(env, indent=2))
    print(send_zc_verdict(env), file=sys.stderr)
