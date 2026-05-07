# xxHash64 file-dedup heuristic (internal-only)

This note specifies an internal, in-memory pre-screen that skips the
signature/delta computation for files whose 64-bit content fingerprint
matches a sibling file already processed in the same `flist` sweep.
The heuristic is purely a local CPU optimization; it never alters wire
output, never appears on the CLI, and never participates in capability
negotiation.

This is a design note. No Rust code lands in this PR. Implementation
is tracked in #2102 / its follow-up; property tests in the same track;
benchmark scaffolding in the same track.

## Background and goal

When the receiver builds the file list, large transfers commonly contain
many files that are byte-identical (CI artefacts, container layer caches,
build outputs, CMS media replicas, log rotations of the same payload).
For each such file, the current pipeline computes a full block-level
signature and emits the signature on the wire so the sender can search
its source for matches. When the file is identical to a sibling that
already has a matching basis on the destination, that work is wasted:
the delta script ends up being a sequence of `COPY` tokens that we could
have predicted without a signature exchange at all.

The optimization is to compute a 64-bit fingerprint at flist build time
and bucket files by it. When a later file in the same sweep hashes to
the same bucket, has identical `size`, and identical `mtime` (whole
seconds plus nanoseconds), the receiver reuses the sibling's basis
instead of issuing a fresh signature pass on the candidate. Misses
cost a single 64-bit hash insert.

xxHash64 is the chosen hash:

- Non-cryptographic (collisions are tolerated; size+mtime gate validates).
- 5-10 GB/s on modern x86_64 / aarch64 cores with a single-pass scalar
  implementation. Well above any realistic disk read bandwidth.
- 64-bit output keeps the `HashMap` key compact (8 bytes) and matches
  the existing `xxh3` family already linked into `checksums`.
- Stable across architectures (no SIMD divergence concerns).

## 1. Internal-only positioning

The heuristic is invisible to upstream rsync. It MUST NOT touch any of
the layers governed by `protocol`, `signature`, or `core::client::setup`.

| Layer                                   | Change |
|-----------------------------------------|--------|
| Wire signature payload                  | none   |
| NDX framing / negative tokens           | none   |
| Multiplex `MSG_*` frames                | none   |
| `build_capability_string()`             | none   |
| Greeting / version negotiation          | none   |
| `crates/protocol/tests/golden/` bytes   | none   |
| `tools/ci/run_interop.sh` matrix output | none   |
| CLI flags                               | none   |

The only observable effect is reduced receiver CPU on workloads that
contain duplicate files. When the cache misses, behavior is byte-for-byte
identical to today's pipeline.

This positioning matches the user-feedback rule recorded in
`feedback_no_wire_protocol_features.md`: do not add wire-visible features
for niche performance wins. xxHash64-dedup is local-only.

## 2. Use case

Pre-screen identical files before signature/delta computation. Concrete
shapes:

1. **CI artefact mirrors.** A repository's `target/` or `dist/` tree
   commonly carries dozens of copies of the same `.so` / `.dll` /
   `.jar` across feature builds. Each copy currently pays a full
   signature pass.
2. **Container image layers.** Container registries store identical
   blobs under content-addressable names; a single layer flush can
   contain hundreds of equal files at different paths.
3. **Log rotation snapshots.** `app.log.1`, `app.log.2`, ... where the
   tail is rotated unchanged; many bytes overlap.
4. **Media-CDN replicas.** Pre-rendered thumbnails, asset hashes used
   under multiple URLs.

For all four shapes, the current behaviour is a per-file signature
pass on the receiver and a per-file basis search on the sender. With
the dedup heuristic, the second and later identical siblings inherit
the first's basis decision in O(1).

## 3. Hash composition

The fingerprint inputs are chosen to be:

- **Cheap.** No full-file read on cold caches.
- **Discriminating.** Two files with identical `(size, mtime)` and
  identical 4 KB head + tail are unlikely to collide unless the middle
  is also identical. The sibling-equality check (Section 5) catches
  the residual cases.
- **Path-independent.** Identical content under different names hashes
  the same.

### Inputs

```
xxh64_seed = 0
xxh64_input = LE(size_u64) || LE(mtime_secs_i64) || LE(mtime_nsec_u32)
            || head_bytes || tail_bytes

head_bytes = first min(4096, size) bytes of the file
tail_bytes = last  min(4096, size) bytes of the file
              (omitted when size <= 4096; in that case head_bytes already
               covers the full content)
```

For files of size `<= 4096`, the dedup hash degenerates to a hash over
the full content plus the size/mtime tuple, which is strictly stronger
than the larger-file probe.

### Why head + tail, not full file

A full-file scan defeats the latency goal: any duplicate-detection
strategy that reads the whole file end-to-end is no cheaper than just
running the existing block signature, which already streams the file
and produces a wire-usable artefact.

Head + tail (`8 KB` total) catches the overwhelming majority of practical
collisions in workloads where files share a magic number, header, and
trailer/footer (ELF, PE, ZIP, tar, image formats). The residual collision
risk is contained by:

1. The `(size_u64, mtime_secs_i64, mtime_nsec_u32)` prefix in the hash
   input.
2. The size+mtime equality gate at probe time (Section 5).
3. Deferring to the existing pipeline on miss, so a wrong-positive
   never produces wrong output, only wrong work.

### Why not a cryptographic hash

xxHash64 is chosen explicitly because the dedup gate does not need to
defend against an adversary. Misses fall back to the safe path. Using
SHA-256 would multiply CPU cost by ~10x while adding no robustness in
this position.

## 4. Implementation point

Computation lives at flist build time, in the receiver's flist sweep.
The sweep is the only point where the file is opened for `lstat`,
short-read, and metadata capture, so reading 4 KB head and 4 KB tail
adds two pread calls (or one read + one seek+read on platforms without
positional reads) and zero extra syscalls relative to the existing
`O_RDONLY` open during basis preparation.

### Insertion-point binding

| File                                      | Symbol                                        | Role                                                                 |
|-------------------------------------------|-----------------------------------------------|----------------------------------------------------------------------|
| `crates/flist/src/entry.rs`               | `FileListEntry`                               | Add private `dedup_fp: Option<u64>` (or carry in a sibling struct).  |
| `crates/flist/src/lazy_metadata.rs`       | metadata fill path                            | Call site for `compute_dedup_fp(...)` after metadata is captured.    |
| `crates/flist/src/file_list_walker.rs`    | walker emit step                              | Wire the fingerprint into the emitted entry.                         |
| `crates/checksums/src/xxh64_dedup.rs`     | `pub(crate) fn xxh64_file_dedup(...)`         | New helper; lives next to existing `xxh3` plumbing.                  |
| `crates/core/src/receiver/dedup_index.rs` | `DedupIndex` (new module, internal)           | `HashMap<u64, SmallVec<[FileNdx; 1]>>` populated during flist sweep. |
| `crates/core/src/receiver/...basis...`    | basis selection path                          | Probe `DedupIndex` before requesting a fresh signature.              |

The exact line bindings inside `lazy_metadata.rs` and the basis path are
finalized in the implementation PR. The constraints are:

- The fingerprint is computed on the **first** flist visit, never on a
  later sibling lookup. Lookups are pure reads.
- The fingerprint is only computed for **regular files**. Symlinks,
  directories, devices, FIFOs, and sockets are excluded by an early
  return; their `dedup_fp` is `None`.
- The fingerprint is not computed for files smaller than a threshold
  (default 1 byte, i.e. always for regular files) - the per-file
  syscall floor matters more than the hash cost.

### Compute helper shape

```
pub(crate) fn xxh64_file_dedup(
    size: u64,
    mtime_secs: i64,
    mtime_nsec: u32,
    head: &[u8],   // up to 4096 bytes from offset 0
    tail: &[u8],   // up to 4096 bytes from end (empty if size <= 4096)
) -> u64;
```

The helper is `pub(crate)` to `checksums` and re-exported under a
`pub(crate)` alias in `flist`. It is not in any public API surface.

### Read strategy

For each regular file at flist build:

1. Open `O_RDONLY` (already done by the existing metadata path on
   platforms that need it; reuse the same fd if possible).
2. `pread(fd, &mut head, 0)` for `min(4096, size)` bytes.
3. If `size > 4096`, `pread(fd, &mut tail, size - 4096)` for 4096 bytes.
4. Hash via `xxh64_file_dedup`.
5. Store the resulting `u64` into `FileListEntry::dedup_fp`.

On read failure, store `None` and continue. Failure here is never fatal:
the worst case is one extra signature pass.

## 5. Sibling-equality gate

The fingerprint alone is not authoritative. Before reusing a sibling's
basis decision, the receiver MUST also confirm:

```
size_a == size_b
&& mtime_secs_a == mtime_secs_b
&& mtime_nsec_a == mtime_nsec_b
```

This mirrors rsync's quick-check semantics (size + mtime), which the
upstream pipeline already trusts as evidence of identical content for
the purposes of skip decisions. The fingerprint adds a third
independent dimension, raising confidence well above quick-check alone.

### False-negative handling

If `mtime_secs` or `mtime_nsec` differs but the fingerprint matches,
**skip the optimization**. The candidate file may have the same content
under a different timestamp (e.g. `cp` without `-p`), but proving that
requires a full read - exactly what the heuristic exists to avoid. The
correct response is to fall through to the existing signature/delta
path, which handles the case correctly at its normal cost.

This is conservative on purpose:

- A false-positive deduplication would corrupt the destination by
  reusing the wrong basis.
- A missed deduplication just costs the existing signature pass, with
  no correctness consequence.

The asymmetry justifies the strict three-way gate (`fp` + `size` +
`mtime` exact match).

### False-positive handling

xxHash64 is non-cryptographic; collisions over a 64-bit space at
flist scales (`N <= 10^7` files) have an expected count of
`N(N-1) / 2^65 ~= 1.5e-6` for `N = 10^7`. In practice, the size+mtime
gate eliminates virtually all of these. To bound the residual risk:

- The implementation MAY perform a final byte-equality probe on the
  first 64 KB before reusing the basis. This costs one read on the
  candidate but bounds the false-positive impact to a verified prefix.
- If the prefix probe diverges, fall through to the signature path.

The prefix probe is opt-in behind a profile-driven decision, not a CLI
flag. The default position is to trust the `(fp, size, mtime)` triple.

## 6. Memory cost

Two layouts are considered. Both are internal-only.

### Layout A: per-entry `Option<u64>` field

- `FileListEntry { ..., dedup_fp: Option<u64> }`
- Cost: 16 bytes per `FileListEntry` (8 byte payload + 8 byte
  discriminant on 64-bit targets, with niche packing this can drop to
  9 bytes; in practice expect 16).
- Pros: zero indirection; the fingerprint travels with the entry; trivial
  to reason about lifetime.
- Cons: pays the cost on entries that never participate in dedup
  (symlinks, devices, etc.) unless gated by an early `Option::None`
  store.

### Layout B: side-table `HashMap<u64, Vec<FileNdx>>`

- `DedupIndex { buckets: HashMap<u64, SmallVec<[FileNdx; 1]>> }`
- Cost: 8 bytes per fingerprinted file (key) + 4-8 bytes per file index
  (value) + amortized HashMap overhead (~1.4x load factor target).
- Pros: only fingerprinted files pay; lookup is O(1) by fingerprint.
- Cons: an extra allocation; lifetime tied to the receiver session;
  needs a clear ownership story relative to the flist.

### Recommendation

The implementation lands **Layout A** (per-entry field) for the
fingerprint and **Layout B** (side-table) for the bucket index used at
probe time. The two layouts cooperate:

- Layout A keeps the fingerprint adjacent to the entry, cheap to clone
  and survive across the flist.
- Layout B accelerates the "is there a sibling with this fingerprint"
  question without scanning the flist linearly per probe.

For the canonical workload sizes:

| `N` (files) | Layout A bytes | Layout B bytes | Total bytes  |
|-------------|----------------|----------------|--------------|
| 1 K         | ~8 KB          | ~16 KB         | ~24 KB       |
| 100 K       | ~800 KB        | ~1.6 MB        | ~2.4 MB      |
| 10 M        | ~80 MB         | ~160 MB        | ~240 MB      |

At `N = 10 M`, the side-table dominates. If memory pressure becomes a
concern at extreme flist sizes, the side-table can be sharded by
`fp >> 32` and built lazily on first probe. The implementation PR
benches this and decides; no design freeze here.

## 7. Property-test contract

The dedup gate is **safety-side**, not performance-side. Its only
correctness invariant:

> For every pair of files `(a, b)` that the receiver chooses to dedup
> via this heuristic, the chosen basis MUST yield a transfer outcome
> byte-for-byte identical to the outcome obtained without the heuristic.

The strict three-way gate (`fp_a == fp_b && size_a == size_b &&
mtime_a == mtime_b`) is the proof obligation. Property tests in the
implementation PR exercise:

1. **Identical-file dedup.** Two files with identical content, size,
   and mtime. Asserts the dedup path is taken AND the destination
   matches the source byte-for-byte.
2. **Same-size, same-mtime, different-content.** Synthetic 64-bit
   collision via crafted head/tail. Asserts the prefix probe (Section
   5) catches it and falls through. (Without the prefix probe, this
   test is replaced by a "we accept this is a 64-bit collision" budget
   measurement.)
3. **Same-content, different-mtime.** Asserts dedup is **skipped** and
   the regular path runs.
4. **Symlink / directory / device.** Asserts `dedup_fp` is `None` and
   no probe is attempted.
5. **Read failure during head/tail capture.** Asserts the entry stores
   `None` and the regular path runs.
6. **End-to-end byte-equality.** Round-trip a directory containing
   intentional duplicates. Assert the receiver's output tree matches
   the source tree, both with and without the dedup feature gated by
   a cfg flag.

Property tests live in `crates/flist/tests/dedup_property.rs` and
`crates/core/tests/receiver_dedup_endtoend.rs`.

## 8. Wire-compat restatement

The dedup heuristic is purely in-memory. It MUST NOT alter any of the
following layers; each remains byte-identical to the pre-dedup baseline
and to upstream rsync 3.0.9 / 3.1.3 / 3.4.1.

1. **Signature payload.** When a sibling lookup hits, the receiver
   reuses the sibling's already-emitted signature decision. No new
   payload is constructed.
2. **NDX framing.** Sender-side block-index encoding is untouched. The
   dedup index gates only receiver-side request emission; the sender
   sees a sequence indistinguishable from the non-dedup case (because
   the receiver elects to re-emit identical bytes when needed).
3. **Capability negotiation.** `build_capability_string()` in
   `core/src/client/setup.rs` is untouched. No new flag.
4. **Protocol-32 handshake.** Greeting, version negotiation, multiplex
   `MSG_*` frames - none are touched.
5. **Golden bytes.** `crates/protocol/tests/golden/` byte-comparison
   tests pass unchanged.
6. **tcpdump replay.** `tcpdump`-captured application-layer payloads
   for an oc-rsync push to upstream daemon are byte-identical with
   dedup on vs off (interop verification under the implementation PR).
7. **CLI surface.** No new `clap` argument, no new env var that affects
   wire output. Internal toggles are cfg-gated benchmark scaffolding
   only.
8. **Interop matrix.** `tools/ci/run_interop.sh` against upstream
   3.0.9 / 3.1.3 / 3.4.1 produces zero new entries in
   `tools/ci/known_failures.conf`.

The only observable effect is reduced CPU and reduced fd churn on the
receiver during flist build / basis preparation, with the same delta
bytes flowing on the wire.

## 9. Bench scaffolding plan

Bench scaffolding is **cfg-gated**, never wired into the CLI.

### What gets measured

Three quantities determine whether the dedup is pulling its weight:

1. **Hit rate.** Of all files that produced a fingerprint, what
   fraction found a same-`(fp, size, mtime)` sibling in the flist?

       hit_rate = dedup_hits / fingerprinted_files

   Target: `>= 0.10` on workloads with intentional duplicates (CI
   artefacts, container layers). Below `0.01` across typical workloads:
   the dedup table adds memory without filtering enough; revert.

2. **End-to-end CPU delta.** Wall-clock time on `crates/flist` and
   `crates/core` micro-benches and on the existing `scripts/benchmark.sh`
   workloads, comparing dedup-on vs dedup-off. Target: `>= 5%`
   receiver CPU saved on workloads with `>= 10%` duplicate ratio.

3. **Memory ceiling.** Side-table bytes resident at peak, compared to
   `2 * N` byte budget at `N` files. Target: stay within 2x of the
   table-of-fingerprints lower bound.

### Counters and harness

Cfg-gated `AtomicU64` counters (`probes`, `hits`, `mtime_skips`,
`fp_misses`, `prefix_probe_failures`) live behind
`#[cfg(feature = "bench-dedup")]` on the receiver's `DedupIndex`. A
new bench at `crates/core/benches/dedup_hit_rate.rs` drives synthetic
flist sizes at `N` in `{1_000, 10_000, 100_000, 1_000_000}` and prints
`hit_rate / probes / wall_time_us / memory_bytes`.

The feature flag is **internal**: declared in `crates/core/Cargo.toml`
but never propagated to workspace `default-features`, the CLI `features`
table, or the daemon. CI does not run the bench. There is no way to
turn it on from a release `oc-rsync` invocation.

### Decision flow

1. Implementation PR lands `xxh64_file_dedup` and the `DedupIndex`
   wiring behind no feature gate; the dedup is always built and always
   probed.
2. Property-tests PR lands the proptest suite (no feature gate).
3. Bench PR lands the harness behind `bench-dedup`.
4. Bench results feed a decision record. If hit rate is `>= 0.10` on
   duplicate-heavy workloads AND CPU savings `>= 5%`, the
   implementation stays. If either fails, the implementation is
   reverted and the design moves to "shelved" alongside
   `parallel_chunks_design.md`.
5. The `bench-dedup` feature flag is **deleted** at cleanup regardless
   of outcome. No feature flag survives into release builds.

## References

### xxHash

- xxHash64 specification: <https://github.com/Cyan4973/xxHash>
- `xxh3` family already linked into oc-rsync via the `checksums` crate
  (used for protocol-32 strong checksum negotiation).

### oc-rsync source

- `crates/flist/src/entry.rs` - `FileListEntry` field declaration site
  for `dedup_fp`.
- `crates/flist/src/lazy_metadata.rs` - metadata fill path; insertion
  point for the fingerprint computation.
- `crates/flist/src/file_list_walker.rs` - walker emit step that wires
  the fingerprint into the emitted entry.
- `crates/checksums/src/` - existing home of `xxh3` plumbing; the new
  `xxh64_file_dedup` helper lives alongside it.
- `crates/core/src/receiver/` - basis selection path; insertion point
  for `DedupIndex` probes.

### Wire-compat parity guards

- `crates/checksums/tests/rolling_simd_parity.rs` - SIMD-vs-scalar
  parity proptest (must stay green).
- `crates/protocol/tests/golden/` - wire-format byte goldens (must stay
  green).
- `tools/ci/run_interop.sh` - upstream 3.0.9 / 3.1.3 / 3.4.1 matrix
  (must stay green).

### Related design notes

- `docs/design/zsync-inspired-matching.md` - sister optimization track
  on the matching index; same wire-compat invariants apply.
- `docs/design/zsync-bithash.md` - same template for an internal-only
  receiver-side optimization.

### Tracking

- #2102 - this design note (this PR)
- Implementation, property-tests, and bench-scaffolding PRs follow
  under the same task track.
