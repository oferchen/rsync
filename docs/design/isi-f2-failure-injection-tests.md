# ISI.f.2 - Failure-injection test implementation for sender INC_RECURSE

Tracking: ISI.f.2 (#2974). Spec parent: ISI.f.1 (#2973,
`docs/design/isi-f-1-sender-inc-recurse-failure-modes.md`). Sibling:
ISI.f.3 (#2975, receiver-side io_error propagation verification).
Parent series: ISI (#2737).

## 1. Scope

ISI.f.2 implements the failure-injection tests catalogued in ISI.f.1
section 4 (FM2 through FM10). FM1 already ships in the ISI.f test at
`tests/inc_recurse_sender_flist_io_error_isi_f.rs`; ISI.f.2 does not
duplicate it.

Each test exercises the sender-side INC_RECURSE walk under a specific
failure condition and asserts observable behavior: exit code, io_error
propagation, destination tree shape, and stderr diagnostics. The tests
run against an upstream rsync 3.4.1 receiver over wired stdio pipes,
reusing the `run_pipe_push` harness from ISI.f.

## 2. Test catalog

Nine test functions, one per failure mode from ISI.f.1 section 4.
All reside in a single test target
`tests/isi_f_2_sender_inc_recurse_failure_injection.rs` under
`crates/transfer/` (or at the workspace root alongside the ISI.f file,
per the existing layout convention).

### 2.1 FM2 - Subdirectory deleted mid-walk

**Function:** `sender_inc_recurse_subdir_vanishes_midwalk_isi_f_2`

**Injection mechanism:** Source tree with >= 4 sibling subdirectories,
each containing >= 64 files. A coordinator thread polls for a sentinel
file proving the sender has enumerated the first sibling, then
`rm -rf`s a later-ordered sibling the sender has not yet reached.

**Assertions:**
- Exit code in `{0, 23, 24}` (`RERR_PARTIAL` or `RERR_VANISHED` or
  silent skip).
- At least one peer exits non-zero if the race window was wide enough
  for the deletion to land between stat and opendir.
- Readable siblings arrive byte-identical (SHA-256 snapshot comparison).
- Deleted subtree absent from destination.

**Synchronization:** Sentinel file plus bounded poll loop (200
iterations x 50 ms = 10 s deadline). No fixed sleeps.

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

### 2.2 FM3 - Subdirectory appearing mid-walk

**Function:** `sender_inc_recurse_subdir_appears_after_parent_walk_isi_f_2`

**Injection mechanism:** Source tree with `a/`, `b/`, `c/`. Coordinator
waits for the sentinel proving `a/` has been emitted, then creates
`a/late_dir/` with a file inside.

**Assertions:**
- `a/late_dir/` absent from destination - INC_RECURSE is forward-only
  within a segment.
- Both peers exit 0.
- All pre-existing files transfer byte-identical.

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

### 2.3 FM4 - Symbolic link loop

**Function:** `sender_inc_recurse_symlink_loop_isi_f_2`

**Injection mechanism:** Source tree with `a/b/c` where `c` is a
symlink to `../../a`, constructed via `std::os::unix::fs::symlink`.
Two sub-cases:

- (a) Default symlink treatment (no `--copy-links`): symlink encoded
  as-is in flist.
- (b) `--copy-links`: sender must detect the loop and report
  `IOERR_GENERAL`.

**Assertions:**
- Sub-case (a): destination contains the symlink (not recursive
  expansion); exit 0; transfer terminates.
- Sub-case (b): exit code in `{23}` (`RERR_PARTIAL`); sender stderr
  references the looping path.

**Platform gate:** `#[cfg(unix)]`.

### 2.4 FM5 - Receiver disconnect mid-flist transmission

**Function:** `sender_inc_recurse_receiver_disconnect_midflist_isi_f_2`

**Injection mechanism:** Source tree with >= 1000 entries so flist
emission spans multiple write buffers. Receiver is a Rust harness
thread that reads the greeting plus the first 1 KiB of flist bytes,
then closes the read half (drops the pipe handles).

**Assertions:**
- Sender exits within 5 s.
- Exit code in `{10, 12}` (`RERR_PROTOCOL` or `RERR_STREAMIO`).
- On Linux (`#[cfg(target_os = "linux")]`): no leaked threads
  attributable to the spawned oc-rsync process.

**Platform gate:** `#[cfg(unix)]`.

### 2.5 FM6 - `.rsync-filter` discovered mid-walk in deep subdir

**Function:** `sender_inc_recurse_dir_merge_filter_midwalk_isi_f_2`

**Injection mechanism:** Source tree:
- `a/keep.txt`, `a/drop.skip`
- `b/sub/.rsync-filter` (content: `- *.skip`)
- `b/sub/keep.txt`, `b/sub/drop.skip`

Sender invoked with `-F` (or `--filter='dir-merge .rsync-filter'`).

**Assertions:**
- `b/sub/drop.skip` absent from destination (filtered by dir-local
  rule).
- `b/sub/keep.txt` transferred.
- `a/drop.skip` transferred (the filter is dir-local to `b/sub/`,
  not retroactive).
- `a/keep.txt` transferred.
- Exit 0.

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

### 2.6 FM7 - Segment ordering corruption (synthetic)

**Function:** `sender_inc_recurse_segment_ordering_corruption_isi_f_2`

**Gate:** `#[cfg(feature = "test-hooks")]`. If the test-hook
infrastructure does not exist at implementation time, this test splits
into ISI.f.2.a (implement hook) and ISI.f.2.b (implement test).

**Injection mechanism:** A test hook increments the segment ID counter
by 2 instead of 1 on the second segment, simulating a dropped segment
header. Source tree wide enough to produce >= 3 segments.

**Assertions:**
- Receiver detects the gap (sequence number mismatch).
- Receiver exits with `RERR_PROTOCOL` (10) or `RERR_STREAMIO` (12).
- Every file present at the destination is byte-identical to its
  source (no silent corruption).

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

### 2.7 FM8 - OOM mid-walk on huge directory

**Function:** `sender_inc_recurse_oom_during_walk_isi_f_2`

**Injection mechanism:** Source directory with 100,000 small files.
Sender launched with `OC_RSYNC_FLIST_MEM_CAP_BYTES=1048576` (1 MiB)
environment variable. The walk path reads this cap once at start and
returns a graceful error when the flist exceeds it.

If the env var is not yet implemented at ISI.f.2 time, this test
splits into ISI.f.2.c (add cap under
`crates/transfer/src/generator/file_list/` behind a config field) and
ISI.f.2.d (add test).

**Assertions:**
- Sender exits with `RERR_MALLOC` (22) or `RERR_PARTIAL` (23).
- Receiver exits with `RERR_PROTOCOL` (10) or `RERR_STREAMIO` (12).
- No file present at destination is corrupted.
- Must NOT actually exhaust system RAM in CI - the cap env var is the
  only mechanism.

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

### 2.8 FM9 - Source file rewritten between flist emission and block transfer

**Function:** `sender_inc_recurse_source_rewrite_between_segments_isi_f_2`

**Injection mechanism:** Source file `a/changing.bin` (initial content:
64 KiB of 0xAA). Coordinator thread waits for sentinel proving `a/`'s
flist segment has been emitted, then rewrites `a/changing.bin` to
64 KiB of 0xBB.

**Assertions:**
- Destination contains post-rewrite content (0xBB), OR sender detects
  size/mtime drift and reports `IOERR_VANISHED`.
- Upstream rsync reads whatever is in the file at block-transfer time;
  the test accepts either outcome and pins the observed one via a
  snapshot comment.
- Exit code in `{0, 23, 24}`.

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

### 2.9 FM10 - `--max-size` / `--min-size` interaction with INC_RECURSE

**Function:** `sender_inc_recurse_size_filters_isi_f_2`

**Injection mechanism:** Source spans 5 directories, each with 5 files
of sizes {0, 1 KiB, 64 KiB, 1 MiB, 8 MiB}. Two runs:
- (a) `--max-size=1M`
- (b) `--min-size=64K`

Each run is paired with a non-INC_RECURSE reference run
(`--no-inc-recursive`) against the same fixture.

**Assertions:**
- Destination tree from the INC_RECURSE run is byte-identical to the
  destination tree from the reference run.
- Exit 0 in both cases.

**Platform gate:** `#[cfg(all(unix, not(target_os = "macos")))]`.

## 3. Failure injection mechanisms

### 3.1 Filesystem-level injection

Most failure modes use filesystem manipulation to trigger errors
without modifying production code:

| Mechanism | Failure modes | How it works |
|-----------|---------------|-------------|
| `chmod 0000` | FM1 (existing) | `read_dir` returns EACCES on non-root |
| `rm -rf` mid-walk | FM2 | Coordinator deletes sibling dir between stat and opendir |
| `mkdir` mid-walk | FM3 | Coordinator creates dir after segment emitted |
| `symlink("../../a", "c")` | FM4 | Circular symlink in source tree |
| Pipe close | FM5 | Drop receiver's read half during flist transmission |
| `.rsync-filter` file | FM6 | Dir-local filter discovered during walk |
| File rewrite | FM9 | Coordinator overwrites file content after flist emitted |
| Size-filter args | FM10 | `--max-size` / `--min-size` on CLI |

### 3.2 Code-level injection

Two failure modes require hooks or env-var-gated behavior:

| Mechanism | Failure modes | Implementation |
|-----------|---------------|---------------|
| Segment ID corruption hook | FM7 | `#[cfg(feature = "test-hooks")]` in `protocol_io.rs::encode_and_send_segment` |
| Flist memory cap env var | FM8 | `OC_RSYNC_FLIST_MEM_CAP_BYTES` read in `file_list/walk.rs::scan_directory_batched` |

If either infrastructure is absent at implementation time, the
affected test splits into a "build infrastructure" sub-task and a
"build test" sub-task per ISI.f.1 section 4.

### 3.3 Mid-walk coordination protocol

Tests requiring mid-walk mutation (FM2, FM3, FM5, FM9) share a
sentinel-based coordination protocol:

1. Test harness creates a sentinel directory (outside the source tree)
   and passes its path to the sender via an env var or a known
   filesystem location.
2. The sender (or the test pipe driver) creates a marker file in the
   sentinel directory after reaching a known enumeration point.
3. The coordinator thread polls for the marker with a bounded retry
   loop: 200 iterations x 50 ms = 10 s deadline.
4. Once the marker appears, the coordinator performs its mutation
   (delete, create, rewrite, disconnect).
5. If the marker never appears within the deadline, the test fails
   loudly with a descriptive message.

No fixed `sleep` calls. The polling loop is the only timing mechanism.

## 4. Wire-level assertions

### 4.1 io_error flag in flist end marker

For failure modes that trigger `record_io_error` or `add_io_error`
(FM1, FM2, FM4b, FM7, FM8, FM9), the sender writes a non-zero
io_error into the flist segment end marker:

- Varint mode (protocol >= 30 with `VARINT_FLIST_FLAGS`):
  `write_varint(writer, 0)` + `write_varint(writer, io_error)` per
  `crates/protocol/src/flist/write/encoding.rs:352-354`.
- Safe file list mode:
  `[XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST]` + `write_varint(writer, error)` per
  `crates/protocol/src/flist/write/encoding.rs:358-363`.

The receiver reads the io_error via
`crates/protocol/src/flist/read/flags.rs:78-86` and accumulates it
into `FileListReader.io_error` (OR'd, per upstream `flist.c io_error |= err`).

**Test assertion shape:** For black-box tests (upstream rsync
receiver), parse receiver stderr from `--info=stats2` for the io_error
summary line. For Rust-level tests (ISI.f.3 scope), read
`FileListReader::io_error()` directly.

### 4.2 io_error flag codes

| Flag | Value | Upstream constant | Triggered by |
|------|-------|-------------------|-------------|
| `IOERR_GENERAL` | `1 << 0` | `rsync.h:168` | FM1, FM4b, FM8 |
| `IOERR_VANISHED` | `1 << 1` | `rsync.h:169` | FM2, FM9 |
| `IOERR_DEL_LIMIT` | `1 << 2` | `rsync.h:170` | (not triggered by this suite) |

### 4.3 Exit code mapping

| io_error bits | Exit code | Upstream constant |
|---------------|-----------|-------------------|
| `IOERR_DEL_LIMIT` set | 25 | `RERR_DEL_LIMIT` |
| `IOERR_GENERAL` set | 23 | `RERR_PARTIAL` |
| `IOERR_VANISHED` set (only) | 24 | `RERR_VANISHED` |
| No bits set | 0 | success |

Per `crates/transfer/src/generator/io_error_flags.rs::to_exit_code`.

## 5. Interop dimension

### 5.1 Tested configuration

All tests pipe oc-rsync as `--server --sender` (sender role) against
upstream rsync 3.4.1 as `--server` (receiver role). This validates:

- oc-rsync sender correctly emits io_error in the flist end marker.
- Upstream receiver parses the io_error and exits gracefully.
- No wire-format divergence causes the receiver to abort with
  `RERR_PROTOCOL` or `RERR_STREAMIO` on a legitimately partial
  transfer.

### 5.2 Upstream receiver behavior under io_error

Upstream rsync 3.4.1 receiver behavior when it sees a non-zero
io_error in the flist end marker:

- Accumulates the error via `io_error |= err`
  (`flist.c:recv_file_list()`).
- Continues processing the partial flist - does not abort.
- At end-of-transfer, maps the accumulated io_error to exit code via
  `log_exit()` -> `RERR_PARTIAL` / `RERR_VANISHED`.
- Emits diagnostic to stderr: "IO error encountered - skipping file
  deletion" (when `--delete` is active) or silently continues.

### 5.3 Binary lookup

Tests reuse `integration::helpers::upstream_rsync_binary("3.4.1")` to
locate the upstream binary at
`target/interop/upstream-install/3.4.1/bin/rsync`. If absent, the test
logs `skip:` and returns successfully. Run `tools/ci/run_interop.sh`
to populate the install tree.

## 6. Test fixture setup

### 6.1 Directory layout convention

All fixtures use `TestDir::new()` from
`tests/integration/helpers.rs` for automatic cleanup. Each test
creates:

- `src/` - source tree with the failure condition injected.
- `dst/` - empty destination for the upstream receiver.
- `sentinel/` (when needed) - coordination directory for mid-walk
  mutation.

### 6.2 Permission manipulation

Tests that use `chmod 0000` (FM1) or need DAC-enforced EACCES:

- Gate on `!is_root()` - root bypasses DAC on POSIX.
- Use `PoisonGuard` (RAII guard from ISI.f) to restore permissions
  on drop, preventing `TestDir` cleanup failures on panic.
- Gate on `#[cfg(unix)]` - Windows ACL semantics differ.

### 6.3 Large fixture creation (FM8)

The 100,000-file fixture for FM8 uses `BufWriter` for write batching
and must complete under 2 s on CI hardware. Files are minimal size
(0 or 1 byte) to keep I/O low. The fixture creation time is logged
so regressions in fixture setup are visible.

### 6.4 Snapshot helper promotion

ISI.f's `snapshot()` function (lines 220-243 of
`tests/inc_recurse_sender_flist_io_error_isi_f.rs`) maps relative
paths to SHA-256 digests for destination tree comparison. ISI.f.2
promotes this into a shared module at `tests/common/snapshot.rs` (or
the `tests/integration/helpers.rs` file if the existing layout
prefers consolidation) so multiple failure-mode tests share it without
duplication.

Similarly, `run_pipe_push` (lines 276-339) is promoted to
`tests/common/inc_recurse_pipe.rs` once ISI.f.2 becomes the second
caller, per ISI.f.1 section 6.

## 7. Platform considerations

### 7.1 Unix-only failure modes

| Gate | Reason | Tests |
|------|--------|-------|
| `#[cfg(unix)]` | `symlink()` unavailable on Windows without privilege | FM4 |
| `#[cfg(all(unix, not(target_os = "macos")))]` | Upstream binaries only pre-built for Linux | FM2, FM3, FM5, FM6, FM7, FM8, FM9, FM10 |

### 7.2 Root detection

Tests using `chmod 0000` skip when `geteuid() == 0` because root
bypasses POSIX DAC. Detection uses `Command::new("id").arg("-u")`
(no FFI) per ISI.f's `is_root()` pattern.

### 7.3 Windows

No ISI.f.2 tests run on Windows. The permission-denial mechanism
(POSIX DAC via `chmod`) and the upstream binary dependency (Linux
pre-built) both exclude Windows. If Windows-native failure-injection
is needed in the future, it would use NTFS ACL manipulation via
the `windows` crate and a separate test target.

### 7.4 macOS

macOS is excluded because the upstream rsync binaries are only
pre-built for Linux in `tools/ci/run_interop.sh`. The `chmod 0000`
mechanism works on macOS but the interop harness has no receiver
binary to drive. If macOS interop infrastructure is added later,
the `not(target_os = "macos")` gate can be narrowed.

## 8. Exit code verification

### 8.1 Partial transfer (FM1, FM2, FM4b, FM6, FM8, FM9)

Tests that inject errors during enumeration expect exit code
`RERR_PARTIAL` (23) from the sender. The receiver may exit 23 or 0
depending on whether upstream maps the accumulated io_error to its
own exit. Assertions accept either.

### 8.2 Vanished files (FM2, FM9)

When the specific error is `ENOENT` (file/dir vanished between stat
and open), the sender records `IOERR_VANISHED` instead of
`IOERR_GENERAL`, yielding exit code `RERR_VANISHED` (24) unless
`IOERR_GENERAL` is also set (which downgrades to 23 per the priority
in `to_exit_code`). Tests accept both 23 and 24.

### 8.3 Protocol/stream errors (FM5, FM7)

Tests that simulate receiver disconnect or wire corruption expect
`RERR_PROTOCOL` (10) or `RERR_STREAMIO` (12). These are fatal -
the transfer cannot recover gracefully.

### 8.4 Success (FM3, FM10)

Tests where no error occurs (new dir appears after walk, size filters)
expect exit 0 from both peers.

### 8.5 Exit code assertion shape

```rust
assert!(
    matches!(exit_code, EXPECTED_CODE_1 | EXPECTED_CODE_2 | ...),
    "descriptive message with actual code and stderr dumps"
);
```

The set membership assertion documents the rationale inline:

```rust
// Accept 23 (RERR_PARTIAL) or 24 (RERR_VANISHED) because the
// exact code depends on whether the deletion lands before or after
// stat vs opendir. Both are upstream-compatible partial-transfer
// diagnostics.
```

## 9. Feature flag activation

All ISI.f.2 tests require the `sender-inc-recurse` cargo feature.

### 9.1 Cargo.toml entry

```toml
[[test]]
name = "isi_f_2_sender_inc_recurse_failure_injection"
path = "tests/isi_f_2_sender_inc_recurse_failure_injection.rs"
required-features = ["sender-inc-recurse"]
```

This makes `cargo test -p transfer` (without the feature) skip the
target rather than fail to compile.

### 9.2 CI invocation

```sh
cargo nextest run -p transfer --features sender-inc-recurse \
    -E 'test(isi_f_2)'
```

Must be added to the existing ISI interop workflow job matrix at
`.github/workflows/` alongside the ISI.c/.d/.e/.f cells.

### 9.3 Post-ISI.h

When ISI.h.1 flips the default to `true` and ISI.i.2 retires the
feature flag, the `required-features` line is removed and the tests
become unconditional. ISI.i.2's PR must drop this line.

## 10. Test infrastructure requirements

### 10.1 Existing infrastructure to reuse

| Component | Location | Used by |
|-----------|----------|---------|
| `TestDir` | `tests/integration/helpers.rs:70` | All tests |
| `upstream_rsync_binary()` | `tests/integration/helpers.rs:720` | All interop tests |
| `PoisonGuard` | `tests/inc_recurse_sender_flist_io_error_isi_f.rs:206` | FM1 (promote to shared) |
| `is_root()` | `tests/inc_recurse_sender_flist_io_error_isi_f.rs:149` | Permission tests |
| `snapshot()` | `tests/inc_recurse_sender_flist_io_error_isi_f.rs:222` | All snapshot assertions |
| `run_pipe_push()` | `tests/inc_recurse_sender_flist_io_error_isi_f.rs:276` | All pipe-driven tests |
| `locate_oc_rsync()` | `tests/inc_recurse_sender_flist_io_error_isi_f.rs:115` | All tests |
| `copy_until_eof()` | `tests/inc_recurse_sender_flist_io_error_isi_f.rs:249` | Pipe driver |

### 10.2 New infrastructure to build

| Component | Purpose | Notes |
|-----------|---------|-------|
| Shared `snapshot` module | Deduplicate SHA-256 tree comparison | Promote from ISI.f |
| Shared `inc_recurse_pipe` module | Deduplicate pipe driver | Promote from ISI.f |
| Sentinel polling helper | Bounded poll for mid-walk coordination | New; reused by FM2, FM3, FM5, FM9 |
| Segment ID corruption hook | Synthetic wire corruption for FM7 | `#[cfg(feature = "test-hooks")]` in `protocol_io.rs` |
| Flist memory cap | Graceful OOM for FM8 | Env var `OC_RSYNC_FLIST_MEM_CAP_BYTES` in walk path |

### 10.3 Nextest filter

```sh
cargo nextest run --features sender-inc-recurse \
    -E 'test(isi_f_2)' --color never
```

The `_isi_f_2` suffix on all function names ensures the filter selects
only this suite without colliding with ISI.f's `_isi_f` suffix or
ISI.f.1's `_isi_f_1` suffix.

## 11. Ordering and dependencies

### 11.1 Implementation order

The tests are ordered by infrastructure dependency, not FM number:

1. **Phase 1 - Filesystem-only tests (no new infrastructure):**
   FM3, FM4, FM6, FM10. These use only filesystem manipulation and the
   existing pipe driver.

2. **Phase 2 - Mid-walk coordination tests:**
   FM2, FM5, FM9. These require the sentinel polling helper.

3. **Phase 3 - Code-level injection tests:**
   FM7 (requires test-hook infrastructure), FM8 (requires env-var cap).

### 11.2 PR structure

Single PR containing:
- Promoted shared helpers (snapshot, pipe driver, sentinel poller).
- All FM2-FM10 test implementations.
- Cargo.toml test target entry.
- CI workflow addition.

If FM7 or FM8 infrastructure is deferred, those tests are stubbed
with `#[ignore]` and a tracking comment referencing ISI.f.2.a/b/c/d.

## 12. Pass / fail criteria

Identical to ISI.f.1 section 5. Each test must assert specific
observables:

- **Exit code:** exact value or membership in a documented set with
  inline rationale.
- **io_error count:** parsed from receiver `--info=stats2` stderr
  (black-box) or from `TransferStats.io_error` (Rust-level, ISI.f.3
  scope).
- **Destination tree shape:** SHA-256 digest map compared against the
  expected set via the promoted `snapshot()` helper.
- **Stderr content:** substring match on upstream-compatible diagnostic
  strings (`opendir`, `readdir`, `vanished`, the failed path).

Tests must NOT depend on wall-clock timing. Mid-walk coordination
uses sentinel files plus bounded polling, never fixed sleeps.

## 13. V61D-2 regression constraint

Per ISI.f.1 section 9: ISI.f.2 tests must NOT regress the performance
characteristics locked in by the V61D-2 regression test at
`crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`.
The failure-injection tests add no production-code hot-path changes;
the only production-code additions (if needed) are the `test-hooks`
feature gate and the `OC_RSYNC_FLIST_MEM_CAP_BYTES` env var check,
both of which are no-ops in release builds.

## 14. Cross-references

- ISI.f.1 spec:
  `docs/design/isi-f-1-sender-inc-recurse-failure-modes.md` (#2973).
- ISI.f shipped test:
  `tests/inc_recurse_sender_flist_io_error_isi_f.rs`.
- ISI.a call graph:
  `docs/design/isi-a-sender-inc-recurse-call-graph.md`.
- ISI.h flip implementation:
  `docs/design/isi-h-flip-implementation.md`.
- ISI.i.1 bake-window criteria:
  `docs/design/isi-h-bake-window-criteria.md`.
- io_error flags:
  `crates/transfer/src/generator/io_error_flags.rs`.
- Walk entry point:
  `crates/transfer/src/generator/file_list/walk.rs::scan_directory_batched`.
- Flist end marker with io_error:
  `crates/transfer/src/generator/protocol_io.rs::send_file_list`.
- Flist reader io_error accumulation:
  `crates/protocol/src/flist/read/mod.rs:119-123`.
- V61D-2 regression test:
  `crates/transfer/tests/v61d_2_daemon_push_increcurse_perf_regression.rs`.
- Memory note:
  `[[project_v061_daemon_push_increcurse_disable]]`.
