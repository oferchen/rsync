# WDF - Wire differential fuzzing

Design note for the outcome-based differential fuzzing harness that compares
oc-rsync transfer results against upstream rsync.

## 1. Problem statement

The existing 15+ fuzz targets in `fuzz/fuzz_targets/` exercise parser-level
robustness: they feed arbitrary bytes into protocol decoders, filter parsers,
and checksum implementations to verify they do not crash. The
`filter_differential` target goes further by comparing filter verdicts against
upstream rsync for the same rule chain.

None of these targets verify that oc-rsync produces the same *transfer outcome*
as upstream rsync for the same input. A file could be transferred with correct
checksums, valid wire framing, and passing filter rules, yet still diverge in
final destination content because of a bug in the delta-apply pipeline, the
`--inplace` path, or the quick-check algorithm. This gap is what the WDF series
addresses.

## 2. Approach

### WDF-1: Coverage audit (completed)

Inventoried existing fuzz targets and confirmed the outcome-comparison gap.

### WDF-2/3: Outcome-based differential target (this work)

A new fuzz target `differential_outcome` that:

1. Takes structured input via `arbitrary::Arbitrary`:
   - Source file content (`Vec<u8>`, capped at 64 KiB)
   - File name (sanitised to safe ASCII)
   - A subset of deterministic rsync flags
   - Pre-existing destination state (empty, different content, identical)

2. Creates two identical transfer scenarios:
   - `src/` -> `dst-oc/` via oc-rsync
   - `src/` -> `dst-upstream/` via upstream rsync

3. Compares outcomes:
   - Exit code: both succeed or both fail (normalised to success/failure)
   - Destination file content: byte-for-byte equality

4. Reports divergences as panics (libFuzzer crash artifacts).

### WDF-4: Frame-level differential (future)

Capture raw wire bytes from both implementations during a daemon-mode transfer
and compare frame-by-frame. Requires the protocol capture/replay harness from
`docs/design/protocol-capture-replay-harness.md`.

### WDF-5: File-list-level differential (future)

Compare the serialised file list entries between oc-rsync and upstream rsync
for the same source tree. Requires structured file-list extraction from both
implementations.

## 3. Input structure

```rust
struct DifferentialInput {
    content: Vec<u8>,        // source file content (capped at 64 KiB)
    name_bytes: Vec<u8>,     // raw bytes, sanitised to safe ASCII name
    flags: FuzzFlags,        // subset of safe rsync flags
    dest_state: DestinationState,  // empty, different, or identical
}
```

The `FuzzFlags` struct independently toggles:
- `--checksum` - force checksum-based transfer decisions
- `--whole-file` - disable delta-transfer algorithm
- `--inplace` - update files in place
- `--ignore-existing` / `--ignore-non-existing` - skip logic
- `--size-only` - size-based skip logic
- `--sparse` - sparse file handling

All invocations include `--no-times` and `--no-perms` to suppress
platform-dependent divergences in timestamp precision and permission bits.

## 4. Binary discovery

Both binaries are located via environment variables with fallback chains:

- **oc-rsync**: `$OC_RSYNC_BIN` -> `target/release/oc-rsync` -> `target/debug/oc-rsync`
- **upstream rsync**: `$UPSTREAM_RSYNC` -> interop installs -> system paths

When either binary is unavailable, the iteration exits cleanly (no crash).

## 5. Known-acceptable divergences

The harness suppresses these known divergence sources:

| Source | Mitigation |
|--------|-----------|
| Timestamp precision | `--no-times` suppresses mtime comparison |
| Permission bits | `--no-perms` suppresses mode comparison |
| Quick-check mtime race | Destination files backdated 2 hours |
| `--inplace --sparse` | Skipped (upstream rejects on older versions) |
| `--ignore-existing` + `--ignore-non-existing` | Skipped (undefined) |
| Embedded NUL / `/` in names | Rejected by name sanitiser |
| File content > 64 KiB | Capped to keep iteration speed reasonable |

## 6. Timeout handling

Each child process is given a 10-second timeout. If either binary exceeds this,
the iteration is skipped cleanly. The timeout prevents the fuzzer from hanging
on pathological inputs that trigger interactive prompts, infinite retry loops,
or extremely slow transfers.

## 7. Throughput expectations

Each iteration spawns two child processes. Expected throughput: 50-200 exec/sec
depending on hardware and file sizes. This is two orders of magnitude slower
than in-process targets, but the goal is correctness, not raw coverage rate.

## 8. Limitations

- **Local-copy mode only.** No daemon or SSH transport is exercised. The
  transfer pipeline and file-handling logic are the focus; network-layer bugs
  require a separate harness (WDF-4).

- **Single file per iteration.** Multi-file transfers with directory
  hierarchies, hardlinks, and symlinks are not covered. Extending to multi-file
  inputs is a natural next step but increases iteration cost significantly.

- **No recursive mode.** The harness transfers a single flat file. Recursive
  transfers with filter chains are partially covered by `filter_differential`.

- **No ACL/xattr/device comparison.** These require root privileges or
  platform-specific setup. Covered separately by the interop test suite.

- **Child process overhead.** The `fork`+`exec` cost dominates iteration time.
  An in-process approach (linking oc-rsync as a library) would be faster but
  requires significant refactoring of the entry point.

## 9. Future expansion

1. **Multi-file inputs (WDF-3b):** Generate a small directory tree (2-5 files)
   with varying sizes and compare the full destination tree.

2. **Delta-transfer stress (WDF-3c):** Pre-populate the destination with a
   mutated version of the source to exercise the delta pipeline (matching
   blocks, insertions, tail data).

3. **Frame-level capture (WDF-4):** Run both binaries in daemon mode with
   packet capture, compare wire frames.

4. **File-list comparison (WDF-5):** Extract and compare serialised file-list
   entries.

5. **Symlink and hardlink coverage:** Add these to the input structure once
   the single-file harness is stable.
