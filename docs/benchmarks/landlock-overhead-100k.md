# Landlock overhead bench (URV-5.c.4)

Per-connection cost of the SEC-1.p Landlock LSM allowlist on a 100K-file
daemon receive. Gates the URV-5.c.5 default-on flip in
`crates/daemon/Cargo.toml`.

## Bench plan

Two harnesses, each with a defined scope:

- **Micro-bench (criterion)** -- `crates/fast_io/benches/landlock_overhead.rs`.
  Measures the per-connection setup the daemon pays before any transfer
  byte moves: kernel probe (`is_supported`) plus
  `restrict_to_module_paths` over 1 / 2 / 4 allowlist roots. Cross-platform
  stub on non-Linux hosts.
- **Macro-bench (hyperfine + strace)** -- `scripts/landlock_overhead_macro.sh`.
  Drives a 100K-file push transfer (upstream rsync client -> oc-rsync
  daemon) on localhost TCP. Captures wall-clock (median of 5 runs), peak
  RSS, and `strace -c -f` syscall counts for landlock ON vs OFF.

The macro-bench requires two pre-built `oc-rsync` release binaries: one
with `landlock` wired in (current Linux default) and one with
`features = ["landlock"]` stripped from the daemon's Linux dep on
`fast_io`. URV-5.c.5 replaces the manual edit with a Cargo-level toggle.

## Workload

- 100,000 files across 1,000 directories, 1 KB each (10^5 inode pressure,
  ~100 MB payload). Modelled on `scripts/benchmark_100k.sh` so the cell
  sits in the same regime as DIS-7 / RSS-1.
- Push transfer: `rsync -a TREE/ rsync://localhost:PORT/bench/`.
- hyperfine `--runs 5 --warmup 1`, fresh destination directory per run.
- strace cell attaches `strace -c -f` to the daemon PID for one full
  transfer per cell (no measurement-time amortisation because the
  attached strace dominates the wall-clock).

## Environment

- Container: `rsync-profile` (Debian, `rust:latest` base), aarch64
  Linux 6.x. Required because Landlock is Linux-only and the host
  development machine cannot exercise the LSM. See
  `feedback_use_container_for_linux_bench`.

## Results

Pending: container run on `rsync-profile`. Numbers below populate once
the macro-bench has been driven; the micro-bench cells are included so
the per-connection floor is on record independently of the data-path
run.

### Macro-bench: full 100K-file daemon receive

| Configuration | Wall-clock median (ms) | Peak RSS (MB) | Total syscalls | Setup overhead (us) |
|---------------|------------------------|---------------|----------------|---------------------|
| landlock OFF  | TBD pending container run | TBD | TBD | 0 |
| landlock ON   | TBD pending container run | TBD | TBD | TBD |
| Delta         | TBD                    | TBD           | TBD            | TBD                 |

### Micro-bench: per-connection setup cost

`cargo bench -p fast_io --bench landlock_overhead --features landlock`

| Cell                              | Median (us) | p99 (us) | Notes                                                                     |
|-----------------------------------|-------------|----------|---------------------------------------------------------------------------|
| `is_supported/probe`              | TBD         | TBD      | Pre-feature floor; runs on every daemon connection regardless of feature. |
| `baseline/thread_spawn_join`      | TBD         | TBD      | Noise floor; subtract from `restrict/*` to isolate Landlock-attributable. |
| `restrict/roots=1`                | TBD         | TBD      | Typical single-module daemon path.                                        |
| `restrict/roots=2`                | TBD         | TBD      | Module + `--temp-dir` after PR #5601.                                     |
| `restrict/roots=4`                | TBD         | TBD      | Worst case after PR #5601 (module + temp/partial + 1 ref_dir).            |

### strace -c -f syscall breakdown

Pending container run. Expected delta: a handful of
`landlock_create_ruleset` / `landlock_add_rule` / `landlock_restrict_self`
calls plus the inherited overhead on later `openat` / `linkat` /
`renameat2` calls.

```
# landlock OFF strace summary -- TBD
# landlock ON strace summary  -- TBD
```

## Decision criteria for URV-5.c.5

The default-on flip lands when the macro-bench numbers fall into one of
the first two rows:

| Wall-clock overhead | Action                                                                                   |
|---------------------|------------------------------------------------------------------------------------------|
| < 1%                | Flip default-on with no release-note callout beyond the security note.                   |
| 1% to 5%            | Flip default-on; release notes call out the trade and the per-module opt-out path.       |
| > 5%                | Keep opt-in; document the gap in `docs/packaging/landlock-feature-guidance.md` and re-task. |

The micro-bench gates a complementary axis: if the per-connection setup
floor exceeds 1 ms median on a clean kernel, the flip is deferred even
when the macro-bench wall-clock is under 1%, because the setup latency
hits every short-lived daemon connection rather than amortising over the
transfer.

## Cross-references

- Design: [`docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`](../design/sec-1-p-landlock-defense-in-depth-2026-05-22.md)
- Packaging guidance: [`docs/packaging/landlock-feature-guidance.md`](../packaging/landlock-feature-guidance.md)
- Allowlist widening (URV-5.b.REOPEN): PR #5601
- Preferred sandboxing primitive: `feedback_rust_landlock_preferred`
- Linux bench container policy: `feedback_use_container_for_linux_bench`

## Reproduction

Inside `rsync-profile`:

```sh
# Stage the landlock-OFF binary by stripping `features = ["landlock"]`
# from `crates/daemon/Cargo.toml`'s Linux fast_io dep, then:
cargo build --release --bin oc-rsync
cp target/release/oc-rsync /tmp/oc-rsync-landlock-off

# Revert the Cargo.toml edit and rebuild for the ON binary:
git checkout -- crates/daemon/Cargo.toml
cargo build --release --bin oc-rsync
cp target/release/oc-rsync /tmp/oc-rsync-landlock-on

# Drive the macro-bench (creates the 100K tree, runs hyperfine + strace,
# appends results to docs/benchmarks/landlock-overhead-100k.md):
OC_ON=/tmp/oc-rsync-landlock-on \
OC_OFF=/tmp/oc-rsync-landlock-off \
KEEP_BUILDS=1 \
bash scripts/landlock_overhead_macro.sh

# Micro-bench (no manual binary staging required):
cargo bench -p fast_io --bench landlock_overhead --features landlock
```
