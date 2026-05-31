# Upstream Cross-References Audit

This document catalogues the `// upstream:` comment convention used throughout
the codebase and identifies gaps where non-obvious upstream behavior is
implemented without a corresponding reference.

## Convention

The project uses inline comments of the form:

```rust
// upstream: <file>:<line-range> - <brief description>
```

Examples:

```rust
// upstream: token.c:send_token() - literals are written as write_int(length)
// upstream: flist.c:send_file_entry() lines 545-565
// upstream: compat.c:117 (CF_* macros)
// upstream: io.c:2243-2287 - write_ndx() function
```

Module-level `//!` docs may also reference upstream source files in prose form.

## Current Coverage by Crate

The table below shows the count of `// upstream:` comments in source files
(excluding test files) for the critical crates:

| Crate | Source refs | Key upstream files referenced |
|-------|------------|-------------------------------|
| `transfer` | ~458 | io.c, receiver.c, sender.c, token.c, batch.c, generator.c |
| `protocol` | ~349 | flist.c, exclude.c, io.c, compat.c, clientserver.c, hlink.c |
| `daemon` | ~214 | clientserver.c, loadparm.c, authenticate.c, daemon-parm.h |
| `core` | ~173 | options.c, errcode.h, main.c, compat.c, rsync.c, checksum.c |
| `engine` | ~145 | receiver.c, generator.c, backup.c, token.c, match.c, options.c |
| `matching` | ~40 | match.c, hashtable.c, generator.c, rsum.c |
| `checksums` | ~15 | checksum.c |
| `filters` | ~14 | exclude.c |

Test files add approximately 1,200 additional references across all crates.

## Inventory of Referenced Upstream Files

The following upstream C source files are referenced in the codebase:

- `token.c` - Token/compressed delta stream encoding
- `flist.c` - File list construction, sorting, wire encoding/decoding
- `io.c` - Multiplexed I/O, NDX codec, varint, batch tee
- `match.c` - Block matching, hash probes, want_i hint
- `sender.c` - Sender transfer loop, NDX ordering
- `receiver.c` - Receiver transfer loop, temp files, append mode
- `generator.c` - Generator loop, quick-check, fuzzy basis
- `compat.c` - Compatibility flags, negotiation strings, compression/checksum selection
- `clientserver.c` - Daemon negotiation, module access, authentication
- `options.c` - Option parsing, server_options(), conflict validation
- `exclude.c` - Filter rule parsing, evaluation, wire encoding
- `backup.c` - Backup file creation, rename, cross-device copy
- `checksum.c` - Strong checksum computation, seed handling
- `hlink.c` - Hardlink tracking, leader/follower logic
- `errcode.h` - Exit code constants
- `rsync.h` - Protocol constants (XMIT_*, NDX_*, MPLEX_BASE)
- `loadparm.c` - Daemon configuration parameter handling
- `uidlist.c` - UID/GID list exchange and mapping
- `hashtable.c` - Hash table construction for block matching
- `util1.c` - Utility functions (safe_chars, partial_dir)
- `fileio.c` - Buffered file I/O (map_file sliding window)
- `pipe.c` - Child process argument tracing
- `log.c` - Logging codes, daemon log handling
- `syscall.c` - System call wrappers (do_mknod)
- `cleanup.c` - Transfer cleanup on abnormal exit
- `batch.c` - Batch file I/O

## Identified Gaps

The following areas implement non-obvious upstream behavior without
`// upstream:` comments. These represent opportunities for improved
traceability.

### Protocol Crate

| File | Missing reference | Upstream source |
|------|------------------|-----------------|
| `envelope/constants.rs` | `MPLEX_BASE = 7` magic value | rsync.h:68 `#define MPLEX_BASE 7` |
| `envelope/header.rs` | 4-byte LE header encoding with tag in high byte | io.c:965 `send_msg()` |
| `envelope/message_code.rs` | Numeric code assignments (Data=0, Info=2, etc.) | rsync.h `enum msgcode` |
| `multiplex/codec.rs` | 24-bit payload length limit | io.c `BIGPATHBUFLEN` / rsync.h |
| `multiplex/frame.rs` | Frame construction and payload assembly | io.c:read_a_msg() |
| `wire/delta/int_encoding.rs` | `write_int` / `read_int` 4-byte LE primitives | io.c:2082,2091 (noted in doc but not `// upstream:`) |
| `wire/delta/token.rs` | Token wire format (positive=literal, negative=match, zero=end) | token.c:simple_send_token() (noted in doc but not `// upstream:`) |
| `wire/signature.rs` | Signature header format (block_count, block_length, sum_length) | sender.c / match.c sum_struct |
| `codec/ndx/codec.rs` | Modern NDX delta encoding (prev_positive tracking) | io.c:2243-2287 (noted in struct doc but not inline) |
| `compatibility/flags.rs` | CF_* bit definitions (INC_RECURSE=1<<0, etc.) | compat.c:117 (noted in struct doc but not per-constant) |
| `varint/encode.rs` | Variable-length integer encoding algorithm | io.c:write_varint() (noted in doc but not `// upstream:`) |
| `flist/name_cmp.rs` | `f_name_cmp()` byte-wise comparison logic | flist.c:3217 (noted in module doc but not inline) |

### Engine Crate

| File | Missing reference | Upstream source |
|------|------------------|-----------------|
| `delta/mod.rs` | Block size selection heuristic | generator.c:sum_sizes_sqroot() (noted in doc but not `// upstream:`) |
| `concurrent_delta/` (all files) | Parallel delta application pipeline | No upstream equivalent (novel implementation) |
| `pipeline/spsc.rs` | Lock-free channel design | No upstream equivalent |

### Transfer Crate

| File | Missing reference | Upstream source |
|------|------------------|-----------------|
| `pipeline/state.rs` | FIFO response ordering requirement | receiver.c main loop ordering |
| `map_file/buffered.rs` (partial) | Sliding window overlap reuse | fileio.c:268-279 (has one ref but not full coverage) |

### Daemon Crate

| File | Missing reference | Upstream source |
|------|------------------|-----------------|
| `auth.rs` | Challenge-response MD4/MD5 computation | authenticate.c:auth_server() |
| `rsyncd_config/` (various) | Configuration parameter parsing | loadparm.c (partially covered) |

### Checksums Crate

| File | Missing reference | Upstream source |
|------|------------------|-----------------|
| `rolling/checksum/mod.rs` | Adler-32 variant algorithm with signed-byte treatment | checksum.c:285 (has some refs, but core algorithm logic not fully annotated) |
| `strong/md4.rs` | MD4 hash implementation details | md4.c / checksum.c |

## Classification

Not every piece of code needs an upstream reference. The following
classification helps decide when to add one.

### Always add `// upstream:` when:

1. **Wire format encoding/decoding** - Any code that reads or writes bytes on
   the rsync protocol wire must reference the upstream source that defines the
   format. This is the primary use case.

2. **Non-obvious behavioral choices** - When code makes a choice that is not
   self-evident from the problem domain but matches upstream behavior (e.g.,
   sorting order, checksum seed ordering, flag bit assignments).

3. **Magic constants and sentinel values** - Numeric constants whose meaning is
   defined by the C source (MPLEX_BASE=7, NDX_DONE=-1, CHUNK_SIZE=32768).

4. **Error handling semantics** - When error codes, error messages, or recovery
   behavior must match upstream exactly for interoperability.

5. **Algorithm fidelity** - When an algorithm is deliberately structured to
   match upstream's control flow (e.g., rolling checksum signed-byte treatment,
   `want_i` hint optimization in block matching).

### Do not add `// upstream:` when:

1. **Standard Rust patterns** - Buffer management, error propagation, trait
   implementations, and other Rust idioms that have no upstream C counterpart.

2. **Novel implementation** - Code that has no upstream equivalent (e.g., the
   parallel delta pipeline in `concurrent_delta/`, the SPSC channel, io_uring
   integration).

3. **Self-documenting code** - When the code's intent is obvious from its name
   and context (e.g., a `Vec::push` that appends to a list).

4. **Test utilities** - Helper functions for test setup/teardown with no
   protocol semantics.

### Format guidelines:

- Use `// upstream:` (lowercase, with colon and space) for inline comments.
- Use `/// upstream:` for doc-comment references on types and functions.
- Include the filename and line number or line range: `token.c:307-314`.
- Include the function name when it clarifies the context: `io.c:read_buf()`.
- Keep the description brief - one phrase explaining what the upstream code does.
- When multiple upstream locations are relevant, use separate comments or a
  comma-separated list.

### Preferred patterns:

```rust
// Single reference with description
// upstream: token.c:307-314 - literals chunked at CHUNK_SIZE

// Function-level reference
/// upstream: flist.c:send_file_entry() lines 545-565
pub fn encode_file_entry(...) { ... }

// Constant with upstream definition
/// Upstream: `rsync.h:285` - `#define NDX_DONE -1`
pub const NDX_DONE: i32 = -1;
```

## Recommendations

1. **Priority 1: Wire format primitives** - Add `// upstream:` to
   `envelope/constants.rs` (MPLEX_BASE), `envelope/header.rs` (header
   encoding), and `envelope/message_code.rs` (code assignments). These are
   the most critical for interoperability debugging.

2. **Priority 2: Varint/NDX encoding** - The varint encoder and modern NDX
   codec reference upstream in their doc comments but lack inline `// upstream:`
   markers at the implementation level. Adding them aids line-level tracing.

3. **Priority 3: Compatibility flags** - Each `CF_*` constant in
   `compatibility/flags.rs` should have an inline upstream reference linking
   to the specific `compat.c` line that defines it.

4. **Priority 4: Signature wire format** - `wire/signature.rs` lacks any
   upstream reference. The header format (sum_struct fields) should reference
   `sender.c` and `match.c`.

5. **No action needed** - The `concurrent_delta/` module, `pipeline/spsc.rs`,
   and other novel Rust-specific implementations correctly omit upstream
   references since they have no C counterpart.

## Maintenance

When adding new protocol features:

1. Read the upstream C source first (at `target/interop/upstream-src/rsync-3.4.1/`).
2. Add `// upstream:` on every non-obvious line that mirrors C behavior.
3. Run `grep -rn "// upstream:" crates/<crate>/src/ | wc -l` to verify coverage
   trends upward, not downward, with new code.
