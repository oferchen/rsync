# ISI.f.1 - Sender-side INC_RECURSE failure-mode test specification

Tracking: ISI.f.1 (#2973). Implementing follow-ups: ISI.f.2 (#2974,
test implementation), ISI.f.3 (#2975, receiver-side io_error
propagation verification). Parent series: ISI (#2737). Sibling that
already shipped: ISI.f (#2743).

Memory note: `[[project_v061_daemon_push_increcurse_disable]]`.

## 1. Scope

ISI.f.1 specifies the failure-mode test cases that exercise the
sender-side INC_RECURSE walk in conditions where the source tree
changes during enumeration, where enumeration fails partially, or where
the walk interacts pathologically with filter rules or symlinks. The
spec frames each test case as a contract over observable behavior
(exit code, io_error accumulation, destination tree shape, stderr
content) so the implementing sibling can write the tests without
re-deriving the upstream-compatibility goals.

ISI.f.2 implements the tests listed in section 4. ISI.f.3 verifies
that the sender-side `io_error` counter round-trips into the
receiver's goodbye stats (section 7). Both follow this spec; if either
diverges, this doc is the place to reconcile.

Out of scope: failure modes that are not INC_RECURSE-specific (e.g.,
basis-file read errors, mid-transfer disk full on the receiver).
Those are covered by the general transfer-failure suites and do not
require sender-INC_RECURSE-specific harnesses.

## 2. Current coverage (ISI.f, #2743)

ISI.f shipped one failure-mode test:

- File: `tests/inc_recurse_sender_flist_io_error_isi_f.rs`
- Test: `sender_inc_recurse_partial_walk_propagates_io_error`
- Setup: source tree with `a/readable_one`, `a/readable_two`, and
  `a/forbidden` (chmod 0000); pipes `oc-rsync --server --sender`
  through an upstream rsync 3.4.1 receiver.
- Contracts: both peers exit with `RERR_PARTIAL` (23) or 0; readable
  siblings transfer byte-identical; poisoned subtree does not leak
  into the destination; sender stderr surfaces the failed directory.
- Gated by `#[cfg(all(unix, not(target_os = "macos"), feature = "sender-inc-recurse"))]`.

ISI.f covers exactly one failure mode (FM1 in the catalog below):
permission-denied subdirectory mid-walk against an upstream receiver.
The remaining nine modes catalogued in section 4 are unexercised
today; ISI.f.2 closes that gap.

## 3. Implementation references

Code paths the failure-mode tests exercise:

- Walk entry point with io_error accumulation -
  `crates/transfer/src/generator/file_list/walk.rs::scan_directory_batched`.
- Flist end-marker emission with io_error bitfield -
  `crates/transfer/src/generator/protocol_io.rs::send_file_list`.
- IO error bitfield definitions and exit-code mapping -
  `crates/transfer/src/generator/io_error_flags.rs` (`IOERR_GENERAL`
  = 1<<0, `IOERR_VANISHED` = 1<<1, `IOERR_DEL_LIMIT` = 1<<2;
  `to_exit_code` maps to `RERR_PARTIAL`=23 / `RERR_VANISHED`=24 /
  `RERR_DEL_LIMIT`=25).
- Capability string (`'i'` flag gating) -
  `crates/transfer/src/setup/capability.rs::build_capability_string`.
- Stats surface visible to receiver -
  `crates/protocol/src/stats/transfer.rs::TransferStats`.

## 4. Failure-mode catalog

Each entry below names the test function ISI.f.2 will add, the setup
the test must construct, and the assertions the test must hold. Test
function naming convention follows ISI.f: prefix with `sender_inc_recurse_`
so the filter `-E 'test(isi_f_1)'` plus per-fn `_isi_f_1` suffix
selects only this suite.

### FM1 - Permission-denied subdirectory mid-walk

Test: `sender_inc_recurse_perm_denied_midwalk_isi_f_1`

Already covered by ISI.f. This entry is retained for completeness;
ISI.f.2 does NOT duplicate the test, only references it.

### FM2 - Subdirectory deleted mid-walk

Test: `sender_inc_recurse_subdir_vanishes_midwalk_isi_f_1`

Setup: source tree wide enough that the walk takes measurable time
(>= 4 sibling subdirs, each with >= 64 files). After the sender
spawns, a coordinator thread `rmdir -rf`s a subdirectory the sender
has not yet reached. Synchronization: the coordinator polls for the
appearance of a sentinel file the sender creates after enumerating
the first sibling, then deletes a later-ordered sibling. (Sentinel
plus poll keeps the test free of fixed sleep timing.)

Expected: sender either silently skips (vanished before stat) or
records `IOERR_VANISHED` (vanished between stat and opendir).
Transfer continues; readable siblings arrive byte-identical;
receiver exits with `RERR_VANISHED` (24) or `RERR_PARTIAL` (23) or
0 (silent skip path). At least one of the two non-zero codes must
fire across the matrix - if the test never observes io_error
accumulation, the race window is too tight; widen the fixture.

### FM3 - Subdirectory appearing mid-walk

Test: `sender_inc_recurse_subdir_appears_after_parent_walk_isi_f_1`

Setup: source tree with `a/`, `b/`, `c/`. Sender starts. Coordinator
thread waits for the sentinel proving `a/` has been emitted, then
creates `a/late_dir/` with a file inside.

Expected: sender does NOT re-walk `a/`; the late directory is absent
from the destination. Both peers exit 0. This codifies the upstream
contract that INC_RECURSE is forward-only within a segment.

### FM4 - Symbolic link loop

Test: `sender_inc_recurse_symlink_loop_isi_f_1`
Platform gate: `#[cfg(unix)]`.

Setup: source tree with `a/b/c -> ../../a` constructed via
`std::os::unix::fs::symlink`. Run sender with default symlink
treatment (no `--copy-links`).

Expected: sender encodes the looping symlink as a symlink entry in
the flist (not followed); recursion terminates; transfer exits 0;
destination contains the symlink, not a recursive expansion. If
`--copy-links` is passed, sender must detect the loop and report
`IOERR_GENERAL` rather than infinite-looping.

### FM5 - Receiver disconnect mid-flist transmission

Test: `sender_inc_recurse_receiver_disconnect_midflist_isi_f_1`
Platform gate: `#[cfg(unix)]` (uses `/proc/self/status` Threads
counter; Linux-only assertion guarded `#[cfg(target_os = "linux")]`,
the macOS variant asserts only "process exits" and skips the thread
count).

Setup: source tree deep enough that flist emission spans multiple
write buffers (>= 1000 entries). Receiver is a Rust harness thread
that accepts the pipe, reads the greeting + first 1 KiB of flist
bytes, then closes the read half (TCP-equivalent: drops the
stdin/stdout pipe handles).

Expected: sender exits within 5 s (writes hit `EPIPE`); process exit
status is non-zero but specifically `RERR_STREAMIO` (12) or
`RERR_PROTOCOL` (10) - the exact code is acceptable as either; the
test asserts membership in that set. Thread count snapshot before
the test vs 1 s after process exit must not have leaked threads
attributable to the spawned `oc-rsync` (delta <= 0 against the
test harness's own baseline; the snapshot is taken in the test
process, not the spawned subprocess).

### FM6 - `.rsync-filter` discovered mid-walk in deep subdir

Test: `sender_inc_recurse_dir_merge_filter_midwalk_isi_f_1`

Setup: source tree `a/`, `b/sub/.rsync-filter` (content:
`- *.skip`), `b/sub/keep.txt`, `b/sub/drop.skip`, `a/drop.skip`,
`a/keep.txt`. Sender invoked with `-F` (or `--filter='dir-merge .rsync-filter'`).

Expected: `b/sub/drop.skip` is filtered out at the destination;
`b/sub/keep.txt` is transferred; `a/drop.skip` IS transferred (the
filter is dir-local to `b/sub/` only); `a/keep.txt` is transferred.
Exit 0. This locks in the contract that mid-walk filter discovery
applies prospectively within its own segment, not retroactively to
already-emitted segments.

### FM7 - Segment ordering corruption (synthetic)

Test: `sender_inc_recurse_segment_ordering_corruption_isi_f_1`
Gate: `#[cfg(feature = "test-hooks")]` or equivalent; the test-only
hook to corrupt segment sequence numbers must NOT ship in release
builds.

Setup: enable a test hook that increments the segment ID counter by
2 instead of 1 on the second segment, simulating a dropped segment
header. Source tree wide enough to produce >= 3 segments.

Expected: receiver detects the gap (sequence number does not match
the expected `prev + 1`); receiver exits with `RERR_PROTOCOL` (10)
or `RERR_STREAMIO` (12); destination tree is partial but no file is
silently corrupted (every file present at the destination is
byte-identical to its source). If the test-hook infrastructure does
not exist at ISI.f.2 time, this test is split into ISI.f.2.a:
implement hook; ISI.f.2.b: implement test.

### FM8 - OOM mid-walk on huge directory

Test: `sender_inc_recurse_oom_during_walk_isi_f_1`

Setup: source directory populated with 100 000 small files (use
`tempfile::TempDir` + `BufWriter` to keep fixture creation under
2 s). Run sender with an env-injected flist memory cap
(`OC_RSYNC_FLIST_MEM_CAP_BYTES=1048576`, 1 MiB) that the walk path
respects by returning a graceful error rather than panicking. If
the cap env var does not exist at ISI.f.2 time, this test is split
into ISI.f.2.c: add cap; ISI.f.2.d: add test.

Expected: sender exits with `RERR_MALLOC` (22) or `RERR_PARTIAL`
(23); receiver exits with `RERR_PROTOCOL` (10) or `RERR_STREAMIO`
(12); destination tree may be empty or partial but no file present
at the destination is corrupted. Test MUST NOT actually exhaust
system RAM in CI - the cap env var is the only allowed mechanism.

### FM9 - Source file rewritten between flist emission and block transfer

Test: `sender_inc_recurse_source_rewrite_between_segments_isi_f_1`

Setup: source file `a/changing.bin` (initial content: 64 KiB of
0xAA). Sender starts. Coordinator thread waits for the sentinel
proving `a/`'s flist segment has been emitted (sentinel: a marker
file in a separate observation directory the test harness creates
once sender's first `MSG_DATA` for the changing file appears), then
rewrites `a/changing.bin` to 64 KiB of 0xBB.

Expected: destination contains the post-rewrite content (0xBB), or
the sender detects the size/mtime drift and reports
`IOERR_VANISHED`. Upstream rsync's behavior here is "read whatever
is in the file at block-transfer time"; the test asserts oc-rsync
matches by accepting either outcome and pinning the exact one
observed today via a snapshot fixture (record what oc-rsync does in
the test's golden-file comment, update if behavior intentionally
changes).

### FM10 - `--max-size` / `--min-size` interaction with INC_RECURSE

Test: `sender_inc_recurse_size_filters_isi_f_1`

Setup: source spans 5 directories, each with 5 files of sizes
{0, 1 KiB, 64 KiB, 1 MiB, 8 MiB}. Sender invoked twice:
- (a) with `--max-size=1M`,
- (b) with `--min-size=64K`.

Expected: outcomes match a non-INC_RECURSE reference run (same
sender invocation with `--no-inc-recursive`, executed against the
same fixture). The test asserts that the destination tree from the
INC_RECURSE run is byte-identical to the destination tree from the
reference run. Exit 0 in both cases.

## 5. Pass / fail criteria

Each test must assert a specific observable. Acceptable assertion
shapes:

- Exit code: exact value, or membership in a documented set (e.g.,
  "`{0, 23}` because the upstream peer may map io_error to success
  on its own path"). Document the rationale inline next to the
  assertion.
- io_error count: parsed from receiver `--info=stats2` stderr, or
  read from the receiver's `TransferStats` struct in a
  Rust-level test (section 7).
- Destination tree shape: SHA-256 digest of every regular file under
  the destination root, compared against the expected map. Snapshot
  helper is in ISI.f's existing `snapshot()` function (lines
  220-243 of `tests/inc_recurse_sender_flist_io_error_isi_f.rs`) -
  ISI.f.2 should promote it into the shared `tests/common/`
  module rather than duplicating per failure mode.
- Stderr content: substring match on upstream-compatible diagnostic
  strings (`opendir`, `readdir`, `vanished`, the failed path, etc.).

Tests MUST NOT depend on wall-clock timing. Where mid-walk
coordination is required (FM2, FM3, FM5, FM9), use sentinel files
plus polling loops with bounded retries (e.g., 200 iterations of
50 ms = 10 s deadline) rather than fixed sleeps. The deadline is a
hard upper bound; the test fails loudly if the sentinel never
appears.

## 6. Test infrastructure

Concrete implementation pointers for ISI.f.2:

- Fixture creation: use `TestDir` (already in
  `tests/integration/helpers.rs:70`) plus `TestDir::mkdir` and
  `TestDir::write_file`. ISI.f's existing `build_fault_tree` is
  fixture-specific; per-FM helpers in `tests/common/` should follow
  the same shape.
- Upstream binary lookup: reuse `upstream_rsync_binary("3.4.1")`
  from `tests/integration/helpers.rs:720`.
- Pipe driver: reuse `run_pipe_push` from
  `tests/inc_recurse_sender_flist_io_error_isi_f.rs:276-339`.
  Promote to `tests/common/inc_recurse_pipe.rs` when the second
  caller arrives (ISI.f.2 is that caller; promotion is part of the
  ISI.f.2 PR, not a separate refactor).
- Mid-walk mutation coordinator: spawn a `std::thread::spawn`
  closure that polls for a sentinel file the sender writes
  (sentinel path passed in via env var; sender hook injected behind
  `#[cfg(feature = "test-hooks")]`). For tests where a sender hook
  is too invasive, use an inotify watch on the destination tree
  (Linux) and a kqueue watch (macOS); skip the test on Windows.
- OOM cap (FM8): env var `OC_RSYNC_FLIST_MEM_CAP_BYTES`. If absent
  at ISI.f.2 implementation time, add it under
  `crates/transfer/src/generator/file_list/` behind a config field
  read once at walk start. Default unset = no cap (current
  behavior).
- Symlink loop (FM4): `std::os::unix::fs::symlink`; skip on Windows
  via `#[cfg(unix)]`. Privilege gating for Windows symlinks is out
  of scope for this suite.
- Filter (FM6): write `.rsync-filter` files directly; invoke sender
  with `-F` (single dash-F). Verify the dir-merge parse hits at
  `crates/engine/src/local_copy/dir_merge/parse/`.
- Invocation under nextest: filter selector is
  `cargo nextest run -p transfer -E 'test(isi_f_1)' --features sender-inc-recurse`.
  CI must pass `--features sender-inc-recurse` until ISI.h flips
  the default; section 8 has the activation details.

## 7. Receiver-side assertions (ISI.f.3 spec)

ISI.f.3 verifies that the sender's accumulated `io_error` bitfield
arrives at the receiver and lands in the receiver's exposed stats.
The verification path:

- For black-box tests: parse receiver stderr from `--info=stats2`.
  Look for the lines `Number of files transferred:` and the
  io_error summary that upstream emits when the bitfield is
  non-zero. The presence and value pin the round-trip.
- For Rust-level tests: invoke the receiver via the public `core`
  facade (`core::session`) with a `TransferStats` collector wired
  through `core::CoreConfig`. After the session returns, inspect
  the stats: today `TransferStats` in
  `crates/protocol/src/stats/transfer.rs:50-99` exposes counts but
  not the io_error bitfield directly. ISI.f.3 adds an `io_error:
  i32` field to `TransferStats` (defaulting 0) and wires it through
  the receiver path in `crates/transfer/src/receiver/stats.rs`.
  The new field is read from the partial-flist end marker that the
  sender already writes (`flist.c:2518 write_int(f, io_error)`
  equivalent in `generator/protocol_io.rs`).
- Exit-code assertion: receiver exit must reflect the partial
  failure. Mapping is documented in
  `crates/transfer/src/generator/io_error_flags.rs::to_exit_code`:
  `IOERR_DEL_LIMIT` -> 25, `IOERR_GENERAL` -> 23, `IOERR_VANISHED`
  -> 24, zero -> 0. ISI.f.3 asserts the receiver's exit equals the
  sender's exit (or, when the receiver runs as the upstream binary
  in a mixed harness, equals the upstream-equivalent code).
- Successfully-transferred files: each file the receiver claims to
  have written must be SHA-256 identical to its source counterpart.
  The "claimed written" set is the destination tree minus any
  preexisting files; the snapshot helper from ISI.f produces this
  diff.

If `TransferStats.io_error` cannot land in ISI.f.3 for scope
reasons, the test falls back to parsing receiver stderr. The
preferred path is the struct field; the stderr parse is the
documented fallback.

## 8. Sender INC_RECURSE feature-flag activation

All ISI.f.1 series tests must run under the temporary
`sender-inc-recurse` cargo feature until ISI.h.1 lands. Activation
options:

- Per-test-target in `crates/transfer/Cargo.toml`:

  ```toml
  [[test]]
  name = "isi_f_1_sender_inc_recurse_failure_modes"
  path = "tests/isi_f_1_sender_inc_recurse_failure_modes.rs"
  required-features = ["sender-inc-recurse"]
  ```

  This is the preferred form: it documents the dependency in
  Cargo.toml directly and makes `cargo test -p transfer` without
  the feature simply skip the target rather than fail-to-compile.
- CI invocation:
  `cargo nextest run -p transfer --features sender-inc-recurse -E 'test(isi_f_1)'`.
  The existing ISI interop workflow (`.github/workflows/`,
  search for `sender-inc-recurse`) already toggles the feature for
  ISI.c/.d/.e/.f; ISI.f.2 must add its test target to the same job
  matrix so it runs on every PR.

The feature flag wiring is:

- Workspace level: `Cargo.toml:85`
  `sender-inc-recurse = ["core/sender-inc-recurse", "transfer/sender-inc-recurse"]`.
- Transfer crate: `crates/transfer/Cargo.toml:134`
  `sender-inc-recurse = []` (marker feature).
- Capability gating: `core` reads
  `cfg!(feature = "sender-inc-recurse")` as the default for
  `inc_recursive_send` in
  `crates/core/src/client/config/builder/mod.rs:445-447`.

When ISI.h.1 (#2976) flips the default and ISI.i.2 (#2979) removes
the feature, ISI.f.1's tests stay valid - they just stop needing
the explicit feature opt-in. ISI.i.2's PR must drop the
`required-features` line from any test target ISI.f.2 added.

## 9. Cross-references

- ISI.f shipped test - `tests/inc_recurse_sender_flist_io_error_isi_f.rs`
  (this is the seed test the catalog extends).
- ISI.i.1 bake-window doc - `docs/design/isi-h-bake-window-criteria.md`
  (defines the criteria under which ISI.h flips the default; ISI.f.1
  tests are part of the "all interop green" pre-condition).
- ISI.a sender call graph - `docs/design/isi-a-sender-inc-recurse-call-graph.md`
  (background on the sender-side INC_RECURSE walk path the failure
  modes target).
- V61D-2 daemon-push regression - `crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`
  (the regression that motivated the v0.6.1 default-off; ISI.f.1
  tests must NOT regress the perf characteristics V61D-2 locks in).
- Memory note - `[[project_v061_daemon_push_increcurse_disable]]`
  (sender-side INC_RECURSE off by default since v0.6.1; the ISI.h
  series is the planned re-enable, gated on ISI.f.1's coverage
  landing).
