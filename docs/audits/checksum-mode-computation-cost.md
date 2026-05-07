# Checksum-mode computation cost audit

Tracks task #1041. Profiles the per-file cost of `--checksum` (`-c`) mode in
oc-rsync, compares it to upstream rsync 3.4.1, and proposes wire-compatible
reductions.

## 1. Flow of `--checksum` (`-c`) mode

In rsync, `-c` switches the receiver's quick-check from mtime+size to a content
hash comparison. Concretely:

- The sender hashes every regular source file end-to-end while building the
  file list, and embeds the digest in the per-entry flist trailer.
- The receiver, when stat'ing each destination candidate, hashes the local
  file and compares the leading bytes of the digest against the sender's
  payload. A mismatch (or missing destination) flips the entry into the
  delta-transfer pipeline; a match skips the file entirely.

The relevant code paths in oc-rsync:

| Stage                        | File                                                                                  |
| ---------------------------- | ------------------------------------------------------------------------------------- |
| Flag wiring (CLI -> config)  | `crates/core/src/client/run/batch.rs:95`, `crates/core/src/client/remote/batch_support.rs:45` |
| Sender flist writer toggle   | `crates/transfer/src/generator/mod.rs:551-560`                                        |
| Wire emission of digest      | `crates/protocol/src/flist/write/encoding.rs:260-304` (`write_checksum`)              |
| Receiver candidate filter    | `crates/transfer/src/receiver/transfer/candidates.rs:111-152`                         |
| Receiver per-file hash check | `crates/transfer/src/receiver/quick_check.rs:225-250` (`file_checksum_matches`)       |
| Local-copy fast path         | `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:88-242`         |
| Algorithm dispatch           | `crates/checksums/src/strong/{md4,md5,xxhash}.rs`, `crates/checksums/src/parallel/files.rs` |

Two subsystems exercise the heavy work. The `transfer` crate handles wire mode
(SSH, daemon, server-spawn). The `engine` crate's local-copy executor owns the
fully local `oc-rsync src/ dst/` path and short-circuits the protocol entirely.

The negotiated digest length comes from `ChecksumFactory::from_negotiation`
which respects compat flags and the SAFE_FLIST handshake, then is stamped into
the writer with `with_always_checksum(factory.digest_length())`. The receiver
re-reads the same byte count via the matching `FileListReader` configuration.

## 2. Per-file cost model

### Sender side

Upstream signals "I know each file's content hash" before any delta logic
fires. oc-rsync's wire layer reserves space for the digest in
`write_checksum`, but the populating call site that mirrors upstream
`flist.c:1412 file_checksum(thisname, &st, tmp_sum)` is currently absent in
the sender flist builder (`crates/transfer/src/generator/file_list/entry.rs`):
no caller invokes `FileEntry::set_checksum`. The wire writer therefore emits
zeroed digests when `always_checksum` is enabled but the entry has no payload
(`crates/protocol/src/flist/write/encoding.rs:295-297`). This is a behavioural
gap that masks part of the expected sender-side cost: in upstream, every
regular file is opened and hashed once during `make_file()`; in oc-rsync the
sender currently performs the open+hash work only as part of the later delta
phase, not as part of flist construction.

The latent cost when the sender path is finished is upstream's:

- One `open()` + `mmap()` per regular file (`checksum.c:415`,
  `map_file(fd, len, MAX_MAP_SIZE, CHUNK_SIZE)`).
- One full pass through the file feeding `EVP_DigestUpdate` (OpenSSL build) or
  the chosen `XXH3*_update` / `md5_update` / `md4_update` loop in
  `CHUNK_SIZE` (256 KiB) increments.
- One digest finalize plus zero-padding into a `file_sum_len`-byte slot.

The work scales with the total regular-file byte count, not file count, but
the per-file fixed overhead (open, mmap setup, digest init/finalize) dominates
for trees with many small files.

### Receiver side

Receiver cost is exercised today via `quick_check_matches` ->
`file_checksum_matches`:

- `fs::File::open(path)` per candidate.
- A 64 KiB stack buffer (`let mut buf = [0u8; 64 * 1024];`) reused across
  reads.
- `read_exact` loop until `remaining == 0`, feeding `ChecksumVerifier::update`.
- `finalize_into` writes up to `MAX_DIGEST_LEN` bytes; the comparison length
  is the minimum of `expected.len()` and the digest length, mirroring
  `flist_csum_len`.

This buffer is half the size of upstream's 128 KiB `CHUNK_SIZE` and not
reused across files (it is stack-allocated inside the function and re-zeroed
on each entry). On large trees this adds two penalties: more `read()`
syscalls per file and no chance for SIMD batch hashing to amortise the digest
state setup.

The local-copy executor takes a different route via
`parallel_checksum::prefetch_checksums`. It collects `FilePair`s, fans them
out with `rayon::par_iter`, and inside each lane runs `rayon::join` on the
source and destination so the two reads progress in parallel. The buffer
comes from `BufferPool` (`crates/engine/src/local_copy/buffer_pool/`), so
allocation pressure across files is eliminated. Two notable shortfalls:

- Each call site builds a fresh `Md5` / `Md4` / `Xxh3` hasher instance per
  file. The `simd_batch` MD5/MD4 dispatcher (`crates/checksums/src/simd_batch/`)
  is not wired here, so AVX2/NEON 4-, 8-, and 16-lane batch paths are unused
  for the workload that benefits from them most.
- The `pipelined::DoubleBufferedReader`
  (`crates/checksums/src/pipelined/`) is wired into transfer-stage signatures
  but not into `--checksum` quick-check hashing, leaving the documented
  20-40% I/O-overlap win unrealised.

## 3. Comparison with upstream rsync 3.4.1

`target/interop/upstream-src/rsync-3.4.1/checksum.c:402` implements
`file_checksum`. Salient differences:

- **Single mmap'd pass**. Upstream calls `map_file(fd, len, MAX_MAP_SIZE,
  CHUNK_SIZE)` and walks the file with `map_ptr(buf, i, CHUNK_SIZE)`. There is
  no userspace buffer; the kernel paginates as `EVP_DigestUpdate` /
  `XXH3_*_update` consume the slice. oc-rsync's receiver path uses a 64 KiB
  read buffer, which costs an extra `read()` per chunk and copies into
  userspace before hashing.
- **Static digest contexts**. Upstream allocates `XXH3_state_t` /
  `XXH64_state_t` with `static` storage and resets them per file. oc-rsync
  builds a fresh `ChecksumVerifier` each call, paying the algorithm dispatch
  on every file.
- **OpenSSL EVP fast path**. Upstream prefers `file_sum_evp_md` when
  `USE_OPENSSL` is set, picking up CPU-accelerated MD5 / SHA implementations
  (`AES-NI`, `SHA-NI`). oc-rsync routes through pure-Rust `Md5`, with
  `openssl_support` available in `crates/checksums/src/strong/openssl_support.rs`
  but not engaged from the `--checksum` path.
- **Sender-time hashing**. `flist.c:1412` performs `file_checksum` while
  building each entry inside `make_file`. oc-rsync's sender flist builder does
  not currently call any equivalent, so wire-mode pulls fall back to zeroed
  digests on the wire and force the receiver into the delta path even when
  files match. Upstream amortises the cost by hashing once on the sender and
  once on the receiver; oc-rsync only pays the receiver half.

## 4. Proposed reductions

The four items below are wire-compatible. Each is sized in 1-2 sprint units of
work and avoids new protocol bits.

### 4.1 Reuse `ChecksumCache` across the receiver path

Move the `parallel_checksum::ChecksumCache` from the local-copy executor into
a shared receiver helper invoked from
`receiver/transfer/candidates.rs`. The current `--checksum` candidate filter
hashes files sequentially inside `quick_check_matches`. Replacing the inner
call with a rayon-driven prefetch (gated by the existing
`PARALLEL_STAT_THRESHOLD`-style threshold) gives near-linear core scaling on
trees of small/medium files. Side-effect-free: the digest comparison is a
trailing prefix match that does not change.

### 4.2 Engage `simd_batch::digest_batch` for MD5/MD4

Both upstream-default `MD4` (proto < 30) and the negotiated `MD5` path can
feed the AVX-512/AVX2/NEON dispatcher in `crates/checksums/src/simd_batch/`.
Two wiring points:

- `parallel_checksum::hash_file_contents` (engine path) when files fit in
  RAM (the `max_memory_file_size` branch already buffers the entire file).
- A new batch entry point used by the receiver to hash a window of small
  files together. This is the single biggest CPU lever: SIMD MD5 reaches
  4-16 lanes per core depending on the host.

### 4.3 mmap-first hashing on the receiver

`crates/checksums/src/parallel/files.rs:36-45` already shows the pattern:
small files via `read_to_end`, mid-size files via `BufReader`, large files
via `MmapReader::open` with `advise_sequential`. Port the same tier to
`receiver/quick_check.rs::file_checksum_matches` and the engine path so the
receiver matches upstream's `map_file` strategy. This eliminates the 64 KiB
chunked `read` loop and gives the kernel a chance to populate the page cache
ahead of the hashing pointer.

### 4.4 Hardware-accelerated digest backends

Wire `crates/checksums/src/strong/openssl_support.rs` into the
`ChecksumVerifier` factory used by `quick_check_matches` and
`parallel_checksum::hash_file_contents` so AES-NI / SHA-NI / ARMv8 Crypto
Extensions kick in when available. The `simd_batch` module covers MD4/MD5
algorithmic SIMD, but hardware extensions for SHA1/SHA256 (which arrive when
`--checksum-choice=sha1` is negotiated) only land if EVP is engaged.
Combine this with detection cached via `OnceLock` to avoid per-file cost.

### 4.5 Pipeline I/O with hashing

Adopt `pipelined::DoubleBufferedReader` from
`crates/checksums/src/pipelined/` for the receiver's per-file hash. The
sequential `read -> hash` pattern in `file_checksum_matches` leaves the CPU
idle whenever the kernel is satisfying a `read()`; the double-buffered reader
keeps a producer thread filling buffer A while the digest consumes buffer B
and flips them on EOF of each chunk. The crate's own commentary documents
20-40% throughput improvements for CPU-heavy hashes (MD4, MD5, SHA1).
Particularly valuable for SSH transfers where the receiver is otherwise
waiting on the network.

### Sequencing and risk

- 4.1 and 4.5 are independent, both touch only the receiver hash routine, and
  can land in either order.
- 4.2 builds on 4.1's batch shape; landing 4.1 first lets the batch dispatcher
  see a window of files at once.
- 4.3 should land before 4.4: mmap'd input simplifies feeding EVP because the
  contiguous slice maps onto one `EVP_DigestUpdate` per CHUNK_SIZE without a
  bounce buffer.
- All five items preserve digest-byte equivalence with upstream and require no
  new compat flag, capability letter, or wire field.

## References

- `crates/transfer/src/receiver/quick_check.rs`
- `crates/transfer/src/receiver/transfer/candidates.rs`
- `crates/transfer/src/generator/mod.rs`
- `crates/protocol/src/flist/write/encoding.rs`
- `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs`
- `crates/checksums/src/parallel/files.rs`
- `crates/checksums/src/simd_batch/mod.rs`
- `crates/checksums/src/pipelined/mod.rs`
- `target/interop/upstream-src/rsync-3.4.1/checksum.c`
- `target/interop/upstream-src/rsync-3.4.1/flist.c`
