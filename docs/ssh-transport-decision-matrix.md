# SSH Transport: russh vs subprocess decision matrix (SSR-3)

This document records the post-v0.6.2 stance on which SSH transport
backs an `oc-rsync` transfer, why, and what new code must not do.
It complements the general transport notes in
[`ssh-transport.md`](ssh-transport.md), the wire-level analysis in
[`audits/ssh-daemon-perf-verification.md`](audits/ssh-daemon-perf-verification.md),
and the 3-way benchmark methodology in
[`benchmarks/ssh-transport-3way.md`](benchmarks/ssh-transport-3way.md).

## 1. Decision matrix

| Concern                       | Embedded russh (`embedded-ssh` feature) | System `ssh` subprocess (default build) |
|-------------------------------|-----------------------------------------|-----------------------------------------|
| Routing trigger               | `ssh://host[:port]/path` URI operand    | `host:path` operand                     |
| Preferred for new deployments | **Yes**                                 | No                                      |
| Build status                  | Opt-in Cargo feature on `rsync_io`      | Always available                        |
| Process model                 | In-process tokio task + russh channel   | `Command::spawn("ssh")` + pipes         |
| Goodbye-phase deadlock risk   | Eliminated (PR #4154)                   | Eliminated (PR #4154)                   |
| Measured cost on the v0.6.2 benchmark | 0.16 s (148.3 MB / 10 000 files) | 0.77 s same workload, ~1.3x upstream    |
| External dependency           | None at runtime                         | Requires `ssh` on `PATH` of every host  |
| `~/.ssh/config` parsing       | In-process via `embedded::ssh_config`   | Delegated to the system `ssh` binary    |
| Stderr capture                | russh channel, no pipe deadlock surface | Pipe or socketpair (see SSE-1..SSE-8)   |
| `ControlMaster` multiplexing  | Not used (one connection per transfer)  | Honoured by the system `ssh` binary     |
| When to choose                | High fan-out, container images without `ssh`, hosts needing deterministic cipher selection | Operator already relies on `~/.ssh/config`, `ssh-agent`, `ControlMaster`, or `ProxyCommand` chains that the embedded client does not implement |

The 3-way release benchmark in
[`benchmarks/ssh-transport-3way.md`](benchmarks/ssh-transport-3way.md)
publishes the `oc_russh` vs `oc_subprocess` ratio for every SSH
workload so the gap above can be tracked release over release.

## 2. Why russh is the preferred path

- **No fork/exec per transfer.** The subprocess path pays a process
  spawn, an argv build, and stdio pipe setup before the first byte
  flows. The russh path opens a TCP socket and negotiates SSH inside
  the same address space.
- **No pipe-buffer deadlock surface.** The subprocess path has to
  drain stderr concurrently to avoid the 64 KiB pipe filling and
  back-pressuring the child. That class of bug is the topic of the
  `ssh-socketpair-stderr` work documented in
  [`ssh-transport.md`](ssh-transport.md) and is structurally absent
  in the russh path.
- **Single source of cipher and compression policy.** With russh the
  cipher list is chosen by `embedded::cipher`, which prefers
  AES-GCM where the CPU has AES acceleration. The subprocess path
  inherits whatever the operator's `ssh_config` says, including the
  "operator typed `Compression yes` once and forgot" failure mode
  audited in
  [`audits/ssh-cipher-compression.md`](audits/ssh-cipher-compression.md).
- **Benchmark numbers.** On the v0.6.2 release run
  (`benchmark.yml` run `25964839057`, SHA `c99bbbc6d`), the
  auxiliary "SSH Transport" sub-benchmark shows russh completing the
  10 000-file workload in 0.16 s while the subprocess path takes
  0.77 s. Both are sub-second; the subprocess path is the one that
  still falls outside the project's "on par with upstream" target.

## 3. Why the subprocess fallback still exists

- **Operator compatibility.** Many users have non-trivial
  `~/.ssh/config` stanzas, `ProxyCommand` chains, jump hosts,
  `ControlMaster` sockets, hardware-token authentication, or
  custom `--rsh` wrappers that the embedded client does not yet
  reproduce. Forcing those users onto russh would be a regression in
  feature surface even when it is a win on wall-clock time.
- **Diagnostics parity with upstream rsync.** When debugging a real
  user problem, "run the same `ssh user@host` invocation and see
  whether it works" is a useful step. The subprocess path keeps that
  workflow truthful.
- **No-feature builds.** `embedded-ssh` is not in the default
  workspace feature set (`Cargo.toml` default = zstd, lz4, acl,
  xattr, iconv, parallel, copy_file_range, io_uring, iocp, async).
  Distribution builds that omit the feature must still be able to
  reach `host:path` targets.

The subprocess path is therefore a **compatibility** fallback, not a
**performance** fallback.

## 4. What new code MUST NOT do

These prohibitions apply to any change that touches transport
selection, the SSH builder, or any code path that decides whether a
transfer goes through russh or through a spawned `ssh`:

1. **Do not re-introduce a subprocess-driven SSH path as a
   performance optimisation.** The v0.6.1 regression (see Section 5)
   shipped exactly that argument and resulted in a 200x slowdown.
   New optimisations that change SSH framing, buffering, or
   back-pressure go into the russh path first and only then into the
   subprocess path.
2. **Do not silently downgrade russh to subprocess.** If russh is
   compiled in and the operand routes to it, the transfer either
   succeeds through russh or fails with a russh error. Falling back
   to the subprocess path on transient russh errors hides regressions
   like #4154 from the benchmark suite and from operators.
3. **Do not assume the subprocess path drains stderr for you.** Any
   code path that spawns `ssh` directly or via the builder must keep
   the stderr drain alive for the full duration of the transfer.
   See SSE-1..SSE-8 (issues #2370-#2377) and
   [`ssh-transport.md`](ssh-transport.md) for the pipe-vs-socketpair
   reasoning; the regression in #4154 sat at this boundary.
4. **Do not bypass `~/.ssh/config` in the russh path.** PR #4154
   wired `embedded::ssh_config` into the russh connection step
   precisely because operators expect the file to apply. New russh
   code must read the resolved `SshConfig` before composing
   connection parameters; see
   [`design/ssh-config-parser-evaluation.md`](design/ssh-config-parser-evaluation.md).

## 5. Historical anchor: the v0.6.1 SSH push regression

| Field                | Value                                                                      |
|----------------------|----------------------------------------------------------------------------|
| Release that shipped the regression | `v0.6.1` (2026-05-03)                                       |
| Symptom              | SSH push hung until the harness 120 s wall-clock timeout fired             |
| Magnitude            | ~200x slower than upstream rsync (120 s vs 0.6 s on the 148.3 MB workload) |
| Root cause           | Goodbye-phase deadlock in the subprocess SSH wrapper; the parent stopped reading the child's stderr while the child waited to flush the final protocol frames, and the child also waited on stderr back-pressure |
| How it was caught    | Release benchmark run `25278560260` appended to the v0.6.1 release notes; every SSH push and daemon push row hit the harness timeout |
| Fix                  | PR #4154 (`fix(ssh): resolve goodbye-phase deadlock + load ~/.ssh/config in russh path`), commit `c99bbbc6d`. Drains the stderr socketpair on the subprocess side until the child exits, and loads `~/.ssh/config` in the russh path so operator-configured options actually apply |
| Verification         | Post-fix benchmark run `25964839057` (v0.6.2), audited in [`audits/ssh-daemon-perf-verification.md`](audits/ssh-daemon-perf-verification.md). SSH push initial 0.769 s vs upstream 0.596 s; no harness timeouts on any mode |
| Residual gap         | Subprocess SSH push is still ~1.29x to ~1.53x slower than upstream; russh path is faster than upstream on the same workload. Closing the subprocess gap is follow-on work, not a re-introduction of any earlier design |

The lesson encoded in this matrix is the one PR #4154 paid for in
production downtime: when an SSH path looks slow because of a
subprocess interaction, the fix is to harden the russh path and to
keep the subprocess path correct, not to invent a new subprocess
pipeline whose perf claims sit ahead of its deadlock analysis.

## 6. References

- General transport notes and socketpair-stderr opt-in:
  [`ssh-transport.md`](ssh-transport.md).
- 3-way benchmark methodology and JSON schema:
  [`benchmarks/ssh-transport-3way.md`](benchmarks/ssh-transport-3way.md).
- Post-fix performance audit and regression check:
  [`audits/ssh-daemon-perf-verification.md`](audits/ssh-daemon-perf-verification.md).
- `ssh_config` "Compression yes" double-compression risk on the
  subprocess path:
  [`audits/ssh-cipher-compression.md`](audits/ssh-cipher-compression.md).
- `ssh_config` parser scope and limitations:
  [`design/ssh-config-parser-evaluation.md`](design/ssh-config-parser-evaluation.md).
