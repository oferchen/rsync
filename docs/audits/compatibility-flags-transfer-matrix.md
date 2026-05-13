# Compatibility-Flags Transfer Matrix (#2106)

Exhaustive audit of 16 CLI flag families that affect transfer behaviour,
mapping each to its wire protocol impact, protocol version requirements,
mutual exclusions, oc-rsync implementation status, known divergences,
and interop test coverage against upstream rsync 3.4.1.

## Sources Consulted

- Upstream rsync 3.4.1: `compat.c` (setup_protocol, set_allow_inc_recurse),
  `options.c` (server_options, parse_arguments), `main.c`, `generator.c`,
  `receiver.c`, `token.c`, `match.c`, `io.c`.
- `crates/protocol/src/compatibility/{flags.rs,known.rs}` - CF_* definitions.
- `crates/transfer/src/setup/{mod.rs,capability.rs,restrictions.rs,types.rs}` -
  protocol setup and restrictions.
- `crates/core/src/client/config/` - config builder with mutual exclusions.
- `crates/core/src/client/remote/flags.rs` - server flag string builder.
- `crates/engine/src/` - delta pipeline, sparse writer, inplace writer.
- `tools/ci/run_interop.sh` - interop test harness.

## 1. Summary Matrix

| Flag | Wire impact | Proto min | Mutual exclusions | oc-rsync status | Divergences | Interop coverage |
|------|-------------|:---------:|-------------------|:---------------:|:-----------:|:----------------:|
| `--checksum` (`-c`) | Whole-file checksum in flist extras; seed ordering via `CF_CHKSUM_SEED_FIX`; algorithm via vstring | 28 | none | Implemented | none | `checksum-content`, `checksum-skip`, `hardlinks-checksum` |
| `--inplace` | Writes directly to dest file; `CF_INPLACE_PARTIAL_DIR` enables coexistence with `--partial-dir` | 28; 29 with basis dirs | `--partial-dir` (without CF bit 6), `--delay-updates` | Implemented | none | `inplace` |
| `--partial` | No wire change - receiver-local option | 28 | none | Implemented | none | `partial-dir` (exercises partial path) |
| `--partial-dir=DIR` | No wire protocol change; filter rule added for DIR; gated by `CF_INPLACE_PARTIAL_DIR` when combined with `--inplace` | 28 | `--inplace` (without CF bit 6) | Implemented | none | `partial-dir` |
| `--whole-file` (`-W`) | Skips delta algorithm; raw data sent instead of match tokens; `W` flag in server args | 28 | `--append` (options-level) | Implemented | Gap: `--append --whole-file` not rejected at config-build | `whole-file` |
| `--append` / `--append-verify` | Sender seeks past existing data; receiver appends; mode 1 forced to 2 at proto < 30 | 28 | `--whole-file`, `--partial-dir`, `--delay-updates` | Implemented | none | `append` |
| `--compress` (`-z`) | Token stream wrapped in zlib/zstd/lz4 frames; algorithm via vstring negotiation gated by CF bit 7 | 28 | none | Implemented | none | `compress-level`, `zstd-negotiation`, batch compression tests |
| `--compress-choice=STR` | Explicit algorithm; vstring exchange skipped | 28 | none | Implemented | none | `zstd-negotiation`, `compress-level` |
| `--delete` family | No wire bytes - generator-side operation; default phase depends on proto version | 28 | `--delete-before`/`--delete-after` suppress `CF_INC_RECURSE` | Implemented | none | `delete-after`, `delete-excluded`, `delete-with-filters`, `delete-filter-protect`, `delete-filter-risk` |
| `--hard-links` (`-H`) | `XMIT_HLINKED` + `XMIT_HLINK_FIRST` flist flags; hardlink list extras | 28 | none | Implemented | none | `hardlinks`, `hardlinks-comprehensive`, `hardlinks-checksum` |
| `--sparse` (`-S`) | No wire change - receiver-side seek optimization; `S` flag in server args | 28 | none | Implemented | none | `sparse` |
| `--dry-run` (`-n`) | Suppresses data transfer; `n` flag in server args; generator still sends file list | 28 | none | Implemented | none | `dry-run` |
| `--numeric-ids` | Suppresses uid/gid name mapping; long-form `--numeric-ids` in server args; `CF_ID0_NAMES` (bit 8) controls root name inclusion independently | 28 | none | Implemented | none | `numeric-ids-standalone` |
| `--fake-super` | Xattr-based metadata storage; long-form `--fake-super` in server args; sets `am_root = -1` | 28 | none | Implemented | none | (no dedicated interop test) |
| `--trust-sender` | Receiver skips safety checks on sender-provided paths; long-form in server args | 31 | none | Implemented | none | `trust-sender` |
| `--inc-recursive` | Direct control of `CF_INC_RECURSE` (bit 0); incremental file-list segments | 30 (bit exchange) | Many - see section 4 | Implemented (receiver); sender disabled | INC_RECURSE sender not wired for interop | `inc-recurse-comprehensive`, `inc-recurse-sender-push` |

## 2. CF_* Bit Reference

All bits defined in upstream `compat.c:117-125`. Exchanged as a varint at
protocol >= 30 (server writes, client reads).

| Bit | Upstream macro | oc-rsync constant | Cap char |
|----:|----------------|-------------------|:--------:|
| 0 | `CF_INC_RECURSE` | `CompatibilityFlags::INC_RECURSE` | `i` |
| 1 | `CF_SYMLINK_TIMES` | `CompatibilityFlags::SYMLINK_TIMES` | `L` |
| 2 | `CF_SYMLINK_ICONV` | `CompatibilityFlags::SYMLINK_ICONV` | `s` |
| 3 | `CF_SAFE_FLIST` | `CompatibilityFlags::SAFE_FILE_LIST` | `f` |
| 4 | `CF_AVOID_XATTR_OPTIM` | `CompatibilityFlags::AVOID_XATTR_OPTIMIZATION` | `x` |
| 5 | `CF_CHKSUM_SEED_FIX` | `CompatibilityFlags::CHECKSUM_SEED_FIX` | `C` |
| 6 | `CF_INPLACE_PARTIAL_DIR` | `CompatibilityFlags::INPLACE_PARTIAL_DIR` | `I` |
| 7 | `CF_VARINT_FLIST_FLAGS` | `CompatibilityFlags::VARINT_FLIST_FLAGS` | `v` |
| 8 | `CF_ID0_NAMES` | `CompatibilityFlags::ID0_NAMES` | `u` |

Source: `crates/protocol/src/compatibility/flags.rs:34-50`,
`crates/protocol/src/compatibility/known.rs:52-62`.

## 3. Per-Flag Detail

### 3.1 `--checksum` (`-c`)

**Wire protocol impact.** The `--checksum` flag changes the file-comparison
strategy from quick-check (mtime + size) to whole-file checksum. On the wire
this means:

1. The sender computes a per-file checksum during the file-list build phase
   and includes it in the flist extras. The receiver does the same for local
   files and compares. This adds `SUM_LENGTH` bytes (16 for MD5, 4 for XXH3)
   per file entry in the flist stream.
2. The checksum algorithm is selected via vstring negotiation gated by
   `CF_VARINT_FLIST_FLAGS` (bit 7). Without bit 7 (proto < 30 or peer
   without `v` capability): MD4 for proto < 30, MD5 for proto >= 30.
   With bit 7: peer-negotiated (XXH3/XXH128/MD5/MD4).
3. The seed ordering for MD5 is controlled by `CF_CHKSUM_SEED_FIX` (bit 5):
   `proper_seed_order = compat_flags & CF_CHKSUM_SEED_FIX` (upstream
   `compat.c:747`).

The `c` flag is sent in the compact server flag string (upstream
`options.c` server_options).

**Protocol version requirements.** No minimum - works at proto 28+. Algorithm
negotiation at proto >= 30 with bit 7.

**Mutual exclusions.** None at the compat-flag level.

**oc-rsync implementation.** Implemented. The `c` flag is emitted in the server
flag string (`core/src/client/remote/flags.rs:68`). Checksum seed ordering
keyed off `CompatibilityFlags::CHECKSUM_SEED_FIX` in the checksums crate.
Algorithm negotiation via `should_negotiate()` (`transfer/src/setup/mod.rs:251-268`).
Legacy fallback at `transfer/src/setup/mod.rs:162-173`.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenarios: `checksum-content`
(same-size different-content files detected by `-c`), `checksum-skip`
(identical files not re-transferred with `-c`), `hardlinks-checksum`
(hard links with checksum mode). Cross-tested against upstream 3.0.9,
3.1.3, 3.4.1.

### 3.2 `--inplace`

**Wire protocol impact.** The `--inplace` flag does not change the wire
encoding of delta tokens. The delta stream (match tokens + literal data)
is identical whether `--inplace` is active or not. What changes is the
receiver-side write strategy: instead of writing to a temporary file and
renaming, the receiver writes directly to the destination file.

When combined with `--partial-dir`, the `CF_INPLACE_PARTIAL_DIR` (bit 6)
compatibility flag controls whether the combination is allowed. When bit 6
is set by both peers, `inplace_partial = 1` (upstream `compat.c:777-778`)
and `--partial-dir` can coexist with `--inplace`.

**Protocol version requirements.** Proto >= 28. When combined with basis
directories (`--compare-dest`/`--copy-dest`/`--link-dest`), requires
proto >= 29 (upstream `compat.c:687-693`).

**Mutual exclusions.** Without `CF_INPLACE_PARTIAL_DIR`: mutually exclusive
with `--partial-dir`. Always mutually exclusive with `--delay-updates`
(upstream `options.c:2406-2414`). oc-rsync enforces both at config-build
time (`core/src/client/config/builder/mod.rs:287-292`).

**oc-rsync implementation.** Implemented. `CF_INPLACE_PARTIAL_DIR` set
unconditionally in SSH client mode (`transfer/src/setup/mod.rs:215`) and
via capability char `I` in daemon mode (`transfer/src/setup/capability.rs:96-102`).
Protocol < 29 restriction at `transfer/src/setup/restrictions.rs:132-139`.
The engine's inplace writer handles seek-based patching of existing files.

**Known divergences.** None at the protocol level.

**Interop coverage.** `run_interop.sh` scenario `inplace` (line 5216):
upstream pushes to oc-rsync daemon with `--inplace`, then oc-rsync pushes
to upstream daemon with `--inplace`. File content verified.

### 3.3 `--partial`

**Wire protocol impact.** None. `--partial` (`keep_partial`) is a purely
receiver-local option that controls whether partially transferred files
are retained when a transfer is interrupted. No bytes change on the wire.

The `P` flag is sent in the compact server flag string (upstream
`options.c` server_options), informing the remote receiver. This is a
single-character flag, not to be confused with `--partial-dir` which is
sent as a long-form argument.

**Protocol version requirements.** None (proto 28+).

**Mutual exclusions.** None.

**oc-rsync implementation.** Implemented. `P` flag emitted in server flag
string (`core/src/client/remote/flags.rs:111`). Config stored as
`keep_partial`.

**Known divergences.** None.

**Interop coverage.** Exercised indirectly through the `partial-dir` test.
No dedicated standalone `--partial` interop test.

### 3.4 `--partial-dir=DIR`

**Wire protocol impact.** No wire protocol bytes change for partial-dir
itself. It is sent as a long-form argument `--partial-dir=DIR` to the
remote server. A filter rule is generated to hide the partial directory
from the transfer (upstream `options.c:2413-2416`).

The key protocol interaction is with `CF_INPLACE_PARTIAL_DIR` (bit 6):
when both peers support bit 6, `--partial-dir` and `--inplace` can coexist.
Without bit 6, they are mutually exclusive.

**Protocol version requirements.** None (proto 28+).

**Mutual exclusions.** `--inplace` (without `CF_INPLACE_PARTIAL_DIR`).

**oc-rsync implementation.** Implemented. Long-form argument sent to server.
Filter rule for partial directory generated during transfer setup.
Partial files are moved to the specified directory on interruption and
resumed from there on restart.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenario `partial-dir` (line 5759):
tests partial directory creation and file resumption with upstream rsync.

### 3.5 `--whole-file` (`-W`)

**Wire protocol impact.** When `--whole-file` is active, the delta algorithm
is bypassed entirely. Instead of sending match tokens (block index +
offset) and literal data, the sender transmits the entire file content as
a single literal stream. On the wire this means:

1. No signature block request from the receiver (no `sum_head` with
   `block_count > 0` is sent).
2. The token stream consists of a single `LONG_TOKEN` containing the
   complete file data, followed by an end-of-file token.
3. The match report (`MSG_SUCCESS`) still contains the file index.

The `W` flag is sent in the compact server flag string (upstream
`options.c` server_options), but only when `--append` is not active
(append requires delta transfer to append new data).

**Protocol version requirements.** None (proto 28+).

**Mutual exclusions.** `--append` at the options level (upstream
`options.c:2382-2387`). When `--append` is active, `--whole-file` is
suppressed because append mode requires the delta algorithm to identify
the append point.

**oc-rsync implementation.** Implemented. `W` flag emitted in server flag
string only when append is not active (`core/src/client/remote/flags.rs:96-99`).
The generator/receiver path handles whole-file mode by setting block
count to 0 in the signature request, causing the sender to transmit the
entire file as literal data.

**Known divergences.** The mutual exclusion with `--append` is not enforced
as an error at config-build time in oc-rsync. Upstream rejects
`--append --whole-file` with an error; oc-rsync silently suppresses
`-W` from the flag string, which produces correct behaviour but does not
warn the user. This is a CLI validation gap, not a wire protocol
divergence.

**Interop coverage.** `run_interop.sh` scenario `whole-file` (line 4862):
upstream pushes to oc-rsync daemon with `-W`, then oc-rsync pushes to
upstream daemon with `-W`. Data integrity verified.

### 3.6 `--append` / `--append-verify`

**Wire protocol impact.** The append flag changes the delta transfer
strategy:

1. The receiver sends a signature request with `s2length = 0` (no block
   checksums) and `remainder = current_file_size`, telling the sender
   to skip existing data and only send bytes beyond the current offset.
2. In mode 2 (`--append-verify`), the receiver additionally requests a
   whole-file checksum verification after the append completes, matching
   the checksum of the source file against a checksum computed over the
   destination file including the appended data.
3. `--append` forces `inplace = 1` (upstream `options.c:2392`), so the
   receiver writes directly to the existing destination file at the
   append offset.

At protocol < 30, `append_mode == 1` is forced to `append_mode = 2`
(upstream `compat.c:653-654`), ensuring verify-mode is always used
with older peers that may not handle mode 1 correctly.

**Protocol version requirements.** Proto 28+. Mode forcing at proto < 30.

**Mutual exclusions.** `--whole-file` (options-level), `--partial-dir`,
`--delay-updates` (upstream `options.c:2406-2414`). oc-rsync enforces
`--partial-dir` and `--delay-updates` exclusions at config-build time.

**oc-rsync implementation.** Implemented. Mode forcing at
`transfer/src/setup/restrictions.rs:91-93`. The forced `inplace` flag
inherits all `CF_INPLACE_PARTIAL_DIR` behaviour. Append logic in the
receiver sends the appropriate signature request.

**Known divergences.** None at the protocol level.

**Interop coverage.** `run_interop.sh` scenario `append` (line 5286):
tests appending new data to existing files. Cross-tested with upstream
3.4.1 in both push and pull directions.

### 3.7 `--compress` (`-z`)

**Wire protocol impact.** When compression is active, the token stream
between sender and receiver is wrapped in compression frames. The wire
format changes are:

1. **Token encoding.** Each literal data token is compressed before
   transmission. Match tokens (block references) are sent uncompressed.
   The token framing uses `FLAG_DEFLATED` / `FLAG_ZSTD` / `FLAG_LZ4`
   markers to distinguish compressed from uncompressed tokens (upstream
   `token.c`).
2. **Algorithm negotiation.** The compression algorithm is selected through
   vstring exchange gated by `CF_VARINT_FLIST_FLAGS` (bit 7). In
   `negotiate_the_strings()` (upstream `compat.c:534-564`), compression
   vstrings are exchanged when `do_compression && !compress_choice`
   (upstream `compat.c:543`).
3. **Without negotiation** (proto < 30 or peer without `v` capability),
   the default compression is `CPRES_ZLIB` (upstream `compat.c:195`).

The `z` flag is sent in the compact server flag string only for the
default compression algorithm. Explicit algorithms (lz4, zstd) use
`--compress-choice=ALGO` as a long-form argument instead (upstream
`options.c:2704`).

**Protocol version requirements.** Proto 28+ (zlib only without negotiation).
Algorithm negotiation requires proto >= 30 with bit 7.

**Mutual exclusions.** None at the compat-flag level.

**oc-rsync implementation.** Implemented. Compression negotiation follows
the upstream path: `ProtocolSetupConfig::do_compression` and
`compress_choice` control vstring exchange. The `send_compression` guard
at `transfer/src/setup/mod.rs:109` matches upstream's
`do_compression && !compress_choice`. Legacy fallback to zlib at
`transfer/src/setup/mod.rs:163-165`. The `z` flag is only emitted for
the default algorithm (`core/src/client/remote/flags.rs:61-66`).

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenarios: `compress-level`
(line 5420 - tests `--compress-level=6`), `zstd-negotiation` (line 5506 -
tests zstd compression negotiation with upstream 3.4.1),
`write-batch-read-batch-compressed`, `upstream-compressed-batch-oc-reads`,
`oc-compressed-batch-upstream-reads`, `compressed-batch-delta-interop`.

### 3.8 `--compress-choice=STR`

**Wire protocol impact.** When `--compress-choice=ALGO` is specified, the
vstring exchange for compression is skipped (upstream `compat.c:543` -
`!compress_choice` is false). The specified algorithm is used directly via
`parse_compress_choice()` (upstream `compat.c:181-220`). This does not
affect which CF_* bits are set - `CF_VARINT_FLIST_FLAGS` is still
negotiated independently.

Related options `--old-compress` and `--new-compress` are syntactic sugar
that set `compress_choice` to `"zlib"` or `"zlibx"` respectively
(upstream `options.c:1614-1618`).

The chosen algorithm is sent as a long-form argument
`--compress-choice=ALGO` rather than the compact `z` flag.

**Protocol version requirements.** Proto 28+ (the algorithm itself must
be supported by both peers, but no minimum proto for the option).

**Mutual exclusions.** None.

**oc-rsync implementation.** Implemented. `transfer/src/setup/mod.rs:109` -
`config.compress_choice.is_none()` mirrors upstream gate. When set,
compression override is passed directly to the negotiator, skipping
vstring exchange.

**Known divergences.** None.

**Interop coverage.** Covered by `zstd-negotiation` and several batch
compression tests that use `--compress-choice=zlib` explicitly to work
around upstream zstd interop quirks.

### 3.9 `--delete` / `--delete-before` / `--delete-during` / `--delete-after` / `--delete-excluded`

**Wire protocol impact.** The delete family does not add bytes to the wire
protocol directly. Deletion is a generator-side operation performed on the
receiver before, during, or after file transfer. The only wire-visible
effects are:

1. **`NDX_DEL_STATS`** - At protocol >= 31, the generator sends a
   `NDX_DEL_STATS` message during the goodbye phase containing 5 varints
   (counts by type: files, dirs, symlinks, devices, specials). The
   receiver parses this via `read_del_stats()`.
2. **File list ordering** - `--delete-before` requires the complete file
   list before transfer begins, which affects `CF_INC_RECURSE` (see below).
3. **`--delete-excluded`** - The `--delete-excluded` flag is sent as a
   long-form argument. It causes excluded destination files to also be
   deleted, but does not change the wire encoding of exclusion rules.

Delete mode defaults: when `--delete` is specified without an explicit
phase, upstream defaults to `delete_before` for proto < 30 and
`delete_during` for proto >= 30 (upstream `compat.c:671-676`).

**CF_INC_RECURSE interaction.** On the receiver side, `delete_before` or
`delete_after` suppresses `allow_inc_recurse` (upstream `compat.c:173-176`),
preventing `CF_INC_RECURSE` (bit 0) from being set. `delete_during` does
not suppress it - this is the key reason upstream defaults to
`delete_during` at proto >= 30.

**Protocol version requirements.** Proto 28+. Default phase selection
depends on proto version.

**Mutual exclusions.** `--delete-before`/`--delete-after` suppress
`CF_INC_RECURSE`. `--delay-updates` also suppresses it.

**oc-rsync implementation.** Implemented. Default phase selection at
`transfer/src/setup/restrictions.rs:113-119`. `DeleteStats` with
`NDX_DEL_STATS` sending implemented. `allow_inc_recurse` suppression at
config-build time.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenarios: `delete-after`
(line 4436), `delete-excluded` (line 5966), `delete-with-filters`
(line 6361), `delete-filter-protect` (line 6451), `delete-filter-risk`
(line 6608). Push/pull delete scenarios tested in the core scenario matrix
across upstream versions 3.0.9, 3.1.3, 3.4.1.

### 3.10 `--hard-links` (`-H`)

**Wire protocol impact.** Hard link detection adds data to the file-list
stream:

1. **XMIT flags.** `XMIT_HLINKED` (extended bit 9, proto 28+) marks
   entries that are part of a hard link group. `XMIT_HLINK_FIRST`
   (extended bit 12, proto 30+) marks the first entry in a group.
2. **Hardlink extras.** Each hardlinked file entry carries an `hl_ndx`
   (hard link index) in its extras array. For non-first members, a
   `hl_extra` reference points to the first member of the group.
3. **`H` flag** is sent in the compact server flag string.

The only tangential CF_* connection is `CF_AVOID_XATTR_OPTIM` (bit 4):
when `--hard-links` and `--xattrs` are both active, upstream uses an xattr
optimization for hardlinked files that is disabled when bit 4 is set
(`want_xattr_optim = protocol >= 31 && !(compat & CF_AVOID_XATTR_OPTIM)`,
upstream `compat.c:746`).

**Protocol version requirements.** Proto 28+ for basic hardlink support.
Proto 30+ for `XMIT_HLINK_FIRST` (inline first-member tracking).

**Mutual exclusions.** None.

**oc-rsync implementation.** Implemented. `H` flag emitted in server flag
string (`core/src/client/remote/flags.rs:71`). Hardlink detection and
preservation handled in the file-list builder and generator. The
`want_xattr_optim` computation is at the receiver/wire layer, keyed off
bit 4 and proto >= 31.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenarios: `hardlinks` (line 4519),
`hardlinks-comprehensive` (line 3451 - multi-directory hard links, cross-dir
links), `hardlinks-checksum` (hard links with `--checksum`). Tested against
upstream 3.0.9, 3.1.3, 3.4.1. The core scenario matrix exercises hard links
in push and pull directions.

### 3.11 `--sparse` (`-S`)

**Wire protocol impact.** The `--sparse` flag does not change the wire
encoding. Delta tokens and literal data are transmitted identically whether
sparse mode is active or not. Sparse handling is a receiver-side
optimization:

1. The receiver detects zero-filled runs in incoming data using `u128`
   zero-run detection (16 bytes at a time).
2. Instead of writing zeros, the receiver seeks past them, creating a
   sparse file on the filesystem.
3. A single seek-per-zero-run invariant is maintained for efficiency.

The `S` flag is sent in the compact server flag string (upstream
`options.c` server_options) to inform the remote receiver.

**Protocol version requirements.** None (proto 28+).

**Mutual exclusions.** None. `--sparse` and `--inplace` can coexist
(upstream handles this by falling back to non-sparse writes for inplace
mode in some edge cases).

**oc-rsync implementation.** Implemented. `S` flag emitted in server flag
string (`core/src/client/remote/flags.rs:102`). The engine provides
`SparseWriter`, `SparseReader`, and `SparseDetector` types
(`crates/engine/src/local_copy/`). Detection strategy is configurable
via `SparseDetectStrategy::Auto`. The async copier also supports sparse
detection (`crates/engine/src/async_io/copier.rs:73`).

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenario `sparse` (line 4803):
upstream rsync pushes a 2MB zero-filled file and a regular file with
`--sparse` to oc-rsync daemon. Content integrity and sparse allocation
verified.

### 3.12 `--dry-run` (`-n`)

**Wire protocol impact.** Dry-run mode suppresses actual data transfer
while preserving the protocol handshake and file-list exchange:

1. The generator still sends file indices for files that would be
   transferred, but the sender does not transmit file data.
2. The `n` flag is sent in the compact server flag string (upstream
   `options.c` server_options). Note: `n` means dry_run, NOT
   numeric_ids (which is always long-form).
3. Statistics and itemize output are generated as if a real transfer
   occurred, but no file writes happen on the receiver side.

**Protocol version requirements.** None (proto 28+).

**Mutual exclusions.** None. Dry-run is compatible with all other flags.

**oc-rsync implementation.** Implemented. `n` flag emitted in server flag
string (`core/src/client/remote/flags.rs:87`). The generator and receiver
skip data transfer and file writes when dry-run is active while still
computing and displaying itemize output and statistics.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenario `dry-run` (line 4939):
verifies that no files are created at the destination during a dry-run
transfer. Tests that the transfer completes successfully (exit code 0)
and that itemize output is produced.

### 3.13 `--numeric-ids`

**Wire protocol impact.** The `--numeric-ids` flag suppresses uid/gid name
mapping on the receiver side. Wire protocol effects:

1. **Uid/gid name list.** When `--numeric-ids` is active, the receiver
   does not attempt to map uid/gid names to local system ids. The names
   are still transmitted in the flist stream by the sender (the flag
   controls receiver-side interpretation, not sender-side emission).
2. **`CF_ID0_NAMES` (bit 8).** This bit controls whether root's name
   (uid 0, gid 0) is included in the id map. `--numeric-ids` and
   `CF_ID0_NAMES` are independent: `--numeric-ids` controls name-to-id
   mapping; `CF_ID0_NAMES` controls whether root's name is transmitted.
   Both can be active simultaneously.
3. The flag is sent as long-form `--numeric-ids` (upstream
   `options.c:2887-2888`), never as a compact flag character.

**Protocol version requirements.** None (proto 28+).

**Mutual exclusions.** None.

**oc-rsync implementation.** Implemented. Long-form `--numeric-ids` sent in
server args. `CF_ID0_NAMES` advertised unconditionally in SSH client mode
(`transfer/src/setup/mod.rs:216`) and via `u` in daemon mode
(`transfer/src/setup/capability.rs:111-117`). Receiver-side id mapping
suppression implemented.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenario `numeric-ids-standalone`
(line 8954): upstream client pushes to oc-rsync daemon with `--numeric-ids`,
then oc-rsync client pushes to upstream daemon with `--numeric-ids`.
Also exercised in the daemon configuration (`numeric ids = yes` in interop
module configs throughout `run_interop.sh`).

### 3.14 `--fake-super`

**Wire protocol impact.** No wire protocol change. `--fake-super` sets
`am_root = -1` (upstream `options.c:653`), which causes the receiver to
store ownership and permission metadata as xattrs (`user.rsync.*`)
instead of using `chown`/`chmod`. This is entirely an application-layer
concern handled in `xattrs.c`, not during protocol negotiation.

The flag is sent as long-form `--fake-super` in server args (upstream
`options.c` server_options).

**Protocol version requirements.** None (proto 28+). However, since
`--fake-super` relies on xattr storage, it implicitly requires xattr
support on the filesystem.

**Mutual exclusions.** None at the compat-flag level.

**oc-rsync implementation.** Implemented. Long-form `--fake-super` sent in
server args (`core/src/client/remote/invocation/builder.rs`). No CF_*
interaction. The xattr-based metadata storage is handled at the metadata
layer.

**Known divergences.** None.

**Interop coverage.** No dedicated interop test. `--fake-super` is
implicitly tested through daemon configurations that use
`fake super = yes` in module settings, but there is no standalone
`test_fake_super` function in `run_interop.sh`.

### 3.15 `--trust-sender`

**Wire protocol impact.** No wire protocol change. `--trust-sender` is a
receiver-side safety flag that controls whether the receiver performs
validation checks on sender-provided file paths (e.g., rejecting paths
that escape the destination directory). It is sent as long-form
`--trust-sender` in server args.

This flag was introduced in rsync 3.2.5 (protocol 31+) to address
CVE-2022-29154. Without `--trust-sender`, the receiver validates that
sender-provided paths do not traverse outside the destination.

**Protocol version requirements.** Proto 31+ (upstream `options.c`). The
flag itself has no CF_* bit interaction.

**Mutual exclusions.** None.

**oc-rsync implementation.** Implemented. Stored as `trust_sender` in
config (`core/src/client/config/builder/preservation.rs:89`). Applied to
server config via `apply_common_server_flags()`
(`core/src/client/remote/flags.rs:204`). The receiver-side validation
is gated on this flag.

**Known divergences.** None.

**Interop coverage.** `run_interop.sh` scenario `trust-sender` (line 5696):
creates source files and transfers with `--trust-sender` to verify the
flag is accepted and transfer succeeds.

### 3.16 `--inc-recursive` / `--no-inc-recursive`

**Wire protocol impact.** Incremental recursion fundamentally changes the
file-list exchange pattern:

1. **Without INC_RECURSE (traditional).** The sender builds and transmits
   the complete file list before any data transfer begins. The receiver
   must hold the entire list in memory.
2. **With INC_RECURSE (incremental).** The sender transmits file-list
   segments incrementally as directories are traversed. Each segment
   is a sorted sub-list. The receiver processes files as segments arrive,
   reducing peak memory usage for large trees.
3. **Wire encoding.** Each segment ends with a zero-byte sentinel.
   `XMIT_TOP_DIR` marks directory boundaries. Sub-lists are sorted
   using `sort_unstable_by` for INC_RECURSE segments.
4. **`CF_INC_RECURSE` (bit 0).** Directly controls incremental mode.
   Only set when both peers support it and `allow_inc_recurse` survives
   all suppression checks.

**Protocol version requirements.** Proto >= 30 (bit exchange requires
binary negotiation). The `--inc-recursive` / `--no-inc-recursive` CLI
flags directly set or clear `allow_inc_recurse` (upstream
`options.c:614-617`).

**`allow_inc_recurse` suppression.** Multiple conditions suppress
incremental recursion (upstream `compat.c:161-179`):

| Condition | Upstream cite |
|-----------|---------------|
| `!recurse` | `compat.c:171` |
| `use_qsort` | `compat.c:171` |
| Receiver + `delete_before` | `compat.c:173-174` |
| Receiver + `delete_after` | `compat.c:174` |
| Receiver + `delay_updates` | `compat.c:175` |
| Receiver + `prune_empty_dirs` | `compat.c:175` |
| Server + client lacks `i` | `compat.c:177-178` |
| `--files-from` (forces `recurse=0`) | `options.c:2188` |
| `--no-inc-recursive` | `options.c:615` |

**Mutual exclusions.** Many options indirectly conflict by suppressing
`allow_inc_recurse` (see table above). No hard mutual exclusion at
the options level - the option is simply silently disabled.

**oc-rsync implementation.** Receiver-side incremental recursion is
implemented and tested. Sender-side INC_RECURSE code exists in the
generator but interop has not been validated against upstream - the `i`
capability char is only advertised for receiver direction
(`build_capability_string(!is_sender)` in
`crates/protocol/src/setup.rs`/`transfer/src/setup/capability.rs`).

`allow_inc_recurse` controls bit 0 in `build_our_flags()`
(`transfer/src/setup/mod.rs:233-235`) and capability char `i` in
`build_capability_string()` (`transfer/src/setup/capability.rs:144-146`).
Client-side defensive mask at `transfer/src/setup/mod.rs:121-123`
silently clears the bit rather than aborting.

**Known divergences.** Sender-side `CF_INC_RECURSE` is intentionally
disabled for push transfers pending sender-side validation. Upstream
advertises `i` symmetrically; oc-rsync only advertises it for receiver
direction. This means oc-rsync push transfers to upstream will never
use incremental recursion even if the upstream receiver supports it.
This is conservative and wire-safe but suboptimal for memory usage on
large push transfers.

**Interop coverage.** `run_interop.sh` scenarios: `inc-recurse-comprehensive`
(line 3601 - 6 sub-tests: local transfer, SSH push, SSH pull, daemon push,
daemon pull, delete with inc-recursive), `inc-recurse-sender-push`
(line 3935 - sender-side inc-recursive push to upstream and daemon).
Tested against upstream 3.4.1.

## 4. Cross-Cutting Interactions

### 4.1 `allow_inc_recurse` Suppression

The `CF_INC_RECURSE` bit (0) is the most cross-cutting compatibility
flag because several options suppress it indirectly through
`set_allow_inc_recurse()` (upstream `compat.c:161-179`).

| Condition | Upstream cite | oc-rsync handling | Status |
|-----------|---------------|-------------------|:------:|
| `!recurse` | `compat.c:171` | config-build time | ok |
| `use_qsort` | `compat.c:171` | config-build time | ok |
| Receiver + `delete_before` | `compat.c:173-174` | config-build time | ok |
| Receiver + `delete_after` | `compat.c:174` | config-build time | ok |
| Receiver + `delay_updates` | `compat.c:175` | config-build time | ok |
| Receiver + `prune_empty_dirs` | `compat.c:175` | config-build time | ok |
| Server + client lacks `i` | `compat.c:177-178` | `build_compat_flags_from_client_info()` | ok |
| `--files-from` (forces `recurse=0`) | `options.c:2188` | invocation builder | ok |
| `--no-inc-recursive` | `options.c:615` | config field | ok |

### 4.2 Protocol-Version Restrictions

Options that impose protocol-version minimums, enforced in upstream
`compat.c:641-709` and oc-rsync `transfer/src/setup/restrictions.rs`.

| Option | Min proto | Upstream cite | oc-rsync cite | Status |
|--------|:---------:|---------------|---------------|:------:|
| `--acls` | 30 | `compat.c:655-661` | `restrictions.rs:96-102` | ok |
| `--xattrs` | 30 | `compat.c:662-668` | `restrictions.rs:104-109` | ok |
| `--fuzzy` | 29 | `compat.c:679-685` | `restrictions.rs:124-129` | ok |
| `--inplace` + basis dirs | 29 | `compat.c:687-693` | `restrictions.rs:132-139` | ok |
| Multiple basis dirs | 29 | `compat.c:695-701` | `restrictions.rs:143-151` | ok |
| `--prune-empty-dirs` | 29 | `compat.c:703-709` | `restrictions.rs:154-161` | ok |
| `--append` (mode 1 -> 2) | < 30 forces mode 2 | `compat.c:653-654` | `restrictions.rs:91-93` | ok |
| `--delete` default phase | < 30: before; >= 30: during | `compat.c:671-676` | `restrictions.rs:113-119` | ok |
| `--crtimes` | needs bit 7 (>= 3.2.0) | `compat.c:750-753` | protocol layer | ok |

### 4.3 Negotiation-Dependent Options

Options whose behaviour varies based on the outcome of vstring negotiation
(gated by `CF_VARINT_FLIST_FLAGS`, bit 7).

| Option | Without negotiation (proto < 30 or no `v`) | With negotiation (proto >= 30 + `v`) | oc-rsync handling |
|--------|-------------------------------------------|--------------------------------------|-------------------|
| `--checksum` | MD4 (proto < 30); MD5 (proto >= 30) | Peer-negotiated (XXH3/XXH128/MD5/MD4) | Legacy fallback (`setup/mod.rs:162-173`) and vstring path |
| `--compress` | CPRES_ZLIB only | Peer-negotiated (zstd/lz4/zlibx/zlib) | Legacy fallback and `send_compression` guard |
| `--compress-choice=ALGO` | Specified directly (no exchange) | Specified directly (vstring skipped) | `compress_choice.is_none()` gate |
| `--crtimes` | Rejected (`compat.c:750-753`) | Allowed | Protocol-layer enforcement |

### 4.4 Flag Interaction Matrix

Key flag combinations and their behaviour:

| Combination | Behaviour | Wire effect |
|-------------|-----------|-------------|
| `--inplace --partial-dir` | Allowed only with CF bit 6 | No wire change; receiver writes in-place to partial dir |
| `--inplace --delay-updates` | Rejected (mutual exclusion) | N/A |
| `--append --whole-file` | Upstream rejects; oc-rsync silently suppresses `W` | Append mode prevails; delta transfer used |
| `--append --delay-updates` | Rejected (mutual exclusion) | N/A |
| `--delete-before --inc-recursive` | `CF_INC_RECURSE` suppressed | Full file list built before transfer |
| `--delete-during --inc-recursive` | `CF_INC_RECURSE` preserved | Incremental file list; deletion interleaved |
| `--compress --checksum` | Independent | Compressed token stream + whole-file checksums in flist |
| `--sparse --inplace` | Both active; sparse seeks applied to in-place writes | No wire change |
| `--dry-run --delete` | Generator reports deletions but does not perform them | No data transfer |
| `--hard-links --xattrs` | xattr optimization gated by CF bit 4 | Hardlink extras + xattr data in flist |

## 5. Interop Test Coverage Summary

Coverage of each audited flag family in `tools/ci/run_interop.sh`:

| Flag | Dedicated test | Additional coverage | Upstream versions tested |
|------|----------------|---------------------|:------------------------:|
| `--checksum` | `checksum-content`, `checksum-skip` | `hardlinks-checksum` | 3.0.9, 3.1.3, 3.4.1 |
| `--inplace` | `inplace` | Core scenario matrix | 3.4.1 |
| `--partial` | (via `partial-dir`) | - | 3.4.1 |
| `--partial-dir` | `partial-dir` | - | 3.4.1 |
| `--whole-file` | `whole-file` | Core scenario matrix | 3.4.1 |
| `--append` | `append` | - | 3.4.1 |
| `--compress` | `compress-level`, `zstd-negotiation` | 5 batch compression tests | 3.4.1 |
| `--compress-choice` | `zstd-negotiation` | Batch tests with `--compress-choice=zlib` | 3.4.1 |
| `--delete` family | `delete-after`, `delete-excluded` | `delete-with-filters`, `delete-filter-protect`, `delete-filter-risk`, core matrix | 3.0.9, 3.1.3, 3.4.1 |
| `--hard-links` | `hardlinks`, `hardlinks-comprehensive` | `hardlinks-checksum`, core matrix | 3.0.9, 3.1.3, 3.4.1 |
| `--sparse` | `sparse` | - | 3.4.1 |
| `--dry-run` | `dry-run` | - | 3.4.1 |
| `--numeric-ids` | `numeric-ids-standalone` | Daemon module configs | 3.4.1 |
| `--fake-super` | (none dedicated) | Daemon module configs | - |
| `--trust-sender` | `trust-sender` | - | 3.4.1 |
| `--inc-recursive` | `inc-recurse-comprehensive` | `inc-recurse-sender-push` | 3.4.1 |

## 6. Known Gaps

1. **`--append --whole-file` validation.** oc-rsync silently suppresses
   the `W` flag rather than rejecting the combination with an error as
   upstream does. This produces correct wire behaviour but may confuse
   users expecting an error message. Tracked as a CLI validation issue.

2. **INC_RECURSE sender direction.** oc-rsync only advertises `CF_INC_RECURSE`
   when acting as receiver. Push transfers to an upstream receiver that
   supports incremental recursion will fall back to traditional full file-list
   mode. This is intentionally conservative pending sender-side interop
   validation.

3. **`--fake-super` interop.** No dedicated interop test exercises
   `--fake-super` in cross-implementation scenarios. The feature is
   implemented and used in daemon module configs, but there is no explicit
   test verifying xattr-based metadata storage interop between oc-rsync
   and upstream rsync.

4. **`--partial` standalone.** No dedicated interop test for `--partial`
   in isolation (without `--partial-dir`). The feature is simple
   (receiver-local) and unlikely to have interop issues, but explicit
   coverage would strengthen confidence.

## 7. Conformance Statement

All 16 audited flag families conform to upstream rsync 3.4.1 wire protocol
behaviour. The three gaps identified are:

- One CLI validation gap (`--append --whole-file` not rejected) that does
  not affect wire correctness.
- One intentional capability restriction (INC_RECURSE sender) that is
  wire-safe but reduces functionality.
- Two interop test coverage gaps (`--fake-super`, `--partial` standalone)
  that do not indicate implementation defects.

No wire protocol divergences were identified.
