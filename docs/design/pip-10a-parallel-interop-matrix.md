# PIP-10.a - Full upstream interop matrix through parallel receive-delta path

Date: 2026-05-26
Status: design spec
Tracking: PIP-10.a (#3022)
Parent: PIP-10 (end-to-end validation series for parallel receive-delta)
Predecessors: PIP-9 (production wire-up), PIP-8 (dead scaffolding teardown),
PIP-7 (corruption investigation)

## 1. Scope

PIP-10.a validates that the `--features parallel-receive-delta` build
produces byte-identical transfer results against all four supported
upstream rsync versions (3.0.9, 3.1.3, 3.4.1, 3.4.2) across the full
scenario matrix already defined in `tools/ci/run_interop.sh`. This is
the end-to-end interop gate that PIP-9.f.1
(`docs/design/pip-9-f-1-bake-criterion.md`) requires before the
parallel path can become the default.

The existing interop suite exercises the sequential receive path
exclusively. The existing `interop-parallel-receive-delta` job in
`_interop.yml` re-runs the full harness with a parallel-feature
build, but it does not:

- enforce threshold-trip coverage (transfers large enough to trigger
  parallel dispatch);
- compare sequential vs parallel output byte-for-byte via sha256;
- cover all file types (devices, FIFOs) under the parallel path;
- provide per-version per-scenario pass/fail visibility in CI summaries.

PIP-10.a closes these gaps.

## 2. Test matrix

### 2.1 Version axis

| Upstream version | Protocol | Notes |
| --- | --- | --- |
| 3.0.9 | 30 | Core scenarios only (no xattrs, no --protocol=31) |
| 3.1.3 | 31 | Core scenarios only |
| 3.4.1 | 32 | Core + extended scenarios |
| 3.4.2 | 32 | Core + extended scenarios |

The matrix reuses the version set from `tools/ci/run_interop.sh:55`
(`versions=(3.0.9 3.1.3 3.4.1 3.4.2)`). The 2.6.9 pre-30 peer
stays non-blocking per the RP28 series and is excluded from PIP-10.a.

### 2.2 Direction axis

Each scenario runs in both directions, matching the existing harness
pattern at `run_comprehensive_interop_case`:

- **upstream sender -> oc-rsync receiver (pull):** upstream client
  pushes to oc-rsync daemon. The parallel receive-delta path is
  exercised on the oc-rsync side.
- **oc-rsync sender -> upstream receiver (push):** oc-rsync client
  pushes to upstream daemon. The parallel path is not exercised
  directly (oc-rsync is the sender), but this direction validates
  that the feature-flag build does not regress sender behaviour.

### 2.3 Transfer mode axis

The scenario list matches the comprehensive set in
`run_comprehensive_interop_case` (lines 9657-9747 of
`tools/ci/run_interop.sh`). The parallel-interop run must exercise
every scenario the sequential run does. Key scenario groups:

**Core scenarios (all versions):**

| Scenario | Flags | Verification type |
| --- | --- | --- |
| archive | `-av` | basic |
| relative | `-avR` | basic |
| checksum | `-avc` | basic |
| compress | `-avz` | compress |
| whole-file | `-avW` | whole-file |
| delta | `-av --no-whole-file -I` | delta |
| inplace | `-av --inplace` | inplace |
| numeric-ids | `-av --numeric-ids` | numeric-ids |
| symlinks | `-rlptv` | symlinks |
| hardlinks | `-avH` | hardlinks |
| delete | `-av --delete` | delete |
| exclude | `-av --exclude=*.log` | exclude |
| permissions | `-rlpv` | perms |
| itemize | `-avi` | itemize |
| acls | `-avA` | acls |

**Extended scenarios (3.4.1+ only):**

xattrs, one-file-system, whole-file-replace, delay-updates,
recursive-only, delete-after, delete-during, include-exclude,
filter-rule, merge-filter, exclude-from, size-only, ignore-times,
checksum-skip, checksum-content, copy-links, safe-links, existing,
backup, backup-dir, link-dest, max-delete, update, dry-run, sparse,
partial, append, bwlimit, compress-level-1, compress-level-9,
protocol-30, protocol-31, compress-delta, devices, compare-dest,
files-from, hardlinks-relative, hardlinks-delete, hardlinks-numeric,
hardlinks-checksum, hardlinks-existing, inc-recursive-delete,
inc-recursive-symlinks, hardlinks-inc-recursive, compress-zstd
(conditional), compress-lz4 (conditional).

**PIP-10.a-specific scenario (all versions):**

| Scenario | Flags | Verification type |
| --- | --- | --- |
| parallel-threshold-trip | `-av` | parallel-threshold |

This scenario is the PIP-9.c entry that was removed from the default
scenario list pending PIP-7 resolution (line 9673 of
`run_interop.sh`). PIP-10.a re-enables it in the parallel-feature
build only.

### 2.4 Total cell count

Core: 15 scenarios x 4 versions x 2 directions = 120 cells.
Extended: ~45 scenarios x 2 versions x 2 directions = ~180 cells.
Parallel-threshold: 1 scenario x 4 versions x 2 directions = 8 cells.
Inc-recursive (protocol >= 30): 1 x 4 x 2 = 8 cells.
Standalone tests (hardlinks-comprehensive, inc-recurse-comprehensive):
2 x 4 = 8 cells.

Approximate total: ~324 cells per CI run.

## 3. Feature flag activation

### 3.1 Build configuration

The parallel-interop job builds oc-rsync with:

```sh
cargo build --profile dist --features parallel-receive-delta
```

The `dist` profile is mandatory per PIP-7 finding: the corruption only
reproduced under `dist` (LTO + `panic=abort` + `opt-level=z`). Building
with `--release` masks the bug. The built binary replaces
`target/dist/oc-rsync` before the harness runs, matching the V61D-3
pattern established in `_interop.yml:427-449`.

### 3.2 Runtime activation

No runtime knob is required. The `parallel-receive-delta` feature flag
is a compile-time `#[cfg]` gate (PIP-9.b.2 Variant A). When the feature
is compiled in, every file dispatch goes through the parallel arm of the
cfg if-else at the cutover site
(`crates/transfer/src/receiver/transfer/sync.rs:241-253`). There is no
threshold, no env var, and no CLI flag - the feature is always active in
a feature-enabled build per PIP-8's removal of
`PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD` and
`PARALLEL_RECEIVE_BYTES_THRESHOLD`.

## 4. Threshold-trip scenarios

### 4.1 Problem statement

The parallel dispatch path must actually be exercised during the interop
run. With small fixtures (a few files totaling < 1 MB), the parallel
applier's `apply_batch_parallel` may never fan out because the per-file
chunk count stays below the batch threshold
(`ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY = 64`). This
means a small-fixture interop run may pass without ever exercising the
parallel verify+write path.

### 4.2 Threshold-trip fixture

PIP-10.a adds a threshold-trip fixture to the source tree created by
`setup_comprehensive_src` (or an extension function called after it):

```sh
# Create 120 files to trip the file-count threshold (historical
# PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100 from PIP-3; the
# threshold is gone but the fixture stays as defensive coverage).
mkdir -p "$dir/parallel_threshold"
for i in $(seq 1 120); do
  printf 'pt-payload-%03d\n' "$i" > "$dir/parallel_threshold/file_${i}.txt"
done

# Create a large file to trip per-file chunk batching. The file
# must be large enough that the delta token stream produces more
# than DEFAULT_PER_FILE_REORDER_CAPACITY (64) chunks. With the
# default block size of 700 bytes (for files < 512 KiB), a 256 KiB
# file produces ~375 blocks, well above the 64-chunk threshold.
dd if=/dev/urandom of="$dir/parallel_threshold/large_delta.bin" \
   bs=1K count=256 2>/dev/null
```

The `parallel-threshold` verification type asserts:

1. Every file in `parallel_threshold/` has a sha256 match between
   source and destination.
2. File count in `parallel_threshold/` matches between source and
   destination.
3. `file_1.txt` gets an explicit sha256 check (the PIP-7 corruption
   manifested on the first dispatched file specifically).
4. `large_delta.bin` gets an explicit sha256 check (exercises the
   multi-chunk batch path).

### 4.3 Delta update threshold trip

A second pass through the same fixture with modifications exercises the
delta update path (as opposed to initial sync):

```sh
# Mutate a subset of files to force delta transfers on the second sync.
for i in 1 50 100 120; do
  printf 'pt-payload-%03d-updated\n' "$i" > "$dir/parallel_threshold/file_${i}.txt"
done
# Append to the large file to produce a delta rather than whole-file.
dd if=/dev/urandom of="$dir/parallel_threshold/large_delta.bin" \
   bs=1K count=32 seek=256 2>/dev/null
```

After the second sync, all 120 files plus `large_delta.bin` must
sha256-match between source and destination.

## 5. File type coverage

The comprehensive source tree created by `setup_comprehensive_src`
(lines 892-915 of `run_interop.sh`) already includes:

| Type | Example | Interop scenarios exercising it |
| --- | --- | --- |
| Regular files | `hello.txt`, `binary.dat`, `large.dat` | All basic/delta/compress/checksum |
| Directories | `subdir/`, `subdir/nested/`, `empty_dir/` | All recursive |
| Symlinks | `link.txt -> hello.txt` | symlinks, safe-links, copy-links |
| Hardlinks | `hardlink.txt` (same inode as `hello.txt`) | hardlinks, hardlinks-* |
| Cross-dir hardlinks | `subdir/crossdir_link.txt` | hardlinks-inc-recursive |
| Executable scripts | `script.sh` (mode 755) | permissions |
| Empty files | `empty.txt` | basic |

### 5.1 Devices and FIFOs

The `devices` scenario (`-avD`) exercises device and special file
transfer. The harness does not create device nodes (requires root) but
does create FIFOs when running as root. On CI runners (non-root), the
`devices` scenario exercises the `-D` flag path without actual device
transfers - this matches the existing interop coverage.

PIP-10.a does not extend device/FIFO coverage beyond the existing
harness. The parallel path dispatches devices/FIFOs through the same
`apply_delta_tokens_parallel` / `apply_delta_tokens` cutover as regular
files; there is no device-specific parallel code path.

### 5.2 Sparse files

The `sparse` scenario (`-avS`) creates a sparse test file inside the
source tree. The parallel path preserves sparse semantics through the
same `sparse_state` threading documented in PIP-9.b.2 section 2 - the
sparse handler stays on the commit thread. The sha256 check on the
destination file validates byte-content parity; the sparse-on-disk
property is a filesystem concern outside interop scope.

## 6. Wire-byte parity: sequential vs parallel output comparison

### 6.1 Approach

The strongest correctness assertion is that the parallel path produces
byte-identical destination files compared to the sequential path for the
same source tree and upstream peer. PIP-10.a implements this via a
dual-run comparison:

1. **Run A (sequential):** build oc-rsync without
   `--features parallel-receive-delta` (default build). Run the full
   interop matrix. After each completed version, compute a recursive
   sha256 manifest of the destination tree:
   ```sh
   find "$dest" -type f -exec sha256sum {} + | sort > "$manifest_seq"
   ```

2. **Run B (parallel):** build oc-rsync with
   `--features parallel-receive-delta`. Run the same interop matrix
   against the same upstream binaries and the same source tree. Compute
   the same sha256 manifest:
   ```sh
   find "$dest" -type f -exec sha256sum {} + | sort > "$manifest_par"
   ```

3. **Compare:** `diff "$manifest_seq" "$manifest_par"` must produce
   zero output. Any divergence is a correctness failure that blocks
   PIP-9.f.

### 6.2 Implementation site

The dual-run comparison is implemented as a new harness script
`tools/ci/run_parallel_parity.sh` that:

- Takes the sequential and parallel oc-rsync binaries as arguments.
- Runs both binaries against each upstream version in sequence (not in
  parallel - avoids port/daemon contention).
- Captures per-version sha256 manifests.
- Emits a pass/fail verdict per version.
- Exits non-zero on any divergence.

The CI workflow calls this script after both binaries are built.

### 6.3 Scope limitation

The sha256 comparison covers file content only. Metadata (permissions,
timestamps, ownership, xattrs, ACLs) are verified by the existing
`comp_verify_*` functions in the interop harness and are assumed
identical between sequential and parallel builds since the parallel
path only affects the delta-token application loop, not the post-file
metadata application at `sync.rs:255-374`.

## 7. CI integration

### 7.1 Existing infrastructure

Two CI cells already exercise the parallel path:

- **`ci.yml:596` - `parallel-receive-delta-dist`:** builds with
  `--profile dist --features parallel-receive-delta`, runs the
  `parallel_threshold` nextest filter. Non-required. Does not run the
  full interop harness.

- **`_interop.yml:555` - `interop-parallel-receive-delta`:** builds with
  `--profile dist --features parallel-receive-delta`, runs the full
  `tools/ci/run_interop.sh` harness. Non-required (`continue-on-error:
  true`). Emits a per-version summary table.

### 7.2 PIP-10.a extensions

PIP-10.a extends the existing `interop-parallel-receive-delta` job in
`_interop.yml` rather than creating a new workflow. Changes:

1. **Add threshold-trip fixture** to `setup_comprehensive_src` or a
   post-setup hook in `run_interop.sh`. The fixture is always created
   but the `parallel-threshold-trip` scenario entry is only added to
   the scenario list when the `PIP10A_PARALLEL_INTEROP` environment
   variable is set (avoiding regression risk to the default sequential
   run).

2. **Add sha256 parity step** after the main interop run. The
   `interop-parallel-receive-delta` job already builds the parallel
   binary; PIP-10.a adds a preceding step that builds the sequential
   binary, then runs `tools/ci/run_parallel_parity.sh` with both
   binaries.

3. **Promote per-version summary** to a structured output. The existing
   summary step at `_interop.yml:654-680` is extended to include:
   - Per-scenario pass/fail (not just per-version).
   - Threshold-trip explicit verdict.
   - Parity-check explicit verdict.

4. **Upload wire captures** on failure. When a scenario fails, the
   harness captures the oc-rsync daemon log and the transfer log as
   CI artifacts for post-mortem analysis.

### 7.3 Job dependency graph

```
build (existing)
  |
  +-> interop (existing, sequential)
  |
  +-> interop-parallel-receive-delta (extended by PIP-10.a)
        |
        +-- step: Build sequential binary (default features)
        +-- step: Build parallel binary (--features parallel-receive-delta)
        +-- step: Run parallel interop matrix (run_interop.sh)
        +-- step: Run sha256 parity comparison (run_parallel_parity.sh)
        +-- step: Emit per-version + per-scenario summary
        +-- step: Upload logs/captures on failure
```

### 7.4 Promotion path

The `interop-parallel-receive-delta` job stays non-required
(`continue-on-error: true`) during the PIP-9.f bake window. Once the
bake criterion (PIP-9.f.1) is satisfied - 5 consecutive green
nightlies across all gates - the job is promoted to required by
removing `continue-on-error: true`. This promotion is part of the
PIP-9.f.2 Cargo.toml flip PR.

## 8. Harness implementation details

### 8.1 Parallel-threshold verification function

A new `comp_verify_parallel_threshold` function in `run_interop.sh`:

```sh
comp_verify_parallel_threshold() {
  local s=$1 d=$2
  # 1. File count match
  local sc dc
  sc=$(find "$s/parallel_threshold" -type f | wc -l)
  dc=$(find "$d/parallel_threshold" -type f | wc -l)
  if [[ "$sc" != "$dc" ]]; then
    echo "    parallel_threshold file count: src=$sc dst=$dc"
    return 1
  fi
  # 2. Per-file sha256 sweep
  local fail=0
  while IFS= read -r f; do
    local rel="${f#$s/}"
    if [[ ! -f "$d/$rel" ]]; then
      echo "    Missing: $rel"
      fail=1
      continue
    fi
    local sh dh
    sh=$(sha256sum "$f" | cut -d' ' -f1)
    dh=$(sha256sum "$d/$rel" | cut -d' ' -f1)
    if [[ "$sh" != "$dh" ]]; then
      echo "    sha256 mismatch: $rel"
      echo "      src: $sh"
      echo "      dst: $dh"
      fail=1
    fi
  done < <(find "$s/parallel_threshold" -type f | sort)
  return $fail
}
```

### 8.2 Delta update scenario

The `parallel-threshold-trip` scenario runs two passes:

1. **Initial sync:** standard `comp_run_scenario` with `-av` flags.
   Verify via `comp_verify_parallel_threshold`.
2. **Delta update:** mutate source files per Section 4.3, re-run
   `comp_run_scenario` with `-av --no-whole-file -I` flags. Verify
   via `comp_verify_parallel_threshold`.

The delta update pass exercises the `BlockRef` token resolution path
in the parallel arm, which is the highest-risk code path (basis-map
reads on the receive thread, chunk dispatch to the applier, per-file
reorder buffer drainage).

### 8.3 Environment variable gating

The threshold-trip scenario and parity check are gated behind
`PIP10A_PARALLEL_INTEROP=1` to avoid impacting the default sequential
interop run. The `interop-parallel-receive-delta` job in `_interop.yml`
sets this variable in its `env` block:

```yaml
env:
  PIP10A_PARALLEL_INTEROP: "1"
```

## 9. Success criteria

PIP-10.a is complete when all of the following hold:

1. **100% pass rate** across all cells in the parallel-interop matrix
   (Section 2.4, ~324 cells). Zero unexpected failures. Known
   limitations tracked in the `is_known_failure` table are excluded
   from the pass-rate calculation.

2. **sha256 parity** between sequential and parallel builds for every
   upstream version. Zero divergences in the file-content manifests
   (Section 6).

3. **Threshold-trip scenario green** for all 4 versions x 2 directions.
   `file_1.txt` and `large_delta.bin` sha256 checks pass. Delta update
   pass succeeds.

4. **CI job green for 5 consecutive nightly runs** per the PIP-9.f.1
   bake criterion (Section 3 of
   `docs/design/pip-9-f-1-bake-criterion.md`).

5. **No regression in sequential interop.** The default-build interop
   job must remain green after PIP-10.a harness changes land.

## 10. Risk catalogue

| Risk | Impact | Mitigation |
| --- | --- | --- |
| R1: PIP-7 corruption resurfaces under dist+parallel | Silent data corruption | sha256 parity check (Section 6), file_1.txt explicit check (Section 4.2) |
| R2: Parallel path reorders chunks for large files | Wrong file content | large_delta.bin 256 KiB fixture (Section 4.2), per-file ReorderBuffer in applier |
| R3: Delta update produces wrong basis resolution | Partial corruption on modified files | Delta update pass (Section 4.3) with sha256 sweep |
| R4: Interop harness port contention between seq/par runs | Flaky CI failures | Sequential (not parallel) dual-run in parity script (Section 6.2) |
| R5: Extended scenarios timeout on parallel build | CI timeout, incomplete coverage | 45-minute job timeout matches existing interop cell; parallel build is ~same speed as default |
| R6: Threshold-trip fixture pollutes other scenarios | Unexpected file count in basic verify | Isolated `parallel_threshold/` subdirectory, excluded from `comp_verify_transfer` |

## 11. Implementation punch list

| Task | Deliverable | Depends on |
| --- | --- | --- |
| PIP-10.a.1 | Add `comp_verify_parallel_threshold` to `run_interop.sh` | - |
| PIP-10.a.2 | Add threshold-trip fixture to `setup_comprehensive_src` | PIP-10.a.1 |
| PIP-10.a.3 | Add `parallel-threshold-trip` scenario entry (gated on `PIP10A_PARALLEL_INTEROP`) | PIP-10.a.1, PIP-10.a.2 |
| PIP-10.a.4 | Add delta update pass to threshold-trip scenario | PIP-10.a.3 |
| PIP-10.a.5 | Create `tools/ci/run_parallel_parity.sh` | - |
| PIP-10.a.6 | Extend `interop-parallel-receive-delta` job in `_interop.yml` with parity step | PIP-10.a.5 |
| PIP-10.a.7 | Extend per-version summary with per-scenario and parity verdicts | PIP-10.a.6 |
| PIP-10.a.8 | Validate 5 consecutive green nightlies | PIP-10.a.1-7 |

## 12. References

### Code citations

- `tools/ci/run_interop.sh:55` - version array.
- `tools/ci/run_interop.sh:892-915` - `setup_comprehensive_src`.
- `tools/ci/run_interop.sh:1028` - `comp_run_scenario`.
- `tools/ci/run_interop.sh:9623` - `run_comprehensive_interop_case`.
- `tools/ci/run_interop.sh:9657-9747` - scenario list.
- `tools/ci/run_interop.sh:9673-9676` - parallel-threshold removal note.
- `.github/workflows/_interop.yml:555-692` - `interop-parallel-receive-delta`.
- `.github/workflows/ci.yml:586-633` - `parallel-receive-delta-dist`.
- `crates/transfer/src/receiver/transfer/sync.rs:241-253` - cutover site.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` - `ParallelDeltaApplier`.
- `Cargo.toml:72-76` - workspace `parallel-receive-delta` feature.

### Design documents

- `docs/design/pip-9-parallel-receive-wireup.md` - PIP-9 design.
- `docs/design/pip-9b2-cfg-dispatch-sketch.md` - cfg-gated dispatch.
- `docs/design/pip-9-b-3-parallel-arm-feed-loop.md` - feed loop spec.
- `docs/design/pip-9-f-1-bake-criterion.md` - bake window.
- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md` - PIP-7.
- `docs/design/parallel-receive-delta-application.md` - umbrella design.

### Related PRs

- PIP-9.b.2 (PR #4776) - cfg-gated dispatch sketch.
- PIP-8 (#4731) - dead scaffolding teardown.
- PIP-7 (#4730, #4725) - corruption investigation and mitigation.
- PIP-4 (#4720) - parallel-threshold-trip scenario.
- PIP-3+5 (#4666) - original default-on flip (reverted).
