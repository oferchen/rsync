# RP28.f.1 - Client-mode rsync 2.6.9 daemon interop harness

Status: Spec
Task: RP28.f.1 (#2966)
Parent: RP28.f (#2731)
Grandparent: RP28 (#2725)
Follow-up: RP28.f.2 (#2967, CI wiring), RP28.f.3 (outcome documentation)

## 1. Scope

RP28.f.1 specifies the harness for the client-mode protocol-28 interop
test. The topology is the inverse of RP28.e: upstream rsync 2.6.9 runs
as `--daemon --no-detach`, and oc-rsync drives the conversation as the
client, issuing both pull and push transfers against the legacy daemon.

This document defines fixtures, daemon configuration, client invocations,
pass/fail criteria, harness script layout, CI integration plan, and known
fragilities. RP28.f.2 implements the harness scripts and wires the job
into CI; RP28.f.3 documents the observed outcomes once the harness is
exercised against the cached `rsync-2.6.9` binary from RP28.b.1
(`scripts/build_rsync_2_6_9.sh`, PR #4903).

Out of scope: changes to oc-rsync client code, capability-string edits,
or wire-byte regression tests. Those belong to RP28.g / RP28.h / RP28.i
and the existing test files under `crates/protocol/tests/`
(`flist_wire_flags_rp28g.rs`, `flist_sort_keys_rp28h.rs`).

## 2. Why client-mode matters

Modern oc-rsync clients in real deployments need to interoperate with
legacy backup-server appliances frozen on rsync 2.6.x daemons. When
oc-rsync acts as the client against such a daemon, it MUST handle:

- Negotiating DOWN to protocol 28 when the daemon advertises 28 in the
  `@RSYNCD: 28` greeting.
- The pre-INC_RECURSE receiver path: when oc-rsync is the receiver, it
  must NOT advertise the `i` capability flag in the modern capability
  string because the proto-28 daemon rejects unknown flags.
- The proto-28 flist DECODE path: name-prefix runs, no atime, no
  crtime, no extended xflags.
- The proto < 31 zlib codec DECODE without expecting cursor-advance
  behavior - upstream pre-31 zlib does not advance the codec cursor on
  empty flushes the same way modern builds do.
- The proto < 31 absence of `NDX_DEL_STATS` in the goodbye phase. The
  receiver must not block waiting for delete-stats varints.
- Pre-checksum-negotiation MD5 fallback. The daemon will not send a
  `CSUM_*` negotiation frame, so oc-rsync must default to MD5 instead
  of waiting for XXH3/XXH128 advertisement.

Each fixture in section 4 exercises at least one of these code paths.

## 3. Test topology

Daemon (peer under test):

    /usr/local/bin/rsync-2.6.9 \
        --daemon --no-detach \
        --port "$port" \
        --config "$cfg"

Daemon config (`/tmp/rsyncd-2-6-9.conf`):

    use chroot = no
    pid file = /tmp/rsyncd-2-6-9.pid
    [legacy]
        path = /tmp/rp28-f/daemon-share
        read only = false
        list = yes

Client (system under test):

    target/release/oc-rsync

Client invocations:

- Pull: `oc-rsync -av rsync://localhost:$port/legacy/ /tmp/rp28-f/client-dest/`
- Push: `oc-rsync -av /tmp/rp28-f/client-src/ rsync://localhost:$port/legacy/`

`$port` is selected by the harness at startup (see section 6) and
written to a sentinel file so the runner and per-fixture invocations
read the same value.

## 4. Fixture matrix

| Fixture | Direction | What it exercises |
|---------|-----------|-------------------|
| F1: empty dir | pull + push | smoke test - daemon greeting, module list, empty flist |
| F2: 100 small files | pull + push | flist DECODE / ENCODE at proto 28 |
| F3: file with `-z` (client requests) | pull | zlib DECODE at proto 28 without cursor-advance assumption |
| F4: file with `--checksum` (client requests) | pull | MD5 fallback when daemon lacks checksum-negotiation |
| F5: file with `--delete` on local | pull + push | delete-stats absence handling at proto < 31 |
| F6: directory tree (3 levels) | push | non-INC_RECURSE sender path against legacy receiver |
| F7: file with extended chars in name | pull + push | name DECODE at proto 28 (no UTF-8 hint frame) |
| F8: hardlink group (2 files) | pull + push | hardlink wire DECODE at proto 28 |
| F9: incremental update | pull (twice) | quick-check + delta DECODE at proto 28 |
| F10: 1 MiB file with delta | pull (modify on daemon side) | rolling + strong checksum DECODE at proto 28 |
| F11: oc-rsync sends `-e` capability string | push | verify back-negotiation to 28 in capability string |
| F12: `--exclude` / `--filter` | pull + push | filter encoding at proto 28 |

Fixture-direction count: 12 fixtures, 17 transfer runs total (counting
pull+push and the F9 double-pull).

## 5. Pass / fail criteria

Per fixture:

- Client (oc-rsync) exits 0.
- Destination tree is byte-identical to source. Verified by `diff -r`
  against the post-transfer destination, plus a content checksum check
  for files larger than 64 KiB.
- oc-rsync client stderr contains no panics, no `error:` log lines, and
  no `WARNING` other than expected protocol-downgrade messages
  (matched by an allowlist regex maintained alongside the runner
  script).
- The wire transcript (when captured) shows a successful protocol-28
  handshake: the daemon advertises `@RSYNCD: 28`, the client replies
  with `@RSYNCD: 32` and the negotiated protocol resolves to 28.

A fixture fails the overall job if any of the above are violated.
Stderr keyword tolerance (see section 8) is enforced through the
allowlist, not by suppressing stderr entirely.

## 6. Harness structure

RP28.f.2 implements two scripts:

- `scripts/rp28_f_1_setup.sh`
  - Deterministic fixture generator.
  - Builds the daemon-side share (`/tmp/rp28-f/daemon-share`) and the
    client-side source (`/tmp/rp28-f/client-src`) for each fixture.
  - Writes the daemon config (`/tmp/rsyncd-2-6-9.conf`).
  - Uses fixed seeds for any random content so reruns produce
    byte-identical fixtures.
  - Cleans up `/tmp/rp28-f` and `/tmp/rsyncd-2-6-9.{pid,conf}` on
    EXIT trap.

- `scripts/rp28_f_1_run.sh`
  - Orchestrates daemon start: launches
    `rsync-2.6.9 --daemon --no-detach` on an ephemeral port.
  - Writes the chosen port to `/tmp/rp28-f/port`; subsequent fixture
    runs read it back rather than re-binding.
  - Iterates each fixture in sequence, capturing exit code, stdout,
    stderr, and (if `RP28_F_PCAP=1`) a tcpdump of the loopback port.
  - Kills the daemon on EXIT trap, including failure paths.
  - Returns exit 0 only when every fixture passes; otherwise returns
    the index of the first failing fixture.

Both scripts use `set -euo pipefail` and unconditional `trap` cleanups
so a failing fixture does not leak a daemon, a pid file, or a fixture
tree.

Ephemeral-port allocation follows the same pattern as the RP28.e.1
sibling harness: bind a transient TCP socket to port 0, read back the
kernel-chosen port, then close the probe socket immediately before
launching the daemon. The narrow race between probe-close and daemon-
bind is mitigated by a retry loop bounded at 5 attempts.

## 7. CI workflow integration (for RP28.f.2)

`.github/workflows/_interop.yml` gains a new job
`rp28_f_client_2_6_9_interop` with the following shape:

- `needs:` the rsync-2.6.9 build artifact cache (RP28.b.3), which
  publishes a cached `rsync-2.6.9` binary built via
  `scripts/build_rsync_2_6_9.sh`.
- Builds oc-rsync via the standard cargo path already used by the
  existing interop job (the workflow handles cargo invocation; the
  harness scripts never run cargo themselves).
- Invokes `scripts/rp28_f_1_run.sh`; asserts exit 0.
- `continue-on-error: true` initially. Promotion to required-check is
  tracked as a separate follow-up under RP28.f and is intentionally
  not part of RP28.f.2.
- Uploads `/tmp/rp28-f` and any captured pcap as a workflow artifact
  on failure for offline diagnosis.

## 8. Known fragilities

- Stderr keyword tolerance. rsync 2.6.9 emits log messages and warning
  strings the modern client doesn't recognise (e.g. legacy phrasing for
  partial-transfer warnings). The runner allowlist must distinguish
  expected legacy-protocol notices from genuine errors. The allowlist
  lives next to the runner script so it can be diffed in code review.

- Capability-string back-negotiation. oc-rsync's
  `build_capability_string` produces a modern string; the proto-28
  daemon must accept it without rejecting the connection. F11 surfaces
  any incompatibility early. Any mitigating string changes belong to
  the RP28.g / RP28.h / RP28.i wire-byte work, not to this harness.

- Ephemeral-port allocation race. Identical concern to RP28.e.1: bound
  retry loop with a small backoff is the agreed mitigation.

- Daemon stale pid file. `/tmp/rsyncd-2-6-9.pid` must be removed
  between runs. The EXIT trap in the runner handles the normal path;
  the setup script also unlinks it pre-start to recover from prior
  crashed runs.

- Container vs host `/tmp` semantics. CI runs on ubuntu-latest where
  `/tmp` is process-local, so leakage across jobs is not a concern.
  Local container reruns should pass `WORKDIR=` to override the path.

- 2.6.9 sender quirks under push. F6 / F8 / F12 are the most likely
  fixtures to surface latent push-direction gaps; failures should be
  triaged against the RP28.c.a push-direction fixture spec (PR #4923)
  before being attributed to client-side regressions.

## 9. Cross-references

- RP28.b.1 build script: `scripts/build_rsync_2_6_9.sh` (PR #4903).
- RP28.c.a push-direction fixture spec (PR #4923).
- RP28.e.1 daemon-mode harness spec (sibling task in this sprint;
  inverse topology - oc-rsync is the daemon, rsync 2.6.9 is the
  client).
- RP28.g flist wire-flag tests: `crates/protocol/tests/flist_wire_flags_rp28g.rs`.
- RP28.h flist sort-key tests: `crates/protocol/tests/flist_sort_keys_rp28h.rs`.
- RP28.i wire-byte regression tests: tracked under RP28 (#2725).
- Memory note: [[project_protocol_compat]] - oc-rsync must remain
  wire-equivalent to the supported upstream rsync versions, of which
  2.6.9 is the protocol-28 baseline.
