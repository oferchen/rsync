# Binary startup overhead measurement plan

Tracking issue: oc-rsync #979. Branch: `audits/binary-startup-overhead`.

## 1. Goal

Measure the wall-clock interval between `execve("oc-rsync", ...)` and the
first useful filesystem syscall (typically the first `stat(2)` or `openat(2)`
on the source path) and compare it against upstream rsync 3.4.1. Wrapper
scripts that fork rsync per file, and invocations on small or empty source
trees, are dominated by fixed startup cost rather than transfer cost. We
want a hard number for that cost, broken down by phase, and a baseline to
track regressions per release. For `--version` (no work) the metric reduces
to time until `write(1, ...)` of the version banner. Both modes are cheap to
measure and have direct upstream rsync analogues (`rsync --version`,
`rsync -nv` on empty trees).

## 2. Suspected costs

- **Dynamic linker / loader**: `ld.so` mapping libc, libssl, libz, libxxhash,
  libacl, libattr, ssh client. Visible in `LD_DEBUG=statistics` output and in
  `strace -tt -e trace=openat,mmap` before the binary's `_start`.
- **Lazy `OnceLock` init**: SIMD feature detection in `crates/checksums`,
  logging subsystem in `crates/logging`, version banner assembly in
  `crates/branding`, capability probe in `crates/protocol`, runtime io_uring
  detection in `crates/fast_io`. Each `OnceLock::get_or_init` block contributes
  one-time allocation and platform syscalls (e.g., `getauxval`, `uname`,
  `io_uring_setup`, `kqueue` probes).
- **Clap argument parsing**: `crates/cli` registers ~250 flags. Building the
  Clap `Command` tree, applying defaults, and constructing the `Cli` struct
  is the largest pure-CPU init cost.
- **Environment variable scan**: `RSYNC_*`, `OC_RSYNC_*`, `XDG_*`, `LANG`,
  `TERM`, `HOME`, `PATH`, locale lookups via `setlocale(3)`.
- **Config file resolution**: stat probes for `~/.popt`, `oc-rsyncd.conf` (in
  daemon mode only), filter merge files referenced via `--filter=:`.

## 3. Measurement plan

Run on the existing `rsync-profile` container (Debian, upstream rsync 3.4.1
installed) with both binaries built `--release` and stripped consistently:

```sh
hyperfine --warmup 5 --runs 100 \
  'oc-rsync --version' 'rsync --version'

mkdir -p /tmp/empty /tmp/empty2
hyperfine --warmup 5 --runs 100 \
  'oc-rsync -nv /tmp/empty/ /tmp/empty2/' \
  'rsync    -nv /tmp/empty/ /tmp/empty2/'
```

Capture per-call syscall traces for the breakdown:

```sh
strace -tt -T -ff -o /tmp/oc-trace -e trace=openat,stat,fstat,mmap,brk,write \
  oc-rsync --version
strace -tt -T -ff -o /tmp/rsync-trace -e trace=openat,stat,fstat,mmap,brk,write \
  rsync --version
```

Record results in `scripts/benchmark.sh`-compatible JSON via
`hyperfine --export-json`. A regression threshold of +10% over upstream rsync
3.4.1 on the `--version` run, or a hard ceiling of 15 ms wall-clock on the
`-nv` run with empty directories, would gate the release CI lane added in the
follow-up task.

## 4. Profile inside the binary

Build with frame pointers and DWARF symbols so `perf` can attribute the early
`OnceLock` and Clap costs:

```sh
RUSTFLAGS='-C force-frame-pointers=yes -C debuginfo=2' \
  cargo build --release -p oc-rsync

perf record -F 4000 --call-graph fp -- ./target/release/oc-rsync --version
perf report --stdio --sort=symbol,dso | head -80

# For the larger init phase, take a flamegraph:
perf record -F 4000 --call-graph fp -o init.data -- \
  ./target/release/oc-rsync -nv /tmp/empty/ /tmp/empty2/
perf script -i init.data | inferno-collapse-perf | inferno-flamegraph \
  > /tmp/oc-startup.svg
```

On macOS the equivalent is `xctrace record --template 'Time Profiler'`; on
Linux without `perf` (containers without `CAP_SYS_ADMIN`), substitute
`samply record -- oc-rsync --version`.

## 5. Mitigation candidates

- **Defer probes**: ensure SIMD detection, io_uring probe, capability
  negotiation, and version-banner assembly are wrapped in `OnceLock` and
  invoked only on first use. Audit `crates/checksums`, `crates/fast_io`, and
  `crates/branding` for any eager `lazy_static!` or `ctor`-style init that
  fires before `main` does useful work.
- **Strip debuginfo from release**: keep `[profile.release] debug = false`
  for the shipped binary; emit a separate split debuginfo file
  (`objcopy --only-keep-debug`) for symbolication. Smaller binary = faster
  page-in.
- **`-C panic=abort` for daemon mode**: the daemon process never recovers
  from panic in production. Switching the daemon entry point to
  `panic=abort` removes landing-pad tables and shrinks the binary.
- **LTO=fat**: `[profile.release] lto = "fat"` plus `codegen-units = 1`
  trades build time for smaller, faster code; cross-crate inlining typically
  removes a chunk of trivial wrappers from the init path.
- **Strip locale init**: avoid calling `setlocale(LC_ALL, "")` unless a
  message will actually be formatted with locale-aware code. Upstream rsync
  uses the C locale for wire protocol paths; we should match.
- **Lazy Clap subtree**: Clap supports `defer_help_text` and on-demand
  subcommand registration. Daemon-only flags can be registered only when
  `--daemon` is observed in argv.

## Follow-up tasks

- [ ] #979-1 land the hyperfine harness as
  `scripts/benchmark_startup.sh` so CI can track regressions.
- [ ] #979-2 add a startup-time line to the release notes table generated by
  `.github/workflows/benchmark.yml`.
- [ ] #979-3 implement deferred `OnceLock` audit, file PRs per crate.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`.
- Hyperfine: <https://github.com/sharkdp/hyperfine>.
- `perf-record(1)`, `perf-report(1)`, `inferno-flamegraph`.
- `samply`: <https://github.com/mstange/samply>.
- Existing benchmark plumbing: `scripts/benchmark.sh`,
  `scripts/benchmark_hyperfine.sh`, `.github/workflows/benchmark.yml`.
